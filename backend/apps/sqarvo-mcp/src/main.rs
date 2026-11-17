//! `sqarvo-mcp` 二进制入口（V3.1 §3.2）：
//!
//! - 无参数 / `serve`：stdio MCP Server 模式——stdout 只归协议所有；
//! - `credentials set|delete`：管理模式，不启动 server，不输出协议消息。
//!
//! 骨架提交只固定 CLI 形态与日志落点；两模式的实现在 #207 落地。

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use sqarvo_mcp::credentials_cmd::{self, CredentialsAction};

#[derive(Parser)]
#[command(name = "sqarvo-mcp", about = "Sqarvo MCP Server（stdio）")]
struct Cli {
    /// `mcp.json` 路径；缺省取 `AppPaths` 配置目录下的 `mcp.json`。
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 管理 MCP 引用的系统密钥环凭据。
    Credentials {
        #[command(subcommand)]
        action: CredentialsAction,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // stdout 只能输出 MCP 协议消息（V3.1 §13）。阶段 A spike 实测：默认 writer 会把
    // 日志混进协议流并直接破坏 initialize——writer 必须显式钉死在 stderr。
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,sqarvo_mcp=info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();

    let cli = Cli::parse();
    match cli.command {
        None => sqarvo_mcp::server::run_stdio(cli.config.as_deref()).await?,
        Some(Command::Credentials { action }) => credentials_cmd::run(action).await?,
    }
    Ok(())
}
