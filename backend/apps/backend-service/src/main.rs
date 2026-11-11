//! Sqarvo 后端服务进程入口。
//!
//! 职责：
//!
//! 1. 解析本地目录与命令行参数；
//! 2. 构建驱动注册表、会话管理器与查询登记表；
//! 3. 绑定本地 IPC（Unix socket 或回环 TCP），生成会话 token；
//! 4. 写入端点描述文件（0600），启动 gRPC 服务；
//! 5. 收到 SIGINT/SIGTERM、或父进程（前端）消失后退出并清理。
//!
//! 安全约束：日志中绝不输出 token 原文；正常退出时删除 socket 与端点文件。
//!
//! # 退出与清理（三平台）
//!
//! Ctrl-C、Unix `SIGTERM`（Linux 上也可能来自 `PR_SET_PDEATHSIG`）与父进程消失三条
//! 触发路径都汇入 [`shutdown_signal`]，之后统一执行 `sessions.close_all()` → 删除
//! socket（仅 Unix）→ 删除端点文件。没有第二条清理路径，避免「某个分支忘了删文件」。
//!
//! **Windows 上没有可捕获的 `SIGTERM`**：前端如果直接用 `TerminateProcess` 结束后端，
//! 进程不会执行这里的任何清理代码——会话不会优雅关闭，端点文件会残留。这是 Win32 的
//! 语义（没有等价于 `SIGTERM` 的、可被所有进程处理的终止通知），不是本模块的缺陷：
//!
//! - 「前端退出后不留孤儿后端」由前端负责（创建 Job Object 并设置
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，把后端放进该 Job）；
//! - 被 `TerminateProcess` 之后残留的端点文件也由前端删除，或由前端凭文件中的 pid
//!   判活后当作残留忽略；
//! - 后端保证的是：只要它还在运行且收到了可捕获的退出信号（Ctrl-C、父进程消失），
//!   就一定走同一条清理路径。详见 `docs/decisions/0002-local-ipc-transport.md` 附记。
//!
//! # 端点描述文件路径
//!
//! 路径来源按优先级排列：显式 `--endpoint-file`、环境变量 `SQARVO_ENDPOINT_FILE`
//! （Unix 兼容口径）、默认运行时目录。Windows 没有 `XDG_RUNTIME_DIR`，前端应始终
//! 显式传 `--endpoint-file`，让「后端写哪儿」和「前端读哪儿」来自同一个参数，
//! 而不是依赖两套约定恰好一致。

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use sqarvo_common::{Error, ErrorCode, Result};
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

/// 端点描述文件路径的兼容环境变量。
///
/// Linux 前端历史上用它（配合 `XDG_RUNTIME_DIR`）覆盖端点位置，因此保留；
/// 新代码（尤其 Windows 前端）应当传显式的 `--endpoint-file`。
const ENDPOINT_FILE_ENV: &str = "SQARVO_ENDPOINT_FILE";

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

    /// 端点描述文件路径；优先于 `SQARVO_ENDPOINT_FILE` 与默认运行时目录。
    ///
    /// 前端（尤其 Windows）应当显式传入绝对路径：Windows 没有 `XDG_RUNTIME_DIR`，
    /// 靠环境变量推导会让「后端写哪儿」和「前端读哪儿」依赖两套不同的约定。
    #[arg(long, value_name = "PATH")]
    endpoint_file: Option<PathBuf>,

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

    /// 内部参数（不出现在 --help）：启动本进程的前端 pid。
    ///
    /// 只有显式给出时才监视父进程：手工启动的后端父进程可能是 shell，
    /// 无条件设置 `PR_SET_PDEATHSIG` 会被误杀。
    #[arg(long, value_name = "PID", hide = true)]
    parent_pid: Option<u32>,
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
    // 先注册 SIGTERM 处理器，再设置 PR_SET_PDEATHSIG：反过来会留下一个窗口期，
    // 父进程在这段时间退出时 SIGTERM 走默认处置（立刻终止），来不及清理。
    let signals = Signals::register();
    let parent = sqarvo_parent_watch::install(cli.parent_pid).map_err(|error| {
        // 参数错误的分类与文案由调用方决定：crate 只管判活，不认识项目错误码。
        Error::invalid_argument(ErrorCode::CONFIG_INVALID, error.to_string())
    })?;
    if let sqarvo_parent_watch::Installed::ParentGone(pid) = parent {
        // 还没有创建目录、绑定 socket 或写端点文件：直接退出即可。
        tracing::warn!(parent_pid = pid, "父进程已不存在，退出");
        return Ok(());
    }

    let paths = AppPaths::discover()?;
    paths.ensure_dirs()?;
    let endpoint_path = resolve_endpoint_path(
        cli.endpoint_file,
        std::env::var_os(ENDPOINT_FILE_ENV),
        paths.endpoint_file(),
    );

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
        Arc::clone(&credentials),
    ));
    let queries = Arc::new(QueryRegistry::new(QueryLimits::default()));
    // 同一份凭据存储同时注入会话管理与 CredentialService：写入的密码必须能被
    // 连接路径读到（ADR 0015 / 0016）。
    let services = Services::new(
        Arc::clone(&sessions),
        Arc::clone(&registry),
        queries,
        credentials,
    );

    let listener = match (cli.listen, cli.socket) {
        (Some(addr), _) => LocalListener::bind_tcp(addr)?,
        (None, Some(path)) => bind_socket(&path)?,
        (None, None) => default_listener(&paths)?,
    };

    let token = AuthToken::generate();
    let socket_path = listener.socket_path().map(Path::to_path_buf);
    let endpoint = build_endpoint_info(&listener, &token);
    write_endpoint_file(&endpoint_path, &endpoint)?;

    tracing::info!(
        endpoint = %endpoint_path.display(),
        transport = ?endpoint.transport,
        pid = std::process::id(),
        "后端已启动"
    );

    let result = serve(listener, token, services, async move {
        shutdown_signal(signals, parent).await.log();
    })
    .await;

    // 唯一的清理路径：三种退出触发都从这里过。
    sessions.close_all().await;
    cleanup(socket_path.as_deref(), &endpoint_path);
    result
}

/// 端点描述文件路径：显式参数 > 环境变量 > 默认推导。
///
/// 空的环境变量按「未设置」处理：`SQARVO_ENDPOINT_FILE=` 这种写法应当退回默认值，
/// 而不是让后端把路径变成空字符串后报错。
fn resolve_endpoint_path(
    explicit: Option<PathBuf>,
    env: Option<OsString>,
    default: PathBuf,
) -> PathBuf {
    explicit
        .or_else(|| env.filter(|value| !value.is_empty()).map(PathBuf::from))
        .unwrap_or(default)
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
///
/// 幂等：文件已被别处删掉（例如前端在 `TerminateProcess` 之后补删）不算错误。
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

/// 关闭信号：SIGTERM 流在 `run` 的早期（设置 PDEATHSIG 之前）就注册好。
struct Signals {
    #[cfg(unix)]
    terminate: Option<tokio::signal::unix::Signal>,
}

impl Signals {
    /// 注册 SIGTERM；失败时只告警，后端仍能通过 Ctrl-C 与父进程轮询退出。
    fn register() -> Self {
        #[cfg(unix)]
        {
            let terminate =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(signal) => Some(signal),
                    Err(error) => {
                        tracing::warn!(%error, "无法监听 SIGTERM 信号");
                        None
                    }
                };
            Self { terminate }
        }
        #[cfg(not(unix))]
        {
            // Windows：`TerminateProcess` 无法被捕获，只能等 Ctrl-C 或父进程消失。
            Self {}
        }
    }
}

/// 退出触发原因。三条路径的**作用完全相同**，保留这个枚举只为日志措辞。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownReason {
    /// Ctrl-C（Windows 控制台对应 `CTRL_C_EVENT`）。
    Interrupt,
    /// Unix `SIGTERM`（Linux 上也可能来自 `PR_SET_PDEATHSIG`）。
    Terminate,
    /// 前端父进程消失。
    ParentGone(u32),
}

impl ShutdownReason {
    /// 记录触发原因；不参与清理逻辑本身。
    fn log(self) {
        match self {
            Self::Interrupt => tracing::info!("收到 Ctrl-C，正在退出"),
            Self::Terminate => tracing::info!("收到 SIGTERM，正在退出"),
            Self::ParentGone(pid) => {
                tracing::warn!(parent_pid = pid, "父进程已消失，开始清理退出");
            }
        }
    }
}

/// 等待停机触发；三条分支返回同一个 [`ShutdownReason`]，作用完全相同。
async fn shutdown_signal(
    signals: Signals,
    parent: sqarvo_parent_watch::Installed,
) -> ShutdownReason {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "无法监听 Ctrl-C 信号");
        }
    };

    tokio::select! {
        () = ctrl_c => ShutdownReason::Interrupt,
        () = wait_terminate(signals) => ShutdownReason::Terminate,
        pid = sqarvo_parent_watch::wait_until_gone(parent) => ShutdownReason::ParentGone(pid),
    }
}

/// 等待 SIGTERM；注册失败时永不返回（其余退出路径仍然有效）。
async fn wait_terminate(signals: Signals) {
    #[cfg(unix)]
    match signals.terminate {
        Some(mut signal) => {
            signal.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
    #[cfg(not(unix))]
    {
        let _ = signals;
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(raw: &str) -> PathBuf {
        PathBuf::from(raw)
    }

    #[test]
    fn explicit_endpoint_file_wins_over_env_and_default() {
        assert_eq!(
            resolve_endpoint_path(
                Some(path("/run/explicit.json")),
                Some(OsString::from("/run/from-env.json")),
                path("/run/default.json"),
            ),
            path("/run/explicit.json")
        );
    }

    #[test]
    fn env_is_used_without_explicit_argument() {
        // `SQARVO_ENDPOINT_FILE` 保留为 Unix 兼容口径。
        assert_eq!(
            resolve_endpoint_path(
                None,
                Some(OsString::from("/run/from-env.json")),
                path("/run/default.json"),
            ),
            path("/run/from-env.json")
        );
    }

    #[test]
    fn default_is_used_without_any_override() {
        assert_eq!(
            resolve_endpoint_path(None, None, path("/run/default.json")),
            path("/run/default.json")
        );
    }

    #[test]
    fn empty_env_value_falls_back_to_default() {
        assert_eq!(
            resolve_endpoint_path(None, Some(OsString::from("")), path("/run/default.json")),
            path("/run/default.json")
        );
    }

    #[test]
    fn cleanup_removes_both_artifacts_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("backend.sock");
        let endpoint = root.path().join("backend-endpoint.json");
        std::fs::write(&socket, b"").unwrap();
        std::fs::write(&endpoint, b"{}").unwrap();

        cleanup(Some(&socket), &endpoint);
        assert!(!socket.exists());
        assert!(!endpoint.exists());

        // 第二次调用（例如前端已经补删过）不应报错或 panic。
        cleanup(Some(&socket), &endpoint);
        assert!(!endpoint.exists());
    }

    #[test]
    fn cleanup_without_socket_only_removes_endpoint() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = root.path().join("backend-endpoint.json");
        std::fs::write(&endpoint, b"{}").unwrap();

        cleanup(None, &endpoint);
        assert!(!endpoint.exists());
    }

    #[test]
    fn shutdown_reasons_are_distinct_and_keep_parent_pid() {
        assert_ne!(ShutdownReason::Interrupt, ShutdownReason::Terminate);
        assert_eq!(
            ShutdownReason::ParentGone(4321),
            ShutdownReason::ParentGone(4321)
        );
        assert_ne!(ShutdownReason::ParentGone(1), ShutdownReason::ParentGone(2));
    }
}
