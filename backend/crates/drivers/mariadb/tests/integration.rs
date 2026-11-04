//! MariaDB 集成测试。
//!
//! 需要真实数据库：设置 `SQARVO_MARIADB_URL`
//! （形如 `mysql://sqarvo_test:sqarvo_test_only@127.0.0.1:53306/sqarvo_test`）。
//! 未设置时用例会打印跳过原因并返回——CI 会显式设置该变量（#25），
//! 不允许在 CI 中静默跳过。
//!
//! 启动测试库（rootless podman）：
//! `podman run -d --name sqarvo-mariadb-test -e MARIADB_USER=sqarvo_test ... -p 127.0.0.1:53306:3306 docker.io/library/mariadb:11.4`

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{ConnectionParams, Driver, DriverSession, ExecuteRequest, TlsMode, Value};
use sqarvo_driver_mariadb::{MariaDbDriver, error};

#[tokio::test]
async fn test_connection_reports_version() {
    let Some((params, secret)) = connection_params() else {
        return;
    };
    let driver = MariaDbDriver::new();
    let report = driver
        .test_connection(&params, Some(&secret))
        .await
        .expect("测试连接应成功");
    assert!(report.reachable);
    let version = report.server_version.expect("应返回服务端版本");
    assert!(version.contains("MariaDB"), "版本串不符合预期：{version}");
    assert!(report.latency < Duration::from_secs(10));
}

#[tokio::test]
async fn query_batches_follow_the_cursor() {
    let Some(session) = session().await else {
        return;
    };
    session
        .execute(request("DROP TABLE IF EXISTS sqarvo_t_batch"))
        .await
        .unwrap();
    session
        .execute(request(
            "CREATE TABLE sqarvo_t_batch (id INT UNSIGNED NOT NULL, v VARCHAR(32))",
        ))
        .await
        .unwrap();
    let inserted = session
        .execute(request(
            "INSERT INTO sqarvo_t_batch VALUES (1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e')",
        ))
        .await
        .unwrap();
    assert_eq!(inserted.statements[0].rows_affected, Some(5));

    let query_id = QueryId::new();
    let start = session
        .execute(ExecuteRequest {
            query_id,
            sql: "SELECT id, v FROM sqarvo_t_batch ORDER BY id".to_owned(),
            max_rows: Some(2),
            timeout: None,
        })
        .await
        .unwrap();
    let summary = &start.statements[0];
    assert!(summary.has_result_set);
    assert_eq!(summary.columns.len(), 2);
    assert_eq!(
        summary.columns[0].original_type_name.as_deref(),
        Some("int unsigned")
    );

    let first = start.first_batch.expect("应有首批");
    assert_eq!(first.rows.len(), 2);
    assert!(first.has_more);
    let mut cursor = first.next_cursor.clone().expect("应有游标");

    let mut ids: Vec<u64> = first.rows.iter().map(uint_at).collect();

    // 查询仍在进行时，错误游标必须显式失败，而不是重查或返回重复行。
    let cursor_error = session
        .fetch(&query_id, 0, Some("999"), 2)
        .await
        .unwrap_err();
    assert_eq!(cursor_error.code(), error::CURSOR_INVALID);
    assert_eq!(cursor_error.category(), ErrorCategory::InvalidArgument);

    let mut batches = 1;
    loop {
        let batch = session.fetch(&query_id, 0, Some(&cursor), 2).await.unwrap();
        ids.extend(batch.rows.iter().map(uint_at));
        batches += 1;
        if !batch.has_more {
            break;
        }
        cursor = batch.next_cursor.expect("has_more 应带游标");
    }
    assert_eq!(ids, vec![1, 2, 3, 4, 5]);
    assert_eq!(batches, 3);

    // 查询结束后再取，应明确报「查询不存在」，而不是返回空批假装成功。
    let finished = session
        .fetch(&query_id, 0, Some(&cursor), 2)
        .await
        .unwrap_err();
    assert_eq!(finished.code(), ErrorCode::QUERY_NOT_FOUND);
}

#[tokio::test]
async fn driver_passes_testkit_smoke() {
    let Some(session) = session().await else {
        return;
    };
    session
        .execute(request("DROP TABLE IF EXISTS sqarvo_t_smoke"))
        .await
        .unwrap();
    session
        .execute(request("CREATE TABLE sqarvo_t_smoke (v INT)"))
        .await
        .unwrap();
    session
        .execute(request("INSERT INTO sqarvo_t_smoke VALUES (1),(2),(3)"))
        .await
        .unwrap();

    let report = sqarvo_driver_testkit::smoke(&*session, "SELECT v FROM sqarvo_t_smoke", 2)
        .await
        .expect("smoke 应通过");
    assert_eq!(report.statements, 1);
    assert_eq!(report.rows, 3);
    assert_eq!(report.batches, 2);
}

#[tokio::test]
async fn cancel_interrupts_long_query_and_is_idempotent() {
    let Some(session) = session().await else {
        return;
    };
    let session: Arc<dyn DriverSession> = Arc::from(session);
    let query_id = QueryId::new();

    let running = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .execute(ExecuteRequest {
                    query_id,
                    sql: "SELECT SLEEP(30)".to_owned(),
                    max_rows: None,
                    timeout: None,
                })
                .await
        })
    };

    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    session.cancel(&query_id).await.expect("首次取消应成功");
    session.cancel(&query_id).await.expect("重复取消应保持幂等");
    let error = running
        .await
        .expect("任务未 panic")
        .expect_err("查询应被取消");
    let elapsed = started.elapsed();

    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);
    assert_eq!(error.category(), ErrorCategory::Cancelled);
    assert!(
        elapsed < Duration::from_secs(2),
        "取消耗时 {elapsed:?} 超过 2 秒"
    );

    // 被中断的连接必须仍可复用（ADR 0011 的实测结论）。
    let start = session
        .execute(request("SELECT 1"))
        .await
        .expect("取消后连接应可继续使用");
    assert!(start.first_batch.is_some());
}

#[tokio::test]
async fn wrong_password_is_reported_as_auth_failure() {
    let Some((params, _)) = connection_params() else {
        return;
    };
    let driver = MariaDbDriver::new();
    let wrong = sqarvo_driver_api::Secret::new("definitely-not-the-password");
    let error = driver
        .connect(&params, Some(&wrong))
        .await
        .err()
        .expect("错误密码必须失败");
    assert_eq!(error.category(), ErrorCategory::Unauthenticated);
    assert_eq!(error.code(), error::AUTH_FAILED);
}

#[tokio::test]
async fn unreachable_host_fails_within_timeout() {
    let Some((mut params, secret)) = connection_params() else {
        return;
    };
    // 保留 host，改成一个必然拒绝连接的端口，并把超时压到 500ms。
    params.port = 1;
    params
        .properties
        .insert("connect_timeout_ms".to_owned(), "500".to_owned());
    let driver = MariaDbDriver::new();
    let started = Instant::now();
    let error = driver
        .connect(&params, Some(&secret))
        .await
        .err()
        .expect("不可达端口必须失败");
    assert!(
        matches!(
            error.category(),
            ErrorCategory::Unavailable | ErrorCategory::Timeout
        ),
        "错误分类不符合预期：{error:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn multiple_statements_are_rejected_before_execution() {
    let Some(session) = session().await else {
        return;
    };
    session
        .execute(request("DROP TABLE IF EXISTS sqarvo_t_multi"))
        .await
        .unwrap();
    let error = session
        .execute(request("SELECT 1; CREATE TABLE sqarvo_t_multi (v INT)"))
        .await
        .unwrap_err();
    assert_eq!(error.category(), ErrorCategory::Unsupported);

    // 第二条语句绝不能被执行：表不应存在。
    let check = session
        .execute(request(
            "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_name = 'sqarvo_t_multi'",
        ))
        .await
        .unwrap();
    let count = uint_at(&check.first_batch.expect("应有计数").rows[0]);
    assert_eq!(count, 0, "多条语句被部分执行了");
}

#[tokio::test]
async fn types_keep_precision_and_zero_dates_stay_unsupported() {
    let Some(session) = session().await else {
        return;
    };
    session
        .execute(request("DROP TABLE IF EXISTS sqarvo_t_types"))
        .await
        .unwrap();
    session
        .execute(request(
            "CREATE TABLE sqarvo_t_types (
                ub BIGINT UNSIGNED, dec_v DECIMAL(20,6), dt DATETIME(3),
                tm TIME(6), bt BIT(8), en ENUM('a','b'), js JSON, bl BLOB
             )",
        ))
        .await
        .unwrap();
    session
        .execute(request(
            "INSERT INTO sqarvo_t_types VALUES
             (18446744073709551615, '12345678901234.123456', '2026-11-04 12:00:00.123',
              '12:34:56.123456', b'10101010', 'b', '{\"k\":1}', x'00ff')",
        ))
        .await
        .unwrap();

    let start = session
        .execute(request("SELECT * FROM sqarvo_t_types"))
        .await
        .unwrap();
    let row = &start.first_batch.expect("应有结果").rows[0];

    assert_eq!(row[0].value, Value::UInt(18_446_744_073_709_551_615));
    match &row[1].value {
        Value::Decimal(decimal) => assert_eq!(decimal.to_string(), "12345678901234.123456"),
        other => panic!("DECIMAL 未保持精度：{other:?}"),
    }
    assert!(
        matches!(row[2].value, Value::DateTime(_)),
        "DATETIME 应映射为无时区时间戳，实际 {:?}",
        row[2].value
    );
    assert!(matches!(row[3].value, Value::Time(_)));
    assert!(
        matches!(row[4].value, Value::Bytes(_)),
        "BIT 按原始字节返回"
    );
    assert_eq!(row[5].value, Value::Text("b".to_owned()));
    // MariaDB 的 JSON 列实际是 longtext，只能如实按文本返回。
    assert_eq!(row[6].value, Value::Text("{\"k\":1}".to_owned()));
    assert!(matches!(row[7].value, Value::Bytes(_)));

    // 零日期在非严格模式下可以写入；驱动必须保留原文而不是伪造日期。
    session
        .execute(request(
            "INSERT INTO sqarvo_t_types (dt) VALUES ('0000-00-00 00:00:00')",
        ))
        .await
        .unwrap();
    let zero = session
        .execute(request(
            "SELECT dt FROM sqarvo_t_types WHERE dt = '0000-00-00 00:00:00'",
        ))
        .await
        .unwrap();
    match &zero.first_batch.expect("应有结果").rows[0][0].value {
        Value::Unsupported { type_name, .. } => assert_eq!(type_name, "datetime"),
        other => panic!("零日期应标记为 Unsupported，实际 {other:?}"),
    }
}

#[tokio::test]
async fn close_is_idempotent_and_session_rejects_use_after_close() {
    let Some(session) = session().await else {
        return;
    };
    session.close().await.unwrap();
    session.close().await.unwrap();
    let error = session.execute(request("SELECT 1")).await.unwrap_err();
    assert_eq!(error.code(), error::SESSION_CLOSED);
}

fn request(sql: &str) -> ExecuteRequest {
    ExecuteRequest {
        query_id: QueryId::new(),
        sql: sql.to_owned(),
        max_rows: None,
        timeout: None,
    }
}

fn uint_at(row: &sqarvo_driver_api::Row) -> u64 {
    match &row[0].value {
        Value::UInt(value) => *value,
        Value::Int(value) => u64::try_from(*value).expect("非负整数"),
        other => panic!("期望整数，实际 {other:?}"),
    }
}

async fn session() -> Option<Box<dyn DriverSession>> {
    let (params, secret) = connection_params()?;
    let driver = MariaDbDriver::new();
    Some(
        driver
            .connect(&params, Some(&secret))
            .await
            .expect("连接 MariaDB 测试库"),
    )
}

/// 从 `SQARVO_MARIADB_URL` 构造连接参数；未设置时打印跳过原因。
fn connection_params() -> Option<(ConnectionParams, sqarvo_driver_api::Secret)> {
    let Ok(url) = std::env::var("SQARVO_MARIADB_URL") else {
        eprintln!("跳过：未设置 SQARVO_MARIADB_URL（示例 mysql://user:pass@127.0.0.1:53306/db）");
        return None;
    };
    let rest = url
        .strip_prefix("mysql://")
        .expect("SQARVO_MARIADB_URL 必须以 mysql:// 开头");
    let (auth, authority) = rest.split_once('@').expect("URL 缺少 @");
    let (user, password) = auth.split_once(':').expect("URL 缺少密码");
    let (endpoint, database) = match authority.split_once('/') {
        Some((endpoint, database)) => (endpoint, Some(database.to_owned())),
        None => (authority, None),
    };
    let (host_name, port_number) = match endpoint.split_once(':') {
        Some((name, port)) => (name.to_owned(), port.parse().expect("端口必须是数字")),
        None => (endpoint.to_owned(), 3306),
    };

    let params = ConnectionParams {
        driver_id: "mariadb".parse().unwrap(),
        host: host_name,
        port: port_number,
        database,
        username: Some(user.to_owned()),
        credential_ref: Some("sqarvo:conn/integration".to_owned()),
        tls: TlsMode::Preferred,
        tls_server_name: None,
        properties: BTreeMap::new(),
    };
    Some((params, sqarvo_driver_api::Secret::new(password)))
}
