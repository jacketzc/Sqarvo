//! `explain_query`（V3.1 §6）。
//!
//! # V1 的驱动范围
//!
//! Driver SPI 没有 Explain 方法，V1 也不扩展 SPI：本工具在 MCP 层为**MariaDB**
//! 构造普通 `EXPLAIN`，SQLite 明确返回 `mcp.unsupported`。
//!
//! 驱动白名单只在本文件维护（[`EXPLAIN_DRIVERS`]）：
//!
//! - **V1 已知例外**：ADR 0032 记录了「MCP 层按数据库类型生成 EXPLAIN」这一临时
//!   设计，后续以驱动能力位取代；
//! - 不在其他模块散落 `driver_id == "mariadb"` 判断；
//! - 不把普通 `EXPLAIN` 与 SQLite 的 `EXPLAIN QUERY PLAN` 混为一谈——语义不同，
//!   本版本不代执行。
//!
//! # 安全顺序
//!
//! 用户 SQL → [`crate::policy::check_explain`]（与 `check_readonly` 完全相同的策略）
//! → 由策略层拼 `EXPLAIN ` 前缀 → 现有 `SessionHandle::execute`。用户自己提交的
//! `EXPLAIN` / `ANALYZE` 形态在策略层就被拒绝，本工具没有第二条绕过路径，也绝不
//! 构造 `EXPLAIN ANALYZE`（MariaDB 的 `ANALYZE SELECT` 会真实执行查询）。

use std::future::Future;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use sqarvo_common::{Error, ErrorCode, Result};

use crate::access::{AccessService, OperationType};
use crate::policy;
use crate::tools::query::{ExecuteOutcome, SqlRunRequest, run_sql};
use crate::tools::{ensure_enabled, require_non_empty};

/// 工具名。
pub const EXPLAIN_QUERY_TOOL_NAME: &str = "explain_query";

/// V1 支持 Explain 的驱动白名单。
///
/// **V1 已知例外**（ADR 0032）：SPI 尚无 Explain 能力位时在 MCP 层单点映射；
/// 新增支持必须同时补测试与 ADR，不得只加一个字符串。
const EXPLAIN_DRIVERS: [&str; 1] = ["mariadb"];

/// 该驱动是否支持 V1 的 `explain_query`。
#[must_use]
pub fn supports_explain(driver_id: &str) -> bool {
    EXPLAIN_DRIVERS.contains(&driver_id)
}

/// `explain_query` 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExplainQueryParams {
    /// 连接别名。
    pub connection_alias: String,
    /// 可选数据库名（执行前切库；驱动不支持切库时返回 `capability.unsupported`）。
    #[serde(default)]
    pub database: String,
    /// 要被 EXPLAIN 的单条只读 SQL。**不要**自带 `EXPLAIN` / `ANALYZE` 前缀：
    /// 前缀由服务端生成，自带前缀会被只读策略拒绝。
    pub sql: String,
}

/// 对一条只读 SQL 执行 `EXPLAIN`，返回计划行（结构与普通查询一致）。
pub async fn explain_query(
    access: &AccessService,
    params: &ExplainQueryParams,
) -> Result<ExecuteOutcome> {
    explain_query_with_cancel(access, params, &|| std::future::pending::<()>()).await
}

/// 同 [`explain_query`]，额外接一个「取消信号工厂」（rmcp 请求取消令牌）。
///
/// 与 `execute_readonly_query_with_cancel` 同一条路径：客户端断开时取消驱动查询
/// 并释放登记，不让 EXPLAIN 在后台失控（V3.1 §9.3）。
pub async fn explain_query_with_cancel<C, F>(
    access: &AccessService,
    params: &ExplainQueryParams,
    cancel: &C,
) -> Result<ExecuteOutcome>
where
    C: Fn() -> F + Sync,
    F: Future<Output = ()> + Send,
{
    ensure_enabled(access, EXPLAIN_QUERY_TOOL_NAME)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let sql = require_non_empty(&params.sql, "sql")?;
    let profile = access
        .guard()
        .authorize_request(OperationType::Read, Some(alias.to_owned()))
        .await?
        .profile
        .ok_or_else(|| Error::internal("已授权请求缺少连接档案"))?;
    let driver_id = profile.params.driver_id.as_str();
    if !supports_explain(driver_id) {
        return Err(Error::unsupported(
            ErrorCode::MCP_UNSUPPORTED,
            format!(
                "explain_query 当前仅支持 MariaDB（普通 EXPLAIN）；驱动 {driver_id} 未声明 Explain 能力。\
                 SQLite 的等价物是 EXPLAIN QUERY PLAN，语义不同，本版本不代执行。"
            ),
        ));
    }
    let checked = policy::check_explain(sql)?;
    let fingerprint = checked.fingerprint.sha256_hex.clone();
    let sql = checked.sql;
    let database = params.database.trim().to_owned();
    let limits = *access.limits();
    let queries = Arc::clone(access.queries());
    let data = access
        .run(alias, |handle| {
            let sql = sql.clone();
            let database = database.clone();
            let queries = Arc::clone(&queries);
            async move {
                run_sql(
                    queries,
                    handle,
                    SqlRunRequest {
                        sql,
                        database,
                        max_rows: limits.max_rows,
                        timeout: limits.query_timeout,
                        max_bytes: limits.max_result_bytes,
                    },
                    cancel(),
                )
                .await
            }
        })
        .await?;
    Ok(ExecuteOutcome {
        output: data.into_output(),
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support;

    fn db() -> std::path::PathBuf {
        std::path::Path::new("/nonexistent/sqarvo-mcp-explain-test.db").to_path_buf()
    }

    #[test]
    fn driver_mapping_is_single_point_and_truthful() {
        assert!(supports_explain("mariadb"));
        assert!(!supports_explain("sqlite"));
        assert!(!supports_explain("postgresql"));
    }

    #[tokio::test]
    async fn sqlite_reports_unsupported_with_an_actionable_message() {
        let access = test_support::service(db().as_path(), None);
        let error = explain_query(
            &access,
            &ExplainQueryParams {
                connection_alias: "dev".to_owned(),
                database: String::new(),
                sql: "SELECT 1".to_owned(),
            },
        )
        .await
        .expect_err("SQLite 不支持 explain_query");
        assert_eq!(error.code(), ErrorCode::MCP_UNSUPPORTED);
        assert!(error.message().contains("EXPLAIN QUERY PLAN"));
        // 不碰数据库：驱动门控发生在会话建立之前。
        assert_eq!(access.session_manager().active_count().await, 0);
    }

    #[tokio::test]
    async fn disabled_and_unauthorized_paths_are_rejected() {
        let disabled = test_support::service(db().as_path(), Some(Vec::new()));
        let error = explain_query(
            &disabled,
            &ExplainQueryParams {
                connection_alias: "dev".to_owned(),
                database: String::new(),
                sql: "SELECT 1".to_owned(),
            },
        )
        .await
        .expect_err("工具未启用");
        assert_eq!(error.code(), ErrorCode::MCP_TOOL_DISABLED);

        let access = test_support::service(db().as_path(), None);
        let error = explain_query(
            &access,
            &ExplainQueryParams {
                connection_alias: "nope".to_owned(),
                database: String::new(),
                sql: "SELECT 1".to_owned(),
            },
        )
        .await
        .expect_err("未授权 alias");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_parameters_are_rejected() {
        let access = test_support::service(db().as_path(), None);
        for params in [
            ExplainQueryParams {
                connection_alias: "dev".to_owned(),
                database: String::new(),
                sql: "  ".to_owned(),
            },
            ExplainQueryParams {
                connection_alias: String::new(),
                database: String::new(),
                sql: "SELECT 1".to_owned(),
            },
        ] {
            let error = explain_query(&access, &params).await.expect_err("参数非法");
            assert_eq!(error.code(), ErrorCode::MCP_INVALID_ARGUMENT);
        }
    }
}
