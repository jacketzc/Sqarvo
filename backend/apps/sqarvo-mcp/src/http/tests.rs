use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use sqarvo_common::QueryId;
use sqarvo_driver_api::ExecuteRequest;
use sqarvo_local_store::{McpAccessPolicy, MemoryCredentialStore, ProfileStore};
use tokio::io::AsyncWriteExt;

use super::*;

struct HttpClient {
    addr: SocketAddr,
    credential: Secret,
    next_request: AtomicU64,
}

impl HttpClient {
    async fn request(&self, method: &str, session: Option<&str>, body: Value) -> String {
        let body = if method == "DELETE" {
            String::new()
        } else {
            body.to_string()
        };
        let mut stream = tokio::net::TcpStream::connect(self.addr).await.unwrap();
        let session = session.map_or_else(String::new, |id| {
            format!("Mcp-Session-Id: {id}\r\nMCP-Protocol-Version: 2025-11-25\r\n")
        });
        let message = format!(
            "{method} /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n{session}Accept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            self.credential.expose(),
            body.len()
        );
        stream.write_all(message.as_bytes()).await.unwrap();
        let mut reply = String::new();
        tokio::time::timeout(Duration::from_secs(15), stream.read_to_string(&mut reply))
            .await
            .unwrap()
            .unwrap();
        reply
    }

    async fn initialize(&self) -> String {
        let response = self.request("POST", None, json!({"jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}
        })).await;
        let id = response
            .lines()
            .find_map(|line| {
                line.split_once(": ")
                    .filter(|(key, _)| key.eq_ignore_ascii_case("mcp-session-id"))
                    .map(|(_, value)| value.to_owned())
            })
            .expect("initialize returns an MCP Session");
        let reply = self
            .request(
                "POST",
                Some(&id),
                json!({
                    "jsonrpc":"2.0", "method":"notifications/initialized"
                }),
            )
            .await;
        assert!(reply.starts_with("HTTP/1.1 202"), "{reply}");
        id
    }

    async fn rpc(&self, session: &str, method: &str, params: Value) -> Value {
        let response = self
            .request(
                "POST",
                Some(session),
                json!({
                    "jsonrpc":"2.0","id":self.next_request.fetch_add(1, Ordering::Relaxed),"method":method,"params":params
                }),
            )
            .await;
        let data = response
            .lines()
            .find_map(|line| {
                line.strip_prefix("data: ")
                    .filter(|data| data.starts_with('{'))
            })
            .unwrap_or_else(|| panic!("expected SSE response: {response}"));
        serde_json::from_str::<Value>(data).unwrap()["result"].clone()
    }

    async fn tool(&self, session: &str, name: &str, arguments: Value) -> Value {
        self.rpc(
            session,
            "tools/call",
            json!({"name":name,"arguments":arguments}),
        )
        .await
    }
}

async fn start_http(
    context: Arc<ServerContext>,
) -> (
    HttpClient,
    CancellationToken,
    Arc<LocalSessionManager>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = addr.to_string();
    let origin = format!("http://{authority}");
    let client = HttpClient {
        addr,
        credential: new_token().unwrap(),
        next_request: AtomicU64::new(2),
    };
    let secret = Arc::new(MemoryCredentialStore::new());
    secret.set(AUTH_REFERENCE, &client.credential).unwrap();
    let shutdown = CancellationToken::new();
    let sessions = Arc::new(LocalSessionManager::default());
    let writes = Arc::new(WriteSessions::new(Some(
        Sha256::digest(client.credential.expose().as_bytes()).into(),
    )));
    let config = StreamableHttpServerConfig::default()
        .with_cancellation_token(shutdown.clone())
        .with_allowed_hosts([authority.clone()])
        .with_allowed_origins([origin.clone()]);
    let router = router(
        SqarvoMcpServer::new_http(context),
        config,
        HttpAuth {
            authority,
            origin,
            store: secret,
            active: Arc::new(Semaphore::new(16)),
            initializing: Arc::new(Semaphore::new(1)),
            sessions: Arc::clone(&sessions),
            writes,
            shutdown: shutdown.clone(),
        },
    );
    let cancellation = shutdown.clone();
    let service = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(cancellation.cancelled_owned())
            .await
            .unwrap();
    });
    (client, shutdown, sessions, service)
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Drive the production router and session middleware over loopback.
async fn authenticated_http_transaction_isolation_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let config = base.join("fixture.json");
    std::fs::write(
        &config,
        json!({"connections":[{
            "alias":"fixture","driver_id":"sqlite","host":base.join("fixture.db"),
            "enabled":true,"properties":{"read_only":"false"},
        }]})
        .to_string(),
    )
    .unwrap();
    let paths = AppPaths::from_roots(base.join("config"), base.to_owned(), base.join("run"));
    ProfileStore::new(&paths)
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();
    let context = Arc::new(
        ServerContext::bootstrap_with(Some(&config), base)
            .await
            .unwrap(),
    );
    context
        .access()
        .run("fixture", |handle| async move {
            handle
                .execute(ExecuteRequest {
                    query_id: QueryId::new(),
                    sql: "CREATE TABLE items (id INTEGER PRIMARY KEY)".into(),
                    max_rows: None,
                    timeout: None,
                })
                .await
        })
        .await
        .unwrap();

    let (client, shutdown, sessions, service) = start_http(Arc::clone(&context)).await;
    let first = client.initialize().await;
    let second = client.initialize().await;
    assert_ne!(first, second);
    assert_live_profiles_http(&client, &first, &ProfileStore::new(&paths), base).await;
    let list = client.rpc(&first, "tools/list", json!({})).await;
    for name in crate::config::tool_names() {
        assert!(
            list["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == name),
            "{name}"
        );
    }
    assert_schema_http(&client, &first).await;
    assert_ddl_http(&client, &first).await;
    let begin = json!({"connection_alias":"fixture","database":"main"});
    let open = client
        .tool(&first, "begin_transaction", begin.clone())
        .await;
    assert_eq!(open["structuredContent"]["state"], "open", "{open}");
    let tx = &open["structuredContent"]["transaction_id"];
    let insert = |tx: &Value, row| {
        json!({
            "transaction_id":tx,"operation_id":format!("insert-{row}"), "operation":"insert",
            "table":"items","rows":[{"id":row}]
        })
    };
    assert_eq!(
        client
            .tool(&second, "execute_in_transaction", insert(tx, 1))
            .await["structuredContent"]["error"]["code"],
        "transaction.not_found"
    );
    let (left, right) = tokio::join!(
        client.tool(&first, "execute_in_transaction", insert(tx, 1)),
        client.tool(&first, "execute_in_transaction", insert(tx, 2)),
    );
    for reply in [&left, &right] {
        assert_eq!(reply["structuredContent"]["state"], "open", "{reply}");
    }
    let mut totals = [
        left["structuredContent"]["accumulated_affected_rows"].as_u64(),
        right["structuredContent"]["accumulated_affected_rows"].as_u64(),
    ];
    totals.sort_unstable();
    assert_eq!(totals, [Some(1), Some(2)]);
    let reply = client
        .tool(&first, "commit_transaction", json!({"transaction_id":tx}))
        .await;
    assert_eq!(reply["structuredContent"]["state"], "committed", "{reply}");
    let race = client
        .tool(&first, "begin_transaction", begin.clone())
        .await;
    let race_id = &race["structuredContent"]["transaction_id"];
    let race_args = json!({"transaction_id":race_id});
    let (commit, rollback) = tokio::join!(
        client.tool(&first, "commit_transaction", race_args.clone()),
        client.tool(&first, "rollback_transaction", race_args),
    );
    assert_ne!(
        commit["isError"].as_bool().unwrap_or(false),
        rollback["isError"].as_bool().unwrap_or(false)
    );
    assert_eq!(
        commit["structuredContent"]["state"],
        rollback["structuredContent"]["state"]
    );
    let open = client.tool(&first, "begin_transaction", begin).await;
    let blocked = client.tool(&second, "alter_table", json!({
        "connection_alias":"fixture","database":"main","table":"items",
        "operation_id":ddl_test_id(),"action":"add_column","column":{"name":"busy_probe","type":"text"},
    })).await;
    assert_eq!(
        blocked["structuredContent"]["state"], "not_started",
        "{blocked}"
    );
    assert_eq!(
        blocked["structuredContent"]["error"]["code"], "sqlite.busy",
        "{blocked}"
    );
    let tx = &open["structuredContent"]["transaction_id"];
    let reply = client
        .tool(&first, "execute_in_transaction", insert(tx, 3))
        .await;
    assert_eq!(reply["structuredContent"]["state"], "open", "{reply}");
    assert!(
        client
            .request("DELETE", Some(&first), Value::Null)
            .await
            .starts_with("HTTP/1.1 202")
    );
    let third = client.initialize().await;
    assert_eq!(
        client
            .tool(&third, "commit_transaction", json!({"transaction_id":tx}))
            .await["structuredContent"]["error"]["code"],
        "transaction.not_found"
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let audit = std::fs::read_to_string(context.audit().path()).unwrap();
            if audit
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|line| {
                    line["transaction_id"] == *tx && line["transaction_state"] == "rolled_back"
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("DELETE rolls back without waiting for the HTTP server shutdown");
    let read = client.tool(&second, "execute_readonly_query", json!({
        "connection_alias":"fixture", "database":"", "sql":"SELECT id FROM items ORDER BY id"
    })).await;
    assert_eq!(read["structuredContent"]["rows"], json!([[1], [2]]));
    shutdown.cancel();
    close_sessions(&sessions).await;
    context.shutdown().await;
    service.await.unwrap();
}

fn ddl_test_id() -> uuid::Uuid {
    let millis = u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000)
        .expect("Unix milliseconds");
    let mut bytes = *uuid::Uuid::new_v4().as_bytes();
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6] = (bytes[6] & 15) | 0x70;
    uuid::Uuid::from_bytes(bytes)
}

async fn assert_live_profiles_http(
    client: &HttpClient,
    session: &str,
    store: &ProfileStore,
    base: &Path,
) {
    let a = base.join("profile-a.db");
    let b = base.join("profile-b.db");
    for (path, sql) in [
        (&a, "CREATE TABLE items (column_a INTEGER)"),
        (&b, "CREATE TABLE items (column_b INTEGER)"),
    ] {
        rusqlite::Connection::open(path)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
    let mut draft = store
        .snapshot()
        .unwrap()
        .resolve("fixture")
        .unwrap()
        .clone();
    draft.profile_id = uuid::Uuid::nil();
    draft.revision = 0;
    draft.name = "HTTP 开发库".into();
    draft.params.host = a.display().to_string();
    draft.file_path.clone_from(&draft.params.host);
    let mut profile = store.save(draft, None).unwrap();
    let id = profile.profile_id.to_string();
    let list = client.tool(session, "list_connections", json!({})).await;
    assert!(
        list["structuredContent"]["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["connection_id"] == id && entry["display_name"] == "HTTP 开发库"),
        "{list}"
    );
    let args = json!({"connection_alias":id,"database":"main","table":"items"});
    let first = client.tool(session, "describe_table", args.clone()).await;
    assert_eq!(
        first["structuredContent"]["columns"][0]["name"], "column_a",
        "{first}"
    );

    let revision = profile.revision;
    profile.name = "HTTP 已改名".into();
    profile.display_name = Some("HTTP 已改名".into());
    profile.params.host = b.display().to_string();
    profile.file_path.clone_from(&profile.params.host);
    let profile = store.save(profile, Some(revision)).unwrap();
    let resource = client
        .rpc(
            session,
            "resources/read",
            json!({"uri":"database://connections"}),
        )
        .await;
    let connections: Value =
        serde_json::from_str(resource["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        connections["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["connection_id"] == id && entry["display_name"] == "HTTP 已改名"),
        "{resource}"
    );
    let second = client.tool(session, "describe_table", args.clone()).await;
    assert_eq!(
        second["structuredContent"]["columns"][0]["name"], "column_b",
        "{second}"
    );

    store.delete(profile.profile_id, profile.revision).unwrap();
    let missing = client.tool(session, "describe_table", args).await;
    assert_eq!(
        missing["structuredContent"]["error"]["code"], "mcp.connection_not_found",
        "{missing}"
    );
}

async fn assert_schema_http(client: &HttpClient, session: &str) {
    let target = json!({"connection_alias":"fixture","database":"main","table":"items"});
    let result = client
        .tool(session, "list_foreign_keys", target.clone())
        .await;
    assert_eq!(
        result["structuredContent"]["foreign_keys"],
        json!([]),
        "{result}"
    );
    let result = client.tool(session, "get_table_ddl", target).await;
    assert_eq!(
        result["structuredContent"]["source"], "database_native",
        "{result}"
    );
    let result = client
        .tool(
            session,
            "get_database_schema",
            json!({
                "connection_alias":"fixture","database":"main","page_size":1
            }),
        )
        .await;
    assert_eq!(
        result["structuredContent"]["objects"][0]["name"], "items",
        "{result}"
    );
}

async fn assert_ddl_http(client: &HttpClient, session: &str) {
    let mut args = json!({
        "connection_alias":"fixture", "database":"main", "table":"http_ddl",
        "columns":[{"name":"id","type":"integer","primary_key":true}],
    });
    let preview = client
        .tool(
            session,
            "preview_schema_change",
            json!({
                "operation":"create_table", "arguments":args,
            }),
        )
        .await;
    assert_eq!(
        preview["structuredContent"]["can_execute"], true,
        "{preview}"
    );
    assert!(preview["structuredContent"]["current_schema"].is_null());
    args["operation_id"] = json!(ddl_test_id());
    let unauthorized = HttpClient {
        addr: client.addr,
        credential: new_token().unwrap(),
        next_request: AtomicU64::new(1),
    };
    let rejected = unauthorized
        .request(
            "POST",
            Some(session),
            json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"create_table","arguments":args},
            }),
        )
        .await;
    assert!(rejected.starts_with("HTTP/1.1 401"), "{rejected}");
    let result = client.tool(session, "create_table", args.clone()).await;
    assert_eq!(
        result["structuredContent"]["state"], "completed",
        "{result}"
    );
    let result = client.tool(session, "create_table", args).await;
    assert_eq!(result["structuredContent"]["replayed"], true, "{result}");
}

#[tokio::test]
async fn native_mariadb_writes_over_http_and_stdio() {
    let Ok(port) = std::env::var("SQARVO_NATIVE_MARIADB_PORT") else {
        eprintln!("requires disposable MariaDB and mcp-native-mariadb-fixture.sql");
        return;
    };
    let port: u16 = port.parse().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path();
    let config = base.join("fixture.json");
    let profile = |name, username, database| {
        json!({
            "alias": name, "driver_id":"mariadb", "host":"127.0.0.1", "port":port,
            "username":username, "database":database, "enabled":true, "tls":"disabled",
        })
    };
    let mut profiles = vec![
        profile("writer", "native_writer", Some("native_a")),
        profile("insert_only", "native_insert", None),
        profile("reader", "native_read", None),
    ];
    let proxy_port = std::env::var("SQARVO_NATIVE_MARIADB_ACK_PROXY_PORT").ok();
    if let Some(proxy_port) = &proxy_port {
        let mut proxy = profile("proxy", "native_writer", Some("native_a"));
        proxy["port"] = json!(proxy_port.parse::<u16>().unwrap());
        profiles.push(proxy);
    }
    std::fs::write(&config, json!({"connections":profiles}).to_string()).unwrap();

    let paths = AppPaths::from_roots(base.join("config"), base.to_owned(), base.join("run"));
    let store = ProfileStore::new(&paths);
    let context = Arc::new(
        ServerContext::bootstrap_with(Some(&config), base)
            .await
            .unwrap(),
    );
    let (client, shutdown, sessions, service) = start_http(Arc::clone(&context)).await;
    let http = client.initialize().await;
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let row = u32::from_be_bytes(bytes[..4].try_into().unwrap()) % 1_000_000_000 + 1;
    assert_native_writes(
        async |tool, args| client.tool(&http, tool, args).await,
        &store,
        row,
    )
    .await;
    assert_native_ddl_lock_timeout(&client, &http, &store, &context).await;
    shutdown.cancel();
    close_sessions(&sessions).await;
    service.await.unwrap();

    assert_native_stdio(&context, &store, row + 10, proxy_port.is_some()).await;
    context.shutdown().await;
}

async fn assert_native_stdio(
    context: &Arc<ServerContext>,
    store: &ProfileStore,
    row: u32,
    simulate_reply_loss: bool,
) {
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (server_pipe, client_pipe) = tokio::io::duplex(32 * 1024);
    let stdio_server = SqarvoMcpServer::new(Arc::clone(context));
    let (server_read, server_write) = tokio::io::split(server_pipe);
    let server = tokio::spawn(async move {
        Box::pin(stdio_server.serve((server_read, server_write)))
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let (read, mut write) = tokio::io::split(client_pipe);
    let mut read = BufReader::new(read);
    let mut rpc = async |method: &str, params: Value, id: Option<u64>| {
        let mut request = json!({"jsonrpc":"2.0","method":method,"params":params});
        if let Some(id) = id {
            request["id"] = json!(id);
        }
        write
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        if id.is_none() {
            return Value::Null;
        }
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(15), read.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str::<Value>(&line).unwrap()["result"].clone()
    };
    let initialize = rpc("initialize", json!({
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}
    }), Some(1)).await;
    assert_eq!(initialize["protocolVersion"], "2025-11-25");
    rpc("notifications/initialized", json!({}), None).await;
    let mut id = 1;
    assert_native_writes(
        async |tool, args| {
            id += 1;
            rpc(
                "tools/call",
                json!({"name":tool,"arguments":args}),
                Some(id),
            )
            .await
        },
        store,
        row,
    )
    .await;
    if simulate_reply_loss {
        assert_native_unknown_reply(
            async |tool, args| {
                id += 1;
                rpc(
                    "tools/call",
                    json!({"name":tool,"arguments":args}),
                    Some(id),
                )
                .await
            },
            store,
        )
        .await;
    }
    write.shutdown().await.unwrap();
    server.await.unwrap();
}

async fn assert_native_ddl_lock_timeout(
    client: &HttpClient,
    session: &str,
    store: &ProfileStore,
    context: &ServerContext,
) {
    use sqarvo_driver_api::{IsolationLevel, TransactionOptions};

    store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();
    let params = store
        .snapshot()
        .unwrap()
        .resolve("writer")
        .unwrap()
        .params
        .clone();
    let blocker = context
        .access()
        .session_manager()
        .connect_isolated("lock fixture", params)
        .await
        .unwrap();
    let transaction = blocker
        .begin_transaction(TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: true,
        })
        .await
        .unwrap();
    blocker
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: "SELECT id FROM native_b.items LIMIT 1".into(),
            max_rows: Some(1),
            timeout: None,
        })
        .await
        .unwrap();
    let response = client
        .tool(
            session,
            "alter_table",
            json!({
                "connection_alias":"writer","database":"native_b","table":"items",
                "action":"add_column","column":{"name":"lock_probe","type":"integer"},
                "operation_id":ddl_test_id(),
            }),
        )
        .await;
    assert_eq!(
        response["structuredContent"]["state"], "failed",
        "{response}"
    );
    assert_eq!(
        response["structuredContent"]["error"]["category"], "timeout",
        "{response}"
    );
    blocker
        .rollback_isolated_transaction(&transaction.transaction_id)
        .await
        .unwrap();
    blocker.close().await.unwrap();
}

async fn assert_native_unknown_reply(
    mut call: impl AsyncFnMut(&str, Value) -> Value,
    store: &ProfileStore,
) {
    store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();
    let arguments = json!({
        "connection_alias":"proxy","database":"native_b","table":"ddl_ack_unknown",
        "columns":[{"name":"id","type":"integer"}], "operation_id":ddl_test_id(),
    });
    let response = call("create_table", arguments.clone()).await;
    assert_eq!(
        response["structuredContent"]["state"], "outcome_unknown",
        "{response}"
    );
    let replay = call("create_table", arguments).await;
    assert_eq!(
        replay["structuredContent"]["state"], "outcome_unknown",
        "{replay}"
    );
    assert_eq!(replay["structuredContent"]["replayed"], true, "{replay}");
    let read = call(
        "describe_table",
        json!({
            "connection_alias":"writer","database":"native_b","table":"ddl_ack_unknown"
        }),
    )
    .await;
    assert_eq!(
        read["structuredContent"]["columns"][0]["name"], "id",
        "{read}"
    );
}

#[allow(clippy::too_many_lines)] // The two protocol fixtures share one ordered set of assertions.
async fn assert_native_writes(
    mut call: impl AsyncFnMut(&str, Value) -> Value,
    store: &ProfileStore,
    row: u32,
) {
    store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: false,
        })
        .unwrap();
    let table = format!("fixture_{}", uuid::Uuid::new_v4().simple());
    let create = json!({
        "connection_alias":"writer","database":"native_b","table":table,
        "columns":[{"name":"id","type":"integer","primary_key":true}], "operation_id":ddl_test_id()
    });
    let add = json!({
        "connection_alias":"writer","database":"native_b","table":table,"action":"add_column",
        "column":{"name":"value","type":"text"}, "operation_id":ddl_test_id()
    });
    for (tool, args) in [("create_table", &create), ("alter_table", &add)] {
        let applied = call(tool, args.clone()).await;
        assert_eq!(
            applied["structuredContent"]["effect"], "applied",
            "{applied}"
        );
        let replay = call(tool, args.clone()).await;
        assert_eq!(replay["structuredContent"]["replayed"], true, "{replay}");
    }
    let description = call(
        "describe_table",
        json!({
            "connection_alias":"writer","database":"native_b","table":table
        }),
    )
    .await;
    assert_eq!(
        description["structuredContent"]["columns"][1]["name"], "value",
        "{description}"
    );
    let insert = json!({
        "connection_alias":"insert_only","database":"native_b","table":"items",
        "rows":[{"id":row,"parent":1}]
    });
    let mut candidate = insert.clone();
    candidate["operation"] = json!("insert");
    let preview = call("preview_write", candidate).await;
    assert_eq!(
        preview["structuredContent"]["estimated_rows"], 1,
        "{preview}"
    );
    let reply = call("insert_rows", insert.clone()).await;
    assert_eq!(
        reply["structuredContent"]["transaction_state"], "committed",
        "{reply}"
    );
    assert_eq!(reply["structuredContent"]["affected_rows"], 1, "{reply}");
    assert_eq!(
        reply["structuredContent"]["count_kind"], "direct",
        "{reply}"
    );
    let mut invalid = insert.clone();
    invalid["rows"] = json!([{"id":row + 1,"parent":-1}]);
    let failure = call("insert_rows", invalid).await;
    assert_eq!(
        failure["structuredContent"]["transaction_state"], "rolled_back",
        "{failure}"
    );
    for table in ["items", "audit"] {
        let read = call(
            "execute_readonly_query",
            json!({
                "connection_alias":"writer","database":"native_b",
                "sql":format!("SELECT id FROM {table} WHERE id IN ({row}, {}) ORDER BY id", row + 1)
            }),
        )
        .await;
        assert_eq!(read["structuredContent"]["rows"], json!([[row]]), "{read}");
    }
    for (tool, mut args) in [
        ("insert_rows", insert.clone()),
        ("create_table", create.clone()),
        ("alter_table", add.clone()),
    ] {
        args["connection_alias"] = json!("reader");
        if tool != "insert_rows" {
            args["operation_id"] = json!(ddl_test_id());
            args["table"] = if tool == "alter_table" {
                json!("items")
            } else {
                json!(format!("{table}_denied"))
            };
        }
        let rejected = call(tool, args).await;
        assert_eq!(
            rejected["structuredContent"]["error"]["code"], "mariadb.permission_denied",
            "{rejected}"
        );
    }
    let mut nontransactional = insert.clone();
    nontransactional["table"] = json!("nontransactional");
    nontransactional["rows"] = json!([{"id":row}]);
    let rejected = call("insert_rows", nontransactional).await;
    assert_eq!(
        rejected["structuredContent"]["error"]["code"], "capability.unsupported",
        "{rejected}"
    );

    store
        .set_mcp_access(McpAccessPolicy {
            enabled: true,
            read_only: true,
        })
        .unwrap();
    for (tool, args) in [
        ("insert_rows", insert),
        ("create_table", create),
        ("alter_table", add),
    ] {
        let rejected = call(tool, args).await;
        assert_eq!(
            rejected["structuredContent"]["error"]["category"], "permission_denied",
            "{rejected}"
        );
    }
    store.set_mcp_access(McpAccessPolicy::default()).unwrap();
    let rejected = call("list_connections", json!({})).await;
    assert_eq!(
        rejected["structuredContent"]["error"]["code"], "mcp.connection_denied",
        "{rejected}"
    );
}
