//! Exclusive database session ownership, command serialization, and terminal cleanup.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result, TransactionId};
use sqarvo_connection::SessionHandle;
use sqarvo_local_store::profiles::ProfileDocument;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::super::{write, write_preflight, write_transaction};
use super::{Begin, denied, invalid};
use crate::access::OperationType;
use crate::audit::{AuditRecord, TransactionAudit};
use crate::policy::write::WritePlan;
use crate::server::ServerContext;
use crate::transactions::{Command, Failure, Owner, Reply, cancelled, terminal_error};

const IDLE: Duration = Duration::from_secs(30);
const LIFETIME_SECONDS: u64 = 120;
const POLICY_INTERVAL: Duration = Duration::from_secs(1);
const MAX_OPERATIONS: usize = 20;
const MAX_ROWS: u64 = 200;
const BUDGET: Duration = write_transaction::TERMINAL_BUDGET;

struct Target {
    id: Uuid,
    alias: String,
    database: String,
    profile: Uuid,
    profile_revision: u64,
    document_revision: u64,
}

impl Target {
    fn check(&self, document: &ProfileDocument) -> Result<()> {
        if !document.mcp.enabled
            || document.mcp.read_only
            || document.revision != self.document_revision
            || !document.profiles.iter().any(|profile| {
                profile.profile_id == self.profile && profile.revision == self.profile_revision
            })
        {
            return Err(denied(
                "Transaction authorization or the connection profile changed",
            ));
        }
        Ok(())
    }

    async fn reauthorize(&self, server: &ServerContext, operation: OperationType) -> Result<()> {
        let authorization = server
            .access()
            .guard()
            .authorize_request(operation, Some(self.profile.to_string()))
            .await
            .map_err(|_| Stop::Revoked.error())?;
        self.check(authorization.document())?;
        if let Some(profile) = &authorization.profile {
            server.access().require_write_profile(profile)?;
        }
        Ok(())
    }

    async fn audit(&self, server: &ServerContext, request: &str, event: Event<'_>) -> bool {
        let mut record = AuditRecord::new(request, event.name)
            .with_target(Some(&self.alias), Some(&self.database));
        if let Some(error) = event.error {
            record = record.failed(error);
        }
        record.transaction = Some(TransactionAudit {
            transaction_id: self.id,
            event: event.name,
            phase: event.phase,
            operation: event.operation,
            affected_rows: event.rows,
            transaction_state: event.state,
        });
        if let Ok(Ok(())) = timeout(BUDGET, server.audit().write(&record)).await {
            true
        } else {
            tracing::error!(transaction_id = %self.id, event = event.name, "MCP transaction auditing failed");
            false
        }
    }
}

struct Event<'a> {
    name: &'static str,
    phase: &'static str,
    operation: &'static str,
    state: &'static str,
    rows: Option<u64>,
    error: Option<&'a Error>,
}

impl Event<'_> {
    fn lifecycle(name: &'static str, operation: &'static str, state: &'static str) -> Self {
        Self {
            name,
            phase: "outcome",
            operation,
            state,
            rows: None,
            error: None,
        }
    }
}

struct Open {
    handle: SessionHandle,
    driver_id: TransactionId,
    deadline: Instant,
    idle_deadline: Instant,
    operations: HashSet<String>,
    rows: u64,
    failed: bool,
}

impl Open {
    fn state(&self) -> &'static str {
        if self.failed { "failed" } else { "open" }
    }
}

struct Reservation {
    server: Arc<ServerContext>,
    id: Uuid,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.server.transactions.release(self.id);
    }
}

#[allow(clippy::too_many_arguments)] // BEGIN owns the reservation and the command receiver.
pub(super) async fn run(
    server: &Arc<ServerContext>,
    owner: &Owner,
    id: Uuid,
    args: Begin,
    request: String,
    cancellation: CancellationToken,
    mut commands: mpsc::Receiver<Command>,
    reply: Reply,
) {
    let reservation = Reservation {
        server: Arc::clone(server),
        id,
    };
    let stop = service_stop(server, owner);
    tokio::pin!(stop);
    let initialization = initialize(server, id, args, &request, reservation, async {
        tokio::select! {
            () = &mut stop => {}
            () = cancellation.cancelled() => {}
        }
    })
    .await;
    let (target, mut session, opened) = match initialization {
        Ok(started) => started,
        Err(error) => {
            let _ = reply.send(Err(error.into()));
            return;
        }
    };
    let mut open = match opened {
        Ok(open) => open,
        Err(error) => {
            let _ = session.close_bounded().await;
            let _ = reply.send(Err(error.into()));
            return;
        }
    };
    let stop = if Instant::now() >= open.idle_deadline {
        Some(Stop::Expired)
    } else if cancellation.is_cancelled()
        || owner.lifetime.is_cancelled()
        || server.transactions.shutdown.is_cancelled()
    {
        Some(Stop::Cancelled)
    } else {
        None
    };
    let mut end = if let Some(stop) = stop {
        End { request, reply: Some(reply), ..End::automatic(stop) }
    } else if reply.send(Ok(json!({
        "request_id": request, "transaction_id": id, "connection_alias": target.alias,
        "database": target.database, "isolation_level": "serializable",
        "expires_at": timestamp(open.deadline), "idle_expires_at": timestamp(open.idle_deadline),
        "state": "open",
    }))).is_err() {
        End::automatic(Stop::Cancelled)
    } else {
        manage(server, owner, &target, &mut open, &mut commands).await
    };
    let (state, audit_warning) = terminate(server, owner, &target, &open, &mut end).await;
    server
        .transactions
        .terminal(owner, target.id, state, audit_warning);
    commands.close();
    let cleanup_warning = !session.close_bounded().await;
    while let Ok(command) = commands.try_recv() {
        let _ = command.reply.send(Err(Failure {
            error: terminal_error(state),
            state: Some(state),
            audit_warning,
        }));
    }
    complete(&target, &open, end, state, audit_warning, cleanup_warning);
}

async fn initialize(
    server: &Arc<ServerContext>,
    id: Uuid,
    args: Begin,
    request: &str,
    reservation: Reservation,
    stop: impl Future<Output = ()>,
) -> Result<(Target, Disposable, Result<Open>)> {
    let Begin {
        connection_alias,
        database,
        _isolation_level: _,
        timeout_seconds,
    } = args;
    let seconds = timeout_seconds.unwrap_or(LIFETIME_SECONDS);
    if !(1..=LIFETIME_SECONDS).contains(&seconds)
        || !crate::policy::write::identifier(&connection_alias)
        || !crate::policy::write::identifier(&database)
    {
        return Err(invalid("Invalid transaction target or timeout"));
    }
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let access = server.access();
    let setup_deadline = deadline.min(Instant::now() + access.limits().query_timeout);
    tokio::pin!(stop);
    let (target, params) = tokio::select! {
        biased;
        () = &mut stop => return Err(cancelled()),
        result = timeout_at(setup_deadline, async {
            let (mut profile, revision) = access.guard().transaction_profile(connection_alias.clone()).await?;
            access.require_write_profile(&profile)?;
            match profile.params.driver_id.as_str() {
                "sqlite" if database == "main" => {}
                "mariadb" if write_preflight::mariadb_target_segment(&database) => {
                    profile.params.database = Some(database.clone());
                }
                _ => return Err(invalid("The driver does not support the specified transaction database")),
            }
            Ok::<_, Error>((Target {
                id, alias: connection_alias, database, profile: profile.profile_id,
                profile_revision: profile.revision, document_revision: revision,
            }, profile.params))
        }) => result.map_err(|_| expired())??,
    };
    let session = tokio::select! {
        biased;
        () = &mut stop => Err(cancelled()),
        result = timeout_at(setup_deadline, async {
            let _permit = access.acquire_permit().await?;
            let mut intent = Event::lifecycle("transaction_begin", "begin", "starting");
            intent.phase = "intent";
            if !target.audit(server, request, intent).await {
                return Err(write::audit_unavailable());
            }
            target.reauthorize(server, OperationType::Insert).await?;
            access.sessions().connect_isolated(target.profile.to_string(), params).await
        }) => result.unwrap_or_else(|_| Err(expired())),
    };
    let session = match session {
        Ok(handle) => Disposable {
            handle: Some(handle),
            reservation: Some(reservation),
        },
        Err(error) => {
            target
                .audit(
                    server,
                    request,
                    Event {
                        error: Some(&error),
                        ..Event::lifecycle("transaction_begin", "begin", "not_started")
                    },
                )
                .await;
            return Err(error);
        }
    };
    let handle = session
        .handle
        .as_ref()
        .ok_or_else(|| Error::internal("The transaction connection has not been initialized"))?;
    let result = start(server, &target, handle.clone(), deadline, stop).await;
    let open = match result {
        Ok(open) => open,
        Err(error) => {
            let mut event = Event::lifecycle("transaction_begin", "begin", "not_started");
            event.error = Some(&error);
            target.audit(server, request, event).await;
            return Ok((target, session, Err(error)));
        }
    };
    if !target
        .audit(
            server,
            request,
            Event::lifecycle("transaction_begin", "begin", "open"),
        )
        .await
    {
        let error = write::audit_unavailable();
        let _ = rollback(server, &target, &open, request, Some(&error)).await;
        return Ok((target, session, Err(error)));
    }
    Ok((target, session, Ok(open)))
}

async fn start(
    server: &ServerContext,
    target: &Target,
    handle: SessionHandle,
    deadline: Instant,
    stop: impl Future<Output = ()>,
) -> Result<Open> {
    let access = server.access();
    let budget = deadline.min(Instant::now() + access.limits().query_timeout);
    tokio::pin!(stop);
    let _permit = tokio::select! {
        biased;
        () = &mut stop => return Err(cancelled()),
        result = timeout_at(budget, async {
            let permit = access.acquire_permit().await?;
            let (snapshot, _lock) = access.guard().transaction_snapshot().await?;
            target.check(&snapshot)?;
            Ok::<_, Error>(permit)
        }) => result.map_err(|_| expired())??,
    };
    let driver = write_transaction::begin(&handle, false, budget, stop).await?;
    Ok(Open {
        handle,
        driver_id: driver.transaction_id,
        deadline,
        idle_deadline: deadline.min(Instant::now() + IDLE),
        operations: HashSet::new(),
        rows: 0,
        failed: false,
    })
}

struct Disposable {
    handle: Option<SessionHandle>,
    reservation: Option<Reservation>,
}

impl Disposable {
    fn start_close(&mut self) -> tokio::task::JoinHandle<bool> {
        let handle = self.handle.take();
        let reservation = self.reservation.take();
        tokio::spawn(async move {
            let _reservation = reservation;
            if let Some(handle) = handle {
                close_connection(handle.close(), handle.abort_isolated()).await
            } else {
                true
            }
        })
    }

    async fn close_bounded(&mut self) -> bool {
        match timeout(BUDGET, self.start_close()).await {
            Ok(Ok(closed)) => closed,
            Ok(Err(error)) => {
                tracing::error!(%error, "The MCP transaction connection cleanup task failed");
                false
            }
            Err(_) => {
                tracing::warn!(
                    "Closing the MCP transaction connection timed out. The connection remains isolated and retains its resource quota"
                );
                false
            }
        }
    }
}

impl Drop for Disposable {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.start_close();
        }
    }
}

async fn close_connection(
    close: impl Future<Output = Result<()>>,
    abort: impl Future<Output = bool>,
) -> bool {
    if let Err(error) = close.await {
        tracing::error!(code = %error.code(), "Failed to close the MCP transaction connection");
        abort.await;
        return false;
    }
    true
}

#[derive(Clone, Copy)]
enum Stop {
    Expired,
    Cancelled,
    Revoked,
}

impl Stop {
    fn error(self) -> Error {
        match self {
            Self::Expired => expired(),
            Self::Cancelled => cancelled(),
            Self::Revoked => {
                denied("Transaction authorization or the connection profile was revoked")
            }
        }
    }
}

async fn service_stop(server: &ServerContext, owner: &Owner) {
    tokio::select! {
        () = server.transactions.shutdown.cancelled() => {}
        () = owner.lifetime.cancelled() => {}
    }
}

async fn policy_stop(server: &ServerContext, target: &Target) {
    loop {
        tokio::time::sleep(POLICY_INTERVAL).await;
        match server.access().guard().transaction_snapshot().await {
            Ok((doc, _lock)) if target.check(&doc).is_ok() => {}
            _ => return,
        }
    }
}

async fn monitor(
    server: &ServerContext,
    owner: &Owner,
    target: &Target,
    deadline: Instant,
) -> Stop {
    tokio::select! {
        biased;
        () = service_stop(server, owner) => Stop::Cancelled,
        () = tokio::time::sleep_until(deadline) => Stop::Expired,
        () = policy_stop(server, target) => Stop::Revoked,
    }
}

struct End {
    request: String,
    reply: Option<Reply>,
    cause: Option<Error>,
    stop: Option<Stop>,
    commit: bool,
    cancellation: Option<CancellationToken>,
}

impl End {
    fn automatic(stop: Stop) -> Self {
        Self {
            request: Uuid::new_v4().to_string(),
            reply: None,
            cause: Some(stop.error()),
            stop: Some(stop),
            commit: false,
            cancellation: None,
        }
    }
}

async fn manage(
    server: &ServerContext,
    owner: &Owner,
    target: &Target,
    open: &mut Open,
    commands: &mut mpsc::Receiver<Command>,
) -> End {
    loop {
        let command = tokio::select! {
            biased;
            reason = monitor(server, owner, target, open.idle_deadline) => return End::automatic(reason),
            command = commands.recv() => match command {
                Some(command) => command,
                None => return End::automatic(Stop::Cancelled),
            },
        };
        if command.tool == "rollback_transaction"
            || (command.tool == "commit_transaction" && !open.failed)
        {
            return End {
                request: command.request_id,
                reply: Some(command.reply),
                stop: None,
                cause: (command.tool == "commit_transaction"
                    && command.cancellation.is_cancelled())
                .then(cancelled),
                commit: command.tool == "commit_transaction"
                    && !command.cancellation.is_cancelled(),
                cancellation: Some(command.cancellation),
            };
        }
        let result = if open.failed {
            Err(Error::failed_precondition(
                ErrorCode::TRANSACTION_FAILED,
                "The transaction failed and must be rolled back",
            ))
        } else {
            execute(server, owner, target, open, &command).await
        };
        if let Err(error) = result {
            let stop = if Instant::now() >= open.deadline {
                Some(Stop::Expired)
            } else if command.cancellation.is_cancelled()
                || owner.lifetime.is_cancelled()
                || server.transactions.shutdown.is_cancelled()
            {
                Some(Stop::Cancelled)
            } else if error.code() == ErrorCode::MCP_CONNECTION_DENIED {
                Some(Stop::Revoked)
            } else {
                None
            };
            if stop.is_some() || open.rows > MAX_ROWS {
                return End {
                    request: command.request_id,
                    reply: Some(command.reply),
                    cause: Some(error),
                    stop,
                    commit: false,
                    cancellation: None,
                };
            }
            let _ = command.reply.send(Err(Failure {
                error,
                state: Some(open.state()),
                audit_warning: false,
            }));
        } else if command.reply.send(result.map_err(Failure::from)).is_err() {
            return End {
                request: command.request_id,
                ..End::automatic(Stop::Cancelled)
            };
        }
    }
}

async fn execute(
    server: &ServerContext,
    owner: &Owner,
    target: &Target,
    open: &mut Open,
    command: &Command,
) -> Result<Value> {
    let mut arguments = command.arguments.clone();
    let operation_id = arguments
        .remove("operation_id")
        .and_then(|value| value.as_str().map(str::to_owned))
        .filter(|value| {
            !value.is_empty() && value.len() <= 64 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| invalid("Invalid operation_id"))?;
    if arguments.contains_key("connection_alias") || arguments.contains_key("database") {
        return Err(invalid(
            "Transaction writes use the connection and database selected at BEGIN",
        ));
    }
    arguments.insert("connection_alias".into(), json!(target.alias));
    arguments.insert("database".into(), json!(target.database));
    let plan = WritePlan::decode(Value::Object(arguments), None)?;
    let op = plan.action.operation();
    let operation = write::label(op);
    let mut event = Event::lifecycle("transaction_execute", operation, open.state());
    let mut affected = None;
    let result = execute_checked(
        server,
        owner,
        target,
        open,
        command,
        &plan,
        &operation_id,
        &mut affected,
    )
    .await;
    event.state = open.state();
    event.rows = affected;
    event.error = result.as_ref().err();
    if !target.audit(server, &command.request_id, event).await {
        open.failed = true;
        return Err(result.err().unwrap_or_else(write::audit_unavailable));
    }
    if Instant::now() >= open.deadline {
        return Err(expired());
    }
    if command.cancellation.is_cancelled()
        || owner.lifetime.is_cancelled()
        || server.transactions.shutdown.is_cancelled()
    {
        return Err(cancelled());
    }
    let rows = result?;
    open.idle_deadline = open.deadline.min(Instant::now() + IDLE);
    Ok(json!({
        "request_id": command.request_id, "transaction_id": target.id,
        "operation_id": operation_id, "operation": operation, "affected_rows": rows,
        "count_kind": if op == OperationType::Update { "matched" } else { "direct" },
        "operation_count": open.operations.len(), "accumulated_affected_rows": open.rows,
        "idle_expires_at": timestamp(open.idle_deadline), "state": "open",
    }))
}

#[allow(clippy::too_many_arguments)] // The same state and row counter are used for the result audit.
async fn execute_checked(
    server: &ServerContext,
    owner: &Owner,
    target: &Target,
    open: &mut Open,
    command: &Command,
    plan: &WritePlan,
    operation_id: &str,
    affected: &mut Option<u64>,
) -> Result<u64> {
    if open.operations.contains(operation_id) {
        return Err(Error::already_exists(
            ErrorCode::from_static("mcp.operation_already_used"),
            "operation_id has already been used",
        ));
    }
    if open.operations.len() >= MAX_OPERATIONS {
        return Err(Error::resource_exhausted(
            ErrorCode::MCP_INVALID_ARGUMENT,
            "The transaction has reached its DML operation limit",
        ));
    }
    let statement = write_transaction::prepare(&open.handle, plan)?;
    let access = server.access();
    let deadline = open
        .deadline
        .min(Instant::now() + access.limits().query_timeout);
    let cancellation = async {
        tokio::select! {
            () = command.cancellation.cancelled() => Stop::Cancelled,
            reason = monitor(server, owner, target, open.deadline) => reason,
        }
    };
    tokio::pin!(cancellation);
    let _permit = tokio::select! {
        biased;
        reason = &mut cancellation => return Err(reason.error()),
        result = timeout_at(deadline, async {
            let permit = access.acquire_permit().await?;
            let mut intent = Event::lifecycle("transaction_execute", write::label(plan.action.operation()), "open");
            intent.phase = "intent";
            if !target.audit(server, &command.request_id, intent).await {
                return Err(write::audit_unavailable());
            }
            target.reauthorize(server, plan.action.operation()).await?;
            Ok::<_, Error>(permit)
        }) => result.map_err(|_| Error::new(
            ErrorCode::MCP_QUERY_TIMEOUT, ErrorCategory::Timeout, "The write exceeded its execution deadline"
        ))??,
    };
    open.operations.insert(operation_id.to_owned());
    let mut reason = None;
    let stop = async {
        reason = Some(cancellation.await);
    };
    tokio::pin!(stop);
    let result = write_transaction::execute_statement(
        &open.handle,
        access.queries(),
        plan,
        statement,
        deadline,
        stop.as_mut(),
        async |handle, plan, query_id| {
            let rows = write::inspect(handle, plan, query_id)
                .await?
                .allowed_rows()?;
            target.reauthorize(server, plan.action.operation()).await?;
            Ok(Some(rows))
        },
        affected,
    )
    .await;
    if let Some(count) = *affected {
        open.rows = open.rows.saturating_add(count);
    }
    let result = reason
        .map_or(result, |reason| Err(reason.error()))
        .and_then(|rows| {
            if open.rows > MAX_ROWS {
                Err(Error::resource_exhausted(
                    ErrorCode::MCP_INVALID_ARGUMENT,
                    "The transaction exceeds the cumulative affected row limit",
                ))
            } else {
                Ok(rows)
            }
        });
    if result.is_err() {
        open.failed = true;
    }
    result
}

async fn terminate(
    server: &ServerContext,
    owner: &Owner,
    target: &Target,
    open: &Open,
    end: &mut End,
) -> (&'static str, bool) {
    if end.commit {
        match commit(server, owner, target, open, end).await {
            Ok(outcome) => return outcome,
            Err(error) => {
                if error.code() == ErrorCode::MCP_CONNECTION_DENIED {
                    end.stop = Some(Stop::Revoked);
                } else if error.code() == ErrorCode::QUERY_CANCELLED {
                    end.stop = Some(Stop::Cancelled);
                } else if error.code() == expired().code() {
                    end.stop = Some(Stop::Expired);
                }
                end.cause = Some(error);
            }
        }
    }
    let (state, audit_warning) =
        rollback(server, target, open, &end.request, end.cause.as_ref()).await;
    let mut warning = audit_warning;
    if let Some(stop) = end.stop {
        let name = if matches!(stop, Stop::Expired) {
            "transaction_expired"
        } else {
            "transaction_cancelled"
        };
        let mut event = Event::lifecycle(name, "rollback", state);
        event.error = end.cause.as_ref();
        warning |= !target.audit(server, &end.request, event).await;
    }
    (state, warning)
}

fn complete(
    target: &Target,
    open: &Open,
    end: End,
    state: &'static str,
    audit_warning: bool,
    cleanup_warning: bool,
) {
    if let Some(reply) = end.reply {
        let error = if matches!(state, "commit_unknown" | "rollback_unconfirmed") {
            Some(terminal_error(state))
        } else {
            end.cause
        };
        let result = match error {
            Some(error) => Err(Failure {
                error,
                state: Some(state),
                audit_warning,
            }),
            None => Ok(json!({
                "request_id": end.request, "transaction_id": target.id, "state": state,
                "operation_count": open.operations.len(), "accumulated_affected_rows": open.rows,
                "audit_warning": audit_warning, "cleanup_warning": cleanup_warning,
            })),
        };
        let _ = reply.send(result);
    }
}

async fn commit(
    server: &ServerContext,
    owner: &Owner,
    target: &Target,
    open: &Open,
    end: &End,
) -> Result<(&'static str, bool)> {
    let deadline = open.idle_deadline.min(open.deadline);
    let cancel = async {
        if let Some(token) = &end.cancellation {
            token.cancelled().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let (lock, _permit) = tokio::select! {
        biased;
        () = service_stop(server, owner) => return Err(cancelled()),
        () = cancel => return Err(cancelled()),
        result = timeout_at(deadline, async {
            let permit = server.access().acquire_permit().await?;
            let mut event = Event::lifecycle("transaction_commit", "commit", "committing");
            event.phase = "intent";
            event.rows = Some(open.rows);
            if !target.audit(server, &end.request, event).await {
                return Err(write::audit_unavailable());
            }
            let (document, lock) = server.access().guard().transaction_snapshot().await
                .map_err(|_| denied("Could not verify authorization to commit the transaction"))?;
            target.check(&document)?;
            Ok::<_, Error>((lock, permit))
        }) => result.map_err(|_| expired())??,
    };
    if Instant::now() >= deadline {
        return Err(expired());
    }
    if owner.lifetime.is_cancelled()
        || server.transactions.shutdown.is_cancelled()
        || end
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(cancelled());
    }
    // The shared profile lock covers authorization through dispatch and terminal acknowledgement.
    let confirmed = write_transaction::finish(
        &open.handle,
        open.handle.commit_transaction(&open.driver_id),
        BUDGET,
    )
    .await;
    drop(lock);
    let (state, name) = if confirmed {
        ("committed", "transaction_commit")
    } else {
        ("commit_unknown", "transaction_commit_unknown")
    };
    let error = (!confirmed).then(|| terminal_error(state));
    let mut event = Event::lifecycle(name, "commit", state);
    event.rows = Some(open.rows);
    event.error = error.as_ref();
    let written = target.audit(server, &end.request, event).await;
    Ok((state, !written))
}

async fn rollback(
    server: &ServerContext,
    target: &Target,
    open: &Open,
    request: &str,
    cause: Option<&Error>,
) -> (&'static str, bool) {
    let confirmed = write_transaction::finish(
        &open.handle,
        open.handle.rollback_isolated_transaction(&open.driver_id),
        BUDGET,
    )
    .await;
    let (state, name) = if confirmed {
        ("rolled_back", "transaction_rollback")
    } else {
        ("rollback_unconfirmed", "transaction_rollback_unconfirmed")
    };
    let error = (!confirmed).then(|| terminal_error(state));
    let mut event = Event::lifecycle(name, "rollback", state);
    event.rows = Some(open.rows);
    event.error = error.as_ref().or(cause);
    let written = target.audit(server, request, event).await;
    (state, !written)
}

fn timestamp(deadline: Instant) -> Option<String> {
    let duration = deadline.saturating_duration_since(Instant::now());
    (OffsetDateTime::now_utc() + duration).format(&Rfc3339).ok()
}

fn expired() -> Error {
    Error::new(
        ErrorCode::from_static("mcp.transaction_expired"),
        ErrorCategory::Timeout,
        "The transaction or operation deadline expired",
    )
}

#[cfg(test)]
mod tests;
