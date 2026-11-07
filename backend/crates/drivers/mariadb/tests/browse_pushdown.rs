//! MariaDB 表浏览的排序 / 筛选 / offset / 计数下推（v0.2.3，#111，ADR 0022）。
//!
//! 需要真实数据库：设置 `SQARVO_MARIADB_URL`
//! （形如 `mysql://user:***@127.0.0.1:53306/sqarvo_test`）；未设置时用例打印跳过原因。
//!
//! 与 `integration.rs` 分开：那边是 #46 的浏览基线（主键升序 + 键集游标），
//! 这里只覆盖 #111 新增的四个维度，避免两个 Issue 改同一个文件。
//!
//! 前置：本用例需要**写权限**（自建/自删表），并且库里的对象名带进程级随机后缀，
//! 并行跑多个测试进程也不会互相删表。

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    BrowseOptions, ColumnMeta, ConnectionParams, Driver, DriverSession, ExecuteRequest, Filter,
    FilterCondition, FilterOperator, ObjectKind, ObjectRef, PageRequest, QualifiedName,
    ReadOnlyReason, Row, RowOrder, SortDirection, SortKey, TableDataProvider, TablePage, TlsMode,
    TotalKind, Value,
};
use sqarvo_driver_mariadb::MariaDbDriver;

// ---------------------------------------------------------------------------
// 基础设施
// ---------------------------------------------------------------------------

fn connection_params() -> Option<(ConnectionParams, sqarvo_driver_api::Secret)> {
    let Ok(url) = std::env::var("SQARVO_MARIADB_URL") else {
        eprintln!("跳过：未设置 SQARVO_MARIADB_URL（示例 mysql://user:***@127.0.0.1:53306/db）");
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
        credential_ref: Some("sqarvo:conn/browse111".to_owned()),
        tls: TlsMode::Preferred,
        tls_server_name: None,
        properties: BTreeMap::new(),
    };
    Some((params, sqarvo_driver_api::Secret::new(password)))
}

async fn session() -> Option<Box<dyn DriverSession>> {
    let (params, secret) = connection_params()?;
    Some(
        MariaDbDriver::new()
            .connect(&params, Some(&secret))
            .await
            .expect("连接 MariaDB 测试库"),
    )
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

/// 进程级随机后缀：并行跑同一个测试二进制（或多个 CI 作业共用一个库）也不会撞名。
fn nonce() -> &'static str {
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间早于 UNIX 纪元")
            .subsec_nanos();
        format!("{nanos:08x}")
    })
}

/// 表名：`bp111_<进程后缀>_<用例名>`。
fn table_name(case: &str) -> String {
    format!("bp111_{}_{case}", nonce())
}

async fn create_table(session: &dyn DriverSession, name: &str, body: &str) {
    exec(session, &format!("DROP TABLE IF EXISTS `{name}`")).await;
    exec(
        session,
        &format!("CREATE TABLE `{name}` ({body}) ENGINE=InnoDB"),
    )
    .await;
}

async fn drop_table(session: &dyn DriverSession, name: &str) {
    exec(session, &format!("DROP TABLE IF EXISTS `{name}`")).await;
}

fn object(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, name),
        kind: ObjectKind::Table,
    }
}

fn page(size: u32, token: Option<String>) -> PageRequest {
    PageRequest {
        page_size: size,
        page_token: token,
    }
}

fn provider(session: &dyn DriverSession) -> &dyn TableDataProvider {
    session.table_data().expect("MariaDB 必须提供表数据能力")
}

fn sort_key(column: &str, direction: SortDirection) -> SortKey {
    SortKey {
        column: column.to_owned(),
        direction,
    }
}

fn condition(column: &str, operator: FilterOperator, value: Option<Value>) -> FilterCondition {
    FilterCondition {
        column: column.to_owned(),
        operator,
        value,
    }
}

fn options(sort: Vec<SortKey>, filter: Option<Filter>, offset: Option<u64>) -> BrowseOptions {
    BrowseOptions {
        offset,
        sort,
        filter,
        include_total: false,
        total_timeout: None,
    }
}

fn with_total(mut options: BrowseOptions) -> BrowseOptions {
    options.include_total = true;
    options
}

/// 整型列（有符号/无符号都归一成 i64）。
fn int_at(row: &Row, index: usize) -> i64 {
    match &row[index].value {
        Value::Int(value) => *value,
        Value::UInt(value) => i64::try_from(*value).expect("整数应在 i64 范围内"),
        other => panic!("期望整数列，实际 {other:?}"),
    }
}

fn text_at(row: &Row, index: usize) -> &str {
    match &row[index].value {
        Value::Text(value) => value,
        other => panic!("期望文本列，实际 {other:?}"),
    }
}

fn ids(page: &TablePage, index: usize) -> Vec<i64> {
    page.rows.iter().map(|row| int_at(row, index)).collect()
}

async fn browse(
    session: &dyn DriverSession,
    object: &ObjectRef,
    page: &PageRequest,
    options: &BrowseOptions,
) -> TablePage {
    provider(session)
        .browse(object, page, options)
        .await
        .expect("浏览表数据")
}

// ---------------------------------------------------------------------------
// 排序
// ---------------------------------------------------------------------------

/// 排序必须**真的改变行序**：驱动把 `sort` 丢掉时这条用例立刻失败。
#[tokio::test]
async fn sort_direction_changes_the_returned_row_order() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("sort");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, v INT NOT NULL",
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO `{table}` (id, v) VALUES (1,30),(2,10),(3,50),(4,20),(5,40)"),
    )
    .await;
    let object = object(&*session, &table);
    let provider = provider(&*session);

    let asc = provider
        .browse(
            &object,
            &page(10, None),
            &options(vec![sort_key("v", SortDirection::Asc)], None, None),
        )
        .await
        .expect("升序浏览");
    assert_eq!(ids(&asc, 0), vec![2, 4, 1, 5, 3], "按 v 升序");
    assert_eq!(asc.ordering, RowOrder::RequestedSort);

    let desc = provider
        .browse(
            &object,
            &page(10, None),
            &options(vec![sort_key("v", SortDirection::Desc)], None, None),
        )
        .await
        .expect("降序浏览");
    assert_eq!(ids(&desc, 0), vec![3, 5, 1, 4, 2], "按 v 降序");
    assert_eq!(desc.ordering, RowOrder::RequestedSort);

    // 不请求排序 → v0.2.2 的主键升序，与上面两个方向都不同：
    // 这证明行序来自 SQL 的 ORDER BY，而不是驱动碰巧返回的顺序。
    let default = provider
        .browse(&object, &page(10, None), &BrowseOptions::default())
        .await
        .expect("默认浏览");
    assert_eq!(ids(&default, 0), vec![1, 2, 3, 4, 5]);
    assert_eq!(default.ordering, RowOrder::PrimaryKeyAscending);

    drop_table(&*session, &table).await;
}

/// 多列排序：先按 grp 升序，组内按 score 降序。
#[tokio::test]
async fn multi_column_sort_applies_keys_in_the_given_order() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("multi");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, grp CHAR(1) NOT NULL, score INT NOT NULL",
    )
    .await;
    exec(
        &*session,
        &format!(
            "INSERT INTO `{table}` (id, grp, score) VALUES \
             (1,'a',2),(2,'a',1),(3,'b',5),(4,'b',4),(5,'a',3)"
        ),
    )
    .await;
    let object = object(&*session, &table);

    let sorted = browse(
        &*session,
        &object,
        &page(10, None),
        &options(
            vec![
                sort_key("grp", SortDirection::Asc),
                sort_key("score", SortDirection::Desc),
            ],
            None,
            None,
        ),
    )
    .await;
    // a 组内 3 > 2 > 1（id 5,1,2），b 组内 5 > 4（id 3,4）。
    assert_eq!(ids(&sorted, 0), vec![5, 1, 2, 3, 4]);
    assert_eq!(sorted.ordering, RowOrder::RequestedSort);

    // 列名按数据库语义匹配（大小写不敏感），但拼进 SQL 的是 schema 里的规范名。
    let upper = browse(
        &*session,
        &object,
        &page(10, None),
        &options(vec![sort_key("GRP", SortDirection::Asc)], None, None),
    )
    .await;
    assert_eq!(upper.rows.len(), 5);

    drop_table(&*session, &table).await;
}

// ---------------------------------------------------------------------------
// 筛选
// ---------------------------------------------------------------------------

/// `LIKE` 的 `%` / `_` 是通配符，`\%` / `\_` 是字面量——匹配串走参数绑定，
/// 驱动不改写它，语义由 MariaDB 决定。
#[tokio::test]
async fn like_patterns_follow_mariadb_wildcard_and_escape_semantics() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("like");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, txt VARCHAR(16)",
    )
    .await;
    exec(
        &*session,
        &format!(
            "INSERT INTO `{table}` (id, txt) VALUES \
             (1,'50%off'),(2,'50xoff'),(3,'a_b'),(4,'axb'),(5,'plain')"
        ),
    )
    .await;
    let object = object(&*session, &table);

    let like = |pattern: &str| {
        let filter = Filter::new(vec![condition(
            "txt",
            FilterOperator::Like,
            Some(Value::Text(pattern.to_owned())),
        )]);
        options(Vec::new(), Some(filter), None)
    };

    // `%` 是通配符：匹配所有 5 行。
    let all = browse(&*session, &object, &page(10, None), &like("%")).await;
    assert_eq!(all.rows.len(), 5, "% 必须按通配符解释");

    // `50\%off`：只匹配字面量百分号。
    let literal_percent = browse(&*session, &object, &page(10, None), &like("50\\%off")).await;
    assert_eq!(ids(&literal_percent, 0), vec![1]);

    // `50%off` 未转义：`%` 仍是通配符，两行都匹配。
    let wildcard = browse(&*session, &object, &page(10, None), &like("50%off")).await;
    assert_eq!(ids(&wildcard, 0), vec![1, 2]);

    // `_` 是单字符通配符：`a_b` 匹配 a_b 与 axb，`a\_b` 只匹配字面下划线。
    let underscore = browse(&*session, &object, &page(10, None), &like("a_b")).await;
    assert_eq!(ids(&underscore, 0), vec![3, 4]);
    let literal_underscore = browse(&*session, &object, &page(10, None), &like("a\\_b")).await;
    assert_eq!(ids(&literal_underscore, 0), vec![3]);

    drop_table(&*session, &table).await;
}

/// NULL 比较只能走 `IS NULL` / `IS NOT NULL`；`= NULL` 是恒未知，驱动预先拒绝。
#[tokio::test]
async fn null_comparisons_use_is_null_operators() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("nulls");
    create_table(&*session, &table, "id INT NOT NULL PRIMARY KEY, v INT NULL").await;
    exec(
        &*session,
        &format!("INSERT INTO `{table}` (id, v) VALUES (1,10),(2,NULL),(3,30),(4,NULL)"),
    )
    .await;
    let object = object(&*session, &table);

    let is_null = browse(
        &*session,
        &object,
        &page(10, None),
        &options(
            Vec::new(),
            Some(Filter::new(vec![condition(
                "v",
                FilterOperator::IsNull,
                None,
            )])),
            None,
        ),
    )
    .await;
    assert_eq!(ids(&is_null, 0), vec![2, 4]);

    let not_null = browse(
        &*session,
        &object,
        &page(10, None),
        &options(
            Vec::new(),
            Some(Filter::new(vec![condition(
                "v",
                FilterOperator::IsNotNull,
                None,
            )])),
            None,
        ),
    )
    .await;
    assert_eq!(ids(&not_null, 0), vec![1, 3]);

    // `v = NULL` 会静默返回空页；驱动在发 SQL 前就拒绝，并指路 IS NULL。
    let error = provider(&*session)
        .browse(
            &object,
            &page(10, None),
            &options(
                Vec::new(),
                Some(Filter::new(vec![condition(
                    "v",
                    FilterOperator::Eq,
                    Some(Value::Null),
                )])),
                None,
            ),
        )
        .await
        .expect_err("比较值不能是 NULL");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);
    assert!(error.message().contains("IS NULL"));

    drop_table(&*session, &table).await;
}

/// 未知列（排序或筛选）在发送 SQL 之前就变成 `config.invalid`，不让数据库报语法错。
#[tokio::test]
async fn unknown_columns_are_rejected_before_any_sql_is_sent() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("unknown");
    create_table(&*session, &table, "id INT NOT NULL PRIMARY KEY, v INT NULL").await;
    exec(
        &*session,
        &format!("INSERT INTO `{table}` (id, v) VALUES (1,1)"),
    )
    .await;
    let object = object(&*session, &table);
    let provider = provider(&*session);

    let bad_sort = provider
        .browse(
            &object,
            &page(10, None),
            &options(
                vec![sort_key("no_such_column", SortDirection::Asc)],
                None,
                None,
            ),
        )
        .await
        .expect_err("未知排序列必须被拒绝");
    assert_eq!(bad_sort.code(), ErrorCode::CONFIG_INVALID);
    assert!(
        !bad_sort.message().to_ascii_uppercase().contains("ORDER BY"),
        "错误文案不得回显 SQL：{}",
        bad_sort.message()
    );

    let bad_filter = provider
        .browse(
            &object,
            &page(10, None),
            &options(
                Vec::new(),
                Some(Filter::new(vec![condition(
                    "no_such_column",
                    FilterOperator::Eq,
                    Some(Value::Int(1)),
                )])),
                None,
            ),
        )
        .await
        .expect_err("未知筛选列必须被拒绝");
    assert_eq!(bad_filter.code(), ErrorCode::CONFIG_INVALID);

    drop_table(&*session, &table).await;
}

/// 多条件按 AND 组合，与排序、offset 一起生效。
#[tokio::test]
async fn filters_combine_with_sort_and_offset_using_and() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("combo");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, grp INT NOT NULL, score INT NOT NULL",
    )
    .await;
    exec(
        &*session,
        &format!(
            "INSERT INTO `{table}` (id, grp, score) SELECT seq, seq % 2, seq \
             FROM seq_1_to_20"
        ),
    )
    .await;
    let object = object(&*session, &table);

    // grp = 0 且 score >= 8，按 score 降序，跳过前 3 行取 4 行。
    let page = browse(
        &*session,
        &object,
        &page(4, None),
        &options(
            vec![sort_key("score", SortDirection::Desc)],
            Some(Filter::new(vec![
                condition("grp", FilterOperator::Eq, Some(Value::Int(0))),
                condition("score", FilterOperator::Ge, Some(Value::Int(8))),
            ])),
            Some(3),
        ),
    )
    .await;
    // grp=0 的偶数 score（8..20）降序为 20,18,16,14,12,10,8；offset 3 → 14,12,10,8。
    assert_eq!(ids(&page, 0), vec![14, 12, 10, 8]);
    assert!(page.rows.iter().all(|row| int_at(row, 1) == 0));
    assert!(!page.has_more, "取到真正末页");
    assert!(page.next_page_token.is_none(), "offset 模式不签发游标");

    drop_table(&*session, &table).await;
}

// ---------------------------------------------------------------------------
// 分页：offset 与游标
// ---------------------------------------------------------------------------

/// 同一次查询用 offset 逐页取与用键集游标逐页取，必须得到同一个行序列。
#[tokio::test]
async fn offset_pages_match_cursor_pages() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("equiv");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, v INT NOT NULL",
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO `{table}` (id, v) SELECT seq, seq * 2 FROM seq_1_to_12"),
    )
    .await;
    let object = object(&*session, &table);

    // 游标路径（v0.2.2 行为）：无 sort、无 offset。
    let mut cursor_ids = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = browse(
            &*session,
            &object,
            &page(5, token.clone()),
            &BrowseOptions::default(),
        )
        .await;
        assert_eq!(page.ordering, RowOrder::PrimaryKeyAscending);
        cursor_ids.extend(ids(&page, 0));
        match page.next_page_token {
            Some(next) => {
                assert!(page.has_more);
                token = Some(next);
            }
            None => break,
        }
    }
    assert_eq!(cursor_ids, (1..=12).collect::<Vec<_>>());

    // offset 路径：同样的页大小，自己推进偏移量。
    let mut offset_ids = Vec::new();
    for offset in (0..12).step_by(5) {
        let page = browse(
            &*session,
            &object,
            &page(5, None),
            &options(Vec::new(), None, (offset > 0).then_some(offset)),
        )
        .await;
        offset_ids.extend(ids(&page, 0));
    }
    assert_eq!(offset_ids, cursor_ids, "offset 与游标必须给出同一序列");

    drop_table(&*session, &table).await;
}

/// 越界 offset 返回空页 + `has_more=false` + 如实总数（不是错误）。
#[tokio::test]
async fn offset_beyond_total_returns_an_empty_page_with_the_real_total() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("beyond");
    create_table(&*session, &table, "id INT NOT NULL PRIMARY KEY").await;
    exec(
        &*session,
        &format!("INSERT INTO `{table}` (id) SELECT seq FROM seq_1_to_12"),
    )
    .await;
    let object = object(&*session, &table);

    let page = browse(
        &*session,
        &object,
        &page(5, None),
        &with_total(options(Vec::new(), None, Some(100))),
    )
    .await;
    assert_eq!(page.rows.len(), 0, "越界 offset 必须返回空页");
    assert!(!page.has_more);
    assert!(page.next_page_token.is_none());
    assert_eq!(page.total_kind, TotalKind::Exact);
    assert_eq!(page.total_rows, Some(12));

    drop_table(&*session, &table).await;
}

/// `page_token` 只对应主键升序：和 `sort`、`offset` 并用都必须显式失败。
#[tokio::test]
async fn cursor_conflicts_with_sort_and_offset() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("conflict");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, v INT NOT NULL",
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO `{table}` (id, v) SELECT seq, seq FROM seq_1_to_6"),
    )
    .await;
    let object = object(&*session, &table);
    let provider = provider(&*session);

    let first = provider
        .browse(&object, &page(2, None), &BrowseOptions::default())
        .await
        .expect("第一页");
    let token = first.next_page_token.expect("应签发游标");

    let with_sort = provider
        .browse(
            &object,
            &page(2, Some(token.clone())),
            &options(vec![sort_key("v", SortDirection::Desc)], None, None),
        )
        .await
        .expect_err("排序不能沿用主键游标");
    assert_eq!(with_sort.code(), ErrorCode::CONFIG_INVALID);

    let with_offset = provider
        .browse(
            &object,
            &page(2, Some(token)),
            &options(Vec::new(), None, Some(4)),
        )
        .await
        .expect_err("offset 与游标互斥");
    assert_eq!(with_offset.code(), ErrorCode::CONFIG_INVALID);

    drop_table(&*session, &table).await;
}

// ---------------------------------------------------------------------------
// 总数三态
// ---------------------------------------------------------------------------

/// `Unknown`（未请求）与 `Exact`（小表实时计数）必须自洽：未请求时**不得**给数字。
#[tokio::test]
async fn total_kind_unknown_and_exact_are_self_consistent() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("total");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, grp INT NOT NULL",
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO `{table}` (id, grp) SELECT seq, seq % 3 FROM seq_1_to_12"),
    )
    .await;
    let object = object(&*session, &table);

    // 未请求总数：Unknown + 没有数字（v0.2.2 行为）。
    let unknown = browse(
        &*session,
        &object,
        &page(5, None),
        &BrowseOptions::default(),
    )
    .await;
    assert_eq!(unknown.total_kind, TotalKind::Unknown);
    assert_eq!(unknown.total_rows, None, "Unknown 时不得编一个数字");
    assert!(unknown.has_more);
    assert!(unknown.next_page_token.is_some());

    // 请求总数（无筛选）：小表走 COUNT(*)，必须是 Exact 且等于真实行数。
    let exact = browse(
        &*session,
        &object,
        &page(5, None),
        &with_total(BrowseOptions::default()),
    )
    .await;
    assert_eq!(exact.total_kind, TotalKind::Exact);
    assert_eq!(exact.total_rows, Some(12));

    // 带筛选：统计信息反映不了筛选后的行数，只能精确计数。
    let filtered = browse(
        &*session,
        &object,
        &page(5, None),
        &with_total(options(
            Vec::new(),
            Some(Filter::new(vec![condition(
                "grp",
                FilterOperator::Eq,
                Some(Value::Int(1)),
            )])),
            None,
        )),
    )
    .await;
    assert_eq!(filtered.total_kind, TotalKind::Exact);
    // seq % 3 == 1 在 1..=12 里有 1,4,7,10 四个。
    assert_eq!(filtered.total_rows, Some(4));
    assert_eq!(filtered.rows.len(), 4);
    assert!(!filtered.has_more, "总数与页内容必须一致");

    drop_table(&*session, &table).await;
}

/// 计数超时（#127）：`timeout_millis` 到点后放弃统计（`UNKNOWN` + `total_skipped`），
/// 本页照常返回，且服务端不残留计数语句（`PROCESSLIST` 断言）。
///
/// 20 万行 + 筛选：无筛选时 15 万行以上会走 `TABLE_ROWS` 估计，只有带筛选才强制跑
/// `COUNT(*)`，这里正是要给那次计数一个几毫秒的上界。
#[tokio::test]
async fn count_timeout_skips_the_total_and_leaves_no_statement_behind() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("count_timeout");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, bucket INT NOT NULL, pad CHAR(8) NOT NULL",
    )
    .await;
    exec(
        &*session,
        &format!(
            "INSERT INTO `{table}` (id, bucket, pad) \
             SELECT seq, seq % 4, 'x' FROM seq_1_to_200000"
        ),
    )
    .await;
    let object = object(&*session, &table);
    let filtered = || {
        Filter::new(vec![condition(
            "bucket",
            FilterOperator::Eq,
            Some(Value::Int(1)),
        )])
    };
    let short_timeout = BrowseOptions {
        filter: Some(filtered()),
        include_total: true,
        total_timeout: Some(std::time::Duration::from_millis(1)),
        ..BrowseOptions::default()
    };

    let started = Instant::now();
    let first_page = browse(&*session, &object, &page(50, None), &short_timeout).await;
    let elapsed = started.elapsed();

    assert_eq!(
        first_page.total_kind,
        TotalKind::Unknown,
        "超时必须是 UNKNOWN，不得给出半截数量或伪称 EXACT"
    );
    assert!(
        first_page.total_skipped,
        "请求了却没算完必须显式标记 total_skipped"
    );
    assert_eq!(first_page.total_rows, None);
    assert_eq!(first_page.rows.len(), 50, "本页数据不受计数超时影响");
    assert!(
        first_page.rows.iter().all(|row| int_at(row, 1) == 1),
        "筛选必须仍然对整页生效"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "超时后必须尽快返回，实际 {elapsed:?}"
    );

    // 计数语句不得留在服务端。断言语句自己也会出现在 `PROCESSLIST` 里，因此用
    // `CONCAT` 把模式拆开：检查语句的 INFO 不含连续的「SELECT COUNT(*) FROM `」，
    // 不会自己命中自己。
    let statement = format!(
        "SELECT COUNT(*) FROM information_schema.PROCESSLIST \
         WHERE INFO LIKE CONCAT('SELECT COUNT', '(*) FROM ', '`%', '{table}', '`%')"
    );
    let leftover = session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: statement,
            max_rows: None,
            timeout: None,
        })
        .await
        .expect("查询 PROCESSLIST");
    let leftover = leftover.first_batch.expect("PROCESSLIST 应返回一行").rows[0][0]
        .value
        .clone();
    match leftover {
        Value::Int(count) => assert_eq!(count, 0, "超时后服务端仍残留该表的 COUNT(*)"),
        Value::UInt(count) => assert_eq!(count, 0, "超时后服务端仍残留该表的 COUNT(*)"),
        other => panic!("PROCESSLIST 计数类型异常：{other:?}"),
    }

    // 会话连接被 KILL QUERY 打断后必须仍然可用：再跑一次完整计数应拿到真实值。
    let after = browse(
        &*session,
        &object,
        &page(10, None),
        &BrowseOptions {
            filter: Some(filtered()),
            include_total: true,
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(
        after.total_kind,
        TotalKind::Exact,
        "KILL QUERY 只打断那一条语句，会话必须继续可用"
    );
    assert_eq!(after.total_rows, Some(50_000));
    assert!(!after.total_skipped, "算出来了就不能标成已跳过");

    drop_table(&*session, &table).await;
}

/// 大表：`include_total` 走 `TABLE_ROWS` 估计（`Estimate`），并记录 10 万行级
/// `offset=90000` 的耗时（Issue #111 的观察值，不是性能承诺）。
#[tokio::test]
async fn large_table_total_is_an_estimate_and_large_offset_is_timed() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("large");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, bucket INT NOT NULL, pad CHAR(8) NOT NULL",
    )
    .await;
    exec(
        &*session,
        &format!(
            "INSERT INTO `{table}` (id, bucket, pad) \
             SELECT seq, seq % 10, 'x' FROM seq_1_to_150000"
        ),
    )
    .await;
    // InnoDB 的 TABLE_ROWS 来自持久化统计信息；显式 ANALYZE 后才有确定的量级，
    // 否则新建表可能仍报 0 而落回 COUNT(*)。
    exec(&*session, &format!("ANALYZE TABLE `{table}`")).await;
    let object = object(&*session, &table);

    let summary = browse(
        &*session,
        &object,
        &page(100, None),
        &with_total(BrowseOptions::default()),
    )
    .await;
    assert_eq!(
        summary.total_kind,
        TotalKind::Estimate,
        "15 万行的表必须退回统计信息，而不是跑一次全表 COUNT(*)"
    );
    let estimate = summary.total_rows.expect("Estimate 必须带估计值");
    assert!(
        (100_001..=1_000_000).contains(&estimate),
        "估计值量级不合理：{estimate}"
    );

    // 带筛选时不能用整表统计冒充：退回精确计数。
    let filtered = browse(
        &*session,
        &object,
        &page(10, None),
        &with_total(options(
            Vec::new(),
            Some(Filter::new(vec![condition(
                "bucket",
                FilterOperator::Eq,
                Some(Value::Int(3)),
            )])),
            None,
        )),
    )
    .await;
    assert_eq!(filtered.total_kind, TotalKind::Exact);
    assert_eq!(filtered.total_rows, Some(15_000));
    assert!(filtered.rows.iter().all(|row| int_at(row, 1) == 3));

    // 大 offset 的代价：InnoDB 要扫描前面 9 万行。只记录耗时，不作承诺。
    // 先量一次首页（offset 未设置）作对照，再量深 offset。
    let first_started = Instant::now();
    let first = browse(
        &*session,
        &object,
        &page(100, None),
        &options(Vec::new(), None, None),
    )
    .await;
    let first_elapsed = first_started.elapsed();
    assert_eq!(first.rows.len(), 100);
    assert_eq!(int_at(&first.rows[0], 0), 1);

    let started = Instant::now();
    let deep = browse(
        &*session,
        &object,
        &page(100, None),
        &options(Vec::new(), None, Some(90_000)),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(deep.rows.len(), 100);
    assert_eq!(int_at(&deep.rows[0], 0), 90_001);
    assert_eq!(int_at(&deep.rows[99], 0), 90_100);
    println!(
        "MariaDB 15 万行：offset=0 取 100 行耗时 {first_elapsed:?}，\
         offset=90000 取 100 行耗时 {elapsed:?}，总数估计 {estimate}（仅观察值）"
    );

    drop_table(&*session, &table).await;
}

// ---------------------------------------------------------------------------
// 无主键表
// ---------------------------------------------------------------------------

/// 无主键表允许排序 / 筛选 / 计数，但顺序仍标 `Unstable` 且只读（ADR 0022 §6）。
#[tokio::test]
async fn table_without_primary_key_allows_sort_filter_and_total() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("nopk");
    create_table(&*session, &table, "txt VARCHAR(16), level INT NOT NULL").await;
    exec(
        &*session,
        &format!(
            "INSERT INTO `{table}` (txt, level) VALUES \
             ('a',1),('b',2),('c',3),('d',1),('e',2),('f',3)"
        ),
    )
    .await;
    let object = object(&*session, &table);
    assert!(
        provider(&*session)
            .primary_key(&object)
            .await
            .expect("主键定义")
            .is_empty(),
        "无主键表的主键定义必须为空"
    );

    let page = browse(
        &*session,
        &object,
        &page(10, None),
        &with_total(options(
            vec![sort_key("level", SortDirection::Desc)],
            Some(Filter::new(vec![condition(
                "level",
                FilterOperator::Ge,
                Some(Value::Int(2)),
            )])),
            None,
        )),
    )
    .await;
    assert_eq!(ids(&page, 1), vec![3, 3, 2, 2], "排序与筛选都必须下推");
    assert_eq!(page.ordering, RowOrder::Unstable);
    assert!(page.read_only);
    assert_eq!(page.read_only_reason, Some(ReadOnlyReason::NoPrimaryKey));
    assert_eq!(page.total_kind, TotalKind::Exact);
    assert_eq!(page.total_rows, Some(4));
    // 无主键表不签发游标（v0.2.2 语义不变）。
    assert!(page.next_page_token.is_none());
    assert!(!page.has_more);

    drop_table(&*session, &table).await;
}

/// 列元数据必须与 SQL 的 SELECT 列表一致（排序/筛选不改变列集合与顺序）。
#[tokio::test]
async fn pushed_down_browse_keeps_column_metadata_aligned() {
    let Some(session) = session().await else {
        return;
    };
    let table = table_name("cols");
    create_table(
        &*session,
        &table,
        "id INT NOT NULL PRIMARY KEY, name VARCHAR(16) NULL, note VARCHAR(8) NULL",
    )
    .await;
    exec(
        &*session,
        &format!("INSERT INTO `{table}` (id, name, note) VALUES (1,'a','n')"),
    )
    .await;
    let object = object(&*session, &table);

    let page = browse(
        &*session,
        &object,
        &page(10, None),
        &with_total(options(
            vec![sort_key("name", SortDirection::Asc)],
            Some(Filter::new(vec![condition(
                "note",
                FilterOperator::IsNotNull,
                None,
            )])),
            None,
        )),
    )
    .await;
    assert_eq!(
        page.columns
            .iter()
            .map(|c: &ColumnMeta| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["id", "name", "note"]
    );
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].len(), 3);
    assert_eq!(text_at(&page.rows[0], 1), "a");

    drop_table(&*session, &table).await;
}

/// 跨驱动不变量（`sqarvo_driver_testkit::browse_invariants`，ADR 0022）：
/// `offset` 与 `page_token` 互斥、排序下 `offset` 翻页无重无漏、总数三态自洽、
/// 非法排序/筛选显式失败、无主键表只读且顺序不稳定、能力位与排序行为一致。
///
/// 与上面的用例互补：不重复方言细节（`LIKE` 通配符、NULL 三值逻辑、估算阈值），
/// 只断言「MariaDB 与 SQLite 必须给出同样结论」的契约性质。
#[tokio::test]
async fn table_browse_satisfies_shared_testkit_invariants() {
    let Some(session) = session().await else {
        return;
    };
    let keyed = table_name("invkeys");
    let unkeyed = table_name("invlogs");
    let keyed_create = format!(
        "CREATE TABLE `{keyed}` (id INT NOT NULL PRIMARY KEY, grp INT NOT NULL, name VARCHAR(8) NOT NULL)"
    );
    let keyed_insert = format!(
        "INSERT INTO `{keyed}` (id, grp, name) VALUES \
         (1,2,'n1'),(2,3,'n2'),(3,1,'n3'),(4,3,'n4'),(5,1,'n5'),\
         (6,2,'n6'),(7,1,'n7'),(8,3,'n8'),(9,2,'n9')"
    );
    let unkeyed_create = format!(
        "CREATE TABLE `{unkeyed}` (id INT NULL, grp INT NOT NULL, name VARCHAR(8) NOT NULL)"
    );
    let unkeyed_insert = format!(
        "INSERT INTO `{unkeyed}` (id, grp, name) VALUES \
         (1,2,'n1'),(2,3,'n2'),(3,1,'n3'),(4,3,'n4'),(5,1,'n5'),\
         (6,2,'n6'),(7,1,'n7'),(8,3,'n8'),(9,2,'n9')"
    );
    let keyed_drop = format!("DROP TABLE IF EXISTS `{keyed}`");
    let keyed_count = format!("SELECT COUNT(*) FROM `{keyed}`");
    let unkeyed_drop = format!("DROP TABLE IF EXISTS `{unkeyed}`");
    let unkeyed_count = format!("SELECT COUNT(*) FROM `{unkeyed}`");

    let fixture = sqarvo_driver_testkit::BrowseFixture {
        keyed: sqarvo_driver_testkit::BrowseTable {
            name: &keyed,
            create_sql: &keyed_create,
            insert_sql: &keyed_insert,
            drop_sql: &keyed_drop,
            count_sql: &keyed_count,
            primary_key: Some("id"),
            sort_column: "grp",
            row_count: 9,
        },
        unkeyed: sqarvo_driver_testkit::BrowseTable {
            name: &unkeyed,
            create_sql: &unkeyed_create,
            insert_sql: &unkeyed_insert,
            drop_sql: &unkeyed_drop,
            count_sql: &unkeyed_count,
            primary_key: None,
            sort_column: "grp",
            row_count: 9,
        },
    };

    sqarvo_driver_testkit::browse_invariants(&*session, &fixture)
        .await
        .unwrap_or_else(|error| panic!("MariaDB 未通过表浏览不变量：{error:?}"));

    drop_table(&*session, &keyed).await;
    drop_table(&*session, &unkeyed).await;
}
