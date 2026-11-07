//! SQLite 表数据按 `ObjectRef.catalog` 定位（ADR 0024 / #140）。
//!
//! 覆盖：
//!
//! 1. `catalog = "temp"` 时浏览 / 行变更 / 插入都落在 `temp`，`main` 的同名表不受影响；
//! 2. `catalog` 缺省或 `"main"` 时行为与 v0.2.3 完全一致（回落 `main`）；
//! 3. `main` / `temp` 之外的 catalog 明确报 `capability.unsupported`，
//!    **不再静默按 `main` 处理**（静默降级会把用户请求的库换成另一个库）。
//!
//! 两张同名表 `probe` 分别建在 `main` 与 `temp`：不限定库名的 SQL 会按 SQLite 的
//! 搜索顺序落到其中一张，因此「读/写到了哪张表」从行内容就能直接看出来。

use std::collections::BTreeMap;

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    BrowseOptions, ColumnChange, ConnectionParams, Driver, DriverSession, ExecuteRequest,
    InsertRow, InsertRowsRequest, ObjectKind, ObjectRef, PageRequest, QualifiedName, Row,
    RowChange, RowChangeCancel, RowChangeRequest, RowKey, TableDataProvider, TlsMode, Value,
};
use sqarvo_driver_sqlite::SqliteDriver;
use tempfile::TempDir;

const TABLE: &str = "probe";

async fn session() -> (TempDir, Box<dyn DriverSession>) {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("cross-catalog.db");
    let params = ConnectionParams {
        driver_id: "sqlite".parse().unwrap(),
        host: path.display().to_string(),
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

/// 建主库与临时库的同名表，内容不同：`main` 是 1..=2，`temp` 是 10..=12。
async fn prepare(session: &dyn DriverSession) {
    exec(session, "DROP TABLE IF EXISTS probe").await;
    exec(
        session,
        "CREATE TABLE probe (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
    )
    .await;
    exec(session, "INSERT INTO probe VALUES (1,'m1'),(2,'m2')").await;
    exec(session, "DROP TABLE IF EXISTS temp.probe").await;
    exec(
        session,
        "CREATE TEMP TABLE probe (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
    )
    .await;
    exec(
        session,
        "INSERT INTO temp.probe VALUES (10,'t10'),(11,'t11'),(12,'t12')",
    )
    .await;
}

fn object(session: &dyn DriverSession, catalog: Option<&str>) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(catalog.map(str::to_owned), None, TABLE),
        kind: ObjectKind::Table,
    }
}

fn provider(session: &dyn DriverSession) -> &dyn TableDataProvider {
    session.table_data().expect("SQLite 必须提供表数据能力")
}

async fn browse(
    session: &dyn DriverSession,
    catalog: Option<&str>,
) -> sqarvo_common::Result<Vec<Row>> {
    provider(session)
        .browse(
            &object(session, catalog),
            &PageRequest {
                page_size: 100,
                page_token: None,
            },
            &BrowseOptions::default(),
        )
        .await
        .map(|page| page.rows)
}

fn ids(rows: &[Row]) -> Vec<i64> {
    rows.iter()
        .map(|row| match row[0].value {
            Value::Int(value) => value,
            ref other => panic!("期望整型主键，实际 {other:?}"),
        })
        .collect()
}

fn text(rows: &[Row], id: i64) -> String {
    let row = rows
        .iter()
        .find(|row| row[0].value == Value::Int(id))
        .unwrap_or_else(|| panic!("找不到主键 {id} 的行"));
    match &row[1].value {
        Value::Text(value) => value.clone(),
        other => panic!("期望文本列，实际 {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 浏览
// ---------------------------------------------------------------------------

#[tokio::test]
async fn browse_reads_the_requested_catalog() {
    let (_dir, session) = session().await;
    prepare(&*session).await;

    let temp = browse(&*session, Some("temp")).await.expect("浏览 temp");
    assert_eq!(ids(&temp), vec![10, 11, 12], "catalog=temp 必须读 temp 库");
    assert_eq!(text(&temp, 10), "t10");

    let main = browse(&*session, Some("main")).await.expect("浏览 main");
    assert_eq!(ids(&main), vec![1, 2], "catalog=main 必须读 main 库");

    // 缺省与空串都回落到 main（v0.2.3 行为不变）。
    for catalog in [None, Some("")] {
        let rows = browse(&*session, catalog)
            .await
            .expect("catalog 缺省必须回落 main");
        assert_eq!(ids(&rows), vec![1, 2], "catalog={catalog:?} 必须回落 main");
    }

    // 大小写不敏感：库名是标识符，SQLite 的 database 名不区分大小写。
    let upper = browse(&*session, Some("TEMP")).await.expect("浏览 TEMP");
    assert_eq!(ids(&upper), vec![10, 11, 12]);
}

// ---------------------------------------------------------------------------
// 行变更
// ---------------------------------------------------------------------------

#[tokio::test]
async fn row_changes_target_the_requested_catalog() {
    let (_dir, session) = session().await;
    prepare(&*session).await;

    provider(&*session)
        .apply_row_changes(
            RowChangeRequest {
                object: object(&*session, Some("temp")),
                changes: vec![RowChange::Update {
                    key: RowKey::new(vec![Value::Int(10)]),
                    changes: vec![ColumnChange {
                        column: "name".to_owned(),
                        value: Value::Text("edited".to_owned()),
                    }],
                    expected_rows_affected: 0,
                }],
            },
            &RowChangeCancel::new(),
        )
        .await
        .expect("更新 temp.probe");

    let temp = browse(&*session, Some("temp")).await.expect("浏览 temp");
    assert_eq!(text(&temp, 10), "edited", "写入必须落在 temp");

    let main = browse(&*session, Some("main")).await.expect("浏览 main");
    assert_eq!(ids(&main), vec![1, 2], "main 的行数不能变");
    assert_eq!(text(&main, 1), "m1", "main 的内容不能变");
    assert_eq!(text(&main, 2), "m2", "main 的内容不能变");
}

// ---------------------------------------------------------------------------
// 插入
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inserts_target_the_requested_catalog() {
    let (_dir, session) = session().await;
    prepare(&*session).await;

    provider(&*session)
        .insert_rows(InsertRowsRequest {
            object: object(&*session, Some("temp")),
            rows: vec![InsertRow::new(vec![ColumnChange {
                column: "name".to_owned(),
                value: Value::Text("new".to_owned()),
            }])],
        })
        .await
        .expect("插入 temp.probe");

    let temp = browse(&*session, Some("temp")).await.expect("浏览 temp");
    assert_eq!(temp.len(), 4, "temp.probe 必须多一行");
    let main = browse(&*session, Some("main")).await.expect("浏览 main");
    assert_eq!(ids(&main), vec![1, 2], "main.probe 不能多出插入的行");
}

// ---------------------------------------------------------------------------
// 不支持的 catalog
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_catalog_is_unsupported_on_every_table_data_path() {
    let (_dir, session) = session().await;
    prepare(&*session).await;

    let errors = [
        provider(&*session)
            .browse(
                &object(&*session, Some("other")),
                &PageRequest {
                    page_size: 10,
                    page_token: None,
                },
                &BrowseOptions::default(),
            )
            .await
            .expect_err("浏览 other 必须报错"),
        provider(&*session)
            .apply_row_changes(
                RowChangeRequest {
                    object: object(&*session, Some("other")),
                    changes: vec![RowChange::Delete {
                        key: RowKey::new(vec![Value::Int(1)]),
                        expected_rows_affected: 0,
                    }],
                },
                &RowChangeCancel::new(),
            )
            .await
            .expect_err("行变更 other 必须报错"),
        provider(&*session)
            .insert_rows(InsertRowsRequest {
                object: object(&*session, Some("other")),
                rows: vec![InsertRow::new(vec![ColumnChange {
                    column: "name".to_owned(),
                    value: Value::Text("x".to_owned()),
                }])],
            })
            .await
            .expect_err("插入 other 必须报错"),
    ];

    for error in errors {
        assert_eq!(
            error.code(),
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "不支持的库必须报 capability.unsupported，实际：{error}"
        );
        assert_eq!(error.category(), ErrorCategory::Unsupported);
        assert!(
            error.to_string().contains("main / temp"),
            "错误文案必须说明只支持 main / temp：{error}"
        );
    }

    // 拒绝必须发生在发 SQL 之前：两张表都完好无损。
    assert_eq!(
        ids(&browse(&*session, Some("main")).await.expect("浏览 main")),
        vec![1, 2]
    );
    assert_eq!(
        ids(&browse(&*session, Some("temp")).await.expect("浏览 temp")),
        vec![10, 11, 12]
    );
}
