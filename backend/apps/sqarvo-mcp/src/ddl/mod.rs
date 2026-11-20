//! Independent ownership and durable terminal state for schema changes.

mod ledger;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result, TransactionId};
use sqarvo_connection::SessionHandle;
use sqarvo_driver_api::ddl::{Acknowledgment, Action, Dispatch, Table};
use sqarvo_driver_api::{ExecuteRequest, IsolationLevel, TransactionOptions};
use sqarvo_local_store::credentials::CredentialStore;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::access::{AuthorizedContext, OperationType};
use crate::audit::{AuditRecord, DdlAudit};
use crate::policy::ddl::{self as policy, Effect, Request};
use crate::server::ServerContext;

const SETUP: Duration = Duration::from_secs(5);
const PREFLIGHT: Duration = Duration::from_secs(8);
const DISPATCH: Duration = Duration::from_millis(250);
const EXECUTE: Duration = Duration::from_secs(30);
const TERMINAL: Duration = Duration::from_secs(5);
const CLOSE_GRACE: Duration = Duration::from_secs(4);

pub(crate) struct Service {
    previews: Semaphore,
    mutations: Semaphore,
    shutdown: CancellationToken,
    preview_cleanup_failed: AtomicBool,
    credentials: Option<Arc<dyn CredentialStore>>,
}

impl Default for Service {
    fn default() -> Self {
        Self {
            previews: Semaphore::new(2),
            mutations: Semaphore::new(1),
            shutdown: CancellationToken::new(),
            preview_cleanup_failed: AtomicBool::new(false),
            credentials: None,
        }
    }
}

impl Service {
    #[cfg(test)]
    pub(crate) fn use_test_credentials(&mut self, credentials: Arc<dyn CredentialStore>) {
        self.credentials = Some(credentials);
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown.cancel();
        let _ = timeout(EXECUTE + TERMINAL * 3, async {
            let _preview = self.previews.acquire_many(2).await;
            let _mutation = self.mutations.acquire().await;
        })
        .await;
    }
}

pub(crate) struct Call {
    pub tool: &'static str,
    pub id: String,
    pub started: Instant,
    pub request: Request,
    pub cancelled: CancellationToken,
    pub owner: CancellationToken,
}

struct Outcome {
    state: &'static str,
    effect: &'static str,
    error: Option<Error>,
}

impl Outcome {
    fn rejected(state: &'static str, error: Error) -> Self {
        Self {
            state,
            effect: if state == "outcome_unknown" {
                "unknown"
            } else {
                "none"
            },
            error: Some(error),
        }
    }

    fn completed(effect: &'static str) -> Self {
        Self {
            state: "completed",
            effect,
            error: None,
        }
    }

    fn unknown() -> Self {
        Self::rejected(
            "outcome_unknown",
            Error::failed_precondition(
                ErrorCode::MCP_DDL_OUTCOME_UNKNOWN,
                "结构变更结果待核查，不要重复执行",
            ),
        )
    }
}

pub(crate) fn worker_failure(call: &Call) -> Value {
    let outcome = if call.request.id.is_some() {
        Outcome::unknown()
    } else {
        Outcome::rejected("not_started", Error::internal("DDL 预览执行任务中断"))
    };
    let mut result = payload(call, &outcome);
    result["audit_warning"] = json!(true);
    result
}

pub(crate) async fn run(server: Arc<ServerContext>, call: Call) -> Value {
    let preview = call.request.id.is_none();
    let permit = if preview {
        &server.ddl.previews
    } else {
        &server.ddl.mutations
    }
    .try_acquire();
    let result = match permit {
        Ok(_permit) if preview => {
            if server.ddl.preview_cleanup_failed.load(Ordering::Acquire) {
                Err(Error::failed_precondition(
                    ErrorCode::MCP_DDL_BUSY,
                    "先前的预览会话尚未确认关闭",
                ))
            } else {
                preview_schema(&server, &call).await
            }
        }
        Ok(_permit) => mutate(&server, &call).await,
        Err(_) => Err(Error::failed_precondition(
            ErrorCode::MCP_DDL_BUSY,
            "DDL 并发额度已满",
        )),
    };
    match result {
        Ok(response) => response,
        Err(error) => {
            let outcome = Outcome::rejected("not_started", error);
            let audit_warning = !audit(&server, &call, "terminal", &outcome).await;
            let mut response = payload(&call, &outcome);
            response["audit_warning"] = json!(audit_warning);
            response
        }
    }
}

async fn authorize(server: &ServerContext, call: &Call) -> Result<AuthorizedContext> {
    server
        .access()
        .guard()
        .authorize_request(
            if call.request.id.is_some() {
                OperationType::Ddl
            } else {
                OperationType::Read
            },
            Some(call.request.alias.clone()),
        )
        .await
}

async fn mutate(server: &ServerContext, call: &Call) -> Result<Value> {
    let auth = stop_before(server, call, SETUP, authorize(server, call)).await?;
    let profile = auth
        .profile
        .as_ref()
        .ok_or_else(|| Error::internal("DDL 缺少档案"))?;
    let id = call
        .request
        .id
        .ok_or_else(|| Error::internal("DDL 缺少操作 ID"))?;
    let canonical =
        serde_json::to_vec(&(profile.profile_id, profile.revision, &call.request.change))
            .map_err(|_| Error::internal("无法规范化 DDL 参数"))?;
    let directory = server.audit().dir().to_owned();
    let target = target(call);
    let credentials = server.ddl.credentials.clone();
    let (slot, reservation) = stop_before(server, call, TERMINAL, async {
        tokio::task::spawn_blocking(move || {
            let slot = ledger::Slot::acquire(&directory)?;
            let credentials = credentials.unwrap_or_else(|| {
                sqarvo_local_store::credentials::open_credential_store("mcp-ddl")
            });
            let reservation = slot.reserve(id, &canonical, &target, credentials.as_ref())?;
            Ok((slot, reservation))
        })
        .await
        .map_err(|_| Error::internal("DDL 持久化任务中断"))?
    })
    .await?;
    if let ledger::Reservation::Existing(mut response) = reservation {
        response["replayed"] = json!(true);
        let state = match response["state"].as_str() {
            Some("completed") => "completed",
            Some("failed") => "failed",
            Some("not_started") => "not_started",
            _ => "outcome_unknown",
        };
        let mut record = audit_record(call, "replay", state);
        if let Some(error) = response.get("error") {
            record.success = false;
            record.error_code = error["category"].as_str().map(str::to_owned);
            record.error_subcode = error["code"].as_str().map(str::to_owned);
        }
        if !write_audit(server, &record).await {
            response["audit_warning"] = json!(true);
        }
        return Ok(response);
    }

    let (outcome, cleanup_warning) = match open(server, call, &auth).await {
        Ok(handle) => {
            let outcome = execute(server, call, &auth, &handle).await;
            let warning = !close(&handle).await;
            (outcome, warning)
        }
        Err(error) => (Outcome::rejected("not_started", error), false),
    };
    let mut response = payload(call, &outcome);
    response["cleanup_warning"] = json!(cleanup_warning);
    response["audit_warning"] = json!(!audit(server, call, "terminal", &outcome).await);
    let saved = response.clone();
    let unresolved = outcome.state == "outcome_unknown" || cleanup_warning;
    if !matches!(
        timeout(
            TERMINAL,
            tokio::task::spawn_blocking(move || { slot.finish(id, &saved, unresolved) })
        )
        .await,
        Ok(Ok(Ok(())))
    ) {
        response["state_storage_warning"] = json!(true);
    }
    Ok(response)
}

async fn open(
    server: &ServerContext,
    call: &Call,
    auth: &AuthorizedContext,
) -> Result<SessionHandle> {
    let profile = auth
        .profile
        .as_ref()
        .ok_or_else(|| Error::internal("DDL 缺少档案"))?;
    stop_before(
        server,
        call,
        SETUP,
        server.access().session_manager().connect_ddl(
            profile.profile_id.to_string(),
            profile.params.clone(),
            call.request.id.is_none(),
        ),
    )
    .await
}

async fn execute(
    server: &ServerContext,
    call: &Call,
    auth: &AuthorizedContext,
    handle: &SessionHandle,
) -> Outcome {
    let preflight = stop_before(server, call, PREFLIGHT, async {
        let current = authorize(server, call).await?;
        same_authorization(auth, &current)?;
        let syntax = handle.ddl_syntax().ok_or_else(unsupported)?;
        let transactional = syntax.delimiter == '"';
        let transaction = if transactional {
            Some(
                handle
                    .begin_transaction(options(false))
                    .await?
                    .transaction_id,
            )
        } else {
            None
        };
        let inspection = handle.inspect_ddl(call.request.change.clone()).await?;
        let effect = policy::check_inspection(&call.request.change, &inspection, &syntax)?;
        let compiled = policy::compile(&call.request.change, &syntax)?;
        Ok((effect, transaction, compiled))
    })
    .await;
    let (effect, transaction, compiled) = match preflight {
        Ok(plan) => plan,
        Err(error) => return Outcome::rejected("not_started", error),
    };
    match effect {
        Effect::AlreadyExists => return Outcome::completed("already_exists"),
        Effect::AlreadyAbsent => return Outcome::completed("already_absent"),
        Effect::Apply => {}
    }
    let query = match server
        .access()
        .queries()
        .register(handle.summary().session_id)
    {
        Ok(query) => query,
        Err(error) => return Outcome::rejected("not_started", error),
    };
    let outcome = execute_checked(
        server,
        call,
        auth,
        handle,
        transaction,
        ExecuteRequest {
            query_id: query,
            sql: compiled.sql,
            max_rows: Some(1),
            timeout: Some(EXECUTE),
        },
    )
    .await;
    let _ = server.access().queries().complete(&query);
    outcome
}

async fn execute_checked(
    server: &ServerContext,
    call: &Call,
    auth: &AuthorizedContext,
    handle: &SessionHandle,
    transaction: Option<TransactionId>,
    request: ExecuteRequest,
) -> Outcome {
    let intent = Outcome {
        state: "not_started",
        effect: "none",
        error: None,
    };
    if !audit(server, call, "intent", &intent).await {
        return Outcome::rejected("not_started", Error::internal("DDL 意图审计未持久化"));
    }
    let query = request.query_id;
    let dispatch = match gate(server, call, auth, || handle.dispatch_ddl(request)).await {
        Ok(dispatch) => dispatch,
        Err(error) => return Outcome::rejected("not_started", error),
    };
    let acknowledged = tokio::select! {
        biased;
        reply = timeout(EXECUTE, dispatch) => reply.ok(),
        () = stopped(server, call) => None,
    };
    if acknowledged.is_none() {
        let _ = timeout(TERMINAL, handle.cancel(&query)).await;
    }
    match (transaction, acknowledged) {
        (None, Some(Acknowledgment::Confirmed)) => Outcome::completed("applied"),
        (None, Some(Acknowledgment::Rejected(error))) => Outcome::rejected("failed", error),
        (None, _) => Outcome::unknown(),
        (Some(id), Some(Acknowledgment::Confirmed)) => {
            let commit = match gate(server, call, auth, || handle.dispatch_ddl_commit(id)).await {
                Ok(commit) => commit,
                Err(error) => return rollback(handle, id, error).await,
            };
            match timeout(TERMINAL, commit).await {
                Ok(Acknowledgment::Confirmed) => Outcome::completed("applied"),
                Ok(Acknowledgment::Rejected(error)) if error.code().as_str() == "sqlite.busy" => {
                    rollback(handle, id, error).await
                }
                _ => Outcome::unknown(),
            }
        }
        (Some(id), Some(Acknowledgment::Rejected(error) | Acknowledgment::Unconfirmed(error))) => {
            rollback(handle, id, error).await
        }
        (Some(id), None) => rollback(handle, id, cancelled()).await,
    }
}

async fn rollback(handle: &SessionHandle, id: TransactionId, error: Error) -> Outcome {
    match timeout(TERMINAL, handle.rollback_isolated_transaction(&id)).await {
        Ok(Ok(())) => Outcome::rejected("failed", error),
        _ => Outcome::unknown(),
    }
}

async fn close(handle: &SessionHandle) -> bool {
    close_with_abort(handle.close(), handle.abort_isolated()).await
}

async fn close_with_abort(
    orderly: impl Future<Output = Result<()>>,
    abort: impl Future<Output = bool>,
) -> bool {
    let deadline = tokio::time::Instant::now() + TERMINAL;
    if let Ok(Ok(())) = timeout(CLOSE_GRACE, orderly).await {
        true
    } else {
        let _ = tokio::time::timeout_at(deadline, abort).await;
        false
    }
}

async fn gate(
    server: &ServerContext,
    call: &Call,
    auth: &AuthorizedContext,
    dispatch: impl FnOnce() -> Result<Dispatch>,
) -> Result<Dispatch> {
    stop_before(server, call, DISPATCH, async {
        let (current, _lock) = server.access().guard().transaction_snapshot().await?;
        let profile = auth
            .profile
            .as_ref()
            .ok_or_else(|| Error::internal("DDL 缺少档案"))?;
        if !current.mcp.enabled
            || current.mcp.read_only
            || current.revision != auth.document().revision
            || !current.profiles.iter().any(|candidate| {
                candidate.profile_id == profile.profile_id && candidate.revision == profile.revision
            })
        {
            return Err(revoked());
        }
        if is_stopped(server, call) {
            return Err(cancelled());
        }
        dispatch()
    })
    .await
}

async fn preview_schema(server: &ServerContext, call: &Call) -> Result<Value> {
    let auth = stop_before(server, call, SETUP, authorize(server, call)).await?;
    let handle = open(server, call, &auth).await?;
    let result = stop_before(server, call, PREFLIGHT, async {
        let current = authorize(server, call).await?;
        same_authorization(&auth, &current)?;
        let syntax = handle.ddl_syntax().ok_or_else(unsupported)?;
        let transactional = syntax.delimiter == '"';
        if transactional {
            handle.begin_transaction(options(true)).await?;
        }
        let mut blockers = Vec::new();
        let directory = server.audit().dir().to_owned();
        match tokio::task::spawn_blocking(move || ledger::has_unresolved(&directory)).await {
            Ok(Ok(false)) => {},
            Ok(Ok(true)) => blockers.push("mcp.ddl_unresolved".to_owned()),
            _ => blockers.push("ddl_state_unavailable".to_owned()),
        }
        let operation_tool = match call.request.change.action {
            Action::AddColumn { .. } | Action::DropColumn { .. }
            | Action::RenameColumn { .. } | Action::RenameTable { .. } => "alter_table",
            _ => call.request.change.action.operation(),
        };
        if !server.config().tools().allows(operation_tool) {
            blockers.push("tool_disabled".to_owned());
        }
        if current.read_only() {
            blockers.push("read_only".to_owned());
        }
        if auth.profile.as_ref().and_then(|profile| profile.params.properties.get("read_only"))
            .is_some_and(|value| value.trim() == "1" || value.trim().eq_ignore_ascii_case("true")) {
            blockers.push("profile_read_only".to_owned());
        }
        let (snapshot, effect) = match handle.inspect_ddl(call.request.change.clone()).await {
            Ok(snapshot) => {
                let effect = policy::check_inspection(&call.request.change, &snapshot, &syntax);
                (Some(snapshot), effect)
            }
            Err(error) if error.code() == ErrorCode::CAPABILITY_UNSUPPORTED => (None, Err(error)),
            Err(error) => return Err(error),
        };
        let compiled = if matches!(effect, Ok(Effect::Apply)) {
            match policy::compile(&call.request.change, &syntax) {
                Ok(sql) => Some(sql.redacted_sql),
                Err(error) => {
                    blockers.push(error.code().as_str().to_owned());
                    None
                }
            }
        } else {
            if let Err(error) = &effect {
                blockers.push(error.code().as_str().to_owned());
            }
            None
        };
        let current_schema = snapshot.as_ref().and_then(|snapshot| snapshot.table.as_ref())
            .map_or(Value::Null, schema);
        let proposed = if matches!(effect, Ok(Effect::Apply)) && compiled.is_some() {
            proposed_schema(call, &current_schema)
        } else {
            current_schema.clone()
        };
        let mut warnings = warnings(call, transactional);
        if matches!(effect, Ok(Effect::AlreadyExists)) {
            warnings.push("existing_table_definition_not_compared");
        } else if matches!(effect, Ok(Effect::AlreadyAbsent)) {
            warnings.push("target_already_absent");
        }
        Ok(json!({
            "operation": call.request.change.action.operation(), "target": target(call),
            "driver": handle.summary().driver_id.as_str(), "current_schema": current_schema,
            "proposed_schema": proposed, "generated_sql": compiled, "generated_sql_redacted": compiled.is_some(),
            "risk_level": risk(call), "transactional_ddl": transactional, "implicit_commit": !transactional,
            "reversible": false, "warnings": warnings,
            "can_execute": blockers.is_empty(), "blockers": blockers,
        }))
    }).await;
    let cleanup_warning = !close(&handle).await;
    if cleanup_warning {
        server
            .ddl
            .preview_cleanup_failed
            .store(true, Ordering::Release);
    }
    let mut result = result?;
    result["cleanup_warning"] = json!(cleanup_warning);
    result["audit_warning"] =
        json!(!audit(server, call, "preview", &Outcome::completed("none")).await);
    Ok(result)
}

fn schema(table: &Table) -> Value {
    json!({
        "type": "table",
        "columns": table.columns.iter().map(|column| json!({
            "name": column.name, "type": column.kind, "nullable": column.nullable, "primary_key_position": column.primary_key,
        })).collect::<Vec<_>>(),
        "indexes": table.indexes.iter().map(|index| json!({
            "name": index.name, "columns": index.columns, "unique": index.unique,
        })).collect::<Vec<_>>(),
    })
}

fn proposed_schema(call: &Call, current: &Value) -> Value {
    let action = &call.request.change.action;
    let mut proposed = current.clone();
    let column_summary = |column: &sqarvo_driver_api::ddl::Column| {
        json!({
            "name": column.name, "logical_type": column.kind, "nullable": column.nullable,
            "identity": column.identity,
        })
    };
    match action {
        Action::CreateTable {
            columns,
            primary_key,
            unique_constraints,
            ..
        } => json!({
            "type": "table", "name": call.request.change.table,
            "columns": columns.iter().map(column_summary).collect::<Vec<_>>(),
            "primary_key": primary_key, "unique_constraints": unique_constraints,
        }),
        Action::DropTable { .. } => Value::Null,
        Action::RenameTable { new_table } => {
            proposed["name"] = json!(new_table);
            proposed
        }
        _ => {
            if let Some(columns) = proposed["columns"].as_array_mut() {
                match action {
                    Action::AddColumn { column } => columns.push(column_summary(column)),
                    Action::DropColumn { column_name } => columns.retain(|column| {
                        column["name"]
                            .as_str()
                            .is_none_or(|name| !name.eq_ignore_ascii_case(column_name))
                    }),
                    Action::RenameColumn {
                        column_name,
                        new_name,
                    } => {
                        for column in columns.iter_mut().filter(|column| {
                            column["name"]
                                .as_str()
                                .is_some_and(|name| name.eq_ignore_ascii_case(column_name))
                        }) {
                            column["name"] = json!(new_name);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(indexes) = proposed["indexes"].as_array_mut() {
                match action {
                    Action::CreateIndex {
                        index,
                        columns,
                        unique,
                    } => indexes.push(json!({
                        "name": index, "columns": columns, "unique": unique,
                    })),
                    Action::DropIndex { index } => indexes.retain(|entry| {
                        entry["name"]
                            .as_str()
                            .is_none_or(|name| !name.eq_ignore_ascii_case(index))
                    }),
                    Action::RenameColumn {
                        column_name,
                        new_name,
                    } => {
                        for names in indexes
                            .iter_mut()
                            .filter_map(|index| index["columns"].as_array_mut())
                        {
                            for name in names.iter_mut().filter(|name| {
                                name.as_str()
                                    .is_some_and(|name| name.eq_ignore_ascii_case(column_name))
                            }) {
                                *name = json!(new_name);
                            }
                        }
                    }
                    _ => {}
                }
            }
            proposed
        }
    }
}

pub(crate) fn error_payload(error: &Error) -> Value {
    let message = match error.code() {
        ErrorCode::MCP_DDL_OUTCOME_UNKNOWN => {
            "数据库可能已经完成结构变更，请核查目标和审计，勿重试"
        }
        ErrorCode::MCP_DDL_UNRESOLVED => "有待核查的结构变更，本机暂停派发后续 DDL",
        ErrorCode::MCP_DDL_BUSY => "结构变更并发额度已占用",
        ErrorCode::MCP_DDL_OPERATION_CONFLICT => "该操作 ID 已用于另一项结构变更",
        ErrorCode::MCP_CONNECTION_DENIED => "当前 MCP 策略或连接授权拒绝该请求",
        _ => "结构变更请求未获成功确认，请核对错误码和目标",
    };
    json!({
        "error": {
            "code": error.code().as_str(), "category": error.category().as_str(),
            "message": message,
            "retryable": false,
        }
    })
}

fn payload(call: &Call, outcome: &Outcome) -> Value {
    let mut response = json!({
        "operation_id": call.request.id, "operation": call.request.change.action.operation(),
        "target": target(call), "state": outcome.state, "effect": outcome.effect,
        "risk_level": risk(call), "warnings": warnings(call, call.request.change.database == "main"),
        "audit_warning": false, "cleanup_warning": false, "state_storage_warning": false, "replayed": false,
    });
    if let Some(error) = &outcome.error {
        response["error"] = error_payload(error)["error"].clone();
    }
    if outcome.effect == "already_exists" {
        response["warnings"] = json!(["existing_table_definition_not_compared"]);
    }
    response
}

fn target(call: &Call) -> Value {
    let change = &call.request.change;
    let mut result = json!({ "connection_alias": call.request.alias, "database": change.database, "table": change.table });
    if let Some(index) = change.action.index() {
        result["index"] = json!(index);
    }
    if let Action::RenameTable { new_table } = &change.action {
        result["new_table"] = json!(new_table);
    }
    result
}

fn risk(call: &Call) -> &'static str {
    match call.request.change.action {
        Action::DropColumn { .. } | Action::DropTable { .. } => "high",
        Action::CreateTable { .. } => "low",
        _ => "medium",
    }
}

fn warnings(call: &Call, transactional: bool) -> Vec<&'static str> {
    let mut warnings = vec!["possible_lock_wait"];
    if !transactional {
        warnings.push("implicit_commit");
    } else if matches!(
        call.request.change.action,
        Action::CreateTable { .. } | Action::AddColumn { .. }
    ) {
        warnings.push("sqlite_declared_types_use_affinity");
    }
    match call.request.change.action {
        Action::DropColumn { .. } | Action::DropTable { .. } => warnings.push("data_loss"),
        Action::CreateIndex { .. } => warnings.push("possible_table_scan"),
        _ => {}
    }
    if matches!(
        call.request.change.action,
        Action::DropColumn { .. } | Action::AddColumn { .. }
    ) {
        warnings.push("possible_table_rewrite");
    }
    warnings
}

async fn audit(
    server: &ServerContext,
    call: &Call,
    phase: &'static str,
    outcome: &Outcome,
) -> bool {
    let mut record = audit_record(call, phase, outcome.state);
    if let Some(error) = &outcome.error {
        record = record.failed(error);
    }
    write_audit(server, &record).await
}

fn audit_record(call: &Call, phase: &'static str, state: &'static str) -> AuditRecord {
    let change = &call.request.change;
    let mut record = AuditRecord::new(&call.id, call.tool)
        .with_target(Some(&call.request.alias), Some(&change.database))
        .with_elapsed(call.started.elapsed());
    record.ddl = Some(DdlAudit {
        operation_id: call.request.id,
        target_object: change.table.clone(),
        index: change.action.index().map(str::to_owned),
        new_table: match &change.action {
            Action::RenameTable { new_table } => Some(new_table.clone()),
            _ => None,
        },
        ddl_operation: change.action.operation(),
        risk_level: risk(call),
        execution_phase: phase,
        ddl_state: state,
    });
    record
}

async fn write_audit(server: &ServerContext, record: &AuditRecord) -> bool {
    matches!(
        timeout(TERMINAL, server.audit().write(record)).await,
        Ok(Ok(()))
    )
}

fn same_authorization(initial: &AuthorizedContext, current: &AuthorizedContext) -> Result<()> {
    if initial.document().revision != current.document().revision {
        return Err(revoked());
    }
    Ok(())
}

async fn stop_before<T>(
    server: &ServerContext,
    call: &Call,
    budget: Duration,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        () = stopped(server, call) => Err(cancelled()),
        result = timeout(budget, work) => result.map_err(|_| Error::new(
            ErrorCode::MCP_QUERY_TIMEOUT, ErrorCategory::Timeout, "DDL 等待超出预算",
        ))?,
    }
}

async fn stopped(server: &ServerContext, call: &Call) {
    tokio::select! {
        () = call.cancelled.cancelled() => {},
        () = call.owner.cancelled() => {},
        () = server.ddl.shutdown.cancelled() => {},
    }
}

fn is_stopped(server: &ServerContext, call: &Call) -> bool {
    call.cancelled.is_cancelled() || call.owner.is_cancelled() || server.ddl.shutdown.is_cancelled()
}

fn options(read_only: bool) -> TransactionOptions {
    TransactionOptions {
        isolation: IsolationLevel::Serializable,
        read_only,
    }
}

fn revoked() -> Error {
    Error::new(
        ErrorCode::MCP_CONNECTION_DENIED,
        ErrorCategory::PermissionDenied,
        "DDL 授权或档案已变更",
    )
}

fn cancelled() -> Error {
    Error::new(
        ErrorCode::QUERY_CANCELLED,
        ErrorCategory::Cancelled,
        "结构变更请求已取消",
    )
}

fn unsupported() -> Error {
    Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, "连接不支持结构管理")
}

/// Acknowledge local reconciliation of an already stopped DDL operation.
///
/// The operator must have independently verified that the original database command is
/// no longer running. This records reconciliation and retains the original outcome.
pub async fn reconcile(id: uuid::Uuid) -> Result<()> {
    let paths = sqarvo_local_store::AppPaths::discover()?;
    let audit =
        crate::audit::AuditWriter::open(paths.data_dir(), crate::audit::RotationPolicy::default())?;
    let directory = audit.dir().to_owned();
    let (slot, target, response) = tokio::task::spawn_blocking(move || {
        let slot = ledger::Slot::acquire(&directory)?;
        let (target, response) = slot.unresolved(id)?;
        Ok::<_, Error>((slot, target, response))
    })
    .await
    .map_err(|_| Error::internal("DDL 核查任务中断"))??;
    let mut record = AuditRecord::new(id.to_string(), "local_ddl_reconcile").with_target(
        target["connection_alias"].as_str(),
        target["database"].as_str(),
    );
    record.ddl = Some(DdlAudit {
        operation_id: Some(id),
        target_object: target["table"].as_str().unwrap_or_default().to_owned(),
        index: target["index"].as_str().map(str::to_owned),
        new_table: target["new_table"].as_str().map(str::to_owned),
        ddl_operation: "reconcile",
        risk_level: "unknown",
        execution_phase: "operator_reconciled",
        ddl_state: "reviewed",
    });
    audit.write(&record).await?;
    tokio::task::spawn_blocking(move || slot.finish(id, &response, false))
        .await
        .map_err(|_| Error::internal("DDL 核查任务中断"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_local_store::{AppPaths, McpAccessPolicy, ProfileStore};
    use std::path::Path;

    fn operation_id(sequence: u16) -> uuid::Uuid {
        let millis =
            u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000)
                .expect("Unix milliseconds");
        let timestamp = millis.to_be_bytes();
        let mut bytes = [0_u8; 16];
        bytes[..6].copy_from_slice(&timestamp[2..]);
        bytes[6] = 0x70;
        bytes[8] = 0x80;
        bytes[14..].copy_from_slice(&sequence.to_be_bytes());
        uuid::Uuid::from_bytes(bytes)
    }

    async fn sqlite_fixture(base: &Path) -> (Arc<ServerContext>, std::path::PathBuf) {
        let paths = AppPaths::from_roots(base.join("config"), base.to_owned(), base.join("run"));
        let file = base.join("ddl-fixture.sqlite");
        rusqlite::Connection::open(&file)
            .unwrap()
            .execute_batch(
                "CREATE TABLE dependents (id INTEGER PRIMARY KEY, x TEXT);
             CREATE INDEX dependent_x ON dependents(x);
             CREATE TABLE special (id INTEGER PRIMARY KEY, y TEXT GENERATED ALWAYS AS (id || ''));",
            )
            .unwrap();
        let config = base.join("mcp.json");
        std::fs::write(
            &config,
            json!({
                "connections":[{"alias":"fixture","driver_id":"sqlite","host":file,"enabled":true}]
            })
            .to_string(),
        )
        .unwrap();
        let context = ServerContext::bootstrap_with(Some(&config), base)
            .await
            .unwrap();
        ProfileStore::new(&paths)
            .set_mcp_access(McpAccessPolicy {
                enabled: true,
                read_only: false,
            })
            .unwrap();
        (Arc::new(context), file)
    }

    async fn call(
        server: &Arc<ServerContext>,
        tool: &'static str,
        args: Value,
        sequence: u16,
    ) -> Value {
        let mut args = args.as_object().unwrap().clone();
        args.extend(
            json!({"connection_alias":"fixture","database":"main","table":"items"})
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| !args.contains_key(*key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Vec<_>>(),
        );
        let input = if tool == "preview_schema_change" {
            let operation = args.remove("operation").unwrap();
            json!({"operation":operation, "arguments":args})
        } else {
            args.insert("operation_id".into(), json!(operation_id(sequence)));
            Value::Object(args)
        };
        run(
            Arc::clone(server),
            Call {
                tool,
                id: format!("request-{sequence}"),
                started: Instant::now(),
                request: policy::decode(tool, input.as_object().unwrap().clone()).unwrap(),
                cancelled: CancellationToken::new(),
                owner: CancellationToken::new(),
            },
        )
        .await
    }

    #[tokio::test]
    async fn sqlite_schema_changes_and_dependency_guards() {
        let directory = tempfile::tempdir().unwrap();
        let (server, file) = sqlite_fixture(directory.path()).await;
        let independent = rusqlite::Connection::open(file).unwrap();
        let columns = json!({"columns":[
            {"name":"id","type":"integer","primary_key":true,"identity":true},
            {"name":"note","type":"text","default":"literal ' quote ; \\ backslash"},
            {"name":"number","type":"decimal","precision":5,"scale":2,"default":{"decimal":"-123.45"}}
        ]});
        let mut preview_args = columns.clone();
        preview_args["operation"] = json!("create_table");
        let preview = call(&server, "preview_schema_change", preview_args, 1).await;
        assert_eq!(preview["can_execute"], true, "{preview}");
        assert!(
            !preview["generated_sql"]
                .as_str()
                .unwrap()
                .contains("literal")
        );
        assert_eq!(
            independent
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name='items'",
                    [],
                    |row| row.get::<_, u32>(0)
                )
                .unwrap(),
            0
        );
        let created = call(&server, "create_table", columns.clone(), 2).await;
        assert_eq!(created["effect"], "applied", "{created}");
        independent
            .execute("INSERT INTO items DEFAULT VALUES", [])
            .unwrap();
        let observed: String = independent
            .query_row("SELECT note FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(observed, "literal ' quote ; \\ backslash");
        let steps = [
            (
                "alter_table",
                json!({"action":"add_column","column":{"name":"other","type":"text"}}),
            ),
            (
                "alter_table",
                json!({"action":"rename_column","column_name":"other","new_name":"new_col"}),
            ),
            (
                "create_index",
                json!({"index":"new_idx","columns":["new_col","id"]}),
            ),
            ("drop_index", json!({"index":"new_idx"})),
            (
                "alter_table",
                json!({"action":"drop_column","column_name":"new_col"}),
            ),
            (
                "alter_table",
                json!({"action":"rename_table","new_table":"renamed"}),
            ),
            ("drop_table", json!({"table":"renamed"})),
        ];
        for (sequence, (tool, args)) in (3..).zip(steps) {
            let result = call(&server, tool, args, sequence).await;
            assert_eq!(result["effect"], "applied", "{tool}: {result}");
        }
        assert_eq!(independent.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('items','renamed','new_idx')", [], |row| row.get::<_, u32>(0)).unwrap(), 0);
        for (sequence, (tool, args)) in (20..).zip([
            (
                "alter_table",
                json!({"table":"dependents","action":"drop_column","column_name":"x"}),
            ),
            ("drop_table", json!({"table":"special"})),
        ]) {
            let result = call(&server, tool, args, sequence).await;
            assert_eq!(result["state"], "not_started", "{result}");
            assert_eq!(result["error"]["code"], "capability.unsupported");
        }
        independent
            .execute("CREATE VIEW dependent_view AS SELECT * FROM dependents", [])
            .unwrap();
        let result = call(&server, "drop_table", json!({"table":"dependents"}), 21).await;
        assert_eq!(
            result["error"]["code"], "capability.unsupported",
            "{result}"
        );
    }

    #[tokio::test]
    async fn sqlite_audit_intent_failure_and_terminal_audit_warning() {
        let directory = tempfile::tempdir().unwrap();
        let (server, file) = sqlite_fixture(directory.path()).await;
        let independent = rusqlite::Connection::open(file).unwrap();
        let count = |table| {
            independent
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name = ?",
                    [table],
                    |row| row.get::<_, u32>(0),
                )
                .unwrap()
        };
        let path = server.audit().path();
        let args = json!({"columns":[{"name":"id","type":"integer"}]});

        std::fs::create_dir(path).unwrap();
        let not_started = call(&server, "create_table", args.clone(), 25).await;
        assert_eq!(not_started["state"], "not_started", "{not_started}");
        assert_eq!(count("items"), 0);
        std::fs::remove_dir(path).unwrap();

        independent
            .execute_batch("BEGIN; SELECT * FROM dependents;")
            .unwrap();
        let context = Arc::clone(&server);
        let mutation = tokio::spawn(async move { call(&context, "create_table", args, 26).await });
        timeout(Duration::from_secs(3), async {
            while !std::fs::read_to_string(path).is_ok_and(|contents| {
                contents.lines().any(|line| {
                    serde_json::from_str::<Value>(line).is_ok_and(|line| {
                        line["request_id"] == "request-26" && line["execution_phase"] == "intent"
                    })
                })
            }) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        std::fs::rename(path, path.with_extension("before-terminal")).unwrap();
        std::fs::create_dir(path).unwrap();
        independent.execute_batch("ROLLBACK").unwrap();
        let committed = mutation.await.unwrap();
        assert_eq!(committed["state"], "completed", "{committed}");
        assert_eq!(committed["audit_warning"], true, "{committed}");
        assert_eq!(count("items"), 1);
    }

    #[tokio::test]
    async fn sqlite_rechecks_schema_and_revocation_after_preview() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let (server, file) = sqlite_fixture(base).await;
        let independent = rusqlite::Connection::open(file).unwrap();
        let preview = call(
            &server,
            "preview_schema_change",
            json!({
                "operation":"drop_table", "table":"dependents",
            }),
            30,
        )
        .await;
        assert_eq!(preview["can_execute"], true, "{preview}");
        independent
            .execute("CREATE VIEW new_dependency AS SELECT * FROM dependents", [])
            .unwrap();
        let refused = call(&server, "drop_table", json!({"table":"dependents"}), 31).await;
        assert_eq!(
            refused["error"]["code"], "capability.unsupported",
            "{refused}"
        );
        let paths = AppPaths::from_roots(base.join("config"), base.to_owned(), base.join("run"));
        ProfileStore::new(&paths)
            .set_mcp_access(McpAccessPolicy {
                enabled: true,
                read_only: true,
            })
            .unwrap();
        let result = call(
            &server,
            "create_table",
            json!({"columns":[{"name":"id","type":"integer"}]}),
            32,
        )
        .await;
        assert_eq!(result["error"]["code"], "mcp.connection_denied", "{result}");
        let preview = call(
            &server,
            "preview_schema_change",
            json!({
                "operation":"create_table", "columns":[{"name":"id","type":"integer"}],
            }),
            33,
        )
        .await;
        assert_eq!(preview["can_execute"], false, "{preview}");
        assert!(
            preview["blockers"]
                .as_array()
                .unwrap()
                .contains(&json!("read_only"))
        );
    }

    #[tokio::test]
    async fn ddl_profile_lock_orders_revocation_and_releases_before_database_reply() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let (server, _) = sqlite_fixture(base).await;
        let input = json!({
            "connection_alias":"fixture","database":"main","table":"items","operation_id":operation_id(42),
            "columns":[{"name":"id","type":"integer"}],
        });
        let call = Call {
            tool: "create_table",
            id: "gate".into(),
            started: Instant::now(),
            request: policy::decode("create_table", input.as_object().unwrap().clone()).unwrap(),
            cancelled: CancellationToken::new(),
            owner: CancellationToken::new(),
        };
        let authorization = authorize(&server, &call).await.unwrap();
        let (sender, reply) = tokio::sync::oneshot::channel();
        let dispatch = gate(&server, &call, &authorization, || {
            Ok(Box::pin(async move {
                reply
                    .await
                    .unwrap_or_else(|_| Acknowledgment::Unconfirmed(cancelled()))
            }))
        })
        .await
        .unwrap();
        let store = ProfileStore::new(&AppPaths::from_roots(
            base.join("config"),
            base.to_owned(),
            base.join("run"),
        ));
        store
            .set_mcp_access(McpAccessPolicy {
                enabled: true,
                read_only: true,
            })
            .unwrap();
        assert!(matches!(gate(&server, &call, &authorization, || {
            panic!("revoked commands may not enter driver queue")
        }).await, Err(error) if error.code() == ErrorCode::MCP_CONNECTION_DENIED));
        assert!(sender.send(Acknowledgment::Confirmed).is_ok());
        assert!(matches!(dispatch.await, Acknowledgment::Confirmed));
    }

    #[tokio::test]
    async fn sqlite_noop_and_constraint_index_results_are_unambiguous() {
        let directory = tempfile::tempdir().unwrap();
        let (server, file) = sqlite_fixture(directory.path()).await;
        let independent = rusqlite::Connection::open(file).unwrap();
        let create = json!({
            "table":"dependents","columns":[{"name":"other","type":"text"}],
            "if_not_exists":true,
        });
        let unchanged = call(&server, "create_table", create.clone(), 51).await;
        assert_eq!(unchanged["effect"], "already_exists", "{unchanged}");
        assert_eq!(
            unchanged["warnings"],
            json!(["existing_table_definition_not_compared"])
        );
        let mut duplicate = create;
        duplicate["if_not_exists"] = json!(false);
        assert_eq!(
            call(&server, "create_table", duplicate, 52).await["state"],
            "not_started"
        );
        let absent = call(&server, "drop_table", json!({"if_exists":true}), 53).await;
        assert_eq!(absent["effect"], "already_absent", "{absent}");
        let created = call(
            &server,
            "create_index",
            json!({
                "table":"dependents","index":"unique_x","columns":["x"],"unique":true,
            }),
            54,
        )
        .await;
        assert_eq!(created["effect"], "applied", "{created}");
        let refused = call(
            &server,
            "drop_index",
            json!({
                "table":"dependents","index":"unique_x",
            }),
            55,
        )
        .await;
        assert_eq!(refused["error"]["code"], "capability.unsupported");
        let missing = call(
            &server,
            "drop_index",
            json!({
                "table":"dependents","index":"missing_index",
            }),
            56,
        )
        .await;
        assert_eq!(missing["error"]["category"], "not_found");
        independent
            .execute("CREATE VIEW items AS SELECT * FROM dependents", [])
            .unwrap();
        let refused = call(&server, "drop_table", json!({"if_exists":true}), 57).await;
        assert_eq!(refused["error"]["code"], "capability.unsupported");
    }

    #[tokio::test]
    async fn failed_session_close_attempts_abort_and_reports_cleanup_warning() {
        let attempted = AtomicBool::new(false);
        let closed = close_with_abort(
            async { Err(Error::internal("fixture close failure")) },
            async {
                attempted.store(true, Ordering::Release);
                false
            },
        )
        .await;
        assert!(!closed);
        assert!(attempted.load(Ordering::Acquire));
    }
}
