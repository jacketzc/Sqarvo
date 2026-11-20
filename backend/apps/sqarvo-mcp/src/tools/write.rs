//! MCP protocol adapter for bounded writes and read-only preview.

use std::future::pending;
use std::sync::Arc;
use std::time::Instant;

use rmcp::RoleServer;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, Tool, ToolAnnotations};
use rmcp::service::RequestContext;
use serde_json::{Map, Value, json};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result};
use sqarvo_connection::SessionHandle;
use tokio_util::sync::CancellationToken;

use crate::access::{AccessService, OperationType};
use crate::audit::{AuditRecord, AuditWriter, WriteAudit, WritePhase};
use crate::config::V2_TOOL_NAMES;
use crate::error::ToolFailure;
use crate::policy::write::{WriteAction, WriteArguments, WritePlan};
use crate::server::ServerContext;

use super::{write_preflight, write_transaction};
use write_preflight::WriteScope;
use write_transaction::TransactionOutcome;

pub(crate) fn descriptions() -> impl Iterator<Item = Tool> {
    V2_TOOL_NAMES.into_iter().map(|tool| {
        let op = operation(tool);
        let schema = serde_json::to_value(schemars::schema_for!(WriteArguments))
            .expect("static write schema is serializable");
        let has_refs = schema.get("$defs").cloned();
        let mut schema = if let Some(op) = op {
            let mut branch = schema["oneOf"]
                .as_array()
                .and_then(|branches| {
                    branches
                        .iter()
                        .find(|branch| branch["properties"]["operation"]["const"] == label(op))
                })
                .and_then(Value::as_object)
                .cloned()
                .expect("each DML operation has a schema");
            if let Some(Value::Object(properties)) = branch.get_mut("properties") {
                properties.remove("operation");
            }
            if let Some(Value::Array(required)) = branch.get_mut("required") {
                required.retain(|field| field != "operation");
            }
            branch
        } else {
            schema
                .as_object()
                .cloned()
                .expect("write schema is an object")
        };
        if let Some(defs) = has_refs {
            schema.insert("$defs".into(), defs);
        }
        schema.insert("type".into(), json!("object"));
        let description = match op {
            Some(OperationType::Insert) => {
                "Insert up to 100 records in a single bounded transaction."
            }
            Some(OperationType::Update) => {
                "Update up to 50 matched rows under structured AND conditions."
            }
            Some(OperationType::Delete) => {
                "Delete up to 20 matched rows under structured AND conditions."
            }
            _ => {
                "Estimate a structured write without modifying the database or approving execution."
            }
        };
        Tool::new(tool, description, Arc::new(schema)).with_annotations(
            ToolAnnotations::new()
                .read_only(op.is_none())
                .destructive(matches!(
                    op,
                    Some(OperationType::Update | OperationType::Delete)
                ))
                .idempotent(op.is_none())
                .open_world(false),
        )
    })
}

pub(crate) async fn call(
    server: &Arc<ServerContext>,
    tool: &'static str,
    request: CallToolRequestParams,
    context: RequestContext<RoleServer>,
) -> std::result::Result<CallToolResponse, rmcp::ErrorData> {
    let access = server.access();
    let audit = server.audit().clone();
    let request_id = context.id.to_string();
    let mut record = AuditRecord::new(request_id.clone(), tool);
    let started = Instant::now();
    let expected = operation(tool);
    let initial_op = expected.unwrap_or(OperationType::Read);
    if let Err(error) = access.guard().authorize_request(initial_op, None).await {
        return reject(&audit, record, initial_op, started, error).await;
    }
    if let Err(error) = server.config().tools().require(tool) {
        return reject(&audit, record, initial_op, started, error).await;
    }
    let mut arguments: Map<String, Value> = request.arguments.unwrap_or_default();
    if let Some(op) = expected
        && arguments
            .insert("operation".into(), json!(label(op)))
            .is_some()
    {
        return reject(
            &audit,
            record,
            op,
            started,
            invalid("执行工具的操作类型由工具名确定"),
        )
        .await;
    }
    let plan = match WritePlan::decode(Value::Object(arguments), expected) {
        Ok(plan) => plan,
        Err(error) => return reject(&audit, record, initial_op, started, error).await,
    };
    let op = plan.action.operation();
    let alias = plan.connection_alias.clone();
    let profile_access = match access
        .guard()
        .authorize_request(OperationType::Read, Some(alias.clone()))
        .await
    {
        Ok(access) => access,
        Err(error) => return reject(&audit, record, op, started, error).await,
    };
    record = record.with_target(Some(&alias), Some(&plan.database));
    let token = context.ct.child_token();
    let cancellation = token.clone();
    let _cancel_on_drop = token.clone().drop_guard();
    let http_lifetime = context
        .extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<crate::http::HttpSessionLifetime>())
        .cloned();
    let cancelled = async move {
        tokio::select! {
            () = token.cancelled() => {}
            () = async {
                if let Some(lifetime) = http_lifetime {
                    lifetime.0.lifetime.cancelled().await;
                } else {
                    pending::<()>().await;
                }
            } => {}
        }
    };
    let call = WriteCall {
        audit,
        record,
        request_id,
        started,
    };
    let response = async {
        if expected.is_none() {
            preview(
                access,
                plan,
                call,
                !profile_access.read_only(),
                &cancellation,
            )
            .await
        } else {
            execute_write(access, plan, call, &cancellation).await
        }
    };
    tokio::pin!(response);
    tokio::select! {
        result = &mut response => result,
        () = cancelled => {
            cancellation.cancel();
            response.await
        }
    }
}

type Response = std::result::Result<CallToolResponse, rmcp::ErrorData>;

struct WriteCall {
    audit: AuditWriter,
    record: AuditRecord,
    request_id: String,
    started: Instant,
}

async fn preview(
    access: &AccessService,
    plan: WritePlan,
    call: WriteCall,
    authorized: bool,
    cancelled: &CancellationToken,
) -> Response {
    let WriteCall {
        audit,
        record,
        request_id,
        started,
    } = call;
    let alias = plan.connection_alias.clone();
    let op = plan.action.operation();
    let table = plan.table.clone();
    let timeout = access.limits().query_timeout;
    let database = plan.database.clone();
    let summary = preview_summary(&plan.action);
    let row_limit = plan.action.max_rows();
    let inspection = if authorized {
        let token = cancelled.clone();
        access
            .run_write_cancellable(&alias, op, cancelled, move |handle| async move {
                let deadline = tokio::time::Instant::now() + timeout;
                let tx =
                    write_transaction::begin(&handle, true, deadline, token.cancelled()).await?;
                let query_id = QueryId::new();
                let result = tokio::select! {
                    biased;
                    () = token.cancelled() => Err(Error::new(
                        ErrorCode::QUERY_CANCELLED, ErrorCategory::Cancelled, "客户端取消了预览",
                    )),
                    () = tokio::time::sleep_until(deadline) => Err(Error::new(
                        ErrorCode::MCP_QUERY_TIMEOUT, ErrorCategory::Timeout, "写入预览超时",
                    )),
                    result = inspect(&handle, &plan, query_id) => result,
                };
                if result.is_err() {
                    write_transaction::finish(
                        &handle,
                        handle.cancel(&query_id),
                        write_transaction::TERMINAL_BUDGET,
                    )
                    .await;
                }
                if !write_transaction::finish(
                    &handle,
                    handle.rollback_isolated_transaction(&tx.transaction_id),
                    write_transaction::TERMINAL_BUDGET,
                )
                .await
                {
                    return Err(Error::failed_precondition(
                        ErrorCode::MCP_INTERNAL,
                        "写入预览未能确认只读事务已结束",
                    ));
                }
                result
            })
            .await
            .map(Some)
    } else {
        Ok(None)
    };
    let (inspection, scope_reason) = match preview_scope(inspection) {
        Ok(value) => value,
        Err(error) => return reject(&audit, record, op, started, error).await,
    };
    let (estimate, scope, can_execute) = match inspection {
        Some(WriteScope::Bounded(count)) => (Some(count), "estimate", true),
        Some(WriteScope::OverLimit(count)) => (Some(count), "lower_bound", false),
        Some(WriteScope::EntireTable(count)) => (Some(count), "entire_table", false),
        None => (None, "unknown", false),
    };
    if persist(
        &audit,
        record,
        op,
        Some(&table),
        WritePhase::Preview,
        None,
        started,
        None,
    )
    .await
    .is_err()
    {
        return ToolFailure::new(audit_unavailable()).into_call_tool_result();
    }
    Ok(CallToolResult::structured(json!({
        "request_id": request_id, "operation": label(op), "database": database, "table": table,
        "summary": summary, "estimated_rows": estimate, "scope": scope, "scope_reason": scope_reason,
        "risk_level": risk(op), "row_limit": row_limit, "authorized": authorized,
        "can_execute": authorized && can_execute,
    }))
    .into())
}

fn preview_scope(
    inspection: Result<Option<WriteScope>>,
) -> Result<(Option<WriteScope>, Option<&'static str>)> {
    match inspection {
        Ok(scope) => Ok((scope, None)),
        Err(error) if error.code() == ErrorCode::CAPABILITY_UNSUPPORTED => {
            Ok((None, Some(ErrorCode::CAPABILITY_UNSUPPORTED.as_str())))
        }
        Err(error) => Err(safe_error(error)),
    }
}

fn preview_summary(action: &WriteAction) -> Value {
    let conditions = |filter: &sqarvo_driver_api::Filter| {
        filter
            .conditions
            .iter()
            .map(|condition| {
                json!({
                    "column": condition.column,
                    "operator": condition.operator.as_str(),
                })
            })
            .collect::<Vec<_>>()
    };
    match action {
        WriteAction::Insert(rows) => json!({
            "row_count": rows.len(),
            "columns": rows.first().map(|row| row.columns()).unwrap_or_default(),
        }),
        WriteAction::Update { changes, filter } => json!({
            "columns": changes.iter().map(|change| &change.column).collect::<Vec<_>>(),
            "conditions": conditions(filter),
        }),
        WriteAction::Delete(filter) => json!({"conditions": conditions(filter)}),
    }
}

async fn execute_write(
    access: &AccessService,
    plan: WritePlan,
    call: WriteCall,
    cancelled: &CancellationToken,
) -> Response {
    let WriteCall {
        audit,
        record,
        request_id,
        started,
    } = call;
    let alias = plan.connection_alias.clone();
    let op = plan.action.operation();
    let table = plan.table.clone();
    let timeout = access.limits().query_timeout;
    if persist(
        &audit,
        record.clone(),
        op,
        Some(&table),
        WritePhase::Intent,
        None,
        started,
        None,
    )
    .await
    .is_err()
    {
        return ToolFailure::new(audit_unavailable()).into_call_tool_result();
    }
    let queries = Arc::clone(access.queries());
    let owner_audit = audit.clone();
    let owner_record = record.clone();
    let owner_table = table.clone();
    let token = cancelled.clone();
    let result = access
        .run_write_cancellable(&alias, op, cancelled, move |handle| async move {
            let outcome = write_transaction::execute(
                &handle,
                &queries,
                &plan,
                timeout,
                token.cancelled(),
                async |handle, plan, query_id| {
                    inspect(handle, plan, query_id)
                        .await?
                        .allowed_rows()
                        .map(Some)
                },
                async |affected| {
                    persist(
                        &owner_audit,
                        owner_record.clone(),
                        op,
                        Some(&owner_table),
                        WritePhase::Prepared,
                        Some(affected),
                        started,
                        None,
                    )
                    .await
                    .map_err(|_| audit_unavailable())
                },
            )
            .await;
            let (phase, rows, error) = outcome_fields(outcome);
            let audit_warning = persist(
                &owner_audit,
                owner_record,
                op,
                Some(&owner_table),
                phase,
                rows,
                started,
                error.as_ref(),
            )
            .await
            .is_err();
            Ok((phase, rows, error, audit_warning))
        })
        .await;

    match result {
        Ok((phase, rows, None, audit_warning)) => Ok(CallToolResult::structured(json!({
            "request_id": request_id, "operation": label(op), "table": table, "affected_rows": rows,
            "count_kind": if op == OperationType::Update { "matched" } else { "direct" },
            "transaction_state": phase, "audit_warning": audit_warning,
        }))
        .into()),
        Ok((phase, rows, Some(error), audit_warning)) => {
            let error = safe_error(error);
            let mut payload = crate::error::structured_payload(&error);
            payload["request_id"] = json!(request_id);
            payload["transaction_state"] = json!(phase);
            payload["affected_rows"] = json!(rows);
            payload["audit_warning"] = json!(audit_warning);
            payload["error"]["retryable"] = json!(false);
            Ok(CallToolResult::structured_error(payload).into())
        }
        Err(error) => reject(&audit, record, op, started, safe_error(error)).await,
    }
}

fn outcome_fields(outcome: TransactionOutcome) -> (WritePhase, Option<u64>, Option<Error>) {
    match outcome {
        TransactionOutcome::Committed(rows) => (WritePhase::Committed, Some(rows), None),
        TransactionOutcome::CommitUnknown => (
            WritePhase::CommitUnknown,
            None,
            Some(Error::new(
                ErrorCode::MCP_COMMIT_UNKNOWN,
                ErrorCategory::FailedPrecondition,
                "数据库提交结果未知，请核查数据和审计后再决定是否重试",
            )),
        ),
        TransactionOutcome::NotStarted(error) => (WritePhase::NotStarted, None, Some(error)),
        TransactionOutcome::RolledBack { affected, error } => {
            (WritePhase::RolledBack, affected, Some(error))
        }
        TransactionOutcome::RollbackUnconfirmed { affected, error } => {
            (WritePhase::RollbackUnconfirmed, affected, Some(error))
        }
    }
}

pub(super) async fn inspect(
    handle: &SessionHandle,
    plan: &WritePlan,
    query_id: QueryId,
) -> Result<WriteScope> {
    match handle.summary().driver_id.as_str() {
        "sqlite" => write_preflight::sqlite(handle, plan, query_id).await,
        "mariadb" => write_preflight::mariadb(handle, plan, query_id).await,
        _ => Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "驱动不支持受控写入",
        )),
    }
}

#[allow(clippy::too_many_arguments)] // Each audit event records the same stable shape.
async fn persist(
    writer: &AuditWriter,
    mut record: AuditRecord,
    op: OperationType,
    table: Option<&str>,
    phase: WritePhase,
    affected: Option<u64>,
    started: Instant,
    error: Option<&Error>,
) -> Result<()> {
    if let Some(error) = error {
        record = record.failed(error);
    }
    let record = record
        .with_elapsed(started.elapsed())
        .with_write(WriteAudit {
            operation_type: label(op),
            table: table.map(str::to_owned),
            affected_rows: affected,
            counts_matched_rows: op == OperationType::Update,
            transaction_state: phase,
            risk_level: risk(op),
        });
    let result = writer.write(&record).await;
    if let Err(error) = &result {
        tracing::error!(code = %error.code(), "MCP 写入审计失败");
    }
    result
}

async fn reject(
    audit: &AuditWriter,
    record: AuditRecord,
    op: OperationType,
    started: Instant,
    error: Error,
) -> std::result::Result<CallToolResponse, rmcp::ErrorData> {
    let _ = persist(
        audit,
        record,
        op,
        None,
        WritePhase::NotStarted,
        None,
        started,
        Some(&error),
    )
    .await;
    ToolFailure::new(error).into_call_tool_result()
}

fn operation(tool: &str) -> Option<OperationType> {
    match tool {
        "insert_rows" => Some(OperationType::Insert),
        "update_rows" => Some(OperationType::Update),
        "delete_rows" => Some(OperationType::Delete),
        _ => None,
    }
}

pub(super) const fn label(op: OperationType) -> &'static str {
    match op {
        OperationType::Read => "preview",
        OperationType::Insert => "insert",
        OperationType::Update => "update",
        OperationType::Delete => "delete",
        OperationType::Ddl => "ddl",
    }
}

const fn risk(op: OperationType) -> &'static str {
    match op {
        OperationType::Read => "none",
        OperationType::Insert => "low",
        OperationType::Update => "medium",
        OperationType::Delete | OperationType::Ddl => "high",
    }
}

pub(super) fn safe_error(error: Error) -> Error {
    if error.code() == ErrorCode::MCP_COMMIT_UNKNOWN {
        return error;
    }
    match error.code() {
        ErrorCode::MCP_INVALID_ARGUMENT
        | ErrorCode::MCP_QUERY_TIMEOUT
        | ErrorCode::QUERY_CANCELLED
        | ErrorCode::MCP_INTERNAL
        | ErrorCode::MCP_CONNECTION_DENIED
        | ErrorCode::CAPABILITY_UNSUPPORTED => error,
        _ => Error::new(
            error.code(),
            error.category(),
            "数据库拒绝或未能完成本次受控写入",
        ),
    }
}

fn invalid(message: &'static str) -> Error {
    Error::invalid_argument(ErrorCode::MCP_INVALID_ARGUMENT, message)
}

pub(super) fn audit_unavailable() -> Error {
    Error::new(
        ErrorCode::MCP_INTERNAL,
        ErrorCategory::Internal,
        "无法持久化写入审计记录",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_preserves_timeout_and_redacts_database_permission_and_transport_errors() {
        let timeout = Error::new(
            ErrorCode::MCP_QUERY_TIMEOUT,
            ErrorCategory::Timeout,
            "写入预览超时",
        );
        let Err(timeout) = preview_scope(Err(timeout)) else {
            panic!("timeout must be reported")
        };
        assert_eq!(timeout.code(), ErrorCode::MCP_QUERY_TIMEOUT);
        assert_eq!(timeout.category(), ErrorCategory::Timeout);

        for (code, category) in [
            (
                ErrorCode::from_static("mariadb.permission_denied"),
                ErrorCategory::PermissionDenied,
            ),
            (ErrorCode::TRANSPORT_FAILED, ErrorCategory::Unavailable),
        ] {
            let Err(error) = preview_scope(Err(Error::new(
                code,
                category,
                "synthetic sensitive database detail",
            ))) else {
                panic!("infrastructure failure must be reported")
            };
            assert_eq!(error.code(), code);
            assert_eq!(error.category(), category);
            assert!(!error.message().contains("synthetic sensitive"));
        }

        let unsupported = Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, "fixture");
        assert!(matches!(
            preview_scope(Err(unsupported)),
            Ok((None, Some("capability.unsupported")))
        ));
    }

    #[tokio::test]
    async fn unconfirmed_transaction_states_are_preserved_in_audit() {
        let directory = tempfile::tempdir().unwrap();
        let writer =
            AuditWriter::open(directory.path(), crate::audit::RotationPolicy::default()).unwrap();
        for outcome in [
            TransactionOutcome::CommitUnknown,
            TransactionOutcome::RollbackUnconfirmed {
                affected: Some(1),
                error: Error::internal("synthetic sensitive detail"),
            },
        ] {
            let (phase, rows, error) = outcome_fields(outcome);
            persist(
                &writer,
                AuditRecord::new("audit-fixture", "insert_rows"),
                OperationType::Insert,
                Some("fixture"),
                phase,
                rows,
                Instant::now(),
                error.as_ref(),
            )
            .await
            .unwrap();
        }
        let log = std::fs::read_to_string(writer.path()).unwrap();
        for phase in ["commit_unknown", "rollback_unconfirmed"] {
            assert!(log.lines().any(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                value["transaction_state"] == phase && value["success"] == false
            }));
        }
        assert!(!log.contains("synthetic sensitive"));
    }
}
