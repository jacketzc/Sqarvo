//! 统一错误模型。
//!
//! 设计目标：
//!
//! - 错误必须可分类，能稳定映射到 gRPC 状态码，供前端做分支处理；
//! - 错误码稳定且可被文档化，不随错误文案变化；
//! - 文案面向用户且安全，禁止包含凭据、完整连接串或内部堆栈。

use std::error::Error as StdError;
use std::fmt;

/// 错误分类。与 gRPC 状态码一一对应，但不依赖 gRPC 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    /// 请求参数非法。
    InvalidArgument,
    /// 资源不存在。
    NotFound,
    /// 资源已存在，例如重复注册驱动。
    AlreadyExists,
    /// 当前状态不允许该操作。
    FailedPrecondition,
    /// 能力不被支持，必须显式失败而不是静默模拟成功。
    Unsupported,
    /// 未通过本地认证（例如缺少会话 token）。
    Unauthenticated,
    /// 权限不足。
    PermissionDenied,
    /// 资源耗尽，例如并发查询超限。
    ResourceExhausted,
    /// 操作被取消。
    Cancelled,
    /// 超时。
    Timeout,
    /// 依赖不可用，例如数据库连接断开。
    Unavailable,
    /// 内部错误。
    Internal,
}

impl ErrorCategory {
    /// 稳定的字符串标识，用于与前端约定。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::FailedPrecondition => "failed_precondition",
            Self::Unsupported => "unsupported",
            Self::Unauthenticated => "unauthenticated",
            Self::PermissionDenied => "permission_denied",
            Self::ResourceExhausted => "resource_exhausted",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    /// 是否属于调用方可以重试的瞬时错误。
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::Timeout | Self::ResourceExhausted
        )
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 稳定的错误码。
///
/// 错误码一旦发布即视为契约的一部分：允许新增，不允许改变既有含义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode(&'static str);

impl ErrorCode {
    /// 请求的驱动未注册。
    pub const DRIVER_NOT_FOUND: Self = Self("driver.not_found");
    /// 会话不存在或已断开。
    pub const SESSION_NOT_FOUND: Self = Self("session.not_found");
    /// 查询不存在或已结束。
    pub const QUERY_NOT_FOUND: Self = Self("query.not_found");
    /// 查询已被取消。
    pub const QUERY_CANCELLED: Self = Self("query.cancelled");
    /// 并发查询数量已达上限。
    pub const QUERY_LIMIT_REACHED: Self = Self("query.limit_reached");
    /// 事务处于失败状态，只能回滚。
    pub const TRANSACTION_FAILED: Self = Self("transaction.failed");
    /// 请求的能力不被当前驱动支持。
    pub const CAPABILITY_UNSUPPORTED: Self = Self("capability.unsupported");
    /// 配置非法。
    pub const CONFIG_INVALID: Self = Self("config.invalid");
    /// 凭据不可用（未集成系统凭据存储或用户拒绝授权）。
    pub const CREDENTIAL_UNAVAILABLE: Self = Self("credential.unavailable");
    /// 本地传输失败。
    pub const TRANSPORT_FAILED: Self = Self("transport.failed");
    /// 本地 I/O 失败。
    pub const IO: Self = Self("io.error");
    /// 内部错误。
    pub const INTERNAL: Self = Self("internal.error");

    /// 由静态字符串构造错误码。用于尚未固定的模块级错误。
    #[must_use]
    pub const fn from_static(code: &'static str) -> Self {
        Self(code)
    }

    /// 取得错误码文本。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// 后端统一错误。
#[derive(Debug)]
pub struct Error {
    code: ErrorCode,
    category: ErrorCategory,
    message: String,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl Error {
    /// 构造错误。
    #[must_use]
    pub fn new(code: ErrorCode, category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            code,
            category,
            message: message.into(),
            source: None,
        }
    }

    /// 附加上游错误，便于日志定位；不会把 source 的文案直接暴露给前端。
    #[must_use]
    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// 参数非法。
    #[must_use]
    pub fn invalid_argument(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::InvalidArgument, message)
    }

    /// 资源不存在。
    #[must_use]
    pub fn not_found(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::NotFound, message)
    }

    /// 能力不支持。
    #[must_use]
    pub fn unsupported(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::Unsupported, message)
    }

    /// 当前状态不允许。
    #[must_use]
    pub fn failed_precondition(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::FailedPrecondition, message)
    }

    /// 资源耗尽。
    #[must_use]
    pub fn resource_exhausted(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::ResourceExhausted, message)
    }

    /// 内部错误。
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::INTERNAL, ErrorCategory::Internal, message)
    }

    /// 资源已存在。
    #[must_use]
    pub fn already_exists(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, ErrorCategory::AlreadyExists, message)
    }

    /// 错误码。
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// 错误分类。
    #[must_use]
    pub const fn category(&self) -> ErrorCategory {
        self.category
    }

    /// 可安全展示的文案。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.message, self.code)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn StdError + 'static))
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        let message = source.to_string();
        Self {
            code: ErrorCode::IO,
            category: ErrorCategory::Internal,
            message,
            source: Some(Box::new(source)),
        }
    }
}

/// 后端通用结果类型。
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_have_stable_names() {
        assert_eq!(ErrorCategory::NotFound.as_str(), "not_found");
        assert_eq!(
            ErrorCategory::FailedPrecondition.to_string(),
            "failed_precondition"
        );
        assert!(ErrorCategory::Unavailable.is_retryable());
        assert!(!ErrorCategory::InvalidArgument.is_retryable());
    }

    #[test]
    fn error_display_includes_code() {
        let error = Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, "事务不受支持");
        assert_eq!(error.to_string(), "事务不受支持 [capability.unsupported]");
        assert_eq!(error.category(), ErrorCategory::Unsupported);
    }

    #[test]
    fn io_error_keeps_source() {
        let error = Error::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ));
        assert_eq!(error.code(), ErrorCode::IO);
        assert_eq!(error.category(), ErrorCategory::Internal);
        assert!(StdError::source(&error).is_some());
    }
}
