//! Resources 的 stdio E2E（#210）：以子进程驱动真实二进制，走完整协议。
//!
//! 为什么不用 in-process `ServerHandler`：`resources/*` 的可见行为里有一半是
//! **协议层**的（`contents[].mimeType`、JSON-RPC `-32602` / `-32002` 的区分、
//! stdout 纯净性），只有真的跑一遍 stdio 才算验证过。
//!
//! 覆盖：
//!
//! - `resources/list`（静态资源）与 `resources/templates/list`（三个模板）；
//! - `resources/read` 四类资源的正向路径（SQLite 夹具，含需要编码的表名）；
//! - URI 编码对抗：非法 `%zz`、双重编码 `%252f`、裸 `/` 多段、未授权 alias；
//! - `tools/list` 按 `enabled_tools` 过滤（#210 顺路项）。
//!
//! 子进程的 `HOME`/`XDG_*` 全部指向临时目录，不污染用户真实的配置与审计目录。

// The binary tests rely on XDG isolation for the real shared profile store.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use sqarvo_common::QueryId;
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{ConnectionParams, DriverId, ExecuteRequest, TlsMode};
use sqarvo_drivers::DriverRegistry;

mod common;

/// 单条协议消息等待上限。
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// 进程退出等待上限。
const EXIT_TIMEOUT: Duration = Duration::from_secs(20);

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_sqarvo-mcp")
}

// ---------------------------------------------------------------------------
// 协议客户端（与 tests/stdio_skeleton.rs 同一形态；集成测试文件之间不能共享代码）
// ---------------------------------------------------------------------------

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    stderr_reader: std::thread::JoinHandle<String>,
    next_id: u32,
}

impl Server {
    fn spawn(config: &Path, home: &Path) -> Self {
        let mut child = Command::new(binary())
            .arg("--config")
            .arg(config)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_RUNTIME_DIR", home.join("run"))
            .env("SQARVO_MCP_LOG", "warn,sqarvo_mcp=info")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 sqarvo-mcp");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let stdin = child.stdin.take();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut collected = String::new();
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                collected.push_str(&line);
                collected.push('\n');
            }
            collected
        });
        Self {
            child,
            stdin,
            lines,
            stderr_reader,
            next_id: 1,
        }
    }

    fn send(&mut self, message: &serde_json::Value) {
        let stdin = self.stdin.as_mut().expect("stdin 仍打开");
        writeln!(stdin, "{message}").expect("写入请求");
        stdin.flush().expect("flush");
    }

    fn next_message(&self) -> serde_json::Value {
        let line = self
            .lines
            .recv_timeout(READ_TIMEOUT)
            .unwrap_or_else(|error| panic!("等待响应超时/断流：{error}"));
        serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!("stdout 出现非 JSON 行（协议被污染）：{line:?} => {error}")
        })
    }

    /// 发一个请求并返回完整的响应对象（`result` 或 `error` 由调用方判断）。
    fn request(&mut self, method: &str, params: &serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        let response = self.next_message();
        assert_eq!(response["id"], id, "响应 id 必须与请求一致：{response}");
        response
    }

    /// 读一个资源，返回 `contents[0]` 的 JSON 文本解析结果。
    fn read_resource(&mut self, uri: &str) -> serde_json::Value {
        let response = self.request("resources/read", &serde_json::json!({ "uri": uri }));
        let result = response
            .get("result")
            .unwrap_or_else(|| panic!("读取 {uri} 失败：{response}"));
        let contents = result["contents"]
            .as_array()
            .unwrap_or_else(|| panic!("contents 必须是数组：{result}"));
        assert_eq!(contents.len(), 1, "每个资源只回一份内容：{result}");
        let content = &contents[0];
        assert_eq!(content["uri"], uri, "contents[].uri 必须回显请求 URI");
        assert_eq!(
            content["mimeType"], "application/json",
            "资源必须是 application/json：{content}"
        );
        let text = content["text"].as_str().expect("text 必须是字符串");
        assert!(
            content.get("blob").is_none(),
            "文本资源不得带 blob 字段：{content}"
        );
        serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("{uri} 的内容不是合法 JSON：{error}\n{text}"))
    }

    /// 断言请求以指定 JSON-RPC 错误码失败。
    fn expect_error(&mut self, method: &str, params: &serde_json::Value, code: i64) -> String {
        let response = self.request(method, params);
        let error = response
            .get("error")
            .unwrap_or_else(|| panic!("应当失败，实际成功：{response}"));
        assert_eq!(error["code"], code, "错误码不符：{response}");
        error["message"].as_str().unwrap_or_default().to_owned()
    }

    fn initialize(&mut self) {
        let response = self.request(
            "initialize",
            &serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "sqarvo-mcp-resources-e2e", "version": "0.0.0" }
            }),
        );
        assert_eq!(response["result"]["serverInfo"]["name"], "sqarvo-mcp");
        assert!(
            response["result"]["capabilities"]["resources"].is_object(),
            "必须声明 resources 能力：{response}"
        );
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
    }

    fn shutdown(mut self) -> (ExitStatus, String) {
        self.stdin.take();
        let deadline = Instant::now() + EXIT_TIMEOUT;
        let status = loop {
            match self.child.try_wait().expect("回收子进程") {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    panic!("子进程未在 {EXIT_TIMEOUT:?} 内退出");
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let stderr = self.stderr_reader.join().expect("stderr 读取线程");
        (status, stderr)
    }
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 用真实驱动建一个 SQLite 库（表 + 视图 + 需要编码的表名）。
async fn create_fixture(path: &Path) {
    let registry = Arc::new(DriverRegistry::with_builtin_drivers());
    let sessions = SessionManager::new(registry);
    let params = ConnectionParams {
        driver_id: DriverId::new("sqlite").expect("driver id"),
        host: path.display().to_string(),
        port: 0,
        database: None,
        username: None,
        credential_ref: None,
        tls: TlsMode::Unspecified,
        tls_server_name: None,
        properties: BTreeMap::new(),
    };
    let summary = sessions
        .connect("fixture", params)
        .await
        .expect("连接 SQLite 夹具");
    let handle = sessions
        .acquire(&summary.session_id)
        .await
        .expect("取得会话句柄");
    for sql in [
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE VIEW v_users AS SELECT id FROM users",
        "CREATE TABLE \"a/b\" (id INTEGER PRIMARY KEY)",
    ] {
        handle
            .execute(ExecuteRequest {
                query_id: QueryId::new(),
                sql: sql.to_owned(),
                max_rows: None,
                timeout: None,
            })
            .await
            .unwrap_or_else(|error| panic!("布置夹具失败：{error}\nSQL: {sql}"));
    }
    sessions.close_all().await;
}

fn write_config(dir: &Path, root: &serde_json::Value) -> PathBuf {
    let path = dir.join("mcp.json");
    std::fs::write(&path, root.to_string()).expect("写入 mcp.json");
    path
}

fn sqlite_root(db: &Path) -> serde_json::Value {
    serde_json::json!({
        "connections": [{
            "alias": "dev",
            "driver_id": "sqlite",
            "host": db.display().to_string(),
            "port": 0,
            "enabled": true,
            "properties": { "read_only": "true" }
        }]
    })
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resources_are_listed_advertised_and_read_over_stdio() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("resources.db");
    create_fixture(&db).await;
    let config = write_config(dir.path(), &sqlite_root(&db));
    common::enable_access(&config, dir.path());
    let mut server = Server::spawn(&config, dir.path());
    server.initialize();

    // resources/list：一个静态资源，JSON 内容。
    let listed = server.request("resources/list", &serde_json::json!({}));
    let resources = listed["result"]["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("resources 必须是数组：{listed}"));
    assert_eq!(resources.len(), 1, "V1 只有一个静态资源：{listed}");
    assert_eq!(resources[0]["uri"], "database://connections");
    assert_eq!(resources[0]["mimeType"], "application/json");
    assert!(resources[0]["description"].is_string());

    // resources/templates/list：三个模板。
    let templates = server.request("resources/templates/list", &serde_json::json!({}));
    let templates = templates["result"]["resourceTemplates"]
        .as_array()
        .unwrap_or_else(|| panic!("resourceTemplates 必须是数组：{templates}"));
    let uris: Vec<&str> = templates
        .iter()
        .map(|template| template["uriTemplate"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        uris,
        vec![
            "database://{connection_alias}/{database}/overview",
            "database://{connection_alias}/{database}/schema",
            "database://{connection_alias}/{database}/tables/{table}",
        ]
    );
    for template in templates {
        assert_eq!(template["mimeType"], "application/json");
    }

    // 四类资源的正向读取。
    let connections = server.read_resource("database://connections");
    assert_eq!(connections["count"], 1);
    assert_eq!(connections["connections"][0]["alias"], "dev");
    assert_eq!(
        connections["connections"][0]["connection_status"],
        "not_connected"
    );

    let overview = server.read_resource("database://dev/main/overview");
    assert_eq!(overview["database_name"], "main");
    assert!(overview["server_version"].is_null());
    assert_eq!(overview["table_count"], 2);
    assert_eq!(overview["view_count"], 1);

    let schema = server.read_resource("database://dev/main/schema");
    assert_eq!(schema["object_count"], 3);
    assert_eq!(schema["truncated"], false);

    let table = server.read_resource("database://dev/main/tables/users");
    assert_eq!(table["table"], "users");
    assert_eq!(table["column_count"], 2);
    assert_eq!(table["primary_key"][0]["name"], "id");

    // 编码对抗：表名里的 `/` 必须编码成一段，不能变成两段。
    let escaped = server.read_resource("database://dev/main/tables/a%2Fb");
    assert_eq!(escaped["table"], "a/b");

    // 负向：非法编码、双重编码、裸 `/` 多段、未授权 alias。
    let message = server.expect_error(
        "resources/read",
        &serde_json::json!({ "uri": "database://dev/main/tables/%zz" }),
        -32602,
    );
    assert!(message.contains("mcp.invalid_argument"), "{message}");
    server.expect_error(
        "resources/read",
        &serde_json::json!({ "uri": "database://dev/main/tables/%252f" }),
        -32602,
    );
    server.expect_error(
        "resources/read",
        &serde_json::json!({ "uri": "database://dev/main/tables/a/b" }),
        -32602,
    );
    let message = server.expect_error(
        "resources/read",
        &serde_json::json!({ "uri": "database://nope/main/overview" }),
        -32002,
    );
    assert!(message.contains("mcp.connection_not_found"), "{message}");

    let (status, stderr) = server.shutdown();
    assert!(status.success(), "stdin EOF 必须干净退出：{stderr}");
}

#[tokio::test]
async fn tools_list_only_advertises_enabled_tools() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("tools-list.db");
    create_fixture(&db).await;
    let mut root = sqlite_root(&db);
    root["enabled_tools"] = serde_json::json!(["list_tables", "describe_table"]);
    let config = write_config(dir.path(), &root);

    common::enable_access(&config, dir.path());
    let mut server = Server::spawn(&config, dir.path());
    server.initialize();
    let listed = server.request("tools/list", &serde_json::json!({}));
    let mut names: Vec<String> = listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools 必须是数组：{listed}"))
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["describe_table", "list_tables"]);

    // 调用期行为不变：被禁用的工具仍被拒绝，且错误是工具级的（isError），
    // 不是「工具不存在」的 -32602。
    let response = server.request(
        "tools/call",
        &serde_json::json!({ "name": "list_connections", "arguments": {} }),
    );
    assert_eq!(
        response["result"]["isError"], true,
        "被禁用的工具必须返回工具级错误：{response}"
    );
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("mcp.tool_disabled"),
        "{response}"
    );

    let (status, stderr) = server.shutdown();
    assert!(status.success(), "stdin EOF 必须干净退出：{stderr}");
}
