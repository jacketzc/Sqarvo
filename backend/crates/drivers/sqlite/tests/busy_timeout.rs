//! 连接建立时的 `PRAGMA busy_timeout`（ADR 0021 决策 5，#110）。
//!
//! 两条证据：
//!
//! 1. 新连接的 `busy_timeout` 就是契约要求的 5000 ms（显式设置，不依赖上游默认值）；
//! 2. 两个连接打开同一个文件，一个持有写锁时，另一个**有界等待后**才失败
//!    （`sqlite.busy`），而不是立刻失败；释放锁后同一连接可以正常写入。

use std::time::{Duration, Instant};

use sqarvo_common::{ErrorCode, QueryId};
use sqarvo_driver_api::{ConnectionParams, Driver, DriverSession, ExecuteRequest, TlsMode, Value};
use sqarvo_driver_sqlite::SqliteDriver;
use tempfile::TempDir;

/// 契约要求的忙等待上限（毫秒）。
const BUSY_TIMEOUT_MS: i64 = 5_000;

async fn connect(path: &std::path::Path) -> Box<dyn DriverSession> {
    let params = ConnectionParams {
        driver_id: "sqlite".parse().unwrap(),
        host: path.display().to_string(),
        port: 0,
        database: None,
        username: None,
        credential_ref: None,
        tls: TlsMode::Unspecified,
        tls_server_name: None,
        properties: std::collections::BTreeMap::new(),
    };
    SqliteDriver::new()
        .connect(&params, None)
        .await
        .expect("连接临时 SQLite 库")
}

async fn exec(session: &dyn DriverSession, sql: &str) -> sqarvo_common::Result<()> {
    session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: sql.to_owned(),
            max_rows: None,
            timeout: None,
        })
        .await
        .map(|_| ())
}

#[tokio::test]
async fn busy_timeout_is_configured_explicitly() {
    let dir: TempDir = tempfile::tempdir().expect("临时目录");
    let session = connect(&dir.path().join("busy-pragma.db")).await;

    let start = session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: "PRAGMA busy_timeout".to_owned(),
            max_rows: None,
            timeout: None,
        })
        .await
        .expect("读取 PRAGMA busy_timeout");
    let batch = start.first_batch.expect("PRAGMA 应返回一行");
    assert_eq!(
        batch.rows[0][0].value,
        Value::Int(BUSY_TIMEOUT_MS),
        "连接建立时必须显式设置 busy_timeout = 5000 ms"
    );

    session.close().await.expect("关闭会话");
}

/// 两个连接同文件、一个持写锁：另一个必须等满预算才失败，而不是立刻失败。
///
/// 失败后释放锁，再写同一张表必须成功——证明失败原因就是锁等待，不是别的错误。
#[tokio::test]
async fn busy_timeout_waits_instead_of_failing_immediately() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("busy-lock.db");

    let writer = connect(&path).await;
    exec(
        writer.as_ref(),
        "CREATE TABLE guard (id INTEGER PRIMARY KEY)",
    )
    .await
    .expect("建表");
    exec(writer.as_ref(), "INSERT INTO guard VALUES (1)")
        .await
        .expect("写入首行");
    // 持有写锁（RESERVED），直到显式回滚。
    exec(writer.as_ref(), "BEGIN IMMEDIATE")
        .await
        .expect("开始写事务");

    let blocked = connect(&path).await;
    let started = Instant::now();
    let error = exec(blocked.as_ref(), "INSERT INTO guard VALUES (2)")
        .await
        .expect_err("写锁被占用时必须失败");
    let waited = started.elapsed();

    assert_eq!(
        error.code(),
        ErrorCode::from_static("sqlite.busy"),
        "锁等待超时必须是 sqlite.busy：{}",
        error.message()
    );
    assert!(
        waited >= Duration::from_secs(4),
        "必须等满接近 5000 ms 的预算，实际只等了 {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(20),
        "等待时间异常长：{waited:?}"
    );

    // 释放锁后同一连接可以写入：失败确实来自锁，而不是表/连接坏了。
    exec(writer.as_ref(), "ROLLBACK").await.expect("回滚");
    exec(blocked.as_ref(), "INSERT INTO guard VALUES (2)")
        .await
        .expect("锁释放后必须能写入");

    writer.close().await.expect("关闭写连接");
    blocked.close().await.expect("关闭被阻塞连接");
}
