//! 父进程监视：前端异常结束时，后端不留下孤儿。
//!
//! 后端只在前端显式传入 `--parent-pid <pid>` 时监视父进程；监视生效后，父进程消失
//! 就产生一个**可等待的信号**，由调用方（`main.rs`）走与 Ctrl-C / `SIGTERM` 完全
//! 相同的清理路径。本 crate 只负责「判活」与「给信号」，不负责关闭会话或删除文件。
//!
//! # 三平台语义
//!
//! | 平台 | 机制 | 判定 |
//! |---|---|---|
//! | Linux | `PR_SET_PDEATHSIG`（内核送 `SIGTERM`）+ 500 ms `getppid()` 轮询 | 双保险，互为兜底 |
//! | 其它 Unix | 500 ms `getppid()` 轮询 | `ppid` 变了即消失 |
//! | Windows | `OpenProcess(SYNCHRONIZE \| PROCESS_QUERY_LIMITED_INFORMATION)` + 创建时间校验 + `WaitForSingleObject` | 句柄可等待；打不开即按已消失（只判一次） |
//!
//! Linux 上两个机制缺一不可：`PR_SET_PDEATHSIG` 只在**设置之后**父进程退出时才触发，
//! 父进程若在设置之前就已消失，只有轮询能发现。
//!
//! Windows 没有 `PR_SET_PDEATHSIG`，也没有可移植的 `getppid()`；旧实现（读不到 `ppid`
//! 就按「父进程活着」处理）会让 `wait_until_gone` 永远不返回。现在改成等一个内核句柄：
//! 父进程退出时句柄变为 signal，不需要轮询，也不会因为「读不到」而空转。
//!
//! # 边界：没有 `--parent-pid` 就什么都不做
//!
//! 手工启动的后端（调试、多个前端共用一个后端、systemd 托管）父进程可能是 shell 或
//! init，无条件监视会在 shell 退出时被误杀。因此 [`decide`] 的第一条规则是
//! 「没有 `--parent-pid` 就不监视」，并且有单测覆盖。
//!
//! # 不误判 pid 复用
//!
//! 进程退出后 pid 可以被系统复用。只比较 pid 会把「复用了同一 pid 的陌生进程」当成
//! 父进程（后者在 Windows 上的后果是永远等下去）。Windows 实现因此在打开句柄后比较
//! 创建时间：父进程的创建时间**晚于**本进程时，这个 pid 已被复用，判定为消失。

#![warn(missing_docs)]

use std::fmt;

#[cfg(unix)]
mod unix;
#[cfg(not(any(unix, windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::Watch;
#[cfg(not(any(unix, windows)))]
pub use unsupported::Watch;
#[cfg(windows)]
pub use windows::Watch;

/// 一次判定的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 没有 `--parent-pid`：不监视（手工启动的后端走这条路）。
    NotManaged,
    /// 父进程仍是前端请求里的那个进程。
    Alive,
    /// 父进程已消失（被重建到 init/subreaper，或 pid 已被复用）。
    Gone,
    /// `--parent-pid` 取值无效（0 或后端自己的 pid）。
    Invalid,
}

/// 平台探针给出的观测结果。
///
/// Unix `getppid()` 与 Windows 的句柄/创建时间最终都翻译成这里的四种取值，
/// 判定规则集中在 [`decide`]，便于在没有 Windows 机器的环境里做单测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// Unix：`getppid()` 读到的父 pid。
    Ppid(u32),
    /// 父进程已不存在（Windows 句柄打不开或等待返回）。
    Exited,
    /// pid 已被复用：句柄指向的进程创建时间晚于本进程，不是父进程。
    PidReused,
    /// 平台读不到可靠答案：保守按活着处理，等其它机制兜底。
    Unknown,
}

/// 判定是否应监视父进程，以及父进程当前是否还在。
///
/// `expected` 是命令行给出的 `--parent-pid`；`own_pid` 用于拒绝「把自己当父进程」
/// 这种明确错误的取值（会立刻被自己的存活骗过去）。
#[must_use]
pub fn decide(expected: Option<u32>, own_pid: u32, observation: Observation) -> Decision {
    let Some(expected) = expected else {
        return Decision::NotManaged;
    };
    if expected == 0 || expected == own_pid {
        return Decision::Invalid;
    }
    match observation {
        Observation::Ppid(ppid) if ppid == expected => Decision::Alive,
        Observation::Ppid(_) | Observation::Exited | Observation::PidReused => Decision::Gone,
        // 「读不到」不等于「已退出」：擅自退出会在权限受限、探针故障时误杀正常后端。
        Observation::Unknown => Decision::Alive,
    }
}

/// `--parent-pid` 取值非法（0 或后端自己的 pid）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPid {
    /// 命令行传入的取值。
    pub pid: u32,
}

impl fmt::Display for InvalidPid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "--parent-pid {} 无效：必须是启动本进程的父进程 pid",
            self.pid
        )
    }
}

impl std::error::Error for InvalidPid {}

/// 安装结果。
#[derive(Debug)]
pub enum Installed {
    /// 没有 `--parent-pid`：不监视父进程。
    Standalone,
    /// 已监视：等待父进程消失的句柄/轮询已就位。
    Managed(Watch),
    /// 监视生效前就发现父进程已消失，应直接清理退出。
    ParentGone(u32),
}

/// 平台侧判定「父进程已经不在」的信号；不需要携带细节，调用方只看 `Err`。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Absent;

/// 安装父进程监视（在创建目录、绑定 socket、写端点文件之前调用）。
///
/// 顺序有意为之：**先**装监视（Linux 上是 `PR_SET_PDEATHSIG`），**再**检查一次父进程
/// 是否还在。这样父进程恰好在两者之间退出时，要么由内核送 `SIGTERM`，要么被这次检查
/// 看出来，不会漏。
pub fn install(expected: Option<u32>) -> Result<Installed, InvalidPid> {
    let Some(parent_pid) = expected else {
        tracing::debug!("未指定 --parent-pid：不监视父进程（手工启动或共用后端）");
        return Ok(Installed::Standalone);
    };

    let own_pid = std::process::id();
    if decide(Some(parent_pid), own_pid, Observation::Unknown) == Decision::Invalid {
        return Err(InvalidPid { pid: parent_pid });
    }

    if let Ok(watch) = monitor(parent_pid) {
        tracing::info!(
            parent_pid,
            mechanism = mechanism(),
            "已监视前端父进程：父进程消失时后端自行清理退出"
        );
        Ok(Installed::Managed(watch))
    } else {
        tracing::warn!(parent_pid, "父进程在监视生效前已退出，不再启动服务");
        Ok(Installed::ParentGone(parent_pid))
    }
}

/// 等待父进程消失；未监视时永不返回。
///
/// 返回消失的父 pid，调用方据此记录日志并走与 `SIGTERM` 相同的清理路径。
pub async fn wait_until_gone(installed: Installed) -> u32 {
    match installed {
        Installed::Managed(watch) => watch.gone().await,
        Installed::Standalone | Installed::ParentGone(_) => std::future::pending::<u32>().await,
    }
}

/// 各平台的监视机制说明，只用于启动日志与排查。
fn mechanism() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "PR_SET_PDEATHSIG + 500ms getppid 轮询"
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        "500ms getppid 轮询"
    }
    #[cfg(windows)]
    {
        "OpenProcess(SYNCHRONIZE) + WaitForSingleObject"
    }
    #[cfg(not(any(unix, windows)))]
    {
        "无（此平台缺少父进程判活手段）"
    }
}

/// 安装平台监视器；父进程已消失（含 pid 复用）时返回 [`Absent`]。
fn monitor(parent_pid: u32) -> Result<Watch, Absent> {
    #[cfg(unix)]
    {
        unix::monitor(parent_pid)
    }
    #[cfg(windows)]
    {
        windows::monitor(parent_pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        unsupported::monitor(parent_pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u32 = 4242;
    const PARENT: u32 = 900;

    #[test]
    fn no_flag_never_monitors() {
        // 手工启动的后端：父进程可能是 shell / init，必须完全不监视。
        assert_eq!(
            decide(None, OWN, Observation::Ppid(1)),
            Decision::NotManaged
        );
        assert_eq!(
            decide(None, OWN, Observation::Ppid(900)),
            Decision::NotManaged
        );
        assert_eq!(decide(None, OWN, Observation::Exited), Decision::NotManaged);
        assert_eq!(
            decide(None, OWN, Observation::Unknown),
            Decision::NotManaged
        );
    }

    #[test]
    fn alive_when_parent_matches() {
        assert_eq!(
            decide(Some(PARENT), OWN, Observation::Ppid(PARENT)),
            Decision::Alive
        );
    }

    #[test]
    fn gone_when_reparented_to_init() {
        assert_eq!(
            decide(Some(PARENT), OWN, Observation::Ppid(1)),
            Decision::Gone
        );
        // 父进程被 subreaper 接管：ppid 换了但不是 1，同样算消失。
        assert_eq!(
            decide(Some(PARENT), OWN, Observation::Ppid(777)),
            Decision::Gone
        );
    }

    #[test]
    fn gone_when_wait_handle_reports_exit() {
        // Windows：句柄打不开或等待返回，都翻译成 Exited。
        assert_eq!(
            decide(Some(PARENT), OWN, Observation::Exited),
            Decision::Gone
        );
    }

    #[test]
    fn gone_when_pid_was_reused() {
        // pid 复用：句柄指向的进程创建时间晚于本进程，不是父进程。
        assert_eq!(
            decide(Some(PARENT), OWN, Observation::PidReused),
            Decision::Gone
        );
    }

    #[test]
    fn invalid_pids_are_rejected() {
        assert_eq!(
            decide(Some(0), OWN, Observation::Ppid(1)),
            Decision::Invalid
        );
        assert_eq!(
            decide(Some(OWN), OWN, Observation::Ppid(1)),
            Decision::Invalid
        );
        // 非法取值优先于观测结果：即使「看起来活着」也必须拒绝。
        assert_eq!(
            decide(Some(OWN), OWN, Observation::Unknown),
            Decision::Invalid
        );
    }

    #[test]
    fn unknown_observation_keeps_running() {
        // 拿不到可靠答案时不擅自退出。
        assert_eq!(
            decide(Some(PARENT), OWN, Observation::Unknown),
            Decision::Alive
        );
    }

    #[test]
    fn install_without_flag_is_standalone() {
        assert!(matches!(
            install(None).expect("无 --parent-pid 不应失败"),
            Installed::Standalone
        ));
    }

    #[test]
    fn install_rejects_own_pid() {
        let own = std::process::id();
        let error = install(Some(own)).expect_err("传自己的 pid 应被拒绝");
        assert_eq!(error.pid, own);
        assert!(error.to_string().contains("--parent-pid"));
    }

    #[test]
    fn install_rejects_zero_pid() {
        let error = install(Some(0)).expect_err("pid 0 应被拒绝");
        assert_eq!(error.pid, 0);
    }

    #[cfg(unix)]
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
