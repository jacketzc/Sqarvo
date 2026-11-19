//! gRPC 连接档案仓库与 GTK 展示模型。

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::backend::client::BackendClient;
use crate::backend::connection::{ConnectionDraft, ConnectionFailure, TlsMode};
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{
    ConnectionParams, ConnectionProfile, DeleteProfileRequest, SaveProfileRequest,
    TlsMode as ProtoTlsMode,
};

const PROFILE_TIMEOUT: Duration = Duration::from_secs(5);

/// 文件型驱动 ID。
pub const SQLITE_DRIVER_ID: &str = "sqlite";

/// 已保存的连接在 GTK 中的表示。凭据只有系统凭据库引用。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SavedConnection {
    /// 共享档案的稳定标识。
    pub profile_id: String,
    /// 保存、删除使用的档案修订。
    pub revision: u64,
    /// 共享档案名称。
    pub name: String,
    /// 可选 GUI 别名。
    pub alias: String,
    /// 驱动 ID。
    pub driver: String,
    /// 主机或本地 socket。
    pub host: String,
    /// 端口。
    pub port: u16,
    /// 初始数据库。
    pub database: String,
    /// 数据库用户名。
    pub username: String,
    /// 文件型驱动的路径。
    pub file_path: String,
    /// 文件型连接的只读模式。
    pub read_only: bool,
    /// 保存的 TLS 模式。
    pub tls_mode: String,
    /// 系统凭据库引用。
    pub credential_ref: String,
    /// TLS 校验主机名。
    pub tls_server_name: String,
    /// 驱动附加参数。
    pub properties: HashMap<String, String>,
    /// 服务端提供的上次使用时间。
    pub last_used: Option<String>,
}

impl SavedConnection {
    /// 转换已验证的 gRPC 响应。任何异常响应走档案错误态。
    fn from_profile(profile: ConnectionProfile) -> Result<Self, ConnectionFailure> {
        let params = profile.params.ok_or_else(invalid_response)?;
        let port = u16::try_from(params.port).map_err(|_| invalid_response())?;
        let tls_mode =
            match ProtoTlsMode::try_from(params.tls_mode).map_err(|_| invalid_response())? {
                ProtoTlsMode::Unspecified => "unspecified",
                ProtoTlsMode::Disabled => "disabled",
                ProtoTlsMode::Preferred => "preferred",
                ProtoTlsMode::Required => "required",
                ProtoTlsMode::VerifyCa => "verify-ca",
                ProtoTlsMode::VerifyFull => "verify-full",
            };
        if profile.profile_id.is_empty() || profile.revision == 0 {
            return Err(invalid_response());
        }
        let file_path = if profile.file_path.is_empty() && params.driver_id == SQLITE_DRIVER_ID {
            params.host.clone()
        } else {
            profile.file_path
        };
        let read_only = params.properties.get("read_only").is_some_and(|value| {
            matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "1")
        });
        Ok(Self {
            profile_id: profile.profile_id,
            revision: profile.revision,
            name: profile.name,
            alias: profile.display_name.unwrap_or_default(),
            driver: params.driver_id,
            host: params.host,
            port,
            database: params.database,
            username: params.username,
            file_path,
            read_only,
            tls_mode: tls_mode.to_owned(),
            credential_ref: params.credential_ref,
            tls_server_name: params.tls_server_name,
            properties: params.properties,
            last_used: profile.last_used,
        })
    }

    /// 共享档案的标识用于对象树，编辑名称或目标不会改变它。
    pub fn key(&self) -> String {
        self.profile_id.clone()
    }

    /// 连接参数直接来自档案。保留表单中没有编辑入口的属性。
    pub fn params(&self) -> ConnectionParams {
        let tls_mode = if self.tls_mode == "unspecified" {
            ProtoTlsMode::Unspecified
        } else {
            TlsMode::from_config_value(&self.tls_mode)
                .unwrap_or_default()
                .to_proto()
        };
        let mut properties = self.properties.clone();
        if self.driver == SQLITE_DRIVER_ID {
            properties
                .entry("read_only".to_owned())
                .or_insert_with(|| self.read_only.to_string());
        }
        let host = if self.driver == SQLITE_DRIVER_ID && self.host.is_empty() {
            self.file_path.clone()
        } else {
            self.host.clone()
        };
        ConnectionParams {
            driver_id: self.driver.clone(),
            host,
            port: self.port.into(),
            database: self.database.clone(),
            username: self.username.clone(),
            credential_ref: self.credential_ref.clone(),
            tls_mode: tls_mode as i32,
            tls_server_name: self.tls_server_name.clone(),
            properties,
        }
    }

    /// 文件型驱动的库路径。
    pub fn file_target(&self) -> Option<&str> {
        (self.driver == SQLITE_DRIVER_ID && !self.file_path.is_empty())
            .then_some(self.file_path.as_str())
    }

    /// 对象树名称。
    pub fn display_label(&self) -> String {
        let alias = self.alias.trim();
        if alias.is_empty() {
            self.target_label()
        } else {
            alias.to_owned()
        }
    }

    /// 连接目标，用于同名连接的 tooltip 和确认对话框。
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

fn invalid_response() -> ConnectionFailure {
    ConnectionFailure::Other {
        code: "profile.invalid".to_owned(),
        message: crate::l10n::t("dialogs.profile_invalid_response").to_owned(),
    }
}

/// 从后端加载当前共享档案。
pub async fn list_profiles(
    client: &BackendClient,
) -> Result<Vec<SavedConnection>, ConnectionFailure> {
    let mut service = client
        .profile_service()
        .map_err(|error| ConnectionFailure::transport(&error))?;
    let response = tokio::time::timeout(PROFILE_TIMEOUT, service.list_profiles(Empty {}))
        .await
        .map_err(|_| ConnectionFailure::timeout("ListProfiles", PROFILE_TIMEOUT))?
        .map_err(|status| ConnectionFailure::from_status(&status))?;
    response
        .into_inner()
        .profiles
        .into_iter()
        .map(SavedConnection::from_profile)
        .collect()
}

/// 保存新档案或使用编辑时读到的 revision 更新已有档案。
pub async fn save_profile(
    client: &BackendClient,
    draft: &ConnectionDraft,
    reference: &str,
) -> Result<SavedConnection, ConnectionFailure> {
    let mut service = client
        .profile_service()
        .map_err(|error| ConnectionFailure::transport(&error))?;
    let response = tokio::time::timeout(
        PROFILE_TIMEOUT,
        service.save_profile(save_request(draft, reference)),
    )
    .await
    .map_err(|_| ConnectionFailure::timeout("SaveProfile", PROFILE_TIMEOUT))?
    .map_err(|status| ConnectionFailure::from_status(&status))?;
    SavedConnection::from_profile(response.into_inner())
}

fn save_request(draft: &ConnectionDraft, reference: &str) -> SaveProfileRequest {
    let previous = draft.existing_profile.as_ref();
    let expected_revision = previous.map(|saved| saved.revision);
    let alias = draft.alias.trim();
    let profile = ConnectionProfile {
        profile_id: previous.map_or_else(String::new, |saved| saved.profile_id.clone()),
        revision: expected_revision.unwrap_or_default(),
        name: if alias.is_empty() {
            draft.name.clone()
        } else {
            alias.to_owned()
        },
        params: Some(draft.params(reference)),
        file_path: draft.file_path.clone(),
        display_name: (!alias.is_empty()).then(|| alias.to_owned()),
        last_used: previous.and_then(|saved| saved.last_used.clone()),
    };
    SaveProfileRequest {
        profile: Some(profile),
        expected_revision,
    }
}

/// 删除以用户确认时的档案 revision 为条件。
pub async fn delete_profile(
    client: &BackendClient,
    profile_id: String,
    expected_revision: u64,
) -> Result<(), ConnectionFailure> {
    let mut service = client
        .profile_service()
        .map_err(|error| ConnectionFailure::transport(&error))?;
    tokio::time::timeout(
        PROFILE_TIMEOUT,
        service.delete_profile(DeleteProfileRequest {
            profile_id,
            expected_revision,
        }),
    )
    .await
    .map_err(|_| ConnectionFailure::timeout("DeleteProfile", PROFILE_TIMEOUT))?
    .map_err(|status| ConnectionFailure::from_status(&status))?;
    Ok(())
}

/// 窗口偏好目录。连接档案仅通过上面的 gRPC API 访问。
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or(ConfigError)?;
    Ok(base.join("sqarvo").join("Sqarvo"))
}

/// 无法定位窗口偏好目录。
#[derive(Debug)]
pub struct ConfigError;

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(crate::l10n::t("dialogs.cfg_dir_unknown"))
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_keeps_the_read_revision_and_unexposed_driver_options() {
        let saved = SavedConnection::from_profile(ConnectionProfile {
            profile_id: "991cd063-94a2-47e0-8cd2-043b0a53706c".to_owned(),
            revision: 5,
            name: "inventory".to_owned(),
            params: Some(ConnectionParams {
                driver_id: "mariadb".to_owned(),
                host: "local.example.test".to_owned(),
                port: 3306,
                tls_mode: ProtoTlsMode::VerifyFull as i32,
                tls_server_name: "db.example.test".to_owned(),
                properties: HashMap::from([("ssl_ca".to_owned(), "ca.pem".to_owned())]),
                ..ConnectionParams::default()
            }),
            last_used: Some("2026-11-18T00:00:00Z".to_owned()),
            ..ConnectionProfile::default()
        })
        .unwrap();
        assert_eq!(saved.params().tls_server_name, "db.example.test");
        let profile_id = saved.profile_id.clone();
        let draft = ConnectionDraft {
            existing_profile: Some(saved),
            name: "inventory".to_owned(),
            alias: "New name".to_owned(),
            driver_id: "mariadb".to_owned(),
            host: "new.example.test".to_owned(),
            port: 3306,
            database: "demo".to_owned(),
            username: "demo".to_owned(),
            tls_mode: TlsMode::VerifyFull,
            file_path: String::new(),
            read_only: false,
            credential_ref: None,
            password: String::new(),
        };
        let request = save_request(&draft, "sqarvo:conn/991cd063-94a2-47e0-8cd2-043b0a53706c");
        assert_eq!(request.expected_revision, Some(5));
        let profile = request.profile.unwrap();
        assert_eq!(profile.profile_id, profile_id);
        assert_eq!(profile.name, "New name");
        assert_eq!(profile.last_used.as_deref(), Some("2026-11-18T00:00:00Z"));
        let params = profile.params.unwrap();
        assert_eq!(params.host, "new.example.test");
        assert_eq!(params.tls_server_name, "db.example.test");
        assert_eq!(params.properties["ssl_ca"], "ca.pem");
        assert_eq!(params.tls_mode, ProtoTlsMode::VerifyFull as i32);
    }

    #[test]
    fn sqlite_preserves_the_driver_path_and_read_only_setting() {
        let connection = SavedConnection::from_profile(ConnectionProfile {
            profile_id: "d7e96a1b-7eb0-4944-9791-57b730624920".to_owned(),
            revision: 1,
            name: "inventory".to_owned(),
            params: Some(ConnectionParams {
                driver_id: "sqlite".to_owned(),
                host: "/tmp/inventory.sqlite".to_owned(),
                properties: HashMap::from([("read_only".to_owned(), "1".to_owned())]),
                ..ConnectionParams::default()
            }),
            ..ConnectionProfile::default()
        })
        .unwrap();
        assert!(connection.read_only);
        assert_eq!(connection.file_target(), Some("/tmp/inventory.sqlite"));
        assert_eq!(connection.params().properties["read_only"], "1");
        assert_eq!(
            connection.params().tls_mode,
            ProtoTlsMode::Unspecified as i32
        );
    }
}
