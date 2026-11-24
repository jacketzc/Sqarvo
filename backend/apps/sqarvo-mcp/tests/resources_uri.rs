//! URI 编解码契约与资源内容契约（#210，V3.1 §7.1）。
//!
//! 两组断言：
//!
//! 1. **URI 矩阵**（纯函数，不需要数据库）：`/`、`%`、空格、中文、大小写 `%2f`/
//!    `%2F`、非法 `%zz`、双重编码 `%252f`、以及「两个不同原始路径段指向同一个
//!    解码后标识符」的歧义构造；
//! 2. **内容契约**（临时 SQLite 库，真实驱动）：四个资源的字段清单、字段互不重复、
//!    与 `describe_table` 的输出等价、未授权 alias 一律失败、整体超限报错。
//!
//! 这里的字段清单就是 #212 文档要写进「Resource 内容契约」的那张表，改字段先改测试。

#[path = "support/profile_access.rs"]
mod profile_access;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use sqarvo_common::{ErrorCode, QueryId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::ExecuteRequest;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::MemoryCredentialStore;
use sqarvo_mcp::access::AccessService;
use sqarvo_mcp::resources::{self, ResourceRequest};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 构造含 `dev` SQLite 连接的访问服务。
fn access(db: &Path, enabled_tools: Option<serde_json::Value>) -> profile_access::TestAccess {
    let mut root = serde_json::json!({
        "connections": [
            {
                "alias": "dev",
                "driver_id": "sqlite",
                "host": db.display().to_string(),
                "port": 0,
                "enabled": true,
                "properties": { "read_only": "false" }
            }
        ]
    });
    if let Some(tools) = enabled_tools {
        root["enabled_tools"] = tools;
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

/// 经驱动执行一条语句（只用于布置夹具，不经过只读策略）。
async fn exec(access: &AccessService, sql: &str) {
    exec_on(access, "dev", sql).await;
}

/// 同 [`exec`]，但显式指定 alias（MariaDB 用例用 `maria`）。
async fn exec_on(access: &AccessService, alias: &str, sql: &str) {
    access
        .run(alias, |handle| {
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

/// 读出资源并解析成 JSON。
async fn payload(access: &AccessService, uri: &str) -> serde_json::Value {
    let body = resources::read(access, uri)
        .await
        .unwrap_or_else(|error| panic!("读取 {uri} 失败：{error}"));
    assert_eq!(body.uri, uri, "contents[].uri 必须回显请求 URI");
    serde_json::from_str(&body.json)
        .unwrap_or_else(|error| panic!("资源内容必须是合法 JSON：{error}\n{}", body.json))
}

/// 顶层字段名集合（拥有所有权，便于跨语句比较）。
fn keys(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("资源内容必须是 JSON 对象：{value:?}"))
        .keys()
        .cloned()
        .collect()
}

/// 断言字段集合与契约完全一致（多一个、少一个都失败）。
fn assert_keys(value: &serde_json::Value, expected: &[&str]) {
    let actual = keys(value);
    let expected: BTreeSet<String> = expected.iter().map(|name| (*name).to_owned()).collect();
    assert_eq!(actual, expected, "字段清单与契约不一致：{value}");
}

// ---------------------------------------------------------------------------
// URI 编解码矩阵
// ---------------------------------------------------------------------------

#[test]
fn encode_segment_only_keeps_rfc3986_unreserved() {
    assert_eq!(resources::encode_segment("users"), "users");
    assert_eq!(resources::encode_segment("aZ0-._~"), "aZ0-._~");
    assert_eq!(resources::encode_segment("a/b"), "a%2Fb");
    assert_eq!(resources::encode_segment("100%"), "100%25");
    assert_eq!(resources::encode_segment("a b"), "a%20b");
    assert_eq!(
        resources::encode_segment("中文"),
        "%E4%B8%AD%E6%96%87",
        "非 ASCII 必须逐字节编码"
    );
    // 同一标识符只有一种编码形式：大写十六进制。
    assert!(!resources::encode_segment("a/b").contains("%2f"));
}

#[test]
fn decode_segment_round_trips_every_hard_case() {
    for raw in ["users", "a/b", "100%", "a b", "中文", "a%b", "a b/c%d"] {
        let encoded = resources::encode_segment(raw);
        let decoded = resources::decode_segment(&encoded).expect("自编码必须能解回");
        assert_eq!(decoded, raw, "往返失败：{raw} -> {encoded}");
    }
}

#[test]
fn decode_segment_accepts_raw_utf8_and_both_hex_cases() {
    // 客户端直接塞未编码的中文/空格：已经是标识符本身，原样接受。
    assert_eq!(
        resources::decode_segment("中文").expect("原始 UTF-8"),
        "中文"
    );
    assert_eq!(resources::decode_segment("a b").expect("原始空格"), "a b");
    // 编码大小写不同 = 同一个字节。
    assert_eq!(resources::decode_segment("a%2Fb").expect("大写"), "a/b");
    assert_eq!(
        resources::decode_segment("%e4%b8%ad%e6%96%87").expect("小写"),
        "中文"
    );
    // `%2f` 单独看是合法编码，但解出来的 `/` 不含 %XX——会被接受；
    // 而 `a%2fb` 这种「解完还剩 %XX」的形态按双重编码拒绝（见下一个测试）。
    assert_eq!(resources::decode_segment("%2f").expect("裸编码"), "/");
}

#[test]
fn decode_segment_rejects_illegal_utf8_and_percent_sequences() {
    for raw in ["%zz", "%4", "%", "a%2", "%G0"] {
        let error = resources::decode_segment(raw).expect_err("非法百分号编码必须拒绝");
        assert_eq!(error.code(), ErrorCode::MCP_INVALID_ARGUMENT, "{raw}");
    }
    // %E4%B8 是截断的 UTF-8（中文三字节只给两字节）。
    let error = resources::decode_segment("%E4%B8").expect_err("非法 UTF-8 必须拒绝");
    assert_eq!(error.code(), ErrorCode::MCP_INVALID_ARGUMENT);
}

#[test]
fn decode_segment_rejects_double_encoding() {
    // `%252f` 解一次是 `%2f`——还能再解一次，编码歧义。一律拒绝。
    for raw in ["%252f", "a%252Fb", "a%2541"] {
        let error = resources::decode_segment(raw).expect_err("双重编码必须拒绝");
        assert_eq!(error.code(), ErrorCode::MCP_INVALID_ARGUMENT, "{raw}");
    }
    // 单次编码后恰好只有字面 % 的（`%25` → `%`）是合法的。
    assert_eq!(
        resources::decode_segment("100%25").expect("字面百分号"),
        "100%"
    );
    // 字面 % 后面不是两位十六进制，也不构成「还能再解一次」。
    assert_eq!(resources::decode_segment("a%25b").expect("a%b"), "a%b");
}

#[test]
fn parse_recognizes_the_four_resource_shapes() {
    assert_eq!(
        resources::parse("database://connections").expect("静态资源"),
        ResourceRequest::Connections
    );
    assert_eq!(
        resources::parse("database://dev/main/overview").expect("概览"),
        ResourceRequest::Overview {
            alias: "dev".to_owned(),
            database: "main".to_owned()
        }
    );
    assert_eq!(
        resources::parse("database://dev/main/schema").expect("directory"),
        ResourceRequest::Schema {
            alias: "dev".to_owned(),
            database: "main".to_owned()
        }
    );
    assert_eq!(
        resources::parse("database://dev/main/tables/users").expect("表"),
        ResourceRequest::Table {
            alias: "dev".to_owned(),
            database: "main".to_owned(),
            table: "users".to_owned()
        }
    );
}

#[test]
fn parse_decodes_each_segment_independently() {
    let cases = [
        ("database://dev/main/tables/a%2Fb", "a/b"),
        ("database://dev/main/tables/a%20b", "a b"),
        ("database://dev/main/tables/100%25", "100%"),
        ("database://dev/main/tables/%E4%B8%AD%E6%96%87", "中文"),
        ("database://dev/main/tables/中文", "中文"),
    ];
    for (uri, table) in cases {
        let request = resources::parse(uri).unwrap_or_else(|error| panic!("{uri}: {error}"));
        assert_eq!(
            request,
            ResourceRequest::Table {
                alias: "dev".to_owned(),
                database: "main".to_owned(),
                table: table.to_owned()
            },
            "{uri}"
        );
    }
}

#[test]
fn two_encodings_of_the_same_segment_are_the_same_identifier() {
    // 大写/小写十六进制只是同一个字节的两种写法：解码后必须给出同一个标识符，
    // 否则「换个大小写就换了个对象」会成为权限绕过通道。
    let upper = resources::parse("database://dev/main/tables/a%2Fb").expect("大写");
    let lower = resources::parse("database://dev/main/tables/a%2fb").expect("小写");
    assert_eq!(upper, lower);

    // 裸 `/` 不是同一个东西：它在解码前就被拆成两个路径段，段数对不上 → 拒绝。
    let error = resources::parse("database://dev/main/tables/a/b").expect_err("裸 / 不得进入表名");
    assert_eq!(error.code(), ErrorCode::MCP_INVALID_ARGUMENT);
}

#[test]
fn parse_rejects_bad_shapes_and_bad_encodings() {
    let cases = [
        ("", "空 URI"),
        ("connections", "缺 scheme"),
        ("http://connections", "错 scheme"),
        ("database://", "空路径"),
        ("database://dev//overview", "空段"),
        ("database://dev/main", "段数不足"),
        ("database://dev/main/tables", "段数不足（tables 后无表名）"),
        ("database://dev/main/unknown", "未知资源名"),
        ("database://dev/main/tables/a/b", "裸 / 拆成 5 段"),
        ("database://dev/main/tables/%zz", "非法百分号编码"),
        ("database://dev/main/tables/%252f", "双重编码"),
        ("database://dev/main/tables/%E4%B8", "非法 UTF-8"),
    ];
    for (uri, why) in cases {
        let error = resources::parse(uri).expect_err("必须拒绝");
        assert_eq!(
            error.code(),
            ErrorCode::MCP_INVALID_ARGUMENT,
            "{why}：{uri} 必须走 mcp.invalid_argument"
        );
    }
}

// ---------------------------------------------------------------------------
// 内容契约（SQLite 夹具）
// ---------------------------------------------------------------------------

/// 建库：一张表 + 一个视图。
async fn fixture(access: &AccessService) {
    exec(
        access,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
    )
    .await;
    exec(access, "CREATE VIEW v_users AS SELECT id FROM users").await;
}

#[tokio::test]
async fn connections_resource_exposes_only_the_contract_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("connections.db"), None);
    let value = payload(&access, resources::CONNECTIONS_URI).await;
    assert_keys(&value, &["connections", "count"]);
    assert_eq!(value["count"], 1, "enabled: false 的连接不得出现");
    let entry = &value["connections"][0];
    assert_keys(entry, &["alias", "driver_id", "connection_status"]);
    assert_eq!(entry["alias"], "dev");
    assert_eq!(entry["driver_id"], "sqlite");
    assert_eq!(entry["connection_status"], "not_connected");
    // 不含凭据、连接串、用户名、credential_ref 的任何影子。
    let text = value.to_string();
    for forbidden in ["password", "credential_ref", "username", "host"] {
        assert!(
            !text.contains(forbidden),
            "资源不得暴露 {forbidden}：{text}"
        );
    }
    assert_eq!(
        access.session_manager().active_count().await,
        0,
        "connections 资源不得触发数据库连接"
    );
}

#[tokio::test]
async fn overview_resource_reports_counts_and_honest_nulls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("overview.db"), None);
    fixture(&access).await;

    let value = payload(&access, "database://dev/main/overview").await;
    assert_keys(
        &value,
        &[
            "connection_alias",
            "driver_id",
            "database_name",
            "server_version",
            "schema_support",
            "table_count",
            "view_count",
            "counts_truncated",
        ],
    );
    assert_eq!(value["connection_alias"], "dev");
    assert_eq!(value["driver_id"], "sqlite");
    assert_eq!(value["database_name"], "main");
    assert!(
        value["server_version"].is_null(),
        "SPI 拿不到 server_version 时必须为 null，不得虚构"
    );
    assert_eq!(value["schema_support"]["has_schema_level"], false);
    let capabilities = value["schema_support"]["capabilities"]
        .as_array()
        .expect("能力名必须是数组");
    assert!(
        !capabilities.iter().any(|name| name == "schemas"),
        "SQLite 声明 schemas: false"
    );
    assert_eq!(value["table_count"], 1);
    assert_eq!(value["view_count"], 1);
    assert_eq!(value["counts_truncated"], false);
    // 概览不给结构明细。
    assert!(!value.to_string().contains("columns"));
}

#[tokio::test]
async fn schema_resource_lists_objects_without_column_details() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("schema.db"), None);
    fixture(&access).await;

    let value = payload(&access, "database://dev/main/schema").await;
    assert_keys(
        &value,
        &[
            "connection_alias",
            "driver_id",
            "database_name",
            "object_count",
            "by_kind",
            "objects",
            "truncated",
        ],
        // `guidance` 只在截断时出现。
    );
    assert_eq!(value["database_name"], "main");
    assert_eq!(value["object_count"], 2);
    assert_eq!(value["by_kind"]["table"], 1);
    assert_eq!(value["by_kind"]["view"], 1);
    assert_eq!(value["truncated"], false);
    let objects = value["objects"].as_array().expect("objects 必须是数组");
    for object in objects {
        assert_keys(object, &["name", "kind", "comment"]);
    }
    let kinds: Vec<&str> = objects
        .iter()
        .map(|object| object["kind"].as_str().unwrap_or_default())
        .collect();
    assert!(kinds.contains(&"table"));
    assert!(kinds.contains(&"view"));
    // 目录不给逐表字段明细。
    assert!(!value.to_string().contains("\"columns\""));
}

#[tokio::test]
async fn table_resource_reuses_describe_table_serialization() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("table.db"), None);
    fixture(&access).await;

    let value = payload(&access, "database://dev/main/tables/users").await;
    assert_keys(
        &value,
        &[
            "connection_alias",
            "database",
            "table",
            "columns",
            "column_count",
            "primary_key",
            "has_primary_key",
        ],
    );
    assert_eq!(value["table"], "users");
    assert_eq!(value["column_count"], 2);
    assert_eq!(value["has_primary_key"], true);
    let common_column_keys = [
        "name",
        "ordinal",
        "type",
        "native_type",
        "nullable",
        "default_kind",
        "default_value",
        "character_set",
        "collation",
        "generated",
    ];
    let id = &value["columns"][0];
    let mut id_keys = common_column_keys.to_vec();
    id_keys.extend(["identity", "primary_key_position"]);
    assert_keys(id, &id_keys);
    assert_eq!(id["name"], "id");
    assert_eq!(id["type"], "integer");
    assert_eq!(id["generated"], false);
    assert_eq!(id["identity"], "rowid_alias");
    assert_eq!(id["primary_key_position"], 1);

    // 普通列的 generated 已知为 false，其余可选属性应省略。
    let name = &value["columns"][1];
    assert_keys(name, &common_column_keys);
    assert_eq!(name["name"], "name");
    assert_eq!(name["generated"], false);
    assert_eq!(value["primary_key"][0]["name"], "id");
    assert_eq!(value["primary_key"][0]["position"], 1);

    // 与 describe_table 的 JSON 字段和值一致。
    let described = sqarvo_mcp::tools::schema::describe_table(
        &access,
        &sqarvo_mcp::tools::schema::DescribeTableParams {
            connection_alias: "dev".to_owned(),
            database: "main".to_owned(),
            table: "users".to_owned(),
            schema: String::new(),
        },
    )
    .await
    .expect("describe_table");
    assert_eq!(
        value["columns"],
        serde_json::to_value(&described.columns).expect("序列化列"),
        "资源与工具的列定义必须同形"
    );
    assert_eq!(
        value["primary_key"],
        serde_json::to_value(&described.primary_key).expect("序列化主键")
    );
}

#[tokio::test]
async fn the_three_resource_payloads_do_not_repeat_each_other() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("disjoint.db"), None);
    fixture(&access).await;

    let overview = keys(&payload(&access, "database://dev/main/overview").await);
    let schema = keys(&payload(&access, "database://dev/main/schema").await);
    let table = keys(&payload(&access, "database://dev/main/tables/users").await);

    // 三者共有的只有「我在说哪个对象」这一组标识字段。
    let header: BTreeSet<String> = ["connection_alias", "driver_id", "database_name", "database"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let overview_own: BTreeSet<String> = overview.difference(&header).cloned().collect();
    let schema_own: BTreeSet<String> = schema.difference(&header).cloned().collect();
    let table_own: BTreeSet<String> = table.difference(&header).cloned().collect();
    assert!(
        overview_own.is_disjoint(&schema_own),
        "概览与目录不得共享内容字段：{overview_own:?} / {schema_own:?}"
    );
    assert!(
        schema_own.is_disjoint(&table_own),
        "目录与表结构不得共享内容字段：{schema_own:?} / {table_own:?}"
    );
    assert!(
        overview_own.is_disjoint(&table_own),
        "概览与表结构不得共享内容字段：{overview_own:?} / {table_own:?}"
    );
    // 具体数据也不重复：概览没有对象名，目录没有列定义。
    assert!(!overview_own.contains("objects") && !overview_own.contains("columns"));
    assert!(!schema_own.contains("columns"));
    assert!(!table_own.contains("objects"));
}

#[tokio::test]
async fn table_resource_survives_names_that_need_encoding() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("names.db"), None);
    exec(&access, "CREATE TABLE \"a/b\" (id INTEGER PRIMARY KEY)").await;
    exec(&access, "CREATE TABLE \"空格 表\" (id INTEGER PRIMARY KEY)").await;

    let slash = resources::encode_segment("a/b");
    let value = payload(&access, &format!("database://dev/main/tables/{slash}")).await;
    assert_eq!(value["table"], "a/b");
    assert_eq!(value["column_count"], 1);

    let spaced = resources::encode_segment("空格 表");
    let value = payload(&access, &format!("database://dev/main/tables/{spaced}")).await;
    assert_eq!(value["table"], "空格 表");
}

#[tokio::test]
async fn unknown_or_disabled_aliases_never_reach_the_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("authz.db"), None);

    for uri in [
        "database://nope/main/overview",
        "database://nope/main/schema",
        "database://nope/main/tables/users",
        // alias 存在但 enabled: false：归一化成「不存在」，不泄漏配置存在性。
        "database://off/main/overview",
        "database://off/main/schema",
        "database://off/main/tables/users",
    ] {
        let error = resources::read(&access, uri)
            .await
            .expect_err("未授权 alias 必须失败");
        assert_eq!(error.code(), ErrorCode::MCP_CONNECTION_NOT_FOUND, "{uri}");
        assert_eq!(error.category(), sqarvo_common::ErrorCategory::NotFound);
        assert!(
            !error.message().contains("off"),
            "错误文案不得回显 alias：{error}"
        );
    }
    assert_eq!(
        access.session_manager().active_count().await,
        0,
        "授权失败路径不得建立连接"
    );
}

#[tokio::test]
async fn unknown_database_or_table_is_reported_not_fabricated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("missing.db"), None);
    fixture(&access).await;

    let error = resources::read(&access, "database://dev/main/tables/no_such_table")
        .await
        .expect_err("不存在的表必须失败");
    assert_ne!(error.code(), ErrorCode::MCP_INTERNAL, "必须是可诊断的分类");

    let error = resources::read(&access, "database://dev/no_such_db/schema")
        .await
        .expect_err("不存在的库必须失败");
    assert_ne!(error.code(), ErrorCode::MCP_INTERNAL);
}

#[tokio::test]
async fn oversized_catalogs_are_marked_truncated_with_tool_guidance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("big.db"), None);
    // 驱动单页上限是 500：造 510 张表，必然触发截断。
    let statements: Vec<String> = (0..510)
        .map(|index| format!("CREATE TABLE t{index:03} (id INTEGER PRIMARY KEY)"))
        .collect();
    exec(&access, &statements.join(";")).await;

    let schema = payload(&access, "database://dev/main/schema").await;
    assert_eq!(schema["truncated"], true, "超过单页上限必须标记：{schema}");
    assert_eq!(
        schema["objects"].as_array().map(Vec::len),
        Some(500),
        "只交付一页，不静默丢数据也不无限放大响应"
    );
    let guidance = schema["guidance"].as_str().expect("截断时必须给出分页指引");
    assert!(guidance.contains("list_tables"), "{guidance}");

    let overview = payload(&access, "database://dev/main/overview").await;
    assert!(
        overview["table_count"].is_null(),
        "超过单页上限时不得给出假的精确计数：{overview}"
    );
    assert_eq!(overview["counts_truncated"], true);
}

#[tokio::test]
async fn resources_are_not_gated_by_enabled_tools() {
    // `enabled_tools` 是 V3.1 §3.1 为「工具」定义的开关；资源是另一条协议面。
    // 这里把开关全关，资源仍必须可用——否则「空数组 = 禁用全部工具」会顺带
    // 关掉资源，与配置文档的文案矛盾。
    let dir = tempfile::tempdir().expect("tempdir");
    let access = access(&dir.path().join("gated.db"), Some(serde_json::json!([])));
    fixture(&access).await;

    let connections = payload(&access, resources::CONNECTIONS_URI).await;
    assert_eq!(connections["count"], 1);
    let overview = payload(&access, "database://dev/main/overview").await;
    assert_eq!(overview["table_count"], 1);
    let table = payload(&access, "database://dev/main/tables/users").await;
    assert_eq!(table["column_count"], 2);
}

// ---------------------------------------------------------------------------
// MariaDB 差异化（`SQARVO_MARIADB_URL` 缺失时跳过）
// ---------------------------------------------------------------------------

/// 测试 DSN 的解析结果。
struct Target {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
}

fn parse_dsn(url: &str) -> Target {
    let rest = url
        .strip_prefix("mysql://")
        .expect("SQARVO_MARIADB_URL 必须以 mysql:// 开头");
    let (auth, authority) = rest.split_once('@').expect("URL 缺少 @");
    let (user, password) = auth.split_once(':').expect("URL 缺少密码");
    let (endpoint, database) = authority
        .split_once('/')
        .expect("URL 缺少数据库路径段（本测试需要显式库名）");
    let (host, port) = endpoint
        .split_once(':')
        .map_or((endpoint.to_owned(), 3306), |(host, port)| {
            (host.to_owned(), port.parse().expect("端口必须是数字"))
        });
    Target {
        host,
        port,
        user: user.to_owned(),
        password: password.to_owned(),
        database: database.to_owned(),
    }
}

/// MariaDB 访问服务：凭据走内存实现，不碰系统密钥环。
fn mariadb_access(target: &Target) -> profile_access::TestAccess {
    let reference = format!("sqarvo:conn/{}", uuid::Uuid::new_v4());
    let root = serde_json::json!({
        "connections": [{
            "alias": "maria",
            "driver_id": "mariadb",
            "host": target.host,
            "port": target.port,
            "database": target.database,
            "username": target.user,
            "tls": "disabled",
            "credential_ref": reference,
            "enabled": true
        }]
    });
    let config = sqarvo_mcp::config::parse(&root.to_string())
        .expect("Parse test configuration")
        .validate()
        .expect("Validate test configuration");
    let registry = Arc::new(DriverRegistry::with_builtin_drivers());
    let credentials = Arc::new(MemoryCredentialStore::new());
    sqarvo_local_store::CredentialStore::set(
        credentials.as_ref(),
        &reference,
        &sqarvo_driver_api::Secret::new(target.password.clone()),
    )
    .expect("写入内存凭据");
    let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
    profile_access::service(config, sessions)
}

/// MariaDB 上四类资源的关键行为：计数、目录、列定义。
#[tokio::test]
async fn mariadb_resources_report_counts_catalog_and_columns() {
    let Ok(url) = std::env::var("SQARVO_MARIADB_URL") else {
        eprintln!("跳过：未设置 SQARVO_MARIADB_URL（MariaDB 差异化用例）");
        return;
    };
    let target = parse_dsn(&url);
    let access = mariadb_access(&target);
    let table = "sqarvo_mcp210_res";
    exec_on(&access, "maria", &format!("DROP TABLE IF EXISTS {table}")).await;
    exec_on(
        &access,
        "maria",
        &format!(
            "CREATE TABLE {table} (\
               id INT NOT NULL AUTO_INCREMENT, \
               email VARCHAR(64) NOT NULL, \
               score DECIMAL(10,2) NULL, \
               PRIMARY KEY (id)\
             ) ENGINE=InnoDB"
        ),
    )
    .await;

    let overview = payload(
        &access,
        &format!("database://maria/{}/overview", target.database),
    )
    .await;
    assert_eq!(overview["driver_id"], "mariadb");
    assert!(
        overview["table_count"].as_i64().unwrap_or(0) >= 1,
        "MariaDB 计数必须可靠：{overview}"
    );
    assert_eq!(
        overview["schema_support"]["has_schema_level"], false,
        "MariaDB 驱动声明 schemas: false"
    );

    let schema = payload(
        &access,
        &format!("database://maria/{}/schema", target.database),
    )
    .await;
    let names: Vec<&str> = schema["objects"]
        .as_array()
        .expect("objects")
        .iter()
        .map(|object| object["name"].as_str().unwrap_or_default())
        .collect();
    assert!(names.contains(&table), "目录必须包含刚建的表：{names:?}");

    let table_payload = payload(
        &access,
        &format!("database://maria/{}/tables/{table}", target.database),
    )
    .await;
    assert_eq!(table_payload["column_count"], 3);
    let score = table_payload["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .find(|column| column["name"] == "score")
        .expect("score 列");
    assert_eq!(score["type"], "decimal");
    assert_eq!(score["nullable"], true);
    assert_eq!(table_payload["primary_key"][0]["name"], "id");

    exec_on(&access, "maria", &format!("DROP TABLE IF EXISTS {table}")).await;
    access.close_all().await;
}
