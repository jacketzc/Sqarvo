//! 多进程共用的连接档案与 MCP 访问策略。

mod files;
pub mod migration;

use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::path::{Component, PathBuf};

use serde::{Deserialize, Serialize};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::ConnectionParams;
use time::OffsetDateTime;
use uuid::{Uuid, Version};

use crate::AppPaths;
use crate::file_lock::LockMode;

const SCHEMA_VERSION: u32 = 2;
const FILE_NAME: &str = "connections.json";

/// MCP 共享档案访问策略。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpAccessPolicy {
    /// 是否授权 MCP 访问共享档案。
    pub enabled: bool,
    /// 是否仅允许读取操作。
    pub read_only: bool,
}

impl Default for McpAccessPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            read_only: true,
        }
    }
}

/// 共享连接档案。凭据由系统凭据库解析。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionProfile {
    /// 创建时用 nil UUID 请求生成标识，存储的档案均为 UUID v4。
    pub profile_id: Uuid,
    /// 当前档案的修订，首次保存为 1。
    pub revision: u64,
    /// 档案名称，用于显示及 MCP 名称匹配。
    pub name: String,
    /// 驱动参数，展平为档案 JSON 的字段。
    #[serde(flatten)]
    pub params: ConnectionParams,
    /// SQLite 的兼容字段，读取和写入时与驱动的 host 路径统一。
    #[serde(default)]
    pub file_path: String,
    /// GUI 使用的可选显示名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 上次使用的时刻，使用 RFC 3339 保存。
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_used: Option<OffsetDateTime>,
}

impl ConnectionProfile {
    fn valid_id(id: Uuid) -> bool {
        id.get_version() == Some(Version::Random) && id.get_variant() == uuid::Variant::RFC4122
    }

    fn normalize_sqlite_path(&mut self) -> Result<()> {
        if self.params.driver_id.as_str() != "sqlite" {
            return Ok(());
        }
        // host 是驱动使用的路径。旧文档只填写一处时，用已填写的值补齐。
        if self.params.host.is_empty() {
            self.params.host.clone_from(&self.file_path);
        }
        if self.file_path.is_empty() {
            self.file_path.clone_from(&self.params.host);
        }
        if self.params.host != self.file_path {
            return Err(invalid("SQLite 档案的 file_path 与 host 路径不一致"));
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if !Self::valid_id(self.profile_id) || self.revision == 0 {
            return Err(invalid("连接档案的标识或修订非法"));
        }
        if self.name.trim().is_empty() || self.name.chars().any(char::is_control) {
            return Err(invalid("连接档案的名称非法"));
        }
        self.params
            .validate()
            .map_err(|_| invalid("连接档案的连接参数非法"))?;
        if let Some(reference) = self.params.credential_ref.as_deref() {
            crate::validate_reference(reference).map_err(|_| invalid("连接档案的凭据引用非法"))?;
        }
        if self.params.properties.keys().any(|key| {
            let key = key.to_ascii_lowercase();
            ["password", "passwd", "secret", "token"]
                .iter()
                .any(|part| key.contains(part))
        }) {
            return Err(invalid("连接档案的扩展参数不得携带凭据"));
        }
        Ok(())
    }

    pub(super) fn same_target(&self, other: &Self) -> bool {
        let params = &self.params;
        let other_params = &other.params;
        if params.driver_id != other_params.driver_id {
            return false;
        }
        if cfg!(target_os = "linux") && params.driver_id.as_str() == "sqlite" {
            // 保留路径大小写及原始拼写。折叠 .. 或解析符号链接可能改变实际目标。
            return params.host == other_params.host;
        }
        if !self.file_path.is_empty() || !other.file_path.is_empty() {
            return !self.file_path.is_empty()
                && !other.file_path.is_empty()
                && normalized_file_path(&self.file_path) == normalized_file_path(&other.file_path);
        }
        params.host.to_lowercase() == other_params.host.to_lowercase()
            && params.port == other_params.port
            && params.username.as_deref().unwrap_or_default()
                == other_params.username.as_deref().unwrap_or_default()
            && params.database.as_deref().unwrap_or_default()
                == other_params.database.as_deref().unwrap_or_default()
    }
}

fn normalized_file_path(path: &str) -> String {
    // 允许保存尚不存在的文件。绝对路径只做词法规范化，不解析符号链接。
    let absolute = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.file_name().is_some() {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized.to_string_lossy().to_lowercase()
}

/// 一个旧文件的已迁移指纹。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationEntry {
    /// 旧文件内容的 SHA-256 十六进制摘要。
    pub sha256: String,
    /// 导入时刻，使用 RFC 3339 保存。
    #[serde(with = "time::serde::rfc3339")]
    pub migrated_at: OffsetDateTime,
}

/// 同一次持锁读取获得的共享档案、访问策略和文档修订。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDocument {
    /// 持久格式版本。
    pub schema_version: u32,
    /// 文档全局修订，从 0 开始。
    pub revision: u64,
    /// MCP 访问策略。
    #[serde(default)]
    pub mcp: McpAccessPolicy,
    /// 当前所有档案。
    pub profiles: Vec<ConnectionProfile>,
    /// 已迁移旧文件的路径、指纹、时间。
    pub migration_state: BTreeMap<String, MigrationEntry>,
}

impl Default for ProfileDocument {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            mcp: McpAccessPolicy::default(),
            profiles: Vec::new(),
            migration_state: BTreeMap::new(),
        }
    }
}

impl ProfileDocument {
    fn normalize_paths(&mut self) -> Result<()> {
        self.profiles
            .iter_mut()
            .try_for_each(ConnectionProfile::normalize_sqlite_path)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid("不支持该共享连接档案格式版本"));
        }
        if !self.mcp.enabled && !self.mcp.read_only {
            return Err(invalid("仅在 MCP 数据访问开启时才可关闭只读"));
        }
        let mut ids = HashSet::with_capacity(self.profiles.len());
        for profile in &self.profiles {
            profile.validate()?;
            if !ids.insert(profile.profile_id) {
                return Err(invalid("共享连接档案包含重复标识"));
            }
        }
        if self.migration_state.iter().any(|(path, entry)| {
            path.is_empty()
                || entry.sha256.len() != 64
                || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(invalid("共享连接档案的迁移记录非法"));
        }
        Ok(())
    }

    /// 优先按 UUID 查找，再按唯一名称解析 MCP 别名。
    pub fn resolve(&self, alias: &str) -> Result<&ConnectionProfile> {
        if let Some(profile) = self.profiles.iter().find(|profile| {
            profile
                .profile_id
                .hyphenated()
                .encode_lower(&mut Uuid::encode_buffer())
                == alias
        }) {
            return Ok(profile);
        }
        unique_match(self.profiles.iter().filter(|p| p.name == alias))?.ok_or_else(not_found)
    }
}

fn unique_match<T>(mut matches: impl Iterator<Item = T>) -> Result<Option<T>> {
    let first = matches.next();
    if matches.next().is_some() {
        return Err(Error::failed_precondition(
            ErrorCode::PROFILE_AMBIGUOUS_NAME,
            "匹配到多个连接档案，请指定档案标识",
        ));
    }
    Ok(first)
}

fn invalid(message: &str) -> Error {
    Error::invalid_argument(ErrorCode::PROFILE_INVALID, message)
}

fn not_found() -> Error {
    Error::not_found(ErrorCode::PROFILE_NOT_FOUND, "连接档案不存在")
}

fn revision_conflict() -> Error {
    Error::failed_precondition(
        ErrorCode::PROFILE_REVISION_CONFLICT,
        "连接档案已变化，请重新读取后重试",
    )
}

/// 共享档案存储。每个操作同步获取文件锁；异步调用方应使用阻塞任务池。
#[derive(Clone, Debug)]
pub struct ProfileStore {
    config_dir: PathBuf,
    #[cfg(windows)]
    windows_legacy_dir: Option<PathBuf>,
}

impl ProfileStore {
    /// 绑定应用配置目录，构造时无磁盘 I/O。
    #[must_use]
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            config_dir: paths.config_dir().to_owned(),
            #[cfg(windows)]
            windows_legacy_dir: paths.windows_legacy_dir().map(std::path::Path::to_owned),
        }
    }

    /// 在共享锁下读取并校验完整文档。
    pub fn snapshot(&self) -> Result<ProfileDocument> {
        let _lock = files::lock(&self.config_dir, LockMode::Shared)?;
        self.load()
    }

    /// 列出当前档案。
    pub fn list(&self) -> Result<Vec<ConnectionProfile>> {
        Ok(self.snapshot()?.profiles)
    }

    /// 读取指定档案。
    pub fn get(&self, id: Uuid) -> Result<ConnectionProfile> {
        self.list()?
            .into_iter()
            .find(|profile| profile.profile_id == id)
            .ok_or_else(not_found)
    }

    /// 创建或更新一个档案。
    ///
    /// nil UUID 按连接目标匹配，未找到目标且未传预期修订时创建。更新必须提供
    /// 当前修订，显式 UUID 只更新已存在的档案。
    pub fn save(
        &self,
        mut profile: ConnectionProfile,
        expected_revision: Option<u64>,
    ) -> Result<ConnectionProfile> {
        profile.normalize_sqlite_path()?;
        self.update(|doc| {
            let existing = if profile.profile_id.is_nil() {
                unique_match(
                    doc.profiles
                        .iter()
                        .enumerate()
                        .filter_map(|(index, saved)| saved.same_target(&profile).then_some(index)),
                )?
            } else {
                if !ConnectionProfile::valid_id(profile.profile_id) {
                    return Err(invalid("连接档案的标识非法"));
                }
                Some(
                    doc.profiles
                        .iter()
                        .position(|saved| saved.profile_id == profile.profile_id)
                        .ok_or_else(not_found)?,
                )
            };

            if let Some(index) = existing {
                let saved = &doc.profiles[index];
                if expected_revision != Some(saved.revision) {
                    return Err(revision_conflict());
                }
                if !saved.same_target(&profile)
                    && doc.profiles.iter().any(|other| other.same_target(&profile))
                {
                    return Err(Error::already_exists(
                        ErrorCode::PROFILE_INVALID,
                        "连接目标已有保存的档案",
                    ));
                }
                profile.profile_id = saved.profile_id;
                profile.revision = next_revision(saved.revision)?;
                doc.profiles[index] = profile.clone();
            } else {
                if expected_revision.is_some() {
                    return Err(revision_conflict());
                }
                profile.profile_id = Uuid::new_v4();
                profile.revision = 1;
                doc.profiles.push(profile.clone());
            }
            Ok(profile)
        })
    }

    /// 按标识与预期修订删除档案。系统凭据引用独立管理。
    pub fn delete(&self, id: Uuid, expected_revision: u64) -> Result<()> {
        self.update(|doc| {
            let index = doc
                .profiles
                .iter()
                .position(|profile| profile.profile_id == id)
                .ok_or_else(not_found)?;
            if doc.profiles[index].revision != expected_revision {
                return Err(revision_conflict());
            }
            doc.profiles.remove(index);
            Ok(())
        })
    }

    /// 从最新文档读取 MCP 访问策略。
    pub fn get_mcp_access(&self) -> Result<McpAccessPolicy> {
        Ok(self.snapshot()?.mcp)
    }

    /// 原子更新 MCP 访问策略。
    pub fn set_mcp_access(&self, policy: McpAccessPolicy) -> Result<McpAccessPolicy> {
        self.update_mcp_access(|_| Ok(policy))
            .map(|(policy, _)| policy)
    }

    /// 持锁读改写策略，返回最终策略与文档修订。
    pub fn update_mcp_access(
        &self,
        update: impl FnOnce(McpAccessPolicy) -> Result<McpAccessPolicy>,
    ) -> Result<(McpAccessPolicy, u64)> {
        let _lock = files::lock(&self.config_dir, LockMode::Exclusive)?;
        let mut doc = self.load()?;
        doc.mcp = update(doc.mcp)?;
        self.commit(&mut doc)?;
        Ok((doc.mcp, doc.revision))
    }

    /// 持独占锁清理上次中断遗留的临时文件，并尝试导入旧档案。
    pub fn run_once_migration(&self) -> Result<()> {
        migration::run_once(self)
    }

    fn with_migration(
        &self,
        run: impl FnOnce(
            &mut ProfileDocument,
            &dyn Fn(&mut ProfileDocument) -> Result<()>,
        ) -> Result<()>,
    ) -> Result<()> {
        let _lock = files::lock(&self.config_dir, LockMode::Exclusive)?;
        files::clear_temporary_files(&self.config_dir)?;
        let mut doc = self.load()?;
        run(&mut doc, &|doc| self.commit(doc))
    }

    fn load(&self) -> Result<ProfileDocument> {
        let path = self.config_dir.join(FILE_NAME);
        let mut doc = match files::read(&path) {
            Ok(payload) => serde_json::from_slice::<ProfileDocument>(&payload)
                .map_err(|_| invalid("共享连接档案 JSON 格式非法"))?,
            Err(err) if err.kind() == ErrorKind::NotFound => ProfileDocument::default(),
            Err(err) => return Err(files::safe_io_error(err)),
        };
        doc.normalize_paths()?;
        doc.validate()?;
        Ok(doc)
    }

    fn update<T>(&self, change: impl FnOnce(&mut ProfileDocument) -> Result<T>) -> Result<T> {
        let _lock = files::lock(&self.config_dir, LockMode::Exclusive)?;
        let mut doc = self.load()?;
        let result = change(&mut doc)?;
        self.commit(&mut doc)?;
        Ok(result)
    }

    fn commit(&self, doc: &mut ProfileDocument) -> Result<()> {
        doc.normalize_paths()?;
        doc.validate()?;
        doc.revision = next_revision(doc.revision)?;
        let payload = serde_json::to_vec_pretty(doc)
            .map_err(|_| Error::internal("共享连接档案序列化失败"))?;
        atomic_write(&self.config_dir.join(FILE_NAME), &payload)
    }
}

fn next_revision(revision: u64) -> Result<u64> {
    revision
        .checked_add(1)
        .ok_or_else(|| invalid("共享连接档案的修订已达到上限"))
}

use files::atomic_write;
