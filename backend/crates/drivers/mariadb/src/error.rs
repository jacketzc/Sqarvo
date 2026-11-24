//! MariaDB 错误分类。
//!
//! 用户可见文案不拼接驱动或服务端原文：服务端 message 可能包含 SQL 片段、
//! 库表名甚至连接信息。原始错误只作为 `source` 保留，供日志诊断。
//!
//! 唯一例外是**未分类**的服务端错误：文案末尾附数字错误码
//! （`执行 MariaDB 语句失败（服务端错误码 1582）`）。数字不泄露 SQL、库表名或
//! 连接信息，却足以让用户和 Issue 定位到服务端的具体错误；否则一句「执行失败」
//! 既说不清原因，也无法判断是否已经归类过。

use std::borrow::Cow;

use sqarvo_common::{Error, ErrorCategory, ErrorCode};

/// 会话已关闭。
pub const SESSION_CLOSED: ErrorCode = ErrorCode::from_static("mariadb.session_closed");
/// 分页游标与当前查询状态不匹配。
pub const CURSOR_INVALID: ErrorCode = ErrorCode::from_static("mariadb.cursor_invalid");
/// 认证失败（用户名/密码错误）。
pub const AUTH_FAILED: ErrorCode = ErrorCode::from_static("mariadb.auth_failed");
/// 数据库不存在。
pub const UNKNOWN_DATABASE: ErrorCode = ErrorCode::from_static("mariadb.unknown_database");
/// SQL 语法错误。
pub const SYNTAX_ERROR: ErrorCode = ErrorCode::from_static("mariadb.syntax_error");
/// 库表/列不存在。
pub const OBJECT_NOT_FOUND: ErrorCode = ErrorCode::from_static("mariadb.object_not_found");
/// 死锁。
pub const DEADLOCK: ErrorCode = ErrorCode::from_static("mariadb.deadlock");
/// 服务端返回的其它错误。
pub const SERVER_ERROR: ErrorCode = ErrorCode::from_static("mariadb.server_error");
/// 连接建立超时。
pub const CONNECT_TIMEOUT: ErrorCode = ErrorCode::from_static("mariadb.connect_timeout");
/// 连接不可用（对端关闭、握手失败等）。
pub const CONNECTION_FAILED: ErrorCode = ErrorCode::from_static("mariadb.connection_failed");
/// 当前会话已有在途查询。
pub const QUERY_ACTIVE: ErrorCode = ErrorCode::from_static("mariadb.query_active");
/// 查询超时（读写超时）。
pub const QUERY_TIMEOUT: ErrorCode = ErrorCode::from_static("mariadb.query_timeout");
/// 权限不足。
pub const PERMISSION_DENIED: ErrorCode = ErrorCode::from_static("mariadb.permission_denied");
/// 事务中出现了会隐式提交的语句，驱动在发送前拒绝（绝不静默提交）。
pub const IMPLICIT_COMMIT_REJECTED: ErrorCode =
    ErrorCode::from_static("mariadb.implicit_commit_rejected");
/// 只读事务中尝试写入。
pub const READ_ONLY_TRANSACTION: ErrorCode =
    ErrorCode::from_static("mariadb.read_only_transaction");
/// 事务随连接断开/超时而丢失，无法确认提交结果。
pub const TRANSACTION_LOST: ErrorCode = ErrorCode::from_static("mariadb.transaction_lost");
/// 连接没有默认库，SQL / 请求里也没给出库归属（`catalog`，或未限定的表名）：
/// 无法确定表数据、对象列表或这条语句落在哪个库。元数据、表数据与 `Execute`
/// 三条路径共用这一个错误码。
pub const NO_DATABASE: ErrorCode = ErrorCode::from_static("mariadb.no_database");
/// 事务打开时请求切库（ADR 0030）：驱动拒绝，不隐式提交。
pub const CATALOG_SWITCH_IN_TRANSACTION: ErrorCode =
    ErrorCode::from_static("mariadb.catalog_switch_in_transaction");
/// 多语句脚本的语句序号越界（#188）。
pub const STATEMENT_INDEX_INVALID: ErrorCode =
    ErrorCode::from_static("mariadb.statement_index_invalid");

/// 取服务端错误码（仅 `Server` 类错误）。
pub(crate) fn server_code(error: &mysql_async::Error) -> Option<u16> {
    match error {
        mysql_async::Error::Server(server) => Some(server.code),
        _ => None,
    }
}

/// 是否为「查询被中断」（KILL QUERY 或服务端取消）。
pub(crate) fn is_query_interrupted(error: &mysql_async::Error) -> bool {
    matches!(server_code(error), Some(1317))
}

/// 把 `mysql_async` 错误映射为可分类错误。
pub(crate) fn map_error(error: mysql_async::Error, context: &'static str) -> Error {
    let (code, category, message) = classify(&error, context);
    Error::new(code, category, message).with_source(error)
}

fn classify(
    error: &mysql_async::Error,
    context: &'static str,
) -> (ErrorCode, ErrorCategory, Cow<'static, str>) {
    match error {
        mysql_async::Error::Server(server) => map_server_code(server.code, context),
        mysql_async::Error::Io(mysql_async::IoError::Io(io)) => {
            if let Some((code, category, message)) = classify_certificate_error(io) {
                return (code, category, Cow::Borrowed(message));
            }
            match io.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => (
                    QUERY_TIMEOUT,
                    ErrorCategory::Timeout,
                    Cow::Borrowed("Database connection timed out"),
                ),
                std::io::ErrorKind::ConnectionRefused => (
                    CONNECTION_FAILED,
                    ErrorCategory::Unavailable,
                    Cow::Borrowed("Cannot connect database (connection denied)"),
                ),
                _ => (
                    CONNECTION_FAILED,
                    ErrorCategory::Unavailable,
                    Cow::Borrowed("Database Connection Interrupted"),
                ),
            }
        }
        mysql_async::Error::Io(mysql_async::IoError::Tls(_)) => (
            CONNECTION_FAILED,
            ErrorCategory::Unavailable,
            Cow::Borrowed("TLS handshake failed"),
        ),
        mysql_async::Error::Url(_) => (
            ErrorCode::CONFIG_INVALID,
            ErrorCategory::InvalidArgument,
            Cow::Borrowed("Connection parameters could not be parsed"),
        ),
        mysql_async::Error::Driver(driver) => match driver {
            mysql_async::DriverError::ConnectionClosed => (
                CONNECTION_FAILED,
                ErrorCategory::Unavailable,
                Cow::Borrowed("Database connection closed"),
            ),
            // 服务端在握手阶段声明没有 SSL 能力：`required` / `verify-*` 档位下这是
            // 「服务端不满足连接要求」（`Unavailable`），不是驱动内部错误（#107 第 3 条）。
            mysql_async::DriverError::NoClientSslFlagFromServer => (
                CONNECTION_FAILED,
                ErrorCategory::Unavailable,
                Cow::Borrowed("The server does not support TLS, which this connection requires"),
            ),
            _ => (
                CONNECTION_FAILED,
                ErrorCategory::Internal,
                Cow::Borrowed(context),
            ),
        },
        mysql_async::Error::Other(_) => (
            CONNECTION_FAILED,
            ErrorCategory::Internal,
            Cow::Borrowed(context),
        ),
    }
}

/// 把证书类 TLS 失败分成「主机名不匹配」与「证书不可信/已失效」两类。
///
/// 为什么能取回强类型：`tokio-rustls` 用 `io::Error::new(ErrorKind::InvalidData,
/// rustls::Error)` 包装握手失败，原始错误留在 `io::Error` 内层。上游 `mysql_async`
/// 反而退化成匹配错误字符串 `contains("NotValidForName")`，rustls 0.23 改了 Display
/// 文案后这个判断永久失效——这里不重蹈覆辙：只匹配枚举，不做字符串判断（#107）。
///
/// 两类分开的理由是处置方式不同：名字不匹配可以用 `tls_server_name` 覆盖校验名，
/// 证书不可信则要换 CA 证书。
fn classify_certificate_error(
    io: &std::io::Error,
) -> Option<(ErrorCode, ErrorCategory, &'static str)> {
    let error = io.get_ref()?.downcast_ref::<rustls::Error>()?;
    let rustls::Error::InvalidCertificate(certificate) = error else {
        return None;
    };
    let classified = match certificate {
        rustls::CertificateError::NotValidForName
        | rustls::CertificateError::NotValidForNameContext { .. } => (
            CONNECTION_FAILED,
            ErrorCategory::Unavailable,
            "The server certificate does not match the hostname",
        ),
        rustls::CertificateError::UnknownIssuer
        | rustls::CertificateError::Expired
        | rustls::CertificateError::ExpiredContext { .. }
        | rustls::CertificateError::NotValidYet
        | rustls::CertificateError::NotValidYetContext { .. }
        | rustls::CertificateError::InvalidPurpose
        | rustls::CertificateError::InvalidPurposeContext { .. }
        | rustls::CertificateError::Revoked
        | rustls::CertificateError::BadSignature => (
            CONNECTION_FAILED,
            ErrorCategory::Unavailable,
            "The server certificate is untrusted or expired",
        ),
        // 其余证书错误（编码、扩展、OCSP 响应等）同样是握手失败，不再细分文案。
        _ => (
            CONNECTION_FAILED,
            ErrorCategory::Unavailable,
            "TLS handshake failed",
        ),
    };
    Some(classified)
}

fn map_server_code(
    code: u16,
    context: &'static str,
) -> (ErrorCode, ErrorCategory, Cow<'static, str>) {
    // 已知错误码都有稳定文案：只写「是什么 + 怎么办」，不拼服务端原文。
    let known = |code: ErrorCode, category: ErrorCategory, message: &'static str| {
        (code, category, Cow::Borrowed(message))
    };
    match code {
        // ER_ACCESS_DENIED_ERROR / ER_ACCESS_DENIED_NO_PASSWORD_ERROR
        1045 | 1698 => known(
            AUTH_FAILED,
            ErrorCategory::Unauthenticated,
            "Database authentication failed (user name or password error)",
        ),
        // ER_DBACCESS_DENIED_ERROR / ER_TABLEACCESS_DENIED_ERROR / ER_COLUMNACCESS_DENIED_ERROR
        1044 | 1142 | 1143 => known(
            PERMISSION_DENIED,
            ErrorCategory::PermissionDenied,
            "Current account does not have access to the object",
        ),
        // ER_BAD_DB_ERROR
        1049 => known(
            UNKNOWN_DATABASE,
            ErrorCategory::InvalidArgument,
            "Database does not exist",
        ),
        // ER_NO_DB_ERROR：连接没有默认库，SQL 又没有限定表名（例如 `select * from t`）。
        // 与元数据/表数据路径同一个错误码与分类：缺的是「库归属」，不是 SQL 写错了。
        1046 => known(
            NO_DATABASE,
            ErrorCategory::FailedPrecondition,
            "No default database is configured. Qualify the table name in SQL (for example, database.table) or set a default database in the connection settings",
        ),
        // ER_PARSE_ERROR
        1064 => known(
            SYNTAX_ERROR,
            ErrorCategory::InvalidArgument,
            "SQL syntax error",
        ),
        // ER_NO_SUCH_TABLE / ER_BAD_FIELD_ERROR / ER_BAD_TABLE_ERROR
        1146 | 1054 | 1051 => known(
            OBJECT_NOT_FOUND,
            ErrorCategory::NotFound,
            "The table or column does not exist",
        ),
        // ER_QUERY_INTERRUPTED
        1317 => known(
            ErrorCode::QUERY_CANCELLED,
            ErrorCategory::Cancelled,
            "Query canceled",
        ),
        // ER_LOCK_WAIT_TIMEOUT
        1205 => known(
            SERVER_ERROR,
            ErrorCategory::Timeout,
            "Timed out waiting for a lock",
        ),
        // ER_LOCK_DEADLOCK
        1213 => known(
            DEADLOCK,
            ErrorCategory::FailedPrecondition,
            "Deadlock detected",
        ),
        // ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION
        1792 => known(
            READ_ONLY_TRANSACTION,
            ErrorCategory::FailedPrecondition,
            "Writes are not allowed in a read-only transaction",
        ),
        // ER_CONNECTION_KILLED / ER_SERVER_SHUTDOWN
        1927 | 1053 => known(
            CONNECTION_FAILED,
            ErrorCategory::Unavailable,
            "Database connection has been closed by the service",
        ),
        // 没归类过的服务端错误：至少把数字错误码给出去，否则用户只看到一句
        // 「执行 MariaDB 语句失败」，既无法自查也无法判断有没有被归类。
        _ => (
            SERVER_ERROR,
            ErrorCategory::Internal,
            Cow::Owned(format!("{context} (server error code {code})")),
        ),
    }
}

/// 连接超时（调用方在 `tokio::time::timeout` 之外判断）。
pub(crate) fn connect_timeout() -> Error {
    Error::new(
        CONNECT_TIMEOUT,
        ErrorCategory::Timeout,
        "Connecting to the database timed out",
    )
}

/// 事务打开时拒绝切库（ADR 0030 第 5 条）。
///
/// 不静默忽略、也不隐式提交用户事务：用户要么先提交 / 回滚，要么在事务里自己写
/// `USE`（那是他的语句，不算驱动替他做主）。
pub(crate) fn catalog_switch_in_transaction() -> Error {
    Error::failed_precondition(
        CATALOG_SWITCH_IN_TRANSACTION,
        "A transaction is open. Commit or roll it back before switching databases",
    )
}
