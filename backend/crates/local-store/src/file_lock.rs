//! 同一文件上跨进程共享或独占加锁。

use std::fs::File;
use std::io;

/// 文件锁模式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockMode {
    /// 允许其他进程同时读。
    Shared,
    /// 独占文件，排斥其他读写锁。
    Exclusive,
}

/// 尝试加锁，遇到锁争用立即返回 `Ok(false)`。
///
/// 使用同步方式打开的文件句柄。获得的锁随文件关闭释放；每次获取应使用独立打开的句柄。
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn try_lock(file: &File, mode: LockMode) -> io::Result<bool> {
    use std::os::fd::AsRawFd;

    let operation = match mode {
        LockMode::Shared => libc::LOCK_SH,
        LockMode::Exclusive => libc::LOCK_EX,
    };
    // SAFETY: 借用期间 fd 一直有效；flock 不接管 fd，也不访问调用方内存。
    let result = unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(false),
        _ => Err(error),
    }
}

/// 尝试对同步文件句柄加锁，遇到竞争立即返回 `Ok(false)`。
///
/// 锁定远离实际内容的保留字节（即使文件为空），锁随文件关闭释放。
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn try_lock(file: &File, mode: LockMode) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let flags = match mode {
        LockMode::Shared => LOCKFILE_FAIL_IMMEDIATELY,
        LockMode::Exclusive => LOCKFILE_FAIL_IMMEDIATELY | LOCKFILE_EXCLUSIVE_LOCK,
    };
    let mut overlapped = OVERLAPPED::default();
    // Windows 的字节锁也约束常规 I/O，保留字节必须避开审计追加写入的数据。
    overlapped.Anonymous.Anonymous.Offset = 0xffff_fffe;
    overlapped.Anonymous.Anonymous.OffsetHigh = 0x7fff_ffff;
    // SAFETY: 文件通过同步 I/O 打开，FAIL_IMMEDIATELY 使锁操作在调用期间结束。
    // 借用期间句柄有效，结构体指针可写且不会被保留。
    let result = unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut overlapped) };
    if result != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        return Ok(false);
    }
    Err(error)
}

/// 未支持文件锁的目标安全失败。
#[cfg(not(any(unix, windows)))]
pub fn try_lock(_file: &File, _mode: LockMode) -> io::Result<bool> {
    Err(io::ErrorKind::Unsupported.into())
}
