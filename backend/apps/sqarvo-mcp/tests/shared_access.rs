//! End-to-end live revocation through the management process and a real SQLite handle.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;

use serde_json::{Value, json};
use sqarvo_mcp::config::{TRANSACTION_TOOL_NAMES, tool_names};

fn database_open(pid: u32, database: &Path) -> bool {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .expect("read child descriptors")
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .any(|target| target == database)
}

fn assert_denied(reply: &Value) {
    let result = common::assert_call_result(reply, true);
    assert_eq!(
        result["structuredContent"]["error"]["code"], "mcp.connection_denied",
        "{reply}"
    );
}

#[test]
fn external_revocation_denies_new_requests_and_closes_physical_connection() {
    let directory = tempfile::tempdir().expect("isolated XDG directory");
    let home = directory.path();
    let database = home.join("live.db");
    let config = common::write_config(
        home,
        &json!({
            "connections": [{
                "alias": "dev",
                "driver_id": "sqlite",
                "host": database,
                "port": 0,
                "enabled": true,
                "properties": {"read_only": "false"}
            }]
        }),
    );

    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    assert_denied(&common::call_tool(
        &mut server,
        2,
        "list_connections",
        json!({}),
        None,
    ));
    assert!(!database.exists(), "legacy enable must grant no access");
    assert!(config.with_file_name("mcp.json.bak").exists());

    common::enable_access(&config, home);
    let query = json!({"connection_alias":"dev","database":"","sql":"SELECT 1"});
    let reply = common::call_tool(
        &mut server,
        3,
        "execute_readonly_query",
        query.clone(),
        None,
    );
    let result = common::assert_call_result(&reply, false);
    assert_eq!(result["structuredContent"]["rows"], json!([[1]]));
    assert!(database_open(server.pid(), &database));

    let disabled = common::manage_access(&config, home, &["off"]);
    assert!(disabled.status.success(), "{disabled:?}");
    let reply = common::call_tool(
        &mut server,
        4,
        "execute_readonly_query",
        query.clone(),
        None,
    );
    assert_denied(&reply);
    assert!(
        !database_open(server.pid(), &database),
        "physical SQLite handle survived revocation"
    );

    for (index, uri) in [
        "database://connections",
        "database://dev/main/overview",
        "database://dev/main/schema",
        "database://dev/main/tables/t",
    ]
    .into_iter()
    .enumerate()
    {
        let reply = server.request(10 + index as u64, "resources/read", json!({"uri":uri}));
        assert_eq!(reply["error"]["code"], -32002, "{reply}");
        assert_eq!(
            reply["error"]["data"]["error"]["code"], "mcp.connection_denied",
            "{reply}"
        );
    }
    for (index, tool) in tool_names().enumerate() {
        // 全局关闭时，统一守卫应先于参数解析拒绝每一个数据库工具。
        assert_denied(&common::call_tool(
            &mut server,
            30 + index as u64,
            tool,
            json!({}),
            None,
        ));
    }

    let static_list = server.request(16, "tools/list", json!({}));
    assert_eq!(
        static_list["result"]["tools"]
            .as_array()
            .expect("tools")
            .len(),
        tool_names().count(),
    );
    let resource_list = server.request(17, "resources/list", json!({}));
    assert_eq!(
        resource_list["result"]["resources"][0]["uri"],
        "database://connections"
    );

    common::enable_access(&config, home);
    let reply = common::call_tool(&mut server, 18, "execute_readonly_query", query, None);
    common::assert_call_result(&reply, false);
    assert!(database_open(server.pid(), &database));

    server.close_stdin();
    let outcome = server.wait();
    assert!(outcome.status.success(), "{}", outcome.stderr);
    outcome.assert_protocol_purity();
    assert_resource_denials_audited(home);
}

#[test]
fn profiles_added_renamed_retargeted_and_deleted_over_stdio() {
    use sqarvo_local_store::{AppPaths, ConnectionProfile, ProfileStore};

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let config = common::write_config(home, &json!({}));
    common::enable_access(&config, home);
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let first = home.join("first.db");
    let second = home.join("second.db");
    for (database, sql) in [
        (&first, "CREATE TABLE items (first_column INTEGER)"),
        (&second, "CREATE TABLE items (second_column TEXT)"),
    ] {
        rusqlite::Connection::open(database)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    let paths = AppPaths::from_roots(
        home.join("config/sqarvo"),
        home.join("data/sqarvo"),
        home.join("run"),
    );
    let store = ProfileStore::new(&paths);
    let legacy = json!({"connections":[{
        "alias":"fixture", "driver_id":"sqlite", "host":first, "enabled":true
    }]});
    let validated = sqarvo_mcp::config::parse(&legacy.to_string())
        .unwrap()
        .validate()
        .unwrap();
    let params = validated
        .connection("fixture")
        .unwrap()
        .to_connection_params()
        .unwrap();
    let mut profile = store
        .save(
            ConnectionProfile {
                profile_id: uuid::Uuid::nil(),
                revision: 0,
                name: "开发库 SQLite".into(),
                params,
                file_path: String::new(),
                display_name: None,
                last_used: None,
            },
            None,
        )
        .unwrap();
    let id = profile.profile_id.to_string();
    let list = common::call_tool(&mut server, 2, "list_connections", json!({}), None);
    let entry = &common::assert_call_result(&list, false)["structuredContent"]["connections"][0];
    assert_eq!(entry["connection_id"], id);
    assert_eq!(entry["display_name"], "开发库 SQLite");
    assert_eq!(entry["alias"], id);
    assert_eq!(entry["connection_status"], "not_connected");
    assert!(!database_open(server.pid(), &first));

    let args = json!({"connection_alias":id,"database":"main","table":"items"});
    let describe = common::call_tool(&mut server, 3, "describe_table", args.clone(), None);
    assert_eq!(
        common::assert_call_result(&describe, false)["structuredContent"]["columns"][0]["name"],
        "first_column"
    );

    let revision = profile.revision;
    profile.name = "开发库 已改名".into();
    let mut profile = store.save(profile, Some(revision)).unwrap();
    let resource = server.request(4, "resources/read", json!({"uri":"database://connections"}));
    let payload: Value =
        serde_json::from_str(resource["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(payload["connections"][0]["display_name"], "开发库 已改名");
    assert_eq!(payload["connections"][0]["connection_id"], id);
    let describe = common::call_tool(&mut server, 5, "describe_table", args.clone(), None);
    common::assert_call_result(&describe, false);
    let uri = format!("database://{id}/main/tables/items");
    let table = server.request(6, "resources/read", json!({"uri":uri}));
    assert!(
        table["result"]["contents"][0]["text"].is_string(),
        "{table}"
    );

    let revision = profile.revision;
    profile.params.host = second.display().to_string();
    profile.file_path = profile.params.host.clone();
    let profile = store.save(profile, Some(revision)).unwrap();
    let describe = common::call_tool(&mut server, 7, "describe_table", args.clone(), None);
    assert_eq!(
        common::assert_call_result(&describe, false)["structuredContent"]["columns"][0]["name"],
        "second_column"
    );
    assert!(!database_open(server.pid(), &first));

    store.delete(profile.profile_id, profile.revision).unwrap();
    let describe = common::call_tool(&mut server, 8, "describe_table", args, None);
    assert_eq!(
        common::assert_call_result(&describe, true)["structuredContent"]["error"]["code"],
        "mcp.connection_not_found"
    );
    assert!(!database_open(server.pid(), &second));
}

#[test]
fn disabled_read_only_keeps_ast_policy_and_store_errors_revoke_access() {
    use std::fs::{self, OpenOptions};

    use sqarvo_local_store::file_lock::{LockMode, try_lock};

    let directory = tempfile::tempdir().expect("isolated XDG directory");
    let home = directory.path();
    let config = common::write_config(
        home,
        &json!({"connections":[{
            "alias": "dev", "driver_id": "sqlite", "host": home.join("test.db"),
            "port": 0, "enabled": true, "properties": {"read_only": "false"}
        }]}),
    );
    common::enable_access(&config, home);
    let mode = common::manage_access(&config, home, &["read-only", "off", "--yes"]);
    assert!(mode.status.success(), "{mode:?}");
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);

    let disallowed = common::call_tool(
        &mut server,
        2,
        "execute_readonly_query",
        json!({"connection_alias":"dev","database":"","sql":"CREATE TABLE t (id INTEGER)"}),
        None,
    );
    let result = common::assert_call_result(&disallowed, true);
    assert_eq!(
        result["structuredContent"]["error"]["code"],
        "mcp.readonly_violation"
    );

    let document = home.join("config/sqarvo/connections.json");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(document.with_extension("json.lock"))
        .unwrap();
    assert!(try_lock(&lock, LockMode::Exclusive).unwrap());
    assert_denied(&common::call_tool(
        &mut server,
        3,
        "list_connections",
        json!({}),
        None,
    ));
    drop(lock);

    fs::write(&document, b"{broken document").unwrap();
    assert_denied(&common::call_tool(
        &mut server,
        4,
        "list_connections",
        json!({}),
        None,
    ));
    assert_eq!(fs::read(document).unwrap(), b"{broken document");

    server.close_stdin();
    let outcome = server.wait();
    assert!(outcome.status.success(), "{}", outcome.stderr);
    outcome.assert_protocol_purity();
}

fn assert_resource_denials_audited(home: &Path) {
    let denied_resources = common::audit_files(&common::audit_dir(home))
        .iter()
        .flat_map(|path| {
            std::fs::read_to_string(path)
                .expect("read audit")
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).expect("audit JSONL"))
                .collect::<Vec<_>>()
        })
        .filter(|record| {
            record["tool_name"] == "resources/read"
                && record["error_code"] == "permission_denied"
                && record["error_subcode"] == "mcp.connection_denied"
        })
        .count();
    assert_eq!(denied_resources, 4);
}

async fn write_fixture(home: &Path) -> Value {
    use sqarvo_common::QueryId;
    use sqarvo_connection::SessionManager;
    use sqarvo_driver_api::ExecuteRequest;
    use sqarvo_drivers::DriverRegistry;
    use std::sync::Arc;

    let database = home.join("writes.db");
    let root = json!({
        "connections": [{
            "alias": "dev", "driver_id": "sqlite", "host": database, "enabled": true,
            "properties": {"read_only": "false"}
        }]
    });
    let validated = sqarvo_mcp::config::parse(&root.to_string())
        .unwrap()
        .validate()
        .unwrap();
    let manager = SessionManager::new(Arc::new(DriverRegistry::with_builtin_drivers()));
    let params = validated
        .connection("dev")
        .unwrap()
        .to_connection_params()
        .unwrap();
    let session = manager.connect("setup", params).await.unwrap();
    let handle = manager.acquire(&session.session_id).await.unwrap();
    for sql in [
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, note TEXT)",
        "INSERT INTO orders VALUES (1, 'base'), (2, 'other')",
        "CREATE TABLE candidates (id INTEGER PRIMARY KEY)",
        "WITH RECURSIVE ids(id) AS (SELECT 1 UNION ALL SELECT id + 1 FROM ids WHERE id < 22)
         INSERT INTO candidates SELECT id FROM ids",
    ] {
        handle
            .execute(ExecuteRequest {
                query_id: QueryId::new(),
                sql: sql.into(),
                max_rows: None,
                timeout: None,
            })
            .await
            .unwrap();
    }
    manager.disconnect(&session.connection_id).await.unwrap();

    root
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Verify one real protocol session from authorization through audit.
async fn stdio_writes_preview_and_audit_use_the_shared_profile_policy() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let root = write_fixture(home).await;
    let config = common::write_config(home, &root);
    common::enable_access(&config, home);
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let insert = json!({
        "connection_alias": "dev", "database": "main", "table": "orders",
        "rows": [{"id": 3, "note": "private fixture string"}]
    });
    assert_denied(&common::call_tool(
        &mut server,
        2,
        "insert_rows",
        insert.clone(),
        None,
    ));
    let mut preview = insert.clone();
    preview["operation"] = json!("insert");
    let reply = common::call_tool(&mut server, 3, "preview_write", preview.clone(), None);
    let result = common::assert_call_result(&reply, false);
    assert_eq!(result["structuredContent"]["authorized"], false);
    assert_eq!(result["structuredContent"]["can_execute"], false);
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );

    let reply = common::call_tool(&mut server, 4, "preview_write", preview, None);
    let result = common::assert_call_result(&reply, false);
    assert_eq!(
        result["structuredContent"]["summary"]["columns"],
        json!(["id", "note"])
    );
    assert!(!result.to_string().contains("private fixture string"));
    assert_eq!(result["structuredContent"]["estimated_rows"], 1);
    let reply = common::call_tool(
        &mut server,
        11,
        "preview_write",
        json!({
            "operation": "delete", "connection_alias": "dev", "database": "main",
            "table": "candidates", "conditions": [{"column": "id", "operator": "gt", "value": 0}]
        }),
        None,
    );
    let result = common::assert_call_result(&reply, false);
    assert_eq!(result["structuredContent"]["estimated_rows"], 21);
    assert_eq!(result["structuredContent"]["scope"], "lower_bound");
    assert_eq!(result["structuredContent"]["can_execute"], false);

    let readonly = json!({
        "connection_alias":"dev", "database":"", "sql":"SELECT count(*) FROM orders"
    });
    let reply = common::call_tool(
        &mut server,
        5,
        "execute_readonly_query",
        readonly.clone(),
        None,
    );
    assert_eq!(
        common::assert_call_result(&reply, false)["structuredContent"]["rows"],
        json!([[2]])
    );

    let reply = common::call_tool(&mut server, 6, "insert_rows", insert, None);
    let result = common::assert_call_result(&reply, false);
    assert_eq!(
        result["structuredContent"]["transaction_state"],
        "committed"
    );
    assert_eq!(result["structuredContent"]["affected_rows"], 1);
    let mut change = json!({
        "connection_alias":"dev", "database":"main", "table":"orders",
        "conditions":[{"column":"id", "operator":"eq", "value":3}]
    });
    change["changes"] = json!({"note":"updated confidential value"});
    let reply = common::call_tool(&mut server, 7, "update_rows", change.clone(), None);
    assert_eq!(
        common::assert_call_result(&reply, false)["structuredContent"]["affected_rows"],
        1
    );
    change.as_object_mut().unwrap().remove("changes");
    let reply = common::call_tool(&mut server, 8, "delete_rows", change.clone(), None);
    assert_eq!(
        common::assert_call_result(&reply, false)["structuredContent"]["affected_rows"],
        1
    );
    let reply = common::call_tool(&mut server, 9, "execute_readonly_query", readonly, None);
    assert_eq!(
        common::assert_call_result(&reply, false)["structuredContent"]["rows"],
        json!([[2]])
    );

    assert!(
        common::manage_access(&config, home, &["read-only", "on"])
            .status
            .success()
    );
    assert_denied(&common::call_tool(
        &mut server,
        10,
        "delete_rows",
        change,
        None,
    ));
    let records = common::read_audit_records(&common::audit_dir(home));
    assert!(records.iter().any(|(_, value)| {
        value["tool_name"] == "update_rows" && value["transaction_state"] == "committed"
    }));
    assert!(records.iter().all(|(_, value)| {
        let text = value.to_string();
        !text.contains("private fixture string") && !text.contains("updated confidential value")
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_preview_connection_failure_is_audited_as_an_error() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let root = write_fixture(home).await;
    std::fs::write(home.join("writes.db"), b"not a valid SQLite database").unwrap();
    let config = common::write_config(home, &root);
    common::enable_access(&config, home);
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let reply = common::call_tool(
        &mut server,
        2,
        "preview_write",
        json!({
            "operation":"insert","connection_alias":"dev","database":"main","table":"orders",
            "rows":[{"id":3, "note":"synthetic private value"}]
        }),
        None,
    );
    let result = common::assert_call_result(&reply, true);
    let error_code = result["structuredContent"]["error"]["code"]
        .as_str()
        .unwrap();
    assert!(!result.to_string().contains("synthetic private value"));
    let records = common::read_audit_records(&common::audit_dir(home));
    assert!(records.iter().any(|(_, record)| {
        record["tool_name"] == "preview_write"
            && record["success"] == false
            && record["error_subcode"] == error_code
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // One real stdio client drives each transaction through its terminal result.
async fn explicit_transactions_over_stdio() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let config = common::write_config(home, &write_fixture(home).await);
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let pid = server.pid();
    let database = home.join("writes.db");
    let mut id = 1;
    let mut call = |tool: &str, args: Value, error: bool| {
        id += 1;
        let response = common::call_tool(&mut server, id, tool, args, None);
        common::assert_call_result(&response, error)["structuredContent"].clone()
    };
    let begin = json!({"connection_alias": "dev", "database": "main"});
    assert_eq!(
        call("begin_transaction", begin.clone(), true)["error"]["code"],
        "mcp.connection_denied"
    );
    for tool in &TRANSACTION_TOOL_NAMES[1..] {
        assert_eq!(
            call(tool, json!({"transaction_id":"not-a-uuid"}), true)["error"]["code"],
            "mcp.connection_denied",
            "{tool}"
        );
    }
    common::enable_access(&config, home);
    assert_eq!(
        call("begin_transaction", json!({}), true)["error"]["code"],
        "mcp.invalid_argument"
    );
    for tool in &TRANSACTION_TOOL_NAMES[1..] {
        assert_eq!(
            call(tool, json!({"transaction_id":"not-a-uuid"}), true)["error"]["code"],
            "mcp.invalid_argument",
            "{tool}"
        );
    }
    assert_eq!(
        call("begin_transaction", begin.clone(), true)["error"]["code"],
        "mcp.connection_denied"
    );
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );
    assert_eq!(
        call(
            "begin_transaction",
            json!({"connection_alias":"dev","database":"other"}),
            true
        )["error"]["code"],
        "mcp.invalid_argument"
    );

    let tx = call("begin_transaction", begin.clone(), false)["transaction_id"].clone();
    let target = json!({"transaction_id": tx});
    let insert = |id| {
        json!({
            "transaction_id": tx, "operation_id": format!("insert-{id}"), "operation": "insert",
            "table": "orders", "rows": [{"id": id, "note": "explicit private fixture"}],
        })
    };
    let query = json!({
        "connection_alias":"dev", "database":"", "sql":"SELECT id FROM orders ORDER BY id"
    });
    assert_eq!(
        call("execute_in_transaction", insert(3), false)["affected_rows"],
        1
    );
    call("execute_in_transaction", insert(3), true);
    let mut foreign_database = insert(4);
    foreign_database["database"] = json!("other");
    call("execute_in_transaction", foreign_database, true);
    assert_eq!(
        call("execute_readonly_query", query.clone(), false)["rows"],
        json!([[1], [2]])
    );
    let update = json!({
        "transaction_id": tx, "operation_id": "update-3", "operation": "update",
        "table": "orders", "changes": {"id": 4},
        "conditions": [{"column":"id", "operator": "eq", "value": 3}]
    });
    assert_eq!(
        call("execute_in_transaction", update, false)["accumulated_affected_rows"],
        2
    );
    let delete = json!({
        "transaction_id": tx, "operation_id": "delete-2", "operation": "delete",
        "table": "orders", "conditions": [{"column":"id", "operator": "eq", "value": 2}]
    });
    assert_eq!(
        call("execute_in_transaction", delete, false)["accumulated_affected_rows"],
        3
    );
    assert_eq!(
        call("commit_transaction", target.clone(), false)["state"],
        "committed"
    );
    assert_eq!(
        call("commit_transaction", target, true)["state"],
        "committed"
    );
    assert_eq!(
        call("execute_readonly_query", query.clone(), false)["rows"],
        json!([[1], [4]])
    );

    let tx = call("begin_transaction", begin.clone(), false)["transaction_id"].clone();
    let insert = |id| {
        json!({
            "transaction_id": tx, "operation_id": format!("insert-{id}"), "operation": "insert",
            "table": "orders", "rows": [{"id": id, "note": "rolled back private fixture"}],
        })
    };
    call("execute_in_transaction", insert(5), false);
    call("execute_in_transaction", insert(6), false);
    let failed = call(
        "execute_in_transaction",
        json!({
            "transaction_id": tx, "operation_id": "duplicate-key", "operation": "insert",
            "table": "orders", "rows": [{"id": 1, "note": "duplicate"}],
        }),
        true,
    );
    assert_eq!(failed["state"], "failed");
    assert_eq!(
        call("commit_transaction", json!({"transaction_id": tx}), true)["state"],
        "failed"
    );
    assert_eq!(
        call("rollback_transaction", json!({"transaction_id": tx}), false)["state"],
        "rolled_back"
    );
    assert_eq!(
        call("execute_readonly_query", query.clone(), false)["rows"],
        json!([[1], [4]])
    );

    let tx = call("begin_transaction", begin.clone(), false)["transaction_id"].clone();
    assert!(
        common::manage_access(&config, home, &["read-only", "on"])
            .status
            .success()
    );
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );
    assert_eq!(
        call("commit_transaction", json!({"transaction_id": tx}), true)["state"],
        "rolled_back"
    );

    let revoked_tx = call("begin_transaction", begin.clone(), false)["transaction_id"].clone();
    call(
        "execute_in_transaction",
        json!({
            "transaction_id":revoked_tx, "operation_id":"revoked", "operation":"insert",
            "table":"orders", "rows":[{"id":8,"note":"revoked"}],
        }),
        false,
    );
    assert!(database_open(pid, &database));
    assert!(
        common::manage_access(&config, home, &["off"])
            .status
            .success()
    );
    assert_eq!(
        call(
            "commit_transaction",
            json!({"transaction_id":revoked_tx}),
            true
        )["error"]["code"],
        "mcp.connection_denied"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while database_open(pid, &database) {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("revocation closes the physical transaction connection");
    common::enable_access(&config, home);
    assert_eq!(
        call("execute_readonly_query", query.clone(), false)["rows"],
        json!([[1], [4]])
    );
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );

    let mut expires = begin.clone();
    expires["timeout_seconds"] = json!(1);
    let tx = call("begin_transaction", expires, false)["transaction_id"].clone();
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    assert_eq!(
        call("commit_transaction", json!({"transaction_id": tx}), true)["state"],
        "rolled_back"
    );

    let tx = call("begin_transaction", begin, false)["transaction_id"].clone();
    call(
        "execute_in_transaction",
        json!({
            "transaction_id": tx, "operation_id": "eof", "operation": "insert",
            "table": "orders", "rows": [{"id": 7, "note": "eof"}],
        }),
        false,
    );
    server.close_stdin();
    let outcome = server.wait();
    assert!(outcome.status.success());

    let mut observer = common::Server::spawn(&config, home);
    common::handshake(&mut observer, "2025-11-25", "2025-11-25", 1);
    let reply = common::call_tool(&mut observer, 2, "execute_readonly_query", query, None);
    assert_eq!(
        common::assert_call_result(&reply, false)["structuredContent"]["rows"],
        json!([[1], [4]])
    );

    let records = common::read_audit_records(&common::audit_dir(home));
    assert!(records.iter().any(|(_, record)| {
        record["transaction_id"] == revoked_tx
            && record["event"] == "transaction_rollback"
            && record["transaction_state"] == "rolled_back"
            && record["error_subcode"] == "mcp.connection_denied"
    }));
    for event in [
        "transaction_begin",
        "transaction_execute",
        "transaction_commit",
        "transaction_rollback",
        "transaction_expired",
        "transaction_cancelled",
    ] {
        assert!(
            records.iter().any(|(_, record)| record["event"] == event),
            "{event}"
        );
    }
    assert!(
        records
            .iter()
            .all(|(_, record)| !record.to_string().contains("private fixture"))
    );
}

#[cfg(target_os = "linux")]
#[test]
#[allow(clippy::too_many_lines)] // Verify real data and audit across all three proxied terminal cases.
fn explicit_mariadb_transactions_and_lost_acknowledgements() {
    use sqarvo_local_store::{AppPaths, ConnectionProfile, McpAccessPolicy, ProfileStore};
    use std::time::Duration;

    if std::env::var("SQARVO_MCP_V21_MARIADB_FIXTURE").as_deref() != Ok("1") {
        eprintln!(
            "requires an isolated MariaDB fixture on loopback 53387 and its terminal proxy on 53388"
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let paths = AppPaths::from_roots(
        home.join("config/sqarvo"),
        home.join("data/sqarvo"),
        home.join("run"),
    );
    let store = ProfileStore::new(&paths);
    let mut profiles = serde_json::Map::new();
    for (name, port) in [("direct", 53387), ("proxy", 53388)] {
        let profile: ConnectionProfile = serde_json::from_value(json!({
            "profile_id":uuid::Uuid::nil(), "revision":0, "name":name, "driver_id":"mariadb",
            "host":"127.0.0.1", "port":port, "database":"sqarvo_mcp_write_fixture",
            "username":"mcp_v21_fixture", "tls":"disabled",
        }))
        .unwrap();
        let saved = store.save(profile, None).unwrap();
        profiles.insert(saved.profile_id.to_string(), json!(saved.revision));
    }
    store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();
    let config = common::write_config(home, &json!({"mariadb_write_profiles":profiles}));
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let proxy_log = std::env::var_os("SQARVO_MCP_V21_MARIADB_PROXY_LOG")
        .expect("set the log path of the running terminal proxy");
    let forwarded = || {
        std::fs::read_to_string(&proxy_log)
            .unwrap()
            .matches("forwarded COMMIT")
            .count()
    };
    let mut id = 1;
    let mut call = |server: &mut common::Server, tool: &str, args: Value, error: bool| {
        id += 1;
        let response = common::call_tool(server, id, tool, args, None);
        common::assert_call_result(&response, error)["structuredContent"].clone()
    };
    for (alias, row, terminal, state) in [
        ("direct", 9891, "commit_transaction", "committed"),
        ("direct", 9892, "rollback_transaction", "rolled_back"),
        ("proxy", 9901, "commit_transaction", "commit_unknown"),
        (
            "proxy",
            9902,
            "rollback_transaction",
            "rollback_unconfirmed",
        ),
        (
            "proxy",
            9903,
            "rollback_transaction",
            "rollback_unconfirmed",
        ),
    ] {
        let open = call(
            &mut server,
            "begin_transaction",
            json!({
                "connection_alias":alias,"database":"sqarvo_mcp_write_fixture",
            }),
            false,
        );
        let tx = open["transaction_id"].clone();
        for (operation, args) in [
            ("insert", json!({"rows":[{"id":row,"note":"v21-probe"}]})),
            (
                "update",
                json!({"changes":{"note":"v21-final"},
                "conditions":[{"column":"id","operator":"eq","value":row}]}),
            ),
        ] {
            let mut args = args.as_object().unwrap().clone();
            args.extend([
                ("transaction_id".into(), tx.clone()),
                ("operation_id".into(), json!(operation)),
                ("operation".into(), json!(operation)),
                ("table".into(), json!("orders")),
            ]);
            assert_eq!(
                call(
                    &mut server,
                    "execute_in_transaction",
                    Value::Object(args),
                    false
                )["affected_rows"],
                1
            );
        }
        let args = json!({"transaction_id":tx});
        let ended = if row == 9901 {
            let previous = forwarded();
            server.send(&json!({
                "jsonrpc":"2.0","id":700,"method":"tools/call",
                "params":{"name":terminal,"arguments":args},
            }));
            let until = std::time::Instant::now() + Duration::from_secs(2);
            while forwarded() == previous {
                assert!(
                    std::time::Instant::now() < until,
                    "proxy must receive COMMIT"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let revoke = store
                .set_mcp_access(McpAccessPolicy {
                    enabled: true,
                    read_only: true,
                })
                .unwrap_err();
            assert_eq!(revoke.code(), sqarvo_common::ErrorCode::PROFILE_LOCKED);
            let reply = server.next_message();
            assert_eq!(reply["id"], 700);
            common::assert_call_result(&reply, true)["structuredContent"].clone()
        } else {
            call(&mut server, terminal, args.clone(), alias == "proxy")
        };
        assert_eq!(ended["state"], state);
        let repeat = call(&mut server, terminal, args, true);
        assert_eq!(repeat["state"], state);
        assert_eq!(repeat["error"]["retryable"], false);
    }
    let state = call(
        &mut server,
        "execute_readonly_query",
        json!({
            "connection_alias":"direct","database":"sqarvo_mcp_write_fixture",
            "sql":"SELECT id, note FROM orders WHERE id BETWEEN 9891 AND 9903 ORDER BY id"
        }),
        false,
    );
    assert_eq!(
        state["rows"],
        json!([[9891, "v21-final"], [9901, "v21-final"]])
    );
    server.close_stdin();
    let outcome = server.wait();
    assert!(outcome.status.success(), "{}", outcome.stderr);
    let records = common::read_audit_records(&common::audit_dir(home));
    assert_eq!(
        records
            .iter()
            .filter(|(_, value)| value["event"] == "transaction_commit_unknown")
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|(_, value)| value["event"] == "transaction_rollback_unconfirmed")
            .count(),
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Exercise limits and observable cleanup using one real SQLite fixture.
async fn explicit_sqlite_limits_busy_and_audit_failure() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let config = common::write_config(home, &write_fixture(home).await);
    common::enable_access(&config, home);
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );
    let mut primary = common::Server::spawn(&config, home);
    let mut observer = common::Server::spawn(&config, home);
    common::handshake(&mut primary, "2025-11-25", "2025-11-25", 1);
    common::handshake(&mut observer, "2025-11-25", "2025-11-25", 1);
    let mut id = 1;
    let mut call = |tool: &str, args: Value, error: bool| {
        id += 1;
        let response = common::call_tool(&mut primary, id, tool, args, None);
        common::assert_call_result(&response, error)["structuredContent"].clone()
    };
    let begin = json!({"connection_alias":"dev","database":"main"});
    let tx = call("begin_transaction", begin.clone(), false)["transaction_id"].clone();
    let busy = common::call_tool(&mut observer, 2, "begin_transaction", begin.clone(), None);
    assert_eq!(
        common::assert_call_result(&busy, true)["structuredContent"]["error"]["code"],
        "sqlite.busy"
    );
    for (start, end, rejected) in [(10, 110, false), (110, 210, false), (210, 211, true)] {
        let rows = (start..end)
            .map(|id| json!({"id":id,"note":"aggregate"}))
            .collect::<Vec<_>>();
        let response = call(
            "execute_in_transaction",
            json!({
                "transaction_id":tx,"operation_id":format!("batch-{start}"),"operation":"insert","table":"orders","rows":rows
            }),
            rejected,
        );
        assert_eq!(
            response["state"],
            if rejected { "rolled_back" } else { "open" }
        );
    }
    assert_eq!(
        call("commit_transaction", json!({"transaction_id":tx}), true)["state"],
        "rolled_back"
    );

    let tx = call("begin_transaction", begin.clone(), false)["transaction_id"].clone();
    for row in 10..=30 {
        let rejected = row == 30;
        let response = call(
            "execute_in_transaction",
            json!({
                "transaction_id":tx,"operation_id":format!("one-{row}"),"operation":"insert","table":"orders",
                "rows":[{"id":row, "note":"operation quota"}]
            }),
            rejected,
        );
        assert_eq!(response["state"], "open");
        if rejected {
            assert_eq!(response["error"]["category"], "resource_exhausted");
        }
    }
    assert_eq!(
        call("rollback_transaction", json!({"transaction_id":tx}), false)["state"],
        "rolled_back"
    );

    let tx = call("begin_transaction", begin, false)["transaction_id"].clone();
    call(
        "execute_in_transaction",
        json!({
            "transaction_id":tx,"operation_id":"audit-blocked","operation":"insert","table":"orders","rows":[{"id":31}]
        }),
        false,
    );
    let lock = common::audit_dir(home).join("audit.lock");
    let saved = lock.with_extension("saved");
    std::fs::rename(&lock, &saved).unwrap();
    std::fs::create_dir(&lock).unwrap();
    let response = call("commit_transaction", json!({"transaction_id":tx}), true);
    assert_eq!(response["state"], "rolled_back");
    assert_eq!(response["audit_warning"], true);
    std::fs::remove_dir(&lock).unwrap();
    std::fs::rename(&saved, &lock).unwrap();

    let read = common::call_tool(
        &mut observer,
        3,
        "execute_readonly_query",
        json!({
            "connection_alias":"dev","database":"","sql":"SELECT id FROM orders ORDER BY id"
        }),
        None,
    );
    assert_eq!(
        common::assert_call_result(&read, false)["structuredContent"]["rows"],
        json!([[1], [2]])
    );
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Validate expiry, edit, and deletion against the same persisted profile.
async fn explicit_transaction_profile_edit_and_idle_expiry() {
    use sqarvo_local_store::{AppPaths, ProfileStore};
    use std::time::Duration;

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let config = common::write_config(home, &write_fixture(home).await);
    common::enable_access(&config, home);
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );
    let paths = AppPaths::from_roots(
        home.join("config/sqarvo"),
        home.join("data/sqarvo"),
        home.join("run"),
    );
    let store = ProfileStore::new(&paths);
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let begin = json!({"connection_alias":"dev","database":"main"});
    let opened = common::call_tool(&mut server, 2, "begin_transaction", begin.clone(), None);
    let tx =
        common::assert_call_result(&opened, false)["structuredContent"]["transaction_id"].clone();
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            if common::read_audit_records(&common::audit_dir(home))
                .iter()
                .any(|(_, record)| {
                    record["transaction_id"] == tx && record["event"] == "transaction_expired"
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("idle expiry is audited without another request");
    let ended = common::call_tool(
        &mut server,
        3,
        "commit_transaction",
        json!({"transaction_id":tx}),
        None,
    );
    assert_eq!(
        common::assert_call_result(&ended, true)["structuredContent"]["state"],
        "rolled_back"
    );

    let tx = {
        let mut id = 4;
        loop {
            let opened =
                common::call_tool(&mut server, id, "begin_transaction", begin.clone(), None);
            if opened["result"]["structuredContent"]["state"] == "open" {
                break opened["result"]["structuredContent"]["transaction_id"].clone();
            }
            assert!(id < 10, "expired transaction must free its reservation");
            assert_eq!(
                opened["result"]["structuredContent"]["error"]["code"],
                "transaction.already_active"
            );
            id += 1;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    };
    let mut profile = store.snapshot().unwrap().resolve("dev").unwrap().clone();
    let revision = profile.revision;
    profile.name = "renamed".into();
    store.save(profile, Some(revision)).unwrap();
    let write = common::call_tool(
        &mut server,
        11,
        "execute_in_transaction",
        json!({
            "transaction_id":tx,"operation_id":"after-profile-edit","operation":"insert","table":"orders",
            "rows":[{"id":3}]
        }),
        None,
    );
    assert_eq!(
        common::assert_call_result(&write, true)["structuredContent"]["state"],
        "rolled_back"
    );
    server.close_stdin();
    assert!(server.wait().status.success());
    let mut restarted = common::Server::spawn(&config, home);
    common::handshake(&mut restarted, "2025-11-25", "2025-11-25", 1);
    let opened = common::call_tool(
        &mut restarted,
        2,
        "begin_transaction",
        json!({
            "connection_alias":"renamed","database":"main"
        }),
        None,
    );
    let tx =
        common::assert_call_result(&opened, false)["structuredContent"]["transaction_id"].clone();
    let profile = store
        .snapshot()
        .unwrap()
        .resolve("renamed")
        .unwrap()
        .clone();
    store.delete(profile.profile_id, profile.revision).unwrap();
    let reply = common::call_tool(
        &mut restarted,
        3,
        "commit_transaction",
        json!({"transaction_id":tx}),
        None,
    );
    assert_eq!(
        common::assert_call_result(&reply, true)["structuredContent"]["state"],
        "rolled_back"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_explicit_begin_releases_its_reservation() {
    use std::time::Duration;
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let config = common::write_config(home, &write_fixture(home).await);
    common::enable_access(&config, home);
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );
    let mut holder = common::Server::spawn(&config, home);
    let mut client = common::Server::spawn(&config, home);
    for server in [&mut holder, &mut client] {
        common::handshake(server, "2025-11-25", "2025-11-25", 1);
    }
    let begin = json!({"connection_alias":"dev","database":"main"});
    let open = common::call_tool(&mut holder, 2, "begin_transaction", begin.clone(), None);
    let holder_id =
        common::assert_call_result(&open, false)["structuredContent"]["transaction_id"].clone();
    client.send(&json!({
        "jsonrpc":"2.0","id":71,"method":"tools/call",
        "params":{"name":"begin_transaction","arguments":begin},
    }));
    let audit = common::audit_dir(home);
    let cancelled_tx = async {
        loop {
            if let Some((_, record)) =
                common::read_audit_records(&audit)
                    .into_iter()
                    .find(|(_, record)| {
                        record["event"] == "transaction_begin"
                            && record["phase"] == "intent"
                            && record["transaction_id"] != holder_id
                    })
            {
                break record["transaction_id"].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let cancelled_tx = tokio::time::timeout(Duration::from_secs(3), cancelled_tx)
        .await
        .unwrap();
    client.notify(
        "notifications/cancelled",
        json!({"requestId":71,"reason":"test cancelled BEGIN"}),
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if common::read_audit_records(&audit)
                .iter()
                .any(|(_, record)| {
                    record["transaction_id"] == cancelled_tx
                        && record["transaction_state"] == "not_started"
                        && record["error_subcode"] == "query.cancelled"
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled BEGIN is audited even though rmcp discards its reply");

    let end = common::call_tool(
        &mut holder,
        3,
        "rollback_transaction",
        json!({"transaction_id":holder_id}),
        None,
    );
    assert_eq!(
        common::assert_call_result(&end, false)["structuredContent"]["state"],
        "rolled_back"
    );
    let recovered = async {
        for id in 72..112 {
            let reply =
                common::call_tool(&mut client, id, "begin_transaction", begin.clone(), None);
            let tool_result = &reply["result"];
            if tool_result["structuredContent"]["state"] == "open" {
                return;
            }
            assert_eq!(
                tool_result["structuredContent"]["error"]["code"],
                "transaction.already_active"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("cancelled BEGIN did not release its reservation");
    };
    recovered.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Hold SQLite's reader lock until the durable commit intent is visible.
async fn explicit_commit_survives_terminal_audit_failure() {
    use sqarvo_common::QueryId;
    use sqarvo_connection::SessionManager;
    use sqarvo_driver_api::{ExecuteRequest, IsolationLevel, TransactionOptions};
    use sqarvo_drivers::DriverRegistry;
    use std::sync::Arc;
    use std::time::Duration;

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let root = write_fixture(home).await;
    let params = sqarvo_mcp::config::parse(&root.to_string())
        .unwrap()
        .validate()
        .unwrap()
        .connection("dev")
        .unwrap()
        .to_connection_params()
        .unwrap();
    let config = common::write_config(home, &root);
    common::enable_access(&config, home);
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let begin = common::call_tool(
        &mut server,
        2,
        "begin_transaction",
        json!({
            "connection_alias":"dev","database":"main"
        }),
        None,
    );
    let tx =
        common::assert_call_result(&begin, false)["structuredContent"]["transaction_id"].clone();
    let inserted = common::call_tool(
        &mut server,
        3,
        "execute_in_transaction",
        json!({
            "transaction_id":tx,"operation_id":"audit_failure","operation":"insert","table":"orders","rows":[{"id":3}]
        }),
        None,
    );
    common::assert_call_result(&inserted, false);

    let manager = SessionManager::new(Arc::new(DriverRegistry::with_builtin_drivers()));
    let session = manager.connect("fixture reader", params).await.unwrap();
    let reader = manager.acquire(&session.session_id).await.unwrap();
    let read = reader
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: true,
        })
        .await
        .unwrap();
    let result = reader
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: "SELECT count(*) FROM orders".into(),
            max_rows: None,
            timeout: None,
        })
        .await
        .unwrap();
    assert!(result.first_batch.is_some());
    server.send(&json!({
        "jsonrpc":"2.0","id":4,"method":"tools/call",
        "params":{"name":"commit_transaction","arguments":{"transaction_id":tx}},
    }));
    let audit = common::audit_dir(home);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if common::read_audit_records(&audit)
                .iter()
                .any(|(_, record)| {
                    record["event"] == "transaction_commit" && record["phase"] == "intent"
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("commit intention is written while SQLite waits on the reader");

    let lock = audit.join("audit.lock");
    let saved = lock.with_extension("saved");
    std::fs::rename(&lock, &saved).unwrap();
    std::fs::create_dir(&lock).unwrap();
    reader
        .rollback_transaction(&read.transaction_id)
        .await
        .unwrap();
    let reply = server.next_message();
    assert_eq!(reply["id"], 4);
    let committed = common::assert_call_result(&reply, false);
    assert_eq!(committed["structuredContent"]["state"], "committed");
    assert_eq!(committed["structuredContent"]["audit_warning"], true);
    std::fs::remove_dir(&lock).unwrap();
    std::fs::rename(&saved, &lock).unwrap();
    manager.disconnect(&session.connection_id).await.unwrap();

    let readback = common::call_tool(
        &mut server,
        5,
        "execute_readonly_query",
        json!({
            "connection_alias":"dev","database":"","sql":"SELECT id FROM orders ORDER BY id"
        }),
        None,
    );
    assert_eq!(
        common::assert_call_result(&readback, false)["structuredContent"]["rows"],
        json!([[1], [2], [3]])
    );
}

#[cfg(target_os = "linux")]
fn ddl_mariadb_profiles(home: &Path, profiles: &[(&str, &str, u16)]) {
    use sqarvo_local_store::{AppPaths, ConnectionProfile, McpAccessPolicy, ProfileStore};

    let store = ProfileStore::new(&AppPaths::from_roots(
        home.join("config/sqarvo"),
        home.join("data/sqarvo"),
        home.join("run"),
    ));
    for &(alias, username, port) in profiles {
        let profile: ConnectionProfile = serde_json::from_value(json!({
            "profile_id":uuid::Uuid::nil(), "revision":0, "name":alias, "driver_id":"mariadb",
            "host":"127.0.0.1", "port":port, "database":"sqarvo_mcp_ddl_fixture",
            "username":username, "tls":"disabled",
        }))
        .unwrap();
        store.save(profile, None).unwrap();
    }
    store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn ddl_stdio_mariadb_managed_schema() {
    if std::env::var("SQARVO_MCP_DDL_MARIADB_FIXTURE").as_deref() != Ok("1") {
        eprintln!("requires disposable MariaDB 11.8.6 on loopback 53429 and a test keyring");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    ddl_mariadb_profiles(
        home,
        &[
            ("ddl", "mcp_ddl_fixture", 53429),
            ("read", "mcp_ddl_read_fixture", 53429),
        ],
    );
    let config = common::write_config(home, &json!({}));
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let mut sequence = 1;
    let mut rpc = |tool: &str, args: Value, error: bool| {
        sequence += 1;
        let response = common::call_tool(&mut server, sequence, tool, args, None);
        common::assert_call_result(&response, error)["structuredContent"].clone()
    };
    let arguments = |extra: Value| {
        let mut root = json!({
            "connection_alias":"ddl", "database":"sqarvo_mcp_ddl_fixture", "table":"mcp_items",
        });
        root.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        root
    };
    let mut create = arguments(json!({
        "columns":[{"name":"id","type":"bigint","primary_key":true,"identity":true},
                   {"name":"note","type":"text","default":"private ' \\ ; default"}]
    }));
    let preview = rpc(
        "preview_schema_change",
        json!({"operation":"create_table","arguments":create}),
        false,
    );
    assert_eq!(preview["can_execute"], true, "{preview}");
    assert_eq!(preview["implicit_commit"], true);
    assert!(
        !preview["generated_sql"]
            .as_str()
            .unwrap()
            .contains("private")
    );
    create["operation_id"] = json!(common::ddl_operation_id());
    let mut denied = create.clone();
    denied["connection_alias"] = json!("read");
    denied["operation_id"] = json!(common::ddl_operation_id());
    assert_eq!(rpc("create_table", denied, true)["state"], "failed");
    let created = rpc("create_table", create.clone(), false);
    assert_eq!(created["effect"], "applied", "{created}");
    let replay = rpc("create_table", create, false);
    assert_eq!(replay["replayed"], true);
    let steps = [
        (
            "alter_table",
            json!({"action":"add_column","column":{"name":"extra","type":"integer"}}),
        ),
        (
            "alter_table",
            json!({"action":"rename_column","column_name":"extra","new_name":"renamed_col"}),
        ),
        (
            "create_index",
            json!({"index":"named_idx","columns":["renamed_col","id"]}),
        ),
        ("drop_index", json!({"index":"named_idx"})),
        (
            "alter_table",
            json!({"action":"drop_column","column_name":"renamed_col"}),
        ),
        (
            "alter_table",
            json!({"action":"rename_table","new_table":"mcp_renamed"}),
        ),
        ("drop_table", json!({"table":"mcp_renamed"})),
    ];
    for (tool, params) in steps {
        let mut args = arguments(params);
        args["operation_id"] = json!(common::ddl_operation_id());
        let result = rpc(tool, args, false);
        assert_eq!(result["effect"], "applied", "{tool}: {result}");
    }
    let found = rpc(
        "execute_readonly_query",
        json!({
            "connection_alias":"ddl", "database":"sqarvo_mcp_ddl_fixture",
            "sql":"SELECT COUNT(*) AS present FROM information_schema.TABLES WHERE TABLE_SCHEMA='sqarvo_mcp_ddl_fixture' AND TABLE_NAME IN ('mcp_items','mcp_renamed')"
        }),
        false,
    );
    assert_eq!(found["rows"][0][0], 0, "{found}");
    server.close_stdin();
    assert!(server.wait().status.success());
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ddl_stdio_sqlite_on_production_keyring() {
    if std::env::var("SQARVO_MCP_DDL_KEYRING_FIXTURE").as_deref() != Ok("1") {
        eprintln!("requires an isolated Secret Service session");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let config = common::write_config(home, &write_fixture(home).await);
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let args = json!({
        "operation_id":common::ddl_operation_id(), "connection_alias":"dev",
        "database":"main","table":"operators","columns":[{"name":"id","type":"integer"}],
    });
    let call = |server: &mut common::Server, id, error| {
        let result = common::call_tool(server, id, "create_table", args.clone(), None);
        common::assert_call_result(&result, error)["structuredContent"].clone()
    };
    assert_eq!(
        call(&mut server, 2, true)["error"]["code"],
        "mcp.connection_denied"
    );
    common::enable_access(&config, home);
    assert_eq!(
        call(&mut server, 3, true)["error"]["code"],
        "mcp.connection_denied"
    );
    assert!(
        common::manage_access(&config, home, &["read-only", "off", "--yes"])
            .status
            .success()
    );
    let created = call(&mut server, 4, false);
    assert_eq!(created["state"], "completed", "{created}");
    server.close_stdin();
    assert!(server.wait().status.success());
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    assert_eq!(call(&mut server, 2, false)["replayed"], true);
    server.close_stdin();
    assert!(server.wait().status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn ddl_stdio_mariadb_unknown_and_reconciliation() {
    if std::env::var("SQARVO_MCP_DDL_PROXY_FIXTURE").as_deref() != Ok("1") {
        eprintln!("requires MariaDB 11.8.6 and DDL acknowledgment proxy on 53429/53431");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    ddl_mariadb_profiles(
        home,
        &[
            ("direct", "mcp_ddl_fixture", 53429),
            ("proxy", "mcp_ddl_fixture", 53431),
        ],
    );
    let config = common::write_config(home, &json!({}));
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let operation_id = common::ddl_operation_id();
    let args = json!({
        "connection_alias":"proxy", "database":"sqarvo_mcp_ddl_fixture", "table":"ddl_ack_unknown",
        "operation_id":operation_id, "columns":[{"name":"id","type":"integer"}],
    });
    let call = |server: &mut common::Server, id, tool, args, error| {
        let result = common::call_tool(server, id, tool, args, None);
        common::assert_call_result(&result, error)["structuredContent"].clone()
    };
    let result = call(&mut server, 2, "create_table", args.clone(), true);
    assert_eq!(result["state"], "outcome_unknown", "{result}");
    assert_eq!(result["operation_id"], operation_id);
    assert_eq!(result["error"]["retryable"], false);
    let read = call(
        &mut server,
        3,
        "execute_readonly_query",
        json!({
           "connection_alias":"direct", "database":"sqarvo_mcp_ddl_fixture",
           "sql":"SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA='sqarvo_mcp_ddl_fixture' AND TABLE_NAME='ddl_ack_unknown'"
        }),
        false,
    );
    assert_eq!(read["rows"][0][0], 1, "{read}");
    server.close_stdin();
    assert!(server.wait().status.success());
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let replay = call(&mut server, 2, "create_table", args, true);
    assert_eq!(replay["replayed"], true, "{replay}");
    assert_eq!(replay["state"], "outcome_unknown");
    let drop = json!({
        "connection_alias":"direct", "database":"sqarvo_mcp_ddl_fixture",
        "table":"ddl_ack_unknown", "operation_id":common::ddl_operation_id(),
    });
    let blocked = call(&mut server, 3, "drop_table", drop.clone(), true);
    assert_eq!(blocked["error"]["code"], "mcp.ddl_unresolved", "{blocked}");
    let reconciled = std::process::Command::new(common::binary())
        .args(["ddl-reconcile", &operation_id, "--verified-stopped"])
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .output()
        .unwrap();
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    let result = call(&mut server, 4, "drop_table", drop, false);
    assert_eq!(result["effect"], "applied", "{result}");
    server.close_stdin();
    assert!(server.wait().status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn ddl_stdio_mariadb_metadata_lock_is_bounded() {
    use sqarvo_common::QueryId;
    use sqarvo_connection::SessionManager;
    use sqarvo_driver_api::{ExecuteRequest, IsolationLevel, TransactionOptions};
    use sqarvo_drivers::DriverRegistry;
    use std::sync::Arc;

    if std::env::var("SQARVO_MCP_DDL_MARIADB_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    ddl_mariadb_profiles(home, &[("ddl", "mcp_ddl_fixture", 53429)]);
    let config = common::write_config(home, &json!({}));
    let params = sqarvo_mcp::config::parse(&json!({
        "connections":[{"alias":"ddl","enabled":true,"driver_id":"mariadb","host":"127.0.0.1",
            "port":53429,"database":"sqarvo_mcp_ddl_fixture","username":"mcp_ddl_fixture","tls":"disabled"}]
    }).to_string()).unwrap().validate().unwrap().connection("ddl").unwrap()
        .to_connection_params().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (handle, transaction) = runtime.block_on(async {
        let manager = SessionManager::new(Arc::new(DriverRegistry::with_builtin_drivers()));
        let handle = manager
            .connect_isolated("hold metadata lock", params)
            .await
            .unwrap();
        let active = handle
            .begin_transaction(TransactionOptions {
                isolation: IsolationLevel::Serializable,
                read_only: true,
            })
            .await
            .unwrap();
        handle
            .execute(ExecuteRequest {
                query_id: QueryId::new(),
                sql: "SELECT id FROM locked_table".into(),
                max_rows: Some(2),
                timeout: None,
            })
            .await
            .unwrap();
        (handle, active.transaction_id)
    });
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let args = json!({
        "connection_alias":"ddl","database":"sqarvo_mcp_ddl_fixture","table":"locked_table",
        "operation_id":common::ddl_operation_id(),"action":"add_column",
        "column":{"name":"locked_probe","type":"integer"},
    });
    let response = common::call_tool(&mut server, 2, "alter_table", args, None);
    let response = &common::assert_call_result(&response, true)["structuredContent"];
    assert_eq!(response["state"], "failed", "{response}");
    assert_eq!(response["error"]["retryable"], false, "{response}");
    runtime.block_on(async {
        handle.rollback_transaction(&transaction).await.unwrap();
        handle.close().await.unwrap();
    });
    let response = common::call_tool(
        &mut server,
        3,
        "execute_readonly_query",
        json!({
            "connection_alias":"ddl","database":"sqarvo_mcp_ddl_fixture",
            "sql":"SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='sqarvo_mcp_ddl_fixture' AND TABLE_NAME='locked_table' AND COLUMN_NAME='locked_probe'"
        }),
        None,
    );
    let response = &common::assert_call_result(&response, false)["structuredContent"];
    assert_eq!(response["rows"][0][0], 0, "{response}");
    server.close_stdin();
    assert!(server.wait().status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn ddl_mariadb_hidden_dependency_refuses_mutation() {
    if std::env::var("SQARVO_MCP_DDL_GUARD_REJECTION_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    ddl_mariadb_profiles(home, &[("ddl", "mcp_ddl_fixture", 53429)]);
    let config = common::write_config(home, &json!({}));
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let query = |sql| {
        json!({
            "connection_alias":"ddl","database":"sqarvo_mcp_ddl_fixture","sql":sql,
        })
    };
    let read = common::call_tool(
        &mut server,
        2,
        "execute_readonly_query",
        query(
            "SELECT COUNT(*) FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA='sqarvo_mcp_ddl_other' AND REFERENCED_TABLE_NAME='locked_table'",
        ),
        None,
    );
    assert_eq!(
        common::assert_call_result(&read, false)["structuredContent"]["rows"][0][0],
        0
    );
    let response = common::call_tool(
        &mut server,
        3,
        "drop_table",
        json!({
            "connection_alias":"ddl", "database":"sqarvo_mcp_ddl_fixture", "table":"locked_table",
            "operation_id":common::ddl_operation_id(),
        }),
        None,
    );
    let response = &common::assert_call_result(&response, true)["structuredContent"];
    assert_eq!(
        response["error"]["code"], "capability.unsupported",
        "{response}"
    );
    assert_eq!(response["state"], "not_started");
    let read = common::call_tool(
        &mut server,
        4,
        "execute_readonly_query",
        query("SELECT id FROM locked_table"),
        None,
    );
    assert_eq!(
        common::assert_call_result(&read, false)["structuredContent"]["rows"][0][0],
        1
    );
    server.close_stdin();
    assert!(server.wait().status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn ddl_stdio_mariadb_request_cancel_keeps_unknown_outcome() {
    use std::time::{Duration, Instant};

    if std::env::var("SQARVO_MCP_DDL_PROXY_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let proxy_log = std::env::var_os("SQARVO_MCP_DDL_PROXY_LOG").expect("set DDL fault proxy log");
    let observed = || {
        std::fs::read_to_string(&proxy_log)
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == "withheld DDL_CANCEL server OK")
            .count()
    };
    let baseline = observed();
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    ddl_mariadb_profiles(home, &[("proxy", "mcp_ddl_fixture", 53431)]);
    let config = common::write_config(home, &json!({}));
    let mut server = common::Server::spawn(&config, home);
    common::handshake(&mut server, "2025-11-25", "2025-11-25", 1);
    let id = common::ddl_operation_id();
    server.send(&json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":"create_table","arguments":{
            "connection_alias":"proxy","database":"sqarvo_mcp_ddl_fixture","table":"ddl_cancel_inflight",
            "operation_id":id,"columns":[{"name":"id","type":"integer"}]
        }},
    }));
    let wait_for = |ready: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "DDL fixture checkpoint timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait_for(&|| observed() > baseline);
    server.notify(
        "notifications/cancelled",
        json!({"requestId":2,"reason":"fixture cancel"}),
    );
    wait_for(&|| {
        common::read_audit_records(&common::audit_dir(home))
            .iter()
            .any(|(_, record)| {
                record["operation_id"] == id && record["ddl_state"] == "outcome_unknown"
            })
    });
    server.close_stdin();
    assert!(server.wait().status.success());
}
