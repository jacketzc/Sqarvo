//! Windows 显式 DACL 的 P/Invoke 实现（只在 `cfg(windows)` 编译）。
//!
//! 调用链：
//!
//! 1. `OpenProcessToken` + `GetTokenInformation(TokenUser)` 取当前进程的用户 SID，
//!    `ConvertSidToStringSidW` 变成文本（`S-1-5-…`）；
//! 2. [`owner_only_sddl`](super::owner_only_sddl) 拼出 `D:P(A[;OICI];FA;;;<SID>)`；
//! 3. `ConvertStringSecurityDescriptorToSecurityDescriptorW(SDDL_REVISION_1)` 解析成
//!    安全描述符，`GetSecurityDescriptorDacl` 取出 DACL；
//! 4. `SetNamedSecurityInfoW(SE_FILE_OBJECT, DACL|PROTECTED_DACL)` 写回：`PROTECTED`
//!    才会清掉从父目录继承来的 ACE（`Users` 等），否则「仅当前用户」只是多了一条
//!    ACE，其他人照样能读 token。
//!
//! 为什么不用 `SetFileSecurityW` 一把梭：它只接受完整安全描述符、且不区分「改 DACL」
//! 与「改属主」，出错时也无法按对象类型区分；`SetNamedSecurityInfoW` 是文档推荐的
//! 现代接口，返回 Win32 错误码而不是只置 `GetLastError`。
//!
//! # 为什么这里允许 unsafe
//!
//! 本模块的全部内容就是 Win32 FFI：`windows-sys` 的函数都是 `unsafe extern`，
//! 没有安全封装可复用。仓库默认 `unsafe_code = "deny"`，此处按 AGENTS.md §11 的
//! 要求**局部**放宽并说明不变量：每个 `unsafe` 块旁边都有 `SAFETY` 注释，指针
//! 生命周期由 `Drop` 守卫（`LocalFree` / `CloseHandle`）保证，所有缓冲区在调用
//! 期间都保持有效且不被并发访问。上层（[`restrict_path`]）是安全函数。

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;

use sqarvo_common::Result;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LocalFree, WIN32_ERROR};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{AclTarget, classify_acl_error, classify_sid_error, owner_only_sddl};

/// Win32 `ERROR_INVALID_PARAMETER`：`GetSecurityDescriptorDacl` 成功但拿不到 DACL 时
/// 用它构造错误，便于和真正的系统错误区分（两者都属于「不该发生」）。
const ERROR_INVALID_PARAMETER: u32 = 87;

fn wide_str(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 路径的 UTF-16 形式，去掉末尾的目录分隔符。
///
/// `SetNamedSecurityInfoW` / `GetNamedSecurityInfoW` 对「带尾分隔符的目录名」在不同
/// Windows 版本上行为不一致（有的返回 `ERROR_INVALID_NAME`），而 `PathBuf` 会原样
/// 保留调用方传入的 `C:\dir\`。这里统一去掉尾部 `\` / `/`，但保留驱动器根 `C:\`
/// 与 UNC 根（长度不足 4 个 UTF-16 单元时不再裁剪）。
fn wide_path(path: &Path) -> Vec<u16> {
    // 走 `OsStr::encode_wide` 而不是 `to_str()`：Windows 路径不保证是 UTF-8，
    // 用 `to_str()` 会在含非 UTF-8 段的目录上失败，那等于「无法收紧 → 放弃」。
    let mut wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    while wide.len() > 4 {
        let separator = wide[wide.len() - 2];
        if separator != u16::from(b'\\') && separator != u16::from(b'/') {
            break;
        }
        wide.remove(wide.len() - 2);
    }
    wide
}

/// 收紧单个文件或目录的 DACL：仅当前用户完全控制，且断开继承。
///
/// 失败返回可分类错误（`PermissionDenied` / `NotFound` / `Internal`），**不会**
/// 静默跳过：调用方（端点文件写入、目录创建）会把错误继续上抛，后端因此拒绝启动。
pub(crate) fn restrict_path(path: &Path, target: AclTarget) -> Result<()> {
    let sid = current_user_sid()?;
    let sddl = owner_only_sddl(&sid, target)?;
    let wide_sddl = wide_str(&sddl);

    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `wide_sddl` 在本次调用期间一直有效且以 NUL 结尾；`descriptor` 由系统
    // 分配，成功后立即交给 `SecurityDescriptor` 守卫负责 `LocalFree`。第四个参数
    // 允许为 NULL（表示不关心 SD 大小）。
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(classify_acl_error(target, path, last_error()));
    }
    let _descriptor = SecurityDescriptor(descriptor);

    let mut dacl_present: i32 = 0;
    let mut dacl: *mut ACL = null_mut();
    let mut dacl_defaulted: i32 = 0;
    // SAFETY: `descriptor` 是上一步由系统写出的有效安全描述符（由守卫持有）；
    // 三个输出参数都指向本栈帧内的局部变量，结构体布局与 Win32 定义一致。
    let got_dacl = unsafe {
        GetSecurityDescriptorDacl(
            descriptor,
            &raw mut dacl_present,
            &raw mut dacl,
            &raw mut dacl_defaulted,
        )
    };
    // `present == 0` 或 DACL 指针为空说明 SDDL 没被正确解析（例如系统实现差异），
    // 这时绝不能当成「已经收紧了」继续写 token。
    if got_dacl == 0 || dacl_present == 0 || dacl.is_null() {
        return Err(classify_acl_error(
            target,
            path,
            if got_dacl == 0 {
                last_error()
            } else {
                ERROR_INVALID_PARAMETER
            },
        ));
    }

    let wide_target = wide_path(path);
    // SAFETY: `wide_target` 是 NUL 结尾的 UTF-16 路径；`dacl` 指向 `_descriptor` 内部，
    // 调用期间 `_descriptor` 未被释放；owner/group/sacl 传 NULL 表示「不改」，
    // 因此不需要 `SE_RESTORE_NAME` 等特权。
    let code: WIN32_ERROR = unsafe {
        SetNamedSecurityInfoW(
            wide_target.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    if code != 0 {
        return Err(classify_acl_error(target, path, code));
    }
    Ok(())
}

/// 当前进程 token 里的用户 SID 文本（`S-1-5-…`）。
///
/// 每次调用都重新读：结果只在本进程内用于拼 SDDL，不做缓存（少一处「缓存污染就
/// 把凭据/端点交给错误主体」的可能性，代价是一次本地系统调用）。
pub(crate) fn current_user_sid() -> Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `GetCurrentProcess` 返回伪句柄（不需要关闭），`token` 是本栈帧内的
    // 输出变量，`OpenProcessToken` 成功后由 `Token` 守卫 `CloseHandle`。
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
    if opened == 0 {
        return Err(classify_sid_error(last_error()));
    }
    let _token = Token(token);

    // 第一次调用只问长度：`TOKEN_USER` 的大小取决于 SID 长度，固定缓冲区会截断。
    let mut needed: u32 = 0;
    // SAFETY: 缓冲区指针为 NULL、长度为 0 是文档允许的「询问所需长度」用法；
    // 此时函数返回失败并写入 `ERROR_INSUFFICIENT_BUFFER`，这是预期行为，不检查返回值。
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &raw mut needed);
    }
    if needed == 0 {
        return Err(classify_sid_error(last_error()));
    }

    // `TOKEN_USER` 含指针，对齐要求是 `size_of::<usize>()`；`Vec<u8>` 只有 1 字节
    // 对齐，直接 `cast::<TOKEN_USER>()` 会是未对齐读取。这里按 usize 分配，既保证
    // 对齐又保持「按系统返回的字节数」长度语义。
    let words = (needed as usize).div_ceil(std::mem::size_of::<usize>());
    let mut buffer = vec![0usize; words];
    // SAFETY: `buffer` 按上面返回的字节数（向上取整到 usize）分配，指针对齐满足
    // `TOKEN_USER`；`TOKEN_USER` 是 `#[repr(C)]`，系统按此布局写入，长度仍传
    // `needed` 字节而不是 `words`。
    let filled = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    };
    if filled == 0 {
        return Err(classify_sid_error(last_error()));
    }

    // SAFETY: `filled != 0` 保证系统已写入完整的 `TOKEN_USER`；`buffer` 的生命周期
    // 覆盖整个 `token_user` 借用（`sid_string` 拷贝出文本后不再引用它）。
    let token_user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let mut sid_string: *mut u16 = null_mut();
    // SAFETY: `token_user.User.Sid` 指向 `buffer` 内的有效 SID；成功后系统分配
    // 一块字符串缓冲区，由下面的 `LocalFree` 释放。
    let converted = unsafe { ConvertSidToStringSidW(token_user.User.Sid, &raw mut sid_string) };
    if converted == 0 || sid_string.is_null() {
        return Err(classify_sid_error(last_error()));
    }
    // SAFETY: 上一步成功即返回 NUL 结尾的 UTF-16 字符串，指针有效直至 `LocalFree`。
    let text = unsafe { wide_ptr_to_string(sid_string) };
    // SAFETY: 该指针由 `ConvertSidToStringSidW` 通过 `LocalAlloc` 分配，
    // `LocalFree` 是文档要求的释放方式，且只释放一次。
    unsafe {
        LocalFree(sid_string.cast::<c_void>());
    }
    Ok(text)
}

/// 读取 NUL 结尾的 UTF-16 字符串。
///
/// # Safety
///
/// `ptr` 必须指向一块以 NUL 结尾、且在调用期间保持有效的 UTF-16 缓冲区。
unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
    let mut len = 0usize;
    // SAFETY: 由调用方保证 NUL 结尾，循环在遇到 NUL 时停止。
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` 是上面的 NUL 之前长度，区间在有效缓冲区内。
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16_lossy(slice)
}

fn last_error() -> u32 {
    // SAFETY: `GetLastError` 无参数、无副作用。
    unsafe { GetLastError() }
}

/// `LocalAlloc` 分配的缓冲区守卫：`LocalFree` 保证只调用一次。
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 指针来自 `ConvertStringSecurityDescriptorToSecurityDescriptorW`，
            // 文档要求用 `LocalFree` 释放；守卫保证只释放一次。
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

/// 进程 token 句柄守卫。
struct Token(HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 句柄来自 `OpenProcessToken`，守卫保证只关闭一次；
            // `CloseHandle` 失败（例如句柄已被关闭）不会造成内存安全问题。
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Windows 实机验证用：读回对象的 DACL 并转成 SDDL 文本。
///
/// 只在 `cfg(all(windows, test))` 下编译。它不是生产路径，存在的意义是让
/// 「DACL 真的只有当前用户一条 ACE」这件事在 Windows 上可以被断言，而不是
/// 只看 `SetNamedSecurityInfoW` 返回值。
#[cfg(all(windows, test))]
fn read_dacl_sddl(path: &Path) -> String {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
    };

    let wide = wide_path(path);
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: 路径 NUL 结尾且在本调用期间有效；owner/group/dacl/sacl 的输出参数传
    // NULL 表示不关心；`descriptor` 由系统分配，交给 `SecurityDescriptor` 守卫释放。
    let code = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &raw mut descriptor,
        )
    };
    assert_eq!(code, 0, "GetNamedSecurityInfoW 失败：Win32 错误码 {code}");
    let _guard = SecurityDescriptor(descriptor);

    let mut out: *mut u16 = null_mut();
    let mut len: u32 = 0;
    // SAFETY: `descriptor` 有效（守卫持有）；成功时系统分配 SDDL 字符串，用
    // `LocalFree` 释放，与上面同一条约定。
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &raw mut out,
            &raw mut len,
        )
    };
    assert_ne!(
        ok,
        0,
        "ConvertSecurityDescriptorToStringSecurityDescriptorW 失败：Win32 错误码 {}",
        last_error()
    );
    assert!(!out.is_null(), "SDDL 输出指针不应为空");
    // SAFETY: 上一步成功即返回 NUL 结尾的 UTF-16 字符串。
    let text = unsafe { wide_ptr_to_string(out) };
    // SAFETY: 该指针由系统的 `LocalAlloc` 分配，`LocalFree` 是唯一正确释放方式。
    unsafe {
        LocalFree(out.cast::<c_void>());
    }
    text
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;

    /// 需要的 Windows 实机证据（Linux 上只能编译检查）：
    ///
    /// - 文件与目录都只剩一条 ACE，且 SDDL 带 `P`（protected，断开继承）；
    /// - 目录的 ACE 带 `OICI`，子文件因此继承「仅当前用户」，而不是继承到空 DACL；
    /// - 收紧之后进程自己仍能读回内容（没把自己关在门外）；
    /// - DACL 里没有 `WD`（Everyone）/`AU`（Authenticated Users）/`BU`（Users）等宽泛 ACE。
    #[test]
    fn file_and_directory_dacls_are_owner_only() {
        let root = tempfile::tempdir().unwrap();

        let dir = root.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        restrict_path(&dir, AclTarget::Directory).expect("收紧目录 DACL");
        let dir_sddl = read_dacl_sddl(&dir);
        assert!(dir_sddl.starts_with("D:P"), "目录 DACL: {dir_sddl}");
        assert!(dir_sddl.contains(";OICI;FA;;;"), "目录 DACL: {dir_sddl}");
        assert_eq!(dir_sddl.matches('(').count(), 1, "目录 DACL: {dir_sddl}");

        let file = dir.join("backend-endpoint.json");
        std::fs::write(&file, br#"{"token":"redacted"}"#).unwrap();
        restrict_path(&file, AclTarget::File).expect("收紧文件 DACL");
        let file_sddl = read_dacl_sddl(&file);
        assert!(file_sddl.starts_with("D:P"), "文件 DACL: {file_sddl}");
        assert!(file_sddl.contains(";;FA;;;"), "文件 DACL: {file_sddl}");
        assert_eq!(file_sddl.matches('(').count(), 1, "文件 DACL: {file_sddl}");

        for sddl in [&dir_sddl, &file_sddl] {
            for group in [";WD)", ";AU)", ";BU)", ";BG)", ";SY)"] {
                assert!(!sddl.contains(group), "出现了宽泛 ACE {group}：{sddl}");
            }
        }

        // 收紧之后自己还能读（否则后端写不出端点文件、前端也读不到）。
        assert_eq!(
            std::fs::read(&file).unwrap(),
            br#"{"token":"redacted"}"#,
            "收紧 DACL 后自己仍应可读写"
        );
    }
}
