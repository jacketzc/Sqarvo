//! `mcp.json` 配置模型与启动期校验（#207 实现；V3.1 §3.1/§8）。
//!
//! 契约要点（改动确认单 #1 §4）：
//! - 默认落 `AppPaths::config_dir()` 下的 `mcp.json`（以 API 返回值为准，
//!   paths.rs 注释与 `ProjectDirs` 实际命名不一致，勿照抄注释），`--config` 覆盖；
//! - 字段映射 driver-api `ConnectionParams`（driver.rs:137-156，无密码字段）；
//! - 启动期驱动白名单预检：mariadb 仅 `ssl_ca`/`ssl_cert`/`ssl_key`/
//!   `connect_timeout_ms`，sqlite 仅 `read_only` 且禁 `username`/`credential_ref`/
//!   TLS/port≠0 组合；
//! - `alias`：`[a-z0-9_-]{1,64}`，全局唯一；
//! - `enabled_tools` 三态：缺失 = 全部启用；空数组 = 全部禁用（警告不拒启）；
//!   未知名 = 配置错误拒绝启动；
//! - `query_limits` 覆盖值一律钳制在硬上限内（200/1000/10s/1MiB/并发≤4）。
//!
//! # 加载策略（默认拒绝）
//!
//! | 情况 | 行为 |
//! |---|---|
//! | 文件不存在 | 空连接列表启动 + stderr 警告；任何工具都无法访问数据库 |
//! | 文件存在但 JSON 非法 / 字段非法 | 拒绝启动，错误写 stderr，退出码非 0 |
//! | 字段合法但含未知工具名或未知驱动 | 拒绝启动（不静默忽略） |
//!
//! # 驱动白名单预检的边界
//!
//! 按驱动规则（`sqlite` / `mariadb`）镜像驱动自己的 `from_params` 参数白名单
//! （`crates/drivers/sqlite/src/driver.rs:133-190`、
//! `crates/drivers/mariadb/src/config.rs:50-125`），目的是把拼错的 `properties`
//! 键在启动时就暴露，而不是等到第一次工具调用才在懒连接里失败。**驱动仍是连接期的
//! 唯一权威**：这里只做「提前拒绝」，不做「提前接受」，也不复制驱动的握手逻辑。
//!
//! 新增驱动必须同时加一条规则分支（未知 `driver_id` 一律拒绝启动，fail-closed）；
//! `builtin_driver_ids_match_rules` 测试会在内置驱动集合变化时失败提醒。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{ConnectionParams, DriverId, TlsMode};
use sqarvo_local_store::{AppPaths, validate_reference};

/// 配置文件文件名。
pub const CONFIG_FILE_NAME: &str = "mcp.json";

/// `alias` 最大字节数。
pub const ALIAS_MAX_LEN: usize = 64;

/// 配置文件大小上限：`mcp.json` 只放连接档案与开关，不该有第二个用途。
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// V1 工具名单（与 #209 工具注册表对齐的单一事实源）。
///
/// `enabled_tools` 的校验必须按这张表判「未知名」；#209 注册工具时使用同一份常量，
/// 两边漂移会让合法配置被拒绝，或多出无法发现的名字。
pub const V1_TOOL_NAMES: [&str; 8] = [
    "list_connections",
    "list_databases",
    "list_tables",
    "describe_table",
    "list_indexes",
    "get_table_sample",
    "execute_readonly_query",
    "explain_query",
];

/// V2 structured write tools.
pub const V2_TOOL_NAMES: [&str; 4] = ["insert_rows", "update_rows", "delete_rows", "preview_write"];

/// V2.1 explicit transaction tools.
pub const TRANSACTION_TOOL_NAMES: [&str; 4] = [
    "begin_transaction",
    "execute_in_transaction",
    "commit_transaction",
    "rollback_transaction",
];

/// V2.2 structured schema tools.
pub const DDL_TOOL_NAMES: [&str; 6] = [
    "create_table",
    "alter_table",
    "drop_table",
    "create_index",
    "drop_index",
    "preview_schema_change",
];

/// Schema Intelligence read-only tools.
pub const SCHEMA_TOOL_NAMES: [&str; 5] = [
    "list_foreign_keys",
    "list_constraints",
    "get_table_ddl",
    "get_table_statistics",
    "get_database_schema",
];

/// Names recognized by the shared tool policy.
pub fn tool_names() -> impl Iterator<Item = &'static str> {
    V1_TOOL_NAMES
        .into_iter()
        .chain(V2_TOOL_NAMES)
        .chain(TRANSACTION_TOOL_NAMES)
        .chain(DDL_TOOL_NAMES)
        .chain(SCHEMA_TOOL_NAMES)
}

/// 工具未指定 `limit` 时的返回行数（V3.1 §10）。
pub const DEFAULT_MAX_ROWS: u32 = 200;
/// 硬性最大返回行数：Agent 显式 `limit` 也越不过去。
pub const HARD_MAX_ROWS: u32 = 1000;
/// 默认查询超时。
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// 查询超时硬上限（覆盖值只能更小）。
pub const HARD_MAX_QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次返回数据大小默认值（1 MiB）。
pub const DEFAULT_MAX_RESULT_BYTES: u64 = 1024 * 1024;
/// 单次返回数据大小硬上限。
pub const HARD_MAX_RESULT_BYTES: u64 = 1024 * 1024;
/// 默认进程级并发。
pub const DEFAULT_MAX_CONCURRENCY: usize = 2;
/// 进程级并发硬上限。
pub const HARD_MAX_CONCURRENCY: usize = 4;

/// 本版本内置驱动标识（与 `sqarvo_drivers::DriverRegistry::with_builtin_drivers()`
/// 对齐，由 `builtin_driver_ids_match_rules` 测试锁定）。
const KNOWN_DRIVER_IDS: [&str; 2] = ["sqlite", "mariadb"];

/// `mcp.json` 根结构。
///
/// `deny_unknown_fields` 是安全约束的一部分：`password` 之类的字段会让**启动失败**，
/// 而不是被静默忽略后让人误以为它生效了。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// 已授权的数据库连接档案。缺失 = 空列表（默认拒绝）。
    #[serde(default)]
    pub connections: Vec<ConnectionProfile>,
    /// 工具开关三态（见模块文档）。字段缺失 = `None` = 全部启用。
    pub enabled_tools: Option<Vec<String>>,
    /// 查询限额覆盖；逐字段钳制到硬上限。
    pub query_limits: Option<QueryLimitsOverride>,
    /// DBA-approved MariaDB profile UUIDs and their exact revisions.
    #[serde(default)]
    pub mariadb_write_profiles: BTreeMap<uuid::Uuid, u64>,
}

/// 单个连接档案。
///
/// 与 `ConnectionParams` 一一对应，但**没有密码字段**（V3.1 §3.1 要求 6）：
/// 密码只能存在于操作系统凭据库，通过 `credential_ref` 引用。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionProfile {
    /// 稳定连接标识：Agent 侧可见，工具参数里用它选连接。
    pub alias: String,
    /// 驱动标识（`sqlite` / `mariadb`；内置集合见 `KNOWN_DRIVER_IDS`）。
    pub driver_id: String,
    /// 主机；SQLite 解释为数据库文件路径。
    pub host: String,
    /// 端口；SQLite 必须为 0。
    #[serde(default)]
    pub port: u16,
    /// 数据库名；SQLite 不允许。
    pub database: Option<String>,
    /// 用户名；SQLite 不允许，MariaDB 必填。
    pub username: Option<String>,
    /// 凭据引用（`sqarvo:conn/<uuid v4>`）；密码本体永不落盘。
    pub credential_ref: Option<String>,
    /// TLS 模式；SQLite 只接受 `unspecified` / `disabled`。
    #[serde(default)]
    pub tls: TlsModeSpec,
    /// TLS 校验主机名。
    pub tls_server_name: Option<String>,
    /// 驱动特有、非敏感的附加参数（白名单见模块文档）。
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    /// 是否把该连接授权给 Agent。**必须显式给出**：缺字段即为非法配置
    /// （V3.1 §3.1 要求 10「每个连接均须明确启用状态」，默认拒绝）。
    pub enabled: bool,
}

/// `tls` 字段的字符串形态。
///
/// 与 [`TlsMode`] 一一对应；`rename_all = "snake_case"` 用
/// `verify_ca` / `verify_full`，不接受大小写变体（写错就是配置错误）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsModeSpec {
    /// 未指定，由驱动选择安全默认值。
    #[default]
    Unspecified,
    /// 显式禁用 TLS（仅限本地开发）。
    Disabled,
    /// 优先 TLS，失败时回退明文。
    Preferred,
    /// 必须 TLS，不校验证书主体。
    Required,
    /// 校验证书链。
    VerifyCa,
    /// 校验证书链与主机名。
    VerifyFull,
}

impl From<TlsModeSpec> for TlsMode {
    fn from(spec: TlsModeSpec) -> Self {
        match spec {
            TlsModeSpec::Unspecified => Self::Unspecified,
            TlsModeSpec::Disabled => Self::Disabled,
            TlsModeSpec::Preferred => Self::Preferred,
            TlsModeSpec::Required => Self::Required,
            TlsModeSpec::VerifyCa => Self::VerifyCa,
            TlsModeSpec::VerifyFull => Self::VerifyFull,
        }
    }
}

/// `query_limits` 覆盖段：每个字段都可缺省，合法区间见 [`EffectiveLimits`]。
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryLimitsOverride {
    /// 工具默认返回行数，钳制到 `[1, HARD_MAX_ROWS]`。
    pub max_rows: Option<u32>,
    /// 查询超时毫秒数，钳制到 `[1, HARD_MAX_QUERY_TIMEOUT]`。
    pub timeout_ms: Option<u64>,
    /// 单次返回数据大小字节数，钳制到 `[1 KiB, HARD_MAX_RESULT_BYTES]`。
    pub max_result_bytes: Option<u64>,
    /// 进程级并发，钳制到 `[1, HARD_MAX_CONCURRENCY]`。
    pub max_concurrency: Option<usize>,
}

/// 钳制后的生效限额：工具层（#209）与服务层（#207）共用同一份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveLimits {
    /// 未指定 `limit` 时的返回行数。
    pub max_rows: u32,
    /// Agent 显式 `limit` 也越不过去的硬上限。
    pub hard_max_rows: u32,
    /// 单次查询超时。
    pub query_timeout: Duration,
    /// 单次返回数据大小上限。
    pub max_result_bytes: u64,
    /// 进程级并发（信号量与 `QueryRegistry` 共用）。
    pub max_concurrency: usize,
}

impl EffectiveLimits {
    /// 由覆盖段计算生效值，并把被钳制的项写进 `warnings`。
    fn resolve(overrides: Option<&QueryLimitsOverride>, warnings: &mut Vec<String>) -> Self {
        let mut limits = Self {
            max_rows: DEFAULT_MAX_ROWS,
            hard_max_rows: HARD_MAX_ROWS,
            query_timeout: DEFAULT_QUERY_TIMEOUT,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
        };
        let Some(overrides) = overrides else {
            return limits;
        };
        if let Some(max_rows) = overrides.max_rows {
            let clamped = max_rows.clamp(1, HARD_MAX_ROWS);
            if clamped != max_rows {
                warnings.push(format!(
                    "query_limits.max_rows = {max_rows} exceeds the hard limit and was clamped to {clamped}"
                ));
            }
            limits.max_rows = clamped;
        }
        if let Some(timeout_ms) = overrides.timeout_ms {
            let hard = u64::try_from(HARD_MAX_QUERY_TIMEOUT.as_millis()).unwrap_or(u64::MAX);
            let clamped = timeout_ms.clamp(1, hard);
            if clamped != timeout_ms {
                warnings.push(format!(
                    "query_limits.timeout_ms = {timeout_ms} exceeds the hard limit and was clamped to {clamped}"
                ));
            }
            limits.query_timeout = Duration::from_millis(clamped);
        }
        if let Some(max_result_bytes) = overrides.max_result_bytes {
            let clamped = max_result_bytes.clamp(1024, HARD_MAX_RESULT_BYTES);
            if clamped != max_result_bytes {
                warnings.push(format!(
                    "query_limits.max_result_bytes = {max_result_bytes} is out of range and was clamped to {clamped}"
                ));
            }
            limits.max_result_bytes = clamped;
        }
        if let Some(max_concurrency) = overrides.max_concurrency {
            let clamped = max_concurrency.clamp(1, HARD_MAX_CONCURRENCY);
            if clamped != max_concurrency {
                warnings.push(format!(
                    "query_limits.max_concurrency = {max_concurrency} exceeds the hard limit ({HARD_MAX_CONCURRENCY}) and was clamped to {clamped}"
                ));
            }
            limits.max_concurrency = clamped;
        }
        limits
    }

    /// 转成 `query-engine` 的登记表限额（并发与进程信号量同源）。
    ///
    /// 两者必须一致：登记表先放行、信号量后阻塞，等于把并发上限抬到
    /// `max_concurrent` 之外（V3.1 §9.1）。
    #[must_use]
    pub fn query_limits(&self) -> sqarvo_query_engine::QueryLimits {
        sqarvo_query_engine::QueryLimits {
            default_batch_rows: self.max_rows,
            max_batch_rows: self.hard_max_rows,
            max_concurrent: self.max_concurrency,
        }
    }
}

/// `enabled_tools` 的判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPolicy {
    /// 字段缺失：启用 V1 全部工具。
    All,
    /// 显式白名单；空集合表示全部禁用（启动时已告警）。
    Only(BTreeSet<String>),
}

impl ToolPolicy {
    /// 该工具是否启用。
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        match self {
            Self::All => tool_names().any(|known| known == name),
            Self::Only(names) => names.contains(name),
        }
    }

    /// 未启用时返回 `mcp.tool_disabled`（V3.1 §3.1：即使 Agent 主动调用也必须拒绝）。
    pub fn require(&self, name: &str) -> Result<()> {
        if self.allows(name) {
            return Ok(());
        }
        Err(Error::new(
            ErrorCode::MCP_TOOL_DISABLED,
            sqarvo_common::ErrorCategory::PermissionDenied,
            format!("Tool {name} is not enabled in mcp.json under enabled_tools"),
        ))
    }

    /// 启用的工具名单（升序）。
    #[must_use]
    pub fn enabled_names(&self) -> Vec<&str> {
        match self {
            Self::All => tool_names().collect(),
            Self::Only(names) => names.iter().map(String::as_str).collect(),
        }
    }
}

/// 通过校验的配置：后续模块只依赖这个类型，不再碰原始 `McpConfig`。
#[derive(Debug, Clone)]
pub struct ValidatedConfig {
    connections: BTreeMap<String, ConnectionProfile>,
    tools: ToolPolicy,
    limits: EffectiveLimits,
    mariadb_write_profiles: BTreeMap<uuid::Uuid, u64>,
    warnings: Vec<String>,
}

impl ValidatedConfig {
    /// 已授权连接（按 alias 升序）。
    #[must_use]
    pub fn connections(&self) -> &BTreeMap<String, ConnectionProfile> {
        &self.connections
    }

    /// 按 alias 取连接档案。
    #[must_use]
    pub fn connection(&self, alias: &str) -> Option<&ConnectionProfile> {
        self.connections.get(alias)
    }

    /// 连接数量。
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// 工具开关。
    #[must_use]
    pub const fn tools(&self) -> &ToolPolicy {
        &self.tools
    }

    /// 生效限额。
    #[must_use]
    pub const fn limits(&self) -> &EffectiveLimits {
        &self.limits
    }

    /// Whether the exact profile revision was opted into the DBA deployment contract.
    pub fn allows_mariadb_write(&self, id: uuid::Uuid, revision: u64) -> bool {
        self.mariadb_write_profiles.get(&id) == Some(&revision)
    }

    /// 校验期产生的告警（非致命）。
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// 配置加载结果。
#[derive(Debug, Clone)]
pub struct LoadOutcome {
    /// 解析后的原始配置（尚未做语义校验）。
    pub config: McpConfig,
    /// 实际使用的配置文件路径。
    pub path: PathBuf,
    /// 文件是否存在；`false` 表示按「空连接列表 + 警告」启动。
    pub found: bool,
    /// 加载期告警（文件缺失提示等）。
    pub warnings: Vec<String>,
}

/// 默认配置文件路径：`AppPaths::config_dir()/mcp.json`。
///
/// 路径一律取 `AppPaths` 返回值：`paths.rs` 模块注释里的 `~/.config/sqarvo` 与
/// `ProjectDirs` 实际生成的目录名不一致（API 卡片 §7 第 9 条）。
pub fn default_config_path() -> Result<PathBuf> {
    Ok(AppPaths::discover()?.config_dir().join(CONFIG_FILE_NAME))
}

/// 解析配置路径：显式 `--config` 优先，否则用默认路径。
pub fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    match explicit {
        Some(path) => Ok(path.to_path_buf()),
        None => default_config_path(),
    }
}

/// 读取并解析 `mcp.json`。
///
/// 文件缺失**不是**错误：返回空配置 + 明确告警（默认拒绝，V3.1 §3.1）。
/// 文件存在但无法读取/解析/超过大小上限则返回错误，调用方必须拒绝启动。
pub fn load(explicit: Option<&Path>) -> Result<LoadOutcome> {
    let path = resolve_config_path(explicit)?;
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let source = if explicit.is_some() {
                "The path specified by --config does not exist"
            } else {
                "The default path does not exist. Create it as described in the documentation"
            };
            return Ok(LoadOutcome {
                config: McpConfig::default(),
                path: path.clone(),
                found: false,
                warnings: vec![format!(
                    "mcp.json not found ({}): {source}. Using the default tool configuration and query limits",
                    path.display()
                )],
            });
        }
        Err(error) => return Err(Error::from(error)),
    };
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(config_error(format!(
            "mcp.json exceeds the {MAX_CONFIG_BYTES}-byte limit ({} bytes)",
            metadata.len()
        )));
    }
    let text = std::fs::read_to_string(&path).map_err(Error::from)?;
    let config = parse(&text)?;
    Ok(LoadOutcome {
        config,
        path,
        found: true,
        warnings: Vec::new(),
    })
}

/// 从 JSON 文本解析配置（不做语义校验，供测试与 [`load`] 复用）。
///
/// 诊断仅报告错误类型与位置，避免 serde 的文案回显配置内容。
pub fn parse(text: &str) -> Result<McpConfig> {
    serde_json::from_str(text).map_err(|error| {
        let reason = match error.classify() {
            serde_json::error::Category::Io => "Read failed",
            serde_json::error::Category::Syntax => "Invalid JSON syntax",
            serde_json::error::Category::Data => "A field does not match the configuration format",
            serde_json::error::Category::Eof => "Unexpected end of JSON",
        };
        config_error(format!(
            "Failed to parse mcp.json: {reason} (line {}, column {})",
            error.line(),
            error.column()
        ))
    })
}

impl McpConfig {
    /// 语义校验，产出 [`ValidatedConfig`]。
    ///
    /// 任一检查失败都拒绝启动；可继续运行的偏差（超限被钳制、空 `enabled_tools`、
    /// MariaDB 未配 `credential_ref`）只记告警。
    pub fn validate(self) -> Result<ValidatedConfig> {
        let mut warnings = Vec::new();
        let mut connections = BTreeMap::new();
        let McpConfig {
            connections: profiles,
            enabled_tools,
            query_limits,
            mariadb_write_profiles,
        } = self;
        for profile in profiles {
            validate_profile(&profile, &mut warnings)?;
            let alias = profile.alias.clone();
            if connections.insert(alias.clone(), profile).is_some() {
                return Err(config_error(format!(
                    "Duplicate connection alias: {alias}. Aliases must be globally unique"
                )));
            }
        }
        let tools = tool_policy(enabled_tools, &mut warnings)?;
        let limits = EffectiveLimits::resolve(query_limits.as_ref(), &mut warnings);
        Ok(ValidatedConfig {
            connections,
            tools,
            limits,
            mariadb_write_profiles,
            warnings,
        })
    }
}

fn tool_policy(
    enabled_tools: Option<Vec<String>>,
    warnings: &mut Vec<String>,
) -> Result<ToolPolicy> {
    let Some(names) = enabled_tools else {
        return Ok(ToolPolicy::All);
    };
    let mut selected = BTreeSet::new();
    for name in names {
        if !tool_names().any(|known| known == name) {
            return Err(config_error(format!(
                "enabled_tools contains an unknown tool name. Supported tools: {}",
                tool_names().collect::<Vec<_>>().join("、")
            )));
        }
        selected.insert(name);
    }
    if selected.is_empty() {
        warnings.push(
            "enabled_tools is empty. The server will start with all tools disabled and reject every tool call"
                .to_owned(),
        );
    }
    Ok(ToolPolicy::Only(selected))
}

impl ConnectionProfile {
    /// 转成驱动层 `ConnectionParams`。
    ///
    /// 只做形状转换：密码不在其中，`credential_ref` 由 `SessionManager` 在握手前解析。
    pub fn to_connection_params(&self) -> Result<ConnectionParams> {
        Ok(ConnectionParams {
            driver_id: DriverId::new(self.driver_id.clone())?,
            host: self.host.clone(),
            port: self.port,
            database: self.database.clone(),
            username: self.username.clone(),
            credential_ref: self.credential_ref.clone(),
            tls: self.tls.into(),
            tls_server_name: self.tls_server_name.clone(),
            properties: self.properties.clone(),
        })
    }
}

fn config_error(message: impl Into<String>) -> Error {
    Error::invalid_argument(ErrorCode::CONFIG_INVALID, message)
}

/// `alias` 格式：`[a-z0-9_-]{1,64}`。
///
/// 限制字符集是为了让 alias 能直接出现在工具参数、资源 URI 路径段与审计日志里，
/// 不必依赖各处的转义约定。
pub fn validate_alias(alias: &str) -> Result<()> {
    let valid_len = !alias.is_empty() && alias.len() <= ALIAS_MAX_LEN;
    let valid_chars = alias
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte));
    if valid_len && valid_chars {
        return Ok(());
    }
    Err(config_error(format!(
        "Invalid connection alias {alias:?}. Use 1 to {ALIAS_MAX_LEN} lowercase letters, digits, underscores, or hyphens"
    )))
}

fn validate_profile(profile: &ConnectionProfile, warnings: &mut Vec<String>) -> Result<()> {
    validate_alias(&profile.alias)?;
    if let Some(reference) = profile.credential_ref.as_deref() {
        // 错误文案不回显引用原文（validate_reference 已保证）。
        validate_reference(reference)?;
    }
    reject_secret_like_properties(profile)?;
    match profile.driver_id.as_str() {
        "sqlite" => validate_sqlite(profile),
        "mariadb" => validate_mariadb(profile, warnings),
        other => Err(config_error(format!(
            "Unsupported driver_id for connection {}: {other}. Built-in drivers in this version: {}",
            profile.alias,
            KNOWN_DRIVER_IDS.join("、")
        ))),
    }
}

/// `properties` 里不允许出现凭据类键（对任何驱动生效）。
///
/// 驱动的参数白名单已经会拒未知键，但这条规则对「将来新增的驱动」也成立：
/// 把密码塞进 `properties` 是明确的错误用法，必须当场拒绝而不是交给驱动解释。
fn reject_secret_like_properties(profile: &ConnectionProfile) -> Result<()> {
    for key in profile.properties.keys() {
        let lower = key.to_ascii_lowercase();
        let secret_like = ["password", "passwd", "secret", "token"]
            .iter()
            .any(|needle| lower.contains(needle));
        if secret_like {
            return Err(config_error(format!(
                "Connection {} does not allow credential parameter {key} in properties. Store the password in the system credential store and reference it with credential_ref",
                profile.alias
            )));
        }
    }
    Ok(())
}

/// SQLite 专属规则（镜像 `crates/drivers/sqlite/src/driver.rs:133-190`）。
fn validate_sqlite(profile: &ConnectionProfile) -> Result<()> {
    let alias = &profile.alias;
    if profile.host.trim().is_empty() {
        return Err(config_error(format!(
            "host for connection {alias} must specify the SQLite database file path"
        )));
    }
    if profile.port != 0 {
        return Err(config_error(format!(
            "Local SQLite connection {alias} requires port 0"
        )));
    }
    if profile.database.is_some() {
        return Err(config_error(format!(
            "SQLite connection {alias} uses host for the database file path and does not accept the database field"
        )));
    }
    if profile.username.is_some() || profile.credential_ref.is_some() {
        return Err(config_error(format!(
            "SQLite connection {alias} does not support username or credential_ref"
        )));
    }
    if !matches!(
        profile.tls,
        TlsModeSpec::Unspecified | TlsModeSpec::Disabled
    ) || profile.tls_server_name.is_some()
    {
        return Err(config_error(format!(
            "Local SQLite connection {alias} does not support TLS"
        )));
    }
    let mut read_only = false;
    for (key, value) in &profile.properties {
        match key.as_str() {
            "read_only" => {
                read_only = parse_bool(value).ok_or_else(|| {
                    config_error(format!(
                        "read_only for connection {alias} must be true, false, 1, or 0. Received {value:?}"
                    ))
                })?;
            }
            other => {
                return Err(config_error(format!(
                    "SQLite connection {alias} does not support parameter {other}"
                )));
            }
        }
    }
    if read_only && profile.host.trim() == ":memory:" {
        return Err(config_error(format!(
            "In-memory SQLite connection {alias} cannot be read-only"
        )));
    }
    Ok(())
}

/// MariaDB 专属规则（镜像 `crates/drivers/mariadb/src/config.rs:50-125`）。
fn validate_mariadb(profile: &ConnectionProfile, warnings: &mut Vec<String>) -> Result<()> {
    let alias = &profile.alias;
    if profile.host.trim().is_empty() {
        return Err(config_error(format!(
            "host for connection {alias} must not be empty"
        )));
    }
    if profile.port == 0 {
        return Err(config_error(format!(
            "MariaDB connection {alias} requires a port"
        )));
    }
    let Some(username) = profile.username.as_deref().map(str::trim) else {
        return Err(config_error(format!(
            "MariaDB connection {alias} requires a username"
        )));
    };
    if username.is_empty() {
        return Err(config_error(format!(
            "MariaDB connection {alias} requires a nonempty username"
        )));
    }
    let mut has_ca = false;
    let mut has_cert = false;
    let mut has_key = false;
    for (key, value) in &profile.properties {
        match key.as_str() {
            "ssl_ca" => has_ca = !value.trim().is_empty(),
            "ssl_cert" => has_cert = !value.trim().is_empty(),
            "ssl_key" => has_key = !value.trim().is_empty(),
            "connect_timeout_ms" => {
                let parsed = value.trim().parse::<u64>().map_err(|_| {
                    config_error(format!(
                        "connect_timeout_ms for connection {alias} must be an integer number of milliseconds"
                    ))
                })?;
                if !(100..=300_000).contains(&parsed) {
                    return Err(config_error(format!(
                        "connect_timeout_ms for connection {alias} must be between 100 and 300000 milliseconds"
                    )));
                }
            }
            other => {
                return Err(config_error(format!(
                    "MariaDB connection {alias} does not support parameter {other}. Allowed: ssl_ca, ssl_cert, ssl_key, and connect_timeout_ms"
                )));
            }
        }
    }
    if has_cert != has_key {
        return Err(config_error(format!(
            "Connection {alias} requires both the client certificate and its private key"
        )));
    }
    if profile.tls == TlsModeSpec::Disabled && (has_ca || has_cert) {
        return Err(config_error(format!(
            "Connection {alias} has TLS disabled and cannot specify certificate parameters"
        )));
    }
    if profile.tls == TlsModeSpec::Disabled && profile.tls_server_name.is_some() {
        return Err(config_error(format!(
            "Connection {alias} has TLS disabled and cannot specify tls_server_name"
        )));
    }
    if profile.credential_ref.is_none() {
        warnings.push(format!(
            "Connection {alias} has no credential_ref. It will try an empty password. To use a credential, save it with `sqarvo-mcp credentials set` and supply the reference"
        ));
    }
    Ok(())
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_profile(alias: &str) -> serde_json::Value {
        serde_json::json!({
            "alias": alias,
            "driver_id": "sqlite",
            "host": "/tmp/sqarvo-mcp-test.db",
            "port": 0,
            "enabled": true,
            "properties": { "read_only": "true" }
        })
    }

    fn mariadb_profile(alias: &str) -> serde_json::Value {
        serde_json::json!({
            "alias": alias,
            "driver_id": "mariadb",
            "host": "127.0.0.1",
            "port": 3306,
            "database": "app_db",
            "username": "app_user",
            "credential_ref": "sqarvo:conn/0b7f4b6e-1c2d-4e5f-8a9b-0c1d2e3f4a5b",
            "enabled": true
        })
    }

    /// 校验一段 JSON：既接受完整的根配置，也接受单个连接档案（自动包一层）。
    ///
    /// 后者让「一个连接档案的非法组合」类测试不必每次手写外壳。
    fn validate_json(value: &serde_json::Value) -> Result<ValidatedConfig> {
        let is_root = value.get("connections").is_some()
            || value.get("enabled_tools").is_some()
            || value.get("query_limits").is_some();
        let root = if is_root {
            value.clone()
        } else {
            serde_json::json!({ "connections": [value] })
        };
        parse(&root.to_string())?.validate()
    }

    #[test]
    fn builtin_driver_ids_match_rules() {
        let registry = sqarvo_drivers::DriverRegistry::with_builtin_drivers();
        let mut ids: Vec<String> = registry
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.id.as_str().to_owned())
            .collect();
        ids.sort();
        let mut known: Vec<String> = KNOWN_DRIVER_IDS.iter().map(|id| (*id).to_owned()).collect();
        known.sort();
        assert_eq!(
            ids, known,
            "内置驱动集合变化时必须同步 config::KNOWN_DRIVER_IDS 与按驱动规则"
        );
    }

    #[test]
    fn missing_file_starts_with_empty_connections_and_warns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        let outcome = load(Some(&path)).expect("缺文件不是错误");
        assert!(!outcome.found);
        assert!(outcome.config.connections.is_empty());
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("default tool configuration and query limits"));
        let validated = outcome.config.validate().expect("空配置合法（默认拒绝）");
        assert_eq!(validated.connection_count(), 0);
    }

    #[test]
    fn load_reads_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        let root = serde_json::json!({ "connections": [sqlite_profile("dev")] });
        std::fs::write(&path, root.to_string()).expect("write");
        let outcome = load(Some(&path)).expect("读取成功");
        assert!(outcome.found);
        assert_eq!(outcome.warnings.len(), 0);
    }

    #[test]
    fn parse_rejects_password_field() {
        let mut value = mariadb_profile("dev");
        value["password"] = serde_json::json!("hunter2");
        let error = validate_json(&value).expect_err("mcp.json 不允许携带密码");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert!(!error.message().contains("hunter2"));
    }

    #[test]
    fn parse_rejects_unknown_top_level_field() {
        let value = serde_json::json!({ "connections": [], "enable_tools": ["list_tables"] });
        let error = parse(&value.to_string()).expect_err("顶层未知字段必须拒绝");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert!(
            error
                .message()
                .contains("A field does not match the configuration format")
        );
        assert!(!error.message().contains("enable_tools"));
    }

    #[test]
    fn enabled_field_is_mandatory() {
        let mut value = sqlite_profile("dev");
        value.as_object_mut().expect("object").remove("enabled");
        let error = validate_json(&value).expect_err("缺少 enabled 必须拒绝");
        assert!(
            error
                .message()
                .contains("A field does not match the configuration format")
        );
    }

    #[test]
    fn alias_format_and_uniqueness_are_enforced() {
        assert!(validate_alias("dev-mariadb_1").is_ok());
        assert!(validate_alias("").is_err());
        assert!(validate_alias("Dev").is_err());
        assert!(validate_alias("dev mariadb").is_err());
        assert!(validate_alias(&"a".repeat(ALIAS_MAX_LEN + 1)).is_err());

        let value = serde_json::json!({
            "connections": [sqlite_profile("dev"), sqlite_profile("dev")]
        });
        let error = validate_json(&value).expect_err("alias 重复必须拒绝");
        assert!(error.message().contains("Duplicate"));
    }

    #[test]
    fn sqlite_rejects_forbidden_combinations() {
        let mut with_username = sqlite_profile("dev");
        with_username["username"] = serde_json::json!("app_user");
        assert!(validate_json(&with_username).is_err());

        let mut with_port = sqlite_profile("dev");
        with_port["port"] = serde_json::json!(3306);
        assert!(validate_json(&with_port).is_err());

        let mut with_database = sqlite_profile("dev");
        with_database["database"] = serde_json::json!("main");
        assert!(validate_json(&with_database).is_err());

        let mut with_tls = sqlite_profile("dev");
        with_tls["tls"] = serde_json::json!("required");
        assert!(validate_json(&with_tls).is_err());

        let mut with_unknown_property = sqlite_profile("dev");
        with_unknown_property["properties"] = serde_json::json!({ "busy_timeout": "5000" });
        assert!(validate_json(&with_unknown_property).is_err());

        let mut memory_read_only = sqlite_profile("dev");
        memory_read_only["host"] = serde_json::json!(":memory:");
        assert!(validate_json(&memory_read_only).is_err());
    }

    #[test]
    fn mariadb_rejects_unknown_property_and_bad_combinations() {
        let mut unknown_property = mariadb_profile("dev");
        unknown_property["properties"] = serde_json::json!({ "charset": "utf8mb4" });
        let error = validate_json(&unknown_property).expect_err("未知 property 必须拒绝");
        assert!(error.message().contains("charset"));

        let mut no_username = mariadb_profile("dev");
        no_username
            .as_object_mut()
            .expect("object")
            .remove("username");
        assert!(validate_json(&no_username).is_err());

        let mut no_port = mariadb_profile("dev");
        no_port["port"] = serde_json::json!(0);
        assert!(validate_json(&no_port).is_err());

        let mut bad_timeout = mariadb_profile("dev");
        bad_timeout["properties"] = serde_json::json!({ "connect_timeout_ms": "10" });
        assert!(validate_json(&bad_timeout).is_err());

        let mut cert_without_key = mariadb_profile("dev");
        cert_without_key["properties"] = serde_json::json!({ "ssl_cert": "/tmp/client.pem" });
        assert!(validate_json(&cert_without_key).is_err());

        let mut disabled_with_ca = mariadb_profile("dev");
        disabled_with_ca["tls"] = serde_json::json!("disabled");
        disabled_with_ca["properties"] = serde_json::json!({ "ssl_ca": "/tmp/ca.pem" });
        assert!(validate_json(&disabled_with_ca).is_err());
    }

    #[test]
    fn secret_like_property_is_rejected_without_echoing_the_value() {
        let mut value = mariadb_profile("dev");
        value["properties"] = serde_json::json!({ "db_password": "hunter2" });
        let error = validate_json(&value).expect_err("properties 携带密码必须拒绝");
        assert!(error.message().contains("db_password"));
        // 只报键名，不回显值。
        assert!(!error.message().contains("hunter2"));
    }

    #[test]
    fn unknown_driver_id_is_rejected() {
        let mut value = mariadb_profile("dev");
        value["driver_id"] = serde_json::json!("postgresql");
        let error = validate_json(&value).expect_err("未知驱动必须拒绝");
        assert!(error.message().contains("Unsupported"));
    }

    #[test]
    fn credential_ref_format_is_enforced_without_echoing_it() {
        let mut value = mariadb_profile("dev");
        value["credential_ref"] = serde_json::json!("sqarvo:CONN/not-a-uuid");
        let error = validate_json(&value).expect_err("引用格式必须校验");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert!(!error.message().contains("not-a-uuid"));
    }

    #[test]
    fn missing_credential_ref_warns_but_starts() {
        let mut value = mariadb_profile("dev");
        value
            .as_object_mut()
            .expect("object")
            .remove("credential_ref");
        let validated = validate_json(&value).expect("无凭据引用仍可启动（空密码）");
        assert!(
            validated
                .warnings()
                .iter()
                .any(|warning| warning.contains("credential_ref"))
        );
    }

    #[test]
    fn enabled_tools_tri_state() {
        // 缺失 = 全部启用
        let value = serde_json::json!({ "connections": [sqlite_profile("dev")] });
        let validated = validate_json(&value).expect("合法配置");
        assert_eq!(validated.tools(), &ToolPolicy::All);
        assert!(validated.tools().allows("execute_readonly_query"));
        assert_eq!(
            validated.tools().enabled_names().len(),
            tool_names().count()
        );

        // 显式列表 = 只启用这些
        let value = serde_json::json!({
            "connections": [sqlite_profile("dev")],
            "enabled_tools": ["list_tables", "describe_table"]
        });
        let validated = validate_json(&value).expect("合法配置");
        assert!(validated.tools().allows("list_tables"));
        assert!(!validated.tools().allows("execute_readonly_query"));
        assert_eq!(
            validated.tools().enabled_names(),
            vec!["describe_table", "list_tables"]
        );

        // 空数组 = 全部禁用 + 告警，但不阻止启动
        let value = serde_json::json!({
            "connections": [sqlite_profile("dev")],
            "enabled_tools": []
        });
        let validated = validate_json(&value).expect("空数组不阻止启动");
        assert_eq!(validated.tools().enabled_names(), Vec::<&str>::new());
        assert!(
            validated
                .warnings()
                .iter()
                .any(|warning| warning.contains("all tools disabled"))
        );

        // 未知名 = 拒绝启动
        let value = serde_json::json!({
            "connections": [sqlite_profile("dev")],
            "enabled_tools": ["list_tables", "drop_database"]
        });
        let error = validate_json(&value).expect_err("未知工具名必须拒绝启动");
        assert!(error.message().contains("enabled_tools"));
        assert!(!error.message().contains("drop_database"));
    }

    #[test]
    fn disabled_tool_returns_tool_disabled_error() {
        let policy = ToolPolicy::Only(BTreeSet::new());
        let error = policy.require("list_tables").expect_err("未启用必须拒绝");
        assert_eq!(error.code(), ErrorCode::MCP_TOOL_DISABLED);
        assert_eq!(
            error.category(),
            sqarvo_common::ErrorCategory::PermissionDenied
        );
    }

    #[test]
    fn query_limits_use_documented_defaults() {
        let limits = EffectiveLimits::resolve(None, &mut Vec::new());
        assert_eq!(limits.max_rows, 200);
        assert_eq!(limits.hard_max_rows, 1000);
        assert_eq!(limits.query_timeout, Duration::from_secs(10));
        assert_eq!(limits.max_result_bytes, 1024 * 1024);
        assert_eq!(limits.max_concurrency, 2);
        let engine = limits.query_limits();
        assert_eq!(engine.default_batch_rows, 200);
        assert_eq!(engine.max_batch_rows, 1000);
        assert_eq!(engine.max_concurrent, 2);
    }

    #[test]
    fn query_limits_overrides_are_clamped() {
        let overrides = QueryLimitsOverride {
            max_rows: Some(50_000),
            timeout_ms: Some(600_000),
            max_result_bytes: Some(64 * 1024 * 1024),
            max_concurrency: Some(64),
        };
        let mut warnings = Vec::new();
        let limits = EffectiveLimits::resolve(Some(&overrides), &mut warnings);
        assert_eq!(limits.max_rows, HARD_MAX_ROWS);
        assert_eq!(limits.query_timeout, HARD_MAX_QUERY_TIMEOUT);
        assert_eq!(limits.max_result_bytes, HARD_MAX_RESULT_BYTES);
        assert_eq!(limits.max_concurrency, HARD_MAX_CONCURRENCY);
        assert_eq!(
            warnings.len(),
            4,
            "每个被钳制的字段都要有告警：{warnings:?}"
        );

        let legal = QueryLimitsOverride {
            max_rows: Some(100),
            timeout_ms: Some(2_500),
            max_result_bytes: Some(4096),
            max_concurrency: Some(3),
        };
        let mut warnings = Vec::new();
        let limits = EffectiveLimits::resolve(Some(&legal), &mut warnings);
        assert_eq!(warnings.len(), 0);
        assert_eq!(limits.max_rows, 100);
        assert_eq!(limits.query_timeout, Duration::from_millis(2_500));
        assert_eq!(limits.max_result_bytes, 4096);
        assert_eq!(limits.query_limits().max_concurrent, 3);
    }

    #[test]
    fn to_connection_params_drops_nothing_but_the_password() {
        let validated = validate_json(&mariadb_profile("dev")).expect("合法配置");
        let profile = validated.connection("dev").expect("存在");
        let params = profile.to_connection_params().expect("转换成功");
        assert_eq!(params.driver_id.as_str(), "mariadb");
        assert_eq!(params.port, 3306);
        assert_eq!(params.database.as_deref(), Some("app_db"));
        assert_eq!(params.username.as_deref(), Some("app_user"));
        assert!(params.credential_ref.is_some());
        // 结构上就没有 password 字段。
        let debug = format!("{params:?}");
        assert!(!debug.contains("password"));
    }

    #[test]
    fn oversize_file_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        let mut text = String::from("{\"connections\":[],\"pad\":\"");
        text.push_str(&"x".repeat(usize::try_from(MAX_CONFIG_BYTES).expect("fits") + 16));
        text.push_str("\"}");
        std::fs::write(&path, text).expect("write");
        let error = load(Some(&path)).expect_err("超大配置必须拒绝");
        assert!(error.message().contains("byte limit"));
    }
}
