//! SQLite 错误分类。

use rusqlite::ErrorCode as SqliteCode;
use sqarvo_common::{Error, ErrorCategory, ErrorCode};

/// 数据库以只读方式打开，写操作被拒绝。
pub const SQLITE_READ_ONLY: ErrorCode = ErrorCode::from_static("sqlite.read_only");
/// 数据库忙或被锁定。
pub const SQLITE_BUSY: ErrorCode = ErrorCode::from_static("sqlite.busy");
/// 打开数据库失败（路径错误、不是数据库文件等）。
pub const SQLITE_OPEN_FAILED: ErrorCode = ErrorCode::from_static("sqlite.open_failed");
/// 会话已关闭。
pub const SESSION_CLOSED: ErrorCode = ErrorCode::from_static("sqlite.session_closed");
/// 游标与当前查询状态不匹配。
pub const SQLITE_CURSOR_INVALID: ErrorCode = ErrorCode::from_static("sqlite.cursor_invalid");
/// 当前会话已有在途查询。
pub const SQLITE_QUERY_ACTIVE: ErrorCode = ErrorCode::from_static("sqlite.query_active");
/// 元数据请求的目标表/视图不存在。
pub const OBJECT_NOT_FOUND: ErrorCode = ErrorCode::from_static("sqlite.object_not_found");

/// 把 SQLite 错误映射为可分类错误。
///
/// 用户可见文案不拼接底层 message：其中可能包含本地路径或 SQL 片段。
/// 原始错误通过 `source` 保留，仅供日志诊断。
pub(crate) fn map_sqlite_error(error: rusqlite::Error, context: &'static str) -> Error {
    let (code, category, message) = match &error {
        rusqlite::Error::SqliteFailure(failure, _) => classify(failure.code, context),
        _ => (ErrorCode::IO, ErrorCategory::Internal, context),
    };
    Error::new(code, category, message).with_source(error)
}

fn classify(code: SqliteCode, context: &'static str) -> (ErrorCode, ErrorCategory, &'static str) {
    match code {
        SqliteCode::CannotOpen | SqliteCode::NotADatabase => (
            SQLITE_OPEN_FAILED,
            ErrorCategory::InvalidArgument,
            "无法打开 SQLite 数据库（路径错误或文件不是数据库）",
        ),
        SqliteCode::PermissionDenied => (
            ErrorCode::IO,
            ErrorCategory::PermissionDenied,
            "没有访问数据库文件所需的权限",
        ),
        SqliteCode::ReadOnly => (
            SQLITE_READ_ONLY,
            ErrorCategory::FailedPrecondition,
            "数据库以只读方式打开，无法执行写操作",
        ),
        SqliteCode::DatabaseBusy | SqliteCode::DatabaseLocked => (
            SQLITE_BUSY,
            ErrorCategory::Unavailable,
            "数据库忙或被其他连接锁定",
        ),
        SqliteCode::OperationInterrupted => (
            ErrorCode::QUERY_CANCELLED,
            ErrorCategory::Cancelled,
            "查询已取消",
        ),
        SqliteCode::SystemIoFailure => (
            ErrorCode::IO,
            ErrorCategory::Unavailable,
            "读取或写入数据库文件失败",
        ),
        SqliteCode::DiskFull => (
            ErrorCode::IO,
            ErrorCategory::ResourceExhausted,
            "磁盘空间不足",
        ),
        _ => (ErrorCode::IO, ErrorCategory::Internal, context),
    }
}

/// 打开数据库失败的专用映射。
pub(crate) fn map_open_error(error: rusqlite::Error) -> Error {
    map_sqlite_error(error, "打开 SQLite 数据库失败")
}
