//! Sqarvo 后端服务进程入口。
//!
//! 职责：
//!
//! 1. 解析本地目录与命令行参数；
//! 2. 构建驱动注册表、会话管理器与查询登记表；
//! 3. 绑定本地 IPC（Unix socket 或回环 TCP），生成会话 token；
//! 4. 写入端点描述文件（0600），启动 gRPC 服务；
//! 5. 收到 SIGINT/SIGTERM 后退出的清理路径。
//!
//! 安全约束：日志中绝不输出 token 原文；正常退出时删除 socket 与端点文件。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use sqarvo_common::Result;
use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::{
    AppPaths, AuthToken, EndpointInfo, UnavailableCredentialStore, open_default_credential_store,
    write_endpoint_file,
};
use sqarvo_query_engine::{QueryLimits, QueryRegistry};
use sqarvo_rpc_server::{LocalListener, PROTOCOL_VERSION, Services, serve};
use time::OffsetDateTime;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "sqarvo-backend",
    version,
    about = "Sqarvo 数据库客户端后端服务（本地 IPC）"
)]
struct Cli {
    /// Unix 域套接字路径；默认使用用户运行时目录。
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,

    /// 仅用于开发调试：监听回环 TCP 地址，例如 127.0.0.1:0。
    #[arg(long, value_name = "ADDR")]
    listen: Option<SocketAddr>,

    /// 日志过滤规则，例如 `info,sqarvo_rpc_server=debug`。
    #[arg(
        long,
        env = "SQARVO_LOG",
        default_value = "info",
        value_name = "FILTER"
    )]
    log_filter: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(&cli.log_filter);

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Display 只包含安全文案与稳定错误码，不含凭据与堆栈。
            tracing::error!(code = %error.code(), %error, "后端退出");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let paths = AppPaths::discover()?;
    paths.ensure_dirs()?;

    let registry = Arc::new(DriverRegistry::with_builtin_drivers());
    if registry.is_empty() {
        tracing::warn!("当前版本没有内置数据库驱动：连接请求将返回 driver.not_found，这是预期行为");
    }
    // 凭据存储：Linux 上是 Secret Service（D-Bus）。装配过程要连接会话总线并打开会话，
    // 属于阻塞调用，放进阻塞池以免占住运行时 worker（ADR 0014）。
    // 失败不会 panic：退回「不可用」实现，凭据相关操作如实报 credential.unavailable；
    // 日志只记实现名与错误码，不记凭据。
    let credentials = match tokio::task::spawn_blocking(open_default_credential_store).await {
        Ok(store) => store,
        Err(join_error) => {
            tracing::error!(%join_error, "凭据存储装配任务异常结束，退回不可用实现");
            Arc::new(UnavailableCredentialStore)
        }
    };
    tracing::info!(
        implementation = credentials.implementation(),
        "凭据存储已装配（不可用时凭据操作会如实报错，不回退明文）"
    );

    let sessions = Arc::new(SessionManager::with_credential_store(
        Arc::clone(&registry),
        credentials,
    ));
    let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
    let services = Services::new(Arc::clone(&sessions), Arc::clone(&registry), queries);

    let listener = match (cli.listen, cli.socket) {
        (Some(addr), _) => LocalListener::bind_tcp(addr)?,
        (None, Some(path)) => bind_socket(&path)?,
        (None, None) => default_listener(&paths)?,
    };

    let token = AuthToken::generate();
    let endpoint_path = paths.endpoint_file();
    let socket_path = listener.socket_path().map(Path::to_path_buf);
    let endpoint = build_endpoint_info(&listener, &token);
    write_endpoint_file(&endpoint_path, &endpoint)?;

    tracing::info!(
        endpoint = %endpoint_path.display(),
        transport = ?endpoint.transport,
        pid = std::process::id(),
        "后端已启动"
    );

    let result = serve(listener, token, services, shutdown_signal()).await;

    sessions.close_all().await;
    cleanup(socket_path.as_deref(), &endpoint_path);
    result
}

fn build_endpoint_info(listener: &LocalListener, token: &AuthToken) -> EndpointInfo {
    let tcp_addr = listener.tcp_addr();
    EndpointInfo {
        transport: listener.transport(),
        socket_path: listener
            .socket_path()
            .map(|path| path.display().to_string()),
        host: tcp_addr.map(|addr| addr.ip().to_string()),
        port: tcp_addr.map(|addr| addr.port()),
        token: token.expose().to_owned(),
        pid: std::process::id(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
        service_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_at: OffsetDateTime::now_utc(),
    }
}

/// 默认监听方式：Unix 平台使用域套接字，其他平台使用回环 TCP。
fn default_listener(paths: &AppPaths) -> Result<LocalListener> {
    #[cfg(unix)]
    {
        LocalListener::bind_unix(paths.socket_path())
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        LocalListener::bind_tcp(SocketAddr::from(([127, 0, 0, 1], 0)))
    }
}

fn bind_socket(path: &Path) -> Result<LocalListener> {
    #[cfg(unix)]
    {
        LocalListener::bind_unix(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(sqarvo_common::Error::unsupported(
            sqarvo_common::ErrorCode::TRANSPORT_FAILED,
            "此平台使用回环 TCP 传输，不支持 --socket",
        ))
    }
}

/// 清理退出：只删除自己的 socket 与端点描述文件。
fn cleanup(socket_path: Option<&Path>, endpoint_path: &Path) {
    if let Some(path) = socket_path
        && let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "清理 socket 文件失败");
    }
    if let Err(error) = std::fs::remove_file(endpoint_path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %endpoint_path.display(), %error, "清理端点文件失败");
    }
}

fn init_tracing(filter: &str) {
    let filter = EnvFilter::try_new(filter).unwrap_or_else(|error| {
        eprintln!("非法日志过滤规则（{error}），回退为 info");
        EnvFilter::new("info")
    });
    if let Err(error) = tracing_subscriber::fmt().with_env_filter(filter).try_init() {
        eprintln!("初始化日志失败: {error}");
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "无法监听 Ctrl-C 信号");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "无法监听 SIGTERM 信号");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("收到 Ctrl-C，正在退出"),
        () = terminate => tracing::info!("收到 SIGTERM，正在退出"),
    }
}
