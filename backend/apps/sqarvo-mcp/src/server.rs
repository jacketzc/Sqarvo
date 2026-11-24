//! stdio MCP server 生命周期（#207 实现）。
//!
//! #207 的实现约束（均有阶段 A spike 实证依据，见 scratch/mcp-spike-rmcp/REPORT.md）：
//! - `rmcp::transport::stdio()` 承载协议，SIGINT/SIGTERM 自接优雅关停
//!   （SDK 不装信号处理器，默认直接杀进程，无清理）；
//! - stdin EOF → 正常退出（exit 0）；
//! - 退出路径必须调用 `SessionManager::close_all()` 并回收 `QueryRegistry` 登记。
//!
//! # 优雅关停怎么接
//!
//! spike 建议用 `serve_with_ct`，但它的参数是 `tokio_util::sync::CancellationToken`；
//! 本包按改动确认单只允许引用 workspace 已有依赖（`tokio-util` 不在其中），无法直接
//! 构造该类型。等价做法是用 `serve()` 拿到 `RunningService` 之后取
//! `cancellation_token()`：`RunningService::close()` 的内部实现正是
//! 「`cancellation_token.cancel()` + 等 JoinHandle」，所以「取消 + `waiting()`」与
//! `serve_with_ct` 的关停语义相同（rmcp 3.10.0 `service.rs:1109/1130-1188`）。
//!
//! **已知缺口**：信号处理器在 `serve()` 返回之后才注册，而 `serve()` 会一直等
//! initialize。也就是说「客户端连上但还没握手就收到 SIGTERM」时进程走系统默认处置
//! （立即终止）。此时既没有会话也没有在途查询，不需要清理；若要覆盖这一段，需要把
//! `tokio-util` 加进 workspace 依赖（见 ADR 0032「已知限制」）。
//!
//! # 工具路由（#209）与资源（#210）
//!
//! [`ServerContext`] 是各泳道唯一需要的入口：配置、连接授权/懒会话、审计。
//! 工具实现与 `#[tool_router]` 位于 [`crate::tools`]（那里生成
//! `SqarvoMcpServer::tool_router()`）；本文件只持有路由表并把它接到
//! `#[tool_handler]` 上，另外覆写 `list_tools`（按 `enabled_tools` 过滤）与
//! 三个 `resources/*` 处理器（实现见 [`crate::resources`]）。
//!
//! 路由表仍是**静态**的：8 个工具都在表里，`tools/list` 只是把被禁用的名字过滤掉；
//! 被禁用的工具即使被直接调用，也仍由执行期检查返回 `mcp.tool_disabled`
//! （`crate::tools`）。两处不是同一份判断，但都读同一份配置（`ToolPolicy`）。

use std::path::Path;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::{IntoCallToolResult, ToolCallContext};
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, Implementation,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ResourceContents,
    ResultType, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, tool_handler};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use sqarvo_local_store::{AppPaths, ProfileStore, profiles::migration};

use crate::access::{AccessService, OperationType};
use crate::audit::{AuditWriter, RotationPolicy};
use crate::config::{self, ValidatedConfig};
use crate::error::protocol_error;
use crate::resources;
use crate::tools::ToolCall;

/// `serverInfo.name`。
pub const SERVER_NAME: &str = "sqarvo-mcp";

/// 给客户端（Agent）的用法说明。
///
/// 只写**事实**：V1 是只读的、同一连接串行、默认并发 2。工具级细节归 #209 的
/// tool description，这里不重复。
const SERVER_INSTRUCTIONS: &str = "\nSqarvo MCP Server uses shared connection profiles when global data access is enabled. Query tools are always read-only.\nTurn off read-only protection to use controlled write tools. Previewing an operation does not authorize execution. Queries on the same connection run sequentially, with one in-flight statement per session.\nDifferent connections can run concurrently within the process-wide limit. Waiting in the queue is normal. Tools absent from enabled_tools are rejected.\nAll calls are recorded in the local audit log, which stores SQL fingerprints and omits raw SQL and results.";

/// #209 / #210 的扩展点：一次装配、三份共享状态。
pub struct ServerContext {
    config: Arc<ValidatedConfig>,
    access: AccessService,
    audit: AuditWriter,
    pub(crate) transactions: crate::transactions::TransactionRegistry,
    pub(crate) ddl: crate::ddl::Service,
}

impl ServerContext {
    /// 加载配置、装配服务并打开审计写入器。
    ///
    /// 配置缺失 → 空连接列表 + stderr 警告（默认拒绝）；配置非法 → 直接返回错误，
    /// 由 `main` 以非零码退出（V3.1 §3.1、§13）。
    pub async fn bootstrap(config_path: Option<&Path>) -> Result<Self> {
        let paths = AppPaths::discover()?;
        Self::bootstrap_at(config_path, paths).await
    }

    #[cfg(test)]
    pub(crate) async fn bootstrap_with(
        config_path: Option<&Path>,
        data_dir: &Path,
    ) -> Result<Self> {
        let paths = AppPaths::from_roots(
            data_dir.join("config"),
            data_dir.to_owned(),
            data_dir.join("run"),
        );
        let mut context = Self::bootstrap_at(config_path, paths).await?;
        context
            .ddl
            .use_test_credentials(Arc::new(sqarvo_local_store::MemoryCredentialStore::new()));
        Ok(context)
    }

    async fn bootstrap_at(config_path: Option<&Path>, paths: AppPaths) -> Result<Self> {
        let store = ProfileStore::new(&paths);
        let setup_store = store.clone();
        let config_path = config_path
            .unwrap_or(&paths.config_dir().join(config::CONFIG_FILE_NAME))
            .to_owned();
        let outcome = tokio::task::spawn_blocking(move || {
            migration::run_once_with_mcp(&setup_store, &config_path)?;
            let outcome = config::load(Some(&config_path))?;
            if !outcome.config.connections.is_empty() {
                return Err(Error::invalid_argument(
                    ErrorCode::CONFIG_INVALID,
                    "mcp.json still contains legacy connection profiles",
                ));
            }
            Ok(outcome)
        })
        .await
        .map_err(|_| Error::internal("The MCP configuration initialization task failed"))??;
        for warning in &outcome.warnings {
            tracing::warn!("{warning}");
        }
        if outcome.found {
            tracing::info!(path = %outcome.path.display(), "Loaded mcp.json");
        }
        let validated = outcome.config.validate()?;
        for warning in validated.warnings() {
            tracing::warn!("{warning}");
        }
        let config = Arc::new(validated);
        tracing::info!(
            path = %outcome.path.display(),
            tools = config.tools().enabled_names().len(),
            max_concurrency = config.limits().max_concurrency,
            query_timeout_ms = config.limits().query_timeout.as_millis(),
            "MCP configuration is active"
        );

        let access = AccessService::assemble(Arc::clone(&config), store).await?;
        let audit = AuditWriter::open(paths.data_dir(), RotationPolicy::default())?;
        tracing::info!(audit = %audit.path().display(), "The audit log is ready");
        Ok(Self {
            config,
            access,
            audit,
            transactions: crate::transactions::TransactionRegistry::default(),
            ddl: crate::ddl::Service::default(),
        })
    }

    /// 生效配置（含工具开关与限额）。
    #[must_use]
    pub const fn config(&self) -> &Arc<ValidatedConfig> {
        &self.config
    }

    /// 连接授权 + 懒会话 + 并发许可。
    #[must_use]
    pub const fn access(&self) -> &AccessService {
        &self.access
    }

    /// 审计写入器。
    #[must_use]
    pub const fn audit(&self) -> &AuditWriter {
        &self.audit
    }

    /// 关闭全部资源（进程退出路径）。
    pub async fn shutdown(&self) {
        tokio::join!(self.transactions.shutdown(), self.ddl.shutdown());
        self.access.close_all().await;
    }
}

/// MCP 服务处理器。
///
/// 工具路由由 [`crate::tools`] 的 `#[tool_router]` 生成；本类型持有配置、访问服务、
/// 审计三份共享状态与静态路由表。
#[derive(Clone)]
pub struct SqarvoMcpServer {
    context: Arc<ServerContext>,
    tool_router: ToolRouter<Self>,
    stdio_owner: Option<crate::transactions::Owner>,
}

impl SqarvoMcpServer {
    /// 由共享上下文构造（同时构建工具路由表）。
    #[must_use]
    pub fn new(context: Arc<ServerContext>) -> Self {
        Self {
            context,
            tool_router: Self::tool_router(),
            stdio_owner: Some(crate::transactions::Owner::new(
                tokio_util::sync::CancellationToken::new(),
            )),
        }
    }

    pub(crate) fn new_http(context: Arc<ServerContext>) -> Self {
        Self {
            context,
            tool_router: Self::tool_router(),
            stdio_owner: None,
        }
    }

    /// 共享上下文（工具实现里用 `self.context.access()` 等取状态）。
    #[must_use]
    pub const fn context(&self) -> &Arc<ServerContext> {
        &self.context
    }

    /// `tools/list` 的可见工具：注册表里被 `enabled_tools` 允许的那些。
    ///
    /// 注册表是静态的，过滤只发生在播发这一层；执行期仍由 `crate::tools` 的
    /// `ensure_enabled` 拒绝被禁用的工具（两处都读同一份 [`crate::config::ToolPolicy`]）。
    #[must_use]
    pub fn visible_tools(&self) -> Vec<Tool> {
        let policy = self.context.config().tools();
        self.tool_router
            .list_all()
            .into_iter()
            .chain(crate::tools::write::descriptions())
            .chain(crate::tools::transaction::descriptions())
            .chain(crate::tools::ddl::descriptions())
            .filter(|tool| policy.allows(tool.name.as_ref()))
            .map(|tool| {
                if config::SCHEMA_TOOL_NAMES.contains(&tool.name.as_ref()) {
                    tool.with_annotations(rmcp::model::ToolAnnotations::new().read_only(true))
                } else {
                    tool
                }
            })
            .collect()
    }
}

// `#[tool_handler]` 生成的 `list_tools` 等包装函数是 async 但内部只返回现成结果；
// 这是宏的形状约束，不是本模块的选择。
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for SqarvoMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
        .with_instructions(SERVER_INSTRUCTIONS)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, McpError> {
        if let Some(tool) = config::DDL_TOOL_NAMES
            .into_iter()
            .find(|tool| *tool == request.name)
        {
            let owner = match context.extensions.get::<axum::http::request::Parts>() {
                Some(parts) => parts
                    .extensions
                    .get::<crate::http::HttpSessionLifetime>()
                    .map(|lifetime| lifetime.0.clone()),
                None => self.stdio_owner.clone(),
            };
            return crate::tools::ddl::call(&self.context, owner, tool, request, context).await;
        }
        if let Some(tool) = config::TRANSACTION_TOOL_NAMES
            .into_iter()
            .find(|tool| *tool == request.name)
        {
            let owner = match context.extensions.get::<axum::http::request::Parts>() {
                Some(parts) => parts
                    .extensions
                    .get::<crate::http::HttpSessionLifetime>()
                    .map(|lifetime| lifetime.0.clone()),
                None => self.stdio_owner.clone(),
            };
            return crate::tools::transaction::call(&self.context, owner, tool, request, context)
                .await;
        }
        if let Some(tool) = config::V2_TOOL_NAMES
            .into_iter()
            .find(|tool| *tool == request.name)
        {
            return crate::tools::write::call(&self.context, tool, request, context).await;
        }
        if let Some(tool) = config::V1_TOOL_NAMES
            .iter()
            .chain(&config::SCHEMA_TOOL_NAMES)
            .find(|name| **name == request.name)
            && let Err(error) = self
                .context
                .access()
                .guard()
                .authorize_request(OperationType::Read, None)
                .await
        {
            let mut call = ToolCall::from_request(&self.context, &context, tool);
            if let Some(args) = &request.arguments
                && let Some(alias) = args
                    .get("connection_alias")
                    .and_then(serde_json::Value::as_str)
            {
                let database = args.get("database").and_then(serde_json::Value::as_str);
                call = call.with_target(alias, database);
            }
            return call.failed(error).await.into_call_tool_result();
        }
        self.tool_router
            .call(ToolCallContext::new(self, request, context))
            .await
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = std::result::Result<ListResourcesResult, McpError>> + Send + '_
    {
        // 只有静态资源；模板走 `resources/templates/list`。两者都不触网。
        std::future::ready(Ok(ListResourcesResult {
            resources: resources::list(),
            ..Default::default()
        }))
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<
        Output = std::result::Result<ListResourceTemplatesResult, McpError>,
    > + Send
    + '_ {
        std::future::ready(Ok(ListResourceTemplatesResult {
            resource_templates: resources::templates(),
            ..Default::default()
        }))
    }

    /// `resources/read`：解析 URI（拆段 → 逐段解码一次 → 判形状）后才授权取数。
    ///
    /// 错误按 [`crate::resources`] 的钉死口径映射：URI 不合法 → `-32602`
    /// （`mcp.invalid_argument`），资源不存在/未授权 → `-32002`。
    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = std::result::Result<ReadResourceResponse, McpError>> + Send + '_
    {
        let uri = request.uri;
        async move {
            match resources::read(self.context.access(), &uri).await {
                Ok(payload) => Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(payload.json, payload.uri)
                        .with_mime_type(resources::MIME_TYPE),
                ])
                .into()),
                Err(error) => {
                    tracing::debug!(code = %error.code(), "Failed to read the resource");
                    let response = protocol_error(&error);
                    if matches!(
                        error.code(),
                        ErrorCode::MCP_CONNECTION_DENIED
                            | ErrorCode::PROFILE_RESTART_REQUIRED
                            | ErrorCode::PROFILE_AMBIGUOUS_NAME
                    ) {
                        ToolCall::from_request(&self.context, &context, "resources/read")
                            .failed(error)
                            .await;
                    }
                    Err(response)
                }
            }
        }
    }

    /// `tools/list`：只列出 `enabled_tools` 允许的工具。
    ///
    /// `#[tool_handler]` 只有在 impl 块里没有 `list_tools` 时才生成默认实现
    /// （rmcp-macros 3.10.0 `tool_handler.rs:64`），所以这里的手写版本就是生效实现；
    /// 其余形状（缓存字段、`resultType`）与默认实现保持一致，避免同一工具在
    /// 新旧协议下被描述成两种东西。
    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = std::result::Result<ListToolsResult, McpError>> + Send + '_
    {
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= rmcp::model::ProtocolVersion::V_2026_07_28);
        let tools = self.visible_tools();
        std::future::ready(Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools,
            meta: None,
            next_cursor: None,
            ttl_ms: supports_cache_hints.then_some(0),
            cache_scope: supports_cache_hints.then_some(CacheScope::Public),
        }))
    }
}

/// 加载配置并运行 stdio MCP server，直到客户端断开或收到终止信号。
///
/// 返回 `Ok(())` 表示干净退出（退出码 0）：stdin EOF、SIGINT、SIGTERM 都算。
///
/// # 握手前 EOF 也是干净退出（V3.1 §13，#220 修复）
///
/// 客户端连上管道却还没完成 initialize 就关闭 stdin（或只做 `server/discover`
/// 后断开）时，rmcp 的 `serve()` 以
/// `ServerInitializeError::ConnectionClosed("initialize request")` 返回
/// （Display 形态 `connection closed: initialize request`）。这是「对端在握手
/// 完成前离开」，不是服务故障：记 info 后按 EOF 正常收尾（exit 0）。
///
/// 判据只认**类型 + 消息**，且只作用于 `serve()`（握手阶段）这一条路径：
/// 类型必须是 `ConnectionClosed`（对端关闭），且消息里的等待上下文必须提到
/// `initialize`——rmcp 3.10.0 服务端唯一构造点是 `service/server.rs:571` 等待
/// `"initialize request"`。
///
/// 2026-07-28 的无握手 `_meta` 会话**不走这条错误路径**：`serve()` 对它是
/// `Ok(RunningService)`（`service/server.rs:644-669` 的 inline-meta 分支），
/// 之后 EOF 经 `waiting()` 得到 `Ok(Closed)`——本判据不会把它误伤
/// （回归钉死见 `tests/protocol_matrix.rs::bare_requests_with_draft_meta_work_without_any_handshake`）。
pub async fn run_stdio(config_path: Option<&Path>) -> Result<()> {
    let context = Arc::new(ServerContext::bootstrap(config_path).await?);
    let server = SqarvoMcpServer::new(Arc::clone(&context));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        latest_protocol = %rmcp::model::ProtocolVersion::LATEST,
        "Starting the stdio MCP server"
    );

    let running = match Box::pin(server.serve(rmcp::transport::stdio())).await {
        Ok(running) => running,
        // 握手前对端关闭 = EOF 收尾，见函数文档；其余 serve 错误照旧非零 + 诊断。
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(waiting_on))
            if waiting_on.contains("initialize") =>
        {
            tracing::info!(
                "The client disconnected before the handshake (connection closed: {waiting_on}). Treating this as normal EOF (V3.1 §13, exit 0)"
            );
            context.shutdown().await;
            return Ok(());
        }
        Err(error) => {
            return Err(Error::new(
                ErrorCode::MCP_INTERNAL,
                ErrorCategory::Internal,
                format!("Failed to initialize the MCP session: {error}"),
            ));
        }
    };

    // 信号 → 取消 serve 循环。`RunningService::close()` 内部就是
    // 「cancel 这个 token + 等 JoinHandle」，所以下面等价于 serve_with_ct。
    let shutdown = running.cancellation_token();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("Received a shutdown signal. Starting graceful shutdown");
        shutdown.cancel();
    });

    let quit = running.waiting().await;
    match quit {
        Ok(reason) => tracing::info!(reason = ?reason, "MCP session ended"),
        Err(error) => tracing::warn!(%error, "The MCP main loop exited unexpectedly"),
    }

    context.shutdown().await;
    Ok(())
}

/// 等第一个退出信号：Ctrl-C（全平台）或 SIGTERM（仅 Unix）。
pub(crate) async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                // 注册失败不致命：至少保留 Ctrl-C。
                tracing::warn!(%error, "Failed to register the SIGTERM handler. Only Ctrl-C will be handled");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    tracing::warn!(%error, "Failed to wait for Ctrl-C");
                }
            }
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        // Windows 没有可被普通进程捕获的 SIGTERM；Ctrl-C 与控制台关闭事件由
        // `ctrl_c()` 覆盖（见 ADR 0002 附记的同类口径）。
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "Failed to wait for Ctrl-C");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn server_announces_all_tools_and_the_resource_capability() {
        let dir = tempfile::tempdir().expect("tempdir");
        let context = ServerContext::bootstrap_with(
            Some(Path::new("/nonexistent/sqarvo-mcp/test-mcp.json")),
            dir.path(),
        )
        .await
        .expect("缺配置也能起服务（空连接列表）");
        let server = SqarvoMcpServer::new(Arc::new(context));
        let info = server.get_info();
        assert_eq!(info.server_info.name, SERVER_NAME);
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.resources.is_some());
        assert!(info.instructions.is_some());
        let mut registered: Vec<String> = server
            .visible_tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        registered.sort();
        let mut expected: Vec<String> = crate::config::tool_names().map(str::to_owned).collect();
        expected.sort();
        assert_eq!(registered, expected, "工具发现必须与配置工具名单一一对应");
        assert_eq!(
            server.context().config().connection_count(),
            0,
            "缺配置时不得凭空造出连接"
        );
        assert!(server.context().audit().path().starts_with(dir.path()));
    }

    /// 写一份 mcp.json（内容由调用方给），返回路径。
    fn write_config(dir: &Path, root: &serde_json::Value) -> std::path::PathBuf {
        let path = dir.join("mcp.json");
        std::fs::write(&path, root.to_string()).expect("写配置");
        path
    }

    fn sqlite_root(enabled_tools: Option<serde_json::Value>) -> serde_json::Value {
        let mut root = serde_json::json!({
            "connections": [{
                "alias": "dev",
                "driver_id": "sqlite",
                "host": "/nonexistent/sqarvo-mcp-tools-list.db",
                "port": 0,
                "enabled": true
            }]
        });
        if let Some(tools) = enabled_tools {
            root["enabled_tools"] = tools;
        }
        root
    }

    /// `enabled_tools` 的三种状态在 `tools/list` 上的投影（V3.1 §3.1）。
    ///
    /// 注册表始终是 8 个；只有播发层按开关过滤。执行期的拒绝行为由
    /// `crate::tools` 的测试覆盖，这里只钉「列表不再出现被禁用的名字」。
    #[tokio::test]
    async fn enabled_tools_filter_the_advertised_tool_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let all = crate::config::tool_names().count();

        let context = ServerContext::bootstrap_with(
            Some(Path::new("/nonexistent/sqarvo-mcp/test-mcp.json")),
            dir.path(),
        )
        .await
        .expect("缺配置也能起服务");
        let server = SqarvoMcpServer::new(Arc::new(context));
        assert_eq!(server.visible_tools().len(), all, "字段缺失 = 全部可见");

        let config = write_config(
            dir.path(),
            &sqlite_root(Some(serde_json::json!(["list_tables", "describe_table"]))),
        );
        let context = ServerContext::bootstrap_with(Some(&config), dir.path())
            .await
            .expect("合法配置");
        let server = SqarvoMcpServer::new(Arc::new(context));
        let mut visible: Vec<String> = server
            .visible_tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        visible.sort();
        assert_eq!(visible, vec!["describe_table", "list_tables"]);
        assert_eq!(
            server.tool_router.list_all().len(),
            crate::config::V1_TOOL_NAMES.len() + crate::config::SCHEMA_TOOL_NAMES.len(),
            "路由表仍是静态的：过滤只发生在播发层"
        );
        // 被禁用的工具仍可被路由到（执行期由 ensure_enabled 拒绝），
        // 这样 Agent 拿到的是 mcp.tool_disabled 而不是 -32602「工具不存在」。
        assert!(server.tool_router.get("execute_readonly_query").is_some());

        let context = ServerContext::bootstrap_with(
            Some(&write_config(
                dir.path(),
                &sqlite_root(Some(serde_json::json!([]))),
            )),
            dir.path(),
        )
        .await
        .expect("空数组不阻止启动");
        let server = SqarvoMcpServer::new(Arc::new(context));
        assert!(
            server.visible_tools().is_empty(),
            "空数组 = 一个工具都不播发"
        );
    }

    /// 资源清单与模板：静态资源一个、模板三个，全部 `application/json`。
    #[tokio::test]
    async fn resources_and_templates_are_advertised_without_touching_the_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let context = ServerContext::bootstrap_with(
            Some(Path::new("/nonexistent/sqarvo-mcp/test-mcp.json")),
            dir.path(),
        )
        .await
        .expect("缺配置也能起服务");
        let server = SqarvoMcpServer::new(Arc::new(context));

        let listed = crate::resources::list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].uri, crate::resources::CONNECTIONS_URI);
        assert_eq!(listed[0].mime_type.as_deref(), Some("application/json"));

        let templates = crate::resources::templates();
        let uris: Vec<&str> = templates
            .iter()
            .map(|template| template.uri_template.as_str())
            .collect();
        assert_eq!(
            uris,
            vec![
                crate::resources::OVERVIEW_TEMPLATE,
                crate::resources::SCHEMA_TEMPLATE,
                crate::resources::TABLE_TEMPLATE
            ]
        );
        for template in &templates {
            assert_eq!(template.mime_type.as_deref(), Some("application/json"));
            assert!(template.description.is_some());
        }
        assert_eq!(
            server
                .context()
                .access()
                .session_manager()
                .active_count()
                .await,
            0,
            "列出资源/模板不得建立任何数据库连接"
        );
    }
}
