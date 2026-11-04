//! 连接域实现：测试连接、建立/断开会话、列出会话。

use std::collections::BTreeMap;
use std::sync::Arc;

use sqarvo_common::ConnectionId;
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{
    ConnectionParams as DriverConnectionParams, DriverId, TlsMode as DriverTlsMode,
};
use sqarvo_query_engine::QueryRegistry;
use tonic::{Request, Response, Status};

use crate::error::to_status;
use crate::proto::common::v1::{Empty, Warning};
use crate::proto::services::v1::{
    ConnectRequest, ConnectResponse, ConnectionParams, DisconnectRequest, ListSessionsResponse,
    SessionSummary, TestConnectionRequest, TestConnectionResponse,
    connection_service_server::ConnectionService,
};

/// `ConnectionService` 实现。
pub struct ConnectionServiceImpl {
    sessions: Arc<SessionManager>,
    queries: Arc<QueryRegistry>,
}

impl ConnectionServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new(sessions: Arc<SessionManager>, queries: Arc<QueryRegistry>) -> Self {
        Self { sessions, queries }
    }
}

#[tonic::async_trait]
impl ConnectionService for ConnectionServiceImpl {
    async fn test_connection(
        &self,
        request: Request<TestConnectionRequest>,
    ) -> Result<Response<TestConnectionResponse>, Status> {
        let params = request
            .into_inner()
            .params
            .ok_or_else(|| Status::invalid_argument("缺少连接参数"))?;
        let params = to_domain_params(params)?;

        // 凭据解析由 SessionManager 负责（ADR 0015）：params.credential_ref 在这里
        // 被解析成 Secret，gRPC 层不接触凭据存储，也不接触明文密码。
        let report = self
            .sessions
            .test_connection(&params)
            .await
            .map_err(to_status)?;

        Ok(Response::new(TestConnectionResponse {
            reachable: report.reachable,
            latency_millis: u64::try_from(report.latency.as_millis()).unwrap_or(u64::MAX),
            server_version: report.server_version.unwrap_or_default(),
            warnings: report
                .warnings
                .into_iter()
                .map(|warning| Warning {
                    code: warning.code,
                    message: warning.message,
                })
                .collect(),
        }))
    }

    async fn connect(
        &self,
        request: Request<ConnectRequest>,
    ) -> Result<Response<ConnectResponse>, Status> {
        let request = request.into_inner();
        let params = request
            .params
            .ok_or_else(|| Status::invalid_argument("缺少连接参数"))?;
        let params = to_domain_params(params)?;
        let display_name = if request.display_name.trim().is_empty() {
            params.host.clone()
        } else {
            request.display_name
        };

        let summary = self
            .sessions
            .connect(display_name, params)
            .await
            .map_err(to_status)?;

        Ok(Response::new(ConnectResponse {
            connection_id: summary.connection_id.to_string(),
            session_id: summary.session_id.to_string(),
            driver_id: summary.driver_id.to_string(),
            // 驱动未在连接阶段暴露版本时保持为空，前端应以 TestConnection 结果为准。
            server_version: String::new(),
            capabilities: summary
                .capabilities
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            warnings: Vec::new(),
        }))
    }

    async fn disconnect(
        &self,
        request: Request<DisconnectRequest>,
    ) -> Result<Response<Empty>, Status> {
        let connection_id: ConnectionId = request
            .into_inner()
            .connection_id
            .parse()
            .map_err(|_| Status::invalid_argument("连接 ID 格式非法"))?;
        let summary = self
            .sessions
            .disconnect(&connection_id)
            .await
            .map_err(to_status)?;
        // 断连即回收该会话的全部查询登记，避免悬挂项。
        self.queries.remove_session(&summary.session_id);
        Ok(Response::new(Empty {}))
    }

    async fn list_sessions(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        let sessions = self
            .sessions
            .list()
            .await
            .into_iter()
            .map(|summary| SessionSummary {
                connection_id: summary.connection_id.to_string(),
                session_id: summary.session_id.to_string(),
                driver_id: summary.driver_id.to_string(),
                display_name: summary.display_name,
                capabilities: summary
                    .capabilities
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
                connected_at: summary
                    .connected_at
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default(),
            })
            .collect();

        Ok(Response::new(ListSessionsResponse { sessions }))
    }
}

/// Protobuf 连接参数 → 领域连接参数。
fn to_domain_params(params: ConnectionParams) -> Result<DriverConnectionParams, Status> {
    let driver_id = DriverId::new(params.driver_id).map_err(to_status)?;
    let port = u16::try_from(params.port)
        .map_err(|_| Status::invalid_argument("端口必须在 0..=65535 之间"))?;
    let tls = to_domain_tls_mode(params.tls_mode)?;

    Ok(DriverConnectionParams {
        driver_id,
        host: params.host,
        port,
        database: non_empty(params.database),
        username: non_empty(params.username),
        credential_ref: non_empty(params.credential_ref),
        tls,
        tls_server_name: non_empty(params.tls_server_name),
        properties: params.properties.into_iter().collect::<BTreeMap<_, _>>(),
    })
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn to_domain_tls_mode(value: i32) -> Result<DriverTlsMode, Status> {
    use crate::proto::services::v1::TlsMode as ProtoTlsMode;

    match ProtoTlsMode::try_from(value) {
        Ok(ProtoTlsMode::Unspecified) => Ok(DriverTlsMode::Unspecified),
        Ok(ProtoTlsMode::Disabled) => Ok(DriverTlsMode::Disabled),
        Ok(ProtoTlsMode::Preferred) => Ok(DriverTlsMode::Preferred),
        Ok(ProtoTlsMode::Required) => Ok(DriverTlsMode::Required),
        Ok(ProtoTlsMode::VerifyCa) => Ok(DriverTlsMode::VerifyCa),
        Ok(ProtoTlsMode::VerifyFull) => Ok(DriverTlsMode::VerifyFull),
        Err(_) => Err(Status::invalid_argument("未知的 TLS 模式")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ConnectionParams {
        ConnectionParams {
            driver_id: "postgres".to_owned(),
            host: "localhost".to_owned(),
            port: 5432,
            database: String::new(),
            username: String::new(),
            credential_ref: String::new(),
            tls_mode: 0,
            tls_server_name: String::new(),
            properties: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn converts_and_normalizes_params() {
        let converted = to_domain_params(params()).unwrap();
        assert_eq!(converted.driver_id.as_str(), "postgres");
        assert!(converted.database.is_none());
        assert_eq!(converted.tls, DriverTlsMode::Unspecified);
    }

    #[test]
    fn rejects_bad_driver_and_port() {
        let mut bad = params();
        bad.driver_id = "MySQL".to_owned();
        assert_eq!(
            to_domain_params(bad).unwrap_err().code(),
            tonic::Code::InvalidArgument
        );

        let mut bad = params();
        bad.port = u32::from(u16::MAX) + 1;
        assert!(to_domain_params(bad).is_err());
    }

    #[test]
    fn rejects_unknown_tls_mode() {
        assert!(to_domain_tls_mode(99).is_err());
    }
}
