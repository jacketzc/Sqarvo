//! 凭据引用与操作系统凭据存储抽象。
//!
//! # 平台映射
//!
//! | 平台 | 后端 | 状态 |
//! | --- | --- | --- |
//! | Linux | Secret Service（`org.freedesktop.secrets`，D-Bus） | 已实现，见 [`SecretServiceCredentialStore`] |
//! | Windows | Credential Manager（`CredWriteW` / `CredReadW` / `CredDeleteW`，`CRED_TYPE_GENERIC`，`CRED_PERSIST_LOCAL_MACHINE`） | 已实现，见 [`WindowsCredentialManagerStore`] |
//! | macOS | Keychain | 未实现，装配为 [`UnavailableCredentialStore`] |
//!
//! 平台侧命名（ADR 0012）：
//!
//! | 平台 | 条目键 | 额外字段 |
//! | --- | --- | --- |
//! | Linux | 属性 `application` / `profile` / `reference`，label 固定 `Sqarvo` | `text/plain` 明文秘密 |
//! | Windows | target name = 引用原文（`sqarvo:conn/<uuid-v4>`） | `UserName` 固定 `Sqarvo`，blob 是密码的 UTF-8 字节 |
//!
//! Windows 的 target name **不拼档案名**：引用在连接配置里必须唯一确定一条凭据，
//! 拼 `profile:reference` 会引入一个隐式命名空间；当前只有 `default` 档案，非默认
//! 档案在 Windows 上不产生独立命名空间（这是与 Linux 属性检索的唯一差异）。
//!
//! 「当前平台尚未实现」与「运行环境里没有可用的凭据服务」对调用方是同一种结果：
//! 所有操作返回 `credential.unavailable`，上层必须如实展示给用户，不得伪造成功。
//!
//! # Windows 实现：本机（Linux）只做了编译检查
//!
//! `windows.rs` 的 P/Invoke 路径在 Linux 上**无法执行**，因此下列行为只经过
//! `cargo check -p sqarvo-local-store --target x86_64-pc-windows-msvc` 的类型检查
//! 与纯逻辑单测（`windows_spec.rs`），必须在 Windows 实机上复核后才算验证完成：
//!
//! 1. `CredWriteW` / `CredReadW` / `CredDeleteW` 的真实往返（写入 → 读回 → 覆盖写
//!    → 删除 → 幂等删除），含中文密码（UTF-8 逐字节还原），以及零长度密码
//!    （`CredentialBlobSize = 0` 时 `CredWriteW` 是否接受需实测；gRPC 层本就拒绝空
//!    密码，见 `grpc_credential` 用例）；
//! 2. 凭据管理器 UI 里的条目形状：`普通凭据`、target name 为引用原文、用户名
//!    `Sqarvo`、无注释/别名；
//! 3. 重启进程后仍能读回（`CRED_PERSIST_LOCAL_MACHINE` 的实际持久化范围：本机
//!    所有用户共享的机器级存储，仍按当前用户隔离）；
//! 4. 超过 2560 字节的密码被明确拒绝（`FailedPrecondition`），且凭据库里没有残留；
//! 5. 非 UTF-8 内容与类型不匹配的存量条目被识别为 `FailedPrecondition`；
//! 6. 以无登录会话的账户（如服务账户）运行时的 `ERROR_NO_SUCH_LOGON_SESSION`
//!    分支确实走 `Unavailable`，并让 `open_credential_store` 退回「不可用」实现；
//! 7. `CredFree` / `CloseHandle` 的释放路径没有句柄或内存泄漏（凭据管理器里反复
//!    增删后进程句柄数稳定）。
//!
//! # 缺失时明确报错，绝不回退明文
//!
//! 本模块只处理「引用」与操作系统凭据库，永远不写配置文件。凭据服务缺失、被锁定或
//! 用户拒绝时不会把密码退化保存到配置文件、环境变量或日志，也不会提示用户「把密码写进
//! 配置即可」——那会直接违反 AGENTS.md §9 与 ADR 0012。
//!
//! 错误文案中不出现密码；引用按 ADR 0012 只含随机 UUID，即使出现在日志里也不暴露
//! 主机、用户名或库名。
//!
//! # 同步 trait 与 D-Bus 的桥接
//!
//! [`CredentialStore`] 的方法是同步签名。这是刻意的：三个平台的系统凭据 API
//! （Windows `CredWrite`、macOS `SecItem*`、Linux D-Bus）本身就是阻塞调用；用
//! `async` trait 会把三个平台都绑到某个运行时上，并逼每个实现自己发明取消语义。
//!
//! 同步方法的代价是调用方必须保证不占用异步 worker：
//!
//! - Linux 实现用 `zbus::blocking::Connection`。zbus 的 blocking 层用
//!   `async_io::block_on` 在**当前线程**上驱动自己的 socket 任务，从不调用
//!   `tokio::runtime::Handle::block_on`，因此不会出现 tokio 的
//!   「Cannot start a runtime from within a runtime」，也不要求 tokio 运行时存在；
//! - 但它确实会阻塞调用线程（本地 D-Bus 往返通常毫秒级；密钥环已锁或等用户确认时更久）。
//!   所以**从 async 上下文（后端 gRPC 服务）调用时必须包在 `tokio::task::spawn_blocking` 里**，
//!   与 AGENTS.md §6.4「阻塞实现使用阻塞任务池」一致；
//! - 备选方案「异步 zbus + 专用线程 + 通道回包」被否决：调用方同样要同步等待回包
//!   （要么阻塞线程，要么把整个上层改成 async），却多出一层线程和一套消息协议，
//!   还多出「谁负责超时」的额外状态。
//!
//! # 错误语义（ADR 0012）
//!
//! | 情形 | 错误码 | 分类 | Windows 对应（Win32 错误码） |
//! | --- | --- | --- | --- |
//! | 引用不存在（配置还在、凭据已被删除） | `credential.not_found` | `NotFound` | `ERROR_NOT_FOUND`（1168） |
//! | 引用格式不合法 | `config.invalid` | `InvalidArgument` | 同上：写库前由 `validate_reference` 拒绝 |
//! | 无会话总线 / 服务缺失 / 通信失败 / 密钥环已锁定 | `credential.unavailable` | `Unavailable` | `ERROR_NO_SUCH_LOGON_SESSION`（1312），即没有可用登录会话 |
//! | D-Bus 权限被拒 | `credential.unavailable` | `PermissionDenied` | `ERROR_ACCESS_DENIED`（5）/ `ERROR_PRIVILEGE_NOT_HELD`（1314） |
//! | 需要用户在桌面会话确认（解锁提示）/ 存量凭据内容损坏 | `credential.unavailable` | `FailedPrecondition` | blob 不是 UTF-8 / 条目类型不是通用凭据 / 超过 2560 字节 |
//! | 未预期的 Secret Service 错误 | `credential.unavailable` | `Internal` | 其它 Win32 错误码（错误码原文写进文案，便于排查） |

use std::sync::Arc;

use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use uuid::{Uuid, Version};

pub mod memory;
#[cfg(target_os = "linux")]
pub mod secret_service;
/// Windows Credential Manager 实现（P/Invoke，仅 Windows 编译）。
#[cfg(windows)]
pub mod windows;
/// Windows Credential Manager 实现的纯逻辑（编解码、target 命名、错误分类）。
///
/// 在 Linux 上随 `cargo test` 一起编译，安全语义因此不必等 Windows 实机才能断言。
#[cfg(any(windows, test))]
pub(crate) mod windows_spec;

pub use memory::MemoryCredentialStore;
#[cfg(target_os = "linux")]
pub use secret_service::SecretServiceCredentialStore;
#[cfg(windows)]
pub use windows::WindowsCredentialManagerStore;

pub use sqarvo_driver_api::Secret;

/// Secret Service 属性 `application` 的取值，与应用 ID 一致（ADR 0012）。
pub const APPLICATION_ID: &str = "io.sqarvo.Sqarvo";

/// 默认用户档案名，写入 Secret Service 属性 `profile`。
pub const DEFAULT_PROFILE: &str = "default";

/// 凭据引用前缀（ADR 0012）。
pub const REFERENCE_PREFIX: &str = "sqarvo:conn/";

/// 系统凭据库条目的 label。
///
/// label 是用户在密钥环管理界面里唯一能读到的描述，因此必须固定：任何拼接
/// （主机名、用户名、库名、引用）都会让密钥环列表变成连接拓扑的第二份副本。
/// ADR 0012 允许 `Sqarvo <驱动显示名>`，但 [`CredentialStore::set`] 只拿到引用、
/// 拿不到驱动显示名；按 #21 的口径固定为 `Sqarvo`。
pub const ITEM_LABEL: &str = "Sqarvo";

/// 操作系统凭据存储抽象。
///
/// 实现必须是 `Send + Sync`（会被装配进 `Arc` 跨任务共享）。方法为同步阻塞调用，
/// 调用约定见[模块文档](self)。
pub trait CredentialStore: Send + Sync {
    /// 按引用读取凭据。
    ///
    /// 引用不存在返回 `credential.not_found`；存储不可用返回 `credential.unavailable`。
    fn get(&self, reference: &str) -> Result<Secret>;

    /// 写入或覆盖凭据。
    ///
    /// 同一引用重复写入覆盖旧值，不做版本化（ADR 0012）。
    fn set(&self, reference: &str, secret: &Secret) -> Result<()>;

    /// 删除凭据。引用不存在视为成功（幂等）。
    fn delete(&self, reference: &str) -> Result<()>;

    /// 实现名，用于启动日志与诊断。
    ///
    /// 必须是固定常量（如 `linux-secret-service`），不能拼接引用、主机或用户名：
    /// 它会进日志，而日志里不允许出现连接拓扑或凭据信息。
    fn implementation(&self) -> &'static str;
}

/// 校验凭据引用格式（ADR 0012）。
///
/// 合法格式是 `sqarvo:conn/<小写带连字符的 UUID v4>`。不合法时返回
/// `config.invalid`：引用会被写进连接配置，格式错了越早失败越好，否则要等到读取凭据
/// 时才暴露。
///
/// 错误文案不回显引用原文——引用本来就不该含主机或用户名，但错误文案会进日志，
/// 不能在「用户（或旧版本）恰好把敏感信息编进去」时再抄一遍。
pub fn validate_reference(reference: &str) -> Result<()> {
    let Some(rest) = reference.strip_prefix(REFERENCE_PREFIX) else {
        return Err(invalid_reference());
    };
    let parsed = Uuid::parse_str(rest).map_err(|_| invalid_reference())?;
    // `parse_str` 还接受大写、无连字符等形式；ADR 固定规范写法，避免同一凭据出现多个键。
    if parsed.get_version() != Some(Version::Random) || parsed.hyphenated().to_string() != rest {
        return Err(invalid_reference());
    }
    Ok(())
}

fn invalid_reference() -> Error {
    Error::invalid_argument(
        ErrorCode::CONFIG_INVALID,
        "凭据引用不合法：必须形如 sqarvo:conn/<uuid-v4>",
    )
}

/// 引用不存在时使用的统一错误。
pub(crate) fn not_found_error() -> Error {
    Error::not_found(
        ErrorCode::CREDENTIAL_NOT_FOUND,
        "系统凭据库中不存在该凭据引用（密码可能已被用户删除）",
    )
}

/// 档案名最大长度：只是防止把路径或连接串误当档案名写进系统凭据库。
pub(crate) const MAX_PROFILE_LEN: usize = 64;

/// 校验用户档案名。
///
/// 三个平台的实现共用同一条规则（不能为空、不超过 64 字节、不含控制字符），
/// 因为档案名会进系统凭据库的属性/键名，格式错了越早失败越好。
pub(crate) fn validate_profile(profile: &str) -> Result<()> {
    let valid = !profile.is_empty()
        && profile.len() <= MAX_PROFILE_LEN
        && !profile.chars().any(char::is_control);
    if valid {
        Ok(())
    } else {
        Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "用户档案名不合法：不能为空、不能超过 64 字节、不能包含控制字符",
        ))
    }
}

/// 占位实现：如实报告「不可用」。
///
/// 用于尚未实现凭据存储的平台，以及运行环境里找不到凭据服务时由
/// [`open_credential_store`] 装配。它不保存任何东西，也不会退回明文。
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableCredentialStore;

impl CredentialStore for UnavailableCredentialStore {
    fn get(&self, _reference: &str) -> Result<Secret> {
        Err(unavailable_store_error("无法读取数据库凭据"))
    }

    fn set(&self, _reference: &str, _secret: &Secret) -> Result<()> {
        Err(unavailable_store_error("无法保存数据库凭据"))
    }

    fn delete(&self, _reference: &str) -> Result<()> {
        Err(unavailable_store_error("无法删除数据库凭据"))
    }

    fn implementation(&self) -> &'static str {
        "unavailable"
    }
}

fn unavailable_store_error(action: &str) -> Error {
    Error::new(
        ErrorCode::CREDENTIAL_UNAVAILABLE,
        ErrorCategory::Unavailable,
        format!("当前环境没有可用的系统凭据存储，{action}"),
    )
}

/// 按运行时能力选择凭据存储实现（默认用户档案）。
///
/// 永不 panic、永不返回明文实现：Linux 上尝试连接 Secret Service，失败则退回
/// [`UnavailableCredentialStore`]，并用 `tracing` 记录一次告警（只含错误码与安全文案）。
pub fn open_default_credential_store() -> Arc<dyn CredentialStore> {
    open_credential_store(DEFAULT_PROFILE)
}

/// 按运行时能力选择凭据存储实现，并指定用户档案名。
///
/// 返回值总是可用对象；不可用时是 [`UnavailableCredentialStore`]，调用方会在真正的
/// 凭据操作上收到 `credential.unavailable`。
pub fn open_credential_store(profile: &str) -> Arc<dyn CredentialStore> {
    #[cfg(target_os = "linux")]
    match SecretServiceCredentialStore::connect(profile) {
        Ok(store) => return Arc::new(store),
        Err(error) => tracing::warn!(
            code = %error.code(),
            category = %error.category(),
            "系统凭据存储不可用，凭据操作将如实报错：{error}"
        ),
    }

    // Windows：Credential Manager 是 advapi32 里的 API，没有「服务未启动」状态；
    // 构造时只做一次 `CredReadW` 探测（见 WindowsCredentialManagerStore::open）。
    // 探测失败（例如进程没有登录会话）同样退回「不可用」，绝不回退明文。
    #[cfg(windows)]
    match WindowsCredentialManagerStore::open(profile) {
        Ok(store) => return Arc::new(store),
        Err(error) => tracing::warn!(
            code = %error.code(),
            category = %error.category(),
            "Windows 凭据管理器不可用，凭据操作将如实报错：{error}"
        ),
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = profile;
        tracing::warn!("当前平台尚未实现系统凭据存储，凭据操作将如实报错");
    }

    Arc::new(UnavailableCredentialStore)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn unavailable_store_reports_unavailable() {
        let store = UnavailableCredentialStore;
        let error = store
            .get("sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4")
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CREDENTIAL_UNAVAILABLE);
        // ADR 0012：`credential.unavailable` 保持 UNAVAILABLE 语义，
        // 不能因为「没实现」就退化成 UNIMPLEMENTED，前端要能提示「检查密钥环」。
        assert_eq!(error.category(), ErrorCategory::Unavailable);
    }

    #[test]
    fn valid_reference_is_accepted() {
        validate_reference("sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4").unwrap();
    }

    /// 三个平台的实现共用同一条档案名规则（Windows 实现在 Linux 上也会跑到这里）。
    #[test]
    fn profile_name_is_validated_for_every_platform() {
        assert!(validate_profile(DEFAULT_PROFILE).is_ok());
        assert!(validate_profile("work").is_ok());
        let too_long = "x".repeat(MAX_PROFILE_LEN + 1);
        for profile in ["", "with\ttab", "with\nnewline", too_long.as_str()] {
            let error = validate_profile(profile).unwrap_err();
            assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
            assert_eq!(error.category(), ErrorCategory::InvalidArgument);
        }
        assert!(validate_profile(&"x".repeat(MAX_PROFILE_LEN)).is_ok());
    }

    #[test]
    fn invalid_references_are_rejected() {
        for reference in [
            "",
            "conn-1",
            "sqarvo:conn/",
            "sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4/extra",
            // 把连接拓扑编进引用的旧想法：必须拒绝
            "sqarvo:conn/db.example.com:3306/root",
            // v1 UUID：不是随机引用
            "sqarvo:conn/2c5ea4c0-4067-11e9-8bad-9b1deb4d3b7d",
            // 大写：不是 ADR 规定的规范写法
            "sqarvo:conn/3F9A1C4E-7B2D-4A6F-9C11-0D2B8E5A71F4",
            // 无连字符
            "sqarvo:conn/3f9a1c4e7b2d4a6f9c110d2b8e5a71f4",
        ] {
            let error = validate_reference(reference).unwrap_err();
            assert_eq!(error.code(), ErrorCode::CONFIG_INVALID, "{reference}");
            assert_eq!(error.category(), ErrorCategory::InvalidArgument);
            // 错误文案是固定串（只解释格式），不回显输入：输入里可能有人塞了主机、
            // 用户名甚至密码。
            assert_eq!(
                error.to_string(),
                "凭据引用不合法：必须形如 sqarvo:conn/<uuid-v4> [config.invalid]"
            );
        }
    }

    #[test]
    fn invalid_reference_error_does_not_echo_input() {
        let reference = "sqarvo:conn/user@db.example.com:hunter2";
        let error = validate_reference(reference).unwrap_err();
        assert!(!error.to_string().contains("hunter2"));
        assert!(!format!("{error:?}").contains("hunter2"));
    }
}
