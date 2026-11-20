//! MCP protocol adapter for bounded writes and read-only preview.

use std::future::{Future, pending};
use std::sync::Arc;
use std::time::Instant;

use rmcp::RoleServer;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, Tool, ToolAnnotations};
use rmcp::service::RequestContext;
use serde_json::{Map, Value, json};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result};
use sqarvo_connection::SessionHandle;
use sqarvo_driver_api::{IsolationLevel, TransactionOptions};

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
    if let Err(error) = access
        .guard()
        .authorize_request(OperationType::Read, Some(alias.clone()))
        .await
    {
        return reject(&audit, record, op, started, error).await;
    }
    record = record.with_target(Some(&alias), Some(&plan.database));
    let token = context.ct.child_token();
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
                    lifetime.0.cancelled().await;
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
    if expected.is_none() {
        preview(access, plan, call, cancelled).await
    } else {
        execute_write(access, plan, call, cancelled).await
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
    cancelled: impl Future<Output = ()> + Send + 'static,
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
    let authorized = access
        .guard()
        .authorize_request(op, Some(alias.clone()))
        .await
        .is_ok();
    let database = plan.database.clone();
    let summary = preview_summary(&plan.action);
    let row_limit = plan.action.max_rows();
    let inspection = if authorized {
        access
            .run_write(&alias, op, move |handle| async move {
                let tx = handle
                    .begin_transaction(TransactionOptions {
                        isolation: IsolationLevel::Serializable,
                        read_only: true,
                    })
                    .await?;
                let query_id = QueryId::new();
                let result = tokio::select! {
                    biased;
                    () = cancelled => Err(Error::new(
                        ErrorCode::QUERY_CANCELLED, ErrorCategory::Cancelled, "客户端取消了预览",
                    )),
                    () = tokio::time::sleep(timeout) => Err(Error::new(
                        ErrorCode::MCP_QUERY_TIMEOUT, ErrorCategory::Timeout, "写入预览超时",
                    )),
                    result = inspect(&handle, &plan, query_id) => result,
                };
                if result.is_err() {
                    let _ = handle.cancel(&query_id).await;
                }
                handle.rollback_transaction(&tx.transaction_id).await?;
                result
            })
            .await
            .ok()
    } else {
        None
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
        "summary": summary, "estimated_rows": estimate, "scope": scope,
        "risk_level": risk(op), "row_limit": row_limit, "authorized": authorized,
        "can_execute": authorized && can_execute,
    }))
    .into())
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
    cancelled: impl Future<Output = ()> + Send + 'static,
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
    let result = access
        .run_write(&alias, op, move |handle| async move {
            let outcome = write_transaction::execute(
                &handle,
                &queries,
                &plan,
                timeout,
                cancelled,
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

async fn inspect(
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

const fn label(op: OperationType) -> &'static str {
    match op {
        OperationType::Read => "preview",
        OperationType::Insert => "insert",
        OperationType::Update => "update",
        OperationType::Delete => "delete",
    }
}

const fn risk(op: OperationType) -> &'static str {
    match op {
        OperationType::Read => "none",
        OperationType::Insert => "low",
        OperationType::Update => "medium",
        OperationType::Delete => "high",
    }
}

fn safe_error(error: Error) -> Error {
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

fn audit_unavailable() -> Error {
    Error::new(
        ErrorCode::MCP_INTERNAL,
        ErrorCategory::Internal,
        "无法持久化写入审计记录",
    )
}
