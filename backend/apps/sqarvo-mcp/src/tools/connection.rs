//! `list_connections`：连接发现（V3.1 §5.1）。
//!
//! 只读 `mcp.json` 的授权结果与懒连接状态，**不发起任何数据库连接**（#207 的
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
    /// 稳定连接标识（工具参数里用它选连接）。
    pub alias: String,
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

/// 实现。失败只可能来自 `enabled_tools` 禁用（状态读取本身不触网、不报错）。
pub fn list_connections(access: &AccessService) -> Result<ListConnectionsOutput> {
    ensure_enabled(access, TOOL_NAME)?;
    let connections: Vec<ConnectionView> = access
        .statuses()
        .into_iter()
        .map(|status| ConnectionView {
            alias: status.alias,
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
    use std::sync::Arc;

    #[test]
    fn disabled_tool_is_rejected_without_reading_any_connection() {
        let access = test_support::service(
            Path::new("/nonexistent/sqarvo-mcp-connections-test.db"),
            Some(Vec::new()),
        );
        let error = list_connections(&access).expect_err("工具未启用必须拒绝");
        assert_eq!(error.code(), ErrorCode::MCP_TOOL_DISABLED);
    }

    #[test]
    fn unauthorized_aliases_never_appear_in_the_list() {
        let json = serde_json::json!({
            "connections": [{
                "alias": "off",
                "driver_id": "sqlite",
                "host": "/nonexistent/sqarvo-mcp-off.db",
                "port": 0,
                "enabled": false
            }]
        });
        let config = crate::config::parse(&json.to_string())
            .expect("解析")
            .validate()
            .expect("校验");
        let registry = Arc::new(sqarvo_drivers::DriverRegistry::with_builtin_drivers());
        let sessions = Arc::new(sqarvo_connection::SessionManager::with_credential_store(
            registry,
            Arc::new(sqarvo_local_store::MemoryCredentialStore::new()),
        ));
        let access = AccessService::with_session_manager(Arc::new(config), sessions);
        let output = list_connections(&access).expect("读取状态不触网");
        assert_eq!(output.count, 0, "enabled: false 的连接不得暴露给 Agent");
    }
}
