//! 通用错误 → gRPC 状态映射。
//!
//! 约定：
//!
//! - status message 只包含可安全展示的文案，不含凭据与堆栈；
//! - 稳定错误码放在 trailing metadata 的 `sqarvo-error-code`，供客户端分支处理；
//! - `Unsupported` 映射为 `UNIMPLEMENTED`，让前端明确区分「未实现」与「执行失败」。

use sqarvo_common::{Error, ErrorCategory};
use tonic::metadata::MetadataValue;
use tonic::{Code, Status};

/// 携带稳定错误码的 metadata 键。
pub const ERROR_CODE_METADATA_KEY: &str = "sqarvo-error-code";

/// 把后端错误转换为 gRPC 状态。
///
/// 按值接收错误：转换后调用方不应继续使用该错误，所有权转移可以避免
/// 误把原始错误（可能带 source）再次记录到别处。
#[must_use]
#[allow(clippy::needless_pass_by_value)]
pub fn to_status(error: Error) -> Status {
    let code = match error.category() {
        ErrorCategory::InvalidArgument => Code::InvalidArgument,
        ErrorCategory::NotFound => Code::NotFound,
        ErrorCategory::AlreadyExists => Code::AlreadyExists,
        ErrorCategory::FailedPrecondition => Code::FailedPrecondition,
        ErrorCategory::Unsupported => Code::Unimplemented,
        ErrorCategory::Unauthenticated => Code::Unauthenticated,
        ErrorCategory::PermissionDenied => Code::PermissionDenied,
        ErrorCategory::ResourceExhausted => Code::ResourceExhausted,
        ErrorCategory::Cancelled => Code::Cancelled,
        ErrorCategory::Timeout => Code::DeadlineExceeded,
        ErrorCategory::Unavailable => Code::Unavailable,
        ErrorCategory::Internal => Code::Internal,
    };

    let mut status = Status::new(code, error.message().to_owned());
    if let Ok(value) = MetadataValue::<tonic::metadata::Ascii>::try_from(error.code().as_str()) {
        status.metadata_mut().insert(ERROR_CODE_METADATA_KEY, value);
    }
    status
}

#[cfg(test)]
mod tests {
    use sqarvo_common::ErrorCode;

    use super::*;

    #[test]
    fn unsupported_maps_to_unimplemented() {
        let status = to_status(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "事务不受支持",
        ));
        assert_eq!(status.code(), Code::Unimplemented);
        assert_eq!(
            status
                .metadata()
                .get(ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some("capability.unsupported")
        );
    }

    #[test]
    fn missing_resource_maps_to_not_found() {
        let status = to_status(Error::not_found(ErrorCode::SESSION_NOT_FOUND, "会话不存在"));
        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(status.message(), "会话不存在");
    }
}
