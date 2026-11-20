//! End-to-end live revocation through the management process and a real SQLite handle.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;

use serde_json::{Value, json};
use sqarvo_mcp::config::tool_names;

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
