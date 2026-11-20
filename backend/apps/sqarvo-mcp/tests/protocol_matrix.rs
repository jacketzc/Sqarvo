//! 协议兼容矩阵 E2E（#211 任务一；V3.1 §4.1 / §13 / §14.4）。
//!
//! 行为基准：`scratch/mcp-spike-rmcp/REPORT.md`（rmcp 3.10.0 实测）——
//! 旧握手三版本协商回显、2026-07-28 `server/discover` + 每请求 `_meta`
//! 双通道、缺 `_meta` → `-32602`、不支持版本 → `-32022`、
//! `initialize(2026-07-28)` 协商降为 `2025-11-25`。
//!
//! 每条流程都以子进程 + 行级 JSON-RPC 驱动真实二进制，公共骨架见
//! `tests/common/mod.rs`。行为与规范有偏差的地方曾在 #211 里「固化当前行为」；
//! #220 修复两处 src 缺陷后，相应用例已翻转为**正向回归钉死**（握手前 EOF /
//! 纯 discover 会话 exit 0；见文件尾部与 (a) 格），其余断言继续钉规范口径。

// The binary tests rely on XDG isolation for the real shared profile store.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};

mod common;
use common::{
    assert_call_result, call_tool, draft_meta, handshake, initialize_request,
    initialized_notification, meta_without_version, write_config,
};

/// 最新草案版本（与 harness 同源）。
use common::DRAFT_VERSION;

/// 一个不需要触碰数据库的 sqlite 连接档案（懒连接：矩阵里没有任何步骤建连）。
fn matrix_config(dir: &Path) -> PathBuf {
    write_config(
        dir,
        &serde_json::json!({
            "connections": [{
                "alias": "dev",
                "driver_id": "sqlite",
                "host": dir.join("matrix.db").display().to_string(),
                "port": 0,
                "enabled": true,
                "properties": { "read_only": "true" }
            }]
        }),
    )
}

/// 旧握手全流程（§14.4 矩阵行）：
/// initialize→initialized→`tools/list`(8)→`tools/call list_connections`
/// →`resources/list`→`resources/templates/list`→`resources/read`→EOF exit 0。
///
/// 每一步都断言协商回显版本、消息形状与 stdout 纯净性（每行合法 JSON-RPC）。
#[allow(clippy::too_many_lines)] // E2E 矩阵步骤天然线性长：逐格断言就地展开最可读。
fn run_legacy_matrix_session(protocol_version: &str) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());

    // 1) initialize：请求哪个已知版本就回显哪个（spike §3 协商口径）。
    handshake(&mut server, protocol_version, protocol_version, 1);

    // 2) tools/list：8 个 V1 工具，名字与 V1_TOOL_NAMES 一一对应。
    let tools = server.request(2, "tools/list", serde_json::json!({}));
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .expect("tools 数组")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    let mut expected: Vec<String> = sqarvo_mcp::config::tool_names()
        .map(str::to_owned)
        .collect();
    expected.sort();
    assert_eq!(
        sorted, expected,
        "{protocol_version} 会话的 tools/list：{tools}"
    );
    for tool in tools["result"]["tools"].as_array().expect("tools") {
        assert!(
            tool.get("inputSchema").is_some(),
            "每个工具必须有 inputSchema：{tool}"
        );
        assert!(
            tool["description"].as_str().is_some_and(|s| !s.is_empty()),
            "每个工具必须有非空 description：{tool}"
        );
    }

    // 3) tools/call list_connections：不触网的状态读取（V3.1 §5.1）。
    let response = call_tool(
        &mut server,
        3,
        "list_connections",
        serde_json::json!({}),
        None,
    );
    let result = assert_call_result(&response, false);
    assert_eq!(result["structuredContent"]["count"], 1, "{response}");
    assert_eq!(
        result["structuredContent"]["connections"][0]["alias"],
        "dev"
    );
    assert_eq!(
        result["structuredContent"]["connections"][0]["driver_id"],
        "sqlite"
    );
    assert_eq!(
        result["structuredContent"]["connections"][0]["connection_status"], "not_connected",
        "list_connections 不得建立连接（懒连接口径）：{response}"
    );
    assert!(
        response.to_string().find("matrix.db").is_none(),
        "响应不得回显连接串/文件路径：{response}"
    );

    // 4) resources/list + resources/templates/list（#210 四类资源）。
    let resources = server.request(4, "resources/list", serde_json::json!({}));
    let uris: Vec<&str> = resources["result"]["resources"]
        .as_array()
        .expect("resources 数组")
        .iter()
        .map(|r| r["uri"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        uris,
        vec![sqarvo_mcp::resources::CONNECTIONS_URI],
        "{resources}"
    );
    let templates = server.request(5, "resources/templates/list", serde_json::json!({}));
    let template_uris: Vec<&str> = templates["result"]["resourceTemplates"]
        .as_array()
        .expect("templates 数组")
        .iter()
        .map(|t| t["uriTemplate"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        template_uris,
        vec![
            sqarvo_mcp::resources::OVERVIEW_TEMPLATE,
            sqarvo_mcp::resources::SCHEMA_TEMPLATE,
            sqarvo_mcp::resources::TABLE_TEMPLATE
        ],
        "{templates}"
    );

    // 5) resources/read database://connections：结构化 JSON 文本。
    let read = server.request(
        6,
        "resources/read",
        serde_json::json!({ "uri": sqarvo_mcp::resources::CONNECTIONS_URI }),
    );
    let contents = read["result"]["contents"]
        .as_array()
        .expect("contents 数组");
    assert_eq!(contents.len(), 1, "{read}");
    assert_eq!(contents[0]["mimeType"], sqarvo_mcp::resources::MIME_TYPE);
    let text = contents[0]["text"].as_str().expect("text 字段");
    let payload: serde_json::Value = serde_json::from_str(text).expect("资源文本必须是合法 JSON");
    assert_eq!(payload["connections"][0]["alias"], "dev", "{payload}");

    // 6) EOF → exit 0（spike §6），stdout 全程只有协议行。
    server.close_stdin();
    let outcome = server.wait();
    assert!(
        outcome.status.success(),
        "{protocol_version} 会话 EOF 必须 exit 0：{:?}\nstderr:\n{}",
        outcome.status,
        outcome.stderr
    );
    outcome.assert_protocol_purity();
    // initialize + tools/list + tools/call + resources/list + templates + read = 6 行。
    assert_eq!(outcome.stdout.len(), 6, "{:#?}", outcome.stdout);
}

#[test]
fn legacy_handshake_2024_11_05_full_matrix() {
    run_legacy_matrix_session("2024-11-05");
}

#[test]
fn legacy_handshake_2025_03_26_full_matrix() {
    run_legacy_matrix_session("2025-03-26");
}

#[test]
fn legacy_handshake_2025_11_25_full_matrix() {
    run_legacy_matrix_session("2025-11-25");
}

// ---------------------------------------------------------------------------
// 2026-07-28 双通道（spike REPORT §4 的 (a)…(e) 五格）
// ---------------------------------------------------------------------------

/// (a) `server/discover` + `_meta`：协商能力探测，无 initialize。
#[test]
fn draft_discover_with_meta_reports_supported_versions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());

    let discover = server.request(
        1,
        "server/discover",
        serde_json::json!({ "_meta": draft_meta(DRAFT_VERSION) }),
    );
    let supported: Vec<&str> = discover["result"]["supportedVersions"]
        .as_array()
        .expect("supportedVersions 必须是数组")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    assert!(
        supported.contains(&DRAFT_VERSION),
        "supportedVersions 必须包含 {DRAFT_VERSION}：{discover}"
    );
    assert!(
        supported.contains(&"2025-11-25") && supported.contains(&"2024-11-05"),
        "旧版本也应在支持列表里：{discover}"
    );
    assert!(discover["result"]["capabilities"].is_object(), "{discover}");

    server.close_stdin();
    let outcome = server.wait();
    outcome.assert_protocol_purity();
    // #220 回归钉死（原 #211「缺陷固化」）：discover 是 bootstrap 探针，不进入
    // initialize 会话；EOF 后 rmcp `serve()` 报 `connection closed: initialize request`
    // 属「握手前对端关闭」，V3.1 §13 口径必须按 EOF 正常收尾（exit 0）。
    // 修复前该会话以非零码退出；再次非零 = 回归（server.rs `run_stdio` 判据）。
    assert!(
        outcome.status.success(),
        "纯 discover 会话 EOF 必须 exit 0（#220 修复回归）：{:?}\nstderr:\n{}",
        outcome.status,
        outcome.stderr
    );
    assert!(
        outcome
            .stderr
            .contains("客户端在握手前断开（connection closed: initialize request）"),
        "stderr 必须记录握手前断开的收尾说明：\n{}",
        outcome.stderr
    );
}

/// (b) 完全裸请求：不 initialize、不 discover，每请求带合法 `_meta` 即可全流程。
///
/// 实测修正（相对 spike REPORT §4「运维语义注记」的笼统口径）：**多请求**的
/// draft `_meta` 会话在 EOF 后走正常 `quit_reason=Closed` 收尾并 exit 0
/// ——spike 的非零退出只发生在「discover/裸拒绝后仍停在 initialize 等待」的
/// 会话（#220 修复后与 (a)、`stdin_close_before_handshake_exits_zero_as_clean_eof`
/// 一起按 exit 0 钉死；本格从未走过错误路径，继续正向钉死）。
#[test]
fn bare_requests_with_draft_meta_work_without_any_handshake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());
    let meta = draft_meta(DRAFT_VERSION);

    let tools = server.request(
        1,
        "tools/list",
        serde_json::json!({ "_meta": meta.clone() }),
    );
    let listed = tools["result"]["tools"].as_array().expect("tools 数组");
    assert_eq!(
        listed.len(),
        sqarvo_mcp::config::tool_names().count(),
        "裸请求也必须拿到全部 8 工具：{tools}"
    );
    // spike §4：2026 会话响应带缓存字段（server.rs 按协议版本条件填）。
    assert_eq!(tools["result"]["ttlMs"], 0, "{tools}");
    assert_eq!(tools["result"]["cacheScope"], "public", "{tools}");

    let response = call_tool(
        &mut server,
        2,
        "list_connections",
        serde_json::json!({}),
        Some(meta.clone()),
    );
    let result = assert_call_result(&response, false);
    assert_eq!(result["structuredContent"]["count"], 1, "{response}");

    let read = server.request(
        3,
        "resources/read",
        serde_json::json!({
            "uri": sqarvo_mcp::resources::CONNECTIONS_URI,
            "_meta": meta,
        }),
    );
    assert_eq!(
        read["result"]["contents"][0]["mimeType"],
        sqarvo_mcp::resources::MIME_TYPE,
        "{read}"
    );

    server.close_stdin();
    let outcome = server.wait();
    outcome.assert_protocol_purity();
    // 实测行为：多请求 draft 会话进 initialized 状态 → EOF 干净退出。
    assert!(
        outcome.status.success(),
        "多请求 draft `_meta` 会话 EOF 应 exit 0（实测口径）：{:?}\nstderr:\n{}",
        outcome.status,
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("MCP 会话已全部关闭") || outcome.stderr.contains("会话结束"),
        "应有正常收尾日志：\n{}",
        outcome.stderr
    );
}

/// (c) 裸请求缺 `_meta` / `_meta` 缺必需键 → `-32602`，消息点名缺失键。
#[test]
fn bare_request_without_meta_is_rejected_with_missing_key_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());

    // 完全没有 _meta：两个必需键都要被点名（rmcp handler/server.rs:125 口径）。
    let message = server.request(1, "tools/list", serde_json::json!({}));
    let error = &message["error"];
    assert_eq!(
        error["code"], -32602,
        "缺 _meta 必须是 invalid_params：{message}"
    );
    let text = error["message"].as_str().expect("错误消息");
    assert!(
        text.contains("io.modelcontextprotocol/protocolVersion")
            && text.contains("io.modelcontextprotocol/clientCapabilities"),
        "消息必须列出全部缺失键：{text}"
    );

    // 只缺 protocolVersion：消息只点该键，且不得再列已提供的键。
    let message = server.request(
        2,
        "tools/list",
        serde_json::json!({ "_meta": meta_without_version() }),
    );
    let error = &message["error"];
    assert_eq!(error["code"], -32602, "残缺 _meta 同样拒绝：{message}");
    let text = error["message"].as_str().expect("错误消息");
    assert!(
        text.contains("io.modelcontextprotocol/protocolVersion"),
        "消息必须点名 protocolVersion：{text}"
    );

    server.close_stdin();
    let outcome = server.wait();
    outcome.assert_protocol_purity();
}

/// (d) `_meta.protocolVersion` 不支持（2099-01-01）→ `-32022` + data.supported。
#[test]
fn bare_request_with_unsupported_draft_version_returns_32022_with_supported_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());

    let message = server.request(
        1,
        "tools/list",
        serde_json::json!({ "_meta": draft_meta("2099-01-01") }),
    );
    let error = &message["error"];
    assert_eq!(error["code"], -32022, "不支持的版本必须 -32022：{message}");
    assert_eq!(error["message"], "Unsupported protocol version");
    let supported = error["data"]["supported"]
        .as_array()
        .expect("data.supported 必须是数组");
    assert!(
        supported.iter().any(|v| v.as_str() == Some(DRAFT_VERSION)),
        "data.supported 必须含当前最新版本：{error}"
    );
    assert_eq!(error["data"]["requested"], "2099-01-01", "{error}");

    server.close_stdin();
    let outcome = server.wait();
    outcome.assert_protocol_purity();
}

/// (e) `initialize(2026-07-28)` → 按 spike 结论协商降为 `2025-11-25`，全流程可用。
#[test]
fn initialize_draft_version_negotiates_down_to_2025_11_25() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());

    // SDK 有意设计：会答 initialize 的服务器不能自称 LATEST（spike §4 表第一行）。
    handshake(&mut server, DRAFT_VERSION, "2025-11-25", 1);

    let tools = server.request(2, "tools/list", serde_json::json!({}));
    assert_eq!(
        tools["result"]["tools"].as_array().expect("tools").len(),
        sqarvo_mcp::config::tool_names().count(),
        "降级会话仍应拿到全部 8 工具：{tools}"
    );
    // 降级后不是 2026 会话：不应播缓存字段（server.rs 的版本门控）。
    assert!(
        tools["result"].get("ttlMs").is_none() || tools["result"].get("cacheScope").is_none(),
        "2025-11-25 会话不得带 2026 缓存字段：{tools}"
    );

    let response = call_tool(
        &mut server,
        3,
        "list_connections",
        serde_json::json!({}),
        None,
    );
    assert_call_result(&response, false);

    let read = server.request(
        4,
        "resources/read",
        serde_json::json!({ "uri": sqarvo_mcp::resources::CONNECTIONS_URI }),
    );
    assert!(read.get("error").is_none(), "降级会话资源可读：{read}");

    server.close_stdin();
    let outcome = server.wait();
    assert!(
        outcome.status.success(),
        "initialize 会话 EOF 必须 exit 0：{:?}\nstderr:\n{}",
        outcome.status,
        outcome.stderr
    );
    outcome.assert_protocol_purity();
}

/// initialize 请求未知版本：不报错，回自身支持的最新握手版本（spike §4 附注）。
#[test]
fn initialize_unknown_version_falls_back_to_latest_with_initialize() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());
    // LATEST_WITH_INITIALIZE = 2025-11-25（rmcp 常量口径）。
    handshake(&mut server, "2099-01-01", "2025-11-25", 9);
    server.close_stdin();
    let outcome = server.wait();
    assert!(outcome.status.success(), "stderr:\n{}", outcome.stderr);
    outcome.assert_protocol_purity();
}

// ---------------------------------------------------------------------------
// 既有错误语义（§14.4「参数验证」+ spike §3/§4）
// ---------------------------------------------------------------------------

/// 未知工具名 → 协议级 `-32602` "tool not found"（spike §3 口径）；
/// 未知资源 URI → `-32002` + 结构化 `mcp.connection_not_found`；
/// 非法 scheme → `-32602` + `mcp.invalid_argument`。
#[test]
fn unknown_tool_and_resource_keep_pinned_protocol_semantics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());
    handshake(&mut server, "2025-11-25", "2025-11-25", 1);

    let message = server.request(
        2,
        "tools/call",
        serde_json::json!({ "name": "drop_database", "arguments": {} }),
    );
    assert_eq!(message["error"]["code"], -32602, "未知工具名：{message}");
    assert_eq!(message["error"]["message"], "tool not found");

    // alias 不存在：形状合法、资源不存在 → -32002（resources.rs 归一口径，
    // 与「存在但未授权」同形，不泄漏「配置里有这个 alias」）。
    let message = server.request(
        3,
        "resources/read",
        serde_json::json!({ "uri": "database://ghost/main/overview" }),
    );
    assert_eq!(message["error"]["code"], -32002, "未知资源 URI：{message}");
    assert_eq!(
        message["error"]["data"]["error"]["code"], "mcp.connection_not_found",
        "{message}"
    );

    // scheme 不符：URI 自身不合法 → -32602。
    let message = server.request(
        4,
        "resources/read",
        serde_json::json!({ "uri": "postgres://dev/main/overview" }),
    );
    assert_eq!(message["error"]["code"], -32602, "非法 scheme：{message}");
    assert_eq!(
        message["error"]["data"]["error"]["code"], "mcp.invalid_argument",
        "{message}"
    );

    server.close_stdin();
    let outcome = server.wait();
    assert!(outcome.status.success(), "stderr:\n{}", outcome.stderr);
    outcome.assert_protocol_purity();
}

// ---------------------------------------------------------------------------
// enabled_tools（#210 过滤）在协议面的投影
// ---------------------------------------------------------------------------

/// `enabled_tools=["list_connections"]`：`tools/list` 只播 1 个；
/// 被禁工具仍可路由，调用返回工具级 `isError` + `mcp.tool_disabled`
/// （server.rs:168-181 注释口径：不是 -32602「工具不存在」）。
/// 资源不受 `enabled_tools` 管辖（#210 文档口径），一并钉住。
#[test]
fn enabled_tools_filter_advertises_one_and_disabled_call_is_tool_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(
        dir.path(),
        &serde_json::json!({
            "connections": [{
                "alias": "dev",
                "driver_id": "sqlite",
                "host": dir.path().join("filter.db").display().to_string(),
                "port": 0,
                "enabled": true
            }],
            "enabled_tools": ["list_connections"]
        }),
    );
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());
    handshake(&mut server, "2025-11-25", "2025-11-25", 1);

    let tools = server.request(2, "tools/list", serde_json::json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        vec!["list_connections"],
        "#210 过滤后只播 1 个：{tools}"
    );

    // 被禁用的工具：协议层仍是 result，工具级 isError + mcp.tool_disabled。
    let response = call_tool(
        &mut server,
        3,
        "list_tables",
        serde_json::json!({ "connection_alias": "dev", "database": "main" }),
        None,
    );
    let result = assert_call_result(&response, true);
    assert_eq!(
        result["structuredContent"]["error"]["code"], "mcp.tool_disabled",
        "{response}"
    );
    assert_eq!(
        result["structuredContent"]["error"]["category"],
        "permission_denied"
    );
    let text = result["content"][0]["text"].as_str().expect("错误文案");
    assert!(
        text.contains("mcp.tool_disabled"),
        "文本也要带稳定子码：{text}"
    );

    // 资源不受 enabled_tools 管辖。
    let resources = server.request(4, "resources/list", serde_json::json!({}));
    assert!(
        resources["result"]["resources"]
            .as_array()
            .is_some_and(|r| !r.is_empty()),
        "禁用工具不得隐藏资源：{resources}"
    );

    server.close_stdin();
    let outcome = server.wait();
    assert!(outcome.status.success(), "stderr:\n{}", outcome.stderr);
    outcome.assert_protocol_purity();
}

// ---------------------------------------------------------------------------
// 信号与 EOF（§13 / spike §6）
// ---------------------------------------------------------------------------

/// SIGTERM（握手完成后）→ 优雅关停：exit 0 且 stderr 有收尾日志。
///
/// 已知限制（不为其写断言）：信号处理器在 `serve()` 返回后才注册，
/// 「握手前 SIGTERM」走系统默认处置（server.rs 模块文档「已知缺口」）。
#[cfg(unix)]
#[test]
fn sigterm_after_initialize_exits_zero_with_shutdown_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());
    handshake(&mut server, "2025-11-25", "2025-11-25", 1);

    server.send_sigterm();
    // 不再关闭 stdin：SIGTERM 自己必须让 serve 循环停下来。
    let outcome = server.wait();
    assert!(
        outcome.status.success(),
        "SIGTERM 必须走优雅关停（exit 0）而不是被信号杀死：{:?}\nstderr:\n{}",
        outcome.status,
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("收到退出信号，开始优雅关停"),
        "stderr 必须有收尾日志（server.rs:307）：\n{}",
        outcome.stderr
    );
    outcome.assert_protocol_purity();
    assert_eq!(outcome.stdout.len(), 1, "只应有 initialize 响应一行");
}

/// 未建立任何会话时 stdin 关闭 → **exit 0**（#220 修复的正向回归钉死）。
///
/// 规范口径（V3.1 §13 / spike §6）：EOF → 正常退出。修复前「握手前 EOF」会让
/// rmcp `serve()` 以 `connection closed: initialize request` 返回 Err，
/// `run_stdio` 把它误映射成 `MCP_INTERNAL` 并非零退出——「客户端连上却没发东西
/// 就断开」不是服务异常。判据（类型 `ConnectionClosed` + 消息含 `initialize`）
/// 见 `run_stdio` 文档；无握手 `_meta` 会话（矩阵 (b)）本就走 `Ok` 路径，不受影响。
/// 若此测试再见到非零退出码或收尾日志缺失，即为回归。
#[test]
fn stdin_close_before_handshake_exits_zero_as_clean_eof() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());

    server.close_stdin();
    let outcome = server.wait();
    assert!(
        outcome.status.success(),
        "握手前 EOF 必须按正常收尾 exit 0（#220 修复回归）：{:?}\nstderr:\n{}",
        outcome.status,
        outcome.stderr
    );
    assert!(
        outcome
            .stderr
            .contains("客户端在握手前断开（connection closed: initialize request）"),
        "stderr 必须记录握手前断开的收尾说明：\n{}",
        outcome.stderr
    );
    assert!(
        outcome.stdout.is_empty(),
        "启动即断开也不得向 stdout 泄漏半条非协议消息：{:#?}",
        outcome.stdout
    );
    outcome.assert_protocol_purity();
}

/// 握手后不发 EOF、不通信：进程必须持续存活（防止「响应了就退出」类回归）。
#[test]
fn server_stays_alive_after_initialize_until_stdin_closes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = matrix_config(dir.path());
    common::enable_access(&config, dir.path());
    let mut server = common::Server::spawn(&config, dir.path());
    server.send(&initialize_request(1, "2025-11-25"));
    let response = server.next_message();
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
    server.send(&initialized_notification());
    server.assert_no_more_output();

    // 静置 ~1s 后仍能正常应答 = 进程存活、会话可用。
    std::thread::sleep(std::time::Duration::from_millis(1_000));
    let tools = server.request(2, "tools/list", serde_json::json!({}));
    assert_eq!(
        tools["result"]["tools"].as_array().expect("tools").len(),
        sqarvo_mcp::config::tool_names().count(),
        "静置后 tools/list 必须仍可服务：{tools}"
    );

    server.close_stdin();
    let outcome = server.wait();
    assert!(outcome.status.success(), "stderr:\n{}", outcome.stderr);
    outcome.assert_protocol_purity();
    assert_eq!(outcome.stdout.len(), 2, "{:#?}", outcome.stdout);
}
