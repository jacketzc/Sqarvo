//! 参数化执行（#59）：值经 `rusqlite` 绑定，不拼进 SQL 文本。
//!
//! 这些用例锁定 `Capabilities::parameterized_statements` 在 SQLite 驱动上为 `true`
//! 的实际内容：绑定参数往返、NULL 与空串可区分、无法表示的类型在执行前被拒绝。

use std::collections::BTreeMap;

use sqarvo_common::{ErrorCode, QueryId};
use sqarvo_driver_api::{
    ConnectionParams, Driver, DriverSession, ExecuteRequest, ParameterizedExecuteRequest, TlsMode,
    Value,
};
use sqarvo_driver_sqlite::SqliteDriver;

async fn session() -> (tempfile::TempDir, Box<dyn DriverSession>) {
    let dir = tempfile::tempdir().expect("临时目录");
    let params = ConnectionParams {
        driver_id: "sqlite".parse().unwrap(),
        host: dir.path().join("parameterized.db").display().to_string(),
        port: 0,
        database: None,
        username: None,
        credential_ref: None,
        tls: TlsMode::Unspecified,
        tls_server_name: None,
        properties: BTreeMap::new(),
    };
    let session = SqliteDriver::new()
        .connect(&params, None)
        .await
        .expect("连接临时 SQLite 库");
    (dir, session)
}

async fn exec(session: &dyn DriverSession, sql: &str) {
    session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: sql.to_owned(),
            max_rows: None,
            timeout: None,
        })
        .await
        .unwrap_or_else(|error| panic!("执行失败：{sql}\n{error:?}"));
}

fn parameterized(sql: &str, params: Vec<Value>) -> ParameterizedExecuteRequest {
    ParameterizedExecuteRequest {
        query_id: QueryId::new(),
        sql: sql.to_owned(),
        params,
        max_rows: None,
        timeout: None,
    }
}

#[tokio::test]
async fn driver_declares_and_implements_parameterized_statements() {
    let (_dir, session) = session().await;
    assert!(session.supports_parameterized_statements());
    session.close().await.unwrap();
}

#[tokio::test]
async fn values_are_bound_not_spliced() {
    let (_dir, session) = session().await;
    exec(
        &*session,
        "CREATE TABLE notes(id INTEGER NOT NULL, body TEXT)",
    )
    .await;

    // 带引号、分号与反斜杠：拼接 SQL 会在这里出问题。
    let tricky = "o'brien\\x\"; DROP TABLE notes; --";
    let start = session
        .execute_parameterized(parameterized(
            "INSERT INTO notes(id, body) VALUES (?, ?)",
            vec![Value::Int(1), Value::Text(tricky.to_owned())],
        ))
        .await
        .expect("参数化 INSERT");
    assert_eq!(start.statements[0].rows_affected, Some(1));

    let start = session
        .execute_parameterized(parameterized(
            "SELECT body FROM notes WHERE id = ?",
            vec![Value::Int(1)],
        ))
        .await
        .expect("参数化 SELECT");
    let rows = start.first_batch.expect("首批行").rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].value, Value::Text(tricky.to_owned()));
    session.close().await.unwrap();
}

#[tokio::test]
async fn null_is_a_valid_bound_value() {
    let (_dir, session) = session().await;
    exec(&*session, "CREATE TABLE t(id INTEGER NOT NULL, v TEXT)").await;
    session
        .execute_parameterized(parameterized(
            "INSERT INTO t(id, v) VALUES (?, ?)",
            vec![Value::Int(1), Value::Null],
        ))
        .await
        .expect("绑定 NULL");
    session
        .execute_parameterized(parameterized(
            "INSERT INTO t(id, v) VALUES (?, ?)",
            vec![Value::Int(2), Value::Text(String::new())],
        ))
        .await
        .expect("绑定空串");

    let start = session
        .execute_parameterized(parameterized("SELECT v FROM t ORDER BY id", Vec::new()))
        .await
        .expect("读取");
    let rows = start.first_batch.expect("首批行").rows;
    assert!(rows[0][0].is_null(), "NULL 必须保持 NULL");
    assert_eq!(
        rows[1][0].value,
        Value::Text(String::new()),
        "空串与 NULL 不能混淆"
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn unsupported_values_are_rejected_before_execution() {
    let (_dir, session) = session().await;
    let error = session
        .execute_parameterized(parameterized(
            "SELECT ?",
            vec![Value::Unsupported {
                type_name: "geometry".to_owned(),
                text: "POLYGON((0 0,1 1))".to_owned(),
            }],
        ))
        .await
        .expect_err("Unsupported 值不得被绑定");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert!(
        !error.message().contains("POLYGON"),
        "错误文案不得包含值内容：{error}"
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn parameterized_execution_respects_the_open_cursor_rule() {
    let (_dir, session) = session().await;
    exec(&*session, "CREATE TABLE many(id INTEGER NOT NULL)").await;
    exec(
        &*session,
        "INSERT INTO many(id) VALUES (1),(2),(3),(4),(5),(6),(7),(8),(9),(10)",
    )
    .await;

    let first = session
        .execute_parameterized(ParameterizedExecuteRequest {
            query_id: QueryId::new(),
            sql: "SELECT id FROM many".to_owned(),
            params: Vec::new(),
            max_rows: Some(2),
            timeout: None,
        })
        .await
        .expect("分批读取");
    assert!(first.first_batch.expect("首批").has_more);

    // 有在途游标时再发参数化语句：与普通执行同一条规则，显式拒绝而不是交错。
    let error = session
        .execute_parameterized(parameterized("SELECT 1", Vec::new()))
        .await
        .expect_err("游标未读完时必须拒绝新语句");
    assert_eq!(error.code().as_str(), "sqlite.query_active");
    session.close().await.unwrap();
}
