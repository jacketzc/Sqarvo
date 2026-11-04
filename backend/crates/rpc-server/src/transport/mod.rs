//! 本地传输层：Unix 域套接字（Linux/macOS）与回环 TCP（Windows / 调试）。
//!
//! 设计约束（来自架构决策）：
//!
//! - 默认只监听本机：Unix socket 位于 0700 目录且权限 0600；
//!   TCP 只允许回环地址，绝不监听 `0.0.0.0`；
//! - 传输实现隐藏在 [`LocalListener`] 之后，业务层不感知 socket/TCP 差异；
//! - 每个请求都经过 [`crate::auth::TokenInterceptor`] 校验 token。
//!
//! Windows 上的 Named Pipe + ACL 方案尚未实现；当前回退为回环 TCP + 随机 token，
//! 端点文件权限收紧到当前用户仍依赖`%APPDATA%` 的 ACL 继承（见 `docs/`）。

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::stream::{self, BoxStream, StreamExt};
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use sqarvo_local_store::{AuthToken, TransportKind, restrict_to_owner};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tonic::transport::Server;
use tonic::transport::server::Connected;

use crate::auth::TokenInterceptor;
use crate::proto::services::v1::{
    connection_service_server::ConnectionServiceServer,
    credential_service_server::CredentialServiceServer,
    metadata_service_server::MetadataServiceServer, query_service_server::QueryServiceServer,
    system_service_server::SystemServiceServer,
    transaction_service_server::TransactionServiceServer,
};
use crate::services::Services;

/// 连接来源信息，可通过 request extensions 获取。
#[derive(Debug, Clone)]
pub struct LocalConnectInfo {
    /// 传输方式。
    pub transport: TransportKind,
    /// TCP 对端地址；Unix socket 为 `None`。
    pub peer: Option<String>,
}

/// 已接受的连接。屏蔽 Unix / TCP 差异，统一实现 Tokio 读写 trait。
pub enum LocalStream {
    /// 回环 TCP。
    Tcp(tokio::net::TcpStream),
    /// Unix 域套接字。
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Connected for LocalStream {
    type ConnectInfo = LocalConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        match self {
            Self::Tcp(stream) => LocalConnectInfo {
                transport: TransportKind::Tcp,
                peer: stream.peer_addr().ok().map(|addr| addr.to_string()),
            },
            #[cfg(unix)]
            Self::Unix(_) => LocalConnectInfo {
                transport: TransportKind::Unix,
                peer: None,
            },
        }
    }
}

impl AsyncRead for LocalStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buffer),
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_read(cx, buffer),
        }
    }
}

impl AsyncWrite for LocalStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buffer),
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_write(cx, buffer),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// 本地监听器。
pub enum LocalListener {
    /// 回环 TCP。
    Tcp(TcpListener),
    /// Unix 域套接字及其文件路径（退出时用于清理）。
    #[cfg(unix)]
    Unix {
        /// 监听器。
        listener: UnixListener,
        /// socket 文件路径。
        path: PathBuf,
    },
}

impl LocalListener {
    /// 绑定 Unix 域套接字。
    ///
    /// 会处理崩溃遗留的 socket 文件；如果该路径被普通文件占用则拒绝启动，
    /// 绝不删除未知文件。socket 文件权限为 0600，父目录为 0700。
    #[cfg(unix)]
    pub fn bind_unix(path: impl AsRef<Path>) -> Result<Self> {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};

        let path = path.as_ref().to_path_buf();
        // 常见平台限制为 108 字节，预留后缀余量。
        if path.as_os_str().len() >= 104 {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "Unix socket 路径过长（上限约 104 字节）",
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::from)?;
            restrict_to_owner(parent)?;
        }

        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                std::fs::remove_file(&path).map_err(Error::from)?;
            }
            Ok(_) => {
                return Err(Error::invalid_argument(
                    ErrorCode::CONFIG_INVALID,
                    "socket 路径已被非 socket 文件占用",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::from(error)),
        }

        let listener = UnixListener::bind(&path).map_err(Error::from)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(Error::from)?;
        Ok(Self::Unix { listener, path })
    }

    /// 绑定回环 TCP。拒绝非回环地址。
    pub fn bind_tcp(addr: SocketAddr) -> Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(Error::invalid_argument(
                ErrorCode::CONFIG_INVALID,
                "后端只允许监听回环地址（127.0.0.1 / ::1）",
            ));
        }
        let listener = std::net::TcpListener::bind(addr).map_err(Error::from)?;
        listener.set_nonblocking(true).map_err(Error::from)?;
        let listener = TcpListener::from_std(listener).map_err(Error::from)?;
        Ok(Self::Tcp(listener))
    }

    /// 传输方式。
    #[must_use]
    pub const fn transport(&self) -> TransportKind {
        match self {
            Self::Tcp(_) => TransportKind::Tcp,
            #[cfg(unix)]
            Self::Unix { .. } => TransportKind::Unix,
        }
    }

    /// Unix socket 文件路径。
    #[must_use]
    pub fn socket_path(&self) -> Option<&Path> {
        match self {
            Self::Tcp(_) => None,
            #[cfg(unix)]
            Self::Unix { path, .. } => Some(path),
        }
    }

    /// TCP 监听地址。
    #[must_use]
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        match self {
            Self::Tcp(listener) => listener.local_addr().ok(),
            #[cfg(unix)]
            Self::Unix { .. } => None,
        }
    }

    async fn accept(&self) -> io::Result<LocalStream> {
        match self {
            Self::Tcp(listener) => {
                let (stream, _) = listener.accept().await?;
                Ok(LocalStream::Tcp(stream))
            }
            #[cfg(unix)]
            Self::Unix { listener, .. } => {
                let (stream, _) = listener.accept().await?;
                Ok(LocalStream::Unix(stream))
            }
        }
    }
}

/// 启动 gRPC 服务，直到 `shutdown` 完成。
///
/// `shutdown` 完成后服务器停止接受新连接并等待在途请求结束。
pub async fn serve<F>(
    listener: LocalListener,
    token: AuthToken,
    services: Services,
    shutdown: F,
) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let incoming = accept_stream(listener);
    let interceptor = TokenInterceptor::new(token);

    let Services {
        system,
        connection,
        credential,
        metadata,
        query,
        transaction,
    } = services;

    Server::builder()
        .add_service(SystemServiceServer::with_interceptor(
            system,
            interceptor.clone(),
        ))
        .add_service(ConnectionServiceServer::with_interceptor(
            connection,
            interceptor.clone(),
        ))
        .add_service(CredentialServiceServer::with_interceptor(
            credential,
            interceptor.clone(),
        ))
        .add_service(MetadataServiceServer::with_interceptor(
            metadata,
            interceptor.clone(),
        ))
        .add_service(QueryServiceServer::with_interceptor(
            query,
            interceptor.clone(),
        ))
        .add_service(TransactionServiceServer::with_interceptor(
            transaction,
            interceptor,
        ))
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await
        .map_err(|error| {
            Error::new(
                ErrorCode::TRANSPORT_FAILED,
                ErrorCategory::Internal,
                "gRPC 服务异常退出",
            )
            .with_source(error)
        })
}

fn accept_stream(listener: LocalListener) -> BoxStream<'static, io::Result<LocalStream>> {
    // 接受错误直接交给 tonic 处理并结束接受循环：本地 IPC 场景下这通常意味着
    // 进程正在退出或文件描述符耗尽，继续空转没有意义。
    stream::unfold(listener, |listener| async move {
        let accepted = listener.accept().await;
        Some((accepted, listener))
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_rejects_non_loopback() {
        let error = LocalListener::bind_tcp(SocketAddr::from(([0, 0, 0, 0], 0)))
            .err()
            .expect("必须拒绝非回环地址");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_is_private_and_stale_file_is_removed() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("backend.sock");

        let listener = LocalListener::bind_unix(&path).unwrap();
        assert_eq!(listener.transport(), TransportKind::Unix);
        assert_eq!(listener.socket_path(), Some(path.as_path()));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        drop(listener);

        // 崩溃残留的 socket 文件可以被接管。
        let listener = LocalListener::bind_unix(&path).expect("残留 socket 应可接管");
        drop(listener);

        // 普通文件占用则该路径，必须拒绝启动且绝不删除文件。
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"not a socket").unwrap();
        let error = LocalListener::bind_unix(&path)
            .err()
            .expect("普通文件占用时必须拒绝启动");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert_eq!(std::fs::read(&path).unwrap(), b"not a socket");
    }
}
