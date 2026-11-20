//! Shared stdio and HTTP schema-tool registration.

use std::sync::Arc;
use std::time::Instant;

use rmcp::RoleServer;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, Tool, ToolAnnotations};
use rmcp::service::RequestContext;
use serde_json::{Map, Value, json};
use sqarvo_common::{Error, ErrorCategory, ErrorCode};

use crate::access::OperationType;
use crate::audit::AuditRecord;
use crate::config::DDL_TOOL_NAMES;
use crate::ddl::{self, Call};
use crate::policy::ddl::decode;
use crate::server::ServerContext;
use crate::transactions::Owner;

pub(crate) fn descriptions() -> impl Iterator<Item = Tool> {
    DDL_TOOL_NAMES.into_iter().map(|tool| {
        let preview = tool == "preview_schema_change";
        Tool::new(
            tool,
            match tool {
                "create_table" => "Create one table from typed columns and constraints.",
                "alter_table" => "Apply one supported column change or rename one table.",
                "drop_table" => "Permanently drop one ordinary table after dependency checks.",
                "create_index" => "Create a simple ordered-column index.",
                "drop_index" => "Drop a verified independent non-unique index.",
                _ => "Inspect a proposed schema change without modifying the database.",
            },
            Arc::new(
                schema(tool, preview)
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            ),
        )
        .with_annotations(
            ToolAnnotations::new()
                .read_only(preview)
                .destructive(matches!(tool, "alter_table" | "drop_table" | "drop_index"))
                .idempotent(preview)
                .open_world(false),
        )
    })
}

fn schema(tool: &str, preview: bool) -> Value {
    if tool == "preview_schema_change" {
        let branches: Vec<_> = DDL_TOOL_NAMES.into_iter().take(5).map(|operation| json!({
            "type": "object", "additionalProperties": false,
            "required": ["operation", "arguments"],
            "properties": {"operation": {"const": operation}, "arguments": schema(operation, true)},
        })).collect();
        return json!({"type":"object", "oneOf": branches});
    }
    let name = json!({"type":"string", "pattern":"^[a-z_][a-z0-9_]{0,63}$"});
    let key =
        json!({"type":"array", "minItems":1, "maxItems":16, "uniqueItems":true, "items":name});
    let boolean = json!({"type":"boolean", "default":false});
    let text = json!({"type":"string"});
    let column = json!({
        "type":"object", "additionalProperties":false, "required":["name","type"],
        "properties": {
            "name":name,
            "type":{"enum":["integer","bigint","varchar","text","decimal","boolean","date","datetime","timestamp","blob"]},
            "length":{"type":"integer","minimum":1,"maximum":4096},
            "precision":{"type":"integer","minimum":1,"maximum":38},
            "scale":{"type":"integer","minimum":0,"maximum":38},
            "nullable":{"type":"boolean"},
            "default":{"oneOf":[
                {"type":"null"}, {"type":"boolean"}, {"type":"integer"}, {"type":"string"},
                {"type":"object","required":["decimal"],"additionalProperties":false,"properties":{"decimal":text}},
                {"type":"object","required":["integer"],"additionalProperties":false,"properties":{"integer":text}}
            ]},
            "primary_key":boolean, "unique":boolean, "auto_increment":boolean, "identity":boolean
        }
    });
    let branch = |required: &[&str], mut properties: Map<String, Value>| {
        let mut fields = vec!["connection_alias", "database", "table"];
        fields.extend_from_slice(required);
        properties.extend(Map::from_iter([
            ("connection_alias".into(), text.clone()),
            ("database".into(), name.clone()),
            ("table".into(), name.clone()),
        ]));
        if !preview {
            fields.push("operation_id");
            properties.insert(
                "operation_id".into(),
                json!({"type":"string","format":"uuid"}),
            );
        }
        json!({"type":"object","required":fields,"additionalProperties":false,"properties":properties})
    };
    let make = |required: &[&str], value: Value| {
        branch(required, value.as_object().cloned().unwrap_or_default())
    };
    match tool {
        "create_table" => make(
            &["columns"],
            json!({
                "columns":{"type":"array","minItems":1,"maxItems":64,"items":column},
                "primary_key":key, "unique_constraints":{"type":"array","maxItems":16,"items":key},
                "if_not_exists":boolean, "foreign_keys":{"type":"array","maxItems":0,"items":false},
            }),
        ),
        "drop_table" => make(&[], json!({"if_exists":boolean})),
        "create_index" => make(
            &["index", "columns"],
            json!({"index":name,"columns":key,"unique":boolean}),
        ),
        "drop_index" => make(&["index"], json!({"index":name})),
        _ => json!({"type":"object", "oneOf":[
            make(&["action","column"], json!({"action":{"const":"add_column"},"column":column})),
            make(&["action","column_name"], json!({"action":{"const":"drop_column"},"column_name":name})),
            make(&["action","column_name","new_name"], json!({"action":{"const":"rename_column"},"column_name":name,"new_name":name})),
            make(&["action","new_table"], json!({"action":{"const":"rename_table"},"new_table":name})),
        ]}),
    }
}

pub(crate) async fn call(
    server: &Arc<ServerContext>,
    owner: Option<Owner>,
    tool: &'static str,
    request: CallToolRequestParams,
    context: RequestContext<RoleServer>,
) -> Result<CallToolResponse, rmcp::ErrorData> {
    let started = Instant::now();
    let id = context.id.to_string();
    let cancelled = context.ct.child_token();
    let _cancel_on_drop = cancelled.clone().drop_guard();
    let authorization = if tool == "preview_schema_change" {
        OperationType::Read
    } else {
        OperationType::Ddl
    };
    let validated = async {
        let owner = owner.ok_or_else(|| {
            Error::new(
                ErrorCode::MCP_CONNECTION_DENIED,
                ErrorCategory::PermissionDenied,
                "缺少已认证的 MCP 会话",
            )
        })?;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            server
                .access()
                .guard()
                .authorize_request(authorization, None),
        )
        .await
        .map_err(|_| {
            Error::new(
                ErrorCode::MCP_QUERY_TIMEOUT,
                ErrorCategory::Timeout,
                "读取 DDL 授权超时",
            )
        })??;
        server.config().tools().require(tool)?;
        let request = decode(tool, request.arguments.unwrap_or_default())?;
        Ok::<_, Error>((owner, request))
    }
    .await;
    let payload = match validated {
        Ok((owner, request)) => {
            let call = Call {
                tool,
                id,
                started,
                request,
                cancelled,
                owner: owner.lifetime,
            };
            let failure = ddl::worker_failure(&call);
            tokio::spawn(ddl::run(Arc::clone(server), call))
                .await
                .unwrap_or(failure)
        }
        Err(error) => {
            let record = AuditRecord::new(id, tool)
                .with_elapsed(started.elapsed())
                .failed(&error);
            let audit_warning = !matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    server.audit().write(&record)
                )
                .await,
                Ok(Ok(()))
            );
            let mut payload = ddl::error_payload(&error);
            payload["state"] = json!("not_started");
            payload["audit_warning"] = json!(audit_warning);
            payload
        }
    };
    Ok(if payload.get("error").is_some() {
        CallToolResult::structured_error(payload)
    } else {
        CallToolResult::structured(payload)
    }
    .into())
}
