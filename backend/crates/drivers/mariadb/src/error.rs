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
                    Cow::Borrowed("数据库连接超时"),
                ),
                std::io::ErrorKind::ConnectionRefused => (
                    CONNECTION_FAILED,
                    ErrorCategory::Unavailable,
                    Cow::Borrowed("无法连接数据库（连接被拒绝）"),
                ),
                _ => (
                    CONNECTION_FAILED,
                    ErrorCategory::Unavailable,
                    Cow::Borrowed("数据库连接中断"),
                ),
            }
        }
        mysql_async::Error::Io(mysql_async::IoError::Tls(_)) => (
            CONNECTION_FAILED,
            ErrorCategory::Unavailable,
            Cow::Borrowed("TLS 握手失败"),
        ),
        mysql_async::Error::Url(_) => (
            ErrorCode::CONFIG_INVALID,
            ErrorCategory::InvalidArgument,
            Cow::Borrowed("连接参数无法解析"),
        ),
        mysql_async::Error::Driver(driver) => match driver {
            mysql_async::DriverError::ConnectionClosed => (
                CONNECTION_FAILED,
                ErrorCategory::Unavailable,
                Cow::Borrowed("数据库连接已关闭"),
            ),
            // 服务端在握手阶段声明没有 SSL 能力：`required` / `verify-*` 档位下这是
            // 「服务端不满足连接要求」（`Unavailable`），不是驱动内部错误（#107 第 3 条）。
            mysql_async::DriverError::NoClientSslFlagFromServer => (
                CONNECTION_FAILED,
                ErrorCategory::Unavailable,
                Cow::Borrowed("服务端不支持 TLS（连接要求 TLS）"),
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
            "服务端证书与主机名不匹配",
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
            "服务端证书未被信任或已失效",
        ),
        // 其余证书错误（编码、扩展、OCSP 响应等）同样是握手失败，不再细分文案。
        _ => (
            CONNECTION_FAILED,
            ErrorCategory::Unavailable,
            "TLS 握手失败",
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
            "数据库认证失败（用户名或密码错误）",
        ),
        // ER_DBACCESS_DENIED_ERROR / ER_TABLEACCESS_DENIED_ERROR / ER_COLUMNACCESS_DENIED_ERROR
        1044 | 1142 | 1143 => known(
            PERMISSION_DENIED,
            ErrorCategory::PermissionDenied,
            "当前账号没有访问该对象的权限",
        ),
        // ER_BAD_DB_ERROR
        1049 => known(
            UNKNOWN_DATABASE,
            ErrorCategory::InvalidArgument,
            "数据库不存在",
        ),
        // ER_NO_DB_ERROR：连接没有默认库，SQL 又没有限定表名（例如 `select * from t`）。
        // 与元数据/表数据路径同一个错误码与分类：缺的是「库归属」，不是 SQL 写错了。
        1046 => known(
            NO_DATABASE,
            ErrorCategory::FailedPrecondition,
            "当前连接没有默认库：请在 SQL 里写明库名（例如「库.表」），\
             或在连接配置里填写默认库",
        ),
        // ER_PARSE_ERROR
        1064 => known(SYNTAX_ERROR, ErrorCategory::InvalidArgument, "SQL 语法错误"),
        // ER_NO_SUCH_TABLE / ER_BAD_FIELD_ERROR / ER_BAD_TABLE_ERROR
        1146 | 1054 | 1051 => known(OBJECT_NOT_FOUND, ErrorCategory::NotFound, "表或列不存在"),
        // ER_QUERY_INTERRUPTED
        1317 => known(
            ErrorCode::QUERY_CANCELLED,
            ErrorCategory::Cancelled,
            "查询已取消",
        ),
        // ER_LOCK_WAIT_TIMEOUT
        1205 => known(SERVER_ERROR, ErrorCategory::Timeout, "等待锁超时"),
        // ER_LOCK_DEADLOCK
        1213 => known(DEADLOCK, ErrorCategory::FailedPrecondition, "检测到死锁"),
        // ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION
        1792 => known(
            READ_ONLY_TRANSACTION,
            ErrorCategory::FailedPrecondition,
            "只读事务中不允许写入",
        ),
        // ER_CONNECTION_KILLED / ER_SERVER_SHUTDOWN
        1927 | 1053 => known(
            CONNECTION_FAILED,
            ErrorCategory::Unavailable,
            "数据库连接已被服务端关闭",
        ),
        // 没归类过的服务端错误：至少把数字错误码给出去，否则用户只看到一句
        // 「执行 MariaDB 语句失败」，既无法自查也无法判断有没有被归类。
        _ => (
            SERVER_ERROR,
            ErrorCategory::Internal,
            Cow::Owned(format!("{context}（服务端错误码 {code}）")),
        ),
    }
}

/// 连接超时（调用方在 `tokio::time::timeout` 之外判断）。
pub(crate) fn connect_timeout() -> Error {
    Error::new(CONNECT_TIMEOUT, ErrorCategory::Timeout, "连接数据库超时")
}
