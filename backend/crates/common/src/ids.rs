//! 强类型标识符。
//!
//! 使用独立的新类型而不是裸 `String`/`Uuid`，避免把连接 ID 当作会话 ID
//! 传递这类错误在编译期就被发现。序列化形式为 `<prefix>_<uuid>`。

use std::error::Error as StdError;
use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

/// 标识符解析失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdParseError {
    input: String,
}

impl fmt::Display for IdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "非法的标识符: {}", self.input)
    }
}

impl StdError for IdParseError {}

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            /// 生成新的随机标识符。
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// 由既有 UUID 构造。
            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            /// 取得内部 UUID。
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// 序列化前缀。
            #[must_use]
            pub const fn prefix() -> &'static str {
                $prefix
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}_{}", $prefix, self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(input: &str) -> Result<Self, Self::Err> {
                let raw = input.strip_prefix(concat!($prefix, "_")).unwrap_or(input);
                Uuid::parse_str(raw)
                    .map(Self)
                    .map_err(|_| IdParseError { input: input.to_owned() })
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: ::serde::Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                let raw = <String as ::serde::Deserialize>::deserialize(deserializer)?;
                raw.parse().map_err(::serde::de::Error::custom)
            }
        }
    };
}

define_id! {
    /// 一条用户可见的连接（可能对应一个或多个物理连接）。
    ConnectionId, "conn"
}

define_id! {
    /// 后端驱动会话。
    SessionId, "sess"
}

define_id! {
    /// 一次 SQL 执行。
    QueryId, "query"
}

define_id! {
    /// 一个显式事务。
    TransactionId, "txn"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_parse_roundtrip() {
        let id = ConnectionId::new();
        let text = id.to_string();
        assert!(text.starts_with("conn_"));
        assert_eq!(text.parse::<ConnectionId>().unwrap(), id);
    }

    #[test]
    fn parse_accepts_bare_uuid() {
        let uuid = Uuid::new_v4();
        let id: SessionId = uuid.to_string().parse().unwrap();
        assert_eq!(id.as_uuid(), &uuid);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!("not-a-uuid".parse::<QueryId>().is_err());
        assert!("query_".parse::<QueryId>().is_err());
    }

    #[test]
    fn serde_uses_prefixed_form() {
        let id = TransactionId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        let decoded: TransactionId = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, id);
    }
}
