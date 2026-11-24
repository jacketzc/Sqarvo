//! Transaction executor for a checked DML statement on a disposable MCP session.

use std::future::Future;
use std::time::Duration;

use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result};
use sqarvo_connection::{SessionHandle, TransactionStatus};
use sqarvo_driver_api::{IsolationLevel, ParameterizedExecuteRequest, TransactionOptions};
use sqarvo_query_engine::QueryRegistry;
use tokio::time::Instant;

use crate::policy::write::{WriteDialect, WritePlan, check_generated_sql, prepare_sql};

/// Separate budget for resolving a transaction after DML has stopped.
pub(crate) const TERMINAL_BUDGET: Duration = Duration::from_secs(5);

/// Outcome retained for write audit and response handling.
pub enum TransactionOutcome {
    /// DML was never dispatched, even if BEGIN was sent.
    NotStarted(Error),
    /// The database acknowledged commit; the value is the direct DML count.
    Committed(u64),
    /// The database acknowledged rollback; the optional count is from the reverted statement.
    RolledBack {
        /// Direct DML count, when available.
        affected: Option<u64>,
        /// Error that prevented commit.
        error: Error,
    },
    /// Commit was issued, but its outcome could not be established.
    CommitUnknown,
    /// Rollback failed, so the disposable connection must be closed.
    RollbackUnconfirmed {
        /// Direct DML count, when available.
        affected: Option<u64>,
        /// Error that prevented commit.
        error: Error,
    },
}

/// Execute checked DML once, with target and audit hooks inside the transaction.
///
/// Call this only on the disposable session owned by `AccessService::run_write`.
/// `preflight` must validate the database target, privileges, schema, and predicate scope.
/// `before_commit` must persist the pre-commit audit entry. The outer owner must audit the
/// returned outcome and disconnect the session before returning its concurrency permit.
#[allow(clippy::too_many_lines)] // Keep the cancellable transaction and its single terminal decision together.
pub async fn execute(
    handle: &SessionHandle,
    queries: &QueryRegistry,
    plan: &WritePlan,
    timeout: Duration,
    cancelled: impl Future<Output = ()>,
    preflight: impl AsyncFnOnce(&SessionHandle, &WritePlan, QueryId) -> Result<Option<u64>>,
    before_commit: impl AsyncFnOnce(u64) -> Result<()>,
) -> TransactionOutcome {
    let deadline = Instant::now() + timeout;
    let (sql, params) = match prepare(handle, plan) {
        Ok(statement) => statement,
        Err(error) => return TransactionOutcome::NotStarted(error),
    };
    tokio::pin!(cancelled);
    let transaction = match begin(handle, false, deadline, &mut cancelled).await {
        Ok(transaction) => transaction.transaction_id,
        Err(error) => return TransactionOutcome::NotStarted(error),
    };

    let mut affected = None;
    let result = async {
        let count = execute_statement(
            handle,
            queries,
            plan,
            (sql, params),
            deadline,
            cancelled.as_mut(),
            preflight,
            &mut affected,
        )
        .await?;
        tokio::select! {
            biased;
            () = &mut cancelled => Err(cancelled_error()),
            () = tokio::time::sleep_until(deadline) => Err(timeout_error()),
            result = before_commit(count) => result.map(|()| count),
        }
    }
    .await;

    match result {
        Ok(count) => {
            if finish(
                handle,
                handle.commit_transaction(&transaction),
                TERMINAL_BUDGET,
            )
            .await
            {
                TransactionOutcome::Committed(count)
            } else {
                TransactionOutcome::CommitUnknown
            }
        }
        Err(error) => {
            if finish(
                handle,
                handle.rollback_isolated_transaction(&transaction),
                TERMINAL_BUDGET,
            )
            .await
            {
                TransactionOutcome::RolledBack { affected, error }
            } else {
                TransactionOutcome::RollbackUnconfirmed { affected, error }
            }
        }
    }
}

/// Execute one checked statement inside an already open, exclusively owned transaction.
#[allow(clippy::too_many_arguments)] // Keep the statement budget and driver context explicit.
pub(super) async fn execute_statement(
    handle: &SessionHandle,
    queries: &QueryRegistry,
    plan: &WritePlan,
    statement: PreparedStatement,
    deadline: Instant,
    mut cancelled: std::pin::Pin<&mut impl Future<Output = ()>>,
    preflight: impl AsyncFnOnce(&SessionHandle, &WritePlan, QueryId) -> Result<Option<u64>>,
    affected: &mut Option<u64>,
) -> Result<u64> {
    let query_id = queries.register(handle.summary().session_id)?;
    let expected = tokio::select! {
        biased;
        () = cancelled.as_mut() => Err(cancelled_error()),
        () = tokio::time::sleep_until(deadline) => Err(timeout_error()),
        result = preflight(handle, plan, query_id) => result,
    };
    let _ = queries.complete(&query_id);
    let expected = match expected {
        Ok(expected) => expected,
        Err(error) => {
            finish(handle, handle.cancel(&query_id), TERMINAL_BUDGET).await;
            return Err(error);
        }
    };
    let query_id = queries.register(handle.summary().session_id)?;
    let exec = handle.execute_parameterized(ParameterizedExecuteRequest {
        query_id,
        sql: statement.0,
        params: statement.1,
        max_rows: None,
        timeout: None,
    });
    tokio::pin!(exec);
    let result = tokio::select! {
        biased;
        () = cancelled.as_mut() => Err(cancelled_error()),
        () = tokio::time::sleep_until(deadline) => Err(timeout_error()),
        result = &mut exec => Ok(result),
    };
    let _ = queries.complete(&query_id);
    let start = match result {
        Ok(result) => result?,
        Err(error) => {
            finish(handle, handle.cancel(&query_id), TERMINAL_BUDGET).await;
            return Err(error);
        }
    };
    let [statement] = start.statements.as_slice() else {
        return Err(Error::internal(
            "The driver did not return a single write summary",
        ));
    };
    if statement.has_result_set {
        let _ = handle.close_query(&query_id).await;
        return Err(Error::internal(
            "The write unexpectedly returned a result set",
        ));
    }
    let count = statement
        .rows_affected
        .ok_or_else(|| Error::internal("The driver did not return the affected row count"))?;
    *affected = Some(count);
    if count > plan.action.max_rows() as u64 {
        return Err(Error::resource_exhausted(
            ErrorCode::MCP_INVALID_ARGUMENT,
            "The affected row count exceeds the write limit",
        ));
    }
    if expected.is_some_and(|expected| expected != count) {
        return Err(Error::failed_precondition(
            ErrorCode::MCP_INVALID_ARGUMENT,
            "The affected row count differs from the count verified within the transaction",
        ));
    }
    Ok(count)
}

type PreparedStatement = (String, Vec<sqarvo_driver_api::Value>);

/// Begin a transaction without letting an unacknowledged BEGIN proceed to DML.
pub(super) async fn begin(
    handle: &SessionHandle,
    read_only: bool,
    deadline: Instant,
    cancelled: impl Future<Output = ()>,
) -> Result<TransactionStatus> {
    let error = tokio::select! {
        biased;
        () = cancelled => cancelled_error(),
        () = tokio::time::sleep_until(deadline) => timeout_error(),
        result = handle.begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only,
        }) => return result,
    };
    handle.abort_isolated().await;
    Err(error)
}

// Dropping the response future does not stop a driver actor that received the command.
// Physically abort a disposable actor when its response misses the deadline.
pub(super) async fn finish(
    handle: &SessionHandle,
    command: impl Future<Output = Result<()>>,
    budget: Duration,
) -> bool {
    if let Ok(result) = tokio::time::timeout(budget, command).await {
        result.is_ok()
    } else {
        handle.abort_isolated().await;
        false
    }
}

pub(super) fn prepare(handle: &SessionHandle, plan: &WritePlan) -> Result<PreparedStatement> {
    let unsupported = || {
        Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "The driver does not support controlled writes",
        )
    };
    let dialect = match handle.summary().driver_id.as_str() {
        "sqlite" => WriteDialect::Sqlite,
        "mariadb" => WriteDialect::MariaDb,
        _ => return Err(unsupported()),
    };
    if handle.quote_identifier(&plan.table).is_none() {
        return Err(unsupported());
    }
    let (sql, params) = prepare_sql(plan, dialect, |name| {
        handle.quote_identifier(name).unwrap_or_default()
    })?;
    check_generated_sql(plan, dialect, &sql, &params)?;
    Ok((sql, params))
}

fn timeout_error() -> Error {
    Error::new(
        ErrorCode::MCP_QUERY_TIMEOUT,
        ErrorCategory::Timeout,
        "The write exceeded its execution deadline",
    )
}

fn cancelled_error() -> Error {
    Error::new(
        ErrorCode::QUERY_CANCELLED,
        ErrorCategory::Cancelled,
        "The client canceled the write",
    )
}
