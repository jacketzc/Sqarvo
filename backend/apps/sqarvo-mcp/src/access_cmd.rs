//! 本地终端专用的共享档案授权命令。

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use clap::Subcommand;
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_local_store::{AppPaths, McpAccessPolicy, ProfileStore, profiles::migration};

#[derive(Subcommand)]
pub enum AccessAction {
    /// Allow MCP to access shared connection profiles.
    On {
        /// Confirm authorization when running without an interactive terminal.
        #[arg(long)]
        yes: bool,
    },
    /// Revoke MCP access and restore read-only mode.
    Off,
    /// Show both policy flags, the profile count, and the document revision.
    Status,
    /// Manage the global MCP read-only policy.
    ReadOnly {
        #[command(subcommand)]
        action: ReadOnlyAction,
    },
}

#[derive(Subcommand)]
pub enum ReadOnlyAction {
    /// Turn on read-only mode.
    On,
    /// Turn off read-only mode.
    Off {
        /// Confirm turning off read-only mode without an interactive terminal.
        #[arg(long)]
        yes: bool,
    },
    /// Show the current authorization state.
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
            let message = format!(
                "Enabling MCP grants access to the current {count} connection profiles and any profiles added later."
            );
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
                "Turning off read-only protection affects the current {count} connection profiles and any profiles added later. Authorized agents can perform controlled inserts, updates, and deletes within database permissions and write limits."
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
        "Global MCP data access is disabled",
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
            "Use --yes to confirm when running without an interactive terminal",
        ));
    }

    write!(stderr, "Continue? [y/N] ").map_err(Error::from)?;
    stderr.flush().map_err(Error::from)?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(Error::from)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
