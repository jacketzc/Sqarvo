//! Windows 实现：`OpenProcess` + 创建时间校验 + `WaitForSingleObject`。
//!
//! Windows 没有 `PR_SET_PDEATHSIG`，也没有可移植的 `getppid()`。这里换一种思路：
//! 拿一个父进程**句柄**，句柄一旦变为 signal 就说明父进程已经退出——不需要轮询，
//! 也不会出现「读不到父进程」导致的无限空转。
//!
//! 判定分三步，缺一不可：
//!
//! 1. `OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION)`：失败即按
//!    父进程已消失处理（fail-safe）。**只判一次**，不重试、不轮询；
//! 2. 创建时间校验：句柄可用后比较父进程与本进程的创建时间。父进程不可能晚于
//!    子进程创建，晚了就说明这个 pid 已被复用（指向陌生进程），判为消失；
//! 3. `WaitForSingleObject(handle, INFINITE)`：交给一个专门的等待线程阻塞，等到
//!    signal 就发信号给 `gone()`。
//!
//! 为什么不用 `tokio::task::spawn_blocking`：dropping a Tokio runtime 会等待阻塞池
//! 里的任务结束，而这里的等待是 `INFINITE`——用阻塞池会让进程在 Ctrl-C 后卡在
//! runtime 析构上。所以用独立的 detached `std::thread`。
//!
//! 收紧权限：`PROCESS_QUERY_LIMITED_INFORMATION` 是最小的「读创建时间」权限，
//! 不申请 `PROCESS_QUERY_INFORMATION` 的全权，也不需要 `PROCESS_VM_READ`。

use tokio::sync::oneshot;

use crate::{Absent, Decision, Observation, decide};

/// 已安装的 Windows 父进程监视。
#[derive(Debug)]
pub struct Watch {
    parent_pid: u32,
    /// 等待线程在父进程退出时发信号；接收端被提前丢弃（调用方已退出）也无妨。
    gone: oneshot::Receiver<()>,
    /// 仅在等待线程启动失败时持有发送端：没有任何线程会发送，`gone()` 因此永不返回，
    /// 监视退化为「只能靠 Ctrl-C / SIGTERM 退出」，而不是误判父进程已消失。
    _idle: Option<oneshot::Sender<()>>,
}

impl Watch {
    /// 被监视的父进程 pid。
    #[must_use]
    pub fn parent_pid(&self) -> u32 {
        self.parent_pid
    }

    /// 等待父进程消失，返回它的 pid。
    pub async fn gone(self) -> u32 {
        let result = self.gone.await;
        if let Err(error) = result {
            // 等待线程异常结束（发送端被丢弃）：按父进程已消失处理，避免后端留下孤儿。
            tracing::warn!(%error, "父进程等待线程提前结束，按父进程已消失处理");
        }
        self.parent_pid
    }
}

/// 安装监视；句柄打不开或 pid 已被复用时返回 [`Absent`]。
pub(crate) fn monitor(parent_pid: u32) -> Result<Watch, Absent> {
    let Some(handle) = ParentHandle::open(parent_pid) else {
        // fail-safe：打不开句柄就当作父进程已消失。这里只判一次——失败后不重试、
        // 不进入任何轮询循环，因此不会反复空转。
        tracing::warn!(parent_pid, "无法打开父进程句柄，按父进程已消失处理");
        return Err(Absent);
    };

    // pid 复用检查放在开始等待之前：复用时会等一个完全无关的进程，等于永远等不到。
    let observation = if handle.looks_reused() {
        Observation::PidReused
    } else {
        Observation::Unknown
    };
    if decide(Some(parent_pid), std::process::id(), observation) == Decision::Gone {
        tracing::warn!(
            parent_pid,
            "父进程 pid 已被其他进程复用（创建时间晚于本进程），按父进程已消失处理"
        );
        return Err(Absent);
    }

    let (sender, gone) = oneshot::channel();
    match std::thread::Builder::new()
        .name("sqarvo-parent-watch".to_owned())
        .spawn(move || {
            match handle.wait() {
                WaitOutcome::Signalled => tracing::info!(parent_pid, "父进程已退出"),
                WaitOutcome::Failed(code) => tracing::warn!(
                    parent_pid,
                    wait_result = code,
                    "等待父进程句柄失败，按父进程已消失处理"
                ),
            }
            // 句柄随闭包结束由 `ParentHandle::drop` 关闭。
            let _ = sender.send(());
        }) {
        // detached：等待线程的生命周期跟进程一致，退出时由内核回收。
        Ok(_thread) => Ok(Watch {
            parent_pid,
            gone,
            _idle: None,
        }),
        Err(error) => {
            tracing::error!(
                %error,
                parent_pid,
                "无法启动父进程等待线程，父进程监视失效（后端仍可通过 Ctrl-C 退出）"
            );
            // 线程没起来：换一条永不发送的通道，`gone()` 因此一直等待，而不是把
            // 「监视没装上」误读成「父进程已消失」。
            let (idle, never) = oneshot::channel();
            Ok(Watch {
                parent_pid,
                gone: never,
                _idle: Some(idle),
            })
        }
    }
}

/// `WaitForSingleObject` 的两种结论。
enum WaitOutcome {
    /// 句柄变为 signal：进程已退出。
    Signalled,
    /// 等待失败（`WAIT_FAILED` 等）：按「已消失」处理，避免无限等待。
    Failed(u32),
}

/// 父进程句柄，`Drop` 时关闭。
struct ParentHandle {
    handle: ffi::Handle,
    /// 打开句柄时记录下来的父进程创建时间（FILETIME ticks）。
    ///
    /// 记录它的唯一目的是防 pid 复用：进程退出后 pid 可以被系统分给别的进程，
    /// 只认 pid 会把陌生进程当成父进程、等一个永远不结束的句柄。
    created: Option<u64>,
}

// SAFETY: HANDLE 只是本进程句柄表里的一个令牌，句柄表本身是进程级、线程安全的；
// `WaitForSingleObject` 允许在任意线程等待同一个句柄。这里把句柄所有权（连同 `Drop`
// 里的 `CloseHandle`）整体移交给等待线程，同一时刻只有一个线程持有它。
#[allow(unsafe_code)]
unsafe impl Send for ParentHandle {}

impl ParentHandle {
    /// 以最小权限打开父进程；失败返回 `None`（调用方按「已消失」处理）。
    fn open(parent_pid: u32) -> Option<Self> {
        // SAFETY: OpenProcess 只接受值参数（权限位、是否继承、pid），不涉及本进程内存；
        // 成功时返回由本结构独占的句柄。
        #[allow(unsafe_code)]
        let handle = unsafe {
            ffi::OpenProcess(
                ffi::SYNCHRONIZE | ffi::PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                parent_pid,
            )
        };
        (!handle.is_null()).then_some(Self {
            handle,
            // 打开即记录，之后不再重读：句柄一直持有，进程对象不会消失。
            created: creation_time(handle),
        })
    }

    /// 句柄指向的进程创建时间是否晚于本进程——即 pid 已被复用。
    ///
    /// 读不到创建时间时不做复用判断（宁可继续等句柄，也不凭猜测结束一个正常后端）；
    /// 真正打不开句柄的情况在 [`ParentHandle::open`] 里已经按消失处理。
    fn looks_reused(&self) -> bool {
        let (Some(parent), Some(own)) = (self.created, creation_time(own_handle())) else {
            tracing::debug!("读不到进程创建时间，跳过 pid 复用校验");
            return false;
        };
        parent > own
    }

    /// 阻塞等待父进程退出。
    fn wait(self) -> WaitOutcome {
        // SAFETY: 句柄由本结构独占且尚未关闭；INFINITE 表示一直等，不需要超时参数。
        #[allow(unsafe_code)]
        let result = unsafe { ffi::WaitForSingleObject(self.handle, ffi::INFINITE) };
        if result == ffi::WAIT_OBJECT_0 {
            WaitOutcome::Signalled
        } else {
            WaitOutcome::Failed(result)
        }
    }
}

impl Drop for ParentHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄由 OpenProcess 返回、只被本结构持有，Drop 只会执行一次，
        // 因此不会重复关闭；CloseHandle 失败也没有可恢复的动作。
        #[allow(unsafe_code)]
        unsafe {
            ffi::CloseHandle(self.handle);
        }
    }
}

/// 进程创建时间（FILETIME ticks）；失败返回 `None`。
fn creation_time(handle: ffi::Handle) -> Option<u64> {
    let mut creation = ffi::FileTime::default();
    let mut exit = ffi::FileTime::default();
    let mut kernel = ffi::FileTime::default();
    let mut user = ffi::FileTime::default();
    // SAFETY: handle 有效且未关闭；四个输出指针指向本栈帧上的可写 FileTime，
    // GetProcessTimes 只写这四个结构，不保留指针。
    #[allow(unsafe_code)]
    let ok = unsafe {
        ffi::GetProcessTimes(
            handle,
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    };
    (ok != 0).then_some(creation.ticks())
}

/// 当前进程的伪句柄（常量 `-1`），只用于读取自己的创建时间。
fn own_handle() -> ffi::Handle {
    // SAFETY: GetCurrentProcess 是无参数、无副作用的查询，返回伪句柄，
    // 伪句柄不需要（也不能）CloseHandle。
    #[allow(unsafe_code)]
    unsafe {
        ffi::GetCurrentProcess()
    }
}

/// 只声明需要的五个 kernel32 函数。
///
/// 为这几项引入 `windows-sys` 不值得（AGENTS.md §11：新增依赖需评估构建足迹与供应链）；
/// 这里的签名取自 Win32 文档，且 `cargo check --target x86_64-pc-windows-msvc` 会校验
/// 调用点与声明一致。声明里只用值类型与 `*mut`，不涉及 Rust 内存布局假设。
#[allow(non_snake_case)]
// SAFETY: 本模块只声明 Win32 函数，不实现任何逻辑；FFI 声明必须写在 `unsafe extern`
// 块里（edition 2024 要求）。所有调用点都在本文件内逐个标注 `SAFETY`。
#[allow(unsafe_code)]
mod ffi {
    use core::ffi::c_void;

    /// Win32 `HANDLE`。
    pub(crate) type Handle = *mut c_void;
    /// Win32 `BOOL`。
    pub(crate) type Bool = i32;

    /// 等待句柄变为 signal 所需权限。
    pub(crate) const SYNCHRONIZE: u32 = 0x0010_0000;
    /// 读取进程创建时间的最小权限（不需要 `PROCESS_QUERY_INFORMATION` 全权）。
    pub(crate) const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    /// `WaitForSingleObject` 的无限等待。
    pub(crate) const INFINITE: u32 = 0xFFFF_FFFF;
    /// `WaitForSingleObject` 返回「句柄已 signal」。
    pub(crate) const WAIT_OBJECT_0: u32 = 0;

    /// Win32 `FILETIME`：自 1601-01-01 起的 100 ns 计数，高低位分开存放。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub(crate) struct FileTime {
        /// 低 32 位。
        pub(crate) low: u32,
        /// 高 32 位。
        pub(crate) high: u32,
    }

    impl FileTime {
        /// 合成 64 位计数，便于比较先后。
        pub(crate) fn ticks(self) -> u64 {
            (u64::from(self.high) << 32) | u64::from(self.low)
        }
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// 打开进程句柄。
        pub(crate) fn OpenProcess(access: u32, inherit: Bool, pid: u32) -> Handle;
        /// 读取进程的创建/退出/内核/用户时间。
        pub(crate) fn GetProcessTimes(
            process: Handle,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> Bool;
        /// 等待句柄变为 signal 或超时。
        pub(crate) fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
        /// 关闭句柄。
        pub(crate) fn CloseHandle(handle: Handle) -> Bool;
        /// 当前进程的伪句柄。
        pub(crate) fn GetCurrentProcess() -> Handle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_combines_high_and_low_words() {
        let time = ffi::FileTime {
            low: 0x0000_0001,
            high: 0x0000_0002,
        };
        assert_eq!(time.ticks(), 0x0000_0002_0000_0001);
    }

    #[test]
    fn own_creation_time_is_readable() {
        // 自己一定存在且可读：拿不到说明权限位或声明写错了。
        assert!(creation_time(own_handle()).is_some());
    }

    #[test]
    fn watch_of_current_process_is_not_reused() {
        // 当前进程的创建时间不可能晚于自己，复用校验必须为 false。
        let handle = ParentHandle::open(std::process::id()).expect("应能打开自己的句柄");
        assert!(!handle.looks_reused());
    }

    #[test]
    fn missing_pid_is_treated_as_absent() {
        // 一个不存在的 pid：OpenProcess 失败，按父进程已消失处理（只判一次）。
        assert!(ParentHandle::open(u32::MAX).is_none());
    }
}
