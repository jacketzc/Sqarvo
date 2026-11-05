//! SQLite 对象搜索与过滤的集成测试（v0.2.2，#84）。
//!
//! 覆盖 ADR 0019 冻结的过滤语义：
//!
//! 1. `contains` / `exact` 的名称片段匹配，`kinds` 取交集；
//! 2. `%` / `_` / `\` 都是**字面量**（用户搜 `user_log` 不能命中 `userXlog`）；
//! 3. 大小写按 SQLite 真实行为（`LIKE` 对 ASCII 不敏感、`=` 区分大小写、非 ASCII 不折叠）；
//! 4. 过滤下多页翻页无重无漏，且过滤在**切页之前**生效；
//! 5. 换过滤条件后旧 token 被 `config.invalid` 拒绝；
//! 6. 空结果是合法结果，条件本身非法才是 `metadata.filter_invalid`。
//!
//! 所有查询都走真实的 SQLite 文件：这里的断言不是「看起来对」，而是驱动真的把条件
//! 交给了数据库（`LIKE ... ESCAPE`）。

use std::collections::BTreeSet;

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    ConnectionParams, Driver, DriverSession, ExecuteRequest, MetadataProvider, NameMatch,
    ObjectFilter, ObjectKind, ObjectNode, ObjectRef, PageRequest, QualifiedName, TlsMode,
};
use sqarvo_driver_sqlite::SqliteDriver;
use tempfile::TempDir;

async fn session() -> (TempDir, Box<dyn DriverSession>) {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("filter.db");
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

fn provider(session: &dyn DriverSession) -> &dyn MetadataProvider {
    session.metadata().expect("SQLite 必须提供元数据能力")
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

/// 一次过滤在根层（当前库的表 / 视图）。
async fn search_root(
    session: &dyn DriverSession,
    filter: &ObjectFilter,
    page_request: &PageRequest,
) -> sqarvo_common::Result<sqarvo_driver_api::Page<ObjectNode>> {
    provider(session)
        .list_children_filtered(None, filter, page_request)
        .await
}

fn assert_sorted(nodes: &[ObjectNode]) {
    let mut sorted = nodes.to_vec();
    sqarvo_metadata::sort_children(&mut sorted);
    let got = names(nodes);
    let want = names(&sorted);
    assert_eq!(got, want, "过滤结果必须按（种类, 名字）排序");
}

#[tokio::test]
async fn contains_matches_names_and_ignores_ascii_case() {
    let (_dir, session) = session().await;
    for sql in [
        r#"CREATE TABLE "user_log" (id INTEGER)"#,
        r#"CREATE TABLE "USER_PROFILE" (id INTEGER)"#,
        r#"CREATE TABLE "orders" (id INTEGER)"#,
        r#"CREATE VIEW "user_view" AS SELECT id FROM "user_log""#,
    ] {
        exec(&*session, sql).await;
    }

    let found = search_root(&*session, &contains("user"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&found.items),
        ["USER_PROFILE", "user_log", "user_view"],
        "contains 按片段匹配，且 SQLite 的 LIKE 对 ASCII 不敏感"
    );
    assert!(!found.truncated);

    // 大小写方向反过来也一样（ASCII）。
    let upper = search_root(&*session, &contains("USER"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&upper.items),
        ["USER_PROFILE", "user_log", "user_view"]
    );

    // 片段不在开头也能命中；不相关的前缀不会命中。
    let tail = search_root(&*session, &contains("log"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&tail.items), ["user_log"]);

    // 空过滤必须与 `list_children` 完全一致（SPI 约定，token 也仍是 v1）。
    let unfiltered = provider(&*session)
        .list_children(None, &page(10, None))
        .await
        .expect("列出根节点");
    let empty_filter = search_root(&*session, &ObjectFilter::none(), &page(10, None))
        .await
        .expect("空过滤应等价于 list_children");
    assert_eq!(names(&empty_filter.items), names(&unfiltered.items));
}

#[tokio::test]
async fn exact_matches_the_whole_name_only() {
    let (_dir, session) = session().await;
    for sql in [
        r#"CREATE TABLE "user_log" (id INTEGER)"#,
        r#"CREATE TABLE "user_log_archive" (id INTEGER)"#,
        r#"CREATE TABLE "user" (id INTEGER)"#,
    ] {
        exec(&*session, sql).await;
    }

    let found = search_root(&*session, &exact("user_log"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&found.items), ["user_log"], "exact 只能是整名");

    let partial = search_root(&*session, &exact("user"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&partial.items), ["user"], "exact 不能退化成前缀/子串");

    let none = search_root(&*session, &exact("user_l"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(none.items, Vec::new(), "没搜到是合法空列表，不是错误");
    assert!(none.next_page_token.is_none());
}

/// `_` 是 LIKE 的通配符：不转义时 `user_log` 会命中 `userXlog`。这条用例就是「转义
/// 有没有真的做」的看门狗——把 `like_escape` 改成直传原文，它会立刻变红。
#[tokio::test]
async fn underscore_is_a_literal_not_a_wildcard() {
    let (_dir, session) = session().await;
    for sql in [
        r#"CREATE TABLE "user_log" (id INTEGER)"#,
        r#"CREATE TABLE "userXlog" (id INTEGER)"#,
        r#"CREATE TABLE "userYlog" (id INTEGER)"#,
    ] {
        exec(&*session, sql).await;
    }

    let found = search_root(&*session, &contains("user_log"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&found.items),
        ["user_log"],
        "`_` 必须按字面量匹配：userXlog / userYlog 不是命中项"
    );

    // 反过来搜字面上的「下划线位置」也不该被当成通配符。
    let wildcard_like = search_root(&*session, &contains("userXlog"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&wildcard_like.items), ["userXlog"]);

    // exact 里的 `_` 同样是字面量（`=` 不做通配符解释）。
    let exact_hit = search_root(&*session, &exact("user_log"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&exact_hit.items), ["user_log"]);
    let exact_other = search_root(&*session, &exact("userXlog"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&exact_other.items),
        ["userXlog"],
        "exact 只回整名相等的那一个"
    );
}

/// `%` 同 `_`：不转义时 `50%` 会匹配任意以 `50` 开头的名字。
#[tokio::test]
async fn percent_is_a_literal_not_a_wildcard() {
    let (_dir, session) = session().await;
    for sql in [
        r#"CREATE TABLE "pct%100" (id INTEGER)"#,
        r#"CREATE TABLE "pctX100" (id INTEGER)"#,
        r#"CREATE TABLE "pct" (id INTEGER)"#,
    ] {
        exec(&*session, sql).await;
    }

    let found = search_root(&*session, &contains("pct%100"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&found.items),
        ["pct%100"],
        "`%` 必须按字面量匹配：pctX100 不是命中项"
    );

    // 只给 `%` 片段：名字里带 `%` 的只有 pct%100。
    let only_percent = search_root(&*session, &contains("t%1"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&only_percent.items), ["pct%100"]);

    let exact_hit = search_root(&*session, &exact("pct%100"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&exact_hit.items), ["pct%100"]);
}

/// 转义符自身也要转义：否则 `\%` 会被 LIKE 理解成「字面量百分号」，搜索结果悄悄变样。
#[tokio::test]
async fn backslash_is_a_literal_and_an_escape_char_at_once() {
    let (_dir, session) = session().await;
    for sql in [
        r#"CREATE TABLE "back\slash" (id INTEGER)"#,
        r#"CREATE TABLE "backXslash" (id INTEGER)"#,
        r#"CREATE TABLE "back%slash" (id INTEGER)"#,
    ] {
        exec(&*session, sql).await;
    }

    let found = search_root(&*session, &contains(r"back\slash"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&found.items),
        [r"back\slash"],
        "反斜杠按字面量匹配，且不会让后面的字符变成转义序列"
    );

    let trailing = search_root(&*session, &contains("back\\"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&trailing.items),
        [r"back\slash"],
        "只搜反斜杠时命中所有含反斜杠的名字"
    );

    let exact_hit = search_root(&*session, &exact(r"back\slash"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&exact_hit.items), [r"back\slash"]);
}

/// 大小写：SQLite 的 `LIKE` 只对 ASCII 折叠，`=` 则是二进制比较。驱动不加 `lower()`
/// 去「统一」——这是数据库的真实行为，文档（`docs/database/sqlite.md`）写明了差异。
#[tokio::test]
async fn case_sensitivity_follows_sqlite_instead_of_a_fake_uniform_rule() {
    let (_dir, session) = session().await;
    for sql in [
        r#"CREATE TABLE "user_log" (id INTEGER)"#,
        r#"CREATE TABLE "Ärger" (id INTEGER)"#,
    ] {
        exec(&*session, sql).await;
    }

    // LIKE：ASCII 不敏感。
    let ascii_upper = search_root(&*session, &contains("USER_LOG"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&ascii_upper.items), ["user_log"]);

    // LIKE：非 ASCII 不折叠（SQLite 的 LIKE 只对 ASCII 做大小写折叠）。
    let umlaut = search_root(&*session, &contains("ärger"), &page(10, None))
        .await
        .expect("查询应成功");
    assert!(
        umlaut.items.is_empty(),
        "SQLite 的 LIKE 不折叠非 ASCII 大小写；若这里命中说明驱动偷偷换了比较方式"
    );
    let umlaut_exact_case = search_root(&*session, &contains("Ärger"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&umlaut_exact_case.items), ["Ärger"]);

    // `=`（exact）：区分大小写，与 LIKE 不同。文档写明了这个不对称，不在这里假装统一。
    let exact_lower = search_root(&*session, &exact("user_log"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&exact_lower.items), ["user_log"]);
    let exact_upper = search_root(&*session, &exact("USER_LOG"), &page(10, None))
        .await
        .expect("查询应成功");
    assert!(
        exact_upper.items.is_empty(),
        "exact 走 `=`，SQLite 的 `=` 区分大小写"
    );
}

#[tokio::test]
async fn kinds_narrow_the_result_and_never_fake_absence() {
    let (_dir, session) = session().await;
    for sql in [
        r#"CREATE TABLE "user_log" (id INTEGER)"#,
        r#"CREATE TABLE "user_note" (id INTEGER)"#,
        r#"CREATE VIEW "user_view" AS SELECT id FROM "user_log""#,
    ] {
        exec(&*session, sql).await;
    }

    let views = search_root(
        &*session,
        &with_kinds(contains("user"), &[ObjectKind::View]),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(names(&views.items), ["user_view"]);
    assert_eq!(views.items[0].kind, ObjectKind::View);

    let tables = search_root(
        &*session,
        &with_kinds(contains("user"), &[ObjectKind::Table]),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(names(&tables.items), ["user_log", "user_note"]);

    // 这一层没有 Column：空列表（合法的「没搜到」），不是错误、也不是 unsupported。
    let columns = search_root(
        &*session,
        &with_kinds(contains("user"), &[ObjectKind::Column]),
        &page(10, None),
    )
    .await
    .expect("kinds 里没有本层的种类是合法请求");
    assert_eq!(columns.items, Vec::new());

    // 名称与种类取交集：名字命中但种类不符 → 空。
    let mismatch = search_root(
        &*session,
        &with_kinds(contains("view"), &[ObjectKind::Table]),
        &page(10, None),
    )
    .await
    .expect("查询应成功");
    assert_eq!(mismatch.items, Vec::new());
}

#[tokio::test]
async fn table_children_layers_are_filtered_too() {
    let (_dir, session) = session().await;
    exec(
        &*session,
        r#"CREATE TABLE "users" (
             "id" INTEGER PRIMARY KEY,
             "user_name" TEXT,
             "user_email" TEXT,
             "other" TEXT
           )"#,
    )
    .await;
    exec(
        &*session,
        r#"CREATE INDEX "user_idx" ON "users" ("user_name")"#,
    )
    .await;
    exec(
        &*session,
        r#"CREATE TRIGGER "user_trg" AFTER INSERT ON "users" BEGIN SELECT 1; END"#,
    )
    .await;

    let parent = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(Some("main".to_owned()), None, "users"),
        kind: ObjectKind::Table,
    };

    let all = provider(&*session)
        .list_children_filtered(Some(parent.clone()), &contains("user"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&all.items),
        ["user_email", "user_name", "user_idx", "user_trg"],
        "列 / 索引 / 触发器三种子节点都要过滤（排序仍是先种类后名字）"
    );
    assert_sorted(&all.items);

    let columns_only = provider(&*session)
        .list_children_filtered(
            Some(parent.clone()),
            &with_kinds(contains("user"), &[ObjectKind::Column]),
            &page(10, None),
        )
        .await
        .expect("查询应成功");
    assert_eq!(names(&columns_only.items), ["user_email", "user_name"]);

    let index_only = provider(&*session)
        .list_children_filtered(Some(parent.clone()), &exact("user_idx"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&index_only.items), ["user_idx"]);
    assert_eq!(index_only.items[0].kind, ObjectKind::Index);

    // 列名里的 `_` 同样按字面量处理：`user_` 不能匹配成 `userX`。
    let literal = provider(&*session)
        .list_children_filtered(Some(parent), &contains("user_n"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&literal.items), ["user_name"]);
}

#[tokio::test]
async fn catalog_layer_is_filtered_in_sql() {
    let (_dir, session) = session().await;

    let layer = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, ""),
        kind: ObjectKind::Catalog,
    };

    // `main` / `temp` 都在候选里；过滤只留下命中的那个。
    let main_only = provider(&*session)
        .list_children_filtered(Some(layer.clone()), &contains("mai"), &page(10, None))
        .await
        .expect("查询应成功");
    assert_eq!(names(&main_only.items), ["main"]);

    let temp_only = provider(&*session)
        .list_children_filtered(
            Some(layer.clone()),
            &with_kinds(contains("tem"), &[ObjectKind::Catalog]),
            &page(10, None),
        )
        .await
        .expect("查询应成功");
    assert_eq!(names(&temp_only.items), ["temp"]);

    // 种类不含 Catalog：空列表，不是 unsupported。
    let none = provider(&*session)
        .list_children_filtered(
            Some(layer),
            &with_kinds(contains("mai"), &[ObjectKind::Table]),
            &page(10, None),
        )
        .await
        .expect("查询应成功");
    assert_eq!(none.items, Vec::new());
}

#[tokio::test]
async fn filtered_results_paginate_without_duplicates_or_gaps() {
    let (_dir, session) = session().await;
    for index in 0..7 {
        exec(
            &*session,
            &format!(r#"CREATE TABLE "pg_{index:02}" (id INTEGER)"#),
        )
        .await;
    }
    exec(&*session, r#"CREATE TABLE "other_00" (id INTEGER)"#).await;

    let filter = contains("pg_");
    let mut collected = Vec::new();
    let mut token = None;
    let mut pages = 0;
    loop {
        let page = search_root(&*session, &filter, &page(2, token.clone()))
            .await
            .expect("翻页");
        assert!(page.items.len() <= 2);
        for node in &page.items {
            assert!(
                node.display_name.contains("pg_"),
                "翻页结果里混进了不符合条件的对象：{}",
                node.display_name
            );
        }
        collected.extend(names(&page.items).into_iter().map(str::to_owned));
        pages += 1;
        assert!(pages <= 10, "分页未收敛");
        match page.next_page_token {
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
            .map(|index| format!("pg_{index:02}"))
            .collect::<Vec<_>>()
    );
}

/// 过滤必须在**切页之前**生效：命中项排在全集最后时，如果实现是「先按页取一段再筛」，
/// 这里会拿到空页。这条用例连同 `%` / `_` 的用例一起锁住「下推 + 先过滤」。
#[tokio::test]
async fn filter_applies_before_the_page_is_cut() {
    let (_dir, session) = session().await;
    for index in 0..20 {
        exec(
            &*session,
            &format!(r#"CREATE TABLE "aaa_{index:02}" (id INTEGER)"#),
        )
        .await;
    }
    exec(&*session, r#"CREATE TABLE "zz_target" (id INTEGER)"#).await;

    let found = search_root(&*session, &contains("zz_target"), &page(5, None))
        .await
        .expect("查询应成功");
    assert_eq!(
        names(&found.items),
        ["zz_target"],
        "目标排在全集最后也必须出现在第一页：过滤先于切页"
    );
}

#[tokio::test]
async fn changing_the_filter_rejects_the_old_token() {
    let (_dir, session) = session().await;
    for index in 0..5 {
        exec(
            &*session,
            &format!(r#"CREATE TABLE "tok_{index:02}" (id INTEGER)"#),
        )
        .await;
    }

    let first = search_root(&*session, &contains("tok_"), &page(2, None))
        .await
        .expect("首页");
    let token = first.next_page_token.expect("应返回 token");
    assert!(
        token.starts_with("v2:"),
        "过滤分页的 token 必须带过滤标签：{token}"
    );

    // 换关键词（同样的结果集大小、同样的偏移量）：旧 token 必须 config.invalid。
    let error = search_root(&*session, &contains("tok_0"), &page(2, Some(token.clone())))
        .await
        .expect_err("换条件后旧 token 必须被拒");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);

    // 换匹配方式（contains → exact）同样失效。
    let error = search_root(&*session, &exact("tok_"), &page(2, Some(token.clone())))
        .await
        .expect_err("换匹配方式后旧 token 必须被拒");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 加种类条件同样失效。
    let error = search_root(
        &*session,
        &with_kinds(contains("tok_"), &[ObjectKind::Table]),
        &page(2, Some(token.clone())),
    )
    .await
    .expect_err("加 kinds 后旧 token 必须被拒");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

    // 条件不变时仍然可用（不是「一律拒绝」的假实现）。
    let second = search_root(&*session, &contains("tok_"), &page(2, Some(token.clone())))
        .await
        .expect("条件不变应能继续翻页");
    assert_eq!(names(&second.items), ["tok_02", "tok_03"]);

    // 不过滤列表的 v1 token 不能拿来给过滤结果翻页。
    let unfiltered = provider(&*session)
        .list_children(None, &page(2, None))
        .await
        .expect("首页");
    let v1 = unfiltered.next_page_token.expect("应返回 token");
    let error = search_root(&*session, &contains("tok_"), &page(2, Some(v1)))
        .await
        .expect_err("v1 token 不能用于过滤分页");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
}

#[tokio::test]
async fn invalid_filters_are_rejected_but_empty_results_are_not() {
    let (_dir, session) = session().await;
    exec(&*session, r#"CREATE TABLE "anything" (id INTEGER)"#).await;

    // 空匹配串：条件本身非法（用 `metadata.filter_invalid`，不是空列表）。
    let error = search_root(&*session, &contains(""), &page(10, None))
        .await
        .expect_err("空匹配串必须被拒");
    assert_eq!(
        error.code(),
        ErrorCode::from_static("metadata.filter_invalid")
    );
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);

    // 重复的种类：同样非法。
    let duplicated = with_kinds(contains("any"), &[ObjectKind::Table, ObjectKind::Table]);
    let error = search_root(&*session, &duplicated, &page(10, None))
        .await
        .expect_err("重复种类必须被拒");
    assert_eq!(
        error.code(),
        ErrorCode::from_static("metadata.filter_invalid")
    );

    // 没搜到 = 合法空列表（不是错误）。
    let empty = search_root(&*session, &contains("no_such_object"), &page(10, None))
        .await
        .expect("没搜到不是错误");
    assert_eq!(empty.items, Vec::new());
    assert!(empty.next_page_token.is_none());
}

/// 10 万对象规模的耗时记录（`#[ignore]`，默认不跑）。
///
/// 运行：
/// ```text
/// cargo test -p sqarvo-driver-sqlite --release --test metadata_filter \
///   filter_on_hundred_thousand_objects_bench -- --ignored --nocapture
/// ```
///
/// 断言的是**结果正确**（只命中 3 个），耗时只打印、不做发布承诺。若把过滤退回
/// 「全量读进内存再筛」，这里的结果仍然正确，但耗时会从「一次 LIKE 扫」变成
/// 「读回 10 万行 + 内存筛」——数字差距在 Issue 评论里记录。
#[tokio::test]
#[ignore = "10 万对象的规模测试，需显式 --ignored 运行"]
async fn filter_on_hundred_thousand_objects_bench() {
    // 规模可用 `SQARVO_BENCH_OBJECTS` 覆盖（默认 10 万）。
    let total: usize = std::env::var("SQARVO_BENCH_OBJECTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000);
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("bench.db");

    // 直接用 rusqlite 建库：一个事务里建 10 万张表，避免 10 万次会话往返。
    {
        let connection = rusqlite::Connection::open(&path).expect("打开基准库");
        connection
            .execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")
            .expect("关同步");
        connection.execute_batch("BEGIN").expect("开始事务");
        {
            let mut statement = connection
                .prepare("CREATE TABLE \"obj_000000\" (id INTEGER)")
                .expect("准备建表语句");
            for index in 0..total {
                statement
                    .execute([])
                    .unwrap_or_else(|error| panic!("建第 {index} 张表失败：{error}"));
                if index + 1 < total {
                    statement = connection
                        .prepare(&format!(
                            "CREATE TABLE \"obj_{:06}\" (id INTEGER)",
                            index + 1
                        ))
                        .expect("准备建表语句");
                }
            }
        }
        connection.execute_batch("COMMIT").expect("提交");
        for name in ["needle_alpha", "needle_beta", "needle_gamma"] {
            connection
                .execute_batch(&format!("CREATE TABLE \"{name}\" (id INTEGER)"))
                .expect("建命中的表");
        }
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .expect("统计表数");
        assert_eq!(count, i64::try_from(total + 3).unwrap());
    }

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
        .expect("连接基准库");

    // 预热（第一次查询要准备语句）。
    let _ = search_root(&*session, &contains("needle"), &page(50, None))
        .await
        .expect("预热查询");

    let started = std::time::Instant::now();
    let found = search_root(&*session, &contains("needle"), &page(50, None))
        .await
        .expect("过滤查询");
    let filtered = started.elapsed();
    assert_eq!(names(&found.items).len(), 3, "只应命中 3 个 needle_* 对象");

    let started = std::time::Instant::now();
    let unfiltered = provider(&*session)
        .list_children(None, &page(50, None))
        .await
        .expect("不过滤查询");
    let plain = started.elapsed();
    assert_eq!(unfiltered.items.len(), 50);

    println!(
        "对象总数 {total}；过滤 contains(\"needle\") 命中 3 个耗时 {filtered:?}；\
         不过滤首页耗时 {plain:?}（环境：本机 debug/release 见运行命令）"
    );
}
