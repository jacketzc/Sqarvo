//! SQLite 表浏览的排序 / 筛选 / offset 分页 / 精确计数下推（#110，ADR 0022）。
//!
//! 这些用例锁的是**下推后的真实结果**，不是 SQL 文本：
//!
//! - 排序改变行序（与不排序的主键序不同）、多列、ASC/DESC；
//! - 筛选的 `%` / `_` 按字面量、NULL 比较的坑（`= NULL` 被拒、`NE` 不返回 NULL 行）、
//!   未知列与类型不兼容在发送 SQL 前被拒（`config.invalid`，不是数据库语法错）；
//! - offset 分页与游标分页得到同一批行，越界返回空页；
//! - `total_kind` 三态与 `total_rows` 自洽；
//! - 无主键表可以排序/筛选/计数，但仍只读、`ordering = Unstable`、不签发游标。

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    BrowseOptions, ConnectionParams, Driver, DriverSession, ExecuteRequest, Filter,
    FilterCondition, FilterOperator, ObjectKind, ObjectRef, PageRequest, QualifiedName, Row,
    RowOrder, SortDirection, SortKey, TableDataProvider, TablePage, TlsMode, TotalKind, Value,
};
use sqarvo_driver_sqlite::SqliteDriver;
use tempfile::TempDir;

async fn session() -> (TempDir, Box<dyn DriverSession>) {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("browse-options.db");
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

fn page(size: u32, token: Option<String>) -> PageRequest {
    PageRequest {
        page_size: size,
        page_token: token,
    }
}

fn ids(page: &TablePage) -> Vec<i64> {
    page.rows.iter().map(int_key).collect()
}

/// 从行里取第一列的主键值（测试表的第一列都是整型主键）。
fn int_key(row: &Row) -> i64 {
    match &row[0].value {
        Value::Int(value) => *value,
        other => panic!("期望 INTEGER 主键，实际 {other:?}"),
    }
}

fn sort(column: &str, direction: SortDirection) -> SortKey {
    SortKey {
        column: column.to_owned(),
        direction,
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "调用点直接写进 Option<Filter> 字段，统一形态比每处包 Some 更清楚"
)]
fn filter(conditions: Vec<FilterCondition>) -> Option<Filter> {
    Some(Filter::new(conditions))
}

fn condition(column: &str, operator: FilterOperator, value: Option<Value>) -> FilterCondition {
    FilterCondition {
        column: column.to_owned(),
        operator,
        value,
    }
}

async fn browse(
    provider: &dyn TableDataProvider,
    object: &ObjectRef,
    page_size: u32,
    options: BrowseOptions,
) -> TablePage {
    provider
        .browse(object, &page(page_size, None), &options)
        .await
        .expect("浏览一页")
}

/// 排序真改变行序（与不排序的主键序不同），多列按给出顺序，ASC/DESC 都生效。
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "同一份数据上串起默认序、单列 ASC/DESC、多列与 offset 分页，拆开会丢对照"
)]
async fn table_data_sort_orders_by_requested_columns() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE scores (id INTEGER PRIMARY KEY, team INTEGER, score INTEGER)",
    )
    .await;
    exec(
        session.as_ref(),
        "INSERT INTO scores VALUES (1,1,10),(2,2,30),(3,1,30),(4,2,20),(5,1,20)",
    )
    .await;

    let provider = session.table_data().expect("SQLite 必须提供表数据能力");
    let scores = object(session.as_ref(), "scores");

    // 不排序：主键升序（v0.2.2 行为），首行 id = 1。
    let default = browse(provider, &scores, 100, BrowseOptions::default()).await;
    assert_eq!(ids(&default), vec![1, 2, 3, 4, 5]);
    assert_eq!(default.ordering, RowOrder::PrimaryKeyAscending);
    assert_eq!(default.total_kind, TotalKind::Unknown);

    // 单列 DESC：30 → 20 → 10，并列按主键升序打破。
    let desc = browse(
        provider,
        &scores,
        100,
        BrowseOptions {
            sort: vec![sort("score", SortDirection::Desc)],
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&desc), vec![2, 3, 4, 5, 1]);
    assert_eq!(desc.ordering, RowOrder::RequestedSort);
    assert_ne!(
        int_key(&desc.rows[0]),
        int_key(&default.rows[0]),
        "排序必须真的改变行序，而不是原样返回"
    );

    // 单列 ASC：10 → 20 → 30。
    let asc = browse(
        provider,
        &scores,
        100,
        BrowseOptions {
            sort: vec![sort("score", SortDirection::Asc)],
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&asc), vec![1, 4, 5, 2, 3]);

    // 多列：score DESC 先排，score 相等时按 team ASC。
    let multi = browse(
        provider,
        &scores,
        100,
        BrowseOptions {
            sort: vec![
                sort("score", SortDirection::Desc),
                sort("team", SortDirection::Asc),
            ],
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&multi), vec![3, 2, 5, 4, 1]);
    assert_eq!(multi.ordering, RowOrder::RequestedSort);
    assert!(multi.next_page_token.is_none(), "排序翻页不使用主键游标");

    // 排序 + offset 分页：每页 2 行，拼起来与整表排序一致。
    let mut paged = Vec::new();
    for offset in [0_u64, 2, 4] {
        let page = browse(
            provider,
            &scores,
            2,
            BrowseOptions {
                offset: Some(offset),
                sort: vec![
                    sort("score", SortDirection::Desc),
                    sort("team", SortDirection::Asc),
                ],
                ..BrowseOptions::default()
            },
        )
        .await;
        assert!(page.next_page_token.is_none(), "offset 分页不签发主键游标");
        paged.extend(ids(&page));
    }
    assert_eq!(paged, ids(&multi));
}

/// `%` / `_` 按字面量，NULL 比较的坑在发送 SQL 前被拒。
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "LIKE 字面量、NULL 三值逻辑、比较算子与 AND 组合共用一份数据，拆开会丢语义对照"
)]
async fn table_data_filter_supports_like_and_null_pitfalls() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, qty INTEGER)",
    )
    .await;
    exec(
        session.as_ref(),
        "INSERT INTO items VALUES (1,'a_b',1),(2,'axb',2),(3,'50%',3),(4,'500',4),(5,NULL,NULL)",
    )
    .await;

    let provider = session.table_data().expect("表数据能力");
    let items = object(session.as_ref(), "items");

    // `_` 是字面量：只有 a_b 命中，axb 不命中。
    let underscore = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![condition(
                "name",
                FilterOperator::Like,
                Some(Value::Text("a_b".to_owned())),
            )]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&underscore), vec![1], "LIKE 里的 _ 必须按字面量匹配");

    // `%` 是字面量：50% 命中，500 不命中。
    let percent = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![condition(
                "name",
                FilterOperator::Like,
                Some(Value::Text("50%".to_owned())),
            )]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&percent), vec![3], "LIKE 里的 % 必须按字面量匹配");

    // 包含匹配（无通配符输入）：'a' 命中 a_b 与 axb。
    let contains = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![condition(
                "name",
                FilterOperator::Like,
                Some(Value::Text("a".to_owned())),
            )]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&contains), vec![1, 2]);

    // IS NULL / IS NOT NULL。
    let is_null = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![condition("qty", FilterOperator::IsNull, None)]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&is_null), vec![5]);
    let not_null = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![condition("qty", FilterOperator::IsNotNull, None)]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&not_null), vec![1, 2, 3, 4]);

    // NE 不会返回 NULL 行（SQL 三值逻辑）：这是重要行为，不能悄悄改成 IS NOT。
    let ne = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![condition(
                "qty",
                FilterOperator::Ne,
                Some(Value::Int(2)),
            )]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&ne), vec![1, 3, 4], "NE 与 NULL 比较结果为 UNKNOWN");

    // 比较算子：>= 3 与 <= 2。
    let ge = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![condition(
                "qty",
                FilterOperator::Ge,
                Some(Value::Int(3)),
            )]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&ge), vec![3, 4]);
    let le = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![condition(
                "qty",
                FilterOperator::Le,
                Some(Value::Int(2)),
            )]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&le), vec![1, 2]);

    // 多条件 AND 组合：name 含 'a' 且 qty >= 2。
    let and = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![
                condition(
                    "name",
                    FilterOperator::Like,
                    Some(Value::Text("a".to_owned())),
                ),
                condition("qty", FilterOperator::Ge, Some(Value::Int(2))),
            ]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&and), vec![2]);

    // EQ + NULL 必须显式拒绝：`= NULL` 永远不成立，静默空页会误导用户。
    let null_eq = provider
        .browse(
            &items,
            &page(100, None),
            &BrowseOptions {
                filter: filter(vec![condition(
                    "qty",
                    FilterOperator::Eq,
                    Some(Value::Null),
                )]),
                ..BrowseOptions::default()
            },
        )
        .await
        .expect_err("EQ NULL 必须报 config.invalid");
    assert_eq!(null_eq.code(), ErrorCode::CONFIG_INVALID);
    assert_eq!(null_eq.category(), ErrorCategory::InvalidArgument);

    // LIKE 需要文本值：整数在发送前被拒。
    let like_int = provider
        .browse(
            &items,
            &page(100, None),
            &BrowseOptions {
                filter: filter(vec![condition(
                    "name",
                    FilterOperator::Like,
                    Some(Value::Int(5)),
                )]),
                ..BrowseOptions::default()
            },
        )
        .await
        .expect_err("LIKE 非文本值必须报 config.invalid");
    assert_eq!(like_int.code(), ErrorCode::CONFIG_INVALID);

    // IS NULL 忽略比较值（契约允许客户端带值过来）。
    let is_null_with_value = browse(
        provider,
        &items,
        100,
        BrowseOptions {
            filter: filter(vec![condition(
                "qty",
                FilterOperator::IsNull,
                Some(Value::Text("ignored".to_owned())),
            )]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&is_null_with_value), vec![5]);
}

/// 未知列 / 类型不兼容在发送 SQL 前被拒：错误码是 `config.invalid`，不是数据库语法错。
#[tokio::test]
async fn table_data_filter_rejects_unknown_columns_before_sql() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
    )
    .await;
    exec(session.as_ref(), "INSERT INTO t VALUES (1,'a')").await;

    let provider = session.table_data().expect("表数据能力");
    let t = object(session.as_ref(), "t");

    for options in [
        BrowseOptions {
            sort: vec![sort("missing", SortDirection::Asc)],
            ..BrowseOptions::default()
        },
        BrowseOptions {
            filter: filter(vec![condition(
                "missing",
                FilterOperator::Eq,
                Some(Value::Int(1)),
            )]),
            ..BrowseOptions::default()
        },
        BrowseOptions {
            filter: filter(vec![condition(
                "name",
                FilterOperator::Eq,
                Some(Value::Array(Vec::new())),
            )]),
            ..BrowseOptions::default()
        },
    ] {
        let error = provider
            .browse(&t, &page(10, None), &options)
            .await
            .expect_err("未知列 / 不可绑定值必须被拒绝");
        assert_eq!(
            error.code(),
            ErrorCode::CONFIG_INVALID,
            "必须是 config.invalid，而不是 sqlite 语法错：{}",
            error.message()
        );
        assert_eq!(error.category(), ErrorCategory::InvalidArgument);
        assert!(
            !error.message().to_uppercase().contains("SELECT"),
            "错误文案不得回显 SQL：{}",
            error.message()
        );
    }

    // 合法列 + 合法值仍然可用（拒绝不是「筛选整体不可用」）。
    let ok = browse(
        provider,
        &t,
        10,
        BrowseOptions {
            filter: filter(vec![condition(
                "name",
                FilterOperator::Eq,
                Some(Value::Text("a".to_owned())),
            )]),
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(ids(&ok), vec![1]);
}

/// offset 分页与游标分页得到同一批行；越界返回空页；两种方式混用被拒。
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "游标翻页、offset 翻页、越界与两种混用的拒绝必须在同一个数据集上对照"
)]
async fn table_data_offset_pages_match_cursor_pages() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE paged (id INTEGER PRIMARY KEY, v TEXT)",
    )
    .await;
    exec(
        session.as_ref(),
        "WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x < 12) \
         INSERT INTO paged(id, v) SELECT x, 'v' || x FROM seq",
    )
    .await;

    let provider = session.table_data().expect("表数据能力");
    let paged = object(session.as_ref(), "paged");

    // 游标翻页：5 + 5 + 2。
    let mut cursor_rows = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = provider
            .browse(&paged, &page(5, token.clone()), &BrowseOptions::default())
            .await
            .unwrap();
        cursor_rows.extend(ids(&page));
        if let Some(next) = page.next_page_token {
            assert!(page.has_more);
            token = Some(next);
        } else {
            assert!(!page.has_more);
            break;
        }
    }

    // offset 翻页：同样的 5 + 5 + 2。
    let mut offset_rows = Vec::new();
    for offset in [0_u64, 5, 10] {
        let page = browse(
            provider,
            &paged,
            5,
            BrowseOptions {
                offset: Some(offset),
                ..BrowseOptions::default()
            },
        )
        .await;
        assert!(
            page.next_page_token.is_none(),
            "offset 分页不应再签发主键游标"
        );
        offset_rows.extend(ids(&page));
    }

    assert_eq!(offset_rows, cursor_rows, "两种分页必须得到同一批行与顺序");
    assert_eq!(
        offset_rows,
        (1..=12).collect::<Vec<i64>>(),
        "12 行必须完整、有序、无重复"
    );

    // offset 越界：空页 + has_more=false + 如实的总数，不报错。
    let beyond = browse(
        provider,
        &paged,
        5,
        BrowseOptions {
            offset: Some(100),
            include_total: true,
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(beyond.rows, Vec::<Row>::new(), "offset 越界必须返回空页");
    assert!(!beyond.has_more);
    assert!(beyond.next_page_token.is_none());
    assert_eq!(beyond.total_kind, TotalKind::Exact);
    assert_eq!(beyond.total_rows, Some(12));

    // offset + page_token 同时给出：驱动侧也拒绝（rpc-server 已拦一次）。
    let first = provider
        .browse(&paged, &page(5, None), &BrowseOptions::default())
        .await
        .unwrap();
    let token = first.next_page_token.expect("应签发游标");
    let mixed = provider
        .browse(
            &paged,
            &page(5, Some(token.clone())),
            &BrowseOptions {
                offset: Some(5),
                ..BrowseOptions::default()
            },
        )
        .await
        .expect_err("offset 与 page_token 互斥");
    assert_eq!(mixed.code(), ErrorCode::CONFIG_INVALID);

    // 排序 + page_token：游标只表示主键升序位置，必须拒绝而不是错位翻页。
    let sorted_with_cursor = provider
        .browse(
            &paged,
            &page(5, Some(token)),
            &BrowseOptions {
                sort: vec![sort("id", SortDirection::Desc)],
                ..BrowseOptions::default()
            },
        )
        .await
        .expect_err("排序 + 游标必须被拒绝");
    assert_eq!(sorted_with_cursor.code(), ErrorCode::CONFIG_INVALID);
}

/// `total_kind` 三态与 `total_rows` 自洽：未请求 UNKNOWN，请求了就是真实计数。
#[tokio::test]
async fn table_data_total_is_exact_or_unknown() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE counted (id INTEGER PRIMARY KEY, v INTEGER)",
    )
    .await;
    exec(
        session.as_ref(),
        "INSERT INTO counted VALUES (1,10),(2,20),(3,30),(4,40),(5,50),(6,60),(7,70)",
    )
    .await;

    let provider = session.table_data().expect("表数据能力");
    let counted = object(session.as_ref(), "counted");

    // 未请求：UNKNOWN 且 total_rows = None（不得顺手算一个）。
    let unknown = browse(provider, &counted, 3, BrowseOptions::default()).await;
    assert_eq!(unknown.total_kind, TotalKind::Unknown);
    assert_eq!(unknown.total_rows, None);

    // 请求了：EXACT + 真实行数（与页大小、offset 无关）。
    let exact = browse(
        provider,
        &counted,
        3,
        BrowseOptions {
            include_total: true,
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(exact.total_kind, TotalKind::Exact);
    assert_eq!(exact.total_rows, Some(7));
    assert_eq!(exact.rows.len(), 3);

    let second = browse(
        provider,
        &counted,
        3,
        BrowseOptions {
            offset: Some(3),
            include_total: true,
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(second.rows.len(), 3);
    assert_eq!(
        second.total_rows,
        Some(7),
        "总数是筛选后的全集，不随 offset 递减"
    );

    // 带筛选的总数是筛选后的真实计数。
    let filtered = browse(
        provider,
        &counted,
        2,
        BrowseOptions {
            filter: filter(vec![condition(
                "v",
                FilterOperator::Ge,
                Some(Value::Int(40)),
            )]),
            include_total: true,
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(filtered.total_kind, TotalKind::Exact);
    assert_eq!(filtered.total_rows, Some(4));
    assert_eq!(ids(&filtered), vec![4, 5]);

    // 计数是每次请求的真实查询，不是缓存：插入一行后立刻变为 8。
    exec(session.as_ref(), "INSERT INTO counted VALUES (8, 80)").await;
    let after_insert = browse(
        provider,
        &counted,
        2,
        BrowseOptions {
            include_total: true,
            ..BrowseOptions::default()
        },
    )
    .await;
    assert_eq!(after_insert.total_rows, Some(8));
    assert_eq!(after_insert.total_kind, TotalKind::Exact);
}

/// 计数超时（#127）：上限到点后放弃统计（`UNKNOWN` + `total_skipped`），
/// 本页数据照样完整，连接仍可继续使用。
///
/// 成本用视图放大（1200 × 1200 的笛卡尔积），不插入几十万行：要验证的是超时分支
/// 真的会被触发并干净收尾，不是压测计数速度。
#[tokio::test]
async fn count_timeout_skips_the_total_without_touching_the_page() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE count_base (id INTEGER PRIMARY KEY, v INTEGER)",
    )
    .await;
    exec(
        session.as_ref(),
        "WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x < 1200) \
         INSERT INTO count_base(id, v) SELECT x, x % 13 FROM seq",
    )
    .await;
    // 144 万行的 `COUNT(*)`：毫秒级超时一定先到。
    exec(
        session.as_ref(),
        "CREATE VIEW slow_count AS SELECT a.id AS id, a.v AS v FROM count_base a, count_base b",
    )
    .await;

    let provider = session.table_data().expect("表数据能力");
    let slow = ObjectRef {
        kind: ObjectKind::View,
        ..object(session.as_ref(), "slow_count")
    };

    let started = std::time::Instant::now();
    let first_page = provider
        .browse(
            &slow,
            &page(50, None),
            &BrowseOptions {
                include_total: true,
                total_timeout: Some(std::time::Duration::from_millis(1)),
                ..BrowseOptions::default()
            },
        )
        .await
        .expect("计数超时只放弃统计，绝不能报错");
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
        first_page
            .rows
            .iter()
            .all(|row| matches!(row[0].value, Value::Int(_))),
        "本页每一行都必须完整（第一列是整型 id）"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "超时后必须尽快返回，实际 {elapsed:?}"
    );

    // 连接没有被留在「计数仍在跑」的状态：同一会话还能继续浏览与计数。
    let after_timeout = provider
        .browse(&slow, &page(5, None), &BrowseOptions::default())
        .await
        .expect("超时后连接必须仍然可用");
    assert_eq!(after_timeout.rows.len(), 5);

    let counted = object(session.as_ref(), "count_base");
    let exact = provider
        .browse(
            &counted,
            &page(5, None),
            &BrowseOptions {
                include_total: true,
                ..BrowseOptions::default()
            },
        )
        .await
        .expect("超时后计数仍可用");
    assert_eq!(
        exact.total_kind,
        TotalKind::Exact,
        "超时的中断不得残留到下一条语句"
    );
    assert_eq!(exact.total_rows, Some(1_200));
    assert!(!exact.total_skipped, "算出来了就不能标成已跳过");
}

/// 无主键表允许排序/筛选/计数，但仍只读、`ordering = Unstable`、不签发游标。
#[tokio::test]
async fn table_data_without_primary_key_browses_with_options() {
    let (_dir, session) = session().await;
    exec(
        session.as_ref(),
        "CREATE TABLE logs (message TEXT, level INTEGER)",
    )
    .await;
    exec(
        session.as_ref(),
        "INSERT INTO logs VALUES ('m1',1),('m2',3),('m3',2),('other',3)",
    )
    .await;

    let provider = session.table_data().expect("表数据能力");
    let logs = object(session.as_ref(), "logs");

    let page = browse(
        provider,
        &logs,
        2,
        BrowseOptions {
            sort: vec![sort("level", SortDirection::Desc)],
            filter: filter(vec![condition(
                "message",
                FilterOperator::Like,
                Some(Value::Text("m".to_owned())),
            )]),
            include_total: true,
            ..BrowseOptions::default()
        },
    )
    .await;

    assert!(page.read_only, "无主键表必须只读");
    assert_eq!(
        page.read_only_reason,
        Some(sqarvo_driver_api::ReadOnlyReason::NoPrimaryKey)
    );
    assert_eq!(
        page.ordering,
        RowOrder::Unstable,
        "无主键表的排序不构成稳定保证"
    );
    assert!(page.next_page_token.is_none(), "无主键表不签发游标");
    assert!(page.has_more, "4 行符合筛选，每页 2 行应还有下一页");
    assert_eq!(page.total_kind, TotalKind::Exact);
    assert_eq!(page.total_rows, Some(3), "筛选后的计数是 3 而不是 4");
    // level DESC：m2(3) 与 m3(2) 在前，m1(1) 在下一页；'other' 被 LIKE 'm' 排除。
    assert_eq!(
        page.rows[0][0].value,
        Value::Text("m2".to_owned()),
        "排序必须生效（level 最高的在前）"
    );

    let next = browse(
        provider,
        &logs,
        2,
        BrowseOptions {
            offset: Some(2),
            sort: vec![sort("level", SortDirection::Desc)],
            filter: filter(vec![condition(
                "message",
                FilterOperator::Like,
                Some(Value::Text("m".to_owned())),
            )]),
            include_total: true,
            total_timeout: None,
        },
    )
    .await;
    assert_eq!(ids_of_text(&next), vec!["m1"]);
    assert!(!next.has_more);
    assert_eq!(next.total_rows, Some(3));
    assert_eq!(next.ordering, RowOrder::Unstable);
}

fn ids_of_text(page: &TablePage) -> Vec<String> {
    page.rows
        .iter()
        .map(|row| match &row[0].value {
            Value::Text(text) => text.clone(),
            other => panic!("期望 TEXT 列，实际 {other:?}"),
        })
        .collect()
}

/// 能力位与实现一致：描述符与 provider 都声明支持（翻 true 必须有真实路径）。
#[tokio::test]
async fn driver_declares_browse_sort_filter_total() {
    let driver = SqliteDriver::new();
    assert!(
        driver
            .descriptor()
            .capabilities
            .table_browse_sort_filter_total,
        "SQLite 已实现下推，能力位必须为 true"
    );

    let (_dir, session) = session().await;
    let provider = session.table_data().expect("表数据能力");
    assert!(provider.supports_browse_sort_filter_total());
}

/// 跨驱动不变量（`sqarvo_driver_testkit::browse_invariants`，ADR 0022）：
/// `offset` 与 `page_token` 互斥、排序下 `offset` 翻页无重无漏、总数三态自洽、
/// 非法排序/筛选显式失败、无主键表只读且顺序不稳定、能力位与排序行为一致。
///
/// 与上面的用例互补：这里不重复方言细节（LIKE 通配符、NULL 三值逻辑），只断言
/// 「SQLite 与 MariaDB 必须给出同样结论」的契约性质。
#[tokio::test]
async fn table_browse_satisfies_shared_testkit_invariants() {
    let (_dir, session) = session().await;

    let keyed_create = "CREATE TABLE inv_scores (id INTEGER PRIMARY KEY, grp INTEGER NOT NULL, name TEXT NOT NULL)";
    let keyed_insert = "INSERT INTO inv_scores VALUES \
         (1,2,'n1'),(2,3,'n2'),(3,1,'n3'),(4,3,'n4'),(5,1,'n5'),\
         (6,2,'n6'),(7,1,'n7'),(8,3,'n8'),(9,2,'n9')";
    let unkeyed_create =
        "CREATE TABLE inv_logs (id INTEGER, grp INTEGER NOT NULL, name TEXT NOT NULL)";
    let unkeyed_insert = "INSERT INTO inv_logs VALUES \
         (1,2,'n1'),(2,3,'n2'),(3,1,'n3'),(4,3,'n4'),(5,1,'n5'),\
         (6,2,'n6'),(7,1,'n7'),(8,3,'n8'),(9,2,'n9')";

    let fixture = sqarvo_driver_testkit::BrowseFixture {
        keyed: sqarvo_driver_testkit::BrowseTable {
            name: "inv_scores",
            create_sql: keyed_create,
            insert_sql: keyed_insert,
            drop_sql: "DROP TABLE IF EXISTS inv_scores",
            count_sql: "SELECT COUNT(*) FROM inv_scores",
            primary_key: Some("id"),
            sort_column: "grp",
            row_count: 9,
        },
        unkeyed: sqarvo_driver_testkit::BrowseTable {
            name: "inv_logs",
            create_sql: unkeyed_create,
            insert_sql: unkeyed_insert,
            drop_sql: "DROP TABLE IF EXISTS inv_logs",
            count_sql: "SELECT COUNT(*) FROM inv_logs",
            primary_key: None,
            sort_column: "grp",
            row_count: 9,
        },
    };

    sqarvo_driver_testkit::browse_invariants(session.as_ref(), &fixture)
        .await
        .unwrap_or_else(|error| panic!("SQLite 未通过表浏览不变量：{error:?}"));
}
