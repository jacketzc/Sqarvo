//! `sqarvo_common::Error` → MCP 工具错误的映射（#207 实现；V3.1 §11）。
//!
//! 分类与 `mcp.*` / 既有子码复用现有体系（common 的 `mcp_error_codes_are_stable_and_unique`
//! 已锁定文本），本模块只做映射与 `isError` 语义，**不得另立错误码目录**；
//! 映射表可参照 rpc-server `error.rs` 的 category→code 处理，但不依赖它。
//! 错误消息面向 Agent：明确、可操作、不含凭据与敏感连接串。
//!
//! # 两条错误通道（阶段 A spike 实测口径）
//!
//! MCP 把「工具跑失败」和「请求不可路由」分开表达，客户端展示方式完全不同
//! （rmcp `ServerHandler::call_tool` 文档，`handler/server.rs:566-595`）：
//!
//! | 通道 | 产生方式 | 客户端看到 |
//! |---|---|---|
//! | 工具级 `isError: true` | [`ToolFailure`]（`IntoCallToolResult`） | 文本与结构化错误体 |
//! | 协议级 JSON-RPC error | [`protocol_error`] | 只有一句笼统的失败提示 |
//!
//! 因此业务错误（参数非法、未授权、只读违规、超时……）一律走 [`ToolFailure`]：
//! 把 `sqarvo_common::Error` 的子码与分类原样带给 Agent，才能让它自己纠正
//! （例如把 `mcp.readonly_violation` 读成「换一条 SELECT」）。协议级错误只留给
//! 「工具名不存在 / 请求形状不合法 / 服务整体不可用」这三类。

use rmcp::ErrorData;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock};
use serde_json::json;
use sqarvo_common::{Error, ErrorCategory, ErrorCode};

/// 面向 Agent 的错误文案：`<安全文案>（<稳定子码>）`。
///
/// 只使用 [`Error::message`]（调用方保证它是可安全展示的文案），
/// 不拼接 `source` 链，也不回显任何参数、SQL 原文或凭据。
#[must_use]
pub fn agent_message(error: &Error) -> String {
    format!("{}（{}）", error.message(), error.code())
}

/// 结构化错误体：机器可读的子码 + 分类 + 文案。
///
/// 放进 `structuredContent`，让 Agent 不必解析自然语言就能分支处理
/// （例如 `mcp.tool_disabled` 表示「换工具」而 `mcp.query_timeout` 表示「缩小范围重试」）。
#[must_use]
pub fn structured_payload(error: &Error) -> serde_json::Value {
    json!({
        "error": {
            "code": error.code().as_str(),
            "category": error.category().as_str(),
            "message": error.message(),
            "retryable": error.category().is_retryable(),
        }
    })
}

/// 工具级失败：`isError: true`，同时给出文本与结构化错误体。
///
/// 工具的返回类型写 `Result<Json<T>, ToolFailure>` 即可；`#[tool]` 宏会通过
/// [`IntoCallToolResult`] 把它变成合法的 `tools/call` 结果（协议层的 `result`，
/// 不是 JSON-RPC `error`）。
#[derive(Debug)]
pub struct ToolFailure {
    error: Error,
}

impl ToolFailure {
    /// 由统一错误构造。
    #[must_use]
    pub fn new(error: Error) -> Self {
        Self { error }
    }

    /// 原始错误（审计需要取子码/分类）。
    #[must_use]
    pub const fn error(&self) -> &Error {
        &self.error
    }

    /// 稳定子码。
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.error.code()
    }

    /// 分类。
    #[must_use]
    pub const fn category(&self) -> ErrorCategory {
        self.error.category()
    }
}

impl From<Error> for ToolFailure {
    fn from(error: Error) -> Self {
        Self::new(error)
    }
}

impl IntoCallToolResult for ToolFailure {
    fn into_call_tool_result(self) -> std::result::Result<CallToolResponse, ErrorData> {
        let text = agent_message(&self.error);
        let structured = structured_payload(&self.error);
        let mut result = CallToolResult::error(vec![ContentBlock::text(text)]);
        result.structured_content = Some(structured);
        Ok(result.into())
    }
}

/// 协议级错误（JSON-RPC `error`）：只用于「请求不可路由」。
///
/// 分类映射沿用 JSON-RPC/gRPC 的通用语义：
/// - `InvalidArgument` / `FailedPrecondition` → `-32602`（参数非法）；
/// - `NotFound` → 资源不存在（`-32002`）；
/// - 其余（含 `Internal` / `Unavailable`）→ `-32603`。
///
/// 注意：业务失败**不要**走这里——客户端通常看不到消息内容。
#[must_use]
pub fn protocol_error(error: &Error) -> ErrorData {
    match error.category() {
        ErrorCategory::InvalidArgument | ErrorCategory::FailedPrecondition => {
            ErrorData::invalid_params(agent_message(error), Some(structured_payload(error)))
        }
        ErrorCategory::PermissionDenied if error.code() == ErrorCode::MCP_CONNECTION_DENIED => {
            ErrorData::resource_not_found(agent_message(error), Some(structured_payload(error)))
        }
        ErrorCategory::NotFound => {
            ErrorData::resource_not_found(agent_message(error), Some(structured_payload(error)))
        }
        _ => ErrorData::internal_error(agent_message(error), Some(structured_payload(error))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: ErrorCode, category: ErrorCategory) -> ToolFailure {
        ToolFailure::new(Error::new(code, category, "示意失败"))
    }

    #[test]
    fn agent_message_uses_stable_subcode_and_no_source_chain() {
        let error = Error::new(
            ErrorCode::MCP_READONLY_VIOLATION,
            ErrorCategory::PermissionDenied,
            "只读策略拒绝该语句",
        )
        .with_source(std::io::Error::other("底层细节：password=hunter2"));
        let text = agent_message(&error);
        assert!(text.contains("mcp.readonly_violation"));
        assert!(text.contains("只读策略拒绝该语句"));
        // `source` 只进日志，不进 Agent 文案。
        assert!(!text.contains("hunter2"));
    }

    #[test]
    fn tool_failure_is_an_error_result_with_structured_payload() {
        let response = failure(
            ErrorCode::MCP_TOOL_DISABLED,
            ErrorCategory::PermissionDenied,
        )
        .into_call_tool_result()
        .expect("工具级失败不得变成协议错误");
        let result = match response {
            CallToolResponse::Complete(result) => result,
            other => panic!("工具级失败必须映射成 Complete 结果，实际是 {other:?}"),
        };
        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("结构化错误体必须存在");
        assert_eq!(payload["error"]["code"], "mcp.tool_disabled");
        assert_eq!(payload["error"]["category"], "permission_denied");
        assert_eq!(payload["error"]["retryable"], false);
    }

    #[test]
    fn protocol_error_maps_categories_to_jsonrpc_codes() {
        let invalid = protocol_error(&Error::invalid_argument(
            ErrorCode::MCP_INVALID_ARGUMENT,
            "参数非法",
        ));
        assert_eq!(invalid.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let missing = protocol_error(&Error::not_found(
            ErrorCode::MCP_CONNECTION_NOT_FOUND,
            "连接不存在",
        ));
        assert_eq!(missing.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);

        let internal = protocol_error(&Error::internal("内部错误"));
        assert_eq!(internal.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn retryable_categories_are_reported_to_agents() {
        let error = Error::new(
            ErrorCode::MCP_QUERY_TIMEOUT,
            ErrorCategory::Timeout,
            "查询超时",
        );
        let payload = structured_payload(&error);
        assert_eq!(payload["error"]["retryable"], true);
    }
}
