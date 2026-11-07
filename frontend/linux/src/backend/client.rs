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
use crate::backend::metadata::{
    self, DdlView, MetadataError, NodePage, failure_from_status as metadata_failure_from_status,
};
use crate::backend::query::{
    ExecuteOutcome, FetchOutcome, QueryFailure, failure_from_status, failure_timeout,
    rows_from_proto, statements_from_proto,
};
use crate::backend::table::{
    ApplyReport, BrowsePage, BrowseQuery, TABLE_PAGE_SIZE, TableColumn, TableEntry, TableFailure,
    TxnSnapshot, failure_from_status as table_failure_from_status,
    rows_from_proto as table_rows_from_proto,
};
use crate::proto::common::v1::Empty;
use crate::proto::common::v1::PageRequest;
use crate::proto::services::v1::connection_service_client::ConnectionServiceClient;
use crate::proto::services::v1::credential_service_client::CredentialServiceClient;
use crate::proto::services::v1::metadata_service_client::MetadataServiceClient;
use crate::proto::services::v1::query_service_client::QueryServiceClient;
use crate::proto::services::v1::system_service_client::SystemServiceClient;
use crate::proto::services::v1::table_data_service_client::TableDataServiceClient;
use crate::proto::services::v1::transaction_service_client::TransactionServiceClient;
use crate::proto::services::v1::{
    ApplyRowChangesRequest, BeginTransactionRequest, BrowseTableRequest, CancelRowChangesRequest,
    GetDdlRequest, GetPrimaryKeyRequest, ListNodesRequest, NodeFilter, ObjectKind, ObjectRef,
    RowDelete, RowUpdate, TransactionRef,
};
use crate::proto::services::v1::{CancelQueryRequest, CloseQueryRequest};
use crate::proto::services::v1::{ExecuteQueryRequest, FetchBatchRequest};

/// gRPC metadata 中携带 token 的键；必须与后端认证拦截器一致。
pub const TOKEN_METADATA_KEY: &str = "x-sqarvo-token";

/// 建立 Channel 的超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 单次 RPC 的超时：本地 IPC 不应出现长时间无响应。
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// `FetchBatch` / `Cancel` / `Close` 的客户端超时（本地 IPC；超过说明后端异常）。
const QUERY_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// 元数据 RPC（`ListNodes` / `GetDdl`）的客户端超时。
const METADATA_RPC_TIMEOUT: Duration = Duration::from_secs(10);

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

/// `SystemService.ListDrivers` 返回的一条驱动描述。
///
/// **驱动清单的唯一来源是后端**：界面不维护自己的驱动列表，否则后端新增驱动时
/// 前端还在展示一份过期清单。`driver_id` 是稳定标识（`mariadb` / `sqlite`），
/// `display_name` 只用于展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverSummary {
    /// 稳定驱动标识。
    pub driver_id: String,
    /// 展示名（由驱动自己声明）。
    pub display_name: String,
    /// 驱动版本。
    pub version: String,
    /// 能力描述符（名称是稳定标识，只追加不改名）。
    pub capabilities: Vec<String>,
}

impl DriverSummary {
    /// 界面上的下拉标签：展示名 + 驱动 ID（对照后端日志时用 ID）。
    ///
    /// 版本号不放进标签：同一驱动的版本由后端随包发布，放进下拉只会让列表变长。
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}（{}）", self.display_name, self.driver_id)
    }
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

    /// 带 token 认证的 `ConnectionService` 客户端（连接对话框、会话列表）。
    pub fn connection_service(
        &self,
    ) -> Result<ConnectionServiceClient<InterceptedService<Channel, TokenInterceptor>>, BackendError>
    {
        let interceptor = TokenInterceptor::new(&self.token)?;
        Ok(ConnectionServiceClient::with_interceptor(
            self.channel.clone(),
            interceptor,
        ))
    }

    /// 带 token 认证的 `CredentialService` 客户端（写入/删除数据库密码）。
    pub fn credential_service(
        &self,
    ) -> Result<CredentialServiceClient<InterceptedService<Channel, TokenInterceptor>>, BackendError>
    {
        let interceptor = TokenInterceptor::new(&self.token)?;
        Ok(CredentialServiceClient::with_interceptor(
            self.channel.clone(),
            interceptor,
        ))
    }

    /// 带 token 认证的 `QueryService` 客户端（执行 / 分批 / 取消 / 关闭）。
    pub fn query_service(
        &self,
    ) -> Result<QueryServiceClient<InterceptedService<Channel, TokenInterceptor>>, BackendError>
    {
        let interceptor = TokenInterceptor::new(&self.token)?;
        Ok(QueryServiceClient::with_interceptor(
            self.channel.clone(),
            interceptor,
        ))
    }

    /// 带 token 认证的 `MetadataService` 客户端（表列表 / 列 / 主键）。
    pub fn metadata_service(
        &self,
    ) -> Result<MetadataServiceClient<InterceptedService<Channel, TokenInterceptor>>, BackendError>
    {
        let interceptor = TokenInterceptor::new(&self.token)?;
        Ok(MetadataServiceClient::with_interceptor(
            self.channel.clone(),
            interceptor,
        ))
    }

    /// 带 token 认证的 `TableDataService` 客户端（浏览 / 行变更）。
    pub fn table_data_service(
        &self,
    ) -> Result<TableDataServiceClient<InterceptedService<Channel, TokenInterceptor>>, BackendError>
    {
        let interceptor = TokenInterceptor::new(&self.token)?;
        Ok(TableDataServiceClient::with_interceptor(
            self.channel.clone(),
            interceptor,
        ))
    }

    /// 带 token 认证的 `TransactionService` 客户端（开始 / 提交 / 回滚）。
    pub fn transaction_service(
        &self,
    ) -> Result<TransactionServiceClient<InterceptedService<Channel, TokenInterceptor>>, BackendError>
    {
        let interceptor = TokenInterceptor::new(&self.token)?;
        Ok(TransactionServiceClient::with_interceptor(
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

    /// 调用 `SystemService.ListDrivers`，返回后端已注册的驱动。
    ///
    /// 连接对话框的驱动下拉只用这份结果，**不硬编码驱动清单**：后端升级多出一个驱动时，
    /// 前端不需要跟着发版（是否认识它的字段集由 [`crate::driver_form`] 决定）。
    /// 结果按 `driver_id` 排序，保证同一后端每次顺序一致。
    pub async fn list_drivers(&self) -> Result<Vec<DriverSummary>, BackendError> {
        let mut client = self.system_service()?;
        let response = tokio::time::timeout(RPC_TIMEOUT, client.list_drivers(Empty {}))
            .await
            .map_err(|_| BackendError::Rpc {
                code: "deadline_exceeded".to_owned(),
                message: format!("ListDrivers 在 {} 秒内未返回", RPC_TIMEOUT.as_secs()),
            })?
            .map_err(map_status)?;
        let mut drivers: Vec<DriverSummary> = response
            .into_inner()
            .drivers
            .into_iter()
            .map(|driver| DriverSummary {
                driver_id: driver.driver_id,
                display_name: driver.display_name,
                version: driver.version,
                capabilities: driver.capabilities,
            })
            .collect();
        drivers.sort_by(|left, right| left.driver_id.cmp(&right.driver_id));
        Ok(drivers)
    }
}

impl BackendClient {
    /// 执行 SQL，返回首批结果与分页 token。
    ///
    /// **不加客户端超时**：长语句由用户通过 `Cancel` 结束，而不是本地提前放弃——
    /// 本地放弃会让界面显示「超时」而后端仍在执行，用户无法判断真实状态。
    /// 服务端在驱动层登记查询，取消语义见 [`BackendClient::cancel_query`]。
    pub async fn execute_query(
        &self,
        session_id: &str,
        sql: &str,
        max_rows_per_batch: u32,
    ) -> Result<ExecuteOutcome, QueryFailure> {
        let mut client = self.query_service().map_err(local_failure)?;
        let request = ExecuteQueryRequest {
            session_id: session_id.to_owned(),
            sql: sql.to_owned(),
            max_rows_per_batch,
            // v0.1 无显式事务：语句自动提交（事务接线见 #29 之后的工作）。
            auto_commit: true,
            // 0 = 由服务端默认策略决定，不在客户端凭空加期限。
            timeout_millis: 0,
        };
        let response = client
            .execute(request)
            .await
            .map_err(|status| failure_from_status(&status))?
            .into_inner();
        Ok(ExecuteOutcome {
            query_id: response.query_id,
            statements: statements_from_proto(&response.statements),
            rows: rows_from_proto(&response.rows, 1),
            page_token: response.page_token,
            has_more: response.has_more,
            warnings: response
                .warnings
                .into_iter()
                .map(|warning| format!("{}：{}", warning.code, warning.message))
                .collect(),
        })
    }

    /// 拉取后续批次。
    ///
    /// `first_number` 是本批第一行的全局行号（1 起），用于跨批次连续编号。
    pub async fn fetch_batch(
        &self,
        query_id: &str,
        statement_index: u32,
        page_token: &str,
        max_rows: u32,
        first_number: u64,
    ) -> Result<FetchOutcome, QueryFailure> {
        let mut client = self.query_service().map_err(local_failure)?;
        let request = FetchBatchRequest {
            query_id: query_id.to_owned(),
            page_token: page_token.to_owned(),
            max_rows,
            statement_index,
        };
        let response = tokio::time::timeout(QUERY_RPC_TIMEOUT, client.fetch_batch(request))
            .await
            .map_err(|_| failure_timeout("拉取下一批", QUERY_RPC_TIMEOUT))?
            .map_err(|status| failure_from_status(&status))?
            .into_inner();
        Ok(FetchOutcome {
            rows: rows_from_proto(&response.rows, first_number),
            page_token: response.page_token,
            has_more: response.has_more,
            complete: response.complete,
        })
    }

    /// 请求取消查询（服务端幂等：已完成/不存在同样返回成功）。
    pub async fn cancel_query(&self, query_id: &str) -> Result<(), QueryFailure> {
        let mut client = self.query_service().map_err(local_failure)?;
        let request = CancelQueryRequest {
            query_id: query_id.to_owned(),
        };
        tokio::time::timeout(QUERY_RPC_TIMEOUT, client.cancel(request))
            .await
            .map_err(|_| failure_timeout("取消查询", QUERY_RPC_TIMEOUT))?
            .map_err(|status| failure_from_status(&status))?;
        Ok(())
    }

    /// 释放游标（幂等；未读完的结果集必须调用它，避免后端长期占用驱动游标）。
    pub async fn close_query(&self, query_id: &str) -> Result<(), QueryFailure> {
        let mut client = self.query_service().map_err(local_failure)?;
        let request = CloseQueryRequest {
            query_id: query_id.to_owned(),
        };
        tokio::time::timeout(QUERY_RPC_TIMEOUT, client.close(request))
            .await
            .map_err(|_| failure_timeout("关闭查询", QUERY_RPC_TIMEOUT))?
            .map_err(|status| failure_from_status(&status))?;
        Ok(())
    }

    /// 列出当前会话所在数据库的表与视图（`MetadataService.ListNodes` 根节点）。
    ///
    /// 只请求一页（500 个对象）：v0.2.1 的根节点列表就是这一层，超过一页时
    /// 如实记录日志而不是假装「只有这些表」。
    pub async fn list_tables(&self, session_id: &str) -> Result<Vec<TableEntry>, TableFailure> {
        let mut client = self.metadata_service().map_err(table_local_failure)?;
        let request = ListNodesRequest {
            parent: Some(ObjectRef {
                session_id: session_id.to_owned(),
                catalog: String::new(),
                schema: String::new(),
                name: String::new(),
                kind: ObjectKind::Unspecified as i32,
            }),
            page: Some(PageRequest {
                page_size: TABLE_PAGE_SIZE,
                page_token: String::new(),
            }),
            // v0.2.2 契约增量（#81）：根节点列表暂不使用名称/种类过滤。
            filter: None,
        };
        let response = tokio::time::timeout(QUERY_RPC_TIMEOUT, client.list_nodes(request))
            .await
            .map_err(|_| table_timeout("读取表列表"))?
            .map_err(|status| table_failure_from_status(&status))?
            .into_inner();
        if let Some(page) = response.page.as_ref()
            && page.truncated
        {
            eprintln!(
                "[data] 表列表已截断：只显示前 {} 个对象",
                response.nodes.len()
            );
        }
        Ok(response
            .nodes
            .iter()
            .filter_map(TableEntry::from_node)
            .collect())
    }

    /// 列出 `parent` 的**直接**子节点（v0.2.2 逐层懒加载，#86）。
    ///
    /// - `parent` 为空引用 = 根（当前连接库的对象）；目录层用
    ///   `kind = CATALOG` 且不带名字表示「列出全部 Catalog」；
    /// - `filter` 由服务端下推；客户端**不**做二次过滤（大小写敏感性跟随数据库）；
    /// - 顺序由服务端决定（种类, 名字），这里原样保留，不重排。
    pub async fn list_nodes(
        &self,
        parent: &ObjectRef,
        page_token: &str,
        filter: Option<NodeFilter>,
        session_id: &str,
    ) -> Result<NodePage, MetadataError> {
        let mut client = self.metadata_service().map_err(metadata_local_failure)?;
        let request = metadata::list_request(parent.clone(), page_token, filter);
        let response = tokio::time::timeout(METADATA_RPC_TIMEOUT, client.list_nodes(request))
            .await
            .map_err(|_| metadata_timeout("读取对象树"))?
            .map_err(|status| metadata_failure_from_status(&status))?
            .into_inner();
        Ok(NodePage::from_proto(&response, session_id))
    }

    /// 搜索当前连接库的对象（`ListNodes` 根层 + `filter.name.contains`）。
    ///
    /// 关键词原样传给服务端：`%` / `_` 由驱动按字面量转义，前端不拼通配符。
    pub async fn search_nodes(
        &self,
        session_id: &str,
        query: &str,
        page_token: &str,
    ) -> Result<NodePage, MetadataError> {
        let mut client = self.metadata_service().map_err(metadata_local_failure)?;
        let request = metadata::search_request(session_id, query, page_token);
        let response = tokio::time::timeout(METADATA_RPC_TIMEOUT, client.list_nodes(request))
            .await
            .map_err(|_| metadata_timeout("搜索对象"))?
            .map_err(|status| metadata_failure_from_status(&status))?
            .into_inner();
        Ok(NodePage::from_proto(&response, session_id))
    }

    /// 读取对象的 DDL（`GetDdl`）。
    ///
    /// 响应里的 `exact` / `source` / `source_detail` 原样保留：界面**必须**展示
    /// 「原文」还是「由客户端拼装」，不能只给一段 SQL 文本。
    pub async fn get_ddl(&self, object: &ObjectRef) -> Result<DdlView, MetadataError> {
        let mut client = self.metadata_service().map_err(metadata_local_failure)?;
        let request = GetDdlRequest {
            object: Some(object.clone()),
        };
        let response = tokio::time::timeout(METADATA_RPC_TIMEOUT, client.get_ddl(request))
            .await
            .map_err(|_| metadata_timeout("读取 DDL"))?
            .map_err(|status| metadata_failure_from_status(&status))?
            .into_inner();
        Ok(DdlView::from_proto(&response))
    }

    /// 读取主键列（按主键顺序；空列表表示无主键）。
    pub async fn get_primary_key(
        &self,
        object: &ObjectRef,
    ) -> Result<Vec<crate::proto::services::v1::PrimaryKeyColumn>, TableFailure> {
        let mut client = self.metadata_service().map_err(table_local_failure)?;
        let request = GetPrimaryKeyRequest {
            object: Some(object.clone()),
        };
        let response = tokio::time::timeout(QUERY_RPC_TIMEOUT, client.get_primary_key(request))
            .await
            .map_err(|_| table_timeout("读取主键"))?
            .map_err(|status| table_failure_from_status(&status))?
            .into_inner();
        Ok(response.columns)
    }

    /// 浏览一页表数据（offset 分页；`first_number` = offset + 1 由调用方给定）。
    ///
    /// **与 cursor 互斥**：本版一律带 `offset`，`page_token` 始终为空——
    /// 两者同时非空会被服务端拒绝（`config.invalid`），驱动也会拒绝。
    pub async fn browse_table(
        &self,
        object: &ObjectRef,
        query: &BrowseQuery,
        first_number: u64,
    ) -> Result<BrowsePage, TableFailure> {
        let mut client = self.table_data_service().map_err(table_local_failure)?;
        let request = BrowseTableRequest {
            object: Some(object.clone()),
            page_size: query.page_size,
            // offset 分页：绝不带 cursor（ADR 0022）。
            page_token: String::new(),
            offset: query.offset,
            sort: query.sort.clone(),
            filter: query.filter.clone(),
            include_total: query.include_total,
            // 0 = 服务端默认计数超时（2 s，上限由驱动钳制）。
            timeout_millis: 0,
        };
        let response = tokio::time::timeout(QUERY_RPC_TIMEOUT, client.browse_table(request))
            .await
            .map_err(|_| table_timeout("浏览表数据"))?
            .map_err(|status| table_failure_from_status(&status))?
            .into_inner();
        let columns: Vec<TableColumn> = response
            .columns
            .iter()
            .map(TableColumn::from_meta)
            .collect();
        Ok(BrowsePage {
            rows: table_rows_from_proto(&response.rows, &columns, first_number),
            columns,
            next_page_token: response.next_page_token,
            has_more: response.has_more,
            read_only: response.read_only,
            read_only_reason: crate::proto::services::v1::ReadOnlyReason::try_from(
                response.read_only_reason,
            )
            .unwrap_or(crate::proto::services::v1::ReadOnlyReason::Unspecified),
            ordering: crate::proto::services::v1::RowOrder::try_from(response.ordering)
                .unwrap_or(crate::proto::services::v1::RowOrder::Unspecified),
            offset: query.offset,
            page_size: query.page_size,
            total_rows: response.total_rows,
            total_kind: response.total_kind,
            total_skipped: response.total_skipped,
        })
    }

    /// 提交一批行变更（主键值 + 列变更；服务端用 prepared statement 绑定参数）。
    ///
    /// `query_id` 由调用方（worker）**自己生成**：一元 RPC 在返回前拿不到服务端 ID，
    /// 只有事先给出的 ID 才可能在执行期间被 [`BackendClient::cancel_row_changes`] 找到
    /// （ADR 0018）。留空表示「本批不可取消」，v0.3 起不再这样做。
    #[allow(
        clippy::too_many_arguments,
        reason = "与契约字段一一对应，拆成结构体只会增加一层没有语义的包装"
    )]
    pub async fn apply_row_changes(
        &self,
        session_id: &str,
        object: &ObjectRef,
        transaction_id: Option<&str>,
        updates: Vec<RowUpdate>,
        deletes: Vec<RowDelete>,
        acknowledge_risk: bool,
        query_id: &str,
    ) -> Result<ApplyReport, TableFailure> {
        let mut client = self.table_data_service().map_err(table_local_failure)?;
        let request = ApplyRowChangesRequest {
            session_id: session_id.to_owned(),
            object: Some(object.clone()),
            transaction_id: transaction_id.unwrap_or_default().to_owned(),
            updates,
            deletes,
            acknowledge_risk,
            // 客户端生成的批次 ID：服务端据此登记取消令牌，`CancelRowChanges` 用同一个
            // ID 定位（响应里的 `query_id` 必须原样回显）。
            query_id: query_id.to_owned(),
        };
        let response = tokio::time::timeout(QUERY_RPC_TIMEOUT, client.apply_row_changes(request))
            .await
            .map_err(|_| table_timeout("提交行变更"))?
            .map_err(|status| table_failure_from_status(&status))?
            .into_inner();
        Ok(ApplyReport {
            results: response.results,
            total_rows_affected: response.total_rows_affected,
            clamped: response.clamped,
            deferred_changes: response.deferred_changes,
            cancelled: response.cancelled,
            executed_changes: response.executed_changes,
            query_id: response.query_id,
        })
    }

    /// 取消一个在途的行变更批次（ADR 0018）。
    ///
    /// 幂等：批次已结束 / 不存在时服务端同样返回成功，因此「重复点取消」「提交先结束
    /// 再点取消」都不会报错。驱动未实现取消时返回 `capability.unsupported`——这里如实
    /// 返回失败，绝不假装取消成功。
    pub async fn cancel_row_changes(&self, query_id: &str) -> Result<(), TableFailure> {
        let mut client = self.table_data_service().map_err(table_local_failure)?;
        tokio::time::timeout(
            QUERY_RPC_TIMEOUT,
            client.cancel_row_changes(CancelRowChangesRequest {
                query_id: query_id.to_owned(),
            }),
        )
        .await
        .map_err(|_| table_timeout("取消行变更批次"))?
        .map_err(|status| table_failure_from_status(&status))?;
        Ok(())
    }

    /// 开始事务（一个会话同时最多一个；只读事务不能写入）。
    pub async fn begin_transaction(
        &self,
        session_id: &str,
        read_only: bool,
    ) -> Result<TxnSnapshot, TableFailure> {
        let mut client = self.transaction_service().map_err(table_local_failure)?;
        let request = BeginTransactionRequest {
            session_id: session_id.to_owned(),
            // 0 = 未指定，由服务端/数据库默认隔离级别决定。
            isolation: crate::proto::services::v1::IsolationLevel::Unspecified as i32,
            read_only,
        };
        let info = tokio::time::timeout(QUERY_RPC_TIMEOUT, client.begin(request))
            .await
            .map_err(|_| table_timeout("开始事务"))?
            .map_err(|status| table_failure_from_status(&status))?
            .into_inner();
        Ok(TxnSnapshot {
            transaction_id: Some(info.transaction_id),
            read_only: info.read_only,
            state: crate::proto::services::v1::TransactionState::try_from(info.state)
                .unwrap_or(crate::proto::services::v1::TransactionState::Unspecified),
        })
    }

    /// 提交事务（幂等：已结束的事务同样返回成功）。
    pub async fn commit_transaction(&self, transaction_id: &str) -> Result<(), TableFailure> {
        let mut client = self.transaction_service().map_err(table_local_failure)?;
        tokio::time::timeout(
            QUERY_RPC_TIMEOUT,
            client.commit(TransactionRef {
                transaction_id: transaction_id.to_owned(),
            }),
        )
        .await
        .map_err(|_| table_timeout("提交事务"))?
        .map_err(|status| table_failure_from_status(&status))?;
        Ok(())
    }

    /// 回滚事务（幂等；失败态也允许回滚）。
    pub async fn rollback_transaction(&self, transaction_id: &str) -> Result<(), TableFailure> {
        let mut client = self.transaction_service().map_err(table_local_failure)?;
        tokio::time::timeout(
            QUERY_RPC_TIMEOUT,
            client.rollback(TransactionRef {
                transaction_id: transaction_id.to_owned(),
            }),
        )
        .await
        .map_err(|_| table_timeout("回滚事务"))?
        .map_err(|status| table_failure_from_status(&status))?;
        Ok(())
    }
}

/// 客户端自身（Channel / token）的本地失败：不是查询语义错误，保留为 `transport.failed`。
fn local_failure(error: BackendError) -> QueryFailure {
    QueryFailure::Other {
        code: "transport.failed".to_owned(),
        message: error.short(),
    }
}

/// 表数据域的本地失败（与查询域分开：界面按 `TableFailure` 展示）。
fn table_local_failure(error: BackendError) -> TableFailure {
    TableFailure::Other {
        code: "transport.failed".to_owned(),
        message: error.short(),
    }
}

/// 表数据 RPC 的客户端超时。
fn table_timeout(operation: &str) -> TableFailure {
    TableFailure::Other {
        code: "deadline_exceeded".to_owned(),
        message: format!("{operation}在 {} 秒内未返回", QUERY_RPC_TIMEOUT.as_secs()),
    }
}

/// 元数据 RPC 的客户端超时。
fn metadata_timeout(operation: &str) -> MetadataError {
    MetadataError::Other {
        code: "deadline_exceeded".to_owned(),
        message: format!(
            "{operation}在 {} 秒内未返回",
            METADATA_RPC_TIMEOUT.as_secs()
        ),
    }
}

/// 本地建 Channel / 取服务客户端失败：不是「对象不存在」，单独归类。
fn metadata_local_failure(error: BackendError) -> MetadataError {
    MetadataError::Unavailable {
        message: error.short(),
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
