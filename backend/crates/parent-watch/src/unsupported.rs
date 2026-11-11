//! 既不是 Unix 也不是 Windows 的平台：没有可靠的父进程判活手段。
//!
//! 这里不假装能监视：`gone()` 永不返回，后端只能靠 Ctrl-C 之类的信号退出。
//! 保留这个模块是为了让 `lib.rs` 的公共接口在所有目标上都能编译，而不是用
//! `cfg` 把调用方也一起切碎。

use crate::Absent;

/// 占位监视器：不持有任何状态，也永远不会报告「父进程已消失」。
#[derive(Debug)]
pub struct Watch {
    parent_pid: u32,
}

impl Watch {
    /// 被监视的父进程 pid（仅用于日志）。
    #[must_use]
    pub fn parent_pid(&self) -> u32 {
        self.parent_pid
    }

    /// 永不返回：此平台无法判定父进程是否还在。
    pub async fn gone(self) -> u32 {
        tracing::debug!("此平台缺少父进程判活手段，父进程监视不会触发");
        std::future::pending().await
    }
}

/// 安装监视：直接成功，但 `gone()` 永不返回。
pub(crate) fn monitor(parent_pid: u32) -> Result<Watch, Absent> {
    Ok(Watch { parent_pid })
}
