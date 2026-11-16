//! 通用错误 → gRPC 状态映射。
//!
//! 约定：
//!
//! - status message 只包含可安全展示的文案，不含凭据与堆栈；
//! - 稳定错误码放在 trailing metadata 的 `sqarvo-error-code`，供客户端分支处理；
//! - `Unsupported` 映射为 `UNIMPLEMENTED`，让前端明确区分「未实现」与「执行失败」；
//! - 多语句脚本失败时（#187），失败序号与已完成语句数放在 trailing metadata 的
//!   `sqarvo-failed-statement` / `sqarvo-completed-statements`（ADR 0006 修订）。

use sqarvo_common::{Error, ErrorCategory};
use tonic::metadata::MetadataValue;
use tonic::{Code, Status};

/// 携带稳定错误码的 metadata 键。
pub const ERROR_CODE_METADATA_KEY: &str = "sqarvo-error-code";
/// 多语句脚本中失败语句的序号（0 起始，ascii 十进制）。
pub const FAILED_STATEMENT_METADATA_KEY: &str = "sqarvo-failed-statement";
/// 多语句脚本中失败前已成功执行的语句数（ascii 十进制）。
pub const COMPLETED_STATEMENTS_METADATA_KEY: &str = "sqarvo-completed-statements";

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
    // 失败位置只附加、不改分类：code/category 保留驱动 classify 原样（#187）。
    if let Some(failure) = error.statement_failure() {
        if let Ok(value) =
            MetadataValue::<tonic::metadata::Ascii>::try_from(failure.failed_index.to_string())
        {
            status
                .metadata_mut()
                .insert(FAILED_STATEMENT_METADATA_KEY, value);
        }
        if let Ok(value) = MetadataValue::<tonic::metadata::Ascii>::try_from(
            failure.completed_statements.to_string(),
        ) {
            status
                .metadata_mut()
                .insert(COMPLETED_STATEMENTS_METADATA_KEY, value);
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use sqarvo_common::{ErrorCode, StatementFailure};

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

    fn metadata_str(status: &Status, key: &str) -> Option<String> {
        status
            .metadata()
            .get(key)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    /// #187：脚本失败位置写入两个 trailing metadata，错误分类保留驱动原样。
    #[test]
    fn statement_failure_writes_position_metadata_and_keeps_classification() {
        let error = Error::new(
            ErrorCode::IO,
            ErrorCategory::Internal,
            "UNIQUE constraint failed: t.x",
        )
        .with_statement_failure(StatementFailure {
            failed_index: 1,
            completed_statements: 1,
        });
        let status = to_status(error);
        assert_eq!(status.code(), Code::Internal);
        assert_eq!(
            metadata_str(&status, ERROR_CODE_METADATA_KEY).as_deref(),
            Some("io.error")
        );
        assert_eq!(
            metadata_str(&status, FAILED_STATEMENT_METADATA_KEY).as_deref(),
            Some("1")
        );
        assert_eq!(
            metadata_str(&status, COMPLETED_STATEMENTS_METADATA_KEY).as_deref(),
            Some("1")
        );
    }

    /// #187：超预算错误同样携带失败位置，分类为 `ResourceExhausted`。
    #[test]
    fn script_budget_error_carries_position_metadata() {
        let error = Error::resource_exhausted(
            ErrorCode::QUERY_SCRIPT_TOO_LARGE,
            "脚本缓冲行数超过预算，请拆分脚本或给查询加 LIMIT",
        )
        .with_statement_failure(StatementFailure {
            failed_index: 2,
            completed_statements: 2,
        });
        let status = to_status(error);
        assert_eq!(status.code(), Code::ResourceExhausted);
        assert_eq!(
            metadata_str(&status, ERROR_CODE_METADATA_KEY).as_deref(),
            Some("query.script_too_large")
        );
        assert_eq!(
            metadata_str(&status, FAILED_STATEMENT_METADATA_KEY).as_deref(),
            Some("2")
        );
        assert_eq!(
            metadata_str(&status, COMPLETED_STATEMENTS_METADATA_KEY).as_deref(),
            Some("2")
        );
    }

    /// 非脚本错误不得携带失败位置 metadata（旧客户端行为不变）。
    #[test]
    fn plain_error_has_no_position_metadata() {
        let status = to_status(Error::not_found(ErrorCode::QUERY_NOT_FOUND, "查询不存在"));
        assert!(
            status
                .metadata()
                .get(FAILED_STATEMENT_METADATA_KEY)
                .is_none()
        );
        assert!(
            status
                .metadata()
                .get(COMPLETED_STATEMENTS_METADATA_KEY)
                .is_none()
        );
    }
}
