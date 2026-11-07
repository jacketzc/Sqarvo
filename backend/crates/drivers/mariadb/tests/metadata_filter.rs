//! MariaDB 对象搜索与过滤的集成测试（v0.2.2，#84）。
//!
//! 需要真实数据库：设置 `SQARVO_MARIADB_URL`
//! （形如 `mysql://user:***@127.0.0.1:53306/sqarvo_test`）；未设置时用例打印跳过原因。
//!
//! 覆盖 ADR 0019 冻结的过滤语义，与 SQLite 侧（`sqlite/tests/metadata_filter.rs`）
//! 一一对应，**大小写部分刻意断言两个驱动的真实差异**而不是伪造统一语义：
//!
//! - `information_schema` 的名称列是 `utf8mb3_general_ci`，因此 `LIKE` 与 `=` 都不区分
//!   ASCII 大小写（SQLite 的 `exact` 走 `=` 是区分的）；
//! - `general_ci` 连重音都折叠（`café` ≈ `cafe`），SQLite 不折叠；
//! - `%` / `_` / `\` 都是字面量；过滤在 SQL 里发生（`LIKE CONCAT('%', ?, '%')`）。

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use mysql_async::prelude::Queryable as _;
use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    ConnectionParams, Driver, DriverSession, ExecuteRequest, MetadataProvider, NameMatch,
    ObjectFilter, ObjectKind, ObjectNode, ObjectRef, PageRequest, QualifiedName, TlsMode,
};
use sqarvo_driver_mariadb::MariaDbDriver;

/// 本用例的对象前缀：与其它用例的表互不干扰。
const PREFIX: &str = "f84";

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
        credential_ref: Some("sqarvo:conn/filter84".to_owned()),
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

async fn drop_tables(session: &dyn DriverSession, names: &[&str]) {
    for name in names {
        exec(session, &format!("DROP VIEW IF EXISTS `{name}`")).await;
        exec(session, &format!("DROP TABLE IF EXISTS `{name}`")).await;
    }
}

fn provider(session: &dyn DriverSession) -> &dyn MetadataProvider {
    session.metadata().expect("MariaDB 必须提供元数据能力")
}

fn page(size: u32, token: Option<String>) -> PageRequest {
    PageRequest {
        page_size: size,
        page_token: token,
    }
}

fn contains(name: &str) -> ObjectFilter {
    ObjectFilter {
        name: Some(NameMatch::Contains(name.to_owned())),
        kinds: Vec::new(),
    }
}

fn exact(name: &str) -> ObjectFilter {
    ObjectFilter {
        name: Some(NameMatch::Exact(name.to_owned())),
        kinds: Vec::new(),
    }
}

fn with_kinds(mut filter: ObjectFilter, kinds: &[ObjectKind]) -> ObjectFilter {
    filter.kinds = kinds.to_vec();
    filter
}

fn names(nodes: &[ObjectNode]) -> Vec<&str> {
    nodes
        .iter()
        .map(|node| node.display_name.as_str())
        .collect()
}

/// 与 [`names`] 同序的拥有所有权的版本：用于跨 `.await` 保留比较结果（借用活不过循环）。
fn owned_names(nodes: &[ObjectNode]) -> Vec<String> {
    nodes.iter().map(|node| node.display_name.clone()).collect()
}

async fn search_root(
    session: &dyn DriverSession,
    filter: &ObjectFilter,
    page_request: &PageRequest,
) -> sqarvo_common::Result<sqarvo_driver_api::Page<ObjectNode>> {
    provider(session)
        .list_children_filtered(None, filter, page_request)
        .await
}

/// 名字里带 `%`、`_`、`\` 的表名要反引号引用（反引号内的反斜杠是字面量）。
async fn create_table(session: &dyn DriverSession, name: &str) {
    exec(session, &format!("CREATE TABLE `{name}` (id INT)")).await;
}

#[tokio::test]
async fn contains_exact_and_kinds_work_against_information_schema() {
    let Some(session) = session().await else {
        return;
    };
    let tables = [
        format!("{PREFIX}_a_user_log"),
        format!("{PREFIX}_a_USER_PROFILE"),
        format!("{PREFIX}_a_orders"),
    ];
    let view = format!("{PREFIX}_a_user_view");
    let mut all: Vec<&str> = tables.iter().map(String::as_str).collect();
    all.push(&view);
    drop_tables(&*session, &all).await;
    for name in &tables {
        create_table(&*session, name).await;
    }
    exec(
        &*session,
        &format!("CREATE VIEW `{view}` AS SELECT id FROM `{}`", tables[0]),
    )
    .await;

    let found = search_root(
        &*session,
        &contains(&format!("{PREFIX}_a_user")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(
        names(&found.items),
        [
            format!("{PREFIX}_a_USER_PROFILE"),
            format!("{PREFIX}_a_user_log"),
            format!("{PREFIX}_a_user_view"),
        ],
        "contains 按片段匹配（顺序仍是先种类后名字）"
    );

    let exact_one = search_root(
        &*session,
        &exact(&format!("{PREFIX}_a_user_log")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(names(&exact_one.items), [format!("{PREFIX}_a_user_log")]);

    let exact_prefix = search_root(
        &*session,
        &exact(&format!("{PREFIX}_a_user")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(exact_prefix.items, Vec::new(), "exact 不能退化成前缀匹配");

    let views = search_root(
        &*session,
        &with_kinds(contains(&format!("{PREFIX}_a_user")), &[ObjectKind::View]),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(views.items.len(), 1);
    assert_eq!(views.items[0].kind, ObjectKind::View);

    // 这一层没有 Column：空列表 = 合法「没搜到」，不是错误。
    let columns = search_root(
        &*session,
        &with_kinds(contains(&format!("{PREFIX}_a_user")), &[ObjectKind::Column]),
        &page(10, None),
    )
    .await
    .expect("kinds 里没有本层种类是合法请求");
    assert_eq!(columns.items, Vec::new());

    // 空过滤必须与 `list_children` 完全一致。
    //
    // 注意：本文件的其它用例在并行线程里创建/删除 `f84_*` 表，会改变根节点的**全局**
    // 分页窗口（按「种类, 名字」排序的前 5 条）。所以不能拿两次调用直接比较：先要求
    // 「连续两次未过滤结果一致」（窗口稳定），再与空过滤比较；最多重试 20 次、间隔 50 ms。
    // 这与 #101 对 `metadata_lists_tables_and_views_with_stable_pagination` 的处理同一口径：
    // 只让测试容忍并发窗口抖动，不改驱动的错误码语义。
    let mut compared: Option<Vec<String>> = None;
    for _ in 0..20 {
        let first = provider(&*session)
            .list_children(None, &page(5, None))
            .await
            .expect("根节点");
        let second = provider(&*session)
            .list_children(None, &page(5, None))
            .await
            .expect("根节点");
        if names(&first.items) != names(&second.items) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        }
        let empty_filter = search_root(&*session, &ObjectFilter::none(), &page(5, None))
            .await
            .expect("空过滤");
        if names(&empty_filter.items) == names(&first.items) {
            compared = Some(owned_names(&first.items));
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let compared = compared.expect(
        "根节点窗口在 20 次重试内始终不稳定：并发用例在建/删同一前缀的表，空过滤无法与未过滤比较",
    );
    assert!(
        !compared.is_empty(),
        "稳定的根节点窗口为空，无法证明「空过滤与 list_children 一致」"
    );

    drop_tables(&*session, &all).await;
}

/// `_` / `%` / `\` 必须是字面量。`INFORMATION_SCHEMA` 的名字列不会替驱动转义，
/// 所以这条用例直接检验 `LIKE ... ESCAPE` 之外的那一半工作：把模式里的通配符转掉。
#[tokio::test]
async fn wildcards_and_backslash_are_literals() {
    let Some(session) = session().await else {
        return;
    };
    let names_to_create = [
        format!("{PREFIX}_w_user_log"),
        format!("{PREFIX}_w_userXlog"),
        format!("{PREFIX}_w_userYlog"),
        format!("{PREFIX}_w_pct%100"),
        format!("{PREFIX}_w_pctX100"),
        format!("{PREFIX}_w_back\\slash"),
        format!("{PREFIX}_w_backXslash"),
    ];
    let all: Vec<&str> = names_to_create.iter().map(String::as_str).collect();
    drop_tables(&*session, &all).await;
    for name in &names_to_create {
        create_table(&*session, name).await;
    }

    let underscore = search_root(
        &*session,
        &contains(&format!("{PREFIX}_w_user_log")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(
        names(&underscore.items),
        [format!("{PREFIX}_w_user_log")],
        "`_` 必须按字面量匹配：userXlog / userYlog 不是命中项"
    );

    let percent = search_root(
        &*session,
        &contains(&format!("{PREFIX}_w_pct%100")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(
        names(&percent.items),
        [format!("{PREFIX}_w_pct%100")],
        "`%` 必须按字面量匹配：pctX100 不是命中项"
    );

    let backslash = search_root(
        &*session,
        &contains(&format!("{PREFIX}_w_back\\slash")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(
        names(&backslash.items),
        [format!("{PREFIX}_w_back\\slash")],
        "反斜杠按字面量匹配，且不会把后面的字符变成转义序列"
    );

    let exact_percent = search_root(
        &*session,
        &exact(&format!("{PREFIX}_w_pct%100")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(names(&exact_percent.items), [format!("{PREFIX}_w_pct%100")]);

    drop_tables(&*session, &all).await;
}

/// 大小写与重音：`information_schema` 的名称列是 `*_general_ci`，所以 `LIKE` 与 `=`
/// 都不区分 ASCII 大小写，且折叠重音。这与 SQLite 不同（那边 `exact` 区分大小写、
/// 非 ASCII 完全不折叠）——两个驱动的文档各自写明，驱动不得自己加 `LOWER()` 抹平。
#[tokio::test]
async fn case_and_accent_follow_the_column_collation() {
    let Some(session) = session().await else {
        return;
    };
    let log = format!("{PREFIX}_c_log");
    let cafe = format!("{PREFIX}_c_café");
    let all = [log.as_str(), cafe.as_str()];
    drop_tables(&*session, &all).await;
    create_table(&*session, &log).await;
    create_table(&*session, &cafe).await;

    let upper = search_root(
        &*session,
        &contains(&format!("{PREFIX}_C_LOG")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(
        names(&upper.items),
        [log.as_str()],
        "LIKE 不区分 ASCII 大小写"
    );

    // `exact` 走 `=`：驱动把字符串参数按字节串绑定，比较变成二进制语义，
    // 因此 `=` 区分大小写（与 SQLite 的 `=` 一致，但与 `contains` 不对称）。
    // 显式 `COLLATE utf8mb4_general_ci` 才会变成不区分——驱动不加这个修饰。
    let exact_upper = search_root(
        &*session,
        &exact(&format!("{PREFIX}_C_LOG")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert!(
        exact_upper.items.is_empty(),
        "exact 走 `=`，大小写不同不应命中：{:?}",
        names(&exact_upper.items)
    );
    let exact_same_case = search_root(&*session, &exact(&log), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&exact_same_case.items), [log.as_str()]);

    let accent = search_root(
        &*session,
        &contains(&format!("{PREFIX}_c_cafe")),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(
        names(&accent.items),
        [cafe.as_str()],
        "LIKE 在 utf8mb3_general_ci 下折叠重音：`cafe` 命中 `café`（SQLite 不折叠）"
    );

    drop_tables(&*session, &all).await;
}

#[tokio::test]
async fn table_children_layers_are_filtered_too() {
    let Some(session) = session().await else {
        return;
    };
    let table = format!("{PREFIX}_child_table");
    let index = format!("{PREFIX}_user_idx");
    let trigger = format!("{PREFIX}_user_trg");
    drop_tables(&*session, &[table.as_str()]).await;
    exec(&*session, &format!("DROP TRIGGER IF EXISTS `{trigger}`")).await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE `{table}` (\
               `id` INT PRIMARY KEY, \
               `user_name` VARCHAR(32), \
               `user_email` VARCHAR(32), \
               `other` VARCHAR(32), \
               INDEX `{index}` (`user_name`)\
             )"
        ),
    )
    .await;
    exec(
        &*session,
        &format!("CREATE TRIGGER `{trigger}` BEFORE INSERT ON `{table}` FOR EACH ROW SET @f84 = 1"),
    )
    .await;

    let parent = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, &table),
        kind: ObjectKind::Table,
    };

    let all = provider(&*session)
        .list_children_filtered(Some(parent.clone()), &contains("user_"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&all.items),
        [
            "user_email".to_owned(),
            "user_name".to_owned(),
            index.clone(),
            trigger.clone(),
        ],
        "列 / 索引 / 触发器三种子节点都要过滤（先种类后名字）"
    );

    let columns_only = provider(&*session)
        .list_children_filtered(
            Some(parent.clone()),
            &with_kinds(contains("user_"), &[ObjectKind::Column]),
            &page(10, None),
        )
        .await
        .expect("查询应成功");
    assert_eq!(names(&columns_only.items), ["user_email", "user_name"]);

    let index_only = provider(&*session)
        .list_children_filtered(Some(parent), &exact(&index), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&index_only.items), [index.as_str()]);
    assert_eq!(index_only.items[0].kind, ObjectKind::Index);

    drop_tables(&*session, &[table.as_str()]).await;
}

#[tokio::test]
async fn filtered_results_paginate_without_duplicates_or_gaps() {
    let Some(session) = session().await else {
        return;
    };
    let mut created = Vec::new();
    for index in 0..7 {
        created.push(format!("{PREFIX}_pg_{index:02}"));
    }
    created.push(format!("{PREFIX}_other_00"));
    let all: Vec<&str> = created.iter().map(String::as_str).collect();
    drop_tables(&*session, &all).await;
    for name in &created {
        create_table(&*session, name).await;
    }

    let filter = contains(&format!("{PREFIX}_pg_"));
    let mut collected: Vec<String> = Vec::new();
    let mut token = None;
    let mut pages = 0;
    loop {
        let found = search_root(&*session, &filter, &page(2, token.clone()))
            .await
            .expect("翻页");
        assert!(found.items.len() <= 2);
        for node in &found.items {
            assert!(
                node.display_name.contains("_pg_"),
                "翻页结果里混进了不符合条件的对象：{}",
                node.display_name
            );
        }
        collected.extend(names(&found.items).into_iter().map(str::to_owned));
        pages += 1;
        assert!(pages <= 10, "分页未收敛");
        match found.next_page_token {
            Some(next) => {
                assert!(
                    next.starts_with("v2:"),
                    "过滤分页的 token 必须带过滤标签：{next}"
                );
                token = Some(next);
            }
            None => break,
        }
    }
    assert_eq!(pages, 4, "7 个命中项、每页 2 个应分 4 页");
    let unique: BTreeSet<&String> = collected.iter().collect();
    assert_eq!(unique.len(), collected.len(), "翻页不得重复");
    assert_eq!(collected.len(), 7, "翻页不得遗漏");
    assert_eq!(
        collected,
        (0..7)
            .map(|index| format!("{PREFIX}_pg_{index:02}"))
            .collect::<Vec<_>>()
    );

    drop_tables(&*session, &all).await;
}

/// 过滤必须在切页之前生效：命中项排在全集最后时，「先按页取一段再筛」会拿到空页。
#[tokio::test]
async fn filter_applies_before_the_page_is_cut() {
    let Some(session) = session().await else {
        return;
    };
    let mut created: Vec<String> = Vec::new();
    for index in 0..20 {
        created.push(format!("{PREFIX}_aaa_{index:02}"));
    }
    let target = format!("{PREFIX}_zz_target");
    created.push(target.clone());
    let all: Vec<&str> = created.iter().map(String::as_str).collect();
    drop_tables(&*session, &all).await;
    for name in &created {
        create_table(&*session, name).await;
    }

    let found = search_root(&*session, &contains(&target), &page(5, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&found.items), [target.as_str()], "过滤先于切页");

    drop_tables(&*session, &all).await;
}

#[tokio::test]
async fn changing_the_filter_rejects_the_old_token() {
    let Some(session) = session().await else {
        return;
    };
    let mut created = Vec::new();
    for index in 0..5 {
        created.push(format!("{PREFIX}_tok_{index:02}"));
    }
    let all: Vec<&str> = created.iter().map(String::as_str).collect();
    drop_tables(&*session, &all).await;
    for name in &created {
        create_table(&*session, name).await;
    }

    let filter = contains(&format!("{PREFIX}_tok_"));
    let first = search_root(&*session, &filter, &page(2, None))
        .await
        .expect("首页");
    let token = first.next_page_token.expect("应返回 token");
    assert!(
        token.starts_with("v2:"),
        "过滤分页的 token 必须带过滤标签：{token}"
    );

    // 换关键词：旧 token 必须 config.invalid（不是 snapshot_changed）。
    let error = search_root(
        &*session,
        &contains(&format!("{PREFIX}_tok_0")),
        &page(2, Some(token.clone())),
    )
    .await
    .expect_err("换条件后旧 token 必须被拒");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);

    // 换匹配方式。
    let error = search_root(
        &*session,
        &exact(&format!("{PREFIX}_tok_")),
        &page(2, Some(token.clone())),
    )
    .await
    .expect_err("换匹配方式后旧 token 必须被拒");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 条件不变仍可翻页（不是「一律拒绝」的假实现）。
    let second = search_root(&*session, &filter, &page(2, Some(token)))
        .await
        .expect("条件不变应能继续翻页");
    assert_eq!(
        names(&second.items),
        [format!("{PREFIX}_tok_02"), format!("{PREFIX}_tok_03")]
    );

    drop_tables(&*session, &all).await;
}

/// 无权限不能拿空列表冒充：库不可读时是 `metadata.permission_denied`。
///
/// 测试账号只有测试库的权限，`mysql` 库对它不可见（`SHOW CREATE DATABASE` 返回 1044），
/// 因此展开该目录的**搜索**必须报权限错误，而不是「没搜到」。
#[tokio::test]
async fn permission_denied_is_not_reported_as_an_empty_search() {
    let Some(session) = session().await else {
        return;
    };
    let forbidden = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(Some("mysql".to_owned()), None, ""),
        kind: ObjectKind::Catalog,
    };
    let error = provider(&*session)
        .list_children_filtered(Some(forbidden), &contains("user"), &page(10, None))
        .await
        .expect_err("不可读的库必须报错，不能返回空列表");
    assert_eq!(
        error.code(),
        ErrorCode::from_static("metadata.permission_denied")
    );
    assert_eq!(error.category(), ErrorCategory::PermissionDenied);
}

#[tokio::test]
async fn invalid_filters_are_rejected_but_empty_results_are_not() {
    let Some(session) = session().await else {
        return;
    };

    let error = search_root(&*session, &contains(""), &page(10, None))
        .await
        .expect_err("空匹配串必须被拒");
    assert_eq!(
        error.code(),
        ErrorCode::from_static("metadata.filter_invalid")
    );
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);

    let duplicated = with_kinds(contains(PREFIX), &[ObjectKind::Table, ObjectKind::Table]);
    let error = search_root(&*session, &duplicated, &page(10, None))
        .await
        .expect_err("重复种类必须被拒");
    assert_eq!(
        error.code(),
        ErrorCode::from_static("metadata.filter_invalid")
    );

    let empty = search_root(
        &*session,
        &contains(&format!("{PREFIX}_no_such_object_at_all")),
        &page(10, None),
    )
    .await
    .expect("没搜到不是错误");
    assert_eq!(empty.items, Vec::new());
    assert!(empty.next_page_token.is_none());
}

/// 大规模下的搜索耗时记录（`#[ignore]`，默认不跑）。
///
/// 运行（`SQARVO_BENCH_TABLES` 控制规模，默认 10 万）：
/// ```text
/// SQARVO_MARIADB_URL=... SQARVO_BENCH_TABLES=100000 \
///   cargo test -p sqarvo-driver-mariadb --release --test metadata_filter \
///   search_on_large_schema_bench -- --ignored --nocapture
/// ```
///
/// 准备/清理数据走**直连**（`mysql_async`，只在本用例里用）：10 万张表要用服务端存储过程
/// 建，而驱动按契约只接受单条语句。**被测路径仍然走驱动**。
///
/// 除了耗时，还读 `SHOW SESSION STATUS LIKE 'Rows_sent'` 的前后差值：这是**SQL 层证据**
/// ——过滤若退回「读出整层再在 Rust 里筛」，`Rows_sent` 会涨约等于对象总数；下推成功则只
/// 涨「命中行数 + 统计语句自身」。耗时只打印，不做发布承诺。
#[tokio::test]
#[ignore = "10 万对象的规模测试，需显式 --ignored 运行"]
async fn search_on_large_schema_bench() {
    let url = std::env::var("SQARVO_MARIADB_URL").expect("基准用例必须设置 SQARVO_MARIADB_URL");
    let total: usize = std::env::var("SQARVO_BENCH_TABLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000);

    let mut admin = mysql_async::Conn::new(mysql_async::Opts::from_url(&url).expect("URL"))
        .await
        .expect("基准直连");
    let created = prepare_bench_objects(&mut admin, total).await;
    println!("建表 {total} 张耗时 {created:?}");

    let Some(session) = session().await else {
        return;
    };
    let warmup = search_root(&*session, &contains("bench_needle"), &page(50, None))
        .await
        .expect("预热查询");
    assert_eq!(warmup.items.len(), 3, "只应命中 3 个 bench_needle_* 对象");

    let before = rows_sent(&*session).await;
    let started = std::time::Instant::now();
    let found = search_root(&*session, &contains("bench_needle"), &page(50, None))
        .await
        .expect("过滤查询");
    let filtered = started.elapsed();
    let after = rows_sent(&*session).await;
    assert_eq!(names(&found.items).len(), 3, "只应命中 3 个对象");
    let sent = after.saturating_sub(before);
    // SQL 层证据：下推成功时服务器只送回命中行。把过滤退回「读全层再筛」会让这个值
    // 接近 `total`，这条断言立刻变红。
    assert!(
        sent < 100,
        "过滤没有下推：服务器送回了 {sent} 行（对象总数 {total}）"
    );
    println!("contains 命中 3 个耗时 {filtered:?}；Rows_sent 增量 {sent}（对象总数 {total}）");

    let started = std::time::Instant::now();
    let unfiltered = provider(&*session)
        .list_children(None, &page(50, None))
        .await
        .expect("不过滤查询");
    let plain = started.elapsed();
    assert_eq!(unfiltered.items.len(), 50);
    println!("不过滤首页耗时 {plain:?}（环境：本机 release，见运行命令）");

    drop(session);
    let dropped = cleanup_bench_objects(&mut admin).await;
    println!("清理 bench 前缀对象耗时 {dropped:?}");
}

/// 用服务端存储过程建 `total` 张表 + 3 张命中表，返回耗时。
async fn prepare_bench_objects(admin: &mut mysql_async::Conn, total: usize) -> std::time::Duration {
    admin
        .query_drop("DROP PROCEDURE IF EXISTS f84_make_tables")
        .await
        .expect("清理旧过程");
    admin
        .query_drop(
            "CREATE PROCEDURE f84_make_tables(IN total INT) BEGIN \
               DECLARE i INT DEFAULT 0; \
               WHILE i < total DO \
                 SET @f84_sql = CONCAT('CREATE TABLE `bench_', LPAD(i, 6, '0'), '` (id INT)'); \
                 PREPARE f84_stmt FROM @f84_sql; EXECUTE f84_stmt; DEALLOCATE PREPARE f84_stmt; \
                 SET i = i + 1; \
               END WHILE; \
             END",
        )
        .await
        .expect("建表过程");
    let started = std::time::Instant::now();
    admin
        .query_drop(format!("CALL f84_make_tables({total})"))
        .await
        .expect("批量建表");
    let elapsed = started.elapsed();
    for name in [
        "bench_needle_alpha",
        "bench_needle_beta",
        "bench_needle_gamma",
    ] {
        admin
            .query_drop(format!("CREATE TABLE `{name}` (id INT)"))
            .await
            .expect("建命中的表");
    }
    println!(
        "bench 前缀对象数 {:?}",
        admin
            .query_first::<i64, _>(
                "SELECT count(*) FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = DATABASE() AND LEFT(TABLE_NAME, 6) = 'bench_'",
            )
            .await
            .expect("统计对象数")
    );
    elapsed
}

/// 用服务端游标循环删掉 `bench_` 前缀的表，返回耗时。
async fn cleanup_bench_objects(admin: &mut mysql_async::Conn) -> std::time::Duration {
    admin
        .query_drop("DROP PROCEDURE IF EXISTS f84_drop_bench")
        .await
        .expect("清理删除过程");
    admin
        .query_drop(
            "CREATE PROCEDURE f84_drop_bench() BEGIN \
               DECLARE done INT DEFAULT 0; \
               DECLARE object_name VARCHAR(64); \
               DECLARE names CURSOR FOR SELECT TABLE_NAME FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = DATABASE() AND LEFT(TABLE_NAME, 6) = 'bench_'; \
               DECLARE CONTINUE HANDLER FOR NOT FOUND SET done = 1; \
               OPEN names; \
               read_loop: LOOP \
                 FETCH names INTO object_name; \
                 IF done THEN LEAVE read_loop; END IF; \
                 SET @f84_sql = CONCAT('DROP TABLE `', object_name, '`'); \
                 PREPARE f84_stmt FROM @f84_sql; EXECUTE f84_stmt; DEALLOCATE PREPARE f84_stmt; \
               END LOOP; \
               CLOSE names; \
             END",
        )
        .await
        .expect("删除过程");
    let started = std::time::Instant::now();
    admin
        .query_drop("CALL f84_drop_bench()")
        .await
        .expect("批量删除");
    let elapsed = started.elapsed();
    admin
        .query_drop("DROP PROCEDURE IF EXISTS f84_drop_bench")
        .await
        .expect("清理删除过程");
    admin
        .query_drop("DROP PROCEDURE IF EXISTS f84_make_tables")
        .await
        .expect("清理建表过程");
    elapsed
}

/// 读 `SHOW SESSION STATUS LIKE 'Rows_sent'` 的数值。
async fn rows_sent(session: &dyn DriverSession) -> usize {
    let (value, _) = select_count(session, "SHOW SESSION STATUS LIKE 'Rows_sent'").await;
    value
}

/// 读一个整数字符串结果（用于 `SHOW SESSION STATUS` 与 `count(*)`）。
async fn select_count(session: &dyn DriverSession, sql: &str) -> (usize, String) {
    let response = session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: sql.to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await
        .unwrap_or_else(|error| panic!("执行失败：{sql}\n{error:?}"));
    let rows = response.first_batch.expect("应有首批").rows;
    let row = rows.first().expect("应有一行");
    let text = format!("{:?}", row[0].value);
    let number = row
        .iter()
        .find_map(|cell| match &cell.value {
            sqarvo_driver_api::Value::Text(text) | sqarvo_driver_api::Value::Json(text) => {
                text.parse::<usize>().ok()
            }
            sqarvo_driver_api::Value::UInt(value) => usize::try_from(*value).ok(),
            sqarvo_driver_api::Value::Int(value) => usize::try_from(*value).ok(),
            _ => None,
        })
        .unwrap_or(0);
    (number, text)
}
