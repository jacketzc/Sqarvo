//! 父进程监视：前端异常结束时，后端不留下孤儿。
//!
//! 两个独立机制，互为兜底：
//!
//! 1. Linux 的 `PR_SET_PDEATHSIG`：父进程一退出，内核立刻给后端送 `SIGTERM`；
//! 2. 低频轮询（每 500 ms 看一次 `getppid()`）：PDEATHSIG 只在**设置之后**父进程
//!    退出时触发；父进程若在设置之前就已消失（或平台不支持 PDEATHSIG），只有轮询能发现。
//!
//! # 边界：只在前端显式传入 `--parent-pid` 时启用
//!
//! 手工启动的后端（调试、多个前端共用一个后端、systemd 托管）父进程可能是 shell
//! 或 init，无条件设置 PDEATHSIG 会在 shell 退出时被误杀。因此 [`decide`] 的第一条
//! 规则是「没有 `--parent-pid` 就什么都不做」，并有单测覆盖；`--parent-pid` 也
//! 不出现在 `--help` 里，它是前后端之间的内部约定，不是产品开关。

use std::time::Duration;

use sqarvo_common::{Error, ErrorCode};

/// 轮询间隔：父进程消失到后端开始清理最多 500 ms（验收要求 5 秒内退出）。
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 一次判定的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 没有 `--parent-pid`：不监视（手工启动的后端走这条路）。
    NotManaged,
    /// 父进程仍是前端请求里的那个进程。
    Alive,
    /// 父进程已消失（被重建到 init 或其他 subreaper）。
    Gone,
    /// `--parent-pid` 取值无效（0 或后端自己的 pid）。
    Invalid,
}

/// 判定是否应监视父进程，以及父进程当前是否还在。
///
/// `current_ppid` 为 `None` 表示平台拿不到父 pid（Windows）：无法判定时按
/// `Alive` 处理，宁可让 PDEATHSIG 兜底，也不因为「读不到」就擅自退出。
#[must_use]
pub fn decide(expected: Option<u32>, current_ppid: Option<u32>, own_pid: u32) -> Decision {
    let Some(expected) = expected else {
        return Decision::NotManaged;
    };
    if expected == 0 || expected == own_pid {
        return Decision::Invalid;
    }
    match current_ppid {
        Some(ppid) if ppid == expected => Decision::Alive,
        Some(_) => Decision::Gone,
        None => Decision::Alive,
    }
}

/// 安装结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    /// 没有 `--parent-pid`：不监视父进程。
    Standalone,
    /// 已监视：PDEATHSIG（Linux）与轮询都已就位。
    Managed(u32),
    /// 监视生效前就发现父进程已消失，应直接清理退出。
    ParentGone(u32),
}

/// 安装父进程监视（在创建目录、绑定 socket、写端点文件之前调用）。
///
/// 顺序有意为之：**先**设置 PDEATHSIG，**再**检查 `getppid()`。这样父进程恰好在
/// 两者之间退出时，要么由内核送 `SIGTERM`，要么被 `getppid()` 看出来，不会漏。
pub fn install(expected: Option<u32>) -> Result<Installed, Error> {
    let Some(pid) = expected else {
        tracing::debug!("未指定 --parent-pid：不监视父进程（手工启动或共用后端）");
        return Ok(Installed::Standalone);
    };

    let own_pid = std::process::id();
    if decide(Some(pid), current_parent_pid(), own_pid) == Decision::Invalid {
        return Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            format!("--parent-pid {pid} 无效：必须是启动本进程的父进程 pid"),
        ));
    }

    enable_pdeathsig();

    if decide(Some(pid), current_parent_pid(), own_pid) == Decision::Gone {
        tracing::warn!(parent_pid = pid, "父进程在监视生效前已退出，不再启动服务");
        Ok(Installed::ParentGone(pid))
    } else {
        tracing::info!(
            parent_pid = pid,
            pdeathsig = cfg!(target_os = "linux"),
            poll_millis = POLL_INTERVAL.as_millis(),
            "已监视前端父进程：父进程消失时后端自行清理退出"
        );
        Ok(Installed::Managed(pid))
    }
}

/// 等待父进程消失；未监视时永不返回。
///
/// 返回消失的父 pid，调用方据此记录日志并走与 `SIGTERM` 相同的清理路径。
pub async fn wait_until_gone(installed: Installed) -> u32 {
    match installed {
        Installed::Managed(pid) => loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            if decide(Some(pid), current_parent_pid(), std::process::id()) != Decision::Alive {
                return pid;
            }
        },
        Installed::Standalone | Installed::ParentGone(_) => std::future::pending::<u32>().await,
    }
}

/// 让内核在父进程退出时给本进程发 `SIGTERM`。
///
/// 失败只告警：轮询仍然有效，监视不会因此失效（例如非 Linux 平台没有 prctl）。
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
    #[cfg(not(target_os = "linux"))]
    {
        tracing::debug!("此平台没有 PR_SET_PDEATHSIG，只靠父进程轮询兜底");
    }
}

/// 当前父进程 pid；平台不支持时返回 `None`。
// `Option` 是为非 unix 平台（读不到 ppid）保留的：那里返回 None，unix 上永远是 Some，
// 所以这里必须允许「返回值多余地包了一层」的告警。
#[allow(clippy::unnecessary_wraps)]
fn current_parent_pid() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(std::os::unix::process::parent_id())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u32 = 4242;

    #[test]
    fn no_flag_never_monitors() {
        // 手工启动的后端：父进程可能是 shell / init，必须完全不监视。
        assert_eq!(decide(None, Some(1), OWN), Decision::NotManaged);
        assert_eq!(decide(None, Some(900), OWN), Decision::NotManaged);
    }

    #[test]
    fn alive_when_parent_matches() {
        assert_eq!(decide(Some(900), Some(900), OWN), Decision::Alive);
    }

    #[test]
    fn gone_when_reparented_to_init() {
        assert_eq!(decide(Some(900), Some(1), OWN), Decision::Gone);
        // 父进程被 subreaper 接管：ppid 换了但不是 1，同样算消失。
        assert_eq!(decide(Some(900), Some(777), OWN), Decision::Gone);
    }

    #[test]
    fn invalid_pids_are_rejected() {
        assert_eq!(decide(Some(0), Some(1), OWN), Decision::Invalid);
        assert_eq!(decide(Some(OWN), Some(1), OWN), Decision::Invalid);
    }

    #[test]
    fn unknown_platform_ppid_keeps_running() {
        // 拿不到 ppid 时不擅自退出。
        assert_eq!(decide(Some(900), None, OWN), Decision::Alive);
    }

    #[test]
    fn install_without_flag_is_standalone() {
        assert_eq!(
            install(None).expect("无 --parent-pid 不应失败"),
            Installed::Standalone
        );
    }

    #[test]
    fn install_rejects_own_pid() {
        let error = install(Some(std::process::id())).expect_err("传自己的 pid 应被拒绝");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    }

    #[test]
    fn install_detects_vanished_parent() {
        // 用一个刚退出的子进程 pid 当「父进程」：ppid 不可能是它，应判定为已消失。
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("启动临时子进程");
        let dead_pid = child.id();
        let _ = child.wait();
        match install(Some(dead_pid)) {
            Ok(Installed::ParentGone(pid)) => assert_eq!(pid, dead_pid),
            Ok(other) => panic!("父进程已退出却判成 {other:?}"),
            Err(error) => panic!("安装监视不应报错：{error}"),
        }
    }
}
