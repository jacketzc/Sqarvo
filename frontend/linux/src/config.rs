//! 非敏感连接配置持久化：`$XDG_CONFIG_HOME/sqarvo/Sqarvo/connections.json`。
//!
//! # 安全约束（AGENTS.md §9、ADR 0012 / 0016）
//!
//! 这里**只有**「怎么连」的信息：名称、驱动、主机、端口、库名、用户名、TLS 模式，
//! 以及操作系统凭据库里的引用 `credential_ref`。密码、token、连接串永远不写进来。
//!
//! 权限：目录 0700、文件 0600。文件对当前用户可读（这是他自己的连接列表），
//! 同机其他用户读不到；即使读到也拿不到密码——引用是随机 UUID，不含主机或用户名。
//!
//! 写入采用「临时文件 + 同目录 rename」：进程崩溃不会留下半个 JSON 文件，
//! 下一次启动不会因为写入被打断而丢掉整个连接列表。

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 配置文件名。
pub const FILE_NAME: &str = "connections.json";

/// 当前文档版本；字段增删时提升，并为旧版本保留读取路径。
const DOCUMENT_VERSION: u32 = 1;

/// 文件型驱动（SQLite）的驱动 ID，与后端 `ListDrivers` 的返回值一致。
pub const SQLITE_DRIVER_ID: &str = "sqlite";

/// 一条保存的连接（不含任何敏感信息）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedConnection {
    /// 用户可见名称。
    pub name: String,
    /// 驱动 ID（来自 `ListDrivers`）。
    pub driver: String,
    /// 主机（服务器型驱动）。
    pub host: String,
    /// 端口（服务器型驱动）。
    pub port: u16,
    /// 数据库名（服务器型驱动）。
    pub database: String,
    /// 用户（服务器型驱动）。
    pub username: String,
    /// 文件型驱动（SQLite）的数据库文件路径；服务器型驱动为空。
    ///
    /// 单独一列而不是复写进 `host`：`connections.json` 是给人看的，
    /// 把路径写进「host」会让运维误以为这是一个网络地址。
    #[serde(default)]
    pub file_path: String,
    /// 文件型驱动是否以只读方式打开；服务器型驱动为 `false`。
    #[serde(default)]
    pub read_only: bool,
    /// TLS 模式（`disabled` / `preferred` / `required` / `verify-ca` / `verify-full`）。
    pub tls_mode: String,
    /// 系统凭据库中的引用，形如 `sqarvo:conn/<uuid-v4>`；无需凭据的驱动为空。
    pub credential_ref: String,
}

impl SavedConnection {
    /// 连接身份键：同一个「连接目标」共享一个键（#27 / #145）。
    ///
    /// - 服务器型：`driver|host|port|username|database`；主机名大小写不敏感，
    ///   库名留空也参与比较（`db_a` 与 `db_b` 是两条连接，不会互相覆盖）。
    /// - 文件型（SQLite）：`driver|file|<规范化绝对路径>`；`./a.db`、相对路径与
    ///   大小写差异视为同一个文件。**大小写不敏感是有意的取舍**：Linux 文件系统
    ///   区分大小写，这里宁可把 `A.db` 与 `a.db` 合并，也不让用户每点一次就多一条
    ///   指向同一个库的连接；真正的两个仅大小写不同的文件属于罕见冲突，合并后
    ///   保留最近一次保存的路径。
    ///
    /// 这个身份**不是**凭据引用：同一目标换密码只覆盖原引用指向的密钥（ADR 0012），
    /// 引用保持不变；因此不能用 `credential_ref` 当身份，否则同目标两次保存会
    /// 各生成一个引用，配置与密钥环都堆条目。
    ///
    /// #114 起这个身份也是连接注册表里的节点键（树上的连接节点与之绑定），
    /// 所以它是 `pub` 的：注册表要用它把「刚建立的会话」对回「哪一条连接」。
    #[must_use]
    pub fn key(&self) -> String {
        if self.driver == SQLITE_DRIVER_ID {
            format!(
                "{}|file|{}",
                self.driver,
                normalized_file_path(&self.file_path)
            )
        } else {
            format!(
                "{}|{}|{}|{}|{}",
                self.driver,
                self.host.to_lowercase(),
                self.port,
                self.username,
                self.database
            )
        }
    }

    /// 文件型驱动（SQLite）的库路径；服务器型驱动为 `None`。
    #[must_use]
    pub fn file_target(&self) -> Option<&str> {
        (self.driver == SQLITE_DRIVER_ID && !self.file_path.is_empty())
            .then_some(self.file_path.as_str())
    }

    /// 连接目标的展示文案：服务器型 `user@host:port/db`，文件型是文件路径。
    ///
    /// 树上的连接节点用它做 tooltip：两个同名连接靠目标区分，不靠猜。
    #[must_use]
    pub fn target_label(&self) -> String {
        if let Some(path) = self.file_target() {
            return path.to_owned();
        }
        let user = if self.username.is_empty() {
            String::new()
        } else {
            format!("{}@", self.username)
        };
        if self.database.is_empty() {
            format!("{user}{}:{}", self.host, self.port)
        } else {
            format!("{user}{}:{}/{}", self.host, self.port, self.database)
        }
    }
}

/// 文件型连接的身份路径：相对路径补成绝对路径，消掉 `.` / `..`，再统一小写。
///
/// 这里做**词法**规范化而不是 `canonicalize`：连接对话框允许输入尚不存在的新库文件，
/// `canonicalize` 会失败；而且符号链接解析与否不影响「同一路径的两种写法」这个目标。
fn normalized_file_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let raw = Path::new(path);
    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        // 取不到当前目录时退回原样（至少不会 panic，也不会把不同文件判成同一个）。
        std::env::current_dir()
            .map(|cwd| cwd.join(raw))
            .unwrap_or_else(|_| raw.to_path_buf())
    };
    let mut parts: Vec<OsString> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // 绝对路径的根不能被 `..` 弹掉；取不到 cwd 的退化路径按普通相对路径处理。
                if parts.len() > 1 || !absolute.is_absolute() {
                    parts.pop();
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                parts.push(component.as_os_str().to_owned());
            }
            Component::Normal(part) => parts.push(part.to_owned()),
        }
    }
    let normalized: PathBuf = parts.iter().collect();
    normalized.to_string_lossy().to_lowercase()
}

/// 配置文件结构。
#[derive(Debug, Default, Serialize, Deserialize)]
struct Document {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    connections: Vec<SavedConnection>,
}

/// 配置读写错误（文案已可直接展示，不含凭据）。
#[derive(Debug)]
pub struct ConfigError {
    detail: String,
}

impl ConfigError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for ConfigError {}

/// 配置目录：`$XDG_CONFIG_HOME/sqarvo/Sqarvo`（未设置时 `~/.config/sqarvo/Sqarvo`）。
///
/// 这里不用 `directories::ProjectDirs`：它在 Linux 上只取 `application` 一级
/// （得到 `~/.config/sqarvo`），而需求固定的是 `sqarvo/Sqarvo` 两级；显式拼接
/// 才能让三个平台的配置位置与文档一致。
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        // XDG 规范：相对路径必须忽略。
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| {
            ConfigError::new("无法确定用户配置目录（XDG_CONFIG_HOME / HOME 均不可用）")
        })?;
    Ok(base.join("sqarvo").join("Sqarvo"))
}

/// 读取已保存的连接（#114 的连接注册表数据源）。
///
/// 文件不存在、损坏或版本不认识时返回空列表并打印一行说明：连接配置是便利功能，
/// 读失败不能阻止用户新建连接，但也不能假装「读到了」。
#[must_use]
pub fn saved_connections() -> Vec<SavedConnection> {
    let Ok(dir) = config_dir() else {
        return Vec::new();
    };
    load_file(&dir.join(FILE_NAME))
}

/// 插入或更新一条连接，返回写入的路径。
///
/// 以 [`SavedConnection::key`]（连接身份）作为标识：同一个目标视为同一条连接，
/// 后续保存**更新**原条目的非身份字段（名称 / TLS 等），不会追加重复项；改库名、
/// 用户名或端口属于另一个目标，各自独立成条。
/// 凭据引用由调用方（`run_save_and_connect`）在写入密码前按身份复用原条目的引用。
pub fn upsert(connection: SavedConnection) -> Result<PathBuf, ConfigError> {
    upsert_in(&config_dir()?, connection)
}

/// 按连接身份查找已保存的条目（找到最近一次保存的那条）。
///
/// 「保存并连接」必须在**写凭据之前**调用它：命中说明这个目标已经有条目和凭据引用，
/// 本次密码要覆盖原引用指向的密钥，而不是让后端生成一个新引用、在密钥环里多一条。
#[must_use]
pub fn find_by_identity(connection: &SavedConnection) -> Option<SavedConnection> {
    let dir = config_dir().ok()?;
    find_by_identity_in(&dir.join(FILE_NAME), connection)
}

fn find_by_identity_in(path: &Path, connection: &SavedConnection) -> Option<SavedConnection> {
    let key = connection.key();
    load_file(path)
        .into_iter()
        .rev()
        .find(|existing| existing.key() == key)
}

/// 按连接身份（[`SavedConnection::key`]）删除一条已保存连接。
///
/// 返回是否真的删掉了条目：没命中时**不重写文件**（权限、mtime 与内容都不动），
/// 免得一次误点的「删除」把一份只是没命中的配置重新序列化一遍。
/// 命中时与写入共用「临时文件 + 同目录 rename」，文件权限仍是 0600。
pub fn remove_by_key(identity: &str) -> Result<bool, ConfigError> {
    remove_by_key_in(&config_dir()?, identity)
}

fn remove_by_key_in(dir: &Path, identity: &str) -> Result<bool, ConfigError> {
    let path = dir.join(FILE_NAME);
    let mut connections = load_file(&path);
    let before = connections.len();
    connections.retain(|connection| connection.key() != identity);
    if connections.len() == before {
        return Ok(false);
    }
    let document = Document {
        version: DOCUMENT_VERSION,
        connections,
    };
    write_atomic(dir, &path, &document)?;
    Ok(true)
}

fn load_file(path: &Path) -> Vec<SavedConnection> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Document>(&text) {
        Ok(document) => {
            let (connections, merged) = merge_duplicates(document.connections);
            if merged > 0 {
                // 不静默丢数据：旧版本（身份按凭据引用）可能已经写进重复条目。
                eprintln!(
                    "[sqarvo-desktop] 已合并 {merged} 条重复连接（保留最近一次保存的条目）：{}",
                    path.display()
                );
            }
            connections
        }
        Err(error) => {
            eprintln!(
                "[sqarvo-desktop] 连接配置无法解析（{}）：{error}；本次按空列表处理，不会覆盖已有文件",
                path.display()
            );
            Vec::new()
        }
    }
}

/// 按身份合并重复条目，保留**最后一条**（追加写入在末尾，最后一条即最近一次保存）。
///
/// 返回合并后的列表与合并掉的条数；条目顺序按各自最后出现的位置排列，
/// 未重复的条目位置不变。日志由调用方打，测试直接断言返回值，不依赖 stderr。
fn merge_duplicates(connections: Vec<SavedConnection>) -> (Vec<SavedConnection>, usize) {
    let mut merged: Vec<SavedConnection> = Vec::with_capacity(connections.len());
    let mut removed = 0usize;
    for connection in connections {
        let key = connection.key();
        if let Some(index) = merged.iter().position(|existing| existing.key() == key) {
            merged.remove(index);
            removed += 1;
        }
        merged.push(connection);
    }
    (merged, removed)
}

fn upsert_in(dir: &Path, connection: SavedConnection) -> Result<PathBuf, ConfigError> {
    let path = dir.join(FILE_NAME);
    ensure_dir(dir)?;

    let mut connections = load_file(&path);
    let key = connection.key();
    match connections
        .iter_mut()
        .find(|existing| existing.key() == key)
    {
        Some(existing) => {
            // 命中身份：沿用原条目的凭据引用，避免同一连接的密码在密钥环里出现两份。
            let reference = if connection.credential_ref.is_empty() {
                existing.credential_ref.clone()
            } else {
                connection.credential_ref.clone()
            };
            let mut updated = connection;
            updated.credential_ref = reference;
            *existing = updated;
        }
        None => connections.push(connection),
    }
    let document = Document {
        version: DOCUMENT_VERSION,
        connections,
    };
    write_atomic(dir, &path, &document)?;
    Ok(path)
}

/// 创建（或收紧）配置目录到 0700。
fn ensure_dir(dir: &Path) -> Result<(), ConfigError> {
    fs::create_dir_all(dir).map_err(|error| {
        ConfigError::new(format!("无法创建配置目录 {}：{error}", dir.display()))
    })?;
    restrict(dir, 0o700, "配置目录")
}

/// 把权限收紧到指定模式（目录/文件统一入口）。
fn restrict(path: &Path, mode: u32, what: &str) -> Result<(), ConfigError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, permissions).map_err(|error| {
            ConfigError::new(format!(
                "无法把{what} {} 的权限设为 {mode:o}：{error}",
                path.display()
            ))
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode, what);
    }
    Ok(())
}

/// 写入临时文件（0600）后 rename 到目标路径。
fn write_atomic(dir: &Path, path: &Path, document: &Document) -> Result<(), ConfigError> {
    let text = serde_json::to_string_pretty(document)
        .map_err(|error| ConfigError::new(format!("连接配置序列化失败：{error}")))?;
    let temporary = dir.join(format!(".{FILE_NAME}.tmp"));
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|error| {
            ConfigError::new(format!("无法写入 {}：{error}", temporary.display()))
        })?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                ConfigError::new(format!("写入 {} 失败：{error}", temporary.display()))
            })?;
    }
    // 显式收紧：临时文件可能已存在（上一次崩溃），create 不会改权限。
    restrict(&temporary, 0o600, "配置文件")?;
    fs::rename(&temporary, path).map_err(|error| {
        ConfigError::new(format!(
            "无法把 {} 重命名为 {}：{error}",
            temporary.display(),
            path.display()
        ))
    })?;
    restrict(path, 0o600, "配置文件")
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn entry(reference: &str) -> SavedConnection {
        SavedConnection {
            name: "sqarvo_test@127.0.0.1/sqarvo_test".to_owned(),
            driver: "mariadb".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 53306,
            database: "sqarvo_test".to_owned(),
            username: "sqarvo_test".to_owned(),
            file_path: String::new(),
            read_only: false,
            tls_mode: "preferred".to_owned(),
            credential_ref: reference.to_owned(),
        }
    }

    /// 无需凭据的 SQLite 连接（`credential_ref` 为空）。
    fn sqlite_entry(path: &str) -> SavedConnection {
        SavedConnection {
            name: "demo.db（SQLite）".to_owned(),
            driver: "sqlite".to_owned(),
            host: String::new(),
            port: 0,
            database: String::new(),
            username: String::new(),
            file_path: path.to_owned(),
            read_only: true,
            tls_mode: "disabled".to_owned(),
            credential_ref: String::new(),
        }
    }

    const REF: &str = "sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4";

    #[test]
    fn serialized_entry_has_only_non_sensitive_fields() {
        let json = serde_json::to_string(&entry(REF)).expect("序列化");
        for key in [
            "\"name\"",
            "\"driver\"",
            "\"host\"",
            "\"port\"",
            "\"database\"",
            "\"username\"",
            "\"file_path\"",
            "\"read_only\"",
            "\"tls_mode\"",
            "\"credential_ref\"",
        ] {
            assert!(json.contains(key), "缺少字段 {key}：{json}");
        }
        for forbidden in ["password", "secret", "token", "hunter2"] {
            assert!(
                !json.contains(forbidden),
                "配置里不应出现 {forbidden}：{json}"
            );
        }
    }

    #[test]
    fn credentialless_connections_do_not_overwrite_each_other() {
        // SQLite 没有凭据引用：只按引用去重会把两条不同文件当成同一条。
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("cfg");
        let first = sqlite_entry("/tmp/a.db");
        let second = sqlite_entry("/tmp/b.db");

        upsert_in(&target, first.clone()).expect("第一条");
        upsert_in(&target, second.clone()).expect("第二条");
        let connections = load_file(&target.join(FILE_NAME));
        assert_eq!(
            connections.len(),
            2,
            "两个文件必须各占一条：{connections:?}"
        );

        // 同一个文件再次保存：更新原条目，不追加。
        let mut updated = first.clone();
        updated.read_only = false;
        upsert_in(&target, updated.clone()).expect("更新");
        let connections = load_file(&target.join(FILE_NAME));
        assert_eq!(connections.len(), 2);
        let stored = connections
            .iter()
            .find(|connection| connection.file_path == "/tmp/a.db")
            .expect("应能找到 /tmp/a.db");
        assert!(!stored.read_only, "同一文件的条目应被更新");
        // 仍然不写任何敏感字段。
        let text = fs::read_to_string(target.join(FILE_NAME)).expect("读配置");
        assert!(!text.contains("password") && !text.contains("secret"));
    }

    #[test]
    fn roundtrip_through_document() {
        let document = Document {
            version: DOCUMENT_VERSION,
            connections: vec![entry(REF)],
        };
        let json = serde_json::to_string(&document).expect("序列化");
        let parsed: Document = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(parsed.connections, document.connections);
        assert_eq!(parsed.version, DOCUMENT_VERSION);
    }

    #[test]
    fn upsert_writes_private_file_and_updates_in_place() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("nested");

        let path = upsert_in(&target, entry(REF)).expect("首次写入");
        assert_eq!(path, target.join(FILE_NAME));
        let mode = fs::metadata(&path).expect("stat 文件").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "配置文件必须是 0600，实际 {mode:o}");
        let dir_mode = fs::metadata(&target)
            .expect("stat 目录")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "配置目录必须是 0700，实际 {dir_mode:o}");

        // 同一目标再次写入（名称 / TLS 变了、引用复用）：更新原条目而不是追加。
        let mut updated = entry(REF);
        updated.name = "改过的名字".to_owned();
        updated.tls_mode = "required".to_owned();
        upsert_in(&target, updated).expect("再次写入");
        let connections = load_file(&path);
        assert_eq!(
            connections.len(),
            1,
            "同一目标不能追加第二条：{connections:?}"
        );
        assert_eq!(connections[0].name, "改过的名字");
        assert_eq!(connections[0].tls_mode, "required");
        assert_eq!(connections[0].credential_ref, REF);
    }

    #[test]
    fn same_target_saved_twice_keeps_one_entry_with_last_values() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path();

        let first = entry(REF);
        let mut second = entry(REF);
        second.name = "第二次保存".to_owned();
        second.tls_mode = "verify-full".to_owned();

        upsert_in(target, first).expect("第一次");
        upsert_in(target, second).expect("第二次");

        let connections = load_file(&target.join(FILE_NAME));
        assert_eq!(connections.len(), 1, "同一身份只保留一条：{connections:?}");
        assert_eq!(connections[0].name, "第二次保存");
        assert_eq!(connections[0].tls_mode, "verify-full");
        assert_eq!(connections[0].credential_ref, REF, "引用应复用");
    }

    #[test]
    fn same_target_with_new_reference_updates_instead_of_appending() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path();
        let other = "sqarvo:conn/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";

        upsert_in(target, entry(REF)).expect("第一次");
        upsert_in(target, entry(other)).expect("第二次");

        let connections = load_file(&target.join(FILE_NAME));
        assert_eq!(connections.len(), 1, "身份不看引用：{connections:?}");
        assert_eq!(connections[0].credential_ref, other, "应采用新的引用");
    }

    #[test]
    fn different_database_user_or_port_stay_separate() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path();

        let mut other_database = entry(REF);
        other_database.database = "another_db".to_owned();
        let mut other_user = entry(REF);
        other_user.username = "another_user".to_owned();
        let mut other_port = entry(REF);
        other_port.port = 3306;

        for (label, connection) in [
            ("基准", entry(REF)),
            ("换库", other_database),
            ("换用户", other_user),
            ("换端口", other_port),
        ] {
            upsert_in(target, connection).expect(label);
        }

        let connections = load_file(&target.join(FILE_NAME));
        assert_eq!(
            connections.len(),
            4,
            "不同目标必须各自独立：{connections:?}"
        );
    }

    #[test]
    fn server_host_is_case_insensitive_and_empty_database_participates() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path();

        let mut lower = entry(REF);
        lower.host = "LocalHost".to_owned();
        let mut upper = entry(REF);
        upper.host = "LOCALHOST".to_owned();
        let mut empty_database = entry(REF);
        empty_database.database = String::new();

        upsert_in(target, lower).expect("小写主机");
        upsert_in(target, upper).expect("大写主机");
        assert_eq!(
            load_file(&target.join(FILE_NAME)).len(),
            1,
            "主机大小写不敏感"
        );

        upsert_in(target, empty_database).expect("空库名");
        let connections = load_file(&target.join(FILE_NAME));
        assert_eq!(connections.len(), 2, "空库名是另一种目标：{connections:?}");
    }

    #[test]
    fn sqlite_paths_that_normalize_to_the_same_file_share_one_entry() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path();
        let file = dir.path().join("a.db");
        let direct = file.display().to_string();
        let dotted = dir.path().join(".").join("a.db").display().to_string();
        let upper = dir.path().join("A.DB").display().to_string();

        assert_eq!(normalized_file_path("a.db"), normalized_file_path("./a.db"));
        assert_eq!(
            normalized_file_path("/tmp/nested/../a.db"),
            normalized_file_path("/tmp/a.db")
        );
        assert_eq!(normalized_file_path(&direct), normalized_file_path(&dotted));
        assert_eq!(normalized_file_path(&direct), normalized_file_path(&upper));

        upsert_in(target, sqlite_entry(&direct)).expect("直接路径");
        upsert_in(target, sqlite_entry(&dotted)).expect("./ 路径");
        upsert_in(target, sqlite_entry(&upper)).expect("大小写不同");
        let connections = load_file(&target.join(FILE_NAME));
        assert_eq!(
            connections.len(),
            1,
            "规范化后同一文件只保留一条：{connections:?}"
        );
        assert_eq!(connections[0].file_path, upper, "保留最近一次保存的路径");
    }

    #[test]
    fn loading_a_file_with_duplicates_merges_keeping_the_last() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join(FILE_NAME);

        let mut first = entry(REF);
        first.name = "旧的名字".to_owned();
        let mut last = entry("sqarvo:conn/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d");
        last.name = "最近一次保存".to_owned();
        let document = Document {
            version: DOCUMENT_VERSION,
            connections: vec![first, sqlite_entry("/tmp/other.db"), last],
        };
        fs::write(
            &path,
            serde_json::to_string(&document).expect("序列化重复配置"),
        )
        .expect("写入");

        let connections = load_file(&path);
        assert_eq!(connections.len(), 2, "重复条目应合并：{connections:?}");
        let target = connections
            .iter()
            .find(|connection| connection.driver == "mariadb")
            .expect("服务器条目");
        assert_eq!(target.name, "最近一次保存");
        assert_eq!(
            target.credential_ref, "sqarvo:conn/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d",
            "保留最后一条（最近一次保存）"
        );
    }

    #[test]
    fn find_by_identity_returns_the_most_recent_entry() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join(FILE_NAME);

        let mut older = entry(REF);
        older.name = "旧的".to_owned();
        let mut newer = entry("sqarvo:conn/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d");
        newer.name = "新的".to_owned();
        let document = Document {
            version: DOCUMENT_VERSION,
            connections: vec![older, newer],
        };
        fs::write(&path, serde_json::to_string(&document).expect("序列化")).expect("写入");

        let found = find_by_identity_in(&path, &entry(REF)).expect("应命中同一身份");
        assert_eq!(found.name, "新的");
        assert_eq!(
            found.credential_ref,
            "sqarvo:conn/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d"
        );

        let mut other = entry(REF);
        other.database = "elsewhere".to_owned();
        assert!(find_by_identity_in(&path, &other).is_none());
    }

    #[test]
    fn missing_or_broken_file_reads_as_empty() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join(FILE_NAME);
        assert!(load_file(&path).is_empty());

        fs::write(&path, "{ not json").expect("写入坏文件");
        assert!(load_file(&path).is_empty());
    }

    #[test]
    fn config_dir_lives_under_sqarvo_project_dir() {
        if let Ok(dir) = config_dir() {
            assert!(dir.ends_with("sqarvo/Sqarvo"), "{}", dir.display());
            assert_eq!(dir.join(FILE_NAME).file_name().unwrap(), FILE_NAME);
        }
    }

    // -- #144：删除连接（启动态侧栏右键「删除连接」） ------------------------

    #[test]
    fn remove_by_key_deletes_only_the_named_connection() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("cfg");
        let keep = sqlite_entry("/tmp/keep.db");
        let drop = entry(REF);
        let also_keep = {
            let mut other = entry(REF);
            other.database = "another_db".to_owned();
            other
        };
        upsert_in(&target, keep.clone()).expect("SQLite 条目");
        upsert_in(&target, drop.clone()).expect("要删除的条目");
        upsert_in(&target, also_keep.clone()).expect("同凭据但不同目标");

        let removed = remove_by_key_in(&target, &drop.key()).expect("删除");
        assert!(removed, "命中的身份应报告删除成功");

        let connections = load_file(&target.join(FILE_NAME));
        assert_eq!(connections.len(), 2, "只删一条：{connections:?}");
        assert!(
            connections.iter().all(|row| row.key() != drop.key()),
            "被删的连接不应残留：{connections:?}"
        );
        assert!(
            connections.iter().any(|row| row.key() == keep.key()),
            "其它驱动/目标的条目必须保留：{connections:?}"
        );
        assert!(
            connections.iter().any(|row| row.key() == also_keep.key()),
            "同一凭据引用但不同目标不是同一条连接：{connections:?}"
        );
    }

    #[test]
    fn remove_by_key_without_match_leaves_the_file_untouched() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path();
        let path = target.join(FILE_NAME);
        upsert_in(target, entry(REF)).expect("写入");
        let before = fs::read_to_string(&path).expect("读配置");
        let before_mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;

        let removed = remove_by_key_in(target, "mariadb|nowhere|1|nobody|none").expect("尝试删除");
        assert!(!removed, "未命中必须如实返回 false");

        let after = fs::read_to_string(&path).expect("读配置");
        assert_eq!(before, after, "未命中不应重写配置文件");
        let after_mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(before_mode, after_mode, "未命中不应改动文件权限");
    }

    #[test]
    fn remove_by_key_keeps_private_permissions() {
        let dir = tempfile::tempdir().expect("临时目录");
        let target = dir.path().join("nested");
        upsert_in(&target, entry(REF)).expect("写入");
        let path = target.join(FILE_NAME);
        assert!(remove_by_key_in(&target, &entry(REF).key()).expect("删除"));

        let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "删除后配置文件仍是 0600，实际 {mode:o}");
        let dir_mode = fs::metadata(&target)
            .expect("stat 目录")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "配置目录仍是 0700，实际 {dir_mode:o}");
        assert!(load_file(&path).is_empty(), "删掉最后一条后列表为空");
    }

    #[test]
    fn remove_by_key_on_missing_file_is_a_no_op() {
        let dir = tempfile::tempdir().expect("临时目录");
        let removed = remove_by_key_in(dir.path(), "sqlite|file|/tmp/none.db").expect("删除");
        assert!(!removed);
        assert!(
            !dir.path().join(FILE_NAME).exists(),
            "没有配置时不应凭空创建文件"
        );
    }
}
