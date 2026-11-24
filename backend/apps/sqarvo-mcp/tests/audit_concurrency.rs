//! 审计并发与脱敏 E2E（#211 任务二；V3.1 §12 / §14.1「Audit」）。
//!
//! 三个层次：
//! 1. **双 server 子进程共享同一 `XDG_DATA_HOME`**（§12 多实例口径）：各自跑
//!    `tools/call`，合并 `audit.jsonl` + `audit.<pid>.jsonl` 断言每行完整 JSON、
//!    无交织残行、成功/失败记录字段齐全、fingerprint 归一化、失败记录不含
//!    原始 SQL 与假密码字面量；
//! 2. **flock 争用退化路径**：测试进程持有活动文件锁，子进程必须整批退化到
//!    `audit.<pid>.jsonl`（audit.rs `lock_target` 的既定行为，E2E 级钉死）；
//! 3. **轮转注入**（单进程）：`AuditWriter::open` + `RotationPolicy::new` 是公开
//!    构造器（audit.rs:208 / audit.rs:73），阈值可注入——用临时目录验证
//!    超限轮转、历史文件数上限、只在完整记录边界轮转，外加同目录双写入器并发。
//!
//! 密码只可能以 SQL 字面量形式进入这条 E2E 路径（密钥环凭据不能安全地喂给
//! 子进程，见 #211 报告「未实测」节）；审计行同时钉「不含原始 SQL」与
//! 「不含密码字面量」两条脱敏不变量。

// The binary tests rely on XDG isolation for the real shared profile store.
#![cfg(target_os = "linux")]

#[path = "support/profile_access.rs"]
mod profile_access;

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::Arc;

use sqarvo_common::QueryId;
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::ExecuteRequest;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::MemoryCredentialStore;
use sqarvo_mcp::audit::{AuditRecord, AuditWriter, DATA_SUBDIR, RotationPolicy};

mod common;
use common::{DRAFT_VERSION, audit_dir, call_tool, draft_meta, write_config};

/// 只出现在失败 SQL 里的假密码；任何审计文件都不允许出现该字面量。
const FAKE_PW: &str = "FAKE-PW-6f2b9e-not-real";

/// 凭据引用（合法 v4 UUID 形状的**不存在**引用：子进程路径上必然连接失败）。
const MISSING_CREDENTIAL_REF: &str = "sqarvo:conn/11111111-1111-4111-8111-111111111111";

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 用真实驱动直接建一个带数据的 SQLite 文件（布置数据用，绕过只读策略）。
fn seed_sqlite_db(db: &Path) {
    let config_json = serde_json::json!({
        "connections": [{
            "alias": "dev",
            "driver_id": "sqlite",
            "host": db.display().to_string(),
            "port": 0,
            "enabled": true,
            "properties": { "read_only": "false" }
        }]
    });
    let config = sqarvo_mcp::config::parse(&config_json.to_string())
        .expect("解析夹具配置")
        .validate()
        .expect("校验夹具配置");
    let registry = Arc::new(DriverRegistry::with_builtin_drivers());
    let credentials = Arc::new(MemoryCredentialStore::new());
    let sessions = Arc::new(SessionManager::with_credential_store(registry, credentials));
    let access = profile_access::service(config, sessions);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("夹具 runtime");
    runtime.block_on(async {
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, api_key TEXT)",
            "INSERT INTO users (id, api_key) VALUES (1, 'k-1'), (2, 'k-2')",
        ] {
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
                .expect("夹具数据布置成功");
        }
        // 收尾会话：释放文件句柄，避免子进程读到半个 db 文件。
        access.close_all().await;
    });
    drop(runtime);
}

/// 双进程共用的 mcp.json：可读 sqlite + 必然失败的 mariadb 连接。
fn shared_config(dir: &Path, db: &Path) -> std::path::PathBuf {
    write_config(
        dir,
        &serde_json::json!({
            "connections": [
                {
                    "alias": "dev",
                    "driver_id": "sqlite",
                    "host": db.display().to_string(),
                    "port": 0,
                    "enabled": true,
                    "properties": { "read_only": "true" }
                },
                {
                    "alias": "broken-mariadb",
                    "driver_id": "mariadb",
                    "host": "127.0.0.1",
                    "port": 53306,
                    "database": "sqarvo_test_22",
                    "username": "sqarvo_test",
                    "credential_ref": MISSING_CREDENTIAL_REF,
                    "enabled": true
                }
            ]
        }),
    )
}

/// 每个进程发一轮同样的调用（id 偏移避免合并时混淆 `request_id`）。
///
/// 每个进程 6 次 `tools/call`（成功 2 + 只读违规 1 + 执行失败 1 + 连接失败 1
/// + 状态读取 1）+ 握手一轮；EOF 后 legacy 会话必须 exit 0。
fn drive_one_process(server: &mut common::Server, id_base: u64) {
    common::handshake(server, "2025-11-25", "2025-11-25", id_base);
    // 返回值刻意不检查：工具级失败（isError）也是合法 result，这里只钉审计落盘。
    call_tool(
        server,
        id_base + 1,
        "list_connections",
        serde_json::json!({}),
        None,
    );
    call_tool(
        server,
        id_base + 2,
        "execute_readonly_query",
        serde_json::json!({"connection_alias": "dev", "sql": "SELECT id FROM users WHERE id = 1"}),
        None,
    );
    call_tool(
        server,
        id_base + 3,
        "execute_readonly_query",
        // 与上一条同形不同字面量：审计 fingerprint 必须一致（规范化口径）。
        serde_json::json!({"connection_alias": "dev", "sql": "SELECT id FROM users WHERE id = 2"}),
        None,
    );
    call_tool(
        server,
        id_base + 4,
        "execute_readonly_query",
        serde_json::json!({"connection_alias": "dev", "sql": "DELETE FROM users WHERE id = 1"}),
        None,
    );
    call_tool(
        server,
        id_base + 5,
        "execute_readonly_query",
        serde_json::json!({
            "connection_alias": "dev",
            "sql": format!("SELECT api_key FROM ghost_table WHERE token = '{FAKE_PW}'")
        }),
        None,
    );
    call_tool(
        server,
        id_base + 6,
        "list_databases",
        serde_json::json!({"connection_alias": "broken-mariadb"}),
        None,
    );
    server.close_stdin();
}

/// 审计记录必备键（V3.1 §12 清单，序列化时恒出现——Option 落成 null）。
const AUDIT_KEYS: [&str; 13] = [
    "timestamp",
    "request_id",
    "client_info",
    "tool_name",
    "connection_alias",
    "database",
    "sql_fingerprint",
    "execution_time_ms",
    "returned_rows",
    "truncated",
    "success",
    "error_code",
    "error_subcode",
];

fn assert_complete_fields(record: &serde_json::Value, file: &Path) {
    for key in AUDIT_KEYS {
        assert!(
            record.get(key).is_some(),
            "{} 的记录缺审计字段 {key}：{record}",
            file.display()
        );
    }
    assert!(
        record["timestamp"]
            .as_str()
            .is_some_and(|t| t.contains('T')),
        "timestamp 必须是 RFC 3339：{record}"
    );
    assert!(
        record["request_id"].as_str().is_some_and(|s| !s.is_empty()),
        "request_id 必须非空：{record}"
    );
    assert!(record["success"].is_boolean(), "{record}");
    if record["success"] == false {
        assert!(
            record["error_code"].is_string() && record["error_subcode"].is_string(),
            "失败记录必须带分类与稳定子码：{record}"
        );
    } else {
        assert!(
            record["error_code"].is_null() && record["error_subcode"].is_null(),
            "成功记录不得残留错误字段：{record}"
        );
    }
}

/// 扫描全部审计文件原文，断言脱敏不变量。
fn assert_redaction_invariants(dir: &Path) {
    for file in common::audit_files(dir) {
        let text = std::fs::read_to_string(&file).expect("读审计文件");
        // 密码字面量（假密码 + 夹具用户名/库名以外的敏感串都不许出现）。
        assert!(
            !text.contains(FAKE_PW),
            "{} 泄漏了密码字面量：{text}",
            file.display()
        );
        // 原始 SQL 文本（fingerprint 必须替代原文）。
        for fragment in ["SELECT id FROM users", "DELETE FROM users", "ghost_table"] {
            assert!(
                !text.contains(fragment),
                "{} 泄漏了原始 SQL 片段 {fragment:?}：{text}",
                file.display()
            );
        }
        // 每个文件必须以完整行结束（半行 = 交织残行的直接证据）。
        assert!(
            text.is_empty() || text.ends_with('\n'),
            "{} 尾部存在半行（疑似交织残行）",
            file.display()
        );
    }
}

// ---------------------------------------------------------------------------
// 1) 双进程共享审计目录
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)] // E2E 用例天然线性长：断言就地展开比拆分更易读。
fn two_server_processes_merge_into_whole_jsonl_records() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = home.path().join("audit-shared.db");
    seed_sqlite_db(&db);
    let config = shared_config(home.path(), &db);

    common::enable_access(&config, home.path());
    let mut a = common::Server::spawn(&config, home.path());
    let mut b = common::Server::spawn(&config, home.path());
    drive_one_process(&mut a, 100);
    drive_one_process(&mut b, 200);
    let oa = a.wait();
    let ob = b.wait();
    for outcome in [&oa, &ob] {
        assert!(
            outcome.status.success(),
            "legacy 会话 EOF 必须 exit 0：{:?}\nstderr:\n{}",
            outcome.status,
            outcome.stderr
        );
        outcome.assert_protocol_purity();
    }

    let dir = audit_dir(home.path());
    let records = common::read_audit_records(&dir);
    // 每进程 6 次 tools/call → 共 12 条（tools/call 之外的消息不进审计）。
    assert_eq!(
        records.len(),
        12,
        "合并 audit.jsonl + audit.<pid>.jsonl 后必须恰有 12 条：{:?}",
        records
            .iter()
            .map(|(f, r)| (
                f.file_name().map(|n| n.to_string_lossy().into_owned()),
                r.clone()
            ))
            .collect::<Vec<_>>()
    );
    for (file, record) in &records {
        assert_complete_fields(record, file);
    }

    // request_id × tool_name 全枚举：两个进程的 6 次调用都必须留下记录。
    let mut pairs: Vec<(String, String)> = records
        .iter()
        .map(|(_, r)| {
            (
                r["request_id"].as_str().expect("request_id").to_owned(),
                r["tool_name"].as_str().expect("tool_name").to_owned(),
            )
        })
        .collect();
    pairs.sort();
    for base in [100_u64, 200] {
        for offset in 1_u64..=6 {
            let id = (base + offset).to_string();
            assert!(
                pairs.iter().any(|(rid, _)| rid == &id),
                "request_id {id}（进程 {base}）必须被审计：{pairs:?}"
            );
        }
    }
    // 同一 request_id 只出现一次（并发交织不允许复制或丢失）。
    let ids: Vec<&str> = records
        .iter()
        .map(|(_, r)| r["request_id"].as_str().unwrap())
        .collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        ids.len(),
        unique.len(),
        "request_id 出现重复（交织/重写？）：{ids:?}"
    );

    // 错误形态齐备：
    //  - 工具级禁用/策略拒绝与执行失败都有 error_code/error_subcode；
    //  - mariadb 假凭据连接失败的子码属于 credential.*。
    let subcodes: Vec<&str> = records
        .iter()
        .filter(|(_, r)| r["success"] == false)
        .map(|(_, r)| r["error_subcode"].as_str().unwrap_or_default())
        .collect();
    assert!(
        subcodes.contains(&"mcp.readonly_violation"),
        "DELETE 拒绝必须留下 mcp.readonly_violation：{subcodes:?}"
    );
    assert!(
        subcodes.iter().any(|s| s.starts_with("credential.")),
        "不存在凭据的 mariadb 连接必须留下 credential.* 子码：{subcodes:?}"
    );

    // fingerprint 归一化：同形不同字面量的两条成功记录 fingerprint 一致，
    // 且形如 `sha256:...`（不含可还原的原文）。
    let mut fingerprints: Vec<(&str, String)> = records
        .iter()
        .filter(|(_, r)| r["tool_name"] == "execute_readonly_query" && r["success"] == true)
        .map(|(_, r)| {
            (
                r["request_id"].as_str().unwrap(),
                r["sql_fingerprint"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    fingerprints.sort();
    assert_eq!(
        fingerprints.len(),
        4,
        "SELECT 成功记录每进程 2 条：{fingerprints:?}"
    );
    assert!(
        fingerprints[0].1.len() == 64
            && fingerprints[0]
                .1
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "fingerprint 必须是 64 位小写 hex（policy/fingerprint.rs 的 sha256_hex 口径）：{fingerprints:?}"
    );
    assert_eq!(
        fingerprints[0].1, fingerprints[1].1,
        "同形 SQL（仅字面量不同）的 fingerprint 必须一致"
    );

    // client_info：legacy 握手给了 clientInfo，审计必须落上（§12「实际可验证时」）。
    for (_, record) in &records {
        assert_eq!(
            record["client_info"], "sqarvo-mcp-e2e 0.0.0",
            "握手 clientInfo 必须进审计：{record}"
        );
    }

    assert_redaction_invariants(&dir);
}

// ---------------------------------------------------------------------------
// 2) flock 争用 → 整批退化 audit.<pid>.jsonl
// ---------------------------------------------------------------------------

/// 打开活动审计文件并持有 flock（模拟「另一实例正在写」）。
///
/// 与 `src/audit.rs` 的 `exclusive_lock` 同一手法（`#[allow(unsafe_code)]` +
/// SAFETY 注释），不引入新依赖。
#[allow(unsafe_code)] // 理由：单次 flock(2)，不触碰内存，fd 由本函数持有的 File 保证有效。
fn hold_audit_lock(dir: &Path) -> std::fs::File {
    std::fs::create_dir_all(dir).expect("建审计目录");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("audit.jsonl"))
        .expect("打开活动文件");
    // SAFETY: `file` 在本调用期间存活，`as_raw_fd()` 有效；`flock` 只改该 fd
    // 关联 inode 的锁状态，无其它可观察副作用。
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "测试进程应取得独占锁");
    file
}

#[test]
fn held_flock_forces_pid_sidecar_for_server_process() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = home.path().join("sidecar.db");
    seed_sqlite_db(&db);
    let config = shared_config(home.path(), &db);
    let dir = audit_dir(home.path());

    let held = hold_audit_lock(&dir);
    common::enable_access(&config, home.path());
    let mut server = common::Server::spawn(&config, home.path());
    let pid = server.pid();
    drive_one_process(&mut server, 300);
    let outcome = server.wait();
    drop(held);

    assert!(
        outcome.status.success(),
        "争用退化不得影响 EOF 干净退出：stderr:\n{}",
        outcome.stderr
    );
    // 退化路径必须可见（audit.rs lock_target 的 warn）。
    assert!(
        outcome
            .stderr
            .contains("This process will use a separate file"),
        "stderr 必须说明退化到独立文件：\n{}",
        outcome.stderr
    );

    let sidecar = dir.join(format!("audit.{pid}.jsonl"));
    assert!(sidecar.exists(), "争用必须产生 {}", sidecar.display());
    let text = std::fs::read_to_string(&sidecar).expect("读退化文件");
    let mut count = 0;
    for line in text.lines() {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("退化文件每行必须是完整 JSON");
        assert_complete_fields(&record, &sidecar);
        count += 1;
    }
    assert_eq!(count, 6, "该进程的全部审计记录必须落在退化文件");
    // 活动文件没有被这个进程写过（flock 拿不到就整批走 sidecar）。
    let active = std::fs::read_to_string(dir.join("audit.jsonl")).expect("读活动文件");
    assert!(active.is_empty(), "活动文件不应被争用方追加：{active:?}");
    // 权限纪律同样覆盖退化文件。
    let mode = std::fs::metadata(&sidecar)
        .expect("stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "sidecar 必须 0600");

    assert_redaction_invariants(&dir);
}

// ---------------------------------------------------------------------------
// 3) 轮转注入（单进程，公开构造器：RotationPolicy::new）
// ---------------------------------------------------------------------------

fn synthetic_record(index: usize) -> AuditRecord {
    AuditRecord::new(format!("req-{index}"), "execute_readonly_query")
        .with_client(Some("synthetic-client 1.0".to_owned()))
        .with_target(Some("dev"), Some("app_db"))
        .with_fingerprint(Some("sha256:cafebabedeadbeef"))
        .with_elapsed(std::time::Duration::from_millis(7))
        .with_rows(3, false)
}

#[tokio::test]
async fn injected_rotation_keeps_history_capped_and_lines_whole() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 每行 ~260B；阈值 700 ≈ 2 行多；历史 2 份。
    let writer = AuditWriter::open(dir.path(), RotationPolicy::new(700, 2)).expect("打开");
    for index in 0..12 {
        writer.write(&synthetic_record(index)).await.expect("写入");
    }

    let mcp_dir = dir.path().join(DATA_SUBDIR);
    let files = common::audit_files(&mcp_dir);
    // 历史文件数上限：audit.jsonl + audit.1..audit.keep。
    let history = files
        .iter()
        .filter(|f| {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            name != "audit.jsonl"
        })
        .count();
    assert!(history <= 2, "历史文件数 {history} 不得超过 keep_files=2");

    // 只在完整记录边界：任何文件任何行都是完整 JSON；无半行尾巴。
    let mut retained: Vec<usize> = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("读");
        assert!(
            text.is_empty() || text.ends_with('\n'),
            "{} 尾部半行",
            file.display()
        );
        for line in text.lines() {
            let record: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("{} 半行记录（轮转切坏）：{error}\n{line:?}", file.display())
            });
            retained.push(
                record["request_id"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("req-")
                    .parse()
                    .expect("序号"),
            );
        }
    }
    retained.sort_unstable();
    let unique_len = {
        let mut u = retained.clone();
        u.dedup();
        u.len()
    };
    assert_eq!(retained.len(), unique_len, "同一记录不得出现在多份文件");
    let expected: Vec<usize> = (12 - unique_len..12).collect();
    assert_eq!(
        retained, expected,
        "保留的必须是最新记录的连续后缀（只丢最旧整份文件）"
    );
    assert!(expected.contains(&11), "最新记录必须仍在：{retained:?}");

    // 活动文件尺寸受阈值约束（允许「阈值 + 最后一行」的边界越量，禁无界增长）。
    let active_size = std::fs::metadata(writer.path()).expect("stat").len();
    assert!(
        active_size <= 700 + 512,
        "活动文件 {active_size} 字节远超阈值约束"
    );
}

#[tokio::test]
async fn concurrent_writers_in_one_process_never_interleave_partial_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 两个独立 AuditWriter 实例 = 两条 fd；同进程跨 fd 的 flock 也会互相争用，
    // 模拟最坏的并发形状（4 任务 × 10 记录，共享同一目录）。
    let mcp_dir = {
        let probe = AuditWriter::open(dir.path(), RotationPolicy::default()).expect("建目录");
        probe.dir().to_path_buf()
    };
    let writers: Vec<AuditWriter> = (0..2)
        .map(|_| AuditWriter::open(dir.path(), RotationPolicy::default()).expect("打开"))
        .collect();

    let mut handles = Vec::new();
    for (slot, writer) in writers.into_iter().enumerate() {
        for task in 0..4 {
            let writer = writer.clone();
            handles.push(tokio::spawn(async move {
                for index in 0..10 {
                    let n = slot * 40 + task * 10 + index;
                    writer.write(&synthetic_record(n)).await.expect("写入");
                }
            }));
        }
    }
    for handle in handles {
        handle.await.expect("写任务不得 panic");
    }

    let records = common::read_audit_records(&mcp_dir);
    assert_eq!(records.len(), 80, "80 条并发写入必须一条不少、一行不碎");
    let mut ids: Vec<usize> = Vec::new();
    for (_, record) in &records {
        assert_complete_fields(record, &mcp_dir);
        ids.push(
            record["request_id"]
                .as_str()
                .unwrap()
                .trim_start_matches("req-")
                .parse()
                .expect("序号"),
        );
    }
    ids.sort_unstable();
    let mut dedup = ids.clone();
    dedup.dedup();
    assert_eq!(ids, dedup, "并发写不得重复落同一条记录");
    assert_eq!(dedup.len(), 80);
}

// ---------------------------------------------------------------------------
// 2026 每请求 _meta 会话同样进审计（协议面 × 审计面交叉钉一次）
// ---------------------------------------------------------------------------

#[test]
fn draft_meta_bare_call_is_audited_with_request_id_and_client_meta() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = home.path().join("meta-audit.db");
    seed_sqlite_db(&db);
    let config = shared_config(home.path(), &db);

    common::enable_access(&config, home.path());
    let mut server = common::Server::spawn(&config, home.path());
    // 完全不握手，只发带 2026 _meta 的裸调用（协议面见 protocol_matrix.rs）。
    let meta = draft_meta(DRAFT_VERSION);
    call_tool(
        &mut server,
        77,
        "list_connections",
        serde_json::json!({}),
        Some(meta),
    );
    server.close_stdin();
    let _ = server.wait(); // handshakeless 会话退出码当前非零（缺口归协议矩阵钉）

    let dir = audit_dir(home.path());
    let records = common::read_audit_records(&dir);
    assert_eq!(records.len(), 1, "裸调用也必须进审计：{records:?}");
    let (_, record) = &records[0];
    assert_complete_fields(record, &dir.join("audit.jsonl"));
    assert_eq!(record["request_id"], "77");
    assert_eq!(record["tool_name"], "list_connections");
    assert_eq!(record["success"], true);
}
