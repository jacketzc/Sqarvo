//! stdio 骨架 E2E（#207 验收的一部分；完整协议矩阵归 #211）。
//!
//! 直接以子进程驱动真实二进制（`CARGO_BIN_EXE_sqarvo-mcp`），按行喂 JSON-RPC，
//! 覆盖三件 #207 必须成立的事：
//!
//! 1. `initialize(2025-11-25)` → `notifications/initialized` → `tools/list` 全流程成功，
//!    stdin 关闭后进程以 **exit 0** 结束（阶段 A spike：EOF 是干净退出）；
//! 2. **stdout 纯净**：每一行都是带 `jsonrpc` 字段的 JSON——日志只能出现在 stderr；
//! 3. 配置三态在进程级生效：缺失 → 空连接列表仍能握手；非法 → 拒绝启动且 stdout 全空。
//!
//! 子进程的 `HOME`/`XDG_*` 全部指向临时目录，避免测试污染用户的真实配置/审计目录。

// The binary tests rely on XDG isolation for the real shared profile store.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 单条协议消息等待上限：正常响应是毫秒级，超时说明卡死而不是慢。
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// 进程退出等待上限。
const EXIT_TIMEOUT: Duration = Duration::from_secs(20);

/// 被测二进制。
fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_sqarvo-mcp")
}

/// 一个跑起来的 server 子进程。
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stdout_all: Arc<Mutex<Vec<String>>>,
    stderr_reader: JoinHandle<String>,
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
            // 让 stderr 至少能看见 warn 级配置告警。
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
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                sink.lock().expect("stdout 缓冲锁").push(line.clone());
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_reader = std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut collected = String::new();
            for line in reader.lines() {
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
            stdout_all: captured,
            stderr_reader,
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

    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    fn wait(mut self) -> Outcome {
        let deadline = Instant::now() + EXIT_TIMEOUT;
        let status = loop {
            match self.child.try_wait().expect("尝试回收子进程") {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    panic!("子进程未在 {EXIT_TIMEOUT:?} 内退出");
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let stdout = self.stdout_all.lock().expect("stdout 缓冲锁").clone();
        let stderr = self.stderr_reader.join().expect("stderr 读取线程");
        Outcome {
            status,
            stdout,
            stderr,
        }
    }
}

struct Outcome {
    status: ExitStatus,
    stdout: Vec<String>,
    stderr: String,
}

impl Outcome {
    /// 协议纯净性：每一行 stdout 都必须是带 `jsonrpc` 字段的 JSON。
    fn assert_protocol_purity(&self) {
        for line in &self.stdout {
            let value: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("stdout 含非 JSON 行 {line:?}：{error}"));
            assert!(
                value.get("jsonrpc").is_some(),
                "stdout 行缺少 jsonrpc 字段（日志泄漏进协议流）：{line:?}"
            );
        }
    }
}

fn write_config(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("mcp.json");
    std::fs::write(&path, body).expect("写入 mcp.json");
    path
}

fn initialize_message(id: u32) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "sqarvo-mcp-e2e", "version": "0.0.0" }
        }
    })
}

fn initialized_notification() -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
}

#[test]
fn initialize_initialized_tools_list_then_eof_exits_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("test.db");
    let config = write_config(
        dir.path(),
        &serde_json::json!({
            "connections": [{
                "alias": "dev",
                "driver_id": "sqlite",
                "host": db.display().to_string(),
                "port": 0,
                "enabled": true,
                "properties": { "read_only": "true" }
            }]
        })
        .to_string(),
    );

    let mut server = Server::spawn(&config, dir.path());
    server.send(&initialize_message(1));
    let response = server.next_message();
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    let result = &response["result"];
    assert_eq!(
        result["protocolVersion"], "2025-11-25",
        "旧握手必须按客户端请求的已知版本协商（spike 实测口径）"
    );
    assert_eq!(result["serverInfo"]["name"], "sqarvo-mcp");
    assert!(
        result["capabilities"]["tools"].is_object(),
        "骨架阶段也要声明 tools 能力（列表可以是空的）：{result}"
    );

    server.send(&initialized_notification());
    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let tools = server.next_message();
    assert_eq!(tools["id"], 2);
    let listed: Vec<String> = tools["result"]["tools"]
        .as_array()
        .expect("tools 必须是数组")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    let mut listed_sorted = listed;
    listed_sorted.sort();
    let mut expected: Vec<String> = sqarvo_mcp::config::tool_names()
        .map(str::to_owned)
        .collect();
    expected.sort();
    assert_eq!(
        listed_sorted, expected,
        "#209 之后 tools/list 必须与 V1_TOOL_NAMES 一一对应：{tools}"
    );

    server.close_stdin();
    let outcome = server.wait();
    assert!(
        outcome.status.success(),
        "stdin EOF 必须干净退出（exit 0）：{:?}\nstderr:\n{}",
        outcome.status,
        outcome.stderr
    );
    outcome.assert_protocol_purity();
    assert_eq!(
        outcome.stdout.len(),
        2,
        "只应有两行响应：{:#?}",
        outcome.stdout
    );
}

#[test]
fn empty_enabled_tools_still_starts_and_serves_the_handshake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(
        dir.path(),
        &serde_json::json!({
            "connections": [],
            "enabled_tools": []
        })
        .to_string(),
    );
    let mut server = Server::spawn(&config, dir.path());
    server.send(&initialize_message(7));
    let response = server.next_message();
    assert_eq!(response["id"], 7);
    server.close_stdin();
    let outcome = server.wait();
    assert!(outcome.status.success(), "stderr:\n{}", outcome.stderr);
    outcome.assert_protocol_purity();
    assert!(
        outcome.stderr.contains("all tools disabled"),
        "空数组必须在 stderr 给出配置告警：\n{}",
        outcome.stderr
    );
}

#[test]
fn missing_config_starts_with_zero_connections_and_warns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("does-not-exist.json");

    let mut server = Server::spawn(&missing, dir.path());
    server.send(&initialize_message(3));
    let response = server.next_message();
    assert_eq!(response["id"], 3);
    server.close_stdin();
    let outcome = server.wait();
    assert!(
        outcome.status.success(),
        "配置缺失应「空连接列表启动」而不是失败：\n{}",
        outcome.stderr
    );
    outcome.assert_protocol_purity();
    assert!(
        outcome
            .stderr
            .contains("default tool configuration and query limits"),
        "缺失配置必须在 stderr 告警：\n{}",
        outcome.stderr
    );
}

#[test]
fn invalid_config_refuses_to_start_with_a_clean_stdout() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 未知工具名 = 配置错误，必须拒绝启动（V3.1 §3.1）。
    let synthetic_value = "synthetic-do-not-echo-config-value";
    let config = write_config(
        dir.path(),
        &serde_json::json!({
            "enabled_tools": [synthetic_value]
        })
        .to_string(),
    );
    let server = Server::spawn(&config, dir.path());
    let outcome = server.wait();
    assert!(
        !outcome.status.success(),
        "非法配置必须拒绝启动（非零退出码）"
    );
    assert!(
        outcome.stdout.is_empty(),
        "启动失败时 stdout 必须为空（不得出现半条协议消息）：{:#?}",
        outcome.stdout
    );
    assert!(
        outcome.stderr.contains("enabled_tools"),
        "错误必须点名配置字段：\n{}",
        outcome.stderr,
    );
    assert!(!outcome.stderr.contains(synthetic_value));
}

#[cfg(unix)]
#[test]
fn sigterm_shuts_down_gracefully_with_exit_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(
        dir.path(),
        &serde_json::json!({ "connections": [] }).to_string(),
    );
    let mut server = Server::spawn(&config, dir.path());
    server.send(&initialize_message(5));
    assert_eq!(server.next_message()["id"], 5);
    assert_graceful_shutdown(server, "-TERM");
}

#[cfg(unix)]
#[test]
fn sigterm_before_initialize_shuts_down_gracefully() {
    assert_shutdown_before_initialize("-TERM");
}

#[cfg(unix)]
#[test]
fn sigint_before_initialize_shuts_down_gracefully() {
    assert_shutdown_before_initialize("-INT");
}

#[cfg(unix)]
fn assert_shutdown_before_initialize(signal: &str) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(dir.path(), r#"{"connections":[]}"#);
    let mut server = Server::spawn(&config, dir.path());
    // A ping proves the transport is serving while initialize is still pending.
    server.send(&serde_json::json!({"jsonrpc": "2.0", "id": 6, "method": "ping"}));
    let response = server.next_message();
    assert_eq!(response["id"], 6);
    assert_eq!(response["result"], serde_json::json!({}));
    assert_graceful_shutdown(server, signal);
}

#[cfg(unix)]
fn assert_graceful_shutdown(server: Server, signal: &str) {
    let pid = server.child.id();
    let status = Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .expect("调用 kill");
    assert!(status.success(), "发送 {signal} 失败");

    // 不再关闭 stdin：SIGTERM 自己必须能让 serve 循环停下来。
    let outcome = server.wait();
    assert!(
        outcome.status.success(),
        "{signal} 必须走优雅关停（exit 0）：{:?}\nstderr:\n{}",
        outcome.status,
        outcome.stderr
    );
    outcome.assert_protocol_purity();
}

/// `credentials set` 在非交互 stdin 上必须明确报错退出（不碰密钥环、不挂起）。
#[test]
fn credentials_set_requires_an_interactive_terminal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(binary())
        .args(["credentials", "set"])
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 credentials set");

    let deadline = Instant::now() + EXIT_TIMEOUT;
    let status = loop {
        match child.try_wait().expect("回收子进程") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("非交互环境下 credentials set 不得阻塞等待输入");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let output = child.wait_with_output().expect("收集输出");
    assert!(!status.success(), "非 TTY 必须失败退出");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("interactive terminal"),
        "必须给出可操作的错误提示：\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "失败路径不得向 stdout 写东西：{:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}
