//! Windows Credential Manager 的纯逻辑部分：secret 编解码、target 命名、错误分类。
//!
//! 与 Linux 的 `secret_service.rs` 一样，这一层只负责「语义」：把一个
//! [`Secret`] 变成 Credential Manager 的 blob 字节、把一个 Win32 错误码变成
//! [可分类错误](sqarvo_common::ErrorCategory)。真正的 `CredWriteW` / `CredReadW` /
//! `CredDeleteW` 调用在 `windows.rs`（仅 `cfg(windows)` 编译）。
//!
//! 这样拆分的原因很实际：错误语义与容量上限是**契约**（ADR 0012 的错误表、前端
//! 分支），必须在 Linux CI 上就能被断言；而 P/Invoke 只能在 Windows 上跑。

use std::fmt;

use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};

use super::{Secret, not_found_error, validate_reference};

/// Credential Manager 单条通用凭据的 blob 上限（Win32
/// `CRED_MAX_CREDENTIAL_BLOB_SIZE = 5 * 512`）。
///
/// 超过这个长度 `CredWriteW` 会直接失败（`ERROR_INVALID_PARAMETER`），因此必须
/// 在调用前**显式拒绝**并如实报错：截断会把错误密码交给驱动，表现为莫名其妙的
/// 认证失败，而用户以为密码已经保存。
pub(crate) const MAX_SECRET_BYTES: usize = 2560;

/// 数据库密码在 Credential Manager 里的 `UserName` 字段取值。
///
/// 固定常量、不放数据库用户名：凭据条目在「控制面板 → 凭据管理器」里对用户可见，
/// 写进用户名就等于把连接拓扑复制到系统凭据库（ADR 0012 的同一理由，见
/// [`ITEM_LABEL`](super::ITEM_LABEL)）。
pub(crate) const CREDENTIAL_USERNAME: &str = "Sqarvo";

/// Win32 `ERROR_ACCESS_DENIED`。
const ERROR_ACCESS_DENIED: u32 = 5;
/// Win32 `ERROR_NOT_FOUND`：目标名不存在。
const ERROR_NOT_FOUND: u32 = 1168;
/// Win32 `ERROR_NO_SUCH_LOGON_SESSION`：没有可用的登录会话（凭据管理器不可达）。
const ERROR_NO_SUCH_LOGON_SESSION: u32 = 1312;
/// Win32 `ERROR_PRIVILEGE_NOT_HELD`。
const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;

/// 出错的操作类型，只用于错误文案与日志标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialAction {
    /// `CredReadW`。
    Read,
    /// `CredWriteW`。
    Write,
    /// `CredDeleteW`。
    Delete,
}

impl CredentialAction {
    /// 面向用户的中文动词。
    const fn verb(self) -> &'static str {
        match self {
            Self::Read => "读取",
            Self::Write => "保存",
            Self::Delete => "删除",
        }
    }

    /// 日志用的稳定短标签（英文，便于 grep）。
    const fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
        }
    }
}

/// 凭据在 Credential Manager 里的 target name。
///
/// ADR 0012 的平台映射：Windows 用**引用本身**作为 target name
/// （`sqarvo:conn/<uuid-v4>`）。不做任何拼接：拼接会引入「同一引用在不同档案下
/// 指向不同凭据」的隐式命名空间，而引用在配置里必须唯一确定一条凭据。
///
/// 先过一遍 [`validate_reference`]：target name 是写入系统凭据库的键，格式错误
/// 要在这里失败（`config.invalid`），而不是让一条拼错的键永久留在凭据管理器里。
pub(crate) fn target_name(reference: &str) -> Result<&str> {
    validate_reference(reference)?;
    Ok(reference)
}

/// 把 secret 编码成 Credential Manager 的 blob（UTF-8 字节）。
///
/// 只做长度检查与字节拷贝，不做任何转换：密码里的非 ASCII 字符（中文、emoji）以
/// UTF-8 原样存储，读回时逐字节还原。
pub(crate) fn encode_blob(secret: &Secret) -> Result<Vec<u8>> {
    let bytes = secret.expose().as_bytes();
    if bytes.len() > MAX_SECRET_BYTES {
        // 文案里只给长度上限，不回显长度与内容：长度本身能缩小猜测范围。
        return Err(Error::new(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::FailedPrecondition,
            format!(
                "Windows 凭据管理器单条凭据上限为 {MAX_SECRET_BYTES} 字节，该密码无法保存（未保存任何内容）"
            ),
        ));
    }
    Ok(bytes.to_vec())
}

/// 把读回的 blob 解成 [`Secret`]。
///
/// 严格的 UTF-8：`from_utf8_lossy` 会把损坏的字节糊成替换字符，然后驱动拿它去
/// 认证并返回一个与真实原因无关的失败。这里必须如实报「内容损坏、请重新保存」。
pub(crate) fn decode_blob(bytes: &[u8]) -> Result<Secret> {
    match std::str::from_utf8(bytes) {
        Ok(value) => Ok(Secret::new(value)),
        Err(_) => Err(Error::new(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::FailedPrecondition,
            "Windows 凭据管理器中的凭据内容不是合法的 UTF-8 文本，请在应用内重新保存该连接的密码",
        )),
    }
}

/// 同名的系统凭据条目不是本应用写入的通用凭据。
///
/// `CredReadW` 按 `(target, CRED_TYPE_GENERIC)` 精确匹配，正常情况下读不到别的
/// 类型；一旦读到的类型不是 `CRED_TYPE_GENERIC`，说明系统凭据库被外部工具改过，
/// 属于「存量内容损坏、需要用户重新保存」。
pub(crate) fn wrong_type_error() -> Error {
    Error::new(
        ErrorCode::CREDENTIAL_UNAVAILABLE,
        ErrorCategory::FailedPrecondition,
        "同名的系统凭据条目不是本应用写入的通用凭据，请在应用内重新保存该连接的密码",
    )
}

/// 判断 Win32 错误码是否是 `ERROR_NOT_FOUND`。
///
/// 单独抽出来是因为它决定两条语义分支：读取时是 `credential.not_found`，删除时是
/// 「已经不存在 → 成功」（幂等）。把判断放在纯逻辑里，两侧行为都有 Linux 单测覆盖。
pub(crate) fn is_not_found(code: u32) -> bool {
    code == ERROR_NOT_FOUND
}

/// 把 Win32 错误码映射成可分类错误（语义见 `credentials` 模块文档的错误表）。
pub(crate) fn classify_credential_error(action: CredentialAction, code: u32) -> Error {
    let fault = WindowsCredentialFault {
        action: action.label(),
        code,
    };
    match code {
        ERROR_NOT_FOUND => not_found_error().with_source(fault),
        ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD => Error::new(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::PermissionDenied,
            format!(
                "没有{} Windows 凭据管理器的权限：请检查当前账户的凭据管理器策略",
                action.verb()
            ),
        )
        .with_source(fault),
        ERROR_NO_SUCH_LOGON_SESSION => Error::new(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::Unavailable,
            "Windows 凭据管理器不可用：当前进程没有可用的登录会话（例如以服务账户运行）",
        )
        .with_source(fault),
        _ => Error::new(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::Internal,
            format!("Windows 凭据管理器返回了未预期的错误（Win32 错误码 {code}）"),
        )
        .with_source(fault),
    }
}

/// 供日志定位的 Win32 故障摘要。
///
/// 只有操作标签与错误码：`CredentialBlob` 从不进入这里，`Debug` 因此不可能带出
/// 密码（ADR 0012 / AGENTS.md §9）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsCredentialFault {
    action: &'static str,
    code: u32,
}

impl fmt::Display for WindowsCredentialFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wincred {} (Win32 {})", self.action, self.code)
    }
}

impl std::error::Error for WindowsCredentialFault {}

#[cfg(test)]
mod tests {
    use super::*;

    const REF: &str = "sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4";

    fn assert_code_and_category(error: &Error, code: ErrorCode, category: ErrorCategory) {
        assert_eq!(error.code(), code);
        assert_eq!(error.category(), category);
    }

    #[test]
    fn target_name_is_the_reference_itself() {
        assert_eq!(target_name(REF).unwrap(), REF);
        // 引用非法时在写进凭据库之前失败，错误码是 config.invalid。
        for bad in ["", "conn-1", "sqarvo:conn/db.example.com:3306"] {
            let error = target_name(bad).unwrap_err();
            assert_eq!(error.code(), ErrorCode::CONFIG_INVALID, "{bad}");
        }
    }

    #[test]
    fn blob_is_utf8_roundtrip() {
        for value in ["", "hunter2", "pässwörd", "密码🔐"] {
            let blob = encode_blob(&Secret::new(value)).unwrap();
            assert_eq!(blob, value.as_bytes());
            assert_eq!(decode_blob(&blob).unwrap().expose(), value);
        }
    }

    #[test]
    fn oversized_secret_is_rejected_without_truncation() {
        let at_limit = "p".repeat(MAX_SECRET_BYTES);
        assert_eq!(
            encode_blob(&Secret::new(at_limit)).unwrap().len(),
            MAX_SECRET_BYTES
        );

        let too_long = "p".repeat(MAX_SECRET_BYTES + 1);
        let error = encode_blob(&Secret::new(too_long)).unwrap_err();
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::FailedPrecondition,
        );
        // 文案只给上限，不给实际长度，也不回显内容。
        assert!(error.to_string().contains(&MAX_SECRET_BYTES.to_string()));
        assert!(
            !error
                .to_string()
                .contains(&(MAX_SECRET_BYTES + 1).to_string())
        );

        // 多字节字符按字节计数：1024 个汉字（3072 字节）同样超限。
        let cjk = "密".repeat(1024);
        assert_eq!(cjk.len(), 1024 * 3);
        assert!(encode_blob(&Secret::new(cjk)).is_err());
    }

    #[test]
    fn non_utf8_blob_is_rejected() {
        let error = decode_blob(&[0xff, 0xfe, 0x00]).unwrap_err();
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::FailedPrecondition,
        );
        // 错误里不能出现替换字符或原始字节。
        assert!(!error.to_string().contains('\u{fffd}'));
    }

    #[test]
    fn missing_credential_is_not_found() {
        let error = classify_credential_error(CredentialAction::Read, 1168);
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_NOT_FOUND,
            ErrorCategory::NotFound,
        );
    }

    #[test]
    fn not_found_code_is_recognised() {
        // 删除路径用它实现幂等；读取路径用它区分「不存在」与「不可用」。
        assert!(is_not_found(1168));
        assert!(!is_not_found(5));
        assert!(!is_not_found(1312));
        assert!(!is_not_found(0));
    }

    #[test]
    fn wrong_type_is_failed_precondition() {
        let error = wrong_type_error();
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::FailedPrecondition,
        );
        assert!(!format!("{error} {error:?}").contains("hunter2"));
    }

    #[test]
    fn access_denied_keeps_credential_code() {
        for code in [5, 1314] {
            let error = classify_credential_error(CredentialAction::Write, code);
            assert_code_and_category(
                &error,
                ErrorCode::CREDENTIAL_UNAVAILABLE,
                ErrorCategory::PermissionDenied,
            );
        }
    }

    #[test]
    fn no_logon_session_is_unavailable() {
        let error = classify_credential_error(CredentialAction::Read, 1312);
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::Unavailable,
        );
        assert!(error.category().is_retryable());
    }

    #[test]
    fn unexpected_codes_are_internal() {
        for code in [0, 8, 87, 2202, 0xffff_ffff] {
            let error = classify_credential_error(CredentialAction::Delete, code);
            assert_code_and_category(
                &error,
                ErrorCode::CREDENTIAL_UNAVAILABLE,
                ErrorCategory::Internal,
            );
        }
    }

    /// 凭据相关的错误链、日志行里都不能出现密码。
    #[test]
    fn errors_never_leak_the_secret() {
        let secret = Secret::new("hunter2");
        let too_long = Secret::new("hunter2".repeat(MAX_SECRET_BYTES));
        let mut pieces = vec![format!("{secret:?}"), format!("{too_long:?}")];
        for action in [
            CredentialAction::Read,
            CredentialAction::Write,
            CredentialAction::Delete,
        ] {
            for code in [5, 1168, 1312, 87] {
                let error = classify_credential_error(action, code);
                pieces.push(format!("{error}"));
                pieces.push(format!("{error:?}"));
                pieces.push(format!("{:?}", std::error::Error::source(&error)));
            }
        }
        pieces.push(format!("{:?}", encode_blob(&too_long).unwrap_err()));
        pieces.push(format!("{:?}", decode_blob(&[0xff]).unwrap_err()));
        let rendered = pieces.join(" ");
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }

    #[test]
    fn fault_display_is_stable() {
        let error = classify_credential_error(CredentialAction::Read, 1312);
        assert_eq!(
            std::error::Error::source(&error)
                .map(ToString::to_string)
                .as_deref(),
            Some("wincred read (Win32 1312)")
        );
    }

    #[test]
    fn username_and_label_are_fixed_constants() {
        // 凭据管理器里可见：不能出现主机、用户名或引用。
        assert_eq!(CREDENTIAL_USERNAME, "Sqarvo");
        assert!(!CREDENTIAL_USERNAME.contains('@'));
        assert!(!CREDENTIAL_USERNAME.contains(REF));
    }
}
