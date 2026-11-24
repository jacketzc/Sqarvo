//! SQLite 集成测试（#209）：经真实驱动与 `AccessService` 验证 8 个工具的行为。
//!
//! 夹具是临时 SQLite 文件库，不需要外部服务；写入通过 `SessionHandle::execute`
//! 直接完成（工具层本身只读，写入路径仅用于布置数据）。覆盖：
//!
//! - 连接发现 / Schema Discovery / 样本读取 / 只读查询的正向路径；
//! - DML 与多语句被 `policy` 拒绝；
//! - 行数上限与字节上限分别触发 `truncated`，且与单元格级截断互不混淆；
//! - `explain_query` 返回 SQLite 原生查询计划；
//! - 非空 `schema` 参数返回驱动的 `capability.unsupported`（不静默忽略）。

#[path = "support/profile_access.rs"]
mod profile_access;

use std::path::Path;
use std::sync::Arc;

use sqarvo_common::{ErrorCode, QueryId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::ExecuteRequest;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::MemoryCredentialStore;
use sqarvo_mcp::access::AccessService;
use sqarvo_mcp::tools::{connection, explain, query, schema};

/// 构造只含 `dev` SQLite 连接的访问服务。
fn access(db: &Path, enabled_tools: Option<&[&str]>) -> profile_access::TestAccess {
    access_with_limits(db, enabled_tools, serde_json::json!({}))
}

fn access_with_limits(
    db: &Path,
    enabled_tools: Option<&[&str]>,
    query_limits: serde_json::Value,
) -> profile_access::TestAccess {
    let mut root = serde_json::json!({
        "connections": [{
            "alias": "dev",
            "driver_id": "sqlite",
            "host": db.display().to_string(),
            "port": 0,
            "enabled": true,
            "properties": { "read_only": "false" }
        }]
    });
    root["query_limits"] = query_limits;
    if let Some(tools) = enabled_tools {
        root["enabled_tools"] = serde_json::json!(tools);
    }
    let config = sqarvo_mcp::config::parse(&root.to_string())
        .expect("Parse test configuration")
        .validate()
        .expect("Validate test configuration");
    let registry = Arc::new(DriverRegistry::with_builtin_drivers());
    let credentials = Arc::new(MemoryCredentialStore::new());
    let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
    profile_access::service(config, sessions)
}

/// 直接经驱动执行一条语句（测试布置数据用，不经过只读策略）。
async fn exec(access: &AccessService, sql: &str) {
    access
        .run("dev", |handle| {
            let sql = sql.to_owned();
            async move {
                handle
                    .execute(ExecuteRequest {
                        query_id: QueryId::new(),
                        sql,
                        max_rows: None,
                        timeout: None,
                    })
                    .await
                    .map(|_| ())
            }
        })
        .await
        .unwrap_or_else(|error| panic!("布置数据失败：{error}\nSQL: {sql}"));
}

fn sample_params(table: &str, limit: i64) -> query::GetTableSampleParams {
    query::GetTableSampleParams {
        connection_alias: "dev".to_owned(),
        database: "main".to_owned(),
        table: table.to_owned(),
        limit,
    }
}

fn execute_params(sql: &str) -> query::ExecuteReadonlyQueryParams {
    query::ExecuteReadonlyQueryParams {
        connection_alias: "dev".to_owned(),
        database: String::new(),
        sql: sql.to_owned(),
    }
}

#[tokio::test]
async fn list_connections_does_not_open_a_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("discovery.db"), None);
    let output = connection::list_connections(&access)
        .await
        .expect("列出连接");
    assert_eq!(output.count, 1);
    assert_eq!(output.connections[0].alias, "dev");
    assert_eq!(output.connections[0].driver_id, "sqlite");
    assert_eq!(output.connections[0].connection_status, "not_connected");
    assert_eq!(output.connections[0].database, "");
    assert_eq!(output.connections[0].last_error_code, "");
    assert_eq!(
        access.session_manager().active_count().await,
        0,
        "list_connections 不得触发数据库连接"
    );
}

#[tokio::test]
async fn schema_discovery_and_sample_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("schema.db"), None);
    exec(
        &access,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL)",
    )
    .await;
    exec(&access, "CREATE VIEW v_users AS SELECT id FROM users").await;
    exec(
        &access,
        "INSERT INTO users (id, name, score) VALUES (1, 'alice', 1.5), (2, 'bob', 2.5), (3, 'carol', NULL)",
    )
    .await;

    let databases = schema::list_databases(
        &access,
        &schema::ListDatabasesParams {
            connection_alias: "dev".to_owned(),
        },
    )
    .await
    .expect("列出数据库");
    assert!(
        databases.databases.iter().any(|db| db.name == "main"),
        "SQLite 至少应有 main：{databases:?}"
    );
    assert!(!databases.truncated);

    let tables = schema::list_tables(
        &access,
        &schema::ListTablesParams {
            connection_alias: "dev".to_owned(),
            database: "main".to_owned(),
            schema: String::new(),
        },
    )
    .await
    .expect("列出表");
    let users = tables
        .tables
        .iter()
        .find(|table| table.name == "users")
        .expect("应包含 users");
    assert_eq!(users.kind, "table");
    let view = tables
        .tables
        .iter()
        .find(|table| table.name == "v_users")
        .expect("应包含视图");
    assert_eq!(view.kind, "view");

    let description = schema::describe_table(
        &access,
        &schema::DescribeTableParams {
            connection_alias: "dev".to_owned(),
            database: "main".to_owned(),
            table: "users".to_owned(),
            schema: String::new(),
        },
    )
    .await
    .expect("描述表");
    assert_eq!(description.columns.len(), 3);
    assert!(description.has_primary_key);
    assert_eq!(description.primary_key.len(), 1);
    assert_eq!(description.primary_key[0].name, "id");
    assert_eq!(description.primary_key[0].position, 1);
    let name = description
        .columns
        .iter()
        .find(|column| column.name == "name")
        .expect("name 列");
    assert!(!name.nullable);
    assert_eq!(name.native_type, "TEXT");
    // 没有的字段不伪造：默认值状态必须落在四态之一。
    assert!(
        ["unspecified", "no_default", "null", "expression"].contains(&name.default_kind.as_str()),
        "{}",
        name.default_kind
    );

    let sample = query::get_table_sample(&access, &sample_params("users", 2))
        .await
        .expect("读取样本");
    assert_eq!(sample.row_count, 2);
    assert!(sample.truncated, "还有第三行，必须标记行级截断");
    assert_eq!(sample.rows.len(), 2);
    assert_eq!(sample.columns.len(), 3);
    assert_eq!(sample.columns[0].name, "id");
    assert_eq!(sample.columns[0].kind, "integer");
}

#[tokio::test]
async fn execute_readonly_query_returns_typed_columns_and_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("query.db"), None);
    exec(
        &access,
        "CREATE TABLE nums (id INTEGER PRIMARY KEY, label TEXT)",
    )
    .await;
    exec(&access, "INSERT INTO nums VALUES (1, 'one'), (2, 'two')").await;

    let outcome = query::execute_readonly_query(
        &access,
        &execute_params("SELECT id, label FROM nums ORDER BY id"),
    )
    .await
    .expect("只读查询");
    assert_eq!(outcome.output.columns.len(), 2);
    assert_eq!(outcome.output.columns[0].name, "id");
    assert_eq!(outcome.output.columns[0].kind, "integer");
    assert_eq!(outcome.output.columns[1].kind, "text");
    assert_eq!(outcome.output.row_count, 2);
    assert!(!outcome.output.truncated);
    assert_eq!(outcome.output.truncated_reason, "none");
    assert_eq!(outcome.output.rows[0][0], serde_json::json!(1));
    assert_eq!(outcome.output.rows[0][1], serde_json::json!("one"));
    assert!(!outcome.fingerprint.is_empty(), "审计需要 SQL 指纹");
}

#[tokio::test]
async fn write_and_multi_statement_sql_are_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("reject.db"), None);
    exec(&access, "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").await;

    for sql in [
        "INSERT INTO t VALUES (1, 'x')",
        "UPDATE t SET v = 'y'",
        "DELETE FROM t",
        "CREATE TABLE u (id INTEGER)",
        "SELECT 1; SELECT 2",
    ] {
        let error = query::execute_readonly_query(&access, &execute_params(sql))
            .await
            .expect_err("写操作/多语句必须拒绝");
        assert_eq!(error.code(), ErrorCode::MCP_READONLY_VIOLATION, "{sql}");
    }
}

#[tokio::test]
async fn row_limit_truncates_and_reports_the_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("rows.db"), None);
    exec(&access, "CREATE TABLE nums (v INTEGER)").await;
    let values = (0..250)
        .map(|index| format!("({index})"))
        .collect::<Vec<_>>()
        .join(",");
    exec(&access, &format!("INSERT INTO nums (v) VALUES {values}")).await;

    let outcome = query::execute_readonly_query(&access, &execute_params("SELECT v FROM nums"))
        .await
        .expect("只读查询");
    assert_eq!(outcome.output.row_count, 200, "默认行限额 200");
    assert!(outcome.output.truncated);
    assert_eq!(outcome.output.truncated_reason, "rows");
    assert_eq!(outcome.output.rows.len(), 200);
}

#[tokio::test]
async fn byte_limit_truncates_before_the_row_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("bytes.db"), None);
    exec(&access, "CREATE TABLE blobs (id INTEGER, payload TEXT)").await;
    let payload = "x".repeat(60_000);
    for index in 0..40 {
        exec(
            &access,
            &format!("INSERT INTO blobs (id, payload) VALUES ({index}, '{payload}')"),
        )
        .await;
    }

    let outcome =
        query::execute_readonly_query(&access, &execute_params("SELECT id, payload FROM blobs"))
            .await
            .expect("只读查询");
    assert!(outcome.output.truncated);
    assert_eq!(outcome.output.truncated_reason, "bytes");
    assert!(
        outcome.output.row_count < 40,
        "1 MiB 上限必须先于 40 行生效，实际 {} 行",
        outcome.output.row_count
    );
    assert!(
        outcome.output.row_count > 0,
        "至少要交付一行，不能因为计费把结果清空"
    );
}

#[tokio::test]
async fn cell_level_truncation_is_independent_from_result_truncation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("cells.db"), None);
    exec(
        &access,
        "CREATE TABLE docs (id INTEGER, body TEXT, note TEXT)",
    )
    .await;
    let body = "y".repeat(70 * 1024);
    exec(
        &access,
        &format!("INSERT INTO docs (id, body, note) VALUES (1, '{body}', 'short')"),
    )
    .await;

    let outcome =
        query::execute_readonly_query(&access, &execute_params("SELECT id, body, note FROM docs"))
            .await
            .expect("只读查询");
    assert!(
        !outcome.output.truncated,
        "70 KiB 未触达 1 MiB 结果上限，结果级截断必须为 false"
    );
    let row = &outcome.output.rows[0];
    // 短值是裸标量；被截断的单元格是对象——两者在 JSON 结构上可区分。
    assert_eq!(row[2], serde_json::json!("short"));
    let cell = row[1]
        .as_object()
        .unwrap_or_else(|| panic!("截断单元格必须是对象：{:?}", row[1]));
    assert_eq!(cell["truncated"], true);
    assert_eq!(cell["type"], "text");
    assert_eq!(
        cell["value"].as_str().map(str::len),
        Some(64 * 1024),
        "ADR 0007 的 64 KiB 单元格上限"
    );
}

#[tokio::test]
async fn sqlite_query_plan_preserves_native_columns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("explain.db"), None);
    exec(&access, "CREATE TABLE users (id INTEGER, email TEXT)").await;
    exec(&access, "CREATE INDEX users_email ON users(email)").await;
    for (sql, feature) in [
        ("SELECT * FROM users", "SCAN"),
        (
            "SELECT * FROM users WHERE email = 'test@example.invalid'",
            "users_email",
        ),
        (
            "SELECT * FROM users a JOIN users b ON a.email = b.email",
            "users_email",
        ),
        ("WITH c AS (SELECT * FROM users) SELECT * FROM c", "SCAN"),
    ] {
        let plan = explain::explain_query(
            &access,
            &explain::ExplainQueryParams {
                connection_alias: "dev".into(),
                database: "main".into(),
                sql: sql.into(),
            },
        )
        .await
        .expect("native SQLite plan");
        let names = plan
            .output
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["id", "parent", "notused", "detail"]);
        assert!(plan.output.rows.iter().any(|row| {
            row[3]
                .as_str()
                .is_some_and(|detail| detail.contains(feature))
        }));
    }
    for sql in [
        "EXPLAIN SELECT 1",
        "ANALYZE",
        "DELETE FROM users",
        "DROP TABLE users",
        "SELECT 1; SELECT 2",
        "SELECT load_extension('example')",
        "SELECT * FROM",
    ] {
        let error = explain::explain_query(
            &access,
            &explain::ExplainQueryParams {
                connection_alias: "dev".into(),
                database: String::new(),
                sql: sql.into(),
            },
        )
        .await
        .expect_err("plan policy must refuse this statement");
        assert!(matches!(
            error.code(),
            ErrorCode::MCP_SQL_PARSE | ErrorCode::MCP_READONLY_VIOLATION
        ));
    }
}

#[tokio::test]
async fn sqlite_query_plan_honors_row_byte_and_cancellation_limits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plan-budget.db");
    let setup = access(&db, None);
    let table = format!("long_{}", "a".repeat(380));
    exec(&setup, &format!("CREATE TABLE {table}(id INTEGER)")).await;
    let params = explain::ExplainQueryParams {
        connection_alias: "dev".into(),
        database: "main".into(),
        sql: format!(
            "SELECT * FROM {table} a JOIN {table} b ON a.id = b.id JOIN {table} c ON b.id = c.id"
        ),
    };
    let row_access = access_with_limits(&db, None, serde_json::json!({"max_rows":1}));
    let output = explain::explain_query(&row_access, &params)
        .await
        .expect("row budget")
        .output;
    assert_eq!(output.row_count, 1);
    assert_eq!(output.truncated_reason, "rows");
    let byte_access = access_with_limits(&db, None, serde_json::json!({"max_result_bytes":1024}));
    let params = explain::ExplainQueryParams {
        sql: format!(
            "SELECT id FROM {table} UNION ALL SELECT id FROM {table} UNION ALL SELECT id FROM {table}"
        ),
        ..params
    };
    let output = explain::explain_query(&byte_access, &params)
        .await
        .expect("byte budget")
        .output;
    assert_eq!(output.truncated_reason, "bytes");
    assert!(serde_json::to_vec(&output).unwrap().len() <= 1024);
    let error =
        explain::explain_query_with_cancel(&byte_access, &params, &|| std::future::ready(()))
            .await
            .expect_err("client cancelled plan");
    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);
    assert_eq!(byte_access.queries().active(), 0);
}

#[tokio::test]
async fn sqlite_schema_budget_keeps_cursor_progress_and_omits_whole_ddl() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access_with_limits(
        &dir.path().join("schema-budget.db"),
        None,
        serde_json::json!({"max_result_bytes":1024}),
    );
    let definition = format!("CREATE TABLE a(value TEXT DEFAULT '{}')", "x".repeat(1300));
    exec(&access, &definition).await;
    exec(&access, "CREATE TABLE b(value INTEGER)").await;
    let mut params = schema::DatabaseSchemaParams {
        connection_alias: "dev".into(),
        database: "main".into(),
        schema: String::new(),
        tables: vec![],
        include_columns: true,
        include_indexes: false,
        include_foreign_keys: false,
        page_size: 1,
        page_token: None,
    };
    let first = schema::get_database_schema(&access, &params)
        .await
        .expect("first page");
    assert_eq!(first.objects[0].name, "a");
    assert_eq!(first.truncated_reason, "bytes");
    assert!(first.objects[0].columns.is_none());
    assert!(serde_json::to_vec(&first).unwrap().len() <= 1024);
    params.page_token = first.next_page_token;
    let second = schema::get_database_schema(&access, &params)
        .await
        .expect("second page");
    assert_eq!(second.objects[0].name, "b");
    assert!(second.next_page_token.is_none());
    let ddl = schema::get_table_ddl(
        &access,
        &schema::DescribeTableParams {
            connection_alias: "dev".into(),
            database: "main".into(),
            table: "a".into(),
            schema: String::new(),
        },
    )
    .await
    .expect("bounded DDL");
    assert!(ddl.ddl.is_none());
    assert!(!ddl.complete);
    assert!(ddl.warnings.contains(&"ddl_exceeds_response_budget"));
}

#[tokio::test]
async fn sqlite_foreign_keys_preserve_composite_pairs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("foreign-keys.db"), None);
    exec(
        &access,
        "CREATE TABLE parent (a INTEGER, b INTEGER, PRIMARY KEY (b, a))",
    )
    .await;
    exec(&access, "CREATE TABLE child (x INTEGER, y INTEGER, FOREIGN KEY (x, y) REFERENCES parent (b, a) ON DELETE CASCADE ON UPDATE SET NULL)").await;
    let mut params = schema::DescribeTableParams {
        connection_alias: "dev".into(),
        database: "main".into(),
        table: "child".into(),
        schema: String::new(),
    };
    let result = schema::list_foreign_keys(&access, &params)
        .await
        .expect("foreign keys");
    assert_eq!(result.foreign_keys.len(), 1);
    let key = &result.foreign_keys[0];
    assert_eq!(key.name, None);
    assert_eq!(key.target.table, "parent");
    assert_eq!(key.on_delete, "CASCADE");
    assert_eq!(key.on_update, "SET NULL");
    assert_eq!(
        key.columns
            .iter()
            .map(|pair| (pair.column.as_str(), pair.referenced_column.as_deref()))
            .collect::<Vec<_>>(),
        [("x", Some("b")), ("y", Some("a"))]
    );
    params.table = "parent".into();
    assert!(
        schema::list_foreign_keys(&access, &params)
            .await
            .expect("parent")
            .foreign_keys
            .is_empty()
    );
    params.table = "missing".into();
    assert_eq!(
        schema::list_foreign_keys(&access, &params)
            .await
            .expect_err("missing")
            .code(),
        ErrorCode::METADATA_OBJECT_NOT_FOUND
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One schema verifies that each tool agrees with the same objects.
async fn sqlite_schema_intelligence_reads_native_metadata_and_pages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("intelligence.db"), None);
    exec(&access, "CREATE TABLE parent (id INTEGER PRIMARY KEY AUTOINCREMENT, a INTEGER, b INTEGER, c INTEGER GENERATED ALWAYS AS (a + b) STORED, UNIQUE (a, b), CONSTRAINT positive CHECK (a > 0))").await;
    exec(
        &access,
        "CREATE TABLE child (x INTEGER REFERENCES parent, value INTEGER CHECK (value >= 0))",
    )
    .await;
    exec(
        &access,
        "CREATE INDEX child_order ON child (x DESC, value ASC, abs(value))",
    )
    .await;
    exec(
        &access,
        "CREATE UNIQUE INDEX child_explicit ON child (value)",
    )
    .await;
    exec(&access, "CREATE VIEW child_view AS SELECT x FROM child").await;
    let mut params = schema::DescribeTableParams {
        connection_alias: "dev".into(),
        database: "main".into(),
        table: "parent".into(),
        schema: String::new(),
    };
    let description = schema::describe_table(&access, &params)
        .await
        .expect("columns");
    assert_eq!(description.columns[0].identity, Some("autoincrement"));
    assert_eq!(description.columns[3].generated, Some(true));
    assert_eq!(
        description.columns[3].generation_expression.as_deref(),
        Some("a + b")
    );
    let constraints = schema::list_constraints(&access, &params)
        .await
        .expect("parent constraints");
    assert!(
        constraints
            .constraints
            .iter()
            .any(|item| item.kind == "unique"
                && item.columns.as_deref() == Some(&["a".into(), "b".into()]))
    );
    let check = constraints
        .constraints
        .iter()
        .find(|item| item.kind == "check")
        .expect("check");
    assert_eq!(check.name.as_deref(), Some("positive"));
    assert_eq!(check.check_expression.as_deref(), Some("a > 0"));
    let unknown = schema::get_table_statistics(&access, &params)
        .await
        .expect("unknown stats");
    assert_eq!(unknown.row_count_kind, "unknown");
    assert_eq!(unknown.row_count, None);
    assert_eq!(
        (unknown.data_bytes, unknown.index_bytes, unknown.total_bytes),
        (None, None, None)
    );
    exec(&access, "INSERT INTO parent(a,b) VALUES(1,2),(3,4)").await;
    // Fixture setup only. The tool must only read an existing statistic.
    exec(&access, "ANALYZE parent").await;
    let estimate = schema::get_table_statistics(&access, &params)
        .await
        .expect("existing stats");
    assert_eq!(
        (estimate.row_count, estimate.row_count_kind),
        (Some(2), "estimate")
    );
    params.table = "child".into();
    let constraints = schema::list_constraints(&access, &params)
        .await
        .expect("child constraints");
    assert!(
        !constraints
            .constraints
            .iter()
            .any(|item| item.kind == "unique")
    );
    let key = constraints
        .constraints
        .iter()
        .find_map(|item| item.foreign_key.as_ref())
        .expect("foreign key");
    assert_eq!(key.columns[0].referenced_column.as_deref(), Some("id"));
    params.table = "child_view".into();
    let ddl = schema::get_table_ddl(&access, &params)
        .await
        .expect("view definition");
    assert_eq!(
        (ddl.object_type, ddl.source, ddl.complete),
        ("view", "database_native", true)
    );
    assert!(ddl.ddl.unwrap().starts_with("CREATE VIEW child_view"));
    exec(&access, "CREATE VIRTUAL TABLE search USING fts5(body)").await;
    params.table = "search".into();
    let virtual_ddl = schema::get_table_ddl(&access, &params)
        .await
        .expect("virtual table DDL");
    assert_eq!(virtual_ddl.object_type, "virtual_table");
    assert!(virtual_ddl.ddl.unwrap().starts_with("CREATE VIRTUAL TABLE"));
    params.table = "search_data".into();
    assert_eq!(
        schema::get_table_ddl(&access, &params)
            .await
            .expect_err("shadow table")
            .code(),
        ErrorCode::CAPABILITY_UNSUPPORTED
    );
    let mut page_params = schema::DatabaseSchemaParams {
        connection_alias: "dev".into(),
        database: "main".into(),
        schema: String::new(),
        tables: vec![],
        include_columns: true,
        include_indexes: true,
        include_foreign_keys: true,
        page_size: 1,
        page_token: None,
    };
    let first = schema::get_database_schema(&access, &page_params)
        .await
        .expect("first page");
    assert_eq!(first.objects[0].name, "child");
    let indexes = first.objects[0].indexes.as_ref().unwrap();
    let parts = indexes
        .iter()
        .find(|index| index.name == "child_order")
        .unwrap()
        .columns
        .as_ref()
        .unwrap();
    assert_eq!(parts[0].sort_order, Some("desc"));
    assert_eq!(parts[1].name.as_deref(), Some("value"));
    assert_eq!(parts[2].kind, "expression");
    page_params.page_token = first.next_page_token;
    assert!(page_params.page_token.is_some());
    let second = schema::get_database_schema(&access, &page_params)
        .await
        .expect("second page");
    assert_eq!(second.objects[0].name, "child_view");
    page_params.include_columns = false;
    assert_eq!(
        schema::get_database_schema(&access, &page_params)
            .await
            .expect_err("cursor scope must match")
            .code(),
        ErrorCode::MCP_INVALID_ARGUMENT
    );
}

#[tokio::test]
async fn non_empty_schema_is_not_silently_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("schema-param.db"), None);
    exec(&access, "CREATE TABLE t (id INTEGER PRIMARY KEY)").await;

    let error = schema::list_tables(
        &access,
        &schema::ListTablesParams {
            connection_alias: "dev".to_owned(),
            database: "main".to_owned(),
            schema: "public".to_owned(),
        },
    )
    .await
    .expect_err("SQLite 没有 schema 层");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);

    let error = schema::describe_table(
        &access,
        &schema::DescribeTableParams {
            connection_alias: "dev".to_owned(),
            database: "main".to_owned(),
            table: "t".to_owned(),
            schema: "public".to_owned(),
        },
    )
    .await
    .expect_err("SQLite 没有 schema 层");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);

    let error = schema::list_indexes(
        &access,
        &schema::ListIndexesParams {
            connection_alias: "dev".to_owned(),
            database: "main".to_owned(),
            table: "t".to_owned(),
            schema: "public".to_owned(),
        },
    )
    .await
    .expect_err("SQLite 没有 schema 层");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
}

#[tokio::test]
async fn index_names_and_native_details_are_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("index.db"), None);
    exec(
        &access,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT UNIQUE)",
    )
    .await;
    // SQLite 的自动索引（sqlite_autoindex_*）没有 sql 原文，驱动不把它们当独立对象；
    // 显式命名索引才是可发现的。
    exec(&access, "CREATE INDEX idx_email ON t (email)").await;

    let output = schema::list_indexes(
        &access,
        &schema::ListIndexesParams {
            connection_alias: "dev".to_owned(),
            database: "main".to_owned(),
            table: "t".to_owned(),
            schema: String::new(),
        },
    )
    .await
    .expect("列出索引");
    assert!(
        output.indexes.iter().any(|index| index.name == "idx_email"),
        "应列出显式索引：{:?}",
        output.indexes
    );
    assert_eq!(output.count, 1);
    let index = &output.indexes[0];
    assert_eq!(index.name, "idx_email");
    assert_eq!(index.unique, Some(false));
    assert_eq!(
        index.columns.as_ref().unwrap()[0].name.as_deref(),
        Some("email")
    );
    assert_eq!(output.implicit_indexes.len(), 1);
    assert_eq!(output.implicit_indexes[0].origin.as_deref(), Some("u"));
    assert_eq!(output.implicit_indexes[0].unique, Some(true));
}

#[tokio::test]
async fn client_cancel_signal_cancels_the_query() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("cancel.db"), None);

    // 已取消的信号：run_sql 必须先请求驱动取消、释放登记，再返回 query.cancelled。
    let already_cancelled = || std::future::ready(());
    let error = query::execute_readonly_query_with_cancel(
        &access,
        &execute_params("SELECT 1"),
        &already_cancelled,
    )
    .await
    .expect_err("取消信号必须生效");
    assert_eq!(error.code(), ErrorCode::QUERY_CANCELLED);
    assert_eq!(
        access.queries().active(),
        0,
        "取消路径必须释放 QueryRegistry 登记"
    );
    access.close_all().await;
}

#[tokio::test]
async fn disabled_tool_is_rejected_even_when_called_directly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("disabled.db"), Some(&[]));
    let error = query::get_table_sample(&access, &sample_params("t", 0))
        .await
        .expect_err("工具未启用");
    assert_eq!(error.code(), ErrorCode::MCP_TOOL_DISABLED);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The cases share a fixture to verify session and row state after rollback.
async fn checked_write_transaction_commits_and_rolls_back_on_direct_row_limit() {
    use std::future::pending;
    use std::time::Duration;

    use serde_json::json;
    use sqarvo_local_store::McpAccessPolicy;
    use sqarvo_mcp::access::OperationType;
    use sqarvo_mcp::policy::write::WritePlan;
    use sqarvo_mcp::tools::write_transaction::{TransactionOutcome, execute};

    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("write.db"), None);
    exec(&access, "CREATE TABLE orders (id INTEGER PRIMARY KEY)").await;
    exec(
        &access,
        "WITH RECURSIVE ids(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM ids WHERE id<22)
         INSERT INTO orders SELECT id FROM ids",
    )
    .await;
    access
        .store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();

    let insert = WritePlan::decode(
        json!({
            "operation": "insert", "connection_alias": "dev", "database": "main",
            "table": "orders", "rows": [{"id": 23}]
        }),
        Some(OperationType::Insert),
    )
    .unwrap();
    let delete = WritePlan::decode(
        json!({
            "operation": "delete", "connection_alias": "dev", "database": "main",
            "table": "orders",
            "conditions": [{"column": "id", "operator": "lt", "value": 22}]
        }),
        Some(OperationType::Delete),
    )
    .unwrap();

    for plan in [insert, delete] {
        let operation = plan.action.operation();
        let queries = Arc::clone(access.queries());
        let outcome = access
            .run_write("dev", operation, move |handle| async move {
                Ok(execute(
                    &handle,
                    &queries,
                    &plan,
                    Duration::from_secs(3),
                    pending(),
                    async |handle, plan, query_id| {
                        if operation == OperationType::Insert {
                            sqarvo_mcp::tools::write_preflight::sqlite(handle, plan, query_id)
                                .await?
                                .allowed_rows()
                                .map(Some)
                        } else {
                            Ok(None)
                        }
                    },
                    async |_| Ok(()),
                )
                .await)
            })
            .await
            .unwrap();

        match operation {
            OperationType::Insert => assert!(matches!(outcome, TransactionOutcome::Committed(1))),
            OperationType::Delete => assert!(matches!(
                outcome,
                TransactionOutcome::RolledBack {
                    affected: Some(21),
                    ..
                }
            )),
            _ => unreachable!("test uses insert and delete"),
        }
    }

    let cancelled = WritePlan::decode(
        json!({
            "operation": "insert", "connection_alias": "dev", "database": "main",
            "table": "orders", "rows": [{"id": 24}]
        }),
        Some(OperationType::Insert),
    )
    .unwrap();
    let queries = Arc::clone(access.queries());
    let outcome = access
        .run_write("dev", OperationType::Insert, move |handle| async move {
            let (wrote, reached_commit) = tokio::sync::oneshot::channel::<()>();
            Ok(execute(
                &handle,
                &queries,
                &cancelled,
                Duration::from_secs(3),
                async {
                    let _ = reached_commit.await;
                },
                async |handle, plan, query_id| {
                    sqarvo_mcp::tools::write_preflight::sqlite(handle, plan, query_id)
                        .await?
                        .allowed_rows()
                        .map(Some)
                },
                async |_| {
                    let _ = wrote.send(());
                    pending().await
                },
            )
            .await)
        })
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        TransactionOutcome::RolledBack { affected: Some(1), error }
            if error.code() == ErrorCode::QUERY_CANCELLED
    ));

    let result = query::execute_readonly_query(
        &access,
        &execute_params("SELECT id FROM orders ORDER BY id"),
    )
    .await
    .unwrap();
    assert_eq!(result.output.rows.len(), 23);
    assert_eq!(access.session_manager().active_count().await, 1);
}

#[tokio::test]
async fn sqlite_write_preflight_rejects_schema_side_effects_and_full_table_predicates() {
    use std::future::pending;
    use std::time::Duration;

    use serde_json::json;
    use sqarvo_local_store::McpAccessPolicy;
    use sqarvo_mcp::policy::write::WritePlan;
    use sqarvo_mcp::tools::write_preflight;
    use sqarvo_mcp::tools::write_transaction::{TransactionOutcome, execute};

    let dir = tempfile::tempdir().unwrap();
    let access = access(&dir.path().join("write-scope.db"), None);
    for sql in [
        "CREATE TABLE parent (id INTEGER PRIMARY KEY)",
        "CREATE TABLE child (parent_id INTEGER REFERENCES parent(id) ON DELETE CASCADE)",
        "CREATE TABLE singleton (id INTEGER PRIMARY KEY)",
        "INSERT INTO singleton VALUES (1)",
        "CREATE TABLE events (id INTEGER PRIMARY KEY)",
        "CREATE TRIGGER events_insert AFTER INSERT ON events BEGIN INSERT INTO singleton VALUES (2); END",
        "CREATE TABLE conflict (id INTEGER PRIMARY KEY ON CONFLICT REPLACE, note TEXT)",
        "INSERT INTO conflict VALUES (1, 'original')",
    ] {
        exec(&access, sql).await;
    }
    access
        .store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();

    for mut arguments in [
        json!({"operation":"insert", "table":"parent", "rows":[{"id":1}]}),
        json!({"operation":"insert", "table":"events", "rows":[{"id":1}]}),
        json!({"operation":"insert", "table":"conflict", "rows":[{"id":1, "note":"replacement"}]}),
        json!({"operation":"delete", "table":"singleton",
               "conditions":[{"column":"id", "operator":"gt", "value":0}]}),
        json!({"operation":"delete", "table":"singleton",
               "conditions":[{"column":"missing", "operator":"is_not_null"}]}),
    ] {
        arguments["connection_alias"] = json!("dev");
        arguments["database"] = json!("main");
        let plan = WritePlan::decode(arguments, None).unwrap();
        let queries = Arc::clone(access.queries());
        let outcome = access
            .run_write("dev", plan.action.operation(), move |handle| async move {
                Ok(execute(
                    &handle,
                    &queries,
                    &plan,
                    Duration::from_secs(3),
                    pending(),
                    async |handle, plan, query_id| {
                        write_preflight::sqlite(handle, plan, query_id)
                            .await?
                            .allowed_rows()
                            .map(Some)
                    },
                    async |_| Ok(()),
                )
                .await)
            })
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            TransactionOutcome::RolledBack { affected: None, .. }
        ));
    }
    let result = query::execute_readonly_query(
        &access,
        &execute_params("SELECT note FROM conflict WHERE id = 1"),
    )
    .await
    .unwrap();
    assert!(
        serde_json::to_string(&result.output.rows)
            .unwrap()
            .contains("original")
    );
}

#[tokio::test]
async fn sqlite_noop_update_preserves_matched_row_count() {
    use std::{future::pending, time::Duration};

    use sqarvo_local_store::McpAccessPolicy;
    use sqarvo_mcp::policy::write::WritePlan;
    use sqarvo_mcp::tools::{write_preflight, write_transaction};

    let directory = tempfile::tempdir().unwrap();
    let service = access(&directory.path().join("matched.db"), None);
    exec(
        &service,
        "CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT)",
    )
    .await;
    exec(&service, "INSERT INTO items VALUES (1,'kept'), (2,'other')").await;
    service
        .store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();
    let plan = WritePlan::decode(
        serde_json::json!({
            "operation":"update", "connection_alias":"dev", "database":"main", "table":"items",
            "changes":{"value":"kept"}, "conditions":[{"column":"id","operator":"eq","value":1}]
        }),
        None,
    )
    .unwrap();
    let queries = Arc::clone(service.queries());
    let result = service
        .run_write("dev", plan.action.operation(), move |handle| async move {
            Ok(write_transaction::execute(
                &handle,
                &queries,
                &plan,
                Duration::from_secs(3),
                pending(),
                async |handle, plan, query_id| {
                    let expected = write_preflight::sqlite(handle, plan, query_id)
                        .await?
                        .allowed_rows()?;
                    assert_eq!(expected, 1);
                    Ok(Some(expected))
                },
                async |_| Ok(()),
            )
            .await)
        })
        .await
        .unwrap();
    assert!(matches!(
        result,
        write_transaction::TransactionOutcome::Committed(1)
    ));
}
