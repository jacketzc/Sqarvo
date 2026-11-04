//! SQLite 类型映射与截断测试（M2 #14）。
//!
//! SQLite 只有 5 种存储类（NULL / INTEGER / REAL / TEXT / BLOB），声明类型只是
//! 亲和性提示。这些测试固定住三条不能退让的规则：
//!
//! 1. NULL、空字符串、0 必须严格可区分；
//! 2. 值按**实际存储类**映射，不按声明类型伪造 Decimal 或时间戳；
//! 3. 超出 `DEFAULT_MAX_CELL_BYTES` 的文本/二进制只传前缀并置 `truncated`，
//!    文本截断后仍是合法 UTF-8。
//!
//! 用例走公开的 [`Driver`] / [`DriverSession`] 路径，与 gRPC 层使用同一条通路。

use std::collections::BTreeMap;
use std::path::Path;

use sqarvo_common::QueryId;
use sqarvo_driver_api::{
    ColumnType, ConnectionParams, DEFAULT_MAX_CELL_BYTES, Driver, DriverSession, ExecuteRequest,
    ExecutionStart, Row, StatementSummary, TlsMode, Value,
};
use sqarvo_driver_sqlite::SqliteDriver;

fn params_for(path: &Path) -> ConnectionParams {
    ConnectionParams {
        driver_id: "sqlite".parse().expect("sqlite 是合法驱动标识"),
        host: path.display().to_string(),
        port: 0,
        database: None,
        username: None,
        credential_ref: None,
        tls: TlsMode::Unspecified,
        tls_server_name: None,
        properties: BTreeMap::new(),
    }
}

struct TestSession {
    _dir: tempfile::TempDir,
    session: Box<dyn DriverSession>,
}

impl TestSession {
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("临时目录");
        let driver = SqliteDriver::new();
        let session = driver
            .connect(&params_for(&dir.path().join("types.db")), None)
            .await
            .expect("连接临时 SQLite 库");
        Self { _dir: dir, session }
    }

    async fn execute(&self, sql: &str) -> ExecutionStart {
        self.session
            .execute(ExecuteRequest {
                query_id: QueryId::new(),
                sql: sql.to_owned(),
                max_rows: None,
                timeout: None,
            })
            .await
            .unwrap_or_else(|error| panic!("执行 `{sql}` 失败：{error}（{})", error.code()))
    }

    /// 执行查询并返回首批行（用例数据均小于默认批次）。
    async fn query(&self, sql: &str) -> (StatementSummary, Vec<Row>) {
        let start = self.execute(sql).await;
        let summary = start.statements.first().expect("至少一条语句").clone();
        let rows = start
            .first_batch
            .map(|batch| batch.rows)
            .unwrap_or_default();
        (summary, rows)
    }
}

fn text_at(row: &Row, index: usize) -> String {
    match &row[index].value {
        Value::Text(text) => text.clone(),
        other => panic!("期望 TEXT，实际 {other:?}"),
    }
}

#[tokio::test]
async fn null_empty_text_and_zero_are_distinct() {
    let session = TestSession::new().await;
    session
        .execute("CREATE TABLE t(a TEXT, b INTEGER, c BLOB)")
        .await;
    session
        .execute("INSERT INTO t(a, b, c) VALUES (NULL, 0, x'')")
        .await;
    session
        .execute("INSERT INTO t(a, b) VALUES ('', NULL)")
        .await;

    let (_summary, rows) = session.query("SELECT a, b, c FROM t ORDER BY rowid").await;
    assert_eq!(rows.len(), 2);

    // 第一行：NULL / 0 / 空 BLOB，三者互不相等。
    assert_eq!(rows[0][0].value, Value::Null);
    assert!(rows[0][0].is_null());
    assert_eq!(rows[0][1].value, Value::Int(0));
    assert_eq!(rows[0][2].value, Value::Bytes(Vec::new()));
    assert_ne!(rows[0][0].value, Value::Text(String::new()));

    // 第二行：空字符串 / NULL / NULL。
    assert_eq!(rows[0][1].value, Value::Int(0));
    assert_eq!(text_at(&rows[1], 0), "");
    assert_ne!(rows[1][0].value, rows[0][0].value);
    assert_eq!(rows[1][1].value, Value::Null);
    assert_eq!(rows[1][2].value, Value::Null);
    assert!(!rows[1][0].truncated);
}

#[tokio::test]
async fn integer_bounds_roundtrip_without_loss() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v INTEGER)").await;
    // `-9223372036854775808` 在 SQLite 里会被解析成正号溢出为 REAL，
    // 用 `-MAX - 1` 得到 i64 最小值，避免测试自身引入精度问题。
    session
        .execute(&format!(
            "INSERT INTO t(v) VALUES (-9223372036854775807 - 1), ({}), (-1), (0)",
            i64::MAX
        ))
        .await;

    let (summary, rows) = session.query("SELECT v FROM t ORDER BY v").await;
    assert_eq!(
        summary.columns[0].original_type_name.as_deref(),
        Some("INTEGER")
    );
    assert_eq!(
        summary.columns[0].column_type,
        ColumnType::Integer {
            bits: 64,
            unsigned: false
        }
    );

    let values: Vec<Value> = rows.iter().map(|row| row[0].value.clone()).collect();
    assert_eq!(
        values,
        vec![
            Value::Int(i64::MIN),
            Value::Int(-1),
            Value::Int(0),
            Value::Int(i64::MAX)
        ]
    );
}

#[tokio::test]
async fn real_maps_to_float_not_text() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v REAL)").await;
    session
        .execute("INSERT INTO t(v) VALUES (1.5), (-0.25), (3.0)")
        .await;

    let (summary, rows) = session.query("SELECT v FROM t ORDER BY v").await;
    assert_eq!(
        summary.columns[0].column_type,
        ColumnType::Float { bits: 64 }
    );
    assert_eq!(rows[0][0].value, Value::Float(-0.25));
    assert_eq!(rows[1][0].value, Value::Float(1.5));
    // 整数值的 REAL 仍是浮点，不得被「顺手」改成整数或字符串。
    assert_eq!(rows[2][0].value, Value::Float(3.0));
    assert!(
        rows.iter()
            .all(|row| !matches!(row[0].value, Value::Text(_)))
    );
}

#[tokio::test]
async fn numeric_column_keeps_declared_type_and_maps_by_storage_class() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v NUMERIC)").await;
    // 同一个 NUMERIC 列里可以分别存 INTEGER / REAL / TEXT。
    session
        .execute("INSERT INTO t(v) VALUES (42), (1.25), ('12abc')")
        .await;

    let (summary, rows) = session.query("SELECT v FROM t ORDER BY rowid").await;
    let column = &summary.columns[0];
    assert_eq!(column.original_type_name.as_deref(), Some("NUMERIC"));
    assert_eq!(
        column.column_type,
        ColumnType::Unsupported {
            type_name: "NUMERIC".to_owned()
        },
        "NUMERIC 不得被伪造成 Decimal"
    );

    assert_eq!(rows[0][0].value, Value::Int(42));
    assert_eq!(rows[1][0].value, Value::Float(1.25));
    // 不能被亲和性转成数字的文本保持 TEXT。
    assert_eq!(text_at(&rows[2], 0), "12abc");
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row[0].value, Value::Decimal(_)))
    );
}

#[tokio::test]
async fn high_precision_numeric_text_is_not_converted_to_float() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v TEXT)").await;
    session
        .execute("INSERT INTO t(v) VALUES ('1.2345678901234567890')")
        .await;

    let (_summary, rows) = session.query("SELECT v FROM t").await;
    assert_eq!(
        text_at(&rows[0], 0),
        "1.2345678901234567890",
        "高精度十进制文本不得被转成 f64 丢失精度"
    );
}

#[tokio::test]
async fn date_like_columns_return_text_with_original_type_name() {
    let session = TestSession::new().await;
    session
        .execute("CREATE TABLE t(d DATE, ts DATETIME, flag BOOLEAN)")
        .await;
    session
        .execute("INSERT INTO t(d, ts, flag) VALUES ('2026-11-04', '2026-11-04 12:34:56', 1)")
        .await;

    let (summary, rows) = session.query("SELECT d, ts, flag FROM t").await;
    // 声明类型原样保留，且不猜测解析成日期/时间戳。
    assert_eq!(
        summary.columns[0].original_type_name.as_deref(),
        Some("DATE")
    );
    assert_eq!(
        summary.columns[0].column_type,
        ColumnType::Unsupported {
            type_name: "DATE".to_owned()
        }
    );
    assert_eq!(
        summary.columns[1].original_type_name.as_deref(),
        Some("DATETIME")
    );
    assert_eq!(text_at(&rows[0], 0), "2026-11-04");
    assert_eq!(text_at(&rows[0], 1), "2026-11-04 12:34:56");
    assert_eq!(rows[0][2].value, Value::Int(1));
}

#[tokio::test]
async fn binary_column_roundtrips_bytes() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v BLOB)").await;
    session
        .execute("INSERT INTO t(v) VALUES (x'00ff10'), (x''), (NULL)")
        .await;

    let (summary, rows) = session.query("SELECT v FROM t ORDER BY rowid").await;
    assert_eq!(
        summary.columns[0].column_type,
        ColumnType::Binary { length: None }
    );
    assert_eq!(rows[0][0].value, Value::Bytes(vec![0x00, 0xff, 0x10]));
    assert_eq!(rows[1][0].value, Value::Bytes(Vec::new()));
    assert_eq!(rows[2][0].value, Value::Null);
}

#[tokio::test]
async fn long_text_is_truncated_on_char_boundary() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v TEXT)").await;

    // 3 字节/字符，总长超过上限，且上限不一定落在字符边界。
    let long_text = "数".repeat(DEFAULT_MAX_CELL_BYTES / 3 + 10);
    session
        .execute(&format!("INSERT INTO t(v) VALUES ('{long_text}')"))
        .await;

    let (_summary, rows) = session.query("SELECT v FROM t").await;
    let cell = &rows[0][0];
    assert!(cell.truncated, "超长文本必须标记 truncated");

    let Value::Text(value) = &cell.value else {
        panic!("超长文本仍应是 TEXT，实际 {:?}", cell.value);
    };
    assert!(value.len() <= DEFAULT_MAX_CELL_BYTES);
    // `String` 已保证合法 UTF-8；再显式确认没有落到半个字符上。
    assert!(long_text.starts_with(value.as_str()));
    assert_eq!(value.chars().count(), DEFAULT_MAX_CELL_BYTES / 3);
}

#[tokio::test]
async fn short_text_is_not_marked_truncated() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v TEXT)").await;
    // 正好等于上限：不截断。
    let exact = "a".repeat(DEFAULT_MAX_CELL_BYTES);
    session
        .execute(&format!("INSERT INTO t(v) VALUES ('{exact}')"))
        .await;

    let (_summary, rows) = session.query("SELECT v FROM t").await;
    let cell = &rows[0][0];
    assert!(!cell.truncated);
    assert_eq!(text_at(&rows[0], 0).len(), DEFAULT_MAX_CELL_BYTES);
}

#[tokio::test]
async fn long_blob_is_truncated_and_flagged() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v BLOB)").await;
    session
        .execute(&format!(
            "INSERT INTO t(v) VALUES (zeroblob({}))",
            DEFAULT_MAX_CELL_BYTES + 1024
        ))
        .await;

    let (_summary, rows) = session.query("SELECT v FROM t").await;
    let cell = &rows[0][0];
    assert!(cell.truncated, "超长二进制必须标记 truncated");
    let Value::Bytes(bytes) = &cell.value else {
        panic!("期望 BLOB，实际 {:?}", cell.value);
    };
    assert_eq!(bytes.len(), DEFAULT_MAX_CELL_BYTES);
    assert!(bytes.iter().all(|byte| *byte == 0));
}

#[tokio::test]
async fn invalid_utf8_text_is_preserved_as_bytes() {
    let session = TestSession::new().await;
    session.execute("CREATE TABLE t(v TEXT)").await;
    // SQLite 不强制 TEXT 为 UTF-8；这里写入非法字节序列。
    session
        .execute("INSERT INTO t(v) VALUES (CAST(x'fffe61' AS TEXT))")
        .await;

    let (_summary, rows) = session.query("SELECT v FROM t").await;
    // 不得用 U+FFFD 静默替换（那是有损的）：按原始字节返回。
    assert_eq!(
        rows[0][0].value,
        Value::Bytes(vec![0xff, 0xfe, 0x61]),
        "非法 UTF-8 的 TEXT 必须无损呈现，不得替换字符"
    );
    assert!(!rows[0][0].truncated);
}

#[tokio::test]
async fn expression_columns_report_unsupported_type_but_typed_values() {
    let session = TestSession::new().await;
    let (summary, rows) = session.query("SELECT 1 + 1 AS total").await;
    assert_eq!(
        summary.columns[0].column_type,
        ColumnType::Unsupported {
            type_name: String::new()
        },
        "表达式没有声明类型，不得凭空编造型"
    );
    assert_eq!(rows[0][0].value, Value::Int(2));
}
