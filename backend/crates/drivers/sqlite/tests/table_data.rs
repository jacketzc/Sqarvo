//! SQLite 表数据浏览（#46）：键集游标、无主键只读、非法游标。
//!
//! 断言方式刻意做成「主键值全集校验」而不是只看行数：行数相同的重复/遗漏
//! 会被集合比较直接抓出来。10 万行表同时做两件事：
//!
//! 1. 按需求翻 3 页，逐页校验取值区间与页大小；
//! 2. 从第一页一直翻到 `has_more = false`，把 10 万个主键与 `1..=100_000`
//!    做集合相等比较，证明翻页全程无重复、无遗漏、严格升序。
//!
//! 同时记录每页耗时（debug 构建），作为观察值，不是性能承诺。

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    ConnectionParams, Driver, DriverSession, ExecuteRequest, MAX_PAGE_SIZE, ObjectKind, ObjectRef,
    PageRequest, QualifiedName, ReadOnlyReason, RowChangeCancel, RowOrder, TableDataProvider,
    TlsMode, Value,
};
use sqarvo_driver_sqlite::SqliteDriver;
use tempfile::TempDir;

/// 大表行数。
const ROWS: i64 = 100_000;

async fn session() -> (TempDir, Box<dyn DriverSession>) {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("table-data.db");
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

fn browse(session: &dyn DriverSession) -> &dyn TableDataProvider {
    session.table_data().expect("SQLite 必须提供表数据能力")
}

fn page(size: u32, token: Option<String>) -> PageRequest {
    PageRequest {
        page_size: size,
        page_token: token,
    }
}

/// 从行里取第一列的主键值（测试表的第一列都是整型主键）。
fn int_key(row: &sqarvo_driver_api::Row) -> i64 {
    match &row[0].value {
        Value::Int(value) => *value,
        other => panic!("期望 INTEGER 主键，实际 {other:?}"),
    }
}

async fn create_large_table(session: &dyn DriverSession) {
    exec(
        session,
        "CREATE TABLE big (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)",
    )
    .await;
    exec(
        session,
        &format!(
            "WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x < {ROWS}) \
             INSERT INTO big(id, payload) SELECT x, 'row-' || x FROM seq"
        ),
    )
    .await;
}

#[tokio::test]
async fn large_table_pages_are_complete_and_strictly_ordered() {
    let (_dir, session) = session().await;
    create_large_table(session.as_ref()).await;

    let provider = browse(session.as_ref());
    let big = object(session.as_ref(), "big");

    // ---- 1. 需求要求的 3 页：逐页校验区间，主键集合必须完整 ----
    let mut token: Option<String> = None;
    let mut first_three = BTreeSet::new();
    let mut timings: Vec<Duration> = Vec::new();
    for index in 0..3_u32 {
        let started = Instant::now();
        let page = provider
            .browse(&big, &page(500, token.clone()))
            .await
            .expect("浏览一页");
        timings.push(started.elapsed());

        assert_eq!(page.rows.len(), 500, "第 {index} 页应满 500 行");
        assert!(page.has_more, "10 万行表的第 {index} 页之后仍应有数据");
        assert!(page.next_page_token.is_some(), "has_more 时必须给游标");
        assert_eq!(page.ordering, RowOrder::PrimaryKeyAscending);
        // #47 后 SQLite 声明了行编辑能力，有主键表浏览不再只读。
        assert!(!page.read_only, "有主键表在行编辑落地后必须可编辑");
        assert_eq!(page.read_only_reason, None, "可编辑时不得给出只读原因");

        let keys: Vec<i64> = page.rows.iter().map(int_key).collect();
        let expected_start = i64::from(index) * 500 + 1;
        assert_eq!(
            keys,
            (expected_start..expected_start + 500).collect::<Vec<_>>(),
            "第 {index} 页的主键必须连续升序"
        );
        first_three.extend(keys);
        token = page.next_page_token;
    }
    assert_eq!(first_three.len(), 1_500, "3 页之间不得有重复主键");
    assert_eq!(*first_three.first().unwrap(), 1, "第一页必须从最小主键开始");
    assert_eq!(*first_three.last().unwrap(), 1_500);

    // ---- 2. 翻完全表：10 万个主键与全集比较 ----
    let walk_started = Instant::now();
    let mut collected: Vec<i64> = Vec::with_capacity(usize::try_from(ROWS).unwrap());
    let mut token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let page = provider
            .browse(&big, &page(500, token))
            .await
            .expect("翻页");
        pages += 1;
        assert!(page.rows.len() <= 500, "单页不得超过上限 500");
        collected.extend(page.rows.iter().map(int_key));
        if let Some(next) = page.next_page_token {
            assert!(page.has_more);
            assert_ne!(next, String::new());
            token = Some(next);
        } else {
            assert!(!page.has_more);
            break;
        }
        assert!(pages <= 1_000, "翻页未收敛");
    }
    let walk_elapsed = walk_started.elapsed();

    assert_eq!(pages, 200, "10 万行按每页 500 应分 200 页");
    assert_eq!(collected.len(), usize::try_from(ROWS).unwrap());
    let unique: BTreeSet<i64> = collected.iter().copied().collect();
    assert_eq!(
        unique.len(),
        collected.len(),
        "翻页出现重复主键：有 {} 个重复",
        collected.len() - unique.len()
    );
    let expected: BTreeSet<i64> = (1..=ROWS).collect();
    assert_eq!(unique, expected, "主键集合与全集不一致（有遗漏或多余行）");
    assert!(
        collected.windows(2).all(|pair| pair[0] < pair[1]),
        "翻页顺序必须严格升序（主键序）"
    );

    println!(
        "SQLite 10 万行：前 3 页耗时 {:?}/{:?}/{:?}，全表 {pages} 页共 {walk_elapsed:?}",
        timings[0], timings[1], timings[2]
    );
    assert!(
        walk_elapsed < Duration::from_secs(60),
        "全表翻页耗时异常：{walk_elapsed:?}"
    );
}

#[tokio::test]
async fn composite_primary_key_uses_column_order_comparison() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE orders (tenant INTEGER NOT NULL, id INTEGER NOT NULL, note TEXT, \
         PRIMARY KEY (tenant, id))",
    )
    .await;
    exec(
        session.as_ref(),
        "INSERT INTO orders VALUES (1,1,'a'),(1,2,'b'),(1,3,'c'),\
         (2,1,'d'),(2,2,'e'),(3,1,'f')",
    )
    .await;

    let provider = browse(session.as_ref());
    let orders = object(session.as_ref(), "orders");

    let first = provider.browse(&orders, &page(2, None)).await.unwrap();
    assert_eq!(first.rows.len(), 2);
    assert_eq!((int_key(&first.rows[0]), int_key(&first.rows[1])), (1, 1));
    let token = first.next_page_token.expect("应返回游标");

    let second = provider
        .browse(&orders, &page(2, Some(token)))
        .await
        .unwrap();
    let keys: Vec<(i64, i64)> = second
        .rows
        .iter()
        .map(|row| match (&row[0].value, &row[1].value) {
            (Value::Int(tenant), Value::Int(id)) => (*tenant, *id),
            other => panic!("期望复合主键，实际 {other:?}"),
        })
        .collect();
    // 跨 tenant 边界继续：不是按 id 单独比较，而是 (tenant, id) 字典序。
    assert_eq!(keys, vec![(1, 3), (2, 1)]);

    let token = second.next_page_token.expect("应返回游标");
    let third = provider
        .browse(&orders, &page(2, Some(token)))
        .await
        .unwrap();
    let keys: Vec<(i64, i64)> = third
        .rows
        .iter()
        .map(|row| match (&row[0].value, &row[1].value) {
            (Value::Int(tenant), Value::Int(id)) => (*tenant, *id),
            other => panic!("期望复合主键，实际 {other:?}"),
        })
        .collect();
    assert_eq!(keys, vec![(2, 2), (3, 1)]);
    assert!(!third.has_more);
    assert!(third.next_page_token.is_none());

    // 三页拼起来必须正好是主键升序的全集（无重复、无遗漏）。
    let all: Vec<(i64, i64)> = [first.rows, second.rows, third.rows]
        .concat()
        .iter()
        .map(|row| match (&row[0].value, &row[1].value) {
            (Value::Int(tenant), Value::Int(id)) => (*tenant, *id),
            other => panic!("期望复合主键，实际 {other:?}"),
        })
        .collect();
    assert_eq!(all, vec![(1, 1), (1, 2), (1, 3), (2, 1), (2, 2), (3, 1)]);

    // 主键定义与 MetadataService 一致：tenant 在前、id 在后。
    let primary_key = provider.primary_key(&orders).await.unwrap();
    assert_eq!(primary_key.column_names(), ["tenant", "id"]);
}

#[tokio::test]
async fn text_primary_key_round_trips_through_the_cursor() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE tags (name TEXT PRIMARY KEY, hits INTEGER) WITHOUT ROWID",
    )
    .await;
    exec(
        session.as_ref(),
        "INSERT INTO tags VALUES ('alpha',1),('beta',2),('gamma',3),('张三',4)",
    )
    .await;

    let provider = browse(session.as_ref());
    let tags = object(session.as_ref(), "tags");

    let mut seen = Vec::new();
    let mut token = None;
    loop {
        let page = provider.browse(&tags, &page(1, token)).await.unwrap();
        assert_eq!(page.rows.len(), 1);
        match &page.rows[0][0].value {
            Value::Text(name) => seen.push(name.clone()),
            other => panic!("期望 TEXT 主键，实际 {other:?}"),
        }
        match page.next_page_token {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    // SQLite 的 BINARY 排序：ASCII 在前，UTF-8 多字节在后。
    assert_eq!(seen, vec!["alpha", "beta", "gamma", "张三"]);
}

#[tokio::test]
async fn table_without_primary_key_is_read_only_and_not_pageable() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE logs (message TEXT, level INTEGER)",
    )
    .await;
    exec(
        session.as_ref(),
        "WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x < 250) \
         INSERT INTO logs(message, level) SELECT 'm' || x, x % 5 FROM seq",
    )
    .await;

    let provider = browse(session.as_ref());
    let logs = object(session.as_ref(), "logs");
    assert!(
        provider.primary_key(&logs).await.unwrap().is_empty(),
        "无主键表的主键定义必须为空，不能给出 rowid 近似主键"
    );

    let page = provider.browse(&logs, &page(100, None)).await.unwrap();
    assert_eq!(page.rows.len(), 100);
    assert!(page.read_only, "无主键表必须标记只读");
    assert_eq!(
        page.read_only_reason,
        Some(ReadOnlyReason::NoPrimaryKey),
        "只读原因必须明确为「无主键」"
    );
    assert_eq!(
        page.ordering,
        RowOrder::Unstable,
        "无主键表没有稳定排序，不能声称主键序"
    );
    assert!(page.next_page_token.is_none(), "无主键表不签发游标");
    assert!(!page.has_more, "无主键表不提供有意义的继续语义");

    // 只读并不是「拒绝浏览」：列元数据与行仍然可用。
    assert_eq!(page.columns.len(), 2);
    assert_eq!(page.columns[0].name, "message");
    assert_eq!(page.columns[1].name, "level");
}

#[tokio::test]
async fn invalid_cursor_is_rejected_instead_of_returning_the_first_page() {
    let (_dir, session) = session().await;
    create_large_table(session.as_ref()).await;
    exec(
        session.as_ref(),
        "CREATE TABLE other (id INTEGER PRIMARY KEY, payload TEXT)",
    )
    .await;
    exec(session.as_ref(), "INSERT INTO other VALUES (1,'x')").await;

    let provider = browse(session.as_ref());
    let big = object(session.as_ref(), "big");
    let first = provider.browse(&big, &page(2, None)).await.unwrap();
    let token = first.next_page_token.expect("应返回游标");

    // 篡改载荷（保持格式）：必须报 cursor_invalid，绝不回退第一页。
    let mut tampered = token.clone();
    let index = tampered.rfind(|c: char| c.is_ascii_hexdigit()).unwrap();
    let replacement = if tampered.as_bytes()[index] == b'0' {
        '1'
    } else {
        '0'
    };
    tampered.replace_range(index..=index, &replacement.to_string());

    // 换成另一张表的游标：同样必须失效。
    let other = object(session.as_ref(), "other");
    for (object, candidate) in [
        (&big, tampered),
        (&big, "not-a-cursor".to_owned()),
        (&other, token.clone()),
    ] {
        let error = provider
            .browse(object, &page(2, Some(candidate.clone())))
            .await
            .err()
            .unwrap_or_else(|| panic!("非法游标必须报错：{candidate}"));
        assert_eq!(
            error.code(),
            ErrorCode::TABLE_DATA_CURSOR_INVALID,
            "token={candidate}"
        );
        assert_eq!(error.category(), ErrorCategory::InvalidArgument);
        assert!(
            !error.message().contains(&candidate),
            "错误文案不得回显游标原文"
        );
    }
}

#[tokio::test]
async fn page_size_is_clamped_instead_of_erroring() {
    let (_dir, session) = session().await;
    create_large_table(session.as_ref()).await;
    let provider = browse(session.as_ref());
    let big = object(session.as_ref(), "big");

    // 0 → 默认 100。
    let default_page = provider.browse(&big, &page(0, None)).await.unwrap();
    assert_eq!(default_page.rows.len(), 100);

    // 超过上限 → 钳制到 500，不报错。
    let capped = provider.browse(&big, &page(u32::MAX, None)).await.unwrap();
    assert_eq!(capped.rows.len(), usize::try_from(MAX_PAGE_SIZE).unwrap());

    // 显式请求 500 与钳制结果一致。
    let exact = provider.browse(&big, &page(500, None)).await.unwrap();
    assert_eq!(exact.rows.len(), capped.rows.len());
}

#[tokio::test]
async fn missing_table_is_not_found_and_not_an_empty_page() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE present (id INTEGER PRIMARY KEY)",
    )
    .await;
    let provider = browse(session.as_ref());
    let missing = object(session.as_ref(), "absent");

    let error = provider
        .browse(&missing, &page(10, None))
        .await
        .expect_err("不存在的表必须报错");
    assert_eq!(error.category(), ErrorCategory::NotFound);
    assert_eq!(
        error.code(),
        ErrorCode::from_static("sqlite.object_not_found")
    );
}

#[tokio::test]
async fn browse_is_rejected_while_a_result_cursor_is_open() {
    let (_dir, session) = session().await;
    create_large_table(session.as_ref()).await;
    let provider = browse(session.as_ref());
    let big = object(session.as_ref(), "big");

    // 打开一个未读完的结果游标，会话连接被它占用。
    let start = session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: "SELECT * FROM big".to_owned(),
            max_rows: Some(10),
            timeout: None,
        })
        .await
        .unwrap();
    assert!(start.first_batch.expect("应有首批").has_more);

    let error = provider
        .browse(&big, &page(10, None))
        .await
        .expect_err("游标未释放时不得与浏览交错");
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);
    assert_eq!(error.code(), ErrorCode::from_static("sqlite.query_active"));
}

#[tokio::test]
async fn views_browse_as_read_only_without_primary_key() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE base (id INTEGER PRIMARY KEY, v TEXT)",
    )
    .await;
    exec(session.as_ref(), "INSERT INTO base VALUES (1,'a'),(2,'b')").await;
    exec(
        session.as_ref(),
        "CREATE VIEW recent AS SELECT id, v FROM base WHERE id > 0",
    )
    .await;

    let provider = browse(session.as_ref());
    let view = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, "recent"),
        kind: ObjectKind::View,
    };
    let page = provider.browse(&view, &page(10, None)).await.unwrap();
    assert_eq!(page.rows.len(), 2);
    assert!(page.read_only);
    assert_eq!(
        page.read_only_reason,
        Some(ReadOnlyReason::NoPrimaryKey),
        "视图没有主键，必须按无主键只读处理"
    );
    assert_eq!(page.ordering, RowOrder::Unstable);
}

fn params_at(path: &std::path::Path, read_only: bool) -> ConnectionParams {
    let mut properties = std::collections::BTreeMap::new();
    if read_only {
        properties.insert("read_only".to_owned(), "true".to_owned());
    }
    ConnectionParams {
        driver_id: "sqlite".parse().unwrap(),
        host: path.display().to_string(),
        port: 0,
        database: None,
        username: None,
        credential_ref: None,
        tls: TlsMode::Unspecified,
        tls_server_name: None,
        properties,
    }
}

/// 只读连接（#60）：浏览必须自报「连接只读」，写入在准备语句前被拒。
///
/// 这是 `ConnectionParams.read_only` 的真实链路：同一个库文件、同一个驱动，
/// 只改连接参数。可写连接的回归（`read_only = false` + 无只读原因）在同一用例里
/// 先断言一次，确保新原因不会溢到可编辑路径上。
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "同一库文件上串起可写回归、只读浏览与写入拒绝，拆开会丢对照"
)]
async fn read_only_connection_reports_reason_and_rejects_edits() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("readonly-browse.db");

    // ---- 1. 可写连接准备数据，并做可写回归断言 ----
    let writable = SqliteDriver::new()
        .connect(&params_at(&path, false), None)
        .await
        .expect("可写连接");
    for sql in [
        "CREATE TABLE orders(id INTEGER PRIMARY KEY, v TEXT NOT NULL)",
        "INSERT INTO orders VALUES (1,'a'),(2,'b'),(3,'c')",
        "CREATE TABLE logs(message TEXT, level INTEGER)",
        "INSERT INTO logs VALUES ('m1',1),('m2',2)",
    ] {
        exec(writable.as_ref(), sql).await;
    }
    let writable_orders = object(writable.as_ref(), "orders");
    let first = browse(writable.as_ref())
        .browse(&writable_orders, &page(2, None))
        .await
        .expect("可写连接浏览");
    assert!(!first.read_only, "可写连接必须可编辑");
    assert_eq!(
        first.read_only_reason, None,
        "可编辑时不得给出只读原因（回归）"
    );
    writable.close().await.expect("关闭可写连接");

    // ---- 2. 只读连接：同一张表浏览仍可用，但原因必须是「连接只读」 ----
    let read_only = SqliteDriver::new()
        .connect(&params_at(&path, true), None)
        .await
        .expect("只读连接");
    let provider = browse(read_only.as_ref());
    let orders = object(read_only.as_ref(), "orders");
    let first = provider
        .browse(&orders, &page(2, None))
        .await
        .expect("只读连接可以浏览");
    assert!(first.read_only, "只读连接必须自报只读");
    assert_eq!(
        first.read_only_reason,
        Some(ReadOnlyReason::ConnectionReadOnly),
        "只读连接的只读原因必须是「连接只读」，不能沉默或归到别的表结构原因"
    );
    assert_eq!(
        first.ordering,
        RowOrder::PrimaryKeyAscending,
        "只读不改变排序保证"
    );
    eprintln!(
        "[#60] 只读连接 browse：read_only={} reason={:?} ordering={:?} rows={}",
        first.read_only,
        first.read_only_reason,
        first.ordering,
        first.rows.len()
    );
    assert_eq!(first.rows.len(), 2);
    let token = first
        .next_page_token
        .clone()
        .expect("只读连接仍按主键翻页，必须签发游标");
    let second = provider
        .browse(&orders, &page(2, Some(token)))
        .await
        .expect("只读连接翻第二页");
    assert_eq!(int_key(&second.rows[0]), 3);

    // ---- 3. 写入被拒：table_data.read_only，且真的没写进去 ----
    let error = provider
        .apply_row_changes(
            sqarvo_driver_api::RowChangeRequest {
                object: orders.clone(),
                changes: vec![sqarvo_driver_api::RowChange::Update {
                    key: sqarvo_driver_api::RowKey::new(vec![Value::Int(1)]),
                    changes: vec![sqarvo_driver_api::ColumnChange {
                        column: "v".to_owned(),
                        value: Value::Text("changed".to_owned()),
                    }],
                    expected_rows_affected: 0,
                }],
            },
            &RowChangeCancel::new(),
        )
        .await
        .expect_err("只读连接必须拒绝行变更");
    assert_eq!(error.code(), ErrorCode::TABLE_DATA_READ_ONLY);
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);
    eprintln!(
        "[#60] 只读连接写入被拒：code={} category={:?} message={}",
        error.code(),
        error.category(),
        error.message()
    );
    let again = provider.browse(&orders, &page(1, None)).await.unwrap();
    assert_eq!(
        again.rows[0][1].value,
        Value::Text("a".to_owned()),
        "被拒绝的写入不得落库"
    );

    // ---- 4. 两种原因同时成立时报「连接只读」，「无稳定排序」仍由 ordering 表达 ----
    let logs = object(read_only.as_ref(), "logs");
    let logs_page = provider.browse(&logs, &page(10, None)).await.unwrap();
    assert!(logs_page.read_only);
    assert_eq!(
        logs_page.read_only_reason,
        Some(ReadOnlyReason::ConnectionReadOnly),
        "连接只读影响该连接上的所有表，优先于表结构原因"
    );
    assert_eq!(logs_page.ordering, RowOrder::Unstable);

    read_only.close().await.expect("关闭只读连接");
}
