//! SQLite 集成测试（#209）：经真实驱动与 `AccessService` 验证 8 个工具的行为。
//!
//! 夹具是临时 SQLite 文件库，不需要外部服务；写入通过 `SessionHandle::execute`
//! 直接完成（工具层本身只读，写入路径仅用于布置数据）。覆盖：
//!
//! - 连接发现 / Schema Discovery / 样本读取 / 只读查询的正向路径；
//! - DML 与多语句被 `policy` 拒绝；
//! - 行数上限与字节上限分别触发 `truncated`，且与单元格级截断互不混淆；
//! - `explain_query` 在 SQLite 上明确返回 `mcp.unsupported`；
//! - 非空 `schema` 参数返回驱动的 `capability.unsupported`（不静默忽略）。

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
fn access(db: &Path, enabled_tools: Option<&[&str]>) -> AccessService {
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
    if let Some(tools) = enabled_tools {
        root["enabled_tools"] = serde_json::json!(tools);
    }
    let config = sqarvo_mcp::config::parse(&root.to_string())
        .expect("解析测试配置")
        .validate()
        .expect("校验测试配置");
    let registry = Arc::new(DriverRegistry::with_builtin_drivers());
    let credentials = Arc::new(MemoryCredentialStore::new());
    let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
    AccessService::with_session_manager(Arc::new(config), sessions)
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
    let output = connection::list_connections(&access).expect("列出连接");
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
async fn explain_query_is_explicitly_unsupported_on_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("explain.db"), None);
    let error = explain::explain_query(
        &access,
        &explain::ExplainQueryParams {
            connection_alias: "dev".to_owned(),
            database: String::new(),
            sql: "SELECT 1".to_owned(),
        },
    )
    .await
    .expect_err("SQLite 不支持 explain_query");
    assert_eq!(error.code(), ErrorCode::MCP_UNSUPPORTED);
    assert!(error.message().contains("EXPLAIN QUERY PLAN"));
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
async fn index_names_are_reported_without_inventing_details() {
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
    // 结构化字段只有名称：details 不在输出里（由描述指向 information_schema）。
    let encoded = serde_json::to_value(&output).expect("序列化");
    let first = &encoded["indexes"][0];
    assert!(first.get("name").is_some());
    assert!(first.get("columns").is_none());
    assert!(first.get("unique").is_none());
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
