//! 后端 IPC 端点描述。
//!
//! 前端启动后端后，从端点描述文件读取连接方式与 token。该文件必须
//! 只对当前用户可读：
//!
//! - Unix：父目录 0700，文件 0600，原子替换写入；
//! - Windows：依赖用户配置目录 ACL，显式 DACL 收紧尚未实现（见文档）。

use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sqarvo_common::{Error, ErrorCode, Result};
use time::OffsetDateTime;

use crate::paths::restrict_to_owner;

/// 传输方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Unix 域套接字（Linux / macOS）。
    Unix,
    /// 回环 TCP（Windows，或开发调试）。
    Tcp,
}

/// 端点描述文件内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointInfo {
    /// 传输方式。
    pub transport: TransportKind,
    /// Unix socket 路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_path: Option<String>,
    /// TCP 主机，只允许回环地址。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// TCP 端口。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// 会话 token（敏感）。
    pub token: String,
    /// 后端进程 ID，用于识别并清理崩溃残留。
    pub pid: u32,
    /// 协议版本。
    pub protocol_version: String,
    /// 后端版本。
    pub service_version: String,
    /// 写入时间（RFC 3339）。
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// 写入端点描述文件。
///
/// 先写入同目录临时文件并 `sync_all`，再原子 rename，避免前端读到半截 JSON。
/// Unix 下文件以 0600 创建。
pub fn write_endpoint_file(path: &Path, info: &EndpointInfo) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::invalid_argument(ErrorCode::CONFIG_INVALID, "端点文件缺少父目录"))?;
    fs::create_dir_all(parent).map_err(Error::from)?;
    restrict_to_owner(parent)?;

    let payload = serde_json::to_vec_pretty(info)
        .map_err(|error| Error::internal("序列化端点描述失败").with_source(error))?;

    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(Error::from)?;
        file.write_all(&payload).map_err(Error::from)?;
        file.sync_all().map_err(Error::from)?;
    }
    fs::rename(&temporary, path).map_err(Error::from)?;
    restrict_file_to_owner(path)?;
    Ok(())
}

/// 读取端点描述文件。
pub fn read_endpoint_file(path: &Path) -> Result<EndpointInfo> {
    let payload = fs::read(path).map_err(Error::from)?;
    serde_json::from_slice(&payload).map_err(|error| {
        Error::new(
            ErrorCode::CONFIG_INVALID,
            sqarvo_common::ErrorCategory::InvalidArgument,
            "端点描述文件格式非法",
        )
        .with_source(error)
    })
}

fn restrict_file_to_owner(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(Error::from)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(path: &Path) -> EndpointInfo {
        EndpointInfo {
            transport: TransportKind::Unix,
            socket_path: Some(path.display().to_string()),
            host: None,
            port: None,
            token: "a".repeat(64),
            pid: std::process::id(),
            protocol_version: "v1".to_owned(),
            service_version: "0.1.0".to_owned(),
            created_at: OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn endpoint_roundtrip_and_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("run/backend-endpoint.json");
        let info = sample(Path::new("/tmp/sqarvo.sock"));
        write_endpoint_file(&path, &info).unwrap();

        let decoded = read_endpoint_file(&path).unwrap();
        assert_eq!(decoded.transport, TransportKind::Unix);
        assert_eq!(decoded.socket_path.as_deref(), Some("/tmp/sqarvo.sock"));
        assert_eq!(decoded.token, info.token);
        assert_eq!(decoded.created_at, info.created_at);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn tcp_transcript_omits_socket_path() {
        let info = EndpointInfo {
            transport: TransportKind::Tcp,
            socket_path: None,
            host: Some("127.0.0.1".to_owned()),
            port: Some(49152),
            ..sample(Path::new("/tmp/x.sock"))
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("socket_path"));
        assert!(json.contains("\"transport\":\"tcp\""));
    }

    #[test]
    fn corrupt_file_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("endpoint.json");
        fs::write(&path, b"{ not json").unwrap();
        let error = read_endpoint_file(&path).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    }
}
