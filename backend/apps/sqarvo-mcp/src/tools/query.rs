//! 数据查询工具：`get_table_sample` / `execute_readonly_query`（V3.1 §5.7/§5.8）。
//!
//! # 强制限额（V3.1 §10）
//!
//! | 限制 | 默认 | 硬上限 | 落点 |
//! |---|---:|---:|---|
//! | 返回行数 | 200 | 1000 | `ExecuteRequest.max_rows` + 取批上限；`get_table_sample` 的 `limit` 同区间钳制 |
//! | 单次数据量 | 1 MiB | 1 MiB | [`RowBudget`] 在序列化前逐行计费 |
//! | 查询超时 | 10 秒 | 10 秒 | 外层 `tokio::time::timeout` + `SessionHandle::cancel`（驱动层 `ExecuteRequest.timeout` 未实现，见 [`execute_request`]） |
//!
//! 三层并发（进程信号量 / `QueryRegistry` / 会话执行锁）全部经
//! [`AccessService`] 与 [`QueryRegistry`]，不新建连接或查询管理器。
//!
//! # 两种截断
//!
//! 结果级 `truncated` 只表示「行数或字节上限截住了结果」；单元格级截断保留在
//! `Cell.truncated` 上（输出成 `{"truncated":true,...}` 对象），不会被压进同一个布尔。

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, QueryId, Result};
use sqarvo_connection::SessionHandle;
use sqarvo_driver_api::{
    BrowseOptions, ColumnMeta, ExecuteRequest, ExecutionStart, MAX_PAGE_SIZE, ObjectKind,
    ObjectRef, PageRequest, QualifiedName,
};
use sqarvo_query_engine::QueryRegistry;

use crate::access::{AccessService, OperationType};
use crate::policy;
use crate::tools::value::RowBudget;
use crate::tools::{effective_row_limit, ensure_enabled, require_non_empty};

/// `get_table_sample` 工具名。
pub const GET_TABLE_SAMPLE_TOOL_NAME: &str = "get_table_sample";
/// `execute_readonly_query` 工具名。
pub const EXECUTE_READONLY_QUERY_TOOL_NAME: &str = "execute_readonly_query";

/// 结果集列（名称 + 逻辑类型 kind）。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ColumnView {
    /// 列名。
    pub name: String,
    /// 逻辑类型 kind（`integer` / `text` / `decimal` / `timestamp` / `unsupported` …）。
    #[serde(rename = "type")]
    pub kind: String,
}

impl From<&ColumnMeta> for ColumnView {
    fn from(column: &ColumnMeta) -> Self {
        Self {
            name: column.name.clone(),
            kind: column.column_type.kind_name().to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// get_table_sample
// ---------------------------------------------------------------------------

/// `get_table_sample` 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetTableSampleParams {
    /// 连接别名。
    pub connection_alias: String,
    /// 数据库名。
    pub database: String,
    /// 表名（标识符引用由驱动完成，不拼接 SQL）。
    pub table: String,
    /// 行数上限；`0`（缺省）用服务端默认值（200），超过硬上限（1000）按上限钳制。
    #[serde(default)]
    pub limit: i64,
}

/// `get_table_sample` 输出。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TableSampleOutput {
    /// 连接别名。
    pub connection_alias: String,
    /// 数据库名。
    pub database: String,
    /// 表名。
    pub table: String,
    /// 列元数据（名称 + 类型 kind）。
    pub columns: Vec<ColumnView>,
    /// 行数据；单元格按 `tools::value` 的契约编码，截断单元格是对象。
    #[schemars(schema_with = "crate::tools::value::rows_schema")]
    pub rows: Vec<Vec<serde_json::Value>>,
    /// 实际返回行数。
    pub row_count: i64,
    /// 结果级截断（行数或字节上限）。
    pub truncated: bool,
    /// 截断原因：`none` / `rows` / `bytes` / `rows_and_bytes`。
    pub truncated_reason: String,
}

/// 读取一页表数据样本。顺序由驱动决定（有主键按主键升序，无主键为不稳定顺序）。
pub async fn get_table_sample(
    access: &AccessService,
    params: &GetTableSampleParams,
) -> Result<TableSampleOutput> {
    ensure_enabled(access, GET_TABLE_SAMPLE_TOOL_NAME)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let database = require_non_empty(&params.database, "database")?;
    let table = require_non_empty(&params.table, "table")?;
    let limit = effective_row_limit(params.limit, access.limits())?;
    let max_bytes = access.limits().max_result_bytes;
    let data = access
        .run(alias, |handle| {
            let database = database.to_owned();
            let table = table.to_owned();
            async move { sample(&handle, &database, &table, limit, max_bytes).await }
        })
        .await?;
    Ok(TableSampleOutput {
        connection_alias: alias.to_owned(),
        database: database.to_owned(),
        table: table.to_owned(),
        columns: data.columns,
        rows: data.rows,
        row_count: i64::try_from(data.rows_len).unwrap_or(i64::MAX),
        truncated: data.truncated,
        truncated_reason: data.truncated_reason.to_owned(),
    })
}

struct SampleData {
    columns: Vec<ColumnView>,
    rows: Vec<Vec<serde_json::Value>>,
    rows_len: usize,
    truncated: bool,
    truncated_reason: &'static str,
}

async fn sample(
    handle: &SessionHandle,
    database: &str,
    table: &str,
    limit: u32,
    max_bytes: u64,
) -> Result<SampleData> {
    let object = ObjectRef {
        session_id: handle.summary().session_id,
        qualified_name: QualifiedName::new(Some(database.to_owned()), None, table.to_owned()),
        kind: ObjectKind::Table,
    };
    let cap = row_cap(limit);
    let page_size = limit.min(MAX_PAGE_SIZE);
    let mut budget = RowBudget::new(max_bytes);
    let mut columns: Vec<ColumnView> = Vec::new();
    let mut token: Option<String> = None;
    let mut rows_cut = false;
    loop {
        let page = handle
            .browse(
                &object,
                &PageRequest {
                    page_size,
                    page_token: token.clone(),
                },
                &BrowseOptions::default(),
            )
            .await?;
        if columns.is_empty() {
            columns = page.columns.iter().map(ColumnView::from).collect();
        }
        let mut stopped_mid_page = false;
        for row in &page.rows {
            if budget.len() >= cap {
                stopped_mid_page = true;
                break;
            }
            if !budget.push(row) {
                break;
            }
        }
        if stopped_mid_page {
            rows_cut = true;
        }
        if stopped_mid_page || budget.bytes_exceeded() {
            break;
        }
        if budget.len() >= cap {
            // 恰好取满：驱动说还有更多才算截断。
            rows_cut = page.has_more;
            break;
        }
        if !page.has_more {
            break;
        }
        let Some(next_token) = page.next_page_token else {
            // has_more 与 token 不一致：保守标记截断，不静默当完整结果。
            rows_cut = true;
            break;
        };
        token = Some(next_token);
    }
    let (truncated, truncated_reason) = truncation(rows_cut, budget.bytes_exceeded());
    Ok(SampleData {
        columns,
        rows_len: budget.len(),
        rows: budget.into_rows(),
        truncated,
        truncated_reason,
    })
}

// ---------------------------------------------------------------------------
// execute_readonly_query
// ---------------------------------------------------------------------------

/// `execute_readonly_query` 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecuteReadonlyQueryParams {
    /// 连接别名。
    pub connection_alias: String,
    /// 可选数据库名；非空时在执行前切到该库（事务打开时驱动会拒绝）。
    /// 驱动不支持切库（SQLite）时返回 `capability.unsupported`，此时请省略该参数。
    #[serde(default)]
    pub database: String,
    /// 单条只读 SQL（SELECT / 只读 WITH）。DML、DDL、多语句、INTO、锁读与危险函数会被拒绝。
    pub sql: String,
}

/// 只读查询/EXPLAIN 的共用输出。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QueryResultOutput {
    /// 结果列（名称 + 类型 kind）。
    pub columns: Vec<ColumnView>,
    /// 行数据；单元格按 `tools::value` 的契约编码，截断单元格是对象。
    #[schemars(schema_with = "crate::tools::value::rows_schema")]
    pub rows: Vec<Vec<serde_json::Value>>,
    /// 实际返回行数。
    pub row_count: i64,
    /// 结果级截断（行数或字节上限）。
    pub truncated: bool,
    /// 截断原因：`none` / `rows` / `bytes` / `rows_and_bytes`。
    pub truncated_reason: String,
    /// 服务端执行（含取批）耗时，毫秒。
    pub execution_time_ms: i64,
}

/// 执行结果 + 审计用的 SQL 指纹。
#[derive(Debug, Clone)]
pub struct ExecuteOutcome {
    /// 工具输出。
    pub output: QueryResultOutput,
    /// 规范化 SQL 的 SHA-256（审计用；不含原始 SQL）。
    pub fingerprint: String,
}

/// 执行一条通过只读策略检查的查询（不带外部取消信号，供测试与内嵌调用）。
pub async fn execute_readonly_query(
    access: &AccessService,
    params: &ExecuteReadonlyQueryParams,
) -> Result<ExecuteOutcome> {
    execute_readonly_query_with_cancel(access, params, &|| std::future::pending::<()>()).await
}

/// 同 [`execute_readonly_query`]，但额外接收一个「取消信号工厂」。
///
/// MCP 处理器把 rmcp 的请求取消令牌包成这个工厂：客户端断开或发
/// `notifications/cancelled` 时，`run_sql` 会先 `SessionHandle::cancel` 打断驱动查询、
/// 释放 `QueryRegistry` 登记，再返回 `query.cancelled`（V3.1 §9.3）。工厂而不是
/// 单个 future，是因为 `AccessService::run` 在断连重连时可能调用闭包两次。
pub async fn execute_readonly_query_with_cancel<C, F>(
    access: &AccessService,
    params: &ExecuteReadonlyQueryParams,
    cancel: &C,
) -> Result<ExecuteOutcome>
where
    C: Fn() -> F + Sync,
    F: Future<Output = ()> + Send,
{
    ensure_enabled(access, EXECUTE_READONLY_QUERY_TOOL_NAME)?;
    let alias = require_non_empty(&params.connection_alias, "connection_alias")?;
    let sql = require_non_empty(&params.sql, "sql")?;
    // 授权先于解析：未授权的 alias 不该消耗解析与策略检查。
    access
        .guard()
        .authorize_request(OperationType::Read, Some(alias.to_owned()))
        .await?;
    let checked = policy::check_readonly(sql)?;
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
                        explain: false,
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

// ---------------------------------------------------------------------------
// 共用执行内核（EXPLAIN 也走这里）
// ---------------------------------------------------------------------------

/// 一次只读 SQL 执行的参数。
pub(crate) struct SqlRunRequest {
    /// Whether the statement is a driver-native plan.
    pub(crate) explain: bool,
    /// 已通过策略检查的 SQL。
    pub(crate) sql: String,
    /// 目标库；空串 = 不切换。
    pub(crate) database: String,
    /// 行数上限。
    pub(crate) max_rows: u32,
    /// 超时。
    pub(crate) timeout: Duration,
    /// 数据量上限。
    pub(crate) max_bytes: u64,
}

/// 执行结果（尚未映射成工具输出）。
pub(crate) struct QueryData {
    columns: Vec<ColumnView>,
    rows: Vec<Vec<serde_json::Value>>,
    row_count: usize,
    truncated: bool,
    truncated_reason: &'static str,
    execution_time_ms: i64,
}

impl QueryData {
    pub(crate) fn into_output(self) -> QueryResultOutput {
        QueryResultOutput {
            columns: self.columns,
            rows: self.rows,
            row_count: i64::try_from(self.row_count).unwrap_or(i64::MAX),
            truncated: self.truncated,
            truncated_reason: self.truncated_reason.to_owned(),
            execution_time_ms: self.execution_time_ms,
        }
    }
}

/// 构造驱动执行请求（单点定义）。
///
/// # 为什么 `timeout` 是 `None`
///
/// 任务书 §10 的设想是「超时走 `ExecuteRequest.timeout` + 现有取消」。实际检查驱动实现后，
/// MariaDB 在 `serve_query` 里对**非 `None`** 的 `timeout` 显式返回
/// `capability.unsupported`（`crates/drivers/mariadb/src/session.rs:826-833`，
/// 「单次执行超时尚未实现」），SQLite 也不保证等价语义；SPI 没有「支持单次超时」的能力位。
/// 因此 MCP 与 `rpc-server`（`services/query.rs:152-163`）采用同一条既有路径：
/// **外层 `tokio::time::timeout` 计时 + `SessionHandle::cancel` 真正打断查询**
/// （MariaDB 是 KILL QUERY，SQLite 是 interrupt）。超时仍然由服务端强制，不依赖 Agent。
pub(crate) fn execute_request(query_id: QueryId, sql: String, max_rows: u32) -> ExecuteRequest {
    ExecuteRequest {
        query_id,
        sql,
        max_rows: Some(max_rows),
        timeout: None,
    }
}

/// 结果行数上限（`usize`），与 `max_rows` 同源。
fn row_cap(max_rows: u32) -> usize {
    usize::try_from(max_rows).unwrap_or(usize::MAX)
}

/// 执行 SQL 并收集有界结果。
///
/// `cancelled` 由调用方给出「客户端断开」的 future（rmcp 的请求取消令牌）；
/// 触发时先取消驱动查询、释放登记，再返回 `query.cancelled`。
pub(crate) async fn run_sql<C>(
    queries: Arc<QueryRegistry>,
    handle: SessionHandle,
    request: SqlRunRequest,
    cancelled: C,
) -> Result<QueryData>
where
    C: Future<Output = ()> + Send,
{
    let SqlRunRequest {
        explain,
        sql,
        database,
        max_rows,
        timeout,
        max_bytes,
    } = request;
    if !database.is_empty() && !handle.supports_catalog_switch() {
        return Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "当前驱动不支持切换库：请省略 database 参数（SQLite 默认 main），或在 SQL 里使用限定名",
        ));
    }
    let session_id = handle.summary().session_id;
    let query_id = queries.register(session_id)?;
    let started = Instant::now();
    let max_rows = max_rows.max(1);
    let execution = async {
        let request = execute_request(query_id, sql, max_rows);
        let catalog = (!database.is_empty()).then_some(database.as_str());
        if explain {
            handle.execute_explain_in_catalog(request, catalog).await
        } else {
            handle.execute_in_catalog(request, catalog).await
        }
    };
    let deadline = tokio::time::Instant::now() + timeout;
    tokio::pin!(execution);
    tokio::pin!(cancelled);
    let start = tokio::select! {
        outcome = tokio::time::timeout_at(deadline, execution) => {
            if let Ok(result) = outcome {
                result
            } else {
                let _ = handle.cancel(&query_id).await;
                if explain {
                    let _ = handle.close_query(&query_id).await;
                }
                let _ = queries.complete(&query_id);
                return Err(Error::new(
                    ErrorCode::MCP_QUERY_TIMEOUT,
                    ErrorCategory::Timeout,
                    format!("查询超过 {} 毫秒上限，已请求取消", timeout.as_millis()),
                ));
            }
        },
        () = &mut cancelled => {
            let _ = handle.cancel(&query_id).await;
                if explain {
                    let _ = handle.close_query(&query_id).await;
                }
            let _ = queries.complete(&query_id);
            return Err(Error::new(
                ErrorCode::QUERY_CANCELLED,
                ErrorCategory::Cancelled,
                "客户端已取消该调用，查询已请求取消",
            ));
        }
    };
    let start = match start {
        Ok(start) => start,
        Err(error) => {
            let _ = queries.complete(&query_id);
            return Err(error);
        }
    };
    let collected = if explain {
        tokio::select! {
            biased;
            () = &mut cancelled => {
                let _ = handle.cancel(&query_id).await;
                Err(Error::new(ErrorCode::QUERY_CANCELLED, ErrorCategory::Cancelled, "客户端已取消查询计划"))
            }
            result = tokio::time::timeout_at(deadline, collect(&handle, &query_id, &start, max_rows, max_bytes, true)) => {
                if let Ok(result) = result {
                    result
                } else {
                    let _ = handle.cancel(&query_id).await;
                    Err(Error::new(ErrorCode::MCP_QUERY_TIMEOUT, ErrorCategory::Timeout, "查询计划超时"))
                }
            }
        }
    } else {
        collect(&handle, &query_id, &start, max_rows, max_bytes, false).await
    };
    // 无论收集成功与否都释放游标与并发名额。
    let _ = handle.close_query(&query_id).await;
    let _ = queries.complete(&query_id);
    let collected = collected?;
    let elapsed_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    Ok(QueryData {
        columns: collected.columns,
        rows: collected.rows,
        row_count: collected.row_count,
        truncated: collected.truncated,
        truncated_reason: collected.truncated_reason,
        execution_time_ms: elapsed_ms,
    })
}

struct Collected {
    columns: Vec<ColumnView>,
    rows: Vec<Vec<serde_json::Value>>,
    row_count: usize,
    truncated: bool,
    truncated_reason: &'static str,
}

async fn collect(
    handle: &SessionHandle,
    query_id: &QueryId,
    start: &ExecutionStart,
    max_rows: u32,
    max_bytes: u64,
    budget_envelope: bool,
) -> Result<Collected> {
    let statement = start.statements.first().ok_or_else(|| {
        Error::new(
            ErrorCode::MCP_INTERNAL,
            ErrorCategory::Internal,
            "驱动没有返回语句摘要",
        )
    })?;
    let columns: Vec<ColumnView> = statement.columns.iter().map(ColumnView::from).collect();
    let max_bytes = if budget_envelope {
        available_plan_row_bytes(&columns, max_bytes)?
    } else {
        max_bytes
    };
    let cap = row_cap(max_rows);
    let mut budget = RowBudget::new(max_bytes);
    let mut rows_cut = false;
    let mut next_batch = start.first_batch.clone();
    while let Some(batch) = next_batch.take() {
        let mut stopped_mid_batch = false;
        for row in &batch.rows {
            if budget.len() >= cap {
                stopped_mid_batch = true;
                break;
            }
            if !budget.push(row) {
                break;
            }
        }
        if stopped_mid_batch {
            rows_cut = true;
        }
        if stopped_mid_batch || budget.bytes_exceeded() {
            break;
        }
        if budget.len() >= cap {
            // 恰好取满：本批已经消费完，只有驱动说还有更多才算截断。
            rows_cut = batch.has_more;
            break;
        }
        if !batch.has_more {
            break;
        }
        let remaining = max_rows.saturating_sub(u32::try_from(budget.len()).unwrap_or(max_rows));
        if remaining == 0 {
            break;
        }
        let fetched = handle
            .fetch(
                query_id,
                batch.statement_index,
                batch.next_cursor.as_deref(),
                remaining,
            )
            .await?;
        if fetched.rows.is_empty() && fetched.has_more {
            // 无进展保护：驱动声称还有却没有给出任何行，如实标记截断而不是死循环。
            rows_cut = true;
            break;
        }
        next_batch = Some(fetched);
    }
    let (truncated, truncated_reason) = truncation(rows_cut, budget.bytes_exceeded());
    Ok(Collected {
        columns,
        row_count: budget.len(),
        rows: budget.into_rows(),
        truncated,
        truncated_reason,
    })
}

fn available_plan_row_bytes(columns: &[ColumnView], max_bytes: u64) -> Result<u64> {
    // Reserve the JSON envelope using the widest possible scalar fields.
    let envelope = QueryResultOutput {
        columns: columns.to_vec(),
        rows: Vec::new(),
        row_count: i64::MAX,
        truncated: false,
        truncated_reason: "rows_and_bytes".to_owned(),
        execution_time_ms: i64::MAX,
    };
    let bytes = serde_json::to_vec(&envelope).map_err(|_| Error::internal("无法编码查询计划"))?;
    max_bytes.checked_sub(bytes.len() as u64).ok_or_else(|| {
        Error::resource_exhausted(
            ErrorCode::MCP_RESULT_TOO_LARGE,
            "查询计划元数据超过输出限额",
        )
    })
}

fn truncation(rows_cut: bool, bytes_cut: bool) -> (bool, &'static str) {
    match (rows_cut, bytes_cut) {
        (true, true) => (true, "rows_and_bytes"),
        (true, false) => (true, "rows"),
        (false, true) => (true, "bytes"),
        (false, false) => (false, "none"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support;

    fn db() -> std::path::PathBuf {
        std::path::Path::new("/nonexistent/sqarvo-mcp-query-test.db").to_path_buf()
    }

    #[test]
    fn execute_request_leaves_the_deadline_to_the_upper_layer() {
        // 内置驱动对非 None 的 ExecuteRequest.timeout 显式报 capability.unsupported
        // （MariaDB session.rs:826-833）；截止时间由 run_sql 的外层 timeout + cancel 强制。
        let request = execute_request(QueryId::new(), "SELECT 1".to_owned(), 250);
        assert_eq!(request.max_rows, Some(250));
        assert_eq!(request.timeout, None, "驱动层不接受单次超时，交回上层计时");
        assert_eq!(request.sql, "SELECT 1");
    }

    #[test]
    fn configured_timeout_is_the_enforced_deadline() {
        let json = serde_json::json!({
            "connections": [],
            "query_limits": { "timeout_ms": 250 }
        });
        let validated = crate::config::parse(&json.to_string())
            .expect("解析")
            .validate()
            .expect("校验");
        assert_eq!(
            validated.limits().query_timeout,
            Duration::from_millis(250),
            "配置的超时必须原样成为服务端强制截止时间"
        );
    }

    #[test]
    fn row_limits_default_and_clamp() {
        let limits = crate::config::EffectiveLimits {
            max_rows: 200,
            hard_max_rows: 1000,
            query_timeout: Duration::from_secs(10),
            max_result_bytes: 1024 * 1024,
            max_concurrency: 2,
        };
        assert_eq!(effective_row_limit(0, &limits).unwrap(), 200);
        assert_eq!(effective_row_limit(50, &limits).unwrap(), 50);
        assert_eq!(effective_row_limit(5000, &limits).unwrap(), 1000);
        assert_eq!(
            effective_row_limit(-1, &limits)
                .expect_err("负数必须拒绝")
                .code(),
            ErrorCode::MCP_INVALID_ARGUMENT
        );
    }

    #[test]
    fn truncation_reasons_are_explicit() {
        assert_eq!(truncation(false, false), (false, "none"));
        assert_eq!(truncation(true, false), (true, "rows"));
        assert_eq!(truncation(false, true), (true, "bytes"));
        assert_eq!(truncation(true, true), (true, "rows_and_bytes"));
    }

    /// #220 回归：outputSchema 里 `rows` 的单元格不得是布尔 schema。
    ///
    /// `Vec<Vec<serde_json::Value>>` 的 derive 形态（schemars 1.x 把 `Value` 映射为
    /// `true`）会被 MCP 严格校验拒绝（Inspector `--strict` 报 invalid outputSchema）；
    /// `rows_schema` 钉成 `{"type":"array","items":{"type":"array","items":{}}}`。
    /// 运行时序列化不经过本测试——schema 投影与 JSON 形状解耦。
    #[test]
    fn rows_output_schemas_use_object_cell_schemas_not_booleans() {
        fn schema_of<T: JsonSchema>() -> serde_json::Value {
            let mut generator = schemars::SchemaGenerator::default();
            serde_json::Value::from(<T as JsonSchema>::json_schema(&mut generator))
        }
        fn assert_no_boolean_schema(node: &serde_json::Value, path: &str) {
            match node {
                serde_json::Value::Object(map) => {
                    for (key, child) in map {
                        assert!(
                            !child.is_boolean() || key == "truncated" || key == "default",
                            "{path}.{key} 是布尔子 schema：{child}"
                        );
                        assert_no_boolean_schema(child, &format!("{path}.{key}"));
                    }
                }
                serde_json::Value::Array(items) => {
                    for (index, child) in items.iter().enumerate() {
                        assert_no_boolean_schema(child, &format!("{path}[{index}]"));
                    }
                }
                _ => {}
            }
        }
        for name in ["TableSampleOutput", "QueryResultOutput"] {
            let schema = match name {
                "TableSampleOutput" => schema_of::<TableSampleOutput>(),
                _ => schema_of::<QueryResultOutput>(),
            };
            let cell = schema
                .pointer("/properties/rows/items/items")
                .unwrap_or_else(|| panic!("{name} 缺 rows.items.items：{schema}"));
            assert!(
                cell.is_object(),
                "{name}: rows 单元格必须是对象 schema：{cell}"
            );
            assert_no_boolean_schema(&schema, name);
        }
    }

    #[tokio::test]
    async fn query_tools_report_tool_disabled() {
        let access = test_support::service(db().as_path(), Some(Vec::new()));
        let failures = [
            get_table_sample(
                &access,
                &GetTableSampleParams {
                    connection_alias: "dev".to_owned(),
                    database: "main".to_owned(),
                    table: "t".to_owned(),
                    limit: 0,
                },
            )
            .await
            .expect_err("get_table_sample 必须被禁用"),
            execute_readonly_query(
                &access,
                &ExecuteReadonlyQueryParams {
                    connection_alias: "dev".to_owned(),
                    database: String::new(),
                    sql: "SELECT 1".to_owned(),
                },
            )
            .await
            .expect_err("execute_readonly_query 必须被禁用"),
        ];
        for error in failures {
            assert_eq!(error.code(), ErrorCode::MCP_TOOL_DISABLED, "{error}");
        }
    }

    #[tokio::test]
    async fn unknown_alias_is_rejected_before_connecting() {
        let access = test_support::service(db().as_path(), None);
        let failures = [
            execute_readonly_query(
                &access,
                &ExecuteReadonlyQueryParams {
                    connection_alias: "nope".to_owned(),
                    database: String::new(),
                    sql: "SELECT 1".to_owned(),
                },
            )
            .await
            .expect_err("未授权 alias 必须拒绝"),
            get_table_sample(
                &access,
                &GetTableSampleParams {
                    connection_alias: "nope".to_owned(),
                    database: "main".to_owned(),
                    table: "t".to_owned(),
                    limit: 0,
                },
            )
            .await
            .expect_err("未授权 alias 必须拒绝"),
        ];
        for error in failures {
            assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_NOT_FOUND, "{error}");
        }
        assert_eq!(access.session_manager().active_count().await, 0);
    }

    #[tokio::test]
    async fn invalid_parameters_are_rejected() {
        let access = test_support::service(db().as_path(), None);
        let cases = [
            get_table_sample(
                &access,
                &GetTableSampleParams {
                    connection_alias: "dev".to_owned(),
                    database: String::new(),
                    table: "t".to_owned(),
                    limit: 0,
                },
            )
            .await
            .expect_err("空 database"),
            get_table_sample(
                &access,
                &GetTableSampleParams {
                    connection_alias: "dev".to_owned(),
                    database: "main".to_owned(),
                    table: String::new(),
                    limit: 0,
                },
            )
            .await
            .expect_err("空 table"),
            get_table_sample(
                &access,
                &GetTableSampleParams {
                    connection_alias: "dev".to_owned(),
                    database: "main".to_owned(),
                    table: "t".to_owned(),
                    limit: -5,
                },
            )
            .await
            .expect_err("负 limit"),
            execute_readonly_query(
                &access,
                &ExecuteReadonlyQueryParams {
                    connection_alias: "dev".to_owned(),
                    database: String::new(),
                    sql: "   ".to_owned(),
                },
            )
            .await
            .expect_err("空 sql"),
        ];
        for error in cases {
            assert_eq!(error.code(), ErrorCode::MCP_INVALID_ARGUMENT, "{error}");
        }
    }

    #[tokio::test]
    async fn write_and_multi_statement_sql_is_rejected_before_execution() {
        let access = test_support::service(db().as_path(), None);
        for sql in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET x = 1",
            "SELECT 1; SELECT 2",
        ] {
            let error = execute_readonly_query(
                &access,
                &ExecuteReadonlyQueryParams {
                    connection_alias: "dev".to_owned(),
                    database: String::new(),
                    sql: sql.to_owned(),
                },
            )
            .await
            .expect_err("写操作与多语句必须拒绝");
            assert_eq!(error.code(), ErrorCode::MCP_READONLY_VIOLATION, "{sql}");
        }
        assert_eq!(access.session_manager().active_count().await, 0);
    }
}
