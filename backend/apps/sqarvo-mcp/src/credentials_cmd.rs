//! 凭据管理子命令（#207 实现；V3.1 §3.2）。
//!
//! 契约：
//! - `set`：缺省自动生成合法 `sqarvo:conn/<uuid v4>` 引用并打印；密码经 rpassword
//!   交互读取（不回显、禁 argv/env 传密、回读确认）；
//! - `delete`：幂等删除；
//! - 两者都走 `sqarvo-local-store` 的 `CredentialStore`（同步 trait，必须
//!   `spawn_blocking`）；密钥环不可用如实报 `credential.unavailable`，永不明文回退
//!   （ADR 0012）；
//! - 管理模式不向 stdout 输出 MCP 协议消息。

use clap::Subcommand;
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};

/// `credentials` 子命令的动作。CLI 类型放包里，供 lib 与 bin 共用。
#[derive(Subcommand)]
pub enum CredentialsAction {
    /// 写入密钥环凭据；不传 reference 时自动生成并打印。
    Set {
        /// 凭据引用；必须形如 `sqarvo:conn/<uuid v4>`，缺省自动生成。
        reference: Option<String>,
    },
    /// 删除密钥环凭据（不存在视为成功，幂等）。
    Delete {
        /// 待删除的凭据引用原文。
        reference: String,
    },
}

/// 执行管理子命令。
#[allow(clippy::unused_async)] // 骨架占位：真实实现在 #207 落地。
pub async fn run(_action: CredentialsAction) -> Result<()> {
    Err(Error::new(
        ErrorCode::INTERNAL,
        ErrorCategory::Internal,
        "sqarvo-mcp 骨架：credentials 子命令实现待 #207 落地",
    ))
}
