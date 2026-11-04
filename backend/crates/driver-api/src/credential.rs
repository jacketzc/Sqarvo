//! 数据库凭据的传输类型。
//!
//! 凭据不进入 [`crate::ConnectionParams`]：那个结构会被 `Debug` 打印、被写入错误上下文、
//! 被测试快照引用。密码只在「驱动连接」这一个调用点上以本类型的引用形式出现，
//! 用完即丢（见 ADR 0013）。

use std::fmt;

/// 明文秘密值（通常是数据库密码）。
///
/// `Debug` 输出永远被脱敏；取明文只能通过 [`Secret::expose`]，调用方必须立即使用并尽快丢弃，
/// 禁止写入日志、错误信息或任何持久化存储。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn equality_compares_content() {
        assert_eq!(Secret::new("a"), Secret::new("a"));
        assert_ne!(Secret::new("a"), Secret::new("b"));
    }
}
