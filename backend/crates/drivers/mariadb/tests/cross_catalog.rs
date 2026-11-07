//! MariaDB 表数据按 `ObjectRef.catalog` 取库（ADR 0024 / #140）。
//!
//! 需要真实数据库：设置 `SQARVO_MARIADB_URL`
//! （形如 `mysql://user:***@127.0.0.1:53306/sqarvo_test_140a`）；未设置时用例打印跳过原因。
//!
//! 前置：
//!
//! - 第二个库 = `<DSN 里的库>_b`（例如 `sqarvo_test_140a` → `sqarvo_test_140a_b`），
//!   测试账号需要对它拥有全权限；CI 的服务容器与 `docs/database/mariadb.md` 的容器启动
//!   命令都会建好并授权。库不存在时用例会尝试 `CREATE DATABASE`，失败则给出要执行的原话；
//! - 用例自建 / 自删同名表 `probe_<随机后缀>`，两个库里内容不同：
//!   主库 2 行、第二库 3 行，因此「读/写到了哪个库」从行内容就能直接看出来。
//!
//! 覆盖：
//!
//! 1. 会话默认库 = A，`BrowseTable(catalog=B)` 返回 B 的行，`catalog` 留空仍返回 A 的行；
//! 2. 无默认库会话：`catalog=B` 能浏览，`catalog` 留空 → `NO_DATABASE`；
//! 3. `ApplyRowChanges` / `InsertRows` 带 `catalog=B` 时只影响 B，A 的行数与内容不变；
//! 4. testkit 跨库不变量（[`catalog_scoping_is_session_independent`]）。

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use sqarvo_common::{ErrorCategory, QueryId};
use sqarvo_driver_api::{
    BrowseOptions, ColumnChange, ConnectionParams, Driver, DriverSession, ExecuteRequest,
    InsertRow, InsertRowsRequest, ObjectKind, ObjectRef, PageRequest, QualifiedName, Row,
    RowChange, RowChangeCancel, RowChangeRequest, RowKey, Secret, TableDataProvider, TablePage,
    TlsMode, Value,
};
use sqarvo_driver_mariadb::{MariaDbDriver, error};

// ---------------------------------------------------------------------------
// 基础设施
// ---------------------------------------------------------------------------

/// DSN 解析结果：主机/端口/账号 + 默认库名。
struct Dsn {
    params: ConnectionParams,
    secret: Secret,
    database: String,
}

fn dsn() -> Option<Dsn> {
    let Ok(url) = std::env::var("SQARVO_MARIADB_URL") else {
        eprintln!("跳过：未设置 SQARVO_MARIADB_URL（示例 mysql://user:***@127.0.0.1:53306/db）");
        return None;
    };
    let rest = url
        .strip_prefix("mysql://")
        .expect("SQARVO_MARIADB_URL 必须以 mysql:// 开头");
    let (auth, authority) = rest.split_once('@').expect("URL 缺少 @");
    let (user, password) = auth.split_once(':').expect("URL 缺少密码");
    let (endpoint, database) = authority
        .split_once('/')
        .expect("跨库用例要求 DSN 里带默认库（第二库由它派生）");
    let (host_name, port_number) = match endpoint.split_once(':') {
        Some((name, port)) => (name.to_owned(), port.parse().expect("端口必须是数字")),
        None => (endpoint.to_owned(), 3306),
    };
    let params = ConnectionParams {
        driver_id: "mariadb".parse().unwrap(),
        host: host_name,
        port: port_number,
        database: Some(database.to_owned()),
        username: Some(user.to_owned()),
        credential_ref: Some("sqarvo:conn/cross140".to_owned()),
        tls: TlsMode::Preferred,
        tls_server_name: None,
        properties: BTreeMap::new(),
    };
    Some(Dsn {
        params,
        secret: Secret::new(password),
        database: database.to_owned(),
    })
}

async fn connect(params: &ConnectionParams, secret: &Secret) -> Box<dyn DriverSession> {
    MariaDbDriver::new()
        .connect(params, Some(secret))
        .await
        .expect("连接 MariaDB 测试库")
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

/// 进程级随机后缀：并行跑同一个测试二进制也不会撞表名。
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

/// 表名带用例后缀：同一进程里的用例并行执行，不能共用同一个表名。
fn table_name(case: &str) -> String {
    format!("probe_140_{}_{case}", nonce())
}

fn q(identifier: &str) -> String {
    format!("`{}`", identifier.replace('`', "``"))
}

/// 跨库用例的夹具：两个库 + 同名表。
struct Fixture {
    primary: String,
    secondary: String,
    table: String,
}

impl Fixture {
    /// 建第二库与两张同名表；两张表内容不同（主库 2 行、第二库 3 行）。
    async fn prepare(session: &dyn DriverSession, dsn: &Dsn, case: &str) -> Self {
        let fixture = Self {
            primary: dsn.database.clone(),
            secondary: format!("{}_b", dsn.database),
            table: table_name(case),
        };
        // 第二库不存在时尝试自建：账号对它没有 CREATE 权限就会失败，
        // 此时把需要 root 执行的原话打出来，而不是让后面的建表报一个费解的 1044。
        if let Err(error) = session
            .execute(ExecuteRequest {
                query_id: QueryId::new(),
                sql: format!("CREATE DATABASE IF NOT EXISTS {}", q(&fixture.secondary)),
                max_rows: None,
                timeout: None,
            })
            .await
        {
            panic!(
                "第二库 {} 不存在且测试账号无法创建它（{}）。\
                 请用 root 执行：CREATE DATABASE {}; GRANT ALL ON {}.* TO '测试账号'@'%';",
                fixture.secondary,
                error.message(),
                q(&fixture.secondary),
                q(&fixture.secondary)
            );
        }

        let create = format!(
            "CREATE TABLE {} (id INT NOT NULL PRIMARY KEY, name VARCHAR(32) NOT NULL) ENGINE=InnoDB",
            fixture.qualified(&fixture.primary)
        );
        let create_secondary = format!(
            "CREATE TABLE {} (id INT NOT NULL PRIMARY KEY, name VARCHAR(32) NOT NULL) ENGINE=InnoDB",
            fixture.qualified(&fixture.secondary)
        );
        fixture.drop_tables(session).await;
        exec(session, &create).await;
        exec(session, &create_secondary).await;
        exec(
            session,
            &format!(
                "INSERT INTO {} VALUES (1,'a1'),(2,'a2')",
                fixture.qualified(&fixture.primary)
            ),
        )
        .await;
        exec(
            session,
            &format!(
                "INSERT INTO {} VALUES (10,'b10'),(11,'b11'),(12,'b12')",
                fixture.qualified(&fixture.secondary)
            ),
        )
        .await;
        fixture
    }

    fn qualified(&self, database: &str) -> String {
        format!("{}.{}", q(database), q(&self.table))
    }

    async fn drop_tables(&self, session: &dyn DriverSession) {
        for database in [&self.primary, &self.secondary] {
            let _ = session
                .execute(ExecuteRequest {
                    query_id: QueryId::new(),
                    sql: format!("DROP TABLE IF EXISTS {}", self.qualified(database)),
                    max_rows: None,
                    timeout: None,
                })
                .await;
        }
    }
}

fn object(session: &dyn DriverSession, catalog: Option<&str>, table: &str) -> ObjectRef {
    ObjectRef {
        session_id: session.session_id(),
        qualified_name: QualifiedName::new(catalog.map(str::to_owned), None, table),
        kind: ObjectKind::Table,
    }
}

fn provider(session: &dyn DriverSession) -> &dyn TableDataProvider {
    session.table_data().expect("MariaDB 必须提供表数据能力")
}

async fn browse(session: &dyn DriverSession, catalog: Option<&str>, table: &str) -> TablePage {
    provider(session)
        .browse(
            &object(session, catalog, table),
            &PageRequest {
                page_size: 100,
                page_token: None,
            },
            &BrowseOptions::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("浏览 catalog={catalog:?} 失败：{error:?}"))
}

fn ids(page: &TablePage) -> Vec<i64> {
    page.rows
        .iter()
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
async fn browse_uses_request_catalog_not_session_default() {
    let Some(dsn) = dsn() else {
        return;
    };
    let session = connect(&dsn.params, &dsn.secret).await;
    let fixture = Fixture::prepare(&*session, &dsn, "browse").await;

    // 会话默认库 = A，请求显式指定 B：必须读 B，且不与会话默认库比较。
    let secondary = browse(&*session, Some(&fixture.secondary), &fixture.table).await;
    assert_eq!(ids(&secondary), vec![10, 11, 12], "catalog=B 必须读 B 库");
    assert_eq!(text(&secondary.rows, 10), "b10");

    // catalog 留空：回落会话默认库（旧客户端行为不变）。
    let fallback = browse(&*session, None, &fixture.table).await;
    assert_eq!(ids(&fallback), vec![1, 2], "catalog 留空必须回落默认库 A");
    assert_eq!(text(&fallback.rows, 1), "a1");

    // 显式 A 与留空结果一致。
    let primary = browse(&*session, Some(&fixture.primary), &fixture.table).await;
    assert_eq!(ids(&primary), ids(&fallback));

    // 浏览 B 之后再留空：仍然是 A（驱动不得「记住」上一次的库）。
    let again = browse(&*session, None, &fixture.table).await;
    assert_eq!(ids(&again), vec![1, 2], "浏览别的库不得改变会话默认库");

    fixture.drop_tables(&*session).await;
}

#[tokio::test]
async fn session_without_default_database_can_browse_explicit_catalog() {
    let Some(dsn) = dsn() else {
        return;
    };
    // 先用默认库会话把夹具建好，再用无默认库会话浏览。
    let setup = connect(&dsn.params, &dsn.secret).await;
    let fixture = Fixture::prepare(&*setup, &dsn, "nodefault").await;

    let mut params = dsn.params.clone();
    params.database = None;
    let session = connect(&params, &dsn.secret).await;

    let page = browse(&*session, Some(&fixture.secondary), &fixture.table).await;
    assert_eq!(
        ids(&page),
        vec![10, 11, 12],
        "无默认库 + catalog=B 必须能浏览"
    );

    let error = provider(&*session)
        .browse(
            &object(&*session, None, &fixture.table),
            &PageRequest {
                page_size: 10,
                page_token: None,
            },
            &BrowseOptions::default(),
        )
        .await
        .expect_err("无默认库 + catalog 留空必须显式失败");
    assert_eq!(error.code(), error::NO_DATABASE);
    assert_eq!(error.category(), ErrorCategory::FailedPrecondition);
    assert!(
        error.to_string().contains("默认库"),
        "文案必须告诉用户怎么补库名：{error}"
    );

    fixture.drop_tables(&*setup).await;
}

// ---------------------------------------------------------------------------
// 行变更与插入
// ---------------------------------------------------------------------------

#[tokio::test]
async fn row_changes_only_touch_the_requested_catalog() {
    let Some(dsn) = dsn() else {
        return;
    };
    let session = connect(&dsn.params, &dsn.secret).await;
    let fixture = Fixture::prepare(&*session, &dsn, "apply").await;

    provider(&*session)
        .apply_row_changes(
            RowChangeRequest {
                object: object(&*session, Some(&fixture.secondary), &fixture.table),
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
        .expect("更新 B 库的行");

    let secondary = browse(&*session, Some(&fixture.secondary), &fixture.table).await;
    assert_eq!(text(&secondary.rows, 10), "edited", "写入必须落在 B 库");

    let primary = browse(&*session, Some(&fixture.primary), &fixture.table).await;
    assert_eq!(ids(&primary), vec![1, 2], "A 库的行数不能变");
    assert_eq!(text(&primary.rows, 1), "a1", "A 库的内容不能变");
    assert_eq!(text(&primary.rows, 2), "a2", "A 库的内容不能变");

    fixture.drop_tables(&*session).await;
}

#[tokio::test]
async fn inserts_only_touch_the_requested_catalog() {
    let Some(dsn) = dsn() else {
        return;
    };
    let session = connect(&dsn.params, &dsn.secret).await;
    let fixture = Fixture::prepare(&*session, &dsn, "insert").await;

    provider(&*session)
        .insert_rows(InsertRowsRequest {
            object: object(&*session, Some(&fixture.secondary), &fixture.table),
            rows: vec![InsertRow::new(vec![
                ColumnChange {
                    column: "id".to_owned(),
                    value: Value::Int(13),
                },
                ColumnChange {
                    column: "name".to_owned(),
                    value: Value::Text("b13".to_owned()),
                },
            ])],
        })
        .await
        .expect("向 B 库插入一行");

    let secondary = browse(&*session, Some(&fixture.secondary), &fixture.table).await;
    assert_eq!(ids(&secondary), vec![10, 11, 12, 13], "B 库必须多一行");
    assert_eq!(text(&secondary.rows, 13), "b13");

    let primary = browse(&*session, Some(&fixture.primary), &fixture.table).await;
    assert_eq!(ids(&primary), vec![1, 2], "A 库不能多出插入的行");

    fixture.drop_tables(&*session).await;
}

// ---------------------------------------------------------------------------
// cross-driver invariant
// ---------------------------------------------------------------------------

/// testkit 跨库不变量（ADR 0024）：同一个对象引用在「有默认库 / 无默认库」会话下
/// 返回同样的列与行；省略 catalog 回落到默认库，且跨库浏览不改变这一回落。
#[tokio::test]
async fn table_data_satisfies_shared_catalog_invariant() {
    let Some(dsn) = dsn() else {
        return;
    };
    let with_default = connect(&dsn.params, &dsn.secret).await;
    let fixture = Fixture::prepare(&*with_default, &dsn, "invariant").await;

    let mut params = dsn.params.clone();
    params.database = None;
    let without_default = connect(&params, &dsn.secret).await;

    sqarvo_driver_testkit::catalog_scoping_is_session_independent(
        &*with_default,
        &*without_default,
        &sqarvo_driver_testkit::CatalogTable {
            catalog: &fixture.secondary,
            default_catalog: &fixture.primary,
            name: &fixture.table,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("MariaDB 未通过跨库表数据不变量：{error:?}"));

    fixture.drop_tables(&*with_default).await;
}
