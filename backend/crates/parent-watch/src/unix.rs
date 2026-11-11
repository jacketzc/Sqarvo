//! Unix 实现：`PR_SET_PDEATHSIG`（仅 Linux）+ 低频 `getppid()` 轮询。
//!
//! 两个机制互为兜底，缺一不可：
//!
//! 1. `PR_SET_PDEATHSIG`：父进程一退出，内核立刻给后端送 `SIGTERM`。它只对**设置之后**
//!    的父进程退出生效，并且没有平台可移植性（macOS 没有）；
//! 2. 低频轮询 `getppid()`：父进程若在设置之前就已消失（或平台没有 `PDEATHSIG`），
//!    只有轮询能发现。轮询间隔决定「父进程消失 → 开始清理」的最大延迟。

use std::time::Duration;

use crate::{Absent, Decision, Observation, decide};

/// 轮询间隔：父进程消失到后端开始清理最多 500 ms（验收要求 5 秒内退出）。
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 已安装的 Unix 父进程监视。
#[derive(Debug)]
pub struct Watch {
    parent_pid: u32,
    own_pid: u32,
}

impl Watch {
    /// 被监视的父进程 pid。
    #[must_use]
    pub fn parent_pid(&self) -> u32 {
        self.parent_pid
    }

    /// 等待父进程消失，返回它的 pid。
    pub async fn gone(self) -> u32 {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            // 只要不再是「父进程就是它」就停止监视：被 init/subreaper 接管、ppid 变化、
            // 或平台读不到 ppid，都属于「不能再依赖它活着」。
            if decide(Some(self.parent_pid), self.own_pid, probe()) != Decision::Alive {
                return self.parent_pid;
            }
        }
    }
}

/// 安装监视：**先**设置 `PDEATHSIG`，**再**检查父进程是否还在。
///
/// 顺序反过来会留下窗口期：父进程在这段时间退出时，`SIGTERM` 走默认处置（立刻终止），
/// 来不及清理。
pub(crate) fn monitor(parent_pid: u32) -> Result<Watch, Absent> {
    enable_pdeathsig();

    match probe() {
        Observation::Ppid(ppid) if ppid != parent_pid => Err(Absent),
        // 读不到 ppid 不擅自退出：继续监视，轮询里会再判（旧行为保持一致）。
        _ => Ok(Watch {
            parent_pid,
            own_pid: std::process::id(),
        }),
    }
}

/// 当前父进程 pid；Unix 上总是可读，不需要 Windows 那种句柄。
fn probe() -> Observation {
    Observation::Ppid(std::os::unix::process::parent_id())
}

/// 让内核在父进程退出时给本进程发 `SIGTERM`。
///
/// 失败只告警：轮询仍然有效，监视不会因此失效（例如非 Linux 平台没有 `prctl`）。
fn enable_pdeathsig() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl(PR_SET_PDEATHSIG, SIGTERM) 只影响调用线程的父进程死亡信号，
        // 参数是两个整数常量，不解引用指针、不涉及内存所有权。
        #[allow(unsafe_code)]
        let result = unsafe {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGTERM as libc::c_ulong,
                0,
                0,
                0,
            )
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            tracing::warn!(%error, "设置 PR_SET_PDEATHSIG 失败，退回父进程轮询");
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        tracing::debug!("此平台没有 PR_SET_PDEATHSIG，只靠父进程轮询兜底");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_interval_matches_documented_budget() {
        // 引用文档里的 500 ms 口径：改这里等于改「5 秒内退出」的验收依据。
        assert_eq!(POLL_INTERVAL, Duration::from_millis(500));
    }

    #[test]
    fn probe_reads_the_real_parent() {
        // 单测进程的父进程（cargo test 的 runner）必然存在，ppid 不可能是 0。
        match probe() {
            Observation::Ppid(ppid) => assert_ne!(ppid, 0),
            other => panic!("Unix 上应当总能读到 ppid，实际 {other:?}"),
        }
    }
}
