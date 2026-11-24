//! 调用审计（#207 实现；V3.1 §12）。
//!
//! 契约要点：
//! - `AppPaths::data_dir()` 下 `mcp/audit.jsonl`，0600，记录级落盘策略与轮转
//!   （10 MiB × 5，只在完整记录边界轮转）；
//! - 每行 JSON 字段：`timestamp` / `request_id` / `client_info`（仅协议实际提供
//!   且可验证时）/ `tool_name` / `connection_alias` / `database` /
//!   `sql_fingerprint`（见 [`crate::policy::SqlFingerprint`]）/
//!   `execution_time_ms` / `returned_rows` / `truncated` / `success` /
//!   `error_code` / `error_subcode`；
//! - 默认不记录：完整 SQL、查询结果、密码、Token、`credential_ref` 指向的秘密；
//! - 多实例共享文件为 best-effort：flock 争用退化 `audit.<pid>.jsonl`，
//!   已知限制写入架构文档。
//!
//! # 实现说明
//!
//! - **结构上不可能泄漏 SQL**：[`AuditRecord`] 只有 `sql_fingerprint` 字段（字符串），
//!   没有放原始 SQL / 结果集 / 凭据的字段，调用方没有「顺手多记一点」的入口。
//! - **写入是异步的**：文件操作与 flock 都走 `spawn_blocking`，不占 Tokio worker
//!   （AGENTS §6.4 / §11：不在 async 任务里做阻塞 I/O）。
//! - **轮转只在完整记录边界**：先序列化出完整一行，再比较「当前长度 + 本行长度」，
//!   超阈值就先轮转、再把整行写进新文件，绝不出现半行。
//! - **锁**：同进程用 `std::sync::Mutex` 串行化；跨进程用 `std::fs::File::try_lock`
//!   （Unix 落到 `flock(2)`、Windows 落到 `LockFileEx`）。拿不到锁时不阻塞、不丢记录，
//!   改写到 `audit.<pid>.jsonl`。这条退化路径**不是**多实例安全轮转的证明：
//!   两个实例各自轮转同一目录仍可能互相覆盖，见 ADR 0032「已知限制」。
//! - **文件权限**：Unix 以 0600 创建；目录由 [`AuditWriter::open`] 收紧到 0700
//!   （`sqarvo_local_store::restrict_to_owner`）。Windows 依赖用户数据目录的 ACL 继承，
//!   与现有端点文件策略一致。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use sqarvo_local_store::restrict_to_owner;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// 数据目录下的 MCP 专用子目录名。
pub const DATA_SUBDIR: &str = "mcp";
/// 活动审计文件名。
pub const AUDIT_FILE_NAME: &str = "audit.jsonl";
/// 默认单文件大小上限（10 MiB）。
pub const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// 默认保留的历史文件数量。
pub const DEFAULT_KEEP_FILES: usize = 5;

/// 轮转策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationPolicy {
    /// 单个活动文件的大小上限（字节）。
    pub max_file_bytes: u64,
    /// 保留的历史文件数量（`audit.1.jsonl` … `audit.N.jsonl`）。
    pub keep_files: usize,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            keep_files: DEFAULT_KEEP_FILES,
        }
    }
}

impl RotationPolicy {
    /// 显式策略（测试用较小阈值）。
    #[must_use]
    pub const fn new(max_file_bytes: u64, keep_files: usize) -> Self {
        Self {
            max_file_bytes,
            keep_files,
        }
    }
}

/// Write lifecycle events written without bound values or SQL.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePhase {
    /// Durable intent required before opening the transaction.
    Intent,
    /// The checked row count immediately before attempting commit.
    Prepared,
    /// The operation was rejected before its transaction started.
    NotStarted,
    /// The transaction was rolled back.
    RolledBack,
    /// The database acknowledged commit.
    Committed,
    /// The commit response did not establish an outcome.
    CommitUnknown,
    /// Rollback could not be confirmed.
    RollbackUnconfirmed,
    /// The tool calculated a preview without executing DML.
    Preview,
}

/// Additional JSONL fields for a write request.
#[derive(Debug, Clone, Serialize)]
pub struct WriteAudit {
    /// Validated target name, when available for the request.
    pub table: Option<String>,
    /// Static operation label.
    pub operation_type: &'static str,
    /// Direct DML row count, when the driver reported it.
    pub affected_rows: Option<u64>,
    /// Indicates whether UPDATE reports matched rows.
    pub counts_matched_rows: bool,
    /// Transaction state or audit lifecycle phase.
    pub transaction_state: WritePhase,
    /// Server-classified risk.
    pub risk_level: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TransactionAudit {
    pub transaction_id: uuid::Uuid,
    pub event: &'static str,
    pub phase: &'static str,
    pub operation: &'static str,
    pub affected_rows: Option<u64>,
    pub transaction_state: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DdlAudit {
    pub operation_id: Option<uuid::Uuid>,
    pub target_object: String,
    pub index: Option<String>,
    pub new_table: Option<String>,
    pub ddl_operation: &'static str,
    pub risk_level: &'static str,
    pub execution_phase: &'static str,
    pub ddl_state: &'static str,
}

/// 一条审计记录。
///
/// 字段与 V3.1 §12 的清单一一对应；`error_code` 放**分类**
/// （[`ErrorCategory::as_str`]，如 `permission_denied`），`error_subcode` 放
/// **稳定子码**（[`ErrorCode`]，如 `mcp.readonly_violation`）——两者分开才能在
/// 「按分类统计」与「按子码定位」两种用途上都不用二次映射。
#[derive(Debug, Clone, Serialize)]
pub struct AuditRecord {
    /// RFC 3339 UTC 时间戳。
    pub timestamp: String,
    /// MCP 请求 id（协议层原样，便于与客户端日志对齐）。
    pub request_id: String,
    /// 客户端名称/版本；只有协议确实提供了 peer info 时才填。
    pub client_info: Option<String>,
    /// 工具名。
    pub tool_name: String,
    /// 连接 alias。
    pub connection_alias: Option<String>,
    /// 数据库名。
    pub database: Option<String>,
    /// SQL 指纹（规范化 + SHA-256）；**永不**放原始 SQL。
    pub sql_fingerprint: Option<String>,
    /// 执行耗时（毫秒）。
    pub execution_time_ms: Option<u64>,
    /// 返回行数（截断后的实际行数）。
    pub returned_rows: Option<u64>,
    /// 结果级截断标记（行数/字节上限）；单元格级截断见 ADR 0007，不在这里。
    pub truncated: Option<bool>,
    /// 是否成功。
    pub success: bool,
    /// 错误分类（失败时）。
    pub error_code: Option<String>,
    /// Write-specific fields. Read audit records retain their existing format.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub write: Option<WriteAudit>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub(crate) transaction: Option<TransactionAudit>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub(crate) ddl: Option<DdlAudit>,
    /// 稳定错误子码（失败时）。
    pub error_subcode: Option<String>,
}

impl AuditRecord {
    /// 新记录的起点：时间戳取当前 UTC，`success = true`，其余留空。
    #[must_use]
    pub fn new(request_id: impl Into<String>, tool_name: impl Into<String>) -> Self {
        Self {
            timestamp: now_rfc3339(),
            request_id: request_id.into(),
            client_info: None,
            tool_name: tool_name.into(),
            connection_alias: None,
            database: None,
            sql_fingerprint: None,
            execution_time_ms: None,
            returned_rows: None,
            truncated: None,
            success: true,
            error_code: None,
            error_subcode: None,
            write: None,
            transaction: None,
            ddl: None,
        }
    }

    /// Attach write-only audit fields with a timestamp for the current event.
    #[must_use]
    pub fn with_write(mut self, write: WriteAudit) -> Self {
        self.timestamp = now_rfc3339();
        self.write = Some(write);
        self
    }

    /// 客户端信息（只在协议给出且可验证时填）。
    #[must_use]
    pub fn with_client(mut self, client_info: Option<String>) -> Self {
        self.client_info = client_info;
        self
    }

    /// 连接与数据库。
    #[must_use]
    pub fn with_target(mut self, alias: Option<&str>, database: Option<&str>) -> Self {
        self.connection_alias = alias.map(str::to_owned);
        self.database = database.map(str::to_owned);
        self
    }

    /// SQL 指纹（`crate::policy::SqlFingerprint` 的哈希字段）。
    #[must_use]
    pub fn with_fingerprint(mut self, fingerprint: Option<&str>) -> Self {
        self.sql_fingerprint = fingerprint.map(str::to_owned);
        self
    }

    /// 执行耗时。
    #[must_use]
    pub fn with_elapsed(mut self, elapsed: Duration) -> Self {
        self.execution_time_ms = Some(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
        self
    }

    /// 返回行数与结果级截断标记。
    #[must_use]
    pub fn with_rows(mut self, returned_rows: u64, truncated: bool) -> Self {
        self.returned_rows = Some(returned_rows);
        self.truncated = Some(truncated);
        self
    }

    /// 标记失败并带上分类与子码（文案不进审计）。
    #[must_use]
    pub fn failed(mut self, error: &Error) -> Self {
        self.success = false;
        self.error_code = Some(error.category().as_str().to_owned());
        self.error_subcode = Some(error.code().as_str().to_owned());
        self
    }
}

fn now_rfc3339() -> String {
    // `now_utc()` 永远落在 Rfc3339 可表示的年份区间内；失败分支仅为不 panic 的兜底。
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// JSONL 审计写入器。
///
/// `Clone` 很便宜（路径 + `Arc` 门闩），可以随意传进 `spawn_blocking`。
#[derive(Debug, Clone)]
pub struct AuditWriter {
    dir: PathBuf,
    current: PathBuf,
    policy: RotationPolicy,
    gate: Arc<Mutex<()>>,
}

impl AuditWriter {
    /// 打开（或创建）`dir/audit.jsonl`。
    ///
    /// 目录在这里就收紧到 0700：审计里会出现连接别名与库名，和端点文件同级敏感。
    /// 文件本身按需创建，因此「启动了但一次工具都没调用」不会留下空文件。
    pub fn open(data_dir: &Path, policy: RotationPolicy) -> Result<Self> {
        let dir = data_dir.join(DATA_SUBDIR);
        std::fs::create_dir_all(&dir).map_err(Error::from)?;
        restrict_to_owner(&dir)?;
        Ok(Self {
            current: dir.join(AUDIT_FILE_NAME),
            dir,
            policy,
            gate: Arc::new(Mutex::new(())),
        })
    }

    /// 活动文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.current
    }

    /// 目录路径。
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 轮转策略。
    #[must_use]
    pub const fn policy(&self) -> RotationPolicy {
        self.policy
    }

    /// 追加一条记录。
    ///
    /// 阻塞式文件 I/O 与 flock 都放进 `spawn_blocking`；写入失败**不**静默吞掉，
    /// 由调用方决定是继续工具调用还是在结果里标注（#209 的工具实现按「审计失败
    /// 要可见」处理）。
    pub async fn write(&self, record: &AuditRecord) -> Result<()> {
        let writer = self.clone();
        let record = record.clone();
        tokio::task::spawn_blocking(move || writer.append_line(&record))
            .await
            .map_err(|error| {
                Error::new(
                    ErrorCode::MCP_INTERNAL,
                    ErrorCategory::Internal,
                    format!("The audit write task ended unexpectedly: {error}"),
                )
            })?
    }

    fn append_line(&self, record: &AuditRecord) -> Result<()> {
        let line = serde_json::to_string(record).map_err(|error| {
            Error::new(
                ErrorCode::MCP_INTERNAL,
                ErrorCategory::Internal,
                format!("Failed to serialize the audit record: {error}"),
            )
        })?;
        let _guard = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let line = format!("{line}\n");
        // The stable lock remains held when the active log is renamed and reopened.
        // Keep the active-file lock as well for older stdio binaries and log readers.
        let rotation_lock = open_locked(&self.dir.join("audit.lock"))?;
        let (mut file, target) = match &rotation_lock {
            LockOutcome::Locked(_lock) => self.lock_target()?,
            LockOutcome::Contended => self.sidecar()?,
        };
        let size = file.metadata().map_err(Error::from)?.len();
        let needs_rotation = size > 0
            && self.policy.keep_files > 0
            && size.saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX))
                > self.policy.max_file_bytes;
        if needs_rotation {
            // 先在完整记录边界释放锁再改名：改名的是目录项，持有 fd 的锁会跟着
            // inode 走，边改边写会让「当前文件」和持有的锁指向不同对象。
            drop(file);
            rotate(&target, self.policy)?;
            // 轮转后是新文件：重新打开并尽力加锁（此时理论上没有竞争者持有它）。
            file = open_append(&target)?;
            let _ = exclusive_lock(&file)?;
        }
        file.write_all(line.as_bytes()).map_err(Error::from)?;
        file.flush().map_err(Error::from)?;
        if record.write.is_some() || record.transaction.is_some() || record.ddl.is_some() {
            file.sync_all().map_err(Error::from)?;
            #[cfg(unix)]
            File::open(&self.dir)
                .and_then(|dir| dir.sync_all())
                .map_err(Error::from)?;
        }
        Ok(())
    }

    /// 取「可写且已加锁」的文件；跨进程争用时退化到带 pid 的独立文件。
    fn lock_target(&self) -> Result<(File, PathBuf)> {
        match open_locked(&self.current)? {
            LockOutcome::Locked(file) => Ok((file, self.current.clone())),
            LockOutcome::Contended => self.sidecar(),
        }
    }

    fn sidecar(&self) -> Result<(File, PathBuf)> {
        let sidecar = self.dir.join(format!("audit.{}.jsonl", std::process::id()));
        tracing::warn!(
            "Another process is using the audit file. This process will use a separate file"
        );
        let file = open_append(&sidecar)?;
        let _ = exclusive_lock(&file)?;
        Ok((file, sidecar))
    }
}

/// 加锁尝试的结果。
enum LockOutcome {
    /// 已取得独占锁。
    Locked(File),
    /// 另一个进程正持有锁（不阻塞、不丢记录，交给调用方退化）。
    Contended,
}

/// 打开并尝试非阻塞独占锁。
fn open_locked(path: &Path) -> Result<LockOutcome> {
    let file = open_append(path)?;
    if exclusive_lock(&file)? {
        Ok(LockOutcome::Locked(file))
    } else {
        Ok(LockOutcome::Contended)
    }
}

/// 非阻塞独占锁，随文件关闭释放。
fn exclusive_lock(file: &File) -> Result<bool> {
    use sqarvo_local_store::file_lock::{LockMode, try_lock};

    try_lock(file, LockMode::Exclusive).map_err(Error::from)
}

/// 以 0600（Unix）打开追加文件。
///
/// `mode(0o600)` 只在**创建**时生效；已存在的文件（旧版本、手工创建、或被别的工具
/// 放宽过权限）额外收紧一次。收紧失败直接报错而不是继续写：审计里会出现连接别名与
/// 库名，写进一个别人可读的文件等于把访问拓扑交出去。
fn open_append(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(Error::from)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(Error::from)?;
    }
    Ok(file)
}

/// 轮转：`audit.jsonl → audit.1.jsonl`，旧的依次后移，超出 `keep_files` 的删除。
fn rotate(base: &Path, policy: RotationPolicy) -> Result<()> {
    if !base.exists() {
        return Ok(());
    }
    if policy.keep_files == 0 {
        std::fs::remove_file(base).map_err(Error::from)?;
        return Ok(());
    }
    if let Some(oldest) = rotated_path(base, policy.keep_files)
        && oldest.exists()
    {
        std::fs::remove_file(&oldest).map_err(Error::from)?;
    }
    for index in (1..policy.keep_files).rev() {
        let (Some(from), Some(to)) = (rotated_path(base, index), rotated_path(base, index + 1))
        else {
            continue;
        };
        if from.exists() {
            std::fs::rename(&from, &to).map_err(Error::from)?;
        }
    }
    if let Some(first) = rotated_path(base, 1) {
        std::fs::rename(base, first).map_err(Error::from)?;
    }
    Ok(())
}

fn rotated_path(base: &Path, index: usize) -> Option<PathBuf> {
    let stem = base.file_stem()?.to_string_lossy();
    Some(base.with_file_name(format!("{stem}.{index}.jsonl")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "sup3r-s3cret-pw";

    fn record(index: usize) -> AuditRecord {
        AuditRecord::new(format!("req-{index}"), "execute_readonly_query")
            .with_target(Some("dev"), Some("app_db"))
            .with_fingerprint(Some("sha256:deadbeef"))
            .with_elapsed(Duration::from_millis(12))
            .with_rows(200, true)
    }

    fn read_lines(path: &Path) -> Vec<String> {
        let text = std::fs::read_to_string(path).expect("读取审计文件");
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn record_has_the_documented_fields_and_no_raw_sql_slot() {
        let line = serde_json::to_string(&record(1)).expect("序列化");
        let value: serde_json::Value = serde_json::from_str(&line).expect("合法 JSON");
        for field in [
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
        ] {
            assert!(value.get(field).is_some(), "缺少审计字段 {field}: {line}");
        }
        assert!(
            value.get("sql").is_none(),
            "审计不得有存放原始 SQL 的字段：{line}"
        );
        assert!(
            value["timestamp"]
                .as_str()
                .unwrap_or_default()
                .contains('T')
        );
        assert_eq!(value["success"], true);
        assert_eq!(value["truncated"], true);
    }

    #[test]
    fn failure_records_carry_category_and_subcode_but_no_message() {
        let error = Error::new(
            ErrorCode::MCP_READONLY_VIOLATION,
            ErrorCategory::PermissionDenied,
            format!("示意文案：密码 {PASSWORD}"),
        );
        let record = AuditRecord::new("req-9", "execute_readonly_query").failed(&error);
        let line = serde_json::to_string(&record).expect("序列化");
        assert!(!record.success);
        assert_eq!(record.error_code.as_deref(), Some("permission_denied"));
        assert_eq!(
            record.error_subcode.as_deref(),
            Some("mcp.readonly_violation")
        );
        // 审计不存错误文案：文案可能含调用参数。
        assert!(!line.contains(PASSWORD));
        assert!(!line.contains("示意文案"));
    }

    #[tokio::test]
    async fn write_audit_adds_transaction_metadata_without_sql_or_values() {
        let dir = tempfile::tempdir().unwrap();
        let writer = AuditWriter::open(dir.path(), RotationPolicy::default()).unwrap();
        let entry = AuditRecord::new("write-1", "update_rows")
            .with_target(Some("fixture"), Some("main"))
            .with_write(WriteAudit {
                table: Some("items".into()),
                operation_type: "update",
                affected_rows: Some(4),
                counts_matched_rows: true,
                transaction_state: WritePhase::RolledBack,
                risk_level: "medium",
            });
        writer.write(&entry).await.unwrap();
        let lines = read_lines(writer.path());
        assert_eq!(lines.len(), 1);
        let result: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(result["request_id"], "write-1");
        assert_eq!(result["transaction_state"], "rolled_back");
        assert_eq!(result["affected_rows"], 4);
        assert_eq!(result["table"], "items");
        assert!(result["sql_fingerprint"].is_null());
        assert!(result.get("params").is_none());
    }

    #[tokio::test]
    async fn writes_one_json_object_per_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let writer = AuditWriter::open(dir.path(), RotationPolicy::default()).expect("打开");
        writer.write(&record(1)).await.expect("写入");
        writer.write(&record(2)).await.expect("写入");
        let lines = read_lines(writer.path());
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let value: serde_json::Value = serde_json::from_str(line).expect("每行都是完整 JSON");
            assert!(
                value["request_id"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("req-")
            );
        }
    }

    #[tokio::test]
    async fn rotates_only_at_complete_record_boundaries() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 每行约 250 字节；阈值设成能放 2 行左右，便于精确断言。
        let policy = RotationPolicy::new(600, 2);
        let writer = AuditWriter::open(dir.path(), policy).expect("打开");
        for index in 0..10 {
            writer.write(&record(index)).await.expect("写入");
        }
        assert!(writer.path().exists(), "活动文件始终存在");
        let first = dir.path().join(DATA_SUBDIR).join("audit.1.jsonl");
        let second = dir.path().join(DATA_SUBDIR).join("audit.2.jsonl");
        let third = dir.path().join(DATA_SUBDIR).join("audit.3.jsonl");
        assert!(first.exists(), "发生轮转");
        assert!(second.exists(), "保留第二份历史");
        assert!(!third.exists(), "超出 keep_files 的历史文件必须删除");

        // 轮转按设计丢弃**最旧**的整份文件，因此保留的记录必须是被写下序列的连续后缀，
        // 不能出现「中间缺一段」或「同一记录出现两次」。
        let mut retained: Vec<usize> = Vec::new();
        for path in [writer.path().to_path_buf(), first, second] {
            let lines = read_lines(&path);
            for line in &lines {
                let value: serde_json::Value = serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("{} 有半行记录：{error}", path.display()));
                let request_id = value["request_id"].as_str().expect("request_id");
                let index: usize = request_id
                    .trim_start_matches("req-")
                    .parse()
                    .expect("序号可解析");
                retained.push(index);
            }
        }
        assert!(
            (1..=6).contains(&retained.len()),
            "保留的记录数受 keep_files 与单文件容量约束：{retained:?}"
        );
        retained.sort_unstable();
        let unique = retained.len();
        retained.dedup();
        assert_eq!(retained.len(), unique, "同一记录不得出现在多份文件里");
        let expected: Vec<usize> = (10 - unique..10).collect();
        assert_eq!(retained, expected, "保留的是最新记录的连续后缀");
    }

    #[tokio::test]
    async fn keep_files_zero_drops_history() {
        let dir = tempfile::tempdir().expect("tempdir");
        let writer = AuditWriter::open(dir.path(), RotationPolicy::new(300, 0)).expect("打开");
        for index in 0..5 {
            writer.write(&record(index)).await.expect("写入");
        }
        let entries: Vec<String> = std::fs::read_dir(dir.path().join(DATA_SUBDIR))
            .expect("列目录")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
            })
            .collect();
        assert_eq!(entries, vec!["audit.jsonl".to_owned()]);
    }

    #[tokio::test]
    async fn contention_falls_back_to_pid_suffixed_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let writer = AuditWriter::open(dir.path(), RotationPolicy::default()).expect("打开");
        writer.write(&record(0)).await.expect("首次写入建文件");

        // 在测试进程里对活动文件加锁，模拟「另一个实例正在写」。
        let held = open_append(writer.path()).expect("打开");
        assert!(exclusive_lock(&held).expect("加锁"), "测试进程应取得独占锁");

        writer
            .write(&record(1))
            .await
            .expect("争用时应退化而不是失败");

        let sidecar = dir
            .path()
            .join(DATA_SUBDIR)
            .join(format!("audit.{}.jsonl", std::process::id()));
        assert!(sidecar.exists(), "退化文件必须出现：{}", sidecar.display());
        let lines = read_lines(&sidecar);
        assert_eq!(lines.len(), 1);
        assert_eq!(read_lines(writer.path()).len(), 1, "活动文件不被追加");
    }

    #[tokio::test]
    async fn audit_never_contains_a_password_string() {
        let dir = tempfile::tempdir().expect("tempdir");
        let writer = AuditWriter::open(dir.path(), RotationPolicy::default()).expect("打开");
        let error = Error::new(
            ErrorCode::MCP_CONNECTION_NOT_FOUND,
            ErrorCategory::NotFound,
            format!("连接失败：password={PASSWORD}"),
        );
        writer
            .write(&AuditRecord::new("req-secret", "list_tables").failed(&error))
            .await
            .expect("写入");
        let text = std::fs::read_to_string(writer.path()).expect("Read");
        assert!(!text.contains(PASSWORD), "审计行不得包含密码：{text}");
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let writer = AuditWriter::open(dir.path(), RotationPolicy::default()).expect("打开");
        writer.append_line(&record(0)).expect("写入");
        let file_mode = std::fs::metadata(writer.path())
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        let dir_mode = std::fs::metadata(writer.dir())
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn existing_file_permissions_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let writer = AuditWriter::open(dir.path(), RotationPolicy::default()).expect("打开");
        // 模拟「文件已存在且权限被放宽」。
        std::fs::write(writer.path(), "").expect("写文件");
        std::fs::set_permissions(writer.path(), std::fs::Permissions::from_mode(0o644))
            .expect("放宽权限");
        writer.append_line(&record(0)).expect("写入");
        let mode = std::fs::metadata(writer.path())
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
