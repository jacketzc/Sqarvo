//! 将旧版 GTK、Windows 和 MCP 档案导入共享档案库。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{ConnectionParams, DriverId, TlsMode};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ConnectionProfile, MigrationEntry, ProfileStore, atomic_write};

const MAX_MCP_BYTES: u64 = 1024 * 1024;
const MAX_DESKTOP_BYTES: u64 = 16 * 1024 * 1024;
const MCP_FILE_NAME: &str = "mcp.json";
const PROFILE_FILE_NAME: &str = "connections.json";

#[derive(Clone, Copy)]
enum SourceKind {
    Gtk,
    #[cfg(any(windows, test))]
    Windows,
    Mcp,
}

impl SourceKind {
    const fn max_bytes(self) -> u64 {
        match self {
            Self::Gtk => MAX_DESKTOP_BYTES,
            #[cfg(any(windows, test))]
            Self::Windows => MAX_DESKTOP_BYTES,
            Self::Mcp => MAX_MCP_BYTES,
        }
    }
}

struct Source {
    path: PathBuf,
    kind: SourceKind,
}

struct Import {
    path: PathBuf,
    sha256: String,
    profiles: Vec<ConnectionProfile>,
    /// 服务器级配置作为原文件的替换内容，GUI 旧文件只保留备份。
    replacement: Option<Vec<u8>>,
    max_bytes: u64,
}

/// 查找并迁移平台旧配置。成功的导入与备份可安全重试。
pub fn run_once(store: &ProfileStore) -> Result<()> {
    run_sources(store, default_sources(store))
}

/// 迁移默认源以及 `--config` 指定的旧 MCP 文件。
pub fn run_once_with_mcp(store: &ProfileStore, mcp: &Path) -> Result<()> {
    let mut sources = default_sources(store);
    sources.push(Source {
        path: mcp.to_owned(),
        kind: SourceKind::Mcp,
    });
    run_sources(store, sources)
}

fn default_sources(store: &ProfileStore) -> Vec<Source> {
    let mut sources = Vec::with_capacity(3);
    if cfg!(target_os = "linux") {
        sources.push(Source {
            path: store.config_dir.join("Sqarvo").join(PROFILE_FILE_NAME),
            kind: SourceKind::Gtk,
        });
    }
    #[cfg(windows)]
    if let Some(dir) = &store.windows_legacy_dir {
        sources.push(Source {
            path: dir.join(PROFILE_FILE_NAME),
            kind: SourceKind::Windows,
        });
    }
    sources.push(Source {
        path: store.config_dir.join(MCP_FILE_NAME),
        kind: SourceKind::Mcp,
    });
    sources
}

fn run_sources(store: &ProfileStore, sources: Vec<Source>) -> Result<()> {
    store.with_migration(|doc, commit| {
        let target = store.config_dir.join(PROFILE_FILE_NAME);
        let canonical_target = target.canonicalize().ok();
        let mut seen_paths = BTreeSet::new();
        let mut imports = Vec::new();
        let mut changed = false;

        for source in sources {
            let Some(path) = resolve_source(&source.path)? else {
                continue;
            };
            if canonical_target.as_ref() == Some(&path) || !seen_paths.insert(path.clone()) {
                continue;
            }
            let Some(bytes) = read_source(&path, source.kind.max_bytes())? else {
                return Err(source_changed());
            };
            let Some(mut import) = parse_source(path, source.kind, &bytes)? else {
                continue;
            };
            let key = import
                .path
                .to_str()
                .ok_or_else(|| invalid("The legacy connection profile path is not valid UTF-8"))?;
            if doc
                .migration_state
                .get(key)
                .is_none_or(|entry| entry.sha256 != import.sha256)
            {
                doc.profiles.append(&mut import.profiles);
                doc.migration_state.insert(
                    key.to_owned(),
                    MigrationEntry {
                        sha256: import.sha256.clone(),
                        migrated_at: OffsetDateTime::now_utc(),
                    },
                );
                changed = true;
            }
            imports.push(import);
        }

        // 导入和指纹必须一起持久化。若之后的备份或改名失败，重试只完成剩余操作。
        if changed {
            commit(doc)?;
        }
        for import in imports {
            finish(import)?;
        }
        Ok(())
    })
}

fn resolve_source(path: &Path) -> Result<Option<PathBuf>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Err(invalid(
            "The legacy connection profile must be a regular file",
        ));
    }
    path.canonicalize().map(Some).map_err(Error::from)
}

fn read_source(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::from)?;
    if bytes.len() as u64 > max_bytes {
        return Err(invalid(
            "The legacy connection profile exceeds the size limit",
        ));
    }
    Ok(Some(bytes))
}

fn fingerprint(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut hex = String::with_capacity(64);
    for byte in &Sha256::digest(bytes) {
        // Formatting into a String cannot fail.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn parse_source(path: PathBuf, kind: SourceKind, bytes: &[u8]) -> Result<Option<Import>> {
    let invalid_json = |_| invalid("Invalid legacy connection profile format");
    let (profiles, replacement) = if matches!(kind, SourceKind::Mcp) {
        parse_mcp(bytes)?
    } else {
        let legacy: LegacyDesktop = serde_json::from_slice(bytes).map_err(invalid_json)?;
        if legacy.version != 1 && !(matches!(kind, SourceKind::Gtk) && legacy.version == 0) {
            return Err(invalid(
                "The legacy desktop connection profile version is not supported",
            ));
        }
        let mut profiles: Vec<ConnectionProfile> = Vec::with_capacity(legacy.connections.len());
        for entry in legacy.connections {
            let profile = entry.into_profile()?;
            // Linux SQLite 同一文件也可以有不同的名称或只读模式，迁移时保留这些条目。
            let same_entry = |old: &ConnectionProfile| {
                old.same_target(&profile)
                    && (!matches!(kind, SourceKind::Gtk)
                        || profile.params.driver_id.as_str() != "sqlite"
                        || (old.name == profile.name
                            && old.display_name == profile.display_name
                            && old.params == profile.params))
            };
            if let Some(index) = profiles.iter().position(same_entry) {
                profiles.remove(index);
            }
            profiles.push(profile);
        }
        (profiles, None)
    };
    if matches!(kind, SourceKind::Mcp) && replacement.is_none() {
        return Ok(None);
    }
    Ok(Some(Import {
        path,
        sha256: fingerprint(bytes),
        profiles,
        replacement,
        max_bytes: kind.max_bytes(),
    }))
}

fn parse_mcp(bytes: &[u8]) -> Result<(Vec<ConnectionProfile>, Option<Vec<u8>>)> {
    let legacy: LegacyMcp = serde_json::from_slice(bytes)
        .map_err(|_| invalid("Invalid legacy MCP configuration format"))?;
    let Some(connections) = legacy.connections else {
        return Ok((Vec::new(), None));
    };
    let mut aliases = BTreeSet::new();
    let mut profiles = Vec::with_capacity(connections.len());
    for entry in connections {
        if !aliases.insert(entry.alias.clone()) {
            return Err(invalid("Duplicate legacy MCP connection name"));
        }
        profiles.push(entry.into_profile()?);
    }
    let mut settings: Map<String, Value> = serde_json::from_slice(bytes)
        .map_err(|_| invalid("Invalid legacy MCP configuration format"))?;
    settings.remove("connections");
    let replacement = serde_json::to_vec_pretty(&settings)
        .map_err(|_| Error::internal("Failed to serialize the MCP server configuration"))?;
    Ok((profiles, Some(replacement)))
}

fn finish(import: Import) -> Result<()> {
    ensure_fingerprint(&import.path, &import.sha256, import.max_bytes)?;
    let backup = backup_path(&import.path, ".bak");
    if !ensure_backup(&import.path, &backup, &import.sha256, import.max_bytes)?
        && !ensure_backup(
            &import.path,
            &backup_path(&import.path, &format!(".bak-{}", import.sha256)),
            &import.sha256,
            import.max_bytes,
        )?
    {
        return Err(invalid(
            "A backup of the legacy connection profile already exists. The profile could not be backed up safely",
        ));
    }
    ensure_fingerprint(&import.path, &import.sha256, import.max_bytes)?;

    if let Some(settings) = import.replacement {
        atomic_write(&import.path, &settings)?;
    } else {
        fs::remove_file(&import.path).map_err(Error::from)?;
        sync_parent(&import.path)?;
    }
    Ok(())
}

fn ensure_backup(source: &Path, backup: &Path, sha256: &str, max_bytes: u64) -> Result<bool> {
    // 原文件与备份在同一目录。先建立不会覆盖已有备份的链接，
    // 再原子替换或移除原路径，进程中断后可按指纹继续完成。
    match fs::hard_link(source, backup) {
        Ok(()) => {
            sync_backup(backup)?;
            sync_parent(backup)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if resolve_source(backup)?.is_none() {
                return Err(source_changed());
            }
            Ok(read_source(backup, max_bytes)?.is_some_and(|bytes| fingerprint(&bytes) == sha256))
        }
        Err(error) => Err(error.into()),
    }
}

fn sync_backup(path: &Path) -> Result<()> {
    #[cfg(windows)]
    let file = fs::OpenOptions::new().write(true).open(path);
    #[cfg(not(windows))]
    let file = File::open(path);
    file.and_then(|file| file.sync_all()).map_err(Error::from)
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(Error::from)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn ensure_fingerprint(path: &Path, sha256: &str, max_bytes: u64) -> Result<()> {
    if read_source(path, max_bytes)?.is_some_and(|bytes| fingerprint(&bytes) == sha256) {
        Ok(())
    } else {
        Err(source_changed())
    }
}

fn backup_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn invalid(message: &'static str) -> Error {
    Error::invalid_argument(ErrorCode::PROFILE_INVALID, message)
}

fn source_changed() -> Error {
    Error::failed_precondition(
        ErrorCode::PROFILE_INVALID,
        "The legacy connection profile changed during migration. Retry the operation",
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDesktop {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    connections: Vec<LegacyDesktopProfile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDesktopProfile {
    name: String,
    #[serde(default)]
    alias: String,
    driver: String,
    host: String,
    port: u16,
    database: String,
    username: String,
    #[serde(default)]
    file_path: String,
    #[serde(default)]
    read_only: bool,
    tls_mode: String,
    credential_ref: String,
}

impl LegacyDesktopProfile {
    fn into_profile(self) -> Result<ConnectionProfile> {
        let tls = match self.tls_mode.as_str() {
            "disabled" => TlsMode::Disabled,
            "preferred" => TlsMode::Preferred,
            "required" => TlsMode::Required,
            "verify-ca" => TlsMode::VerifyCa,
            "verify-full" => TlsMode::VerifyFull,
            _ => return Err(invalid("Invalid TLS mode in the legacy desktop profile")),
        };
        let mut properties = BTreeMap::new();
        let host = if self.driver == "sqlite" {
            properties.insert("read_only".to_owned(), self.read_only.to_string());
            self.file_path.clone()
        } else {
            self.host
        };
        let params = ConnectionParams {
            driver_id: DriverId::new(self.driver)?,
            host,
            port: self.port,
            database: nonempty(self.database),
            username: nonempty(self.username),
            credential_ref: nonempty(self.credential_ref),
            tls,
            tls_server_name: None,
            properties,
        };
        let display_name = nonempty(self.alias.trim().to_owned());
        let name = display_name.clone().unwrap_or(self.name);
        Ok(ConnectionProfile {
            profile_id: Uuid::new_v4(),
            revision: 1,
            name,
            params,
            file_path: self.file_path,
            last_used: None,
            display_name,
        })
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMcp {
    #[serde(default, deserialize_with = "present_connections")]
    connections: Option<Vec<LegacyMcpProfile>>,
    #[serde(rename = "enabled_tools")]
    _enabled_tools: Option<Vec<String>>,
    #[serde(rename = "query_limits")]
    _query_limits: Option<LegacyQueryLimits>,
    #[serde(default, rename = "mariadb_write_profiles")]
    _mariadb_write_profiles: BTreeMap<Uuid, u64>,
}

// V1 接受缺失或数组；显式 null 是格式错误。
fn present_connections<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<LegacyMcpProfile>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyQueryLimits {
    #[serde(rename = "max_rows")]
    _max_rows: Option<u32>,
    #[serde(rename = "timeout_ms")]
    _timeout_ms: Option<u64>,
    #[serde(rename = "max_result_bytes")]
    _max_result_bytes: Option<u64>,
    #[serde(rename = "max_concurrency")]
    _max_concurrency: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMcpProfile {
    alias: String,
    driver_id: String,
    host: String,
    #[serde(default)]
    port: u16,
    database: Option<String>,
    username: Option<String>,
    credential_ref: Option<String>,
    #[serde(default, deserialize_with = "mcp_tls")]
    tls: TlsMode,
    tls_server_name: Option<String>,
    #[serde(default)]
    properties: BTreeMap<String, String>,
    // 旧授权字段必须合法，但不能把它带入共享文档。
    #[serde(rename = "enabled")]
    _enabled: bool,
}

fn mcp_tls<'de, D>(deserializer: D) -> std::result::Result<TlsMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match String::deserialize(deserializer)?.as_str() {
        "unspecified" => Ok(TlsMode::Unspecified),
        "disabled" => Ok(TlsMode::Disabled),
        "preferred" => Ok(TlsMode::Preferred),
        "required" => Ok(TlsMode::Required),
        "verify_ca" => Ok(TlsMode::VerifyCa),
        "verify_full" => Ok(TlsMode::VerifyFull),
        _ => Err(serde::de::Error::custom(
            "Invalid TLS mode in the legacy MCP configuration",
        )),
    }
}

impl LegacyMcpProfile {
    fn into_profile(self) -> Result<ConnectionProfile> {
        let alias_is_valid = (1..=64).contains(&self.alias.len())
            && self.alias.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
            });
        if !alias_is_valid {
            return Err(invalid("Invalid legacy MCP connection name"));
        }
        let file_path = if self.driver_id == "sqlite" {
            self.host.clone()
        } else {
            String::new()
        };
        Ok(ConnectionProfile {
            profile_id: Uuid::new_v4(),
            revision: 1,
            name: self.alias,
            params: ConnectionParams {
                driver_id: DriverId::new(self.driver_id)?,
                host: self.host,
                port: self.port,
                database: self.database,
                username: self.username,
                credential_ref: self.credential_ref,
                tls: self.tls,
                tls_server_name: self.tls_server_name,
                properties: self.properties,
            },
            file_path,
            last_used: None,
            display_name: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::AppPaths;

    const WINDOWS_FIXTURE: &[u8] =
        include_bytes!("../../../../../tests/fixtures/windows-connections-v1.json");
    const REFERENCE: &str = "sqarvo:conn/58acd9da-8a78-4d19-8d5e-2a33dd0c7c06";

    fn isolated_store(root: &Path) -> ProfileStore {
        let paths = AppPaths::from_roots(root.join("cfg"), root.join("data"), root.join("run"));
        ProfileStore::new(&paths)
    }

    fn write_json(path: &Path, value: &Value) -> Vec<u8> {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = serde_json::to_vec(value).unwrap();
        fs::write(path, &bytes).unwrap();
        bytes
    }

    #[test]
    fn three_legacy_formats_merge_and_v1_authorization_is_discarded() {
        let root = tempfile::tempdir().unwrap();
        let store = isolated_store(root.path());
        let gtk = store.config_dir.join("Sqarvo/connections.json");
        let windows = root.path().join("local-app-data/Sqarvo/connections.json");
        let mcp = store.config_dir.join(MCP_FILE_NAME);
        let gtk_bytes = write_json(
            &gtk,
            &json!({
                "version": 1,
                "connections": [{
                    "name": "sample.sqlite", "alias": "账本 #1", "driver": "sqlite",
                    "host": "", "port": 0, "database": "", "username": "",
                    "file_path": "sample.sqlite", "read_only": false,
                    "tls_mode": "disabled", "credential_ref": ""
                }]
            }),
        );
        fs::create_dir_all(windows.parent().unwrap()).unwrap();
        fs::write(&windows, WINDOWS_FIXTURE).unwrap();
        let mcp_bytes = write_json(
            &mcp,
            &json!({
                "connections": [
                    {
                        "alias": "demo", "driver_id": "mariadb", "host": "db.example.test",
                        "port": 3306, "database": "inventory", "username": "demo",
                        "credential_ref": REFERENCE, "tls": "verify_full",
                        "tls_server_name": "db.example.test", "enabled": true
                    },
                    {
                        "alias": "disabled-v1", "driver_id": "sqlite", "host": "mcp.sqlite",
                        "enabled": false
                    }
                ],
                "enabled_tools": ["list_connections"],
                "query_limits": {"max_rows": 22}
            }),
        );
        let sources = || {
            [
                (&gtk, SourceKind::Gtk),
                (&windows, SourceKind::Windows),
                (&mcp, SourceKind::Mcp),
            ]
            .into_iter()
            .map(|(path, kind)| Source {
                path: path.clone(),
                kind,
            })
            .collect()
        };

        run_sources(&store, sources()).unwrap();
        let document: Value =
            serde_json::from_slice(&fs::read(store.config_dir.join(PROFILE_FILE_NAME)).unwrap())
                .unwrap();
        let rows = document["profiles"].as_array().unwrap();
        assert_eq!(rows.len(), 5);
        let ids = rows
            .iter()
            .map(|row| {
                let id = Uuid::parse_str(row["profile_id"].as_str().unwrap()).unwrap();
                assert_eq!(id.get_version_num(), 4);
                assert_eq!(row["revision"], json!(1));
                id
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), 5);
        assert_eq!(
            rows.iter()
                .filter(|row| row["credential_ref"] == REFERENCE)
                .count(),
            2
        );
        assert_eq!(rows[0]["name"], "账本 #1");
        assert_eq!(rows[0]["display_name"], "账本 #1");
        assert_eq!(rows[2]["properties"]["read_only"], "true");
        assert_eq!(rows[3]["tls_server_name"], "db.example.test");
        assert_eq!(rows[4]["name"], "disabled-v1");
        assert_eq!(document["migration_state"].as_object().unwrap().len(), 3);
        assert_eq!(
            document["mcp"],
            json!({"enabled": false, "read_only": true})
        );
        assert!(!gtk.exists());
        assert!(!windows.exists());
        assert_eq!(fs::read(backup_path(&gtk, ".bak")).unwrap(), gtk_bytes);
        assert_eq!(
            fs::read(backup_path(&windows, ".bak")).unwrap(),
            WINDOWS_FIXTURE
        );
        assert_eq!(fs::read(backup_path(&mcp, ".bak")).unwrap(), mcp_bytes);
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&mcp).unwrap()).unwrap(),
            json!({"enabled_tools": ["list_connections"], "query_limits": {"max_rows": 22}})
        );

        run_sources(&store, sources()).unwrap();
        let after: Value =
            serde_json::from_slice(&fs::read(store.config_dir.join(PROFILE_FILE_NAME)).unwrap())
                .unwrap();
        assert_eq!(after, document);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn gtk_migration_preserves_distinct_sqlite_paths_with_the_same_name() {
        let root = tempfile::tempdir().unwrap();
        let store = isolated_store(root.path());
        let gtk = store.config_dir.join("Sqarvo/connections.json");
        let files = [
            root.path().join("Sample.sqlite"),
            root.path().join("sample.sqlite"),
        ];
        fs::write(&files[0], b"first fixture").unwrap();
        fs::write(&files[1], b"second fixture").unwrap();
        let mut entries = files
            .map(|file_path| {
                json!({
                    "name": "same name", "driver": "sqlite", "host": "", "port": 0,
                    "database": "", "username": "", "file_path": file_path, "read_only": true,
                    "tls_mode": "disabled", "credential_ref": ""
                })
            })
            .to_vec();
        let mut editable = entries[0].clone();
        editable["read_only"] = json!(false);
        entries.push(editable);
        entries.push(entries[0].clone());
        let original = write_json(&gtk, &json!({"version": 1, "connections": entries}));

        store.run_once_migration().unwrap();
        let document = store.snapshot().unwrap();
        assert_eq!(document.profiles.len(), 3);
        for (profile, entry) in document.profiles.iter().zip(&entries[1..]) {
            assert_eq!(profile.name, "same name");
            assert_eq!(entry["file_path"], profile.file_path);
            assert_eq!(profile.params.host, profile.file_path);
            assert_eq!(
                profile.params.properties["read_only"],
                entry["read_only"].to_string()
            );
        }
        assert_eq!(fs::read(backup_path(&gtk, ".bak")).unwrap(), original);
        store.run_once_migration().unwrap();
        assert_eq!(store.snapshot().unwrap(), document);
    }

    #[test]
    fn retry_finishes_backup_without_reimport_and_retains_old_backups() {
        let root = tempfile::tempdir().unwrap();
        let store = isolated_store(root.path());
        let mcp = store.config_dir.join(MCP_FILE_NAME);
        let legacy = |alias| {
            json!({"connections": [{
                "alias": alias, "driver_id": "sqlite", "host": "data.sqlite",
                "enabled": true
            }]})
        };
        let initial = write_json(&mcp, &legacy("first"));
        let backup = backup_path(&mcp, ".bak");
        fs::create_dir(&backup).unwrap();
        assert!(run_once(&store).is_err());
        let path = store.config_dir.join(PROFILE_FILE_NAME);
        let after_commit = fs::read(&path).unwrap();
        assert_eq!(fs::read(&mcp).unwrap(), initial);
        fs::remove_dir(&backup).unwrap();
        run_once(&store).unwrap();
        assert_eq!(fs::read(&path).unwrap(), after_commit);
        assert_eq!(fs::read(&backup).unwrap(), initial);

        let next = write_json(&mcp, &legacy("second"));
        run_once(&store).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), initial);
        let next_backup = backup_path(&mcp, &format!(".bak-{}", fingerprint(&next)));
        assert_eq!(fs::read(next_backup).unwrap(), next);
        let after: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(after["profiles"].as_array().unwrap().len(), 2);
        assert_eq!(after["revision"], json!(2));
    }

    #[test]
    fn explicit_mcp_source_rejects_schema_errors_without_touching_files() {
        let root = tempfile::tempdir().unwrap();
        let store = isolated_store(root.path());
        let mcp = root.path().join("custom.json");
        let bytes = write_json(
            &mcp,
            &json!({"connections": [{
                "alias": "demo", "driver_id": "sqlite", "host": "data.sqlite",
                "enabled": true, "password": "synthetic-marker"
            }]}),
        );
        let error = run_once_with_mcp(&store, &mcp).unwrap_err();
        assert_eq!(error.code(), ErrorCode::PROFILE_INVALID);
        assert!(!error.to_string().contains("synthetic-marker"));
        assert_eq!(fs::read(&mcp).unwrap(), bytes);
        assert!(!backup_path(&mcp, ".bak").exists());
        assert!(!store.config_dir.join(PROFILE_FILE_NAME).exists());
    }

    #[test]
    fn explicit_mcp_source_cannot_replace_the_shared_document() {
        let root = tempfile::tempdir().unwrap();
        let store = isolated_store(root.path());
        let mcp = store.config_dir.join(MCP_FILE_NAME);
        write_json(&mcp, &json!({"connections": []}));
        run_once_with_mcp(&store, &mcp).unwrap();
        let path = store.config_dir.join(PROFILE_FILE_NAME);
        let before = fs::read(&path).unwrap();
        run_once_with_mcp(&store, &path).unwrap();
        assert_eq!(fs::read(path).unwrap(), before);
    }
}
