//! Database-native plans for a validated read-only query.

use std::future::Future;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use sqarvo_common::{Error, ErrorCode, Result};

use crate::access::AccessService;
use crate::policy;
use crate::tools::query::{ExecuteOutcome, SqlRunRequest, run_sql};
use crate::tools::{ensure_enabled, require_non_empty};
use sqarvo_driver_api::driver::ExplainFormat;

/// 工具名。
pub const EXPLAIN_QUERY_TOOL_NAME: &str = "explain_query";

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
    let database = params.database.trim();
    let limits = *access.limits();
    let queries = Arc::clone(access.queries());
    access
        .run_plan(alias, cancel, |handle, timeout| {
            let queries = Arc::clone(&queries);
            async move {
                let (checked, database) = match handle.explain_format() {
                    Some(ExplainFormat::Standard) => (policy::check_explain(sql)?, database),
                    Some(ExplainFormat::SqliteQueryPlan) => (
                        policy::check_sqlite_plan(sql)?,
                        if database.eq_ignore_ascii_case("main") {
                            ""
                        } else {
                            database
                        },
                    ),
                    None => {
                        return Err(Error::unsupported(
                            ErrorCode::MCP_UNSUPPORTED,
                            "当前驱动未提供只读查询计划",
                        ));
                    }
                };
                let data = run_sql(
                    queries,
                    handle,
                    SqlRunRequest {
                        explain: true,
                        sql: checked.sql,
                        database: database.to_owned(),
                        max_rows: limits.max_rows,
                        timeout,
                        max_bytes: limits.max_result_bytes,
                    },
                    cancel(),
                )
                .await?;
                Ok(ExecuteOutcome {
                    output: data.into_output(),
                    fingerprint: checked.fingerprint.sha256_hex,
                })
            }
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support;

    fn db() -> std::path::PathBuf {
        std::path::Path::new("/nonexistent/sqarvo-mcp-explain-test.db").to_path_buf()
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
