//! SQLite 插入新行（#85，v0.2.2）：自增回传、默认值、冲突与批量原子性。
//!
//! 断言方式与 `row_edits.rs` 一致：SQL 侧用**原生查询**复核库里到底有什么，
//! 不拿驱动自己的浏览结果当唯一证据（否则「驱动回滚了但库里没回滚」这类 bug
//! 会被自己的读路径掩盖）。
//!
//! 覆盖：
//!
//! 1. 自增主键回传（`last_insert_rowid` + 回读），显式主键原样回传；
//! 2. 省略列 → 数据库默认值，显式 `VALUE_KIND_NULL` → 写入 SQL NULL（两者可区分）；
//! 3. 全默认值插入（`INSERT INTO t DEFAULT VALUES`）；
//! 4. NOT NULL 两种错误码：显式 NULL（`not_null_violation`）与省略无默认值
//!    （`not_null_without_default`）；
//! 5. 唯一冲突：逐行报 `unique_violation`，其余行继续执行，最后整批回滚；
//! 6. 批量部分失败 → `rolled_back = true`、净影响 0，外部 SQL 复核库里 0 行变化；
//! 7. 事务模式用 savepoint：批失败后外层事务仍可用（可继续写、可提交/回滚）；
//! 8. 类型位级往返（Decimal 文本 / 二进制 / 时间戳文本 / NULL）；
//! 9. 拒绝：视图、生成列、未知列、重复列、空批次、`Value::Unsupported`。

use std::collections::BTreeMap;

use sqarvo_common::{ErrorCode, QueryId};
use sqarvo_driver_api::{
    ColumnChange, ConnectionParams, Decimal, Driver, DriverSession, ExecuteRequest, InsertOutcome,
    InsertRow, InsertRowsRequest, IsolationLevel, ObjectKind, ObjectRef, PageRequest,
    QualifiedName, Row, TableDataProvider, TlsMode, TransactionOptions, Value,
};
use sqarvo_driver_sqlite::SqliteDriver;
use tempfile::TempDir;

async fn session() -> (TempDir, Box<dyn DriverSession>) {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("insert-rows.db");
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

/// 原生查询（绕过驱动表数据路径）：返回首批行。
async fn select(session: &dyn DriverSession, sql: &str) -> Vec<Row> {
    session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: sql.to_owned(),
            max_rows: None,
            timeout: None,
        })
        .await
        .unwrap_or_else(|error| panic!("查询失败：{sql}\n{error:?}"))
        .first_batch
        .map(|batch| batch.rows)
        .unwrap_or_default()
}

/// 原生 `SELECT COUNT(*)`——证明「库里到底有没有行」，不是驱动自己的说法。
async fn count_rows(session: &dyn DriverSession, table: &str) -> u64 {
    let rows = select(session, &format!("SELECT COUNT(*) FROM {table}")).await;
    match rows[0][0].value {
        Value::Int(value) => u64::try_from(value).expect("计数非负"),
        ref other => panic!("期望整数计数，实际 {other:?}"),
    }
}

fn object(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, name),
        kind: ObjectKind::Table,
    }
}

fn view(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        kind: ObjectKind::View,
        ..object(session, name)
    }
}

fn provider(session: &dyn DriverSession) -> &dyn TableDataProvider {
    session.table_data().expect("SQLite 必须提供表数据能力")
}

fn value(column: &str, value: Value) -> ColumnChange {
    ColumnChange {
        column: column.to_owned(),
        value,
    }
}

fn row(values: Vec<ColumnChange>) -> InsertRow {
    InsertRow::new(values)
}

async fn insert(
    session: &dyn DriverSession,
    name: &str,
    rows: Vec<InsertRow>,
) -> sqarvo_common::Result<sqarvo_driver_api::InsertReport> {
    insert_object(session, &object(session, name), rows).await
}

async fn insert_object(
    session: &dyn DriverSession,
    object: &ObjectRef,
    rows: Vec<InsertRow>,
) -> sqarvo_common::Result<sqarvo_driver_api::InsertReport> {
    provider(session)
        .insert_rows(InsertRowsRequest {
            object: object.clone(),
            rows,
        })
        .await
}

async fn browse(session: &dyn DriverSession, name: &str) -> Vec<Row> {
    provider(session)
        .browse(
            &object(session, name),
            &PageRequest {
                page_size: 500,
                page_token: None,
            },
        )
        .await
        .expect("浏览表")
        .rows
}

fn key_of(result: &sqarvo_driver_api::InsertRowResult) -> &Value {
    &result
        .key
        .as_ref()
        .unwrap_or_else(|| panic!("成功行必须回传主键：{result:?}"))
        .values[0]
}

#[tokio::test]
async fn autoincrement_key_is_returned_and_omitted_columns_use_defaults() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE items (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL, \
            label TEXT NOT NULL DEFAULT 'd', \
            note TEXT DEFAULT 'n'\
         )",
    )
    .await;

    let report = insert(
        session.as_ref(),
        "items",
        vec![
            row(vec![value("name", Value::Text("a".to_owned()))]),
            // 显式 NULL 是「写 NULL」，不是「用默认值 'n'」。
            row(vec![
                value("name", Value::Text("b".to_owned())),
                value("note", Value::Null),
            ]),
        ],
    )
    .await
    .expect("插入两行");

    assert!(!report.rolled_back, "全部成功时不得置 rolled_back");
    assert_eq!(report.inserted_rows(), 2);
    assert_eq!(report.rows_affected(), 2);
    assert_eq!(key_of(&report.results[0]), &Value::Int(1));
    assert_eq!(key_of(&report.results[1]), &Value::Int(2));

    let rows = select(
        session.as_ref(),
        "SELECT id, name, label, note FROM items ORDER BY id",
    )
    .await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0][2].value,
        Value::Text("d".to_owned()),
        "省略列取默认值"
    );
    assert_eq!(rows[0][3].value, Value::Text("n".to_owned()));
    assert_eq!(rows[1][2].value, Value::Text("d".to_owned()));
    assert_eq!(
        rows[1][3].value,
        Value::Null,
        "显式 NULL 必须写成 SQL NULL，不能退化成默认值"
    );

    // 自增列继续递增：第一条语句确实写进了库，不是「回传了 1 但没落库」。
    let more = insert(
        session.as_ref(),
        "items",
        vec![row(vec![value("name", Value::Text("c".to_owned()))])],
    )
    .await
    .expect("再插一行");
    assert_eq!(key_of(&more.results[0]), &Value::Int(3));
}

#[tokio::test]
async fn explicit_primary_key_is_echoed_and_continues_the_sequence() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE keyed (id INTEGER PRIMARY KEY, v TEXT NOT NULL)",
    )
    .await;

    let report = insert(
        session.as_ref(),
        "keyed",
        vec![row(vec![
            value("id", Value::Int(42)),
            value("v", Value::Text("x".to_owned())),
        ])],
    )
    .await
    .expect("显式主键插入");
    assert_eq!(key_of(&report.results[0]), &Value::Int(42));

    let next = insert(
        session.as_ref(),
        "keyed",
        vec![row(vec![value("v", Value::Text("y".to_owned()))])],
    )
    .await
    .expect("自增插入");
    assert_eq!(
        key_of(&next.results[0]),
        &Value::Int(43),
        "显式写入 42 之后自增必须从 43 继续"
    );
    assert_eq!(count_rows(session.as_ref(), "keyed").await, 2);
}

#[tokio::test]
async fn all_defaults_row_inserts_without_a_column_list() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE defaults (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            v TEXT NOT NULL DEFAULT 'auto', \
            n INTEGER NOT NULL DEFAULT 7\
         )",
    )
    .await;

    let report = insert(session.as_ref(), "defaults", vec![row(Vec::new())])
        .await
        .expect("全默认值插入");
    assert!(!report.rolled_back);
    assert_eq!(key_of(&report.results[0]), &Value::Int(1));

    let rows = select(session.as_ref(), "SELECT id, v, n FROM defaults").await;
    assert_eq!(rows[0][1].value, Value::Text("auto".to_owned()));
    assert_eq!(rows[0][2].value, Value::Int(7));
}

#[tokio::test]
async fn not_null_violations_have_distinct_codes_and_roll_back() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE strict (id INTEGER PRIMARY KEY, v TEXT NOT NULL, w TEXT)",
    )
    .await;

    // 1) 显式 NULL 写进 NOT NULL 列。
    let report = insert(
        session.as_ref(),
        "strict",
        vec![
            row(vec![
                value("v", Value::Text("ok".to_owned())),
                value("w", Value::Text("w".to_owned())),
            ]),
            row(vec![value("v", Value::Null)]),
        ],
    )
    .await
    .expect("逐行错误不是整批 Err");
    assert!(report.rolled_back, "任一行失败必须整批回滚");
    assert_eq!(report.rows_affected(), 0, "回滚后净影响必须为 0");
    assert_eq!(report.inserted_rows(), 1, "语句层成功条数仍要上报");
    assert_eq!(
        report.results[1].outcome,
        InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_NOT_NULL_VIOLATION
        }
    );
    assert_eq!(
        count_rows(session.as_ref(), "strict").await,
        0,
        "整批回滚后库里必须一行都没有"
    );

    // 2) 省略 NOT NULL 列且没有默认值。
    let report = insert(
        session.as_ref(),
        "strict",
        vec![row(vec![value("w", Value::Text("only-w".to_owned()))])],
    )
    .await
    .expect("逐行错误不是整批 Err");
    assert!(report.rolled_back);
    assert_eq!(
        report.results[0].outcome,
        InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT
        },
        "「省略且无默认值」必须与「显式 NULL」分开"
    );
    assert_eq!(count_rows(session.as_ref(), "strict").await, 0);
}

#[tokio::test]
async fn unique_conflict_is_reported_per_row_and_whole_batch_rolls_back() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE accounts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            email TEXT NOT NULL UNIQUE\
         )",
    )
    .await;

    let report = insert(
        session.as_ref(),
        "accounts",
        vec![
            row(vec![value(
                "email",
                Value::Text("a@example.com".to_owned()),
            )]),
            row(vec![value(
                "email",
                Value::Text("a@example.com".to_owned()),
            )]),
            row(vec![value(
                "email",
                Value::Text("c@example.com".to_owned()),
            )]),
        ],
    )
    .await
    .expect("冲突逐行上报");

    assert_eq!(report.results.len(), 3);
    assert_eq!(
        report.results[1].outcome,
        InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_UNIQUE_VIOLATION
        }
    );
    assert!(
        report.results[2].outcome.is_inserted(),
        "第 2 行冲突后第 3 行必须继续执行（一次报全）"
    );
    assert_eq!(report.inserted_rows(), 2);
    assert!(report.rolled_back);
    assert_eq!(report.rows_affected(), 0);

    // 外部 SQL 复核：整批回滚后库里 0 行（不是「部分成功留在库里」）。
    assert_eq!(count_rows(session.as_ref(), "accounts").await, 0);

    // 失败行不带主键、带稳定错误码。
    assert!(report.results[1].key.is_none(), "失败行不得回传主键");
    assert_eq!(report.results[1].rows_affected(), 0);
}

#[tokio::test]
async fn transaction_mode_rolls_back_to_savepoint_and_keeps_the_outer_transaction() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE guard (id INTEGER PRIMARY KEY, v TEXT NOT NULL UNIQUE)",
    )
    .await;

    // ---- 批失败 → 外层事务仍可用 → 继续写 → 提交 ----
    let transaction = session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: false,
        })
        .await
        .expect("开启事务");

    let failed = insert(
        session.as_ref(),
        "guard",
        vec![
            row(vec![value("v", Value::Text("dup".to_owned()))]),
            row(vec![value("v", Value::Text("dup".to_owned()))]),
        ],
    )
    .await
    .expect("事务模式下的冲突仍是逐行上报");
    assert!(failed.rolled_back, "事务模式下批失败必须回滚到 savepoint");
    assert_eq!(failed.rows_affected(), 0);

    // 外层事务没被回滚：仍能写入，且这次能提交。
    let good = insert(
        session.as_ref(),
        "guard",
        vec![row(vec![value("v", Value::Text("kept".to_owned()))])],
    )
    .await
    .expect("失败批次之后外层事务必须仍可用");
    assert!(!good.rolled_back);
    assert_eq!(good.rows_affected(), 1);

    session
        .commit_transaction(&transaction)
        .await
        .expect("提交外层事务");
    assert_eq!(
        count_rows(session.as_ref(), "guard").await,
        1,
        "只有 savepoint 之后的那一行落库"
    );

    // ---- 批失败 → 外层事务仍可回滚（丢弃后续写入）----
    let transaction = session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: false,
        })
        .await
        .expect("开启第二个事务");
    let failed = insert(
        session.as_ref(),
        "guard",
        vec![
            row(vec![value("v", Value::Text("x".to_owned()))]),
            row(vec![value("v", Value::Text("x".to_owned()))]),
        ],
    )
    .await
    .expect("冲突逐行上报");
    assert!(failed.rolled_back);
    insert(
        session.as_ref(),
        "guard",
        vec![row(vec![value("v", Value::Text("discarded".to_owned()))])],
    )
    .await
    .expect("事务仍可用");
    session
        .rollback_transaction(&transaction)
        .await
        .expect("回滚外层事务");
    assert_eq!(
        count_rows(session.as_ref(), "guard").await,
        1,
        "外层事务回滚后不得留下 savepoint 之后的行"
    );
}

#[tokio::test]
async fn insert_round_trips_scalar_types_bit_for_bit() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE typed (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            label TEXT NOT NULL, \
            amount TEXT, \
            payload BLOB, \
            created_at TEXT, \
            meta TEXT, \
            note TEXT\
         )",
    )
    .await;

    let decimal = Decimal::new("123456789012345678901234567890", 10).unwrap();
    let payload: Vec<u8> = vec![0x00, 0x01, 0xFE, 0xFF, 0x00];
    let report = insert(
        session.as_ref(),
        "typed",
        vec![row(vec![
            value("label", Value::Text("alpha".to_owned())),
            value("amount", Value::Decimal(decimal.clone())),
            value("payload", Value::Bytes(payload.clone())),
            value(
                "created_at",
                Value::Text("2026-11-05 01:02:03.456789".to_owned()),
            ),
            value("meta", Value::Json("{\"k\":[1,2,3]}".to_owned())),
            value("note", Value::Null),
        ])],
    )
    .await
    .expect("插入各类型");
    assert!(!report.rolled_back);
    let rows = browse(session.as_ref(), "typed").await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row[1].value, Value::Text("alpha".to_owned()));
    // SQLite 没有原生 decimal：写入十进制文本，读回必须一字不差（不经过 f64）。
    assert_eq!(
        row[2].value,
        Value::Text("12345678901234567890.1234567890".to_owned()),
        "Decimal 文本往返丢失精度"
    );
    assert_eq!(
        row[3].value,
        Value::Bytes(payload),
        "二进制（含 NUL）往返不一致"
    );
    assert_eq!(
        row[4].value,
        Value::Text("2026-11-05 01:02:03.456789".to_owned()),
        "微秒时间戳文本往返不一致"
    );
    assert_eq!(row[5].value, Value::Text("{\"k\":[1,2,3]}".to_owned()));
    assert_eq!(row[6].value, Value::Null);
}

#[tokio::test]
async fn request_level_rejections_send_no_sql_at_all() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE plain (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT NOT NULL)",
    )
    .await;
    exec(
        session.as_ref(),
        "CREATE VIEW v_plain AS SELECT id, v FROM plain",
    )
    .await;
    exec(
        session.as_ref(),
        "CREATE TABLE generated (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            w INTEGER NOT NULL, \
            doubled INTEGER GENERATED ALWAYS AS (w * 2) STORED\
         )",
    )
    .await;

    // 视图不可插入：明确只读，不是「插入 0 行」。
    let error = insert_object(
        session.as_ref(),
        &view(session.as_ref(), "v_plain"),
        vec![row(vec![value("v", Value::Text("x".to_owned()))])],
    )
    .await
    .expect_err("视图必须拒绝插入");
    assert_eq!(error.code(), ErrorCode::TABLE_DATA_READ_ONLY);

    // 生成列不能显式写入。
    let report = insert(
        session.as_ref(),
        "generated",
        vec![row(vec![
            value("w", Value::Int(1)),
            value("doubled", Value::Int(2)),
        ])],
    )
    .await
    .expect("生成列是逐行错误");
    assert!(report.rolled_back);
    assert_eq!(
        report.results[0].outcome,
        InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_COLUMN_NOT_INSERTABLE
        }
    );

    // 未知列：请求级非法，整批 Err（一条 SQL 都不发）。
    let error = insert(
        session.as_ref(),
        "plain",
        vec![row(vec![value("missing", Value::Int(1))])],
    )
    .await
    .expect_err("未知列必须整批拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 重复列（同一行同一列给两次）同样是请求级非法。
    let error = insert(
        session.as_ref(),
        "plain",
        vec![row(vec![
            value("v", Value::Text("a".to_owned())),
            value("v", Value::Text("b".to_owned())),
        ])],
    )
    .await
    .expect_err("重复列必须整批拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 空批次是非法请求（proto：空列表不是「插入 0 行」）。
    let error = insert(session.as_ref(), "plain", Vec::new())
        .await
        .expect_err("空批次必须拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    assert_eq!(count_rows(session.as_ref(), "plain").await, 0);
    assert_eq!(count_rows(session.as_ref(), "generated").await, 0);
}

#[tokio::test]
async fn batch_is_clamped_to_500_rows_and_reports_the_remainder() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE bulk (id INTEGER PRIMARY KEY AUTOINCREMENT, v INTEGER NOT NULL)",
    )
    .await;

    let rows: Vec<InsertRow> = (0..505)
        .map(|index| row(vec![value("v", Value::Int(index))]))
        .collect();
    let report = insert(session.as_ref(), "bulk", rows)
        .await
        .expect("插入 505 行（钳制）");
    assert!(!report.rolled_back);
    assert_eq!(report.results.len(), 500, "只执行前 500 行");
    assert_eq!(report.deferred, 5, "未执行行数必须回报，不能静默丢弃");
    assert_eq!(report.rows_affected(), 500);
    assert_eq!(count_rows(session.as_ref(), "bulk").await, 500);
}
