//! 端点描述：定位、读取与校验后端写出的 `backend-endpoint.json`。
//!
//! 文件由后端 `LocalStore` 写入（权限 0600，原子 rename），字段定义见
//! `backend/crates/local-store/src/endpoint.rs`。前端必须用**相同**的
//! qualifier / organization / application 推导运行时目录，否则会读到不同的
//! 位置；测试与自定义部署可以用 [`ENDPOINT_FILE_ENV`] 覆盖文件路径。
//!
//! 安全约束：`token` 是本地 IPC 的访问凭据。本模块的 `Debug` 实现与所有错误
//! 文案都只暴露 token 长度，绝不输出原文。

use std::fmt;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::Deserialize;

use crate::backend::error::BackendError;

/// 客户端支持的 gRPC 契约主版本。
///
/// 必须与后端 `backend/crates/rpc-server/src/services/system.rs` 的
/// `PROTOCOL_VERSION` 主版本一致；主版本不同意味着消息布局可能不兼容。
pub const CLIENT_PROTOCOL_VERSION: &str = "v1";

/// 覆盖端点描述文件路径的环境变量（测试、打包或自定义运行时目录）。
pub const ENDPOINT_FILE_ENV: &str = "SQARVO_ENDPOINT_FILE";

/// 端点描述文件名（位于运行时目录下）。
pub(crate) const ENDPOINT_FILE_NAME: &str = "backend-endpoint.json";

/// Linux 上运行时目录为 `$XDG_RUNTIME_DIR/<项目目录>`，项目目录名取自
/// `directories::ProjectDirs`（实测为 `sqarvo`，与后端一致）。
const RUNTIME_PROJECT_DIR: &str = "sqarvo";

/// token 的期望长度：32 字节随机数的十六进制表示。
const TOKEN_HEX_LEN: usize = 64;

/// 传输方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Unix 域套接字。
    Unix,
    /// 回环 TCP（后端在非 Unix 平台的回退方案，或开发调试）。
    Tcp,
}

impl TransportKind {
    /// 展示给用户的传输描述。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unix => "Unix 域套接字",
            Self::Tcp => "回环 TCP",
        }
    }
}

/// 端点描述文件内容（只保留前端实际使用的字段）。
#[derive(Clone, Deserialize)]
pub struct EndpointInfo {
    /// 传输方式。
    pub transport: TransportKind,
    /// Unix socket 路径。
    #[serde(default)]
    pub socket_path: Option<String>,
    /// TCP 主机。
    #[serde(default)]
    pub host: Option<String>,
    /// TCP 端口。
    #[serde(default)]
    pub port: Option<u16>,
    /// 会话 token（敏感，禁止写入日志与界面）。
    pub token: String,
    /// 后端进程 ID。
    pub pid: u32,
    /// 后端声明的协议版本，例如 `v1`。
    pub protocol_version: String,
    /// 后端声明的服务版本，例如 `0.1.0`。
    pub service_version: String,
}

impl EndpointInfo {
    /// token 长度，用于在不泄露原文的前提下做诊断。
    #[must_use]
    pub fn token_len(&self) -> usize {
        self.token.len()
    }

    /// 用于日志的安全摘要：只包含长度与传输方式。
    #[must_use]
    pub fn redacted_summary(&self) -> String {
        format!(
            "transport={:?} pid={} protocol={} service={} token=<redacted>({} 字符)",
            self.transport,
            self.pid,
            self.protocol_version,
            self.service_version,
            self.token_len(),
        )
    }

    /// 传输方式的具体描述（不含 token）。
    #[must_use]
    pub fn transport_label(&self) -> String {
        match self.transport {
            TransportKind::Unix => format!(
                "{}（{}）",
                self.transport.label(),
                self.socket_path.as_deref().unwrap_or("<缺失>")
            ),
            TransportKind::Tcp => format!(
                "{}（{}:{}）",
                self.transport.label(),
                self.host.as_deref().unwrap_or("<缺失>"),
                self.port
                    .map_or_else(|| "<缺失>".to_owned(), |port| port.to_string())
            ),
        }
    }

    /// 校验协议主版本是否兼容。
    pub fn ensure_compatible(&self) -> Result<(), BackendError> {
        ensure_protocol_compatible(&self.protocol_version)
    }

    /// 校验与传输相关的必填字段。
    fn validate(&self) -> Result<(), BackendError> {
        if self.token.len() != TOKEN_HEX_LEN || !self.token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(BackendError::EndpointInvalid {
                path: PathBuf::new(),
                detail: format!(
                    "token 格式非法：期望 {TOKEN_HEX_LEN} 位十六进制，实际 {} 个字符",
                    self.token.len()
                ),
            });
        }
        match self.transport {
            TransportKind::Unix => {
                if self.socket_path.as_deref().is_none_or(str::is_empty) {
                    return Err(BackendError::EndpointInvalid {
                        path: PathBuf::new(),
                        detail: "transport=unix 但缺少 socket_path".to_owned(),
                    });
                }
            }
            TransportKind::Tcp => {
                if self.host.as_deref().is_none_or(str::is_empty) || self.port.is_none() {
                    return Err(BackendError::EndpointInvalid {
                        path: PathBuf::new(),
                        detail: "transport=tcp 但缺少 host 或 port".to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

impl fmt::Debug for EndpointInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 手工实现，避免 derive 把 token 原文写进日志或 panic 信息。
        formatter
            .debug_struct("EndpointInfo")
            .field("transport", &self.transport)
            .field("socket_path", &self.socket_path)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .field("token_len", &self.token.len())
            .field("pid", &self.pid)
            .field("protocol_version", &self.protocol_version)
            .field("service_version", &self.service_version)
            .finish()
    }
}

/// 解析端点描述文件路径。
///
/// 优先使用 [`ENDPOINT_FILE_ENV`]，否则按与后端相同的 `directories`
/// qualifier/org/app 推导运行时目录。
pub fn endpoint_path() -> Result<PathBuf, BackendError> {
    if let Some(raw) = std::env::var_os(ENDPOINT_FILE_ENV)
        && !raw.is_empty()
    {
        return Ok(PathBuf::from(raw));
    }
    let dirs =
        ProjectDirs::from("io", "sqarvo", "Sqarvo").ok_or_else(|| BackendError::Runtime {
            detail: "无法确定用户目录（缺少 HOME 等环境变量）".to_owned(),
        })?;
    let runtime = dirs
        .runtime_dir()
        .map_or_else(|| dirs.data_dir().join("run"), Path::to_path_buf);
    Ok(runtime.join(ENDPOINT_FILE_NAME))
}

/// 计算「若要让后端把端点写到 `endpoint_path`，子进程应有的 XDG_RUNTIME_DIR」。
///
/// 后端用 `directories::ProjectDirs` 推导运行时目录（Linux 为
/// `$XDG_RUNTIME_DIR/<项目目录>`），自身不支持覆盖端点路径。因此当
/// [`ENDPOINT_FILE_ENV`] 把端点指到非默认位置时，前端启动子进程需要同步调整
/// `XDG_RUNTIME_DIR`，否则后端会写到默认目录、前端永远等不到端点文件。
///
/// 返回 `None` 表示路径不符合 `<runtime_root>/sqarvo/backend-endpoint.json` 布局。
#[must_use]
pub fn child_runtime_root(endpoint_path: &Path) -> Option<PathBuf> {
    if endpoint_path.file_name()? != ENDPOINT_FILE_NAME {
        return None;
    }
    let project_dir = endpoint_path.parent()?;
    if project_dir.file_name()? != RUNTIME_PROJECT_DIR {
        return None;
    }
    let root = project_dir.parent()?;
    if root.as_os_str().is_empty() {
        return None;
    }
    Some(root.to_path_buf())
}

/// 读取端点描述文件。
///
/// - 文件不存在返回 `Ok(None)`（由调用方决定是否启动后端）；
/// - 文件存在但不可读、JSON 非法或字段非法返回 [`BackendError::EndpointInvalid`]。
pub fn read(path: &Path) -> Result<Option<EndpointInfo>, BackendError> {
    let payload = match std::fs::read(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(BackendError::EndpointInvalid {
                path: path.to_path_buf(),
                detail: format!("无法读取：{error}"),
            });
        }
    };
    let info: EndpointInfo =
        serde_json::from_slice(&payload).map_err(|error| BackendError::EndpointInvalid {
            path: path.to_path_buf(),
            detail: format!("JSON 解析失败：{error}"),
        })?;
    info.validate().map_err(|error| match error {
        // validate 不知道路径，这里补上，方便用户定位文件。
        BackendError::EndpointInvalid { detail, .. } => BackendError::EndpointInvalid {
            path: path.to_path_buf(),
            detail,
        },
        other => other,
    })?;
    Ok(Some(info))
}

/// 提取协议主版本号：`v1` → `1`，`v2.3` → `2`。
///
/// 返回 `None` 表示字符串不符合 `v<major>[.<minor>]` 形式。
#[must_use]
pub fn protocol_major(version: &str) -> Option<u32> {
    let rest = version.trim().strip_prefix('v')?;
    let major = rest.split('.').next()?;
    if major.is_empty() || !major.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    major.parse().ok()
}

/// 校验后端声明的协议版本与客户端主版本是否兼容。
///
/// 只比较主版本：同一主版本内的字段新增必须保持向后兼容。
pub fn ensure_protocol_compatible(backend_version: &str) -> Result<(), BackendError> {
    let backend_major =
        protocol_major(backend_version).ok_or_else(|| BackendError::ProtocolVersionUnparsable {
            raw: backend_version.to_owned(),
        })?;
    let client_major = protocol_major(CLIENT_PROTOCOL_VERSION).expect("客户端协议常量必须合法");
    if backend_major != client_major {
        return Err(BackendError::ProtocolIncompatible {
            client: CLIENT_PROTOCOL_VERSION.to_owned(),
            backend: backend_version.to_owned(),
        });
    }
    Ok(())
}

/// 判断 PID 是否仍存活。
///
/// Linux 上检查 `/proc/<pid>`；其他平台无法可靠判断（权限限制），
/// 保守返回 `true`，由随后的连接失败暴露问题。
#[must_use]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json(token: &str) -> String {
        format!(
            r#"{{
                "transport": "unix",
                "socket_path": "/run/user/1000/sqarvo/Sqarvo/backend.sock",
                "token": "{token}",
                "pid": 4242,
                "protocol_version": "v1",
                "service_version": "0.1.0",
                "created_at": "2026-11-04T09:00:00Z"
            }}"#
        )
    }

    #[test]
    fn reads_valid_endpoint_and_redacts_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backend-endpoint.json");
        let token = "ab".repeat(32);
        std::fs::write(&path, sample_json(&token)).unwrap();

        let info = read(&path).unwrap().expect("端点文件应存在");
        assert_eq!(info.transport, TransportKind::Unix);
        assert_eq!(info.pid, 4242);
        assert_eq!(info.token_len(), 64);
        assert!(info.ensure_compatible().is_ok());

        assert!(!format!("{info:?}").contains(&token));
        assert!(!info.redacted_summary().contains(&token));
        assert!(info.redacted_summary().contains("<redacted>"));
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("absent.json")).unwrap().is_none());
    }

    #[test]
    fn rejects_malformed_token_without_echoing_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endpoint.json");
        std::fs::write(&path, sample_json("not-a-token")).unwrap();
        let error = read(&path).unwrap_err().to_string();
        assert!(error.contains("token 格式非法"));
        assert!(!error.contains("not-a-token"));
    }

    #[test]
    fn protocol_major_parsing() {
        assert_eq!(protocol_major("v1"), Some(1));
        assert_eq!(protocol_major("v2.7"), Some(2));
        assert_eq!(protocol_major("1"), None);
        assert_eq!(protocol_major("v"), None);
        assert_eq!(protocol_major("vX"), None);
    }

    #[test]
    fn incompatible_protocol_is_rejected_with_upgrade_hint() {
        let error = ensure_protocol_compatible("v2").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("协议版本不兼容"));
        assert!(message.contains("v2"));
        assert!(message.contains("v1"));
        assert!(message.contains("升级"));
        assert!(ensure_protocol_compatible("v1.9").is_ok());
        assert!(matches!(
            ensure_protocol_compatible("garbage"),
            Err(BackendError::ProtocolVersionUnparsable { .. })
        ));
    }

    #[test]
    fn child_runtime_root_follows_standard_layout() {
        assert_eq!(
            child_runtime_root(Path::new("/tmp/run-root/sqarvo/backend-endpoint.json")),
            Some(PathBuf::from("/tmp/run-root"))
        );
        assert_eq!(
            child_runtime_root(Path::new("/tmp/other/backend-endpoint.json")),
            None
        );
        assert_eq!(
            child_runtime_root(Path::new("/tmp/not-the-name.json")),
            None
        );
    }

    #[test]
    fn endpoint_path_honours_env_override() {
        // 该用例会修改进程环境变量，因此与其它读环境的测试串行执行。
        let key = ENDPOINT_FILE_ENV;
        let previous = std::env::var_os(key);
        // SAFETY: 测试内单线程设置环境变量，且仅影响本进程。
        unsafe { std::env::set_var(key, "/tmp/sqarvo-endpoint-override.json") };
        assert_eq!(
            endpoint_path().unwrap(),
            PathBuf::from("/tmp/sqarvo-endpoint-override.json")
        );
        match previous {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}
