//! gRPC 客户端封装：建立 Channel、注入本地会话 token、调用 `SystemService`。
//!
//! 传输细节集中在这里，UI 不接触 socket / TCP / metadata：
//!
//! - Unix 域套接字走 `connect_with_connector` 自定义连接器（与后端 e2e 测试一致）；
//! - 回环 TCP 走 tonic 默认 `connect`；
//! - 每个请求都带 `x-sqarvo-token`，与 `backend/crates/rpc-server/src/auth.rs` 对应。
//!
//! token 只保存在 [`BackendClient`] 内部，`Debug` 输出与错误文案均不包含原文。

use std::fmt;
use std::time::Duration;

use http::Uri;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};
use tower::service_fn;

use crate::backend::endpoint::{EndpointInfo, TransportKind};
use crate::backend::error::BackendError;
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::system_service_client::SystemServiceClient;

/// gRPC metadata 中携带 token 的键；必须与后端认证拦截器一致。
pub const TOKEN_METADATA_KEY: &str = "x-sqarvo-token";

/// 建立 Channel 的超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 单次 RPC 的超时：本地 IPC 不应出现长时间无响应。
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// 后端版本信息（`SystemService.GetVersion` 的结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// 后端服务版本。
    pub service_version: String,
    /// 后端协议版本。
    pub protocol_version: String,
    /// `debug` 或 `release`。
    pub build_profile: String,
}

/// 健康状态摘要（诊断用，不参与握手判定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthInfo {
    /// 状态名，例如 `serving`。
    pub state: String,
    /// 后端自述的诊断信息。
    pub detail: String,
    /// 已注册驱动数量。
    pub registered_drivers: u32,
}

/// 把本地会话 token 注入每个请求的拦截器。
#[derive(Clone)]
pub struct TokenInterceptor {
    value: MetadataValue<tonic::metadata::Ascii>,
}

impl TokenInterceptor {
    /// 由端点描述中的 token 构造拦截器。
    pub fn new(token: &str) -> Result<Self, BackendError> {
        let value = MetadataValue::try_from(token).map_err(|_| BackendError::EndpointInvalid {
            path: std::path::PathBuf::new(),
            detail: "会话 token 不是合法的 HTTP/2 metadata 值".to_owned(),
        })?;
        Ok(Self { value })
    }
}

impl Interceptor for TokenInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request
            .metadata_mut()
            .insert(TOKEN_METADATA_KEY, self.value.clone());
        Ok(request)
    }
}

impl fmt::Debug for TokenInterceptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenInterceptor")
            .field("token", &"<redacted>")
            .finish()
    }
}

/// 已连接的后端客户端。
///
/// 后续的连接对话框（#27）、SQL 执行（#28）都应复用同一个实例：Channel 内部
/// 多路复用，克隆 [`BackendClient`] 是廉价的。
#[derive(Clone)]
pub struct BackendClient {
    channel: Channel,
    token: String,
    transport: String,
}

impl BackendClient {
    /// 按端点描述建立 Channel（不发起 RPC）。
    pub async fn connect(endpoint: &EndpointInfo) -> Result<Self, BackendError> {
        let channel = match endpoint.transport {
            TransportKind::Unix => {
                let socket_path =
                    endpoint
                        .socket_path
                        .clone()
                        .ok_or_else(|| BackendError::ConnectFailed {
                            transport: TransportKind::Unix.label().to_owned(),
                            detail: "端点描述缺少 socket_path".to_owned(),
                        })?;
                let socket = std::path::PathBuf::from(&socket_path);
                Endpoint::from_shared("http://[::]:1".to_owned())
                    .map_err(|error| BackendError::ConnectFailed {
                        transport: TransportKind::Unix.label().to_owned(),
                        detail: error.to_string(),
                    })?
                    .connect_timeout(CONNECT_TIMEOUT)
                    .connect_with_connector(service_fn(move |_: Uri| {
                        let socket = socket.clone();
                        async move {
                            Ok::<_, std::io::Error>(TokioIo::new(
                                UnixStream::connect(socket).await?,
                            ))
                        }
                    }))
                    .await
                    .map_err(|error| BackendError::ConnectFailed {
                        transport: TransportKind::Unix.label().to_owned(),
                        detail: format!("{socket_path}：{error}"),
                    })?
            }
            TransportKind::Tcp => {
                let host = endpoint
                    .host
                    .clone()
                    .ok_or_else(|| BackendError::ConnectFailed {
                        transport: TransportKind::Tcp.label().to_owned(),
                        detail: "端点描述缺少 host".to_owned(),
                    })?;
                let port = endpoint.port.ok_or_else(|| BackendError::ConnectFailed {
                    transport: TransportKind::Tcp.label().to_owned(),
                    detail: "端点描述缺少 port".to_owned(),
                })?;
                // IPv6 字面量必须加方括号才能拼进 URI。
                let authority = if host.contains(':') {
                    format!("[{host}]:{port}")
                } else {
                    format!("{host}:{port}")
                };
                let uri = format!("http://{authority}");
                Endpoint::from_shared(uri)
                    .map_err(|error| BackendError::ConnectFailed {
                        transport: TransportKind::Tcp.label().to_owned(),
                        detail: error.to_string(),
                    })?
                    .connect_timeout(CONNECT_TIMEOUT)
                    .connect()
                    .await
                    .map_err(|error| BackendError::ConnectFailed {
                        transport: TransportKind::Tcp.label().to_owned(),
                        detail: format!("{authority}：{error}"),
                    })?
            }
        };

        Ok(Self {
            channel,
            token: endpoint.token.clone(),
            transport: endpoint.transport_label(),
        })
    }

    /// 共用的 Channel，供后续业务客户端（#27 连接对话框、#28 SQL 执行）复用。
    #[must_use]
    #[allow(
        dead_code,
        reason = "为后续 Issue 复用保留的公开入口，当前仅 SystemService 使用"
    )]
    pub fn channel(&self) -> Channel {
        self.channel.clone()
    }

    /// 传输方式描述（不含 token）。
    #[must_use]
    #[allow(dead_code, reason = "为后续 Issue 展示连接信息保留")]
    pub fn transport(&self) -> &str {
        &self.transport
    }

    /// 带 token 认证的 `SystemService` 客户端。
    pub fn system_service(
        &self,
    ) -> Result<SystemServiceClient<InterceptedService<Channel, TokenInterceptor>>, BackendError>
    {
        let interceptor = TokenInterceptor::new(&self.token)?;
        Ok(SystemServiceClient::with_interceptor(
            self.channel.clone(),
            interceptor,
        ))
    }

    /// 调用 `SystemService.GetVersion`。
    pub async fn get_version(&self) -> Result<VersionInfo, BackendError> {
        let mut client = self.system_service()?;
        let response = tokio::time::timeout(RPC_TIMEOUT, client.get_version(Empty {}))
            .await
            .map_err(|_| BackendError::Rpc {
                code: "deadline_exceeded".to_owned(),
                message: format!("GetVersion 在 {} 秒内未返回", RPC_TIMEOUT.as_secs()),
            })?
            .map_err(map_status)?;
        let version = response.into_inner();
        Ok(VersionInfo {
            service_version: version.service_version,
            protocol_version: version.protocol_version,
            build_profile: version.build_profile,
        })
    }

    /// 调用 `SystemService.GetHealth`；用于连接建立后的存活探测。
    pub async fn get_health(&self) -> Result<HealthInfo, BackendError> {
        let mut client = self.system_service()?;
        let response = tokio::time::timeout(RPC_TIMEOUT, client.get_health(Empty {}))
            .await
            .map_err(|_| BackendError::Rpc {
                code: "deadline_exceeded".to_owned(),
                message: format!("GetHealth 在 {} 秒内未返回", RPC_TIMEOUT.as_secs()),
            })?
            .map_err(map_status)?;
        let health = response.into_inner();
        let state = crate::proto::services::v1::health_response::State::try_from(health.state)
            .map(|state| format!("{state:?}").to_lowercase())
            .unwrap_or_else(|_| format!("unknown({})", health.state));
        Ok(HealthInfo {
            state,
            detail: health.detail,
            registered_drivers: health.registered_drivers,
        })
    }
}

impl fmt::Debug for BackendClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendClient")
            .field("transport", &self.transport)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// 把 gRPC 状态码映射为可展示的错误，附带稳定错误码字符串。
fn map_status(status: Status) -> BackendError {
    BackendError::Rpc {
        code: status.code().to_string(),
        message: sanitize(status.message()),
    }
}

/// 截断并清理后端消息：只保留首行、限制长度，避免把整段调试信息塞进界面。
fn sanitize(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or("").trim();
    if first_line.chars().count() > 300 {
        let truncated: String = first_line.chars().take(300).collect();
        format!("{truncated}…")
    } else if first_line.is_empty() {
        "（后端未提供错误消息）".to_owned()
    } else {
        first_line.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interceptor_debug_redacts_token() {
        let token = "ab".repeat(32);
        let interceptor = TokenInterceptor::new(&token).unwrap();
        assert!(!format!("{interceptor:?}").contains(&token));
    }

    #[test]
    fn sanitize_keeps_first_line_only() {
        assert_eq!(sanitize("第一行\n第二行"), "第一行");
        assert_eq!(sanitize(""), "（后端未提供错误消息）");
        assert!(sanitize(&"x".repeat(400)).chars().count() <= 301);
    }
}
