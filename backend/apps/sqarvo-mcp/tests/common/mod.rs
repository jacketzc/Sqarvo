//! #211 协议/审计 E2E 共用的子进程 stdio harness。
//!
//! 形状沿用 `tests/stdio_skeleton.rs`（不修改那个文件），补充矩阵测试需要的
//! 按 id 匹配请求/响应、`_meta` 构造与共享目录能力：
//!
//! - **环境隔离**：`HOME` / `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_RUNTIME_DIR`
//!   全部指向调用方给的 tempdir——审计文件落在 `$XDG_DATA_HOME/io.sqarvo.Sqarvo/mcp/`
//!   （`directories::ProjectDirs::from("io","sqarvo","Sqarvo")`，见
//!   `crates/local-store/src/paths.rs`），绝不写真实用户目录；
//! - **stdout 纯净性**：`next_message` 与 `Outcome::assert_protocol_purity` 双层钉死
//!   「每一行都是带 `jsonrpc` 字段的合法 JSON」（V3.1 §13 / §14.4）；
//! - 行级 JSON-RPC 直连真实二进制（`CARGO_BIN_EXE_sqarvo-mcp`），不经 SDK 客户端，
//!   这样才能发「裸请求」「缺 `_meta`」这类 SDK 客户端不会主动发的形状。
//!
//! 允许 `dead_code`：两个测试文件各用其中一部分助手，未用到的不视为警告。

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 单条协议消息等待上限：正常响应毫秒级，超时说明卡死而不是慢。
pub const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// 进程退出等待上限（子进程内含驱动与审计 I/O）。
pub const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// 被测二进制。
pub fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_sqarvo-mcp")
}

/// `audit.jsonl` 所在目录：`$home/data/sqarvo/mcp`。
///
/// 实测口径（#211 跑出的 server stderr 日志）：Linux 下 `directories::ProjectDirs`
/// 把数据目录解析为 `$XDG_DATA_HOME/sqarvo`（不是 `io.sqarvo.Sqarvo`），
/// `AuditWriter::open` 再拼 `mcp/` 子目录（audit.rs `DATA_SUBDIR`）。
pub fn audit_dir(home: &Path) -> std::path::PathBuf {
    home.join("data").join("sqarvo").join("mcp")
}

/// 最新草案协议版本（rmcp `ProtocolVersion::LATEST`）。
pub const DRAFT_VERSION: &str = "2026-07-28";

/// 2026-07-28 内联 `_meta`（spike REPORT §4：必需键
/// `io.modelcontextprotocol/protocolVersion` 与
/// `io.modelcontextprotocol/clientCapabilities`）。
pub fn draft_meta(protocol_version: &str) -> serde_json::Value {
    serde_json::json!({
        "io.modelcontextprotocol/protocolVersion": protocol_version,
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

/// 只含 `clientCapabilities` 的残缺 `_meta`（钉「缺 protocolVersion」路径）。
pub fn meta_without_version() -> serde_json::Value {
    serde_json::json!({
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

/// 一个跑起来的 server 子进程。
pub struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stdout_all: Arc<Mutex<Vec<String>>>,
    stderr_reader: JoinHandle<String>,
}

/// Run local management in the test server's isolated profile directory.
pub fn manage_access(config: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(binary())
        .arg("--config")
        .arg(config)
        .arg("access")
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .stdin(Stdio::null())
        .output()
        .expect("启动 access 管理子进程")
}

pub fn enable_access(config: &Path, home: &Path) {
    let enabled = manage_access(config, home, &["on", "--yes"]);
    assert!(
        enabled.status.success(),
        "启用夹具 MCP 访问：{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
}

impl Server {
    /// 以 `--config` + 隔离 XDG 环境启动。
    pub fn spawn(config: &Path, home: &Path) -> Self {
        let mut child = Command::new(binary())
            .arg("--config")
            .arg(config)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_RUNTIME_DIR", home.join("run"))
            // 默认口径：warn 全局 + 本 crate info（SIGTERM 的收尾日志在 info）。
            .env("SQARVO_MCP_LOG", "warn,sqarvo_mcp=info")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 sqarvo-mcp");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let stdin = child.stdin.take();

        let (sender, lines): (Sender<String>, Receiver<String>) = channel();
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

    /// 子进程 pid（flock 争用退化文件名 `audit.<pid>.jsonl` 要用）。
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn send(&mut self, message: &serde_json::Value) {
        let stdin = self.stdin.as_mut().expect("stdin 仍打开");
        writeln!(stdin, "{message}").expect("写入请求");
        stdin.flush().expect("flush");
    }

    /// 发一条请求并等响应；行级 FIFO，响应 `id` 必须与请求一致。
    ///
    /// `params`/`method` 按值接收是刻意的测试 API 形状（调用点全部传 `json!` 字面量），
    /// `json!` 宏内部按引用使用，故对 `needless_pass_by_value` 做定点豁免。
    #[allow(clippy::needless_pass_by_value)]
    pub fn request(
        &mut self,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let message = self.next_message();
        assert_eq!(
            message["id"], id,
            "响应 id 与请求不符（协议流错乱）：{message}"
        );
        assert_eq!(message["jsonrpc"], "2.0", "响应缺 jsonrpc 字段：{message}");
        message
    }

    /// 发一条通知（无 id、无响应）。
    #[allow(clippy::needless_pass_by_value)] // 同 `request`：测试 API 按值收字面量。
    pub fn notify(&mut self, method: &str, params: serde_json::Value) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    /// 取下一条 stdout 消息（必须是合法 JSON，否则视为污染直接 panic）。
    pub fn next_message(&self) -> serde_json::Value {
        let line = self
            .lines
            .recv_timeout(READ_TIMEOUT)
            .unwrap_or_else(|error| panic!("等待响应超时/断流：{error}"));
        serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!("stdout 出现非 JSON 行（协议被污染）：{line:?} => {error}")
        })
    }

    /// 断言「一段时间内没有更多 stdout」。
    pub fn assert_no_more_output(&self) {
        if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(500)) {
            panic!("收到计划外响应：{line}");
        }
    }

    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// 向进程发送 SIGTERM（Unix）。
    #[cfg(unix)]
    pub fn send_sigterm(&self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.pid().to_string())
            .status()
            .expect("调用 kill");
        assert!(status.success(), "发送 SIGTERM 失败");
    }

    /// 回收进程；stdout/stderr 全量收集。
    pub fn wait(mut self) -> Outcome {
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

pub struct Outcome {
    pub status: ExitStatus,
    pub stdout: Vec<String>,
    pub stderr: String,
}

impl Outcome {
    /// 协议纯净性：每一行 stdout 都必须是带 `jsonrpc` 字段的 JSON。
    pub fn assert_protocol_purity(&self) {
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

// ---------------------------------------------------------------------------
// JSON-RPC 消息构造
// ---------------------------------------------------------------------------

/// `initialize` 请求（旧握手）。
pub fn initialize_request(id: u64, protocol_version: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": protocol_version,
            "capabilities": {},
            "clientInfo": { "name": "sqarvo-mcp-e2e", "version": "0.0.0" }
        }
    })
}

/// `notifications/initialized`。
pub fn initialized_notification() -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} })
}

/// 握手：initialize（断言协商回显 `expected_version`）→ initialized 通知。
pub fn handshake(server: &mut Server, request_version: &str, expected_version: &str, id: u64) {
    server.send(&initialize_request(id, request_version));
    let response = server.next_message();
    assert_eq!(response["id"], id, "initialize 响应 id 不符：{response}");
    assert_eq!(
        response["result"]["protocolVersion"], expected_version,
        "协商结果与 spike 口径不符（请求 {request_version}）：{response}"
    );
    assert_eq!(response["result"]["serverInfo"]["name"], "sqarvo-mcp");
    server.notify("notifications/initialized", serde_json::json!({}));
}

/// 完成 `tools/call`（参数原样进 `arguments`），返回响应消息。
#[allow(clippy::needless_pass_by_value)] // 同 request/notify：arguments 传 json! 字面量。
pub fn call_tool(
    server: &mut Server,
    id: u64,
    name: &str,
    arguments: serde_json::Value,
    meta: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut params = serde_json::json!({ "name": name, "arguments": arguments });
    if let Some(meta) = meta {
        params["_meta"] = meta;
    }
    server.request(id, "tools/call", params)
}

/// 工具调用响应必须是协议级 `result`（非 JSON-RPC `error`），且 `isError` 符合预期。
#[allow(clippy::needless_pass_by_value)] // 同 request/notify：arguments 传 json! 字面量。
pub fn assert_call_result(message: &serde_json::Value, expect_error: bool) -> serde_json::Value {
    assert!(
        message.get("error").is_none(),
        "工具级结果不得是 JSON-RPC error：{message}"
    );
    let result = message["result"].clone();
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert_eq!(
        is_error, expect_error,
        "isError 不符预期（期望 {expect_error}）：{message}"
    );
    result
}

// ---------------------------------------------------------------------------
// 测试夹具：mcp.json 与审计文件收集
// ---------------------------------------------------------------------------

/// 写一份 mcp.json，返回路径。
pub fn write_config(dir: &Path, root: &serde_json::Value) -> std::path::PathBuf {
    let path = dir.join("mcp.json");
    std::fs::write(&path, root.to_string()).expect("写入 mcp.json");
    path
}

/// 单个审计文件目录里的全部 JSONL 文件（活动文件 + `audit.N.jsonl` + `audit.<pid>.jsonl`）。
///
/// 审计文件名由 `AuditWriter` 以小写构造生成，扩展名比较无需大小写不敏感。
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn audit_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("审计目录 {} 不可读：{error}", dir.display()))
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with("audit.") && name.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    files
}

/// 读取并解析全部审计行；任何半行 JSON 都在这里炸掉（带文件名定位）。
pub fn read_audit_records(dir: &Path) -> Vec<(std::path::PathBuf, serde_json::Value)> {
    let mut records = Vec::new();
    for file in audit_files(dir) {
        let text = std::fs::read_to_string(&file).expect("读审计文件");
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|error| {
                panic!(
                    "{} 出现半行/交织行（期望完整 JSON）：{error}\n行原文：{line:?}",
                    file.display()
                )
            });
            records.push((file.clone(), value));
        }
    }
    records
}
