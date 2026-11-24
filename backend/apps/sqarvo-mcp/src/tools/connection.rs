//! `list_connections`：连接发现（V3.1 §5.1）。
//!
//! 只读 共享档案的授权结果与懒连接状态，**不发起任何数据库连接**（#207 的
//! [`AccessService::statuses`] 保证这一点）。输出刻意不含用户名、连接串与
//! `credential_ref`：这些信息对 Agent 没有用途，泄漏只会扩大攻击面。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqarvo_common::Result;

use crate::access::AccessService;
use crate::tools::ensure_enabled;

/// 工具名（与 `config::V1_TOOL_NAMES` 对齐）。
pub const TOOL_NAME: &str = "list_connections";

/// 参数：无。保留空结构体让 `inputSchema` 明确是「无参数」而不是缺省。
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListConnectionsParams {}

/// 单个连接的可见信息。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConnectionView {
    /// 与旧客户端兼容的连接别名。
    pub alias: String,
    /// Stable shared profile UUID, accepted by `connection_alias` parameters.
    pub connection_id: String,
    /// Connection name as shown in the desktop client.
    pub display_name: String,
    /// 驱动标识（`sqlite` / `mariadb`）。
    pub driver_id: String,
    /// 档案里的数据库名；SQLite 没有该项，输出空字符串。
    pub database: String,
    /// `not_connected` / `connected` / `failed`。
    pub connection_status: String,
    /// 上次连接失败的稳定子码（如 `credential.not_found`）；正常时为空字符串。
    pub last_error_code: String,
}

/// 工具输出。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListConnectionsOutput {
    /// 已授权连接列表（按 alias 升序）。
    pub connections: Vec<ConnectionView>,
    /// 连接数量。
    pub count: i64,
}

/// 查询全局授权后的连接状态，读取过程不连接数据库。
pub async fn list_connections(access: &AccessService) -> Result<ListConnectionsOutput> {
    ensure_enabled(access, TOOL_NAME)?;
    let connections: Vec<ConnectionView> = access
        .statuses()
        .await?
        .into_iter()
        .map(|status| ConnectionView {
            alias: status.alias,
            connection_id: status.connection_id.to_string(),
            display_name: status.display_name,
            driver_id: status.driver_id,
            database: status.database.unwrap_or_default(),
            connection_status: status.connection_status.to_owned(),
            last_error_code: status.last_error_code.unwrap_or_default().to_owned(),
        })
        .collect();
    let count = i64::try_from(connections.len()).unwrap_or(i64::MAX);
    Ok(ListConnectionsOutput { connections, count })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support;
    use sqarvo_common::ErrorCode;
    use std::path::Path;

    #[tokio::test]
    async fn disabled_tool_is_rejected_without_reading_any_connection() {
        let access = test_support::service(
            Path::new("/nonexistent/sqarvo-mcp-connections-test.db"),
            Some(Vec::new()),
        );
        let error = list_connections(&access)
            .await
            .expect_err("工具未启用必须拒绝");
        assert_eq!(error.code(), ErrorCode::MCP_TOOL_DISABLED);
    }

    #[tokio::test]
    async fn globally_disabled_access_never_reveals_profiles() {
        let access = test_support::service(Path::new("/nonexistent/sqarvo-mcp-off.db"), None);
        access
            .store
            .set_mcp_access(sqarvo_local_store::McpAccessPolicy::default())
            .expect("关闭全局访问");
        let error = list_connections(&access).await.expect_err("全局关闭");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_DENIED);
    }
}
