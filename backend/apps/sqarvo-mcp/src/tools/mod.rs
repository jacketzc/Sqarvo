//! MCP 工具集（#209 实现；V3.1 §5/§6）。
//!
//! 工具清单与描述口径见 Issue #209。约定：
//! - 所有工具先过 `enabled_tools` 检查，再经 [`crate::access::AccessService`]
//!   做 alias 授权、懒连接与并发许可，最后执行；`policy` 类工具在授权后、执行前
//!   做 SQL 只读检查；
//! - 参数类型规避 `u32` / `Option` 在 schemars 下的非标准 schema 形态
//!   （阶段 A 实测：`format:"uint32"`、`type:[T,"null"]` 有可移植性警告）。
//!   可选字符串参数用缺省空串表达，可选数值用 `0` 表达「用服务端默认值」；
//! - 行级截断（策略行数/字节上限）与单元格级截断（`Cell.truncated`，ADR 0007）
//!   是**两个独立标记**，不得压成一个布尔；JSON 编码契约见 [`value`]；
//! - 每次调用（成功与失败）都写一条审计记录：成功带耗时、行数、结果级截断；
//!   失败只带错误分类与稳定子码，永不记录原始 SQL 或结果集。
//!
//! # 工具可见性与 `enabled_tools`
//!
//! `tools/list` 始终列出 V1 的 8 个工具（注册表是静态的），`enabled_tools` 控制的是
//! **能否执行**：未启用的工具被调用时返回 `mcp.tool_disabled`（V3.1 §3.1）。
//! 这样 Agent 看到的名字与配置项一一对应，而不是遇到「名字消失」的第二种失败模式。
//!
//! # 文件分工
//!
//! - [`connection`]：`list_connections`
//! - [`schema`]：`list_databases` / `list_tables` / `describe_table` / `list_indexes`
//! - [`query`]：`get_table_sample` / `execute_readonly_query`
//! - [`explain`]：`explain_query`
//! - [`value`]：`Value` → JSON 的单点序列化契约与结果字节预算
//!
//! 每个工具的核心逻辑都是独立函数（`pub`，便于集成测试与 #211 复用），
//! `#[tool_router]` 里的方法只做参数解包、审计与错误通道映射。

pub mod connection;
pub mod explain;
pub mod query;
pub mod schema;
pub mod value;
pub(crate) mod write;
pub mod write_preflight;
pub mod write_transaction;

use std::time::Instant;

use rmcp::RoleServer;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{Json, tool, tool_router};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};

use crate::access::AccessService;
use crate::audit::AuditRecord;
use crate::config::EffectiveLimits;
use crate::error::ToolFailure;
use crate::server::{ServerContext, SqarvoMcpServer};

/// `enabled_tools` 未启用该工具时返回 `mcp.tool_disabled`。
pub(crate) fn ensure_enabled(access: &AccessService, tool: &'static str) -> Result<()> {
    access.config().tools().require(tool)
}

/// 必填字符串参数：去空白后不得为空。
pub(crate) fn require_non_empty<'a>(value: &'a str, field: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::invalid_argument(
            ErrorCode::MCP_INVALID_ARGUMENT,
            format!("参数 {field} 不能为空"),
        ));
    }
    Ok(trimmed)
}

/// 可选字符串参数：空串 = 未提供。
pub(crate) fn optional_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// 行数参数归一化：`0` = 服务端默认值，其余钳制到 `[1, hard_max_rows]`。
///
/// 钳制是服务端强制限额的一部分（V3.1 §10「不信任 Agent 输入的 limit」），
/// 不做静默放宽；负数属于参数错误。
pub(crate) fn effective_row_limit(requested: i64, limits: &EffectiveLimits) -> Result<u32> {
    if requested < 0 {
        return Err(Error::invalid_argument(
            ErrorCode::MCP_INVALID_ARGUMENT,
            "参数 limit 不能为负数；省略或填 0 表示使用服务端默认值",
        ));
    }
    if requested == 0 {
        return Ok(limits.max_rows);
    }
    let requested = u32::try_from(requested).unwrap_or(limits.hard_max_rows);
    Ok(requested.clamp(1, limits.hard_max_rows))
}

/// 一次工具调用的审计上下文。
///
/// 工具方法在入口构造它、在出口消费它：成功路径的审计写失败会变成可见的
/// `mcp.internal`（「审计失败要可见」），失败路径的审计写失败只记日志，不覆盖
/// 原本要返回给 Agent 的错误。
pub(crate) struct ToolCall<'a> {
    context: &'a ServerContext,
    request_id: String,
    client_info: Option<String>,
    tool: &'static str,
    alias: Option<String>,
    database: Option<String>,
    fingerprint: Option<String>,
    started: Instant,
}

impl<'a> ToolCall<'a> {
    pub(crate) fn new(
        context: &'a ServerContext,
        request_id: impl Into<String>,
        client_info: Option<String>,
        tool: &'static str,
    ) -> Self {
        Self {
            context,
            request_id: request_id.into(),
            client_info,
            tool,
            alias: None,
            database: None,
            fingerprint: None,
            started: Instant::now(),
        }
    }

    pub(crate) fn from_request(
        context: &'a ServerContext,
        request: &RequestContext<RoleServer>,
        tool: &'static str,
    ) -> Self {
        // 客户端信息只在协议确实给出时记录（阶段 A：旧握手在 peer info 里，
        // 新流程在每请求 `_meta` 里；`client_info()` 已按协商结果选好来源）。
        let client_info = request
            .client_info()
            .map(|info| format!("{} {}", info.name, info.version));
        Self::new(context, request.id.to_string(), client_info, tool)
    }

    pub(crate) fn with_target(mut self, alias: &str, database: Option<&str>) -> Self {
        self.alias = Some(alias.to_owned());
        self.database = database.map(str::to_owned);
        self
    }

    pub(crate) fn with_fingerprint(mut self, fingerprint: &str) -> Self {
        self.fingerprint = Some(fingerprint.to_owned());
        self
    }

    fn record(&self) -> AuditRecord {
        AuditRecord::new(self.request_id.clone(), self.tool)
            .with_client(self.client_info.clone())
            .with_target(self.alias.as_deref(), self.database.as_deref())
            .with_fingerprint(self.fingerprint.as_deref())
            .with_elapsed(self.started.elapsed())
    }

    /// 成功路径：写审计；写失败直接变成工具错误（结果不交付）。
    pub(crate) async fn succeeded(
        self,
        rows: Option<i64>,
        truncated: Option<bool>,
    ) -> std::result::Result<(), ToolFailure> {
        let mut record = self.record();
        if let Some(rows) = rows {
            record = record.with_rows(u64::try_from(rows).unwrap_or(0), truncated.unwrap_or(false));
        }
        if let Err(error) = self.context.audit().write(&record).await {
            return Err(ToolFailure::new(Error::new(
                ErrorCode::MCP_INTERNAL,
                ErrorCategory::Internal,
                format!("审计写入失败，结果未交付：{}", error.message()),
            )));
        }
        Ok(())
    }

    /// 失败路径：写审计后把原错误还给 Agent。
    pub(crate) async fn failed(self, error: Error) -> ToolFailure {
        let record = self.record().failed(&error);
        if let Err(audit_error) = self.context.audit().write(&record).await {
            tracing::error!(
                code = %audit_error.code(),
                tool = self.tool,
                "失败调用的审计写入失败"
            );
        }
        ToolFailure::new(error)
    }
}

/// 把 rmcp 的请求取消令牌包成「取消信号工厂」。
///
/// 每次调用返回一份新的 `cancelled()` future：`AccessService::run` 在会话失效时可能
/// 把同一个闭包调用两次，future 不能复用。`CancellationToken` 的类型来自 tokio-util
/// （rmcp 的传递依赖），本包不直接依赖它，所以用装箱 future 把它藏在类型推断后面。
fn request_cancel_factory(
    request: &RequestContext<RoleServer>,
) -> impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Sync + 'static {
    let token = request.ct.clone();
    move || {
        let token = token.clone();
        Box::pin(async move { token.cancelled().await })
    }
}

/// 8 个 V1 工具的路由与入口。
///
/// 方法体只做「解包参数 → 调核心函数 → 审计 → 映射错误通道」；所有业务规则在
/// 各子模块里，便于单测与集成测试直接调用核心函数。
#[tool_router(router = tool_router, vis = "pub(crate)")]
impl SqarvoMcpServer {
    #[tool]
    /// 列出共享连接档案及懒连接状态（需要启用 MCP 全局数据访问）。
    /// 不会主动连接数据库；返回 `alias`、`driver_id`、`database`、
    /// `connection_status` 与上次失败子码，不含用户名、连接串、密码或
    /// `credential_ref`。
    async fn list_connections(
        &self,
        _params: Parameters<connection::ListConnectionsParams>,
        request: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<connection::ListConnectionsOutput>, ToolFailure> {
        let call = ToolCall::from_request(self.context(), &request, connection::TOOL_NAME);
        match connection::list_connections(self.context().access()).await {
            Ok(output) => {
                call.succeeded(None, None).await?;
                Ok(Json(output))
            }
            Err(error) => Err(call.failed(error).await),
        }
    }

    #[tool]
    /// 列出连接上可见的数据库（MariaDB 来自 `information_schema.SCHEMATA`；
    /// SQLite 为 `main` / `temp` / 附加库）。单页最多 500 项，`truncated=true`
    /// 表示还有更多。同一连接上的工具调用串行执行，排队不是故障。
    async fn list_databases(
        &self,
        params: Parameters<schema::ListDatabasesParams>,
        request: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<schema::ListDatabasesOutput>, ToolFailure> {
        let params = params.0;
        let call =
            ToolCall::from_request(self.context(), &request, schema::LIST_DATABASES_TOOL_NAME)
                .with_target(&params.connection_alias, None);
        match schema::list_databases(self.context().access(), &params).await {
            Ok(output) => {
                call.succeeded(Some(output.count), Some(output.truncated))
                    .await?;
                Ok(Json(output))
            }
            Err(error) => Err(call.failed(error).await),
        }
    }

    #[tool]
    /// 列出指定数据库里的表与视图（kind 字段如实转出驱动给的对象种类）。
    /// schema 参数：本版本的 MariaDB 与 SQLite 驱动都没有独立 schema 层，
    /// 传非空值会返回 capability.unsupported（不会静默忽略）。
    /// 单页最多 500 项；同一连接上的调用串行执行。
    async fn list_tables(
        &self,
        params: Parameters<schema::ListTablesParams>,
        request: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<schema::ListTablesOutput>, ToolFailure> {
        let params = params.0;
        let call = ToolCall::from_request(self.context(), &request, schema::LIST_TABLES_TOOL_NAME)
            .with_target(&params.connection_alias, optional_trimmed(&params.database));
        match schema::list_tables(self.context().access(), &params).await {
            Ok(output) => {
                call.succeeded(Some(output.count), Some(output.truncated))
                    .await?;
                Ok(Json(output))
            }
            Err(error) => Err(call.failed(error).await),
        }
    }

    #[tool]
    /// 读取表/视图的列定义与主键。返回列名、序号、逻辑类型、原始类型名、NULL 属性、
    /// 默认值状态与表达式原文、字符集/排序规则，以及按主键顺序排列的主键列。
    /// 本版本 Driver SPI 不提供自增属性与列注释，因此不输出这两项（不伪造）。
    /// schema 非空时返回 capability.unsupported。
    async fn describe_table(
        &self,
        params: Parameters<schema::DescribeTableParams>,
        request: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<schema::DescribeTableOutput>, ToolFailure> {
        let params = params.0;
        let call =
            ToolCall::from_request(self.context(), &request, schema::DESCRIBE_TABLE_TOOL_NAME)
                .with_target(&params.connection_alias, optional_trimmed(&params.database));
        match schema::describe_table(self.context().access(), &params).await {
            Ok(output) => {
                let column_count = i64::try_from(output.columns.len()).unwrap_or(i64::MAX);
                call.succeeded(Some(column_count), Some(false)).await?;
                Ok(Json(output))
            }
            Err(error) => Err(call.failed(error).await),
        }
    }

    #[tool]
    /// 只返回表上的索引名称（主键索引名为 `PRIMARY`）。本版本 Driver SPI 不提供索引列组成、
    /// 唯一性、索引类型与排序方式，因此这些字段不会被虚构。需要明细时用
    /// `execute_readonly_query` 查询 `information_schema.STATISTICS`，例如：
    ///
    /// ```sql
    /// SELECT INDEX_NAME, NON_UNIQUE, SEQ_IN_INDEX, COLUMN_NAME, INDEX_TYPE
    /// FROM information_schema.STATISTICS
    /// WHERE TABLE_SCHEMA = 'app_db' AND TABLE_NAME = 'users'
    /// ORDER BY INDEX_NAME, SEQ_IN_INDEX
    /// ```
    ///
    /// 该查询受数据库账号权限约束。`schema` 非空时返回 `capability.unsupported`。
    async fn list_indexes(
        &self,
        params: Parameters<schema::ListIndexesParams>,
        request: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<schema::ListIndexesOutput>, ToolFailure> {
        let params = params.0;
        let call = ToolCall::from_request(self.context(), &request, schema::LIST_INDEXES_TOOL_NAME)
            .with_target(&params.connection_alias, optional_trimmed(&params.database));
        match schema::list_indexes(self.context().access(), &params).await {
            Ok(output) => {
                call.succeeded(Some(output.count), Some(output.truncated))
                    .await?;
                Ok(Json(output))
            }
            Err(error) => Err(call.failed(error).await),
        }
    }

    #[tool]
    /// 读取一页表数据样本（有主键按主键升序，无主键为不稳定顺序）。标识符引用由驱动完成，
    /// 不拼接 SQL。limit 省略/0 用服务端默认（200），硬上限 1000；单次结果另有 1 MiB 上限。
    /// truncated 是结果级截断（行数或字节）；被截断的单元格输出为
    /// {"truncated":true,"type":...,"value":...} 对象，短值是裸 JSON 值，二者可区分。
    /// 同一连接上的调用串行执行。
    async fn get_table_sample(
        &self,
        params: Parameters<query::GetTableSampleParams>,
        request: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<query::TableSampleOutput>, ToolFailure> {
        let params = params.0;
        let call =
            ToolCall::from_request(self.context(), &request, query::GET_TABLE_SAMPLE_TOOL_NAME)
                .with_target(&params.connection_alias, optional_trimmed(&params.database));
        match query::get_table_sample(self.context().access(), &params).await {
            Ok(output) => {
                call.succeeded(Some(output.row_count), Some(output.truncated))
                    .await?;
                Ok(Json(output))
            }
            Err(error) => Err(call.failed(error).await),
        }
    }

    #[tool]
    /// 执行一条通过 SQL AST 只读策略检查的单条 SELECT / 只读 WITH。拒绝 DML、DDL、
    /// 多语句、任何 INTO、锁读与危险函数（`SLEEP` / `LOAD_FILE` / `BENCHMARK` /
    /// `GET_LOCK` 等）。
    /// 服务端强制：默认最多 200 行（配置可调，硬上限 1000）、单次 1 MiB、10 秒超时，
    /// 超时会请求取消数据库查询。`truncated` / `truncated_reason` 是结果级截断；
    /// 单元格级截断见 `{"truncated":true,...}` 对象。`database` 非空时先切库再执行
    /// （驱动不支持切库会返回 `capability.unsupported`）。同一连接串行执行。
    async fn execute_readonly_query(
        &self,
        params: Parameters<query::ExecuteReadonlyQueryParams>,
        request: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<query::QueryResultOutput>, ToolFailure> {
        let params = params.0;
        let call = ToolCall::from_request(
            self.context(),
            &request,
            query::EXECUTE_READONLY_QUERY_TOOL_NAME,
        )
        .with_target(&params.connection_alias, optional_trimmed(&params.database));
        let cancel = request_cancel_factory(&request);
        match query::execute_readonly_query_with_cancel(self.context().access(), &params, &cancel)
            .await
        {
            Ok(outcome) => {
                let call = call.with_fingerprint(&outcome.fingerprint);
                call.succeeded(
                    Some(outcome.output.row_count),
                    Some(outcome.output.truncated),
                )
                .await?;
                Ok(Json(outcome.output))
            }
            Err(error) => Err(call.failed(error).await),
        }
    }

    #[tool]
    /// 对一条只读 SELECT / 只读 WITH 生成并执行 EXPLAIN，不执行原始查询。
    /// V1 仅支持 MariaDB（普通 EXPLAIN，绝不使用 EXPLAIN ANALYZE）；SQLite 返回
    /// `mcp.unsupported`（其等价物 EXPLAIN QUERY PLAN 语义不同，本版本不代执行）。
    /// SQL 与 `execute_readonly_query` 走同一只读策略；用户自带 EXPLAIN/ANALYZE 前缀
    /// 会被拒绝。限额与 `execute_readonly_query` 相同。同一连接串行执行。
    async fn explain_query(
        &self,
        params: Parameters<explain::ExplainQueryParams>,
        request: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<query::QueryResultOutput>, ToolFailure> {
        let params = params.0;
        let call =
            ToolCall::from_request(self.context(), &request, explain::EXPLAIN_QUERY_TOOL_NAME)
                .with_target(&params.connection_alias, optional_trimmed(&params.database));
        let cancel = request_cancel_factory(&request);
        match explain::explain_query_with_cancel(self.context().access(), &params, &cancel).await {
            Ok(outcome) => {
                let call = call.with_fingerprint(&outcome.fingerprint);
                call.succeeded(
                    Some(outcome.output.row_count),
                    Some(outcome.output.truncated),
                )
                .await?;
                Ok(Json(outcome.output))
            }
            Err(error) => Err(call.failed(error).await),
        }
    }
}

/// 单元测试共用的装配助手。
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::sync::Arc;

    use sqarvo_connection::SessionManager;
    use sqarvo_drivers::DriverRegistry;
    use sqarvo_local_store::MemoryCredentialStore;

    use crate::profile_access::{self, TestAccess};

    /// 构造一个只含 `dev` SQLite 连接的访问服务。
    ///
    /// `enabled_tools = Some(vec![])` 表示全部禁用；`None` 表示字段缺失（全部启用）。
    /// 数据库文件不必存在：所有测试都在授权/参数/策略路径上提前返回，或自行建库。
    pub(crate) fn service(db_path: &Path, enabled_tools: Option<Vec<&str>>) -> TestAccess {
        let mut root = serde_json::json!({
            "connections": [{
                "alias": "dev",
                "driver_id": "sqlite",
                "host": db_path.display().to_string(),
                "port": 0,
                "enabled": true,
                "properties": { "read_only": "false" }
            }]
        });
        if let Some(tools) = enabled_tools {
            root["enabled_tools"] = serde_json::json!(tools);
        }
        let config = crate::config::parse(&root.to_string())
            .expect("解析测试配置")
            .validate()
            .expect("校验测试配置");
        let registry = Arc::new(DriverRegistry::with_builtin_drivers());
        let credentials = Arc::new(MemoryCredentialStore::new());
        let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
        profile_access::service(config, sessions)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use sqarvo_common::{Error, ErrorCategory, ErrorCode};

    use super::ToolCall;
    use crate::server::ServerContext;

    #[tokio::test]
    async fn audit_records_success_and_failure_without_raw_sql() {
        let dir = tempfile::tempdir().expect("tempdir");
        let context = ServerContext::bootstrap_with(
            Some(Path::new("/nonexistent/sqarvo-mcp/mcp.json")),
            dir.path(),
        )
        .await
        .expect("空连接列表也能装配");

        ToolCall::new(
            &context,
            "req-ok",
            Some("e2e 0.0.0".to_owned()),
            "list_tables",
        )
        .with_target("dev", Some("main"))
        .with_fingerprint("sha256:deadbeef")
        .succeeded(Some(3), Some(true))
        .await
        .expect("成功审计");

        let error = Error::new(
            ErrorCode::MCP_READONLY_VIOLATION,
            ErrorCategory::PermissionDenied,
            "SELECT secret FROM t",
        );
        let failure = ToolCall::new(&context, "req-fail", None, "execute_readonly_query")
            .with_target("dev", None)
            .failed(error)
            .await;
        assert_eq!(failure.code(), ErrorCode::MCP_READONLY_VIOLATION);

        let text = std::fs::read_to_string(context.audit().path()).expect("读审计");
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("每行都是 JSON"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["request_id"], "req-ok");
        assert_eq!(lines[0]["success"], true);
        assert_eq!(lines[0]["returned_rows"], 3);
        assert_eq!(lines[0]["truncated"], true);
        assert_eq!(lines[0]["sql_fingerprint"], "sha256:deadbeef");
        assert_eq!(lines[1]["success"], false);
        assert_eq!(lines[1]["error_code"], "permission_denied");
        assert_eq!(lines[1]["error_subcode"], "mcp.readonly_violation");
        assert!(
            !text.contains("SELECT secret"),
            "审计不得出现原始 SQL：{text}"
        );
    }
}
