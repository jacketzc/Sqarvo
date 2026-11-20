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
        SqarvoMcpServer::new_http(Arc::clone(&context)),
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
    let first = client.initialize().await;
    let second = client.initialize().await;
    assert_ne!(first, second);
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
