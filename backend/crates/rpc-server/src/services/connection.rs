//! 连接域实现：测试连接、建立/断开会话、列出会话。

use std::collections::BTreeMap;
use std::sync::Arc;

use sqarvo_common::{ConnectionId, Error, ErrorCode, SessionId};
use sqarvo_connection::{RowChangeBatches, SessionHandle, SessionManager};
use sqarvo_driver_api::{
    ConnectionParams as DriverConnectionParams, DriverId, SessionContext, TlsMode as DriverTlsMode,
};
use sqarvo_query_engine::QueryRegistry;
use tonic::{Request, Response, Status};

use crate::error::to_status;
use crate::proto::common::v1::{Empty, Warning};
use crate::proto::services::v1::{
    ConnectRequest, ConnectResponse, ConnectionParams, DisconnectRequest, GetSessionContextRequest,
    GetSessionContextResponse, ListSessionsResponse, SessionSummary, SetSessionContextRequest,
    TestConnectionRequest, TestConnectionResponse, connection_service_server::ConnectionService,
};
use crate::services::mapping;

/// `ConnectionService` 实现。
pub struct ConnectionServiceImpl {
    sessions: Arc<SessionManager>,
    queries: Arc<QueryRegistry>,
    /// 在途行变更批次（#70）：断连时让它们停下并清掉登记，避免悬挂项。
    batches: Arc<RowChangeBatches>,
}

impl ConnectionServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new(
        sessions: Arc<SessionManager>,
        queries: Arc<QueryRegistry>,
        batches: Arc<RowChangeBatches>,
    ) -> Self {
        Self {
            sessions,
            queries,
            batches,
        }
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
        // 在途行变更批次同理（#70）：请求取消让驱动在下一个行边界停下，
        // 并移除登记，`CancelRowChanges` 随后按幂等返回成功。
        self.batches.remove_session(&summary.session_id);
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
                // 事务字段是读取时的快照；未打开事务时为空 message。
                transaction: summary
                    .transaction
                    .as_ref()
                    .map(mapping::transaction_status_to_proto),
            })
            .collect();

        Ok(Response::new(ListSessionsResponse { sessions }))
    }

    /// 会话的库上下文（ADR 0030）：服务端真值，不是连接配置的回显。
    ///
    /// 只读，不改变会话状态。驱动没有声明 `session_context` 能力时按
    /// `capability.unsupported`（`UNIMPLEMENTED`）返回，而不是伪造一个库名。
    async fn get_session_context(
        &self,
        request: Request<GetSessionContextRequest>,
    ) -> Result<Response<GetSessionContextResponse>, Status> {
        let session_id: SessionId = request
            .into_inner()
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("会话 ID 格式非法"))?;
        let handle = self
            .sessions
            .acquire(&session_id)
            .await
            .map_err(to_status)?;
        // 能力声明先于调用：未声明就不问驱动，直接如实报不支持。
        if !handle.supports_session_context() {
            return Err(to_status(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "当前驱动不提供库上下文",
            )));
        }
        let context = handle.session_context().await.map_err(to_status)?;
        Ok(Response::new(context_response(&handle, context)))
    }

    /// 把会话切到某个库（ADR 0030），返回切完之后的上下文。
    ///
    /// 与 `QueryService.Execute` 的 `catalog` 是**同一能力的两个入口**：都落到驱动的
    /// `use_catalog`，因此幂等、事务中拒绝（`*.catalog_switch_in_transaction`）、失败不回落
    /// 这几条语义完全一致，这里不另起一套。
    ///
    /// `catalog` 为空串表示「不改变会话当前库」：直接读回当前上下文并返回，不报错，也不做
    /// 切库能力检查——不切库就用不到该能力，这条路径在 SQLite 上同样成功。
    async fn set_session_context(
        &self,
        request: Request<SetSessionContextRequest>,
    ) -> Result<Response<GetSessionContextResponse>, Status> {
        let request = request.into_inner();
        let session_id: SessionId = request
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("会话 ID 格式非法"))?;
        let handle = self
            .sessions
            .acquire(&session_id)
            .await
            .map_err(to_status)?;
        let catalog = request.catalog.trim().to_owned();

        if catalog.is_empty() {
            let context = handle.session_context().await.map_err(to_status)?;
            return Ok(Response::new(context_response(&handle, context)));
        }
        // 能力声明先于调用：驱动没声明切库就不发 `USE`，直接如实报不支持。
        if !handle.supports_catalog_switch() {
            return Err(to_status(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "当前驱动不支持「切换库」，请在 SQL 里写限定名",
            )));
        }
        // 切库失败直接返回（未知库 / 无权限 / 事务中），不回落旧库。
        handle.set_catalog(&catalog).await.map_err(to_status)?;
        let context = handle.session_context().await.map_err(to_status)?;
        Ok(Response::new(context_response(&handle, context)))
    }
}

/// 会话句柄 + 驱动回报的上下文 → `GetSessionContextResponse`。
///
/// `GetSessionContext` 与 `SetSessionContext` 共用这一份映射：「空库名不得用连接配置顶替」
/// 的规则只写一遍，两条 RPC 不可能给出互相矛盾的答案。
fn context_response(handle: &SessionHandle, context: SessionContext) -> GetSessionContextResponse {
    let summary = handle.summary();
    GetSessionContextResponse {
        session_id: summary.session_id.to_string(),
        driver_id: summary.driver_id.to_string(),
        // 空串 = 这条会话没有默认库；绝不拿连接配置顶替。
        default_catalog: context.default_catalog.unwrap_or_default(),
        catalog_kind: mapping::catalog_kind_to_proto(context.catalog_kind) as i32,
    }
}

/// Protobuf 连接参数 → 领域连接参数。
pub(super) fn to_domain_params(params: ConnectionParams) -> Result<DriverConnectionParams, Status> {
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
