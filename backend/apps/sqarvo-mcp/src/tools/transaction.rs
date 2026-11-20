//! MCP protocol entry points for explicit transactions.

use std::sync::Arc;

use rmcp::RoleServer;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, Tool, ToolAnnotations};
use rmcp::service::RequestContext;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::write;
use crate::config::TRANSACTION_TOOL_NAMES;
use crate::policy::write::{MAX_ARGUMENT_BYTES, WriteArguments};
use crate::server::ServerContext;
use crate::transactions::{Command, Failure, Owner};

mod session;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Begin {
    connection_alias: String,
    database: String,
    #[serde(rename = "isolation_level")]
    _isolation_level: Option<Isolation>,
    #[schemars(range(min = 1, max = 120))]
    timeout_seconds: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Isolation {
    Serializable,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TransactionArgument {
    #[serde(rename = "transaction_id")]
    _transaction_id: String,
}

pub(crate) fn descriptions() -> impl Iterator<Item = Tool> {
    TRANSACTION_TOOL_NAMES.into_iter().map(|name| {
        let (description, mut schema) = match name {
            "begin_transaction" => ("Begin an isolated transaction for structured writes.", json!(schemars::schema_for!(Begin))),
            "execute_in_transaction" => ("Execute structured DML once. Reuse operation_id if retrying the same logical operation.", execute_schema()),
            "commit_transaction" => ("Commit the transaction once. Never retry an unknown outcome.", json!(schemars::schema_for!(TransactionArgument))),
            _ => ("Roll back an open or failed transaction.", json!(schemars::schema_for!(TransactionArgument))),
        };
        schema["type"] = json!("object");
        Tool::new(name, description, Arc::new(schema.as_object().cloned().unwrap_or_default()))
            .with_annotations(ToolAnnotations::new().read_only(false).idempotent(false).open_world(false))
    })
}

fn execute_schema() -> Value {
    let mut schema = json!(schemars::schema_for!(WriteArguments));
    if let Some(branches) = schema["oneOf"].as_array_mut() {
        for branch in branches {
            if let Some(properties) = branch["properties"].as_object_mut() {
                properties.remove("connection_alias");
                properties.remove("database");
                properties.insert(
                    "transaction_id".into(),
                    json!({"type": "string", "format": "uuid"}),
                );
                properties.insert(
                    "operation_id".into(),
                    json!({"type": "string", "minLength": 1, "maxLength": 64}),
                );
            }
            if let Some(required) = branch["required"].as_array_mut() {
                required.retain(|field| field != "connection_alias" && field != "database");
                required.extend([json!("transaction_id"), json!("operation_id")]);
            }
        }
    }
    schema
}

pub(crate) async fn call(
    server: &Arc<ServerContext>,
    owner: Option<Owner>,
    tool: &'static str,
    request: CallToolRequestParams,
    context: RequestContext<RoleServer>,
) -> std::result::Result<CallToolResponse, rmcp::ErrorData> {
    let cancellation = context.ct.child_token();
    let _cancel_on_drop = cancellation.clone().drop_guard();
    let request_id = context.id.to_string();
    let result = dispatch(
        server,
        owner,
        tool,
        request.arguments.unwrap_or_default(),
        request_id.clone(),
        cancellation,
    )
    .await;
    let response = match result {
        Ok(payload) => CallToolResult::structured(payload),
        Err(failure) => {
            let error = write::safe_error(failure.error);
            let mut payload = crate::error::structured_payload(&error);
            payload["request_id"] = json!(request_id);
            payload["state"] = json!(failure.state);
            payload["audit_warning"] = json!(failure.audit_warning);
            payload["error"]["retryable"] = json!(false);
            CallToolResult::structured_error(payload)
        }
    };
    Ok(response.into())
}

async fn dispatch(
    server: &Arc<ServerContext>,
    owner: Option<Owner>,
    tool: &'static str,
    mut arguments: Map<String, Value>,
    request_id: String,
    cancellation: CancellationToken,
) -> Result<Value, Failure> {
    server.config().tools().require(tool)?;
    if serde_json::to_vec(&arguments).map_or(true, |value| value.len() > MAX_ARGUMENT_BYTES) {
        return Err(invalid("事务工具参数超出大小限制").into());
    }
    let owner = owner.ok_or_else(|| denied("缺少已认证的 MCP 会话"))?;
    let (reply, response) = oneshot::channel();
    if tool == "begin_transaction" {
        let args: Begin = serde_json::from_value(Value::Object(arguments))
            .map_err(|_| invalid("事务起始参数非法"))?;
        let (id, commands) = server.transactions.reserve(&owner)?;
        let server = Arc::clone(server);
        tokio::spawn(async move {
            session::run(
                &server,
                &owner,
                id,
                args,
                request_id,
                cancellation,
                commands,
                reply,
            )
            .await;
        });
    } else {
        let id = arguments
            .remove("transaction_id")
            .and_then(|value| value.as_str().and_then(|value| Uuid::parse_str(value).ok()))
            .ok_or_else(|| invalid("transaction_id 非法"))?;
        if tool != "execute_in_transaction" && !arguments.is_empty() {
            return Err(invalid("事务终结参数非法").into());
        }
        server.transactions.submit(
            &owner,
            id,
            Command {
                tool,
                request_id,
                arguments,
                cancellation,
                reply,
            },
        )?;
    }
    response
        .await
        .map_err(|_| Error::internal("事务处理任务异常"))?
}

fn invalid(message: &'static str) -> Error {
    Error::invalid_argument(ErrorCode::MCP_INVALID_ARGUMENT, message)
}

fn denied(message: &'static str) -> Error {
    Error::new(
        ErrorCode::MCP_CONNECTION_DENIED,
        ErrorCategory::PermissionDenied,
        message,
    )
}
