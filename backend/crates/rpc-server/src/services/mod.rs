//! gRPC 服务实现。
//!
//! 每个服务实现只做三件事：
//!
//! 1. 把 Protobuf 消息转换为领域类型；
//! 2. 调用通用业务层；
//! 3. 把领域错误映射为 gRPC 状态。
//!
//! 业务规则不写在这里，也不写在前端。

pub mod connection;
pub mod credential;
pub mod mapping;
pub mod metadata;
pub mod query;
pub mod system;
pub mod transaction;

use std::sync::Arc;

use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use sqarvo_local_store::CredentialStore;
use sqarvo_query_engine::QueryRegistry;

pub use connection::ConnectionServiceImpl;
pub use credential::CredentialServiceImpl;
pub use metadata::MetadataServiceImpl;
pub use query::QueryServiceImpl;
pub use system::SystemServiceImpl;
pub use transaction::TransactionServiceImpl;

/// 需要挂载到 gRPC 服务器上的服务集合。
pub struct Services {
    /// 系统域。
    pub system: SystemServiceImpl,
    /// 连接域。
    pub connection: ConnectionServiceImpl,
    /// 凭据域。
    pub credential: CredentialServiceImpl,
    /// 元数据域。
    pub metadata: MetadataServiceImpl,
    /// 查询域。
    pub query: QueryServiceImpl,
    /// 事务域。
    pub transaction: TransactionServiceImpl,
}

impl Services {
    /// 由共享状态构造全部服务。
    ///
    /// `credentials` 必须与装配 [`SessionManager`] 时用的是同一个存储：写入路径与读取
    /// 路径看到同一份数据，否则刚保存的密码会在连接时变成 `credential.not_found`。
    /// 因此只保留这一个构造入口，不提供「不带凭据存储」的第二条路径。
    #[must_use]
    pub fn new(
        sessions: Arc<SessionManager>,
        drivers: Arc<DriverRegistry>,
        queries: Arc<QueryRegistry>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            system: SystemServiceImpl::new(Arc::clone(&sessions), drivers),
            connection: ConnectionServiceImpl::new(Arc::clone(&sessions), Arc::clone(&queries)),
            credential: CredentialServiceImpl::new(credentials),
            metadata: MetadataServiceImpl::new(Arc::clone(&sessions)),
            // 事务服务与会话管理共享同一份登记表：`Begin` 绑定的会话必须是
            // `Disconnect` / 断连清理看到的那一个（#44）。
            transaction: TransactionServiceImpl::new(Arc::clone(&sessions)),
            query: QueryServiceImpl::new(sessions, queries),
        }
    }
}
