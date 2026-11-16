//! 不依赖数据库的单元测试：参数校验、描述符与凭据脱敏。

use std::collections::BTreeMap;

use sqarvo_common::ErrorCategory;
use sqarvo_driver_api::{ConnectionParams, Driver, Secret, TlsMode};

use crate::MariaDbDriver;
use crate::config::MariaDbConfig;

fn params() -> ConnectionParams {
    ConnectionParams {
        driver_id: "mariadb".parse().unwrap(),
        host: "127.0.0.1".to_owned(),
        port: 3306,
        database: Some("app".to_owned()),
        username: Some("app".to_owned()),
        credential_ref: Some("sqarvo:conn/test".to_owned()),
        tls: TlsMode::Preferred,
        tls_server_name: None,
        properties: BTreeMap::new(),
    }
}

#[test]
fn driver_id_and_defaults_are_sane() {
    let driver = MariaDbDriver::new();
    assert_eq!(driver.descriptor().id.as_str(), "mariadb");
    let capabilities = driver.descriptor().capabilities;
    assert!(capabilities.query_cancellation);
    assert!(capabilities.streaming_results);
    assert!(!capabilities.multiple_statements);
    // 事务在驱动层实现（Issue #43）；能力声明必须与实现一致。
    assert!(capabilities.transactions);
    assert!(capabilities.names().contains(&"transactions"));
    // v0.2.2（#82）：目录层（database = catalog）可浏览，schema 层不存在。
    assert!(capabilities.catalogs);
    assert!(capabilities.names().contains(&"catalogs"));
    assert!(!capabilities.schemas);
    // #111：表浏览的排序 / 筛选 / offset / 计数已下推到 SQL，能力位必须与实现一致。
    assert!(capabilities.table_browse_sort_filter_total);
    assert!(
        capabilities
            .names()
            .contains(&"table_browse_sort_filter_total")
    );
    // ADR 0020：上游 `skip_domain_validation` 在 rustls 0.23 下失效，verify-ca 归一化为
    // verify-full。能力位必须为 false，否则前端不会标注这层退化。
    assert!(!capabilities.tls_verify_ca_chain_only);
    assert!(!capabilities.names().contains(&"tls_verify_ca_chain_only"));
}

#[test]
fn username_is_required() {
    let mut params = params();
    params.username = None;
    let error = MariaDbConfig::from_params(&params, None).unwrap_err();
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);
}

#[test]
fn port_must_be_non_zero() {
    let mut params = params();
    params.port = 0;
    assert!(MariaDbConfig::from_params(&params, None).is_err());
}

#[test]
fn unknown_properties_are_rejected() {
    let mut params = params();
    params
        .properties
        .insert("charset".to_owned(), "utf8mb4".to_owned());
    let error = MariaDbConfig::from_params(&params, None).unwrap_err();
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);
}

#[test]
fn client_certificate_requires_key() {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("client.pem");
    std::fs::write(&cert, b"not a real cert").unwrap();

    let mut params = params();
    params
        .properties
        .insert("ssl_cert".to_owned(), cert.display().to_string());
    assert!(MariaDbConfig::from_params(&params, None).is_err());
}

#[test]
fn connect_timeout_is_bounded() {
    let mut params = params();
    params
        .properties
        .insert("connect_timeout_ms".to_owned(), "0".to_owned());
    assert!(MariaDbConfig::from_params(&params, None).is_err());

    params
        .properties
        .insert("connect_timeout_ms".to_owned(), "9999999".to_owned());
    assert!(MariaDbConfig::from_params(&params, None).is_err());

    params
        .properties
        .insert("connect_timeout_ms".to_owned(), "5000".to_owned());
    assert!(MariaDbConfig::from_params(&params, None).is_ok());
}

#[test]
fn debug_never_contains_password() {
    let secret = Secret::new("s3cr3t-password");
    let config = MariaDbConfig::from_params(&params(), Some(&secret)).unwrap();
    let debug = format!("{config:?}");
    assert!(
        !debug.contains("s3cr3t-password"),
        "Debug 泄露了密码：{debug}"
    );
    assert!(debug.contains("MariaDbConfig"));
}

#[test]
fn disabled_tls_rejects_certificate_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, b"pem").unwrap();

    let mut params = params();
    params.tls = TlsMode::Disabled;
    params
        .properties
        .insert("ssl_ca".to_owned(), ca.display().to_string());
    assert!(MariaDbConfig::from_params(&params, None).is_err());
}

/// 元数据查询只读 `information_schema`，它同样会把服务端错误交给统一的分类器。
/// 这里直接构造服务端错误，证明「对象不存在」与「无权限」不会被混为一谈。
#[test]
fn server_errors_used_by_metadata_queries_are_classified() {
    let server = |code: u16, message: &str| {
        mysql_async::Error::Server(mysql_async::ServerError {
            code,
            message: message.to_owned(),
            state: "HY000".to_owned(),
        })
    };
    let mapped = crate::error::map_error(server(1146, "Table 'x.y' doesn't exist"), "列信息");
    assert_eq!(mapped.code(), crate::error::OBJECT_NOT_FOUND);
    assert_eq!(mapped.category(), ErrorCategory::NotFound);

    let mapped = crate::error::map_error(server(1142, "SELECT command denied"), "列信息");
    assert_eq!(mapped.code(), crate::error::PERMISSION_DENIED);
    assert_eq!(mapped.category(), ErrorCategory::PermissionDenied);
}

/// 无默认库的连接上跑未限定表名的 SQL（`select * from config_info`）时，服务端
/// 返回 1046（`ERROR 1046 (3D000) No database selected`）。它必须归类成「缺库归属」，
/// 而不是笼统的 `mariadb.server_error`：前者能告诉用户怎么写，后者只剩一句「执行失败」。
#[test]
fn missing_default_database_is_classified_as_no_database() {
    let mapped = crate::error::map_error(
        mysql_async::Error::Server(mysql_async::ServerError {
            code: 1046,
            message: "No database selected".to_owned(),
            state: "3D000".to_owned(),
        }),
        "执行 MariaDB 语句失败",
    );
    assert_eq!(mapped.code(), crate::error::NO_DATABASE);
    assert_eq!(mapped.category(), ErrorCategory::FailedPrecondition);
    let message = mapped.message();
    assert!(
        message.contains("没有默认库") && message.contains("库.表"),
        "文案要同时说明原因与做法，实际：{message}"
    );
}

/// 没归类过的服务端错误至少带上数字错误码：只给一句「执行失败」时，用户既没法自查，
/// 也没法在 Issue 里说清是哪一个服务端错误。
#[test]
fn unclassified_server_error_carries_the_numeric_code() {
    let mapped = crate::error::map_error(
        mysql_async::Error::Server(mysql_async::ServerError {
            code: 1582,
            message: "Incorrect parameter count in the call to native function 'x'".to_owned(),
            state: "42000".to_owned(),
        }),
        "执行 MariaDB 语句失败",
    );
    assert_eq!(mapped.code(), crate::error::SERVER_ERROR);
    assert_eq!(mapped.category(), ErrorCategory::Internal);
    assert_eq!(
        mapped.message(),
        "执行 MariaDB 语句失败（服务端错误码 1582）"
    );

    // 已归类的错误不带数字码：它们的稳定文案本身就是诊断结论。
    let classified = crate::error::map_error(
        mysql_async::Error::Server(mysql_async::ServerError {
            code: 1205,
            message: "Lock wait timeout exceeded".to_owned(),
            state: "HY000".to_owned(),
        }),
        "执行 MariaDB 语句失败",
    );
    assert_eq!(classified.message(), "等待锁超时");
}

// ---------------------------------------------------------------------------
// TLS 归一化与证书错误分类（ADR 0020、#107）
// ---------------------------------------------------------------------------

/// `verify-ca` 归一化为 `verify-full`：构造出的 SSL 选项绝不能跳过主机名校验，
/// 也不能退回「接受无效证书」——退化只能朝更严的方向走。
#[test]
fn verify_ca_is_normalized_to_verify_full() {
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, b"pem").unwrap();

    let mut params = params();
    params.tls = TlsMode::VerifyCa;
    params
        .properties
        .insert("ssl_ca".to_owned(), ca.display().to_string());
    let config = MariaDbConfig::from_params(&params, None).unwrap();
    let ssl = config.opts.ssl_opts().expect("verify-ca 必须走 TLS");
    assert!(
        !ssl.skip_domain_validation(),
        "verify-ca 不再跳过主机名校验（ADR 0020）"
    );
    assert!(!ssl.accept_invalid_certs());

    params.tls = TlsMode::VerifyFull;
    let config = MariaDbConfig::from_params(&params, None).unwrap();
    let full = config.opts.ssl_opts().expect("verify-full 必须走 TLS");
    assert_eq!(ssl.skip_domain_validation(), full.skip_domain_validation());
    assert_eq!(ssl.accept_invalid_certs(), full.accept_invalid_certs());
}

/// 与真实失败同形的错误：`tokio-rustls` 把 `rustls::Error` 塞进 `io::Error` 内层
/// （`ErrorKind::InvalidData`），`mysql_async` 再包一层 `IoError::Io`。
fn io_error_with(inner: impl std::error::Error + Send + Sync + 'static) -> mysql_async::Error {
    mysql_async::Error::Io(mysql_async::IoError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        inner,
    )))
}

#[test]
fn hostname_mismatch_is_reported_as_certificate_problem() {
    let mapped = crate::error::map_error(
        io_error_with(rustls::Error::InvalidCertificate(
            rustls::CertificateError::NotValidForName,
        )),
        "连接 MariaDB 失败",
    );
    assert_eq!(mapped.code(), crate::error::CONNECTION_FAILED);
    assert_eq!(mapped.category(), ErrorCategory::Unavailable);
    assert_eq!(mapped.message(), "服务端证书与主机名不匹配");
}

#[test]
fn untrusted_certificate_is_reported_as_certificate_problem() {
    for certificate in [
        rustls::CertificateError::UnknownIssuer,
        rustls::CertificateError::Expired,
        rustls::CertificateError::BadSignature,
        rustls::CertificateError::NotValidYet,
    ] {
        let mapped = crate::error::map_error(
            io_error_with(rustls::Error::InvalidCertificate(certificate)),
            "连接 MariaDB 失败",
        );
        assert_eq!(mapped.code(), crate::error::CONNECTION_FAILED);
        assert_eq!(mapped.category(), ErrorCategory::Unavailable);
        assert_eq!(mapped.message(), "服务端证书未被信任或已失效");
    }
}

#[test]
fn non_certificate_io_failure_keeps_generic_message() {
    // 不是证书问题的 `InvalidData`（例如对端在握手阶段乱发字节）不能被说成证书错误。
    let mapped = crate::error::map_error(
        io_error_with(std::io::Error::other("garbage")),
        "连接 MariaDB 失败",
    );
    assert_eq!(mapped.category(), ErrorCategory::Unavailable);
    assert_eq!(mapped.message(), "数据库连接中断");
}

#[test]
fn server_without_tls_capability_is_unavailable() {
    let mapped = crate::error::map_error(
        mysql_async::Error::Driver(mysql_async::DriverError::NoClientSslFlagFromServer),
        "连接 MariaDB 失败",
    );
    assert_eq!(mapped.code(), crate::error::CONNECTION_FAILED);
    assert_eq!(mapped.category(), ErrorCategory::Unavailable);
    assert_eq!(mapped.message(), "服务端不支持 TLS（连接要求 TLS）");
}
