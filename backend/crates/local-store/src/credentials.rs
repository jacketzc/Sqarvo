//! 凭据引用与操作系统凭据存储抽象。
//!
//! 当前状态：**尚未集成任何操作系统凭据存储**。`UnavailableCredentialStore`
//! 对所有操作返回 `credential.unavailable`，上层必须如实把错误展示给用户，
//! 不得回退到把密码写入配置文件。
//!
//! 计划实现：
//!
//! | 平台 | 后端 |
//! |---|---|
//! | Windows | Credential Manager（`windows` crate） |
//! | macOS | Keychain（`security-framework`） |
//! | Linux | Secret Service（D-Bus） |

use std::fmt;

use sqarvo_common::{Error, ErrorCode, Result};

/// 内存中的秘密值。`Debug` 输出永远被脱敏。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// 包装明文秘密。
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取出明文。调用方必须立即使用并尽快丢弃，禁止记录到日志。
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// 操作系统凭据存储抽象。
pub trait CredentialStore: Send + Sync {
    /// 按引用读取凭据。
    fn get(&self, reference: &str) -> Result<Secret>;
    /// 写入或覆盖凭据。
    fn set(&self, reference: &str, secret: &Secret) -> Result<()>;
    /// 删除凭据。引用不存在不应视为错误。
    fn delete(&self, reference: &str) -> Result<()>;
}

/// 占位实现：如实报告「不可用」。
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableCredentialStore;

impl CredentialStore for UnavailableCredentialStore {
    fn get(&self, _reference: &str) -> Result<Secret> {
        Err(Error::unsupported(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            "尚未集成操作系统凭据存储，无法读取数据库凭据",
        ))
    }

    fn set(&self, _reference: &str, _secret: &Secret) -> Result<()> {
        Err(Error::unsupported(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            "尚未集成操作系统凭据存储，无法保存数据库凭据",
        ))
    }

    fn delete(&self, _reference: &str) -> Result<()> {
        Err(Error::unsupported(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            "尚未集成操作系统凭据存储，无法删除数据库凭据",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn unavailable_store_reports_unsupported() {
        let store = UnavailableCredentialStore;
        let error = store.get("conn-1").unwrap_err();
        assert_eq!(error.code(), ErrorCode::CREDENTIAL_UNAVAILABLE);
        assert_eq!(error.category(), sqarvo_common::ErrorCategory::Unsupported);
    }
}
