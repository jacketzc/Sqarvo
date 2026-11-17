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
//! # 工具路由（#209）
//!
//! [`ServerContext`] 是各泳道唯一需要的入口：配置、连接授权/懒会话、审计。
//! 工具实现与 `#[tool_router]` 位于 [`crate::tools`]（那里生成
//! `SqarvoMcpServer::tool_router()`）；本文件只持有路由表并把它接到
//! `#[tool_handler]` 上。`resources/*` 保持 #207 的空实现，归 #210。
//!
//! 路由表是**静态**的：`tools/list` 始终列出 V1 的 8 个工具；`enabled_tools`
//! 在执行期拒绝未启用的工具（`mcp.tool_disabled`），见 [`crate::tools`]。

use std::path::Path;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{
    Implementation, ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, tool_handler};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use sqarvo_local_store::AppPaths;

use crate::access::AccessService;
use crate::audit::{AuditWriter, RotationPolicy};
use crate::config::{self, ValidatedConfig};

/// `serverInfo.name`。
pub const SERVER_NAME: &str = "sqarvo-mcp";

/// 给客户端（Agent）的用法说明。
///
/// 只写**事实**：V1 是只读的、同一连接串行、默认并发 2。工具级细节归 #209 的
/// tool description，这里不重复。
const SERVER_INSTRUCTIONS: &str = "\
Sqarvo MCP Server 通过 mcp.json 中已授权的连接别名访问数据库，V1 只提供只读的\
结构与查询能力。同一个数据库连接上的查询会串行执行（单会话单在途语句），不同连接\
可在进程级并发上限内并行；排队不是故障。未在 enabled_tools 中启用的工具会被拒绝。\
所有调用记入本地审计日志（只记录 SQL 指纹，不记录原始 SQL 与结果）。";

/// #209 / #210 的扩展点：一次装配、三份共享状态。
pub struct ServerContext {
    config: Arc<ValidatedConfig>,
    access: AccessService,
    audit: AuditWriter,
}

impl ServerContext {
    /// 加载配置、装配服务并打开审计写入器。
    ///
    /// 配置缺失 → 空连接列表 + stderr 警告（默认拒绝）；配置非法 → 直接返回错误，
    /// 由 `main` 以非零码退出（V3.1 §3.1、§13）。
    pub async fn bootstrap(config_path: Option<&Path>) -> Result<Self> {
        let data_dir = AppPaths::discover()?.data_dir().to_path_buf();
        Self::bootstrap_with(config_path, &data_dir).await
    }

    /// 同 [`ServerContext::bootstrap`]，但显式指定数据目录（测试与打包场景）。
    pub async fn bootstrap_with(config_path: Option<&Path>, data_dir: &Path) -> Result<Self> {
        let outcome = config::load(config_path)?;
        for warning in &outcome.warnings {
            tracing::warn!("{warning}");
        }
        if outcome.found {
            tracing::info!(path = %outcome.path.display(), "已加载 mcp.json");
        }
        let validated = outcome.config.validate()?;
        for warning in validated.warnings() {
            tracing::warn!("{warning}");
        }
        let config = Arc::new(validated);
        tracing::info!(
            path = %outcome.path.display(),
            connections = config.connection_count(),
            tools = config.tools().enabled_names().len(),
            max_concurrency = config.limits().max_concurrency,
            query_timeout_ms = config.limits().query_timeout.as_millis(),
            "MCP 配置已生效"
        );

        let access = AccessService::assemble(Arc::clone(&config)).await?;
        let audit = AuditWriter::open(data_dir, RotationPolicy::default())?;
        tracing::info!(audit = %audit.path().display(), "审计日志已就绪");
        Ok(Self {
            config,
            access,
            audit,
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
}

impl SqarvoMcpServer {
    /// 由共享上下文构造（同时构建工具路由表）。
    #[must_use]
    pub fn new(context: Arc<ServerContext>) -> Self {
        Self {
            context,
            tool_router: Self::tool_router(),
        }
    }

    /// 共享上下文（工具实现里用 `self.context.access()` 等取状态）。
    #[must_use]
    pub const fn context(&self) -> &Arc<ServerContext> {
        &self.context
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

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = std::result::Result<ListResourcesResult, McpError>> + Send + '_
    {
        // #210 填充四个 database:// 资源；V1 骨架返回空列表。
        std::future::ready(Ok(ListResourcesResult::default()))
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<
        Output = std::result::Result<ListResourceTemplatesResult, McpError>,
    > + Send
    + '_ {
        std::future::ready(Ok(ListResourceTemplatesResult::default()))
    }
}

/// 加载配置并运行 stdio MCP server，直到客户端断开或收到终止信号。
///
/// 返回 `Ok(())` 表示干净退出（退出码 0）：stdin EOF、SIGINT、SIGTERM 都算。
pub async fn run_stdio(config_path: Option<&Path>) -> Result<()> {
    let context = Arc::new(ServerContext::bootstrap(config_path).await?);
    let server = SqarvoMcpServer::new(Arc::clone(&context));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        latest_protocol = %rmcp::model::ProtocolVersion::LATEST,
        "启动 stdio MCP server"
    );

    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|error| {
            Error::new(
                ErrorCode::MCP_INTERNAL,
                ErrorCategory::Internal,
                format!("MCP 会话初始化失败：{error}"),
            )
        })?;

    // 信号 → 取消 serve 循环。`RunningService::close()` 内部就是
    // 「cancel 这个 token + 等 JoinHandle」，所以下面等价于 serve_with_ct。
    let shutdown = running.cancellation_token();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("收到退出信号，开始优雅关停");
        shutdown.cancel();
    });

    let quit = running.waiting().await;
    match quit {
        Ok(reason) => tracing::info!(reason = ?reason, "MCP 会话结束"),
        Err(error) => tracing::warn!(%error, "MCP 主循环异常结束"),
    }

    context.shutdown().await;
    Ok(())
}

/// 等第一个退出信号：Ctrl-C（全平台）或 SIGTERM（仅 Unix）。
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                // 注册失败不致命：至少保留 Ctrl-C。
                tracing::warn!(%error, "无法注册 SIGTERM 处理器，仅响应 Ctrl-C");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    tracing::warn!(%error, "等待 Ctrl-C 失败");
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
            tracing::warn!(%error, "等待 Ctrl-C 失败");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn server_announces_all_v1_tools_and_the_resource_capability() {
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
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        registered.sort();
        let mut expected: Vec<String> = crate::config::V1_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        expected.sort();
        assert_eq!(
            registered, expected,
            "工具路由必须与 V1_TOOL_NAMES 一一对应"
        );
        assert_eq!(
            server.context().config().connection_count(),
            0,
            "缺配置时不得凭空造出连接"
        );
        assert!(server.context().audit().path().starts_with(dir.path()));
    }
}
