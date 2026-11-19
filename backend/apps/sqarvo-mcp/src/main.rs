//! `sqarvo-mcp` 二进制入口（V3.1 §3.2）：
//!
//! - 无参数：stdio MCP Server 模式——stdout 只归协议所有；
//! - `credentials` / `access`：本地管理模式，不启动 server。
//!
//! # stdout 纯净性
//!
//! 阶段 A spike 实测：`tracing_subscriber` 默认 writer 是 stdout，会把
//! `INFO serve_inner: Service initialized…` 直接插进协议流，任何严格客户端都会断连
//! （REPORT.md §5）。因此：
//!
//! 1. subscriber 显式 `.with_writer(std::io::stderr)`；
//! 2. 本文件与 `sqarvo_mcp` 全包禁用 `println!`/`print!`/`dbg!`（管理模式的结果输出
//!    是唯一的例外，且那条路径不会启动 server）；
//! 3. 退出错误写 stderr 并给非零退出码。

mod access_cmd;

use std::path::PathBuf;
use std::process::ExitCode;

use access_cmd::AccessAction;
use clap::{Parser, Subcommand};
use sqarvo_mcp::credentials_cmd::{self, CredentialsAction};

/// 日志过滤环境变量；缺省 `warn,sqarvo_mcp=info`。
const LOG_ENV: &str = "SQARVO_MCP_LOG";

/// 退出时等待 Tokio 阻塞线程的上限。
///
/// rmcp 的 stdio transport 用 `tokio::io::stdin`，它背后是一个阻塞读线程；stdin 还
/// 开着时这个读永远不返回。`Runtime` 默认的 drop 会**无限期**等阻塞任务结束，于是
/// 「收到 SIGTERM → 优雅关停已完成 → 进程却挂着不走」。这里显式给一个上限：干净
/// 路径（stdin EOF）本来就立即返回，只有信号路径会用到这个上限。
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Parser)]
#[command(name = "sqarvo-mcp", version, about = "Sqarvo MCP Server（stdio）")]
struct Cli {
    /// `mcp.json` 路径；缺省取 `AppPaths` 配置目录下的 `mcp.json`。
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 启动回环 MCP HTTP 服务。
    Serve {
        #[arg(long, default_value = "http", value_parser = ["http"])]
        transport: String,
        #[arg(long, default_value = "127.0.0.1", value_parser = ["127.0.0.1"])]
        host: String,
        #[arg(long, default_value_t = 8765, value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
        #[arg(long, hide = true)]
        managed: bool,
    },
    /// 管理本机 MCP HTTP 认证。
    HttpAuth {
        #[command(subcommand)]
        action: HttpAuthAction,
    },
    /// 管理 MCP 全局数据访问策略。
    Access {
        #[command(subcommand)]
        action: AccessAction,
    },
    /// 管理 MCP 引用的系统密钥环凭据。
    Credentials {
        #[command(subcommand)]
        action: CredentialsAction,
    },
}

#[derive(Subcommand)]
enum HttpAuthAction {
    /// 轮换 Bearer 凭据。
    Rotate,
    /// 撤销 Bearer 凭据。
    Revoke,
    /// 从系统凭据库向指定的本地 Codex 客户端提供认证。
    Launch {
        #[arg(value_parser = ["codex"])]
        client: String,
        /// 启动 Codex App Server 的 stdio 接口。
        #[arg(long)]
        app_server: bool,
        #[arg(long, default_value_t = 8765, value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
    },
}

fn main() -> ExitCode {
    init_tracing();

    let cli = Cli::parse();
    // 手写 runtime（而不是 `#[tokio::main]`）是为了能调 `shutdown_timeout`：
    // 见 [`SHUTDOWN_TIMEOUT`] 的说明。
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("sqarvo-mcp: 无法创建 Tokio 运行时：{error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        match cli.command {
            None => sqarvo_mcp::server::run_stdio(cli.config.as_deref()).await,
            Some(Command::Serve { port, managed, .. }) => {
                let result = sqarvo_mcp::http::run(cli.config.as_deref(), port, managed).await;
                if managed && let Err(error) = &result {
                    println!("error {}", error.code());
                }
                result
            }
            Some(Command::HttpAuth { action }) => match action {
                HttpAuthAction::Rotate => sqarvo_mcp::http::rotate_token(),
                HttpAuthAction::Revoke => sqarvo_mcp::http::revoke_token(),
                HttpAuthAction::Launch {
                    port, app_server, ..
                } => sqarvo_mcp::http::launch_codex(port, app_server),
            },
            Some(Command::Credentials { action }) => {
                access_cmd::management_store(cli.config.as_deref())?;
                credentials_cmd::run(action).await
            }
            Some(Command::Access { action }) => {
                let store = access_cmd::management_store(cli.config.as_deref())?;
                access_cmd::run(&store, &action)
            }
        }
    });
    runtime.shutdown_timeout(SHUTDOWN_TIMEOUT);

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // 只写 stderr：stdout 归协议所有。错误码是稳定的（`mcp.*` / `config.invalid`…），
            // 文案由各模块保证不含凭据。
            eprintln!("sqarvo-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    // writer 必须显式钉在 stderr（见模块文档）。
    let filter = tracing_subscriber::EnvFilter::try_from_env(LOG_ENV)
        .or_else(|_| tracing_subscriber::EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| "warn,sqarvo_mcp=info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
