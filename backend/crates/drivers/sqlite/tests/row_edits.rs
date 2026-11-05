//! SQLite 行编辑（#47）：参数化 DML、类型保真、影响行数核对、冲突与原子性。
//!
//! 断言方式刻意做成「编辑前后逐单元格比较」而不是只看某个标量：
//! 位级一致要求 NULL、浮点、二进制、文本在往返中不变，任何一次隐式类型转换
//! （Decimal 走 f64、二进制当文本、时间丢微秒）都会被直接抓出来。
//!
//! 覆盖：
//!
//! 1. 各类型列更新后读回位级一致（NULL / 整数 / 浮点 / Decimal / 文本 / 二进制 /
//!    日期时间文本 / JSON），且未触及的列一字不差；
//! 2. 影响行数：0 行 → `RowNotFound`，期望行数不符 → `Conflict`，绝不静默成功；
//! 3. 批次原子性：执行期错误整批回滚（自动提交与显式事务两条路径）；
//! 4. 逐行冲突不中断批次（ADR 0017「单行失败不吞掉整批结果」）；
//! 5. 拒绝：无主键表、视图、只读事务、NULL 主键、缺失/多余主键值、未知列；
//! 6. 复合主键与自适应列名大小写。

use std::collections::BTreeMap;

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    ChangeOutcome, ColumnChange, ConnectionParams, Decimal, Driver, DriverSession, ExecuteRequest,
    IsolationLevel, ObjectKind, ObjectRef, PageRequest, QualifiedName, Row, RowChange,
    RowChangeCancel, RowChangeReport, RowChangeRequest, RowKey, TableDataProvider, TlsMode,
    TransactionOptions, Value,
};
use sqarvo_driver_sqlite::SqliteDriver;
use tempfile::TempDir;

async fn session() -> (TempDir, Box<dyn DriverSession>) {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("row-edits.db");
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

async fn all_rows(session: &dyn DriverSession, name: &str) -> Vec<Row> {
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

async fn read_row(session: &dyn DriverSession, name: &str, key: i64) -> Row {
    all_rows(session, name)
        .await
        .into_iter()
        .find(|row| row[0].value == Value::Int(key))
        .unwrap_or_else(|| panic!("找不到主键 {key} 的行"))
}

fn key(id: i64) -> RowKey {
    RowKey::new(vec![Value::Int(id)])
}

fn update(id: i64, column: &str, value: Value) -> RowChange {
    RowChange::Update {
        key: key(id),
        changes: vec![ColumnChange {
            column: column.to_owned(),
            value,
        }],
        expected_rows_affected: 0,
    }
}

fn delete(id: i64) -> RowChange {
    RowChange::Delete {
        key: key(id),
        expected_rows_affected: 0,
    }
}

async fn apply(
    session: &dyn DriverSession,
    name: &str,
    changes: Vec<RowChange>,
) -> sqarvo_common::Result<RowChangeReport> {
    provider(session)
        .apply_row_changes(
            RowChangeRequest {
                object: object(session, name),
                changes,
            },
            &RowChangeCancel::new(),
        )
        .await
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // 类型往返按「改一列 → NULL/空值 → Decimal → 大值 → 时间」串行
async fn edits_round_trip_every_scalar_type_bit_for_bit() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE typed (\
            id INTEGER PRIMARY KEY, \
            label TEXT NOT NULL, \
            number INTEGER, \
            ratio REAL, \
            amount TEXT, \
            payload BLOB, \
            created_at TEXT, \
            meta TEXT\
         )",
    )
    .await;
    exec(
        session.as_ref(),
        "INSERT INTO typed VALUES (\
            1, 'alpha', -9223372036854775808, 0.1, '123456.789', X'0001FF00', \
            '2026-11-05 01:02:03.456789', '{\"k\":[1,2,3]}'\
         ), (2, 'beta', 0, 0.0, '0', X'', '2026-11-05', '{}')",
    )
    .await;

    let before = read_row(session.as_ref(), "typed", 1).await;
    assert_eq!(before.len(), 8);

    // 1) 更新一列，其他列必须位级不变。
    let report = apply(
        session.as_ref(),
        "typed",
        vec![update(1, "label", Value::Text("alpha-2".to_owned()))],
    )
    .await
    .expect("更新一列");
    assert_eq!(report.results.len(), 1);
    assert_eq!(
        report.results[0].outcome,
        ChangeOutcome::Applied { rows_affected: 1 }
    );

    let after = read_row(session.as_ref(), "typed", 1).await;
    assert_eq!(after[0].value, Value::Int(1));
    assert_eq!(after[1].value, Value::Text("alpha-2".to_owned()));
    for index in [0, 2, 3, 4, 5, 6, 7] {
        assert_eq!(
            after[index], before[index],
            "第 {index} 列在只改另一列时发生了变化（不是位级一致）"
        );
    }
    // 只读回的值与写入值完全相等：整数下界、浮点、二进制带 NUL、微秒时间戳、JSON 原文。
    assert_eq!(before[2].value, Value::Int(i64::MIN));
    assert_eq!(before[3].value, Value::Float(0.1));
    assert_eq!(before[4].value, Value::Text("123456.789".to_owned()));
    assert_eq!(before[5].value, Value::Bytes(vec![0x00, 0x01, 0xFF, 0x00]));
    assert_eq!(
        before[6].value,
        Value::Text("2026-11-05 01:02:03.456789".to_owned())
    );
    // SQLite 没有原生 JSON 类型：JSON 文本按 TEXT 存储类返回，原文一字不差。
    assert_eq!(before[7].value, Value::Text("{\"k\":[1,2,3]}".to_owned()));

    // 2) 写入 NULL、空字符串、空二进制：三者必须可区分。
    let report = apply(
        session.as_ref(),
        "typed",
        vec![RowChange::Update {
            key: key(2),
            changes: vec![
                // `amount` 可空：NULL 必须与空字符串、0 明确区分。
                ColumnChange {
                    column: "amount".to_owned(),
                    value: Value::Null,
                },
                ColumnChange {
                    column: "payload".to_owned(),
                    value: Value::Bytes(Vec::new()),
                },
                ColumnChange {
                    column: "number".to_owned(),
                    value: Value::Int(0),
                },
            ],
            expected_rows_affected: 1,
        }],
    )
    .await
    .expect("写入 NULL 与空值");
    assert_eq!(report.rows_affected(), 1);
    let row = read_row(session.as_ref(), "typed", 2).await;
    assert_eq!(row[4].value, Value::Null, "NULL 不能被写成空字符串");
    assert_eq!(
        row[5].value,
        Value::Bytes(Vec::new()),
        "空 BLOB 必须保持为空 BLOB"
    );
    assert_ne!(row[4].value, Value::Text(String::new()));
    assert_ne!(row[4].value, Value::Int(0));

    // 3) Decimal 以十进制文本精确写入，不经过 f64。
    let decimal = Decimal::new("123456789012345678901234567890", 10).unwrap();
    apply(
        session.as_ref(),
        "typed",
        vec![update(2, "amount", Value::Decimal(decimal.clone()))],
    )
    .await
    .expect("写入 Decimal");
    let row = read_row(session.as_ref(), "typed", 2).await;
    assert_eq!(
        row[4].value,
        Value::Text("12345678901234567890.1234567890".to_owned()),
        "Decimal 必须按十进制文本精确写入"
    );

    // 4) 二进制大值（含 0x00）往返一致，长度不变。
    let blob: Vec<u8> = (0..=255_u8).cycle().take(4_096).collect();
    apply(
        session.as_ref(),
        "typed",
        vec![update(2, "payload", Value::Bytes(blob.clone()))],
    )
    .await
    .expect("写入二进制");
    assert_eq!(
        read_row(session.as_ref(), "typed", 2).await[5].value,
        Value::Bytes(blob)
    );

    // 5) 带微秒的日期时间以 ISO-8601 文本写入并原样读回。
    let stamp = Value::DateTime(time::PrimitiveDateTime::new(
        time::Date::from_calendar_date(2026, time::Month::November, 5).unwrap(),
        time::Time::from_hms_micro(1, 2, 3, 456_789).unwrap(),
    ));
    apply(
        session.as_ref(),
        "typed",
        vec![update(2, "created_at", stamp)],
    )
    .await
    .expect("写入日期时间");
    assert_eq!(
        read_row(session.as_ref(), "typed", 2).await[6].value,
        Value::Text("2026-11-05 01:02:03.456789".to_owned()),
        "微秒不得丢失"
    );
}

#[tokio::test]
async fn missing_row_and_mismatched_expectation_are_explicit() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE items (id INTEGER PRIMARY KEY, v TEXT)",
    )
    .await;
    exec(session.as_ref(), "INSERT INTO items VALUES (1,'a'),(2,'b')").await;

    // 已被其它会话删除（这里用先删后改模拟）：0 行必须报 RowNotFound，不是成功。
    apply(session.as_ref(), "items", vec![delete(2)])
        .await
        .expect("先删除");
    let report = apply(
        session.as_ref(),
        "items",
        vec![update(2, "v", Value::Text("stale".to_owned()))],
    )
    .await
    .expect("更新已删除行应返回逐行结果而不是整批错误");
    assert_eq!(report.results[0].outcome, ChangeOutcome::RowNotFound);
    assert_eq!(report.rows_affected(), 0);
    assert!(report.has_failures());

    let report = apply(session.as_ref(), "items", vec![delete(2)])
        .await
        .expect("重复删除");
    assert_eq!(report.results[0].outcome, ChangeOutcome::RowNotFound);

    // 期望 2 行但主键只命中 1 行：必须报 Conflict，不得当作成功。
    let report = apply(
        session.as_ref(),
        "items",
        vec![RowChange::Update {
            key: key(1),
            changes: vec![ColumnChange {
                column: "v".to_owned(),
                value: Value::Text("x".to_owned()),
            }],
            expected_rows_affected: 2,
        }],
    )
    .await
    .expect("期望不符应返回逐行结果");
    assert_eq!(
        report.results[0].outcome,
        ChangeOutcome::Conflict {
            expected: 2,
            actual: 1
        }
    );
    // 契约（#68）：冲突行必须上报驱动**真实**影响的行数（这条 UPDATE 确实改动了 1 行），
    // 不再恒为 0——否则调用方看不出「动了 1 行但被拒」。
    assert_eq!(report.results[0].rows_affected(), 1);
    assert_eq!(report.rows_affected(), 1, "冲突行计入驱动实际影响行数");
    assert!(report.has_failures(), "行数如实上报不等于判定成功");

    // 未受影响/未匹配的行保持原值。
    assert_eq!(
        read_row(session.as_ref(), "items", 1).await[1].value,
        Value::Text("x".to_owned())
    );
}

#[tokio::test]
async fn same_value_update_counts_as_applied() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE same_value (id INTEGER PRIMARY KEY, v TEXT NOT NULL, n INTEGER)",
    )
    .await;
    exec(
        session.as_ref(),
        "INSERT INTO same_value VALUES (1,'alpha',7),(2,'beta',8)",
    )
    .await;

    // 预读一行，再把 `v` 写成它已有的值：`sqlite3_changes()` 按**匹配**行计，
    // 因此必须是 APPLIED(1) —— 与 MariaDB 的 `CLIENT_FOUND_ROWS` 语义一致。
    let before = read_row(session.as_ref(), "same_value", 1).await;
    let report = apply(
        session.as_ref(),
        "same_value",
        vec![update(1, "v", before[1].value.clone())],
    )
    .await
    .expect("同值更新应返回逐行结果");
    assert_eq!(
        report.results[0].outcome,
        ChangeOutcome::Applied { rows_affected: 1 },
        "同值 UPDATE 的影响行数必须是匹配行数，不是实际变化行数"
    );
    assert_eq!(report.rows_affected(), 1);
    assert!(!report.has_failures());

    let after = read_row(session.as_ref(), "same_value", 1).await;
    for index in 0..before.len() {
        assert_eq!(
            after[index], before[index],
            "第 {index} 列在同值更新后发生了变化"
        );
    }

    // 对照：主键不存在的行 → 0 行 → `ROW_NOT_FOUND`。
    let report = apply(
        session.as_ref(),
        "same_value",
        vec![update(9_999, "v", Value::Text("alpha".to_owned()))],
    )
    .await
    .expect("更新不存在的行应返回逐行结果");
    assert_eq!(report.results[0].outcome, ChangeOutcome::RowNotFound);
    assert_eq!(report.results[0].rows_affected(), 0);
    assert!(report.has_failures());
}

#[tokio::test]
async fn failed_statement_rolls_back_the_whole_batch() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE strict (id INTEGER PRIMARY KEY, v INTEGER NOT NULL CHECK (v >= 0))",
    )
    .await;
    exec(session.as_ref(), "INSERT INTO strict VALUES (1,1),(2,2)").await;

    // 第二条违反 CHECK：整批必须回滚，第一条不得落库。
    let error = apply(
        session.as_ref(),
        "strict",
        vec![
            update(1, "v", Value::Int(10)),
            update(2, "v", Value::Int(-1)),
        ],
    )
    .await
    .expect_err("违反约束必须整批失败");
    assert_eq!(
        error.category(),
        ErrorCategory::Internal,
        "SQLite 约束错误按底层错误分类"
    );

    assert_eq!(
        read_row(session.as_ref(), "strict", 1).await[1].value,
        Value::Int(1)
    );
    assert_eq!(
        read_row(session.as_ref(), "strict", 2).await[1].value,
        Value::Int(2)
    );
}

#[tokio::test]
async fn batch_inside_explicit_transaction_uses_savepoint_and_leaves_tx_usable() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE strict (id INTEGER PRIMARY KEY, v INTEGER NOT NULL CHECK (v >= 0))",
    )
    .await;
    exec(session.as_ref(), "INSERT INTO strict VALUES (1,1),(2,2)").await;

    let transaction = session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: false,
        })
        .await
        .expect("开启事务");

    // 批次里第二条失败：回到保存点，第一条不生效，但外层事务仍然可用。
    apply(
        session.as_ref(),
        "strict",
        vec![
            update(1, "v", Value::Int(10)),
            update(2, "v", Value::Int(-1)),
        ],
    )
    .await
    .expect_err("违反约束必须整批失败");
    assert_eq!(
        read_row(session.as_ref(), "strict", 1).await[1].value,
        Value::Int(1),
        "保存点回滚后第一条变更不得留下"
    );

    // 外层事务仍可继续写，提交后可见。
    apply(
        session.as_ref(),
        "strict",
        vec![update(1, "v", Value::Int(7))],
    )
    .await
    .expect("外层事务仍可用");
    session
        .commit_transaction(&transaction)
        .await
        .expect("提交事务");
    assert_eq!(
        read_row(session.as_ref(), "strict", 1).await[1].value,
        Value::Int(7)
    );
}

#[tokio::test]
async fn per_row_conflict_does_not_abort_the_rest_of_the_batch() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE items (id INTEGER PRIMARY KEY, v TEXT)",
    )
    .await;
    exec(session.as_ref(), "INSERT INTO items VALUES (1,'a'),(2,'b')").await;

    let report = apply(
        session.as_ref(),
        "items",
        vec![
            update(1, "v", Value::Text("a2".to_owned())),
            update(9, "v", Value::Text("ghost".to_owned())),
            delete(2),
        ],
    )
    .await
    .expect("逐行冲突不吞掉整批结果");

    assert_eq!(report.results.len(), 3);
    assert_eq!(
        report.results[0].outcome,
        ChangeOutcome::Applied { rows_affected: 1 }
    );
    assert_eq!(report.results[1].outcome, ChangeOutcome::RowNotFound);
    assert_eq!(
        report.results[2].outcome,
        ChangeOutcome::Applied { rows_affected: 1 }
    );
    assert_eq!(report.rows_affected(), 2);

    assert_eq!(
        read_row(session.as_ref(), "items", 1).await[1].value,
        Value::Text("a2".to_owned())
    );
    assert_eq!(
        all_rows(session.as_ref(), "items").await.len(),
        1,
        "第 2 行应已删除"
    );
}

#[tokio::test]
async fn composite_primary_key_locates_rows_by_the_full_key() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE orders (tenant INTEGER NOT NULL, id INTEGER NOT NULL, note TEXT, \
         PRIMARY KEY (tenant, id))",
    )
    .await;
    exec(
        session.as_ref(),
        "INSERT INTO orders VALUES (1,1,'a'),(1,2,'b'),(2,1,'c')",
    )
    .await;

    let report = apply(
        session.as_ref(),
        "orders",
        vec![
            RowChange::Update {
                key: RowKey::new(vec![Value::Int(1), Value::Int(2)]),
                changes: vec![ColumnChange {
                    // 列名大小写与 schema 不同：必须按 schema 的规范名解析。
                    column: "NOTE".to_owned(),
                    value: Value::Text("b2".to_owned()),
                }],
                expected_rows_affected: 0,
            },
            RowChange::Delete {
                key: RowKey::new(vec![Value::Int(2), Value::Int(1)]),
                expected_rows_affected: 0,
            },
        ],
    )
    .await
    .expect("复合主键编辑");
    assert_eq!(report.rows_affected(), 2);

    let rows = all_rows(session.as_ref(), "orders").await;
    assert_eq!(rows.len(), 2);
    let cells: Vec<(Value, Value, Value)> = rows
        .iter()
        .map(|row| {
            (
                row[0].value.clone(),
                row[1].value.clone(),
                row[2].value.clone(),
            )
        })
        .collect();
    assert_eq!(
        cells,
        vec![
            (Value::Int(1), Value::Int(1), Value::Text("a".to_owned())),
            (Value::Int(1), Value::Int(2), Value::Text("b2".to_owned())),
        ]
    );
}

#[tokio::test]
async fn read_only_targets_and_invalid_keys_are_rejected_before_writing() {
    let (_dir, session) = session().await;
    exec(session.as_ref(), "CREATE TABLE plain (v TEXT)").await;
    exec(session.as_ref(), "INSERT INTO plain VALUES ('a')").await;
    exec(
        session.as_ref(),
        "CREATE TABLE keyed (id INTEGER PRIMARY KEY, v TEXT)",
    )
    .await;
    exec(session.as_ref(), "INSERT INTO keyed VALUES (1,'a')").await;
    exec(
        session.as_ref(),
        "CREATE VIEW keyed_view AS SELECT id, v FROM keyed",
    )
    .await;

    // 无主键表：明确 no_primary_key，不用 rowid 近似定位。
    let error = apply(
        session.as_ref(),
        "plain",
        vec![RowChange::Update {
            key: RowKey::new(vec![Value::Int(1)]),
            changes: vec![ColumnChange {
                column: "v".to_owned(),
                value: Value::Text("b".to_owned()),
            }],
            expected_rows_affected: 0,
        }],
    )
    .await
    .expect_err("无主键表必须拒绝编辑");
    assert_eq!(error.code(), ErrorCode::TABLE_DATA_NO_PRIMARY_KEY);
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);

    // 视图：只读。
    let error = provider(session.as_ref())
        .apply_row_changes(
            RowChangeRequest {
                object: view(session.as_ref(), "keyed_view"),
                changes: vec![delete(1)],
            },
            &RowChangeCancel::new(),
        )
        .await
        .expect_err("视图必须拒绝编辑");
    assert_eq!(error.code(), ErrorCode::TABLE_DATA_READ_ONLY);

    // 主键含 NULL。
    let error = apply(
        session.as_ref(),
        "keyed",
        vec![RowChange::Delete {
            key: RowKey::new(vec![Value::Null]),
            expected_rows_affected: 0,
        }],
    )
    .await
    .expect_err("NULL 主键必须拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 主键值个数不足（复合主键只给一列同理）。
    let error = apply(
        session.as_ref(),
        "keyed",
        vec![RowChange::Delete {
            key: RowKey::new(Vec::new()),
            expected_rows_affected: 0,
        }],
    )
    .await
    .expect_err("空主键必须拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 未知列。
    let error = apply(
        session.as_ref(),
        "keyed",
        vec![update(1, "nope", Value::Int(1))],
    )
    .await
    .expect_err("未知列必须拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 同一列重复变更（会产生歧义 SET）。
    let error = apply(
        session.as_ref(),
        "keyed",
        vec![RowChange::Update {
            key: key(1),
            changes: vec![
                ColumnChange {
                    column: "v".to_owned(),
                    value: Value::Text("x".to_owned()),
                },
                ColumnChange {
                    column: "V".to_owned(),
                    value: Value::Text("y".to_owned()),
                },
            ],
            expected_rows_affected: 0,
        }],
    )
    .await
    .expect_err("重复列必须拒绝");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 所有拒绝都发生在写入之前：表内容未变。
    assert_eq!(
        read_row(session.as_ref(), "keyed", 1).await[1].value,
        Value::Text("a".to_owned())
    );
}

#[tokio::test]
async fn read_only_transaction_rejects_row_edits() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE keyed (id INTEGER PRIMARY KEY, v TEXT)",
    )
    .await;
    exec(session.as_ref(), "INSERT INTO keyed VALUES (1,'a')").await;

    let transaction = session
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: true,
        })
        .await
        .expect("开启只读事务");

    let error = apply(
        session.as_ref(),
        "keyed",
        vec![update(1, "v", Value::Text("b".to_owned()))],
    )
    .await
    .expect_err("只读事务必须拒绝写入");
    assert_eq!(
        error.code(),
        ErrorCode::TABLE_DATA_READ_ONLY,
        "只读事务必须给出明确的只读错误码"
    );
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);

    session
        .rollback_transaction(&transaction)
        .await
        .expect("回滚");
    assert_eq!(
        read_row(session.as_ref(), "keyed", 1).await[1].value,
        Value::Text("a".to_owned())
    );
}

#[tokio::test]
async fn empty_batch_is_a_noop_report() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE keyed (id INTEGER PRIMARY KEY, v TEXT)",
    )
    .await;
    let report = apply(session.as_ref(), "keyed", Vec::new())
        .await
        .expect("空批次");
    assert_eq!(report.results, Vec::new());
    assert_eq!(report.rows_affected(), 0);
    assert!(!report.has_failures());
}

#[tokio::test]
async fn browse_marks_primary_key_tables_editable_after_capability_flip() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE keyed (id INTEGER PRIMARY KEY, v TEXT)",
    )
    .await;
    assert!(provider(session.as_ref()).supports_row_edits());
    let page = provider(session.as_ref())
        .browse(
            &object(session.as_ref(), "keyed"),
            &PageRequest {
                page_size: 10,
                page_token: None,
            },
        )
        .await
        .expect("浏览");
    assert!(!page.read_only, "有主键表在 #47 后必须可编辑");
    assert_eq!(page.read_only_reason, None);
}
