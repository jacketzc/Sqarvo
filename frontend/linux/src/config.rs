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

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    /// 条目身份：有凭据时按凭据引用，没有凭据（SQLite）时按驱动 + 路径。
    ///
    /// 只按 `credential_ref` 去重会让所有 SQLite 连接（引用都是空串）互相覆盖，
    /// 导致连接列表永远只剩最后一条。
    ///
    /// #114 起这个身份也是连接注册表里的节点键（树上的连接节点与之绑定），
    /// 所以它是 `pub` 的：注册表要用它把「刚建立的会话」对回「哪一条连接」。
    #[must_use]
    pub fn key(&self) -> String {
        if self.credential_ref.is_empty() {
            format!("{}|{}|{}", self.driver, self.host, self.file_path)
        } else {
            self.credential_ref.clone()
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
/// 以 `credential_ref` 作为标识：同一个引用视为同一条连接（用户改了主机/库名时更新原条目，
/// 不会堆出多条指向同一份密码的记录）。
pub fn upsert(connection: SavedConnection) -> Result<PathBuf, ConfigError> {
    upsert_in(&config_dir()?, connection)
}

fn load_file(path: &Path) -> Vec<SavedConnection> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Document>(&text) {
        Ok(document) => document.connections,
        Err(error) => {
            eprintln!(
                "[sqarvo-desktop] 连接配置无法解析（{}）：{error}；本次按空列表处理，不会覆盖已有文件",
                path.display()
            );
            Vec::new()
        }
    }
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
        Some(existing) => *existing = connection,
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

        // 同一引用再次写入：更新原条目而不是追加。
        let mut updated = entry(REF);
        updated.database = "renamed_db".to_owned();
        upsert_in(&target, updated.clone()).expect("再次写入");
        let connections = load_file(&path);
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].database, "renamed_db");

        // 不同引用：追加。
        let other = "sqarvo:conn/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
        upsert_in(&target, entry(other)).expect("第二条");
        let connections = load_file(&path);
        assert_eq!(connections.len(), 2);
        assert!(connections.iter().any(|c| c.credential_ref == other));
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
}
