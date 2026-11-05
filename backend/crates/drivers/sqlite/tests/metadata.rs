//! SQLite 元数据集成测试（#45）。
//!
//! 覆盖 v0.2.1 元数据最小子集：表/视图列表分页、列信息、主键、错误分类与
//! 「DDL 显式未实现」。全部使用临时 SQLite 文件，不依赖外部服务。

use std::collections::BTreeMap;

use sqarvo_common::{ErrorCategory, ErrorCode, QueryId};
use sqarvo_driver_api::{
    ColumnType, ConnectionParams, Driver, DriverSession, ExecuteRequest, MAX_PAGE_SIZE,
    MetadataProvider, ObjectKind, ObjectRef, PageRequest, QualifiedName, TlsMode,
};
use sqarvo_driver_sqlite::SqliteDriver;
use tempfile::TempDir;

async fn session() -> (TempDir, Box<dyn DriverSession>) {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("metadata.db");
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

fn metadata_provider(session: &dyn DriverSession) -> &dyn MetadataProvider {
    session.metadata().expect("SQLite 必须提供元数据能力")
}

fn object_ref(session: &dyn DriverSession, name: &str, kind: ObjectKind) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(None, None, name),
        kind,
    }
}

fn page_request(size: u32, token: Option<String>) -> PageRequest {
    PageRequest {
        page_size: size,
        page_token: token,
    }
}

#[tokio::test]
async fn lists_tables_and_views_with_stable_pagination() {
    let (_dir, session) = session().await;
    for index in 1..=5 {
        exec(
            &*session,
            &format!("CREATE TABLE t{index:02}(id INTEGER PRIMARY KEY)"),
        )
        .await;
    }
    exec(&*session, "CREATE VIEW v01 AS SELECT id FROM t01").await;

    let provider = metadata_provider(&*session);
    let mut names = Vec::new();
    let mut kinds = Vec::new();
    let mut token = None;
    let mut pages = 0;
    loop {
        let page = provider
            .list_children(None, &page_request(2, token))
            .await
            .expect("列出根节点");
        assert!(page.items.len() <= 2);
        names.extend(page.items.iter().map(|node| node.display_name.clone()));
        kinds.extend(page.items.iter().map(|node| node.kind.clone()));
        pages += 1;
        assert!(pages <= 10, "分页未收敛");
        if let Some(next) = page.next_page_token {
            assert!(page.truncated);
            token = Some(next);
        } else {
            assert!(!page.truncated);
            break;
        }
    }

    assert_eq!(pages, 3, "6 个对象按每页 2 个应分 3 页");
    assert_eq!(
        names,
        vec!["t01", "t02", "t03", "t04", "t05", "v01"],
        "列表必须按对象名排序且无重复、无遗漏"
    );
    assert_eq!(kinds[5], ObjectKind::View);
    assert!(kinds[..5].iter().all(|kind| *kind == ObjectKind::Table));

    // 每个节点都可以直接回传给 GetColumns / GetPrimaryKey。
    let first_page = provider
        .list_children(None, &page_request(2, None))
        .await
        .expect("第一页");
    let node = &first_page.items[0];
    let reference = ObjectRef {
        session_id: session.session_id(),
        qualified_name: node.qualified_name.clone(),
        kind: node.kind.clone(),
    };
    assert!(provider.columns(&reference).await.is_ok());
}

#[tokio::test]
async fn list_rejects_unknown_levels_tokens_and_oversized_pages_are_clamped() {
    let (_dir, session) = session().await;
    exec(&*session, "CREATE TABLE t01(id INTEGER)").await;
    let provider = metadata_provider(&*session);

    // 上层层级（Catalog/Schema）本版未实现：显式报「不支持」，不返回空列表。
    let catalog = ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(Some("main".to_owned()), None, ""),
        kind: ObjectKind::Catalog,
    };
    let error = provider
        .list_children(Some(catalog), &page_request(10, None))
        .await
        .expect_err("上层节点必须明确拒绝");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert_eq!(error.category(), ErrorCategory::Unsupported);
    assert!(error.message().contains("v0.2.2"), "{error}");

    // 非法 token：显式报错，绝不回退到第一页。
    let error = provider
        .list_children(None, &page_request(2, Some("abc".to_owned())))
        .await
        .expect_err("非法 token 必须失败");
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);

    // 伪造偏移量同样失败。
    let error = provider
        .list_children(
            None,
            &page_request(2, Some("v1:99:0000000000000000".to_owned())),
        )
        .await
        .expect_err("越界偏移量必须失败");
    assert!(error.message().contains("超出数据范围"), "{error}");

    // 页大小超上限被钳制，而不是报错或照单返回。
    let page = provider
        .list_children(None, &page_request(u32::MAX, None))
        .await
        .expect("超上限页大小应被钳制");
    assert_eq!(page.items.len(), 1);
}

/// 页大小超过 500 时必须被钳制：不能照单返回，也不能报错。
#[tokio::test]
async fn page_size_is_clamped_to_the_spi_limit() {
    let (_dir, session) = session().await;
    let total = usize::try_from(MAX_PAGE_SIZE).expect("上限适合 usize") + 1;
    for index in 0..total {
        exec(
            &*session,
            &format!("CREATE TABLE bulk_{index:03}(v INTEGER)"),
        )
        .await;
    }

    let page = metadata_provider(&*session)
        .list_children(None, &page_request(u32::MAX, None))
        .await
        .expect("超上限页大小应被钳制而不是失败");
    assert_eq!(
        page.items.len(),
        usize::try_from(MAX_PAGE_SIZE).unwrap(),
        "必须恰好返回上限条数"
    );
    assert!(page.truncated);
    assert!(page.next_page_token.is_some());

    let next = metadata_provider(&*session)
        .list_children(None, &page_request(u32::MAX, page.next_page_token))
        .await
        .expect("钳制后的 token 必须可继续翻页");
    assert_eq!(
        next.items.len(),
        total - usize::try_from(MAX_PAGE_SIZE).unwrap()
    );
    assert!(!next.truncated);
}

#[tokio::test]
async fn columns_preserve_declared_type_and_nullability() {
    let (_dir, session) = session().await;
    exec(
        &*session,
        "CREATE TABLE items(id INTEGER PRIMARY KEY, name VARCHAR(64) NOT NULL, \
         price DECIMAL(10,2), note TEXT, blob_col BLOB)",
    )
    .await;
    let provider = metadata_provider(&*session);
    let columns = provider
        .columns(&object_ref(&*session, "items", ObjectKind::Table))
        .await
        .expect("读取列信息");

    assert_eq!(columns.len(), 5);
    assert_eq!(columns[0].name, "id");
    assert_eq!(columns[0].ordinal, 0);
    assert_eq!(
        columns[0].original_type_name.as_deref(),
        Some("INTEGER"),
        "原始类型名必须保留"
    );

    let name = &columns[1];
    assert_eq!(name.name, "name");
    assert_eq!(name.column_type, ColumnType::Text { length: Some(64) });
    assert!(!name.nullable, "NOT NULL 必须如实返回");

    let price = &columns[2];
    assert_eq!(
        price.original_type_name.as_deref(),
        Some("DECIMAL(10,2)"),
        "SQLite 的 NUMERIC/DECIMAL 不能伪造成 Decimal"
    );
    assert!(matches!(price.column_type, ColumnType::Unsupported { .. }));
    assert!(price.nullable);

    assert_eq!(columns[4].column_type, ColumnType::Binary { length: None });

    // 大小写不敏感的对象名解析（SQLite 的标识符语义）。
    let upper = provider
        .columns(&object_ref(&*session, "ITEMS", ObjectKind::Table))
        .await
        .expect("SQLite 对象名大小写不敏感");
    assert_eq!(upper.len(), 5);
}

#[tokio::test]
async fn primary_key_handles_single_composite_and_missing_keys() {
    let (_dir, session) = session().await;
    exec(
        &*session,
        "CREATE TABLE single(id INTEGER PRIMARY KEY, v TEXT)",
    )
    .await;
    exec(
        &*session,
        "CREATE TABLE composite(tenant_id TEXT NOT NULL, order_id INTEGER NOT NULL, \
         PRIMARY KEY (order_id, tenant_id))",
    )
    .await;
    exec(&*session, "CREATE TABLE no_key(a TEXT, b TEXT)").await;
    exec(&*session, "CREATE VIEW v AS SELECT 1 AS one").await;
    let provider = metadata_provider(&*session);

    let single = provider
        .primary_key(&object_ref(&*session, "single", ObjectKind::Table))
        .await
        .expect("单列主键");
    assert_eq!(single.len(), 1);
    assert_eq!(single.columns[0].name, "id");
    assert_eq!(single.columns[0].position, 1);

    let composite = provider
        .primary_key(&object_ref(&*session, "composite", ObjectKind::Table))
        .await
        .expect("复合主键");
    assert_eq!(
        composite.column_names(),
        ["order_id", "tenant_id"],
        "position 必须按数据库真实的主键顺序（不是表内列顺序）"
    );
    assert_eq!(composite.columns[0].position, 1);
    assert_eq!(composite.columns[1].position, 2);

    // 无主键表：空列表（浏览只读、编辑拒绝由 #46/#47 落地）。
    let none = provider
        .primary_key(&object_ref(&*session, "no_key", ObjectKind::Table))
        .await
        .expect("无主键表必须成功返回空列表");
    assert!(none.is_empty());

    // 视图没有主键。
    let view = provider
        .primary_key(&object_ref(&*session, "v", ObjectKind::View))
        .await
        .expect("视图主键查询");
    assert!(view.is_empty());
}

#[tokio::test]
async fn missing_objects_and_closed_sessions_are_classified() {
    let (_dir, session) = session().await;
    let provider = metadata_provider(&*session);

    let missing = provider
        .columns(&object_ref(&*session, "ghost", ObjectKind::Table))
        .await
        .expect_err("不存在的表必须报错");
    assert_eq!(
        missing.code(),
        ErrorCode::from_static("sqlite.object_not_found")
    );
    assert_eq!(missing.category(), ErrorCategory::NotFound);

    let missing_pk = provider
        .primary_key(&object_ref(&*session, "ghost", ObjectKind::Table))
        .await
        .expect_err("不存在的主键查询必须报错");
    assert_eq!(missing_pk.category(), ErrorCategory::NotFound);

    // 不支持的层级必须明确拒绝，而不是返回空列。
    let wrong_kind = provider
        .columns(&object_ref(&*session, "ghost", ObjectKind::Sequence))
        .await
        .expect_err("不支持的层级必须明确拒绝");
    assert_eq!(wrong_kind.code(), ErrorCode::CAPABILITY_UNSUPPORTED);

    session.close().await.expect("关闭会话");
    let closed = provider
        .columns(&object_ref(&*session, "ghost", ObjectKind::Table))
        .await
        .expect_err("关闭后必须失败");
    assert_eq!(closed.category(), ErrorCategory::FailedPrecondition);
}

#[tokio::test]
async fn ddl_is_explicitly_unimplemented_until_v0_2_2() {
    let (_dir, session) = session().await;
    exec(&*session, "CREATE TABLE t(id INTEGER)").await;
    let error = metadata_provider(&*session)
        .ddl(&object_ref(&*session, "t", ObjectKind::Table))
        .await
        .expect_err("DDL 本版必须报未实现");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert_eq!(error.category(), ErrorCategory::Unsupported);
    assert!(error.message().contains("v0.2.2"), "{error}");
}

#[tokio::test]
async fn metadata_waits_for_open_cursor_to_be_closed() {
    let (_dir, session) = session().await;
    exec(&*session, "CREATE TABLE t(v INTEGER)").await;
    exec(&*session, "INSERT INTO t(v) VALUES (1),(2),(3)").await;

    let query_id = QueryId::new();
    let start = session
        .execute(ExecuteRequest {
            query_id,
            sql: "SELECT v FROM t".to_owned(),
            max_rows: Some(1),
            timeout: None,
        })
        .await
        .expect("查询");
    assert!(start.first_batch.expect("首批").has_more);

    let busy = metadata_provider(&*session)
        .list_children(None, &page_request(10, None))
        .await
        .expect_err("游标未读完时元数据必须被拒绝");
    assert_eq!(busy.code(), ErrorCode::from_static("sqlite.query_active"));
    assert_eq!(busy.category(), ErrorCategory::FailedPrecondition);

    session.close_query(&query_id).await.expect("关闭游标");
    let ok = metadata_provider(&*session)
        .list_children(None, &page_request(10, None))
        .await
        .expect("游标释放后应可读取元数据");
    assert_eq!(ok.items.len(), 1);
}
