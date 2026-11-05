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
