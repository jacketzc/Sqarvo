//! Windows Credential Manager 凭据存储（`CredWriteW` / `CredReadW` / `CredDeleteW`）。
//!
//! # 条目形态
//!
//! - 类型：`CRED_TYPE_GENERIC`（通用凭据，用户在「凭据管理器 → Windows 凭据 →
//!   普通凭据」里可见、可删除）；
//! - target name：凭据引用原文（`sqarvo:conn/<uuid-v4>`，ADR 0012 的平台映射）；
//! - `UserName`：固定 `Sqarvo`，不放数据库用户名（凭据列表对用户可见，不能变成
//!   连接拓扑的第二份副本）；
//! - `CredentialBlob`：密码的 UTF-8 字节（不是 UTF-16：blob 是任意字节序列，
//!   用 UTF-8 与 Linux Secret Service 的 `text/plain` 语义一致，读回时逐字节还原）；
//! - 持久化：`CRED_PERSIST_LOCAL_MACHINE`（重启后仍在，符合「保存密码」的预期）。
//!
//! # 与 Linux 实现的差异
//!
//! - 没有「服务未启动」这一状态：Credential Manager 是 advapi32 里的 API，不是守护
//!   进程。因此 [`WindowsCredentialManagerStore::open`] 只做一次轻量探测
//!   （读一个不可能存在的 target，`ERROR_NOT_FOUND` 即健康），失败时由
//!   [`open_credential_store`](super::open_credential_store) 退回
//!   [`UnavailableCredentialStore`](super::UnavailableCredentialStore)；
//! - 没有「解锁」交互：凭据随当前用户的登录会话解密，`ERROR_NO_SUCH_LOGON_SESSION`
//!   表示进程没有可用登录会话（例如以服务账户运行），按 `Unavailable` 上报；
//! - 单条上限 2560 字节（`CRED_MAX_CREDENTIAL_BLOB_SIZE`），超过直接拒绝而不是截断。
//!
//! # 为什么这里允许 unsafe
//!
//! 整个实现就是三个 Win32 调用加一个结构体装配，没有安全封装可复用。仓库默认
//! `unsafe_code = "deny"`，此处按 AGENTS.md §11 局部放宽并逐个 `unsafe` 块标注
//! `SAFETY`：指针都指向调用期间有效的缓冲区，`CredReadW` 分配的结构由 `Drop`
//! 守卫 `CredFree`，密码字节在写入后立即在进程内擦除。

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::fmt;
use std::ptr::null_mut;

use sqarvo_common::Result;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};

use super::windows_spec::{
    CREDENTIAL_USERNAME, CredentialAction, classify_credential_error, decode_blob, encode_blob,
    is_not_found, target_name,
};
use super::{CredentialStore, Secret, validate_profile};

/// 探测用的 target name。
///
/// 用 `sqarvo:probe/` 前缀：`validate_reference` 只接受 `sqarvo:conn/<uuid>`，因此
/// 它永远不可能和真实凭据引用撞名，也不会被写进任何配置。
const PROBE_TARGET: &str = "sqarvo:probe/credential-store";

/// Windows Credential Manager 凭据存储。
///
/// 无内部可变状态：每次操作都是独立的系统调用，天然可跨线程共享（`Send + Sync`）。
pub struct WindowsCredentialManagerStore {
    profile: String,
}

impl fmt::Debug for WindowsCredentialManagerStore {
    /// 只输出实现名与档案名：本结构不持有任何 secret，也不打印引用。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsCredentialManagerStore")
            .field("implementation", &self.implementation())
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl WindowsCredentialManagerStore {
    /// 打开凭据存储并做一次可用性探测。
    ///
    /// 失败原因按 `credentials` 模块文档的错误表分类，不会 panic；调用方通常是
    /// [`open_credential_store`](super::open_credential_store)，失败时退回
    /// [`UnavailableCredentialStore`](super::UnavailableCredentialStore)。
    pub fn open(profile: &str) -> Result<Self> {
        validate_profile(profile)?;
        let store = Self {
            profile: profile.to_owned(),
        };
        probe()?;
        Ok(store)
    }
}

/// 轻量探测：读一个不可能存在的 target。
///
/// `ERROR_NOT_FOUND` 说明「API 可用、只是没有这条凭据」——这正是健康状态。
/// 其它错误（无登录会话、被策略禁止）在启动阶段就暴露，而不是等用户点
/// 「保存密码」时才失败。
fn probe() -> Result<()> {
    let target = wide(PROBE_TARGET);
    let mut raw: *mut CREDENTIALW = null_mut();
    // SAFETY: `target` 以 NUL 结尾且在本调用期间有效；`raw` 是栈上输出变量，
    // 成功时系统分配结构体，交给 `Credential` 守卫 `CredFree`。
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut raw) };
    if ok != 0 {
        // 探测名被外部工具占用：能读说明 API 正常，读完释放即可，不解析内容。
        let _guard = Credential(raw);
        return Ok(());
    }
    let code = last_error();
    if is_not_found(code) {
        Ok(())
    } else {
        Err(classify_credential_error(CredentialAction::Read, code))
    }
}

impl CredentialStore for WindowsCredentialManagerStore {
    fn get(&self, reference: &str) -> Result<Secret> {
        let target = wide(target_name(reference)?);
        let mut raw: *mut CREDENTIALW = null_mut();
        // SAFETY: `target` 以 NUL 结尾且在调用期间有效；`raw` 由系统分配，
        // 立刻交给 `Credential` 守卫，避免任何提前返回路径漏掉 `CredFree`。
        let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut raw) };
        if ok == 0 {
            return Err(classify_credential_error(
                CredentialAction::Read,
                last_error(),
            ));
        }
        let guard = Credential(raw);
        // SAFETY: `ok != 0` 保证 `guard.0` 指向系统写满的 `CREDENTIALW`；
        // 结构体归守卫持有，下面只借用不释放。
        let credential = unsafe { &*guard.0 };
        // `CredReadW` 按 `(target, type)` 精确匹配，正常不会拿到别的类型；
        // 真拿到说明凭据库被外部工具改过（存量内容损坏）。
        if credential.Type != CRED_TYPE_GENERIC {
            return Err(super::windows_spec::wrong_type_error());
        }
        let bytes: &[u8] =
            if credential.CredentialBlobSize == 0 || credential.CredentialBlob.is_null() {
                &[]
            } else {
                // SAFETY: `CredentialBlob` 指向 `CredentialBlobSize` 个字节，生命周期
                // 由 `guard`（以及它持有的系统缓冲区）覆盖；这里只读。
                unsafe {
                    std::slice::from_raw_parts(
                        credential.CredentialBlob,
                        credential.CredentialBlobSize as usize,
                    )
                }
            };
        decode_blob(bytes)
    }

    fn set(&self, reference: &str, secret: &Secret) -> Result<()> {
        let target = target_name(reference)?;
        // 长度上限在这里检查：超过 2560 字节时 `CredWriteW` 只会返回
        // `ERROR_INVALID_PARAMETER`，用户看不懂；必须提前给出明确原因。
        let mut blob = encode_blob(secret)?;
        let blob_size = u32::try_from(blob.len()).expect("encode_blob 已保证长度不超过 2560 字节");
        let mut target_wide = wide(target);
        let mut username_wide = wide(CREDENTIAL_USERNAME);

        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target_wide.as_mut_ptr(),
            CredentialBlobSize: blob_size,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: username_wide.as_mut_ptr(),
            // Flags / Comment / LastWritten / Attributes / TargetAlias 保持全零：
            // 「无注释、无属性、无别名」——凭据列表里只显示 target name 与用户名。
            ..CREDENTIALW::default()
        };

        // SAFETY: 结构体里的两个字符串指针指向本栈帧内 NUL 结尾的缓冲区，blob 指针
        // 指向按 `blob_size` 真实分配的 `Vec`，三者在调用期间都有效；`CredWriteW`
        // 只读这些缓冲区，不接管所有权。
        let ok = unsafe { CredWriteW(&raw const credential, 0) };
        let code = last_error();
        // 无论成败都擦掉进程内的明文副本：密码只在系统凭据库里持久化。
        blob.fill(0);
        if ok == 0 {
            return Err(classify_credential_error(CredentialAction::Write, code));
        }
        Ok(())
    }

    fn delete(&self, reference: &str) -> Result<()> {
        let target = wide(target_name(reference)?);
        // SAFETY: `target` 以 NUL 结尾且在调用期间有效；本调用不返回需要释放的
        // 内存，只改系统凭据库。
        let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok == 0 {
            let code = last_error();
            // 幂等：引用不存在就是成功（ADR 0012 的生命周期语义）。
            if is_not_found(code) {
                return Ok(());
            }
            return Err(classify_credential_error(CredentialAction::Delete, code));
        }
        Ok(())
    }

    fn implementation(&self) -> &'static str {
        "windows-credential-manager"
    }
}

/// 把文本转成 NUL 结尾的 UTF-16（Win32 `*W` 接口的入参）。
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: `GetLastError` 无参数、无副作用。
    unsafe { GetLastError() }
}

/// `CredReadW` 分配的 `CREDENTIALW` 守卫：`CredFree` 保证只调用一次。
struct Credential(*mut CREDENTIALW);

impl Drop for Credential {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 指针来自 `CredReadW` 的成功返回，文档要求用 `CredFree` 释放；
            // 守卫保证只释放一次，且释放前不再解引用。
            unsafe {
                CredFree(self.0.cast::<c_void>());
            }
        }
    }
}
