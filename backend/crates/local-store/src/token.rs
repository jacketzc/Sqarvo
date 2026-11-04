//! 本地 IPC 会话 token。
//!
//! 这不是「登录认证」，而是本地进程间访问控制：即使 socket 文件权限被放宽，
//! 或 Windows 上退化为回环 TCP，调用方仍必须持有 token 才能访问后端。

use std::fmt;

use sqarvo_common::{Error, ErrorCode, Result};
use uuid::Uuid;

/// 256 位随机会话 token，十六进制表示。
#[derive(Clone, PartialEq, Eq)]
pub struct AuthToken(String);

impl AuthToken {
    /// 生成新 token。使用两个 `UUIDv4` 拼接，共 256 位随机性。
    #[must_use]
    pub fn generate() -> Self {
        let a = Uuid::new_v4().simple().to_string();
        let b = Uuid::new_v4().simple().to_string();
        Self(format!("{a}{b}"))
    }

    /// 从已有十六进制字符串恢复（例如客户端读取端点描述文件后）。
    pub fn from_hex(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid = value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        if valid {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(Error::invalid_argument(
                ErrorCode::TRANSPORT_FAILED,
                "会话 token 必须是 64 位十六进制字符串",
            ))
        }
    }

    /// 明文形式。只允许写入受权限保护的端点文件或放进请求 metadata，
    /// 禁止写入日志。
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 校验候选 token。使用固定长度、无提前返回的比较，降低时序侧信道风险。
    ///
    /// 说明：这不是经过审计的密码学实现，token 本身为 256 位随机值，
    /// 该比较仅用于避免最明显的时序差异。
    #[must_use]
    pub fn verify(&self, candidate: &str) -> bool {
        let expected = self.0.as_bytes();
        let actual = candidate.as_bytes();
        if expected.len() != actual.len() {
            return false;
        }
        let mut diff = 0u8;
        for (left, right) in expected.iter().zip(actual) {
            diff |= left ^ right;
        }
        diff == 0
    }
}

impl fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthToken(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_hex_and_random() {
        let first = AuthToken::generate();
        let second = AuthToken::generate();
        assert_eq!(first.expose().len(), 64);
        assert!(first.expose().bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn verify_accepts_only_exact_match() {
        let token = AuthToken::generate();
        assert!(token.verify(token.expose()));
        assert!(!token.verify("deadbeef"));
        let mut tampered = token.expose().to_owned();
        tampered.replace_range(0..1, if tampered.starts_with('0') { "1" } else { "0" });
        assert!(!token.verify(&tampered));
    }

    #[test]
    fn debug_never_prints_secret() {
        let token = AuthToken::generate();
        let rendered = format!("{token:?}");
        assert!(!rendered.contains(token.expose()));
        assert_eq!(rendered, "AuthToken(<redacted>)");
    }

    #[test]
    fn from_hex_validates_shape() {
        assert!(AuthToken::from_hex("zz").is_err());
        assert!(AuthToken::from_hex("a".repeat(64)).is_ok());
        let upper = AuthToken::from_hex("A".repeat(64)).unwrap();
        assert_eq!(upper.expose(), &"a".repeat(64));
    }
}
