//! SQLite 层级展开与跨库浏览集成测试（v0.2.2，#82）。
//!
//! 覆盖：
//!
//! 1. 目录层：`main` / `temp` / `ATTACH` 的库都列出，且各自只返回自己的对象；
//! 2. 表 → 列 / 索引 / 触发器；视图 → 列；隐式索引（`sqlite_autoindex_*`）不出现；
//! 3. 每一层分页无重复、无遗漏（跨多页逐页收全后与全集逐项比对）；
//! 4. 跨库引用可原样回传：`ListNodes` 节点 → `GetColumns` / `GetPrimaryKey`；
//! 5. 错误分类：库不存在 `metadata.object_not_found`、不存在的层级
//!    `capability.unsupported`、种类不适用 `metadata.object_kind_unsupported`；
//! 6. 库名含反引号 / 空格 / 中文时，标识符引用（`"<库>".sqlite_master`）仍然正确。

use std::collections::BTreeMap;

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    ConnectionParams, Driver, DriverSession, ExecuteRequest, MetadataProvider, ObjectKind,
    ObjectNode, ObjectRef, PageRequest, QualifiedName, TlsMode,
};
use sqarvo_driver_sqlite::SqliteDriver;
use tempfile::TempDir;

/// 每个用例一个临时库文件，互不干扰。
async fn session() -> (TempDir, Box<dyn DriverSession>) {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("hierarchy.db");
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

fn provider(session: &dyn DriverSession) -> &dyn MetadataProvider {
    session.metadata().expect("SQLite 必须提供元数据能力")
}

fn page(size: u32, token: Option<String>) -> PageRequest {
    PageRequest {
        page_size: size,
        page_token: token,
    }
}

/// 目录层引用：只给种类、不给名字 = 「列出全部 catalog」。
fn catalog_layer(session: &dyn DriverSession) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, ""),
        kind: ObjectKind::Catalog,
    }
}

fn catalog(session: &dyn DriverSession, name: &str) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(Some(name.to_owned()), None, ""),
        kind: ObjectKind::Catalog,
    }
}

fn object(
    session: &dyn DriverSession,
    database: Option<&str>,
    name: &str,
    kind: ObjectKind,
) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(database.map(str::to_owned), None, name),
        kind,
    }
}

/// 逐页收全，返回节点与页数。每页都检查 `truncated` 与 token 的一致性。
async fn walk(
    provider: &dyn MetadataProvider,
    parent: Option<ObjectRef>,
    page_size: u32,
) -> (Vec<ObjectNode>, usize) {
    let mut collected = Vec::new();
    let mut token = None;
    let mut pages = 0;
    loop {
        let page = provider
            .list_children(parent.clone(), &page(page_size, token.clone()))
            .await
            .unwrap_or_else(|error| panic!("列出子节点失败：{error:?}"));
        assert!(page.items.len() <= usize::try_from(page_size).unwrap());
        if page.next_page_token.is_some() {
            assert!(page.truncated, "还有下一页时 truncated 必须为 true");
            assert_eq!(
                page.items.len(),
                usize::try_from(page_size).unwrap(),
                "非末页必须是满页"
            );
        } else {
            assert!(!page.truncated);
        }
        collected.extend(page.items);
        pages += 1;
        assert!(pages <= 100, "分页未收敛");
        match page.next_page_token {
            Some(next) => token = Some(next),
            None => return (collected, pages),
        }
    }
}

fn names(nodes: &[ObjectNode]) -> Vec<(&str, &str)> {
    nodes
        .iter()
        .map(|node| (node.kind.as_str(), node.display_name.as_str()))
        .collect()
}

// ---------------------------------------------------------------------------
// 1. 目录层
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalog_layer_lists_main_temp_and_attached_databases() {
    let (dir, session) = session().await;
    exec(&*session, "CREATE TABLE t_main(id INTEGER PRIMARY KEY)").await;
    exec(&*session, "CREATE TEMP TABLE t_temp(id INTEGER)").await;
    let attached = dir.path().join("attached.db");
    exec(
        &*session,
        &format!("ATTACH DATABASE '{}' AS aux", attached.display()),
    )
    .await;
    exec(&*session, "CREATE TABLE aux.t_aux(id INTEGER)").await;

    let (catalogs, pages) = walk(provider(&*session), Some(catalog_layer(&*session)), 2).await;
    assert!(pages >= 2, "3 个库按每页 2 个应分 2 页，实际 {pages}");
    let listed: Vec<String> = catalogs
        .iter()
        .map(|node| node.display_name.clone())
        .collect();
    assert_eq!(listed, ["aux", "main", "temp"], "目录必须按名字排序");
    for node in &catalogs {
        assert_eq!(node.kind, ObjectKind::Catalog);
        assert!(node.has_children, "目录都可能还有下一层");
        assert_eq!(
            node.qualified_name.catalog.as_deref(),
            Some(node.display_name.as_str()),
            "目录节点的 catalog 必须是库名，客户端才能原样回传"
        );
    }

    // 每个库只返回自己的对象：这是「不隐式切库」的最小证据。
    for (database, expected) in [("main", "t_main"), ("temp", "t_temp"), ("aux", "t_aux")] {
        let (objects, _) = walk(provider(&*session), Some(catalog(&*session, database)), 10).await;
        let object_names: Vec<&str> = objects
            .iter()
            .map(|node| node.display_name.as_str())
            .collect();
        assert_eq!(
            object_names,
            [expected],
            "库 {database} 只应列出自己的表，实际 {object_names:?}"
        );
        assert_eq!(
            objects[0].qualified_name.catalog.as_deref(),
            Some(database),
            "子节点的 catalog 由服务端填满"
        );
    }
}

/// 根节点仍是「当前库（main）的表/视图」：v0.2.1 行为不变，目录层单独寻址。
#[tokio::test]
async fn root_still_lists_only_the_current_database() {
    let (_dir, session) = session().await;
    exec(&*session, "CREATE TABLE t_main(id INTEGER)").await;
    exec(&*session, "CREATE TEMP TABLE t_temp(id INTEGER)").await;

    let (objects, _) = walk(provider(&*session), None, 10).await;
    let listed: Vec<&str> = objects
        .iter()
        .map(|node| node.display_name.as_str())
        .collect();
    assert_eq!(listed, ["t_main"], "根节点不含 temp/attached 的对象");
    assert_eq!(
        objects[0].qualified_name.catalog.as_deref(),
        Some("main"),
        "根节点列出的对象也必须带 catalog"
    );
}

// ---------------------------------------------------------------------------
// 2. 表 / 视图的子节点
// ---------------------------------------------------------------------------

#[tokio::test]
async fn table_children_are_columns_explicit_indexes_and_triggers() {
    let (_dir, session) = session().await;
    exec(
        &*session,
        "CREATE TABLE orders(id INTEGER PRIMARY KEY, \
         code TEXT UNIQUE, amount DECIMAL(10,2), note TEXT)",
    )
    .await;
    exec(&*session, "CREATE INDEX orders_amount ON orders(amount)").await;
    exec(
        &*session,
        "CREATE TRIGGER orders_touch AFTER UPDATE ON orders \
         BEGIN UPDATE orders SET note = note; END",
    )
    .await;
    exec(
        &*session,
        "CREATE VIEW v_orders AS SELECT id, amount FROM orders",
    )
    .await;

    let (children, _) = walk(
        provider(&*session),
        Some(object(&*session, Some("main"), "orders", ObjectKind::Table)),
        10,
    )
    .await;
    assert_eq!(
        names(&children),
        [
            ("column", "amount"),
            ("column", "code"),
            ("column", "id"),
            ("column", "note"),
            ("index", "orders_amount"),
            ("trigger", "orders_touch"),
        ],
        "每层按（种类, 名字）排序；隐式索引（code 的 UNIQUE 自动索引）不出现"
    );
    assert!(
        children.iter().all(|node| !node.has_children),
        "列/索引/触发器都是叶子"
    );
    for node in &children {
        assert_eq!(node.qualified_name.catalog.as_deref(), Some("main"));
    }

    // 视图只有列，没有索引/触发器。
    let (view_children, _) = walk(
        provider(&*session),
        Some(object(
            &*session,
            Some("main"),
            "v_orders",
            ObjectKind::View,
        )),
        10,
    )
    .await;
    assert_eq!(
        names(&view_children),
        [("column", "amount"), ("column", "id")],
        "视图子节点只有列"
    );
}

// ---------------------------------------------------------------------------
// 3. 分页：无重复、无遗漏
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_layer_paginates_without_duplicates_or_gaps() {
    let (_dir, session) = session().await;
    // 目录层必然跨页：main / temp / aux 三个库 + 每页 1 个。
    let dir = tempfile::tempdir().expect("临时目录");
    exec(&*session, "CREATE TEMP TABLE t_temp(id INTEGER)").await;
    exec(
        &*session,
        &format!(
            "ATTACH DATABASE '{}' AS aux",
            dir.path().join("aux.db").display()
        ),
    )
    .await;

    // 表/视图层：21 个对象、每页 4 个 → 6 页。
    let mut expected_objects = Vec::new();
    for index in 0..18 {
        exec(
            &*session,
            &format!("CREATE TABLE page_{index:02}(id INTEGER)"),
        )
        .await;
        expected_objects.push(format!("page_{index:02}"));
    }
    for index in 0..3 {
        exec(
            &*session,
            &format!("CREATE VIEW page_view_{index:02} AS SELECT 1 AS one"),
        )
        .await;
        expected_objects.push(format!("page_view_{index:02}"));
    }
    expected_objects.sort();

    let (objects, object_pages) =
        walk(provider(&*session), Some(catalog(&*session, "main")), 4).await;
    assert_eq!(object_pages, 6, "21 个对象按每页 4 个应分 6 页");
    let listed: Vec<String> = objects
        .iter()
        .map(|node| node.display_name.clone())
        .collect();
    assert_eq!(listed, expected_objects, "翻页必须无重复、无遗漏且有序");

    // 叶子层：列 + 索引 + 触发器混在一层，且故意让列与索引同名。
    exec(
        &*session,
        "CREATE TABLE mixed(same INTEGER, other TEXT, PRIMARY KEY(same))",
    )
    .await;
    exec(&*session, "CREATE INDEX same ON mixed(other)").await;
    exec(
        &*session,
        "CREATE TRIGGER mixed_t AFTER INSERT ON mixed BEGIN SELECT 1; END",
    )
    .await;

    let (children, child_pages) = walk(
        provider(&*session),
        Some(object(&*session, Some("main"), "mixed", ObjectKind::Table)),
        2,
    )
    .await;
    assert_eq!(
        names(&children),
        [
            ("column", "other"),
            ("column", "same"),
            ("index", "same"),
            ("trigger", "mixed_t"),
        ],
        "列与索引同名也不能被合并或跳过"
    );
    assert_eq!(child_pages, 2, "4 个子节点按每页 2 个应分 2 页");

    // 越界 / 篡改 token：显式失败，绝不回退第一页。
    for token in [
        "abc",
        "v1:1",
        "v2:1:0000000000000000",
        "v1:99:0000000000000000",
    ] {
        let error = provider(&*session)
            .list_children(
                Some(catalog(&*session, "main")),
                &page(2, Some(token.to_owned())),
            )
            .await
            .expect_err("非法 token 必须失败");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID, "token={token}");
        assert_eq!(error.category(), ErrorCategory::InvalidArgument);
    }
}

/// 拿 `main` 的 token 去翻 `temp` 的页：必须被拒绝，而不是按偏移量切错页。
#[tokio::test]
async fn page_tokens_do_not_cross_databases() {
    let (_dir, session) = session().await;
    for name in ["a", "b", "c"] {
        exec(&*session, &format!("CREATE TABLE {name}(id INTEGER)")).await;
        exec(
            &*session,
            &format!("CREATE TEMP TABLE {name}_t(id INTEGER)"),
        )
        .await;
    }
    let first = provider(&*session)
        .list_children(Some(catalog(&*session, "main")), &page(1, None))
        .await
        .expect("第一页");
    let token = first.next_page_token.expect("应返回 token");

    let error = provider(&*session)
        .list_children(Some(catalog(&*session, "temp")), &page(1, Some(token)))
        .await
        .expect_err("跨库复用 token 必须失败");
    assert_eq!(
        error.code(),
        ErrorCode::from_static("metadata.snapshot_changed"),
        "作用域不同意味着对象集合不同，旧 token 必须失效"
    );
}

// ---------------------------------------------------------------------------
// 4. 跨库引用可原样回传
// ---------------------------------------------------------------------------

#[tokio::test]
async fn nodes_from_another_database_round_trip_to_columns_and_primary_key() {
    let (_dir, session) = session().await;
    exec(
        &*session,
        "CREATE TEMP TABLE temp_orders(id INTEGER PRIMARY KEY, note TEXT NOT NULL)",
    )
    .await;
    exec(
        &*session,
        "CREATE TABLE main_orders(id INTEGER PRIMARY KEY)",
    )
    .await;

    // 目录层 → temp 目录 → 表节点，节点引用直接回传（前端就是这么做的）。
    let (catalogs, _) = walk(provider(&*session), Some(catalog_layer(&*session)), 10).await;
    let temp = catalogs
        .iter()
        .find(|node| node.display_name == "temp")
        .expect("temp 必须出现在目录层");
    let (objects, _) = walk(
        provider(&*session),
        Some(ObjectRef {
            session_id: session.session_id(),
            qualified_name: temp.qualified_name.clone(),
            kind: temp.kind.clone(),
        }),
        10,
    )
    .await;
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].display_name, "temp_orders");

    let reference = ObjectRef {
        session_id: session.session_id(),
        qualified_name: objects[0].qualified_name.clone(),
        kind: objects[0].kind.clone(),
    };
    let columns = provider(&*session)
        .columns(&reference)
        .await
        .expect("跨库读列");
    assert_eq!(
        columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "note"]
    );
    assert!(!columns[1].nullable, "temp 表的 NOT NULL 必须透传");

    let primary_key = provider(&*session)
        .primary_key(&reference)
        .await
        .expect("跨库读主键");
    assert_eq!(primary_key.column_names(), ["id"]);

    // 同名表在两个库里互不串：main 的同名列只有 id。
    let main_columns = provider(&*session)
        .columns(&object(
            &*session,
            Some("main"),
            "main_orders",
            ObjectKind::Table,
        ))
        .await
        .expect("main 读列");
    assert_eq!(main_columns.len(), 1);
    assert_eq!(main_columns[0].name, "id");
}

// ---------------------------------------------------------------------------
// 5. 错误分类
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hierarchy_errors_are_classified() {
    let (_dir, session) = session().await;
    exec(&*session, "CREATE TABLE t(id INTEGER)").await;
    let provider = provider(&*session);

    // 库不存在 → metadata.object_not_found（NOT_FOUND），消息里列出可用库。
    let error = provider
        .list_children(Some(catalog(&*session, "ghost")), &page(10, None))
        .await
        .expect_err("不存在的库必须报错");
    assert_eq!(error.code(), ErrorCode::METADATA_OBJECT_NOT_FOUND);
    assert_eq!(error.category(), ErrorCategory::NotFound);
    assert!(error.message().contains("ghost"), "{error}");

    // 表不存在 → metadata.object_not_found。
    let error = provider
        .list_children(
            Some(object(&*session, Some("main"), "ghost", ObjectKind::Table)),
            &page(10, None),
        )
        .await
        .expect_err("不存在的表必须报错");
    assert_eq!(error.code(), ErrorCode::METADATA_OBJECT_NOT_FOUND);
    assert_eq!(error.category(), ErrorCategory::NotFound);

    // 不存在的层级：schema / sequence / function → capability.unsupported（不返回空列表）。
    for (kind, qualified_name) in [
        (
            ObjectKind::Schema,
            QualifiedName::new(None, Some("public".to_owned()), ""),
        ),
        (
            ObjectKind::Sequence,
            QualifiedName::new(None, None, "seq_1"),
        ),
        (ObjectKind::Function, QualifiedName::new(None, None, "fn_1")),
    ] {
        let error = provider
            .list_children(
                Some(ObjectRef {
                    session_id: session.session_id(),
                    qualified_name,
                    kind: kind.clone(),
                }),
                &page(10, None),
            )
            .await
            .expect_err("不存在的层级必须明确报错");
        assert_eq!(
            error.code(),
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "种类 {kind:?}"
        );
        assert_eq!(error.category(), ErrorCategory::Unsupported);
    }

    // 对象引用里带 schema：同样是「这一层不存在」。
    let error = provider
        .columns(&ObjectRef {
            session_id: session.session_id(),
            qualified_name: QualifiedName::new(
                Some("main".to_owned()),
                Some("public".to_owned()),
                "t",
            ),
            kind: ObjectKind::Table,
        })
        .await
        .expect_err("带 schema 的引用必须报错");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);

    // 种类不适用（叶子节点不能再展开）→ metadata.object_kind_unsupported（INVALID_ARGUMENT）。
    let error = provider
        .list_children(
            Some(ObjectRef {
                session_id: session.session_id(),
                qualified_name: QualifiedName::new(Some("main".to_owned()), None, "t"),
                kind: ObjectKind::Column,
            }),
            &page(10, None),
        )
        .await
        .expect_err("叶子节点不能再展开");
    assert_eq!(error.code(), ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED);
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);

    // 把视图当表展开：种类与实际对象不符 → object_kind_unsupported，不是「不存在」。
    exec(&*session, "CREATE VIEW v AS SELECT 1 AS one").await;
    let error = provider
        .list_children(
            Some(object(&*session, Some("main"), "v", ObjectKind::Table)),
            &page(10, None),
        )
        .await
        .expect_err("种类与实际对象不符必须报错");
    assert_eq!(error.code(), ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED);
    assert!(error.message().contains("view"), "{error}");
}

// ---------------------------------------------------------------------------
// 6. 特殊字符的库名：标识符必须被正确引用
// ---------------------------------------------------------------------------

/// 库名含反引号、空格、中文时，`"<库>".sqlite_master` 仍然能查；同名对象互不串。
///
/// 这条用例是「库名进 SQL 的标识符位置必须转义」的端到端证据：先把库名当标识符
/// 拼进 `ATTACH`，再靠驱动自己的引用逻辑 `"<库>".sqlite_master` 读回来。
#[tokio::test]
async fn catalog_names_with_quotes_spaces_and_cjk_are_quoted() {
    let (dir, session) = session().await;
    let weird = "订单 库`\"x";
    let attached = dir.path().join("weird.db");
    exec(
        &*session,
        &format!(
            "ATTACH DATABASE '{}' AS {}",
            attached.display(),
            quote_sqlite_identifier(weird)
        ),
    )
    .await;
    exec(
        &*session,
        &format!(
            "CREATE TABLE {}.{} (id INTEGER PRIMARY KEY, `备注` TEXT)",
            quote_sqlite_identifier(weird),
            quote_sqlite_identifier("明细 表`\"y")
        ),
    )
    .await;

    let (catalogs, _) = walk(provider(&*session), Some(catalog_layer(&*session)), 10).await;
    assert!(
        catalogs.iter().any(|node| node.display_name == weird),
        "特殊字符的库名必须原样列出：{:?}",
        catalogs
            .iter()
            .map(|node| &node.display_name)
            .collect::<Vec<_>>()
    );

    let (objects, _) = walk(provider(&*session), Some(catalog(&*session, weird)), 10).await;
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].display_name, "明细 表`\"y");
    assert_eq!(objects[0].qualified_name.catalog.as_deref(), Some(weird));

    let columns = provider(&*session)
        .columns(&ObjectRef {
            session_id: session.session_id(),
            qualified_name: objects[0].qualified_name.clone(),
            kind: objects[0].kind.clone(),
        })
        .await
        .expect("特殊字符库里的表也必须能读列");
    assert_eq!(
        columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "备注"]
    );
}

/// 测试侧的标识符引用（SQL 标准：双引号包裹、内部双引号成对）。
///
/// 与驱动内部的实现分开写：驱动自己的引用逻辑若写错，用例仍然能建出对象，
/// 只是读不回来——这样失败点才落在驱动上，而不是「测试和驱动一起错」。
fn quote_sqlite_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
