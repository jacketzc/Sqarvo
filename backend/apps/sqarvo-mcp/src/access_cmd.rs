//! 本地终端专用的共享档案授权命令。

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use clap::Subcommand;
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_local_store::{AppPaths, McpAccessPolicy, ProfileStore, profiles::migration};

#[derive(Subcommand)]
pub enum AccessAction {
    /// 允许 MCP 访问共享连接档案。
    On {
        /// 确认授权，允许在非交互终端运行。
        #[arg(long)]
        yes: bool,
    },
    /// 撤销 MCP 访问并恢复只读策略。
    Off,
    /// 显示两个开关、档案数与文档修订。
    Status,
    /// 管理 MCP 全局只读策略。
    ReadOnly {
        #[command(subcommand)]
        action: ReadOnlyAction,
    },
}

#[derive(Subcommand)]
pub enum ReadOnlyAction {
    /// 启用只读策略。
    On,
    /// 关闭只读策略。
    Off {
        /// 确认关闭只读策略，允许在非交互终端运行。
        #[arg(long)]
        yes: bool,
    },
    /// 显示当前授权状态。
    Status,
}

pub fn management_store(config: Option<&Path>) -> Result<ProfileStore> {
    let paths = AppPaths::discover()?;
    let store = ProfileStore::new(&paths);
    if let Some(path) = config {
        migration::run_once_with_mcp(&store, path)?;
    } else {
        store.run_once_migration()?;
    }
    Ok(store)
}

pub fn run(store: &ProfileStore, action: &AccessAction) -> Result<()> {
    match action {
        AccessAction::On { yes } => {
            let count = store.snapshot()?.profiles.len();
            let message = format!("启用后，MCP 可访问当前 {count} 个连接档案及此后新增的档案。");
            if !confirm(&message, *yes)? {
                return Ok(());
            }
            store.update_mcp_access(|mut current| {
                current.enabled = true;
                Ok(current)
            })?;
        }
        AccessAction::Off => {
            store.set_mcp_access(McpAccessPolicy {
                enabled: false,
                read_only: true,
            })?;
        }
        AccessAction::ReadOnly {
            action: ReadOnlyAction::On,
        } => {
            store.update_mcp_access(|mut current| {
                current.read_only = true;
                Ok(current)
            })?;
        }
        AccessAction::ReadOnly {
            action: ReadOnlyAction::Off { yes },
        } => {
            let snapshot = store.snapshot()?;
            require_enabled(snapshot.mcp)?;
            let count = snapshot.profiles.len();
            let message = format!(
                "关闭只读保护将影响当前 {count} 个连接档案及此后新增的档案。已授权的 Agent 可执行受控插入、更新和删除，仍受数据库权限和写入限额约束。"
            );
            if !confirm(&message, *yes)? {
                return Ok(());
            }
            store.update_mcp_access(|mut current| {
                require_enabled(current)?;
                current.read_only = false;
                Ok(current)
            })?;
        }
        AccessAction::Status
        | AccessAction::ReadOnly {
            action: ReadOnlyAction::Status,
        } => {}
    }

    let snapshot = store.snapshot()?;
    writeln!(
        io::stdout().lock(),
        "enabled={}\nread_only={}\nprofiles={}\nrevision={}",
        snapshot.mcp.enabled,
        snapshot.mcp.read_only,
        snapshot.profiles.len(),
        snapshot.revision
    )
    .map_err(Error::from)
}

fn require_enabled(policy: McpAccessPolicy) -> Result<()> {
    if policy.enabled {
        return Ok(());
    }
    Err(Error::failed_precondition(
        ErrorCode::MCP_CONNECTION_DENIED,
        "MCP 数据访问全局未开启",
    ))
}

fn confirm(message: &str, yes: bool) -> Result<bool> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "{message}").map_err(Error::from)?;
    if yes {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        return Err(Error::invalid_argument(
            ErrorCode::PROFILE_INVALID,
            "非交互终端必须通过 --yes 确认",
        ));
    }

    write!(stderr, "继续？[y/N] ").map_err(Error::from)?;
    stderr.flush().map_err(Error::from)?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(Error::from)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
