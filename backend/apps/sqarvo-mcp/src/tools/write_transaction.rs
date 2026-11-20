//! Transaction executor for a checked DML statement on a disposable MCP session.

use std::future::Future;
use std::time::Duration;

use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result};
use sqarvo_connection::SessionHandle;
use sqarvo_driver_api::{IsolationLevel, ParameterizedExecuteRequest, TransactionOptions};
use sqarvo_query_engine::QueryRegistry;
use tokio::time::Instant;

use crate::policy::write::{WriteDialect, WritePlan, check_generated_sql, prepare_sql};

/// Separate budget for resolving a transaction after DML has stopped.
pub(crate) const TERMINAL_BUDGET: Duration = Duration::from_secs(5);

/// Outcome retained for write audit and response handling.
pub enum TransactionOutcome {
    /// The transaction never started.
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
    if Instant::now() >= deadline {
        return TransactionOutcome::NotStarted(timeout_error());
    }
    let transaction = match handle
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: false,
        })
        .await
    {
        Ok(transaction) => transaction.transaction_id,
        Err(error) => return TransactionOutcome::NotStarted(error),
    };

    let mut affected = None;
    let result = async {
        let query_id = queries.register(handle.summary().session_id)?;
        let expected = tokio::select! {
            biased;
            () = &mut cancelled => Err(cancelled_error()),
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
            sql,
            params,
            max_rows: None,
            timeout: None,
        });
        tokio::pin!(exec);
        let result = tokio::select! {
            biased;
            () = &mut cancelled => Err(cancelled_error()),
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
            return Err(Error::internal("驱动没有返回单条写入摘要"));
        };
        if statement.has_result_set {
            let _ = handle.close_query(&query_id).await;
            return Err(Error::internal("写入意外返回了结果集"));
        }
        let count = statement
            .rows_affected
            .ok_or_else(|| Error::internal("驱动没有返回受影响行数"))?;
        affected = Some(count);
        if count > plan.action.max_rows() as u64 {
            return Err(Error::resource_exhausted(
                ErrorCode::MCP_INVALID_ARGUMENT,
                "实际影响行数超出写入限额",
            ));
        }
        if expected.is_some_and(|expected| expected != count) {
            return Err(Error::failed_precondition(
                ErrorCode::MCP_INVALID_ARGUMENT,
                "实际影响行数与事务内核对的范围不一致",
            ));
        }
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

fn prepare(
    handle: &SessionHandle,
    plan: &WritePlan,
) -> Result<(String, Vec<sqarvo_driver_api::Value>)> {
    let unsupported =
        || Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, "驱动不支持受控写入");
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
        "写入超过执行时限",
    )
}

fn cancelled_error() -> Error {
    Error::new(
        ErrorCode::QUERY_CANCELLED,
        ErrorCategory::Cancelled,
        "客户端取消了写入",
    )
}
