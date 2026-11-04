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
pub mod mapping;
pub mod metadata;
pub mod query;
pub mod system;
pub mod transaction;

use std::sync::Arc;

use sqarvo_connection::SessionManager;
use sqarvo_drivers::DriverRegistry;
use sqarvo_query_engine::QueryRegistry;

pub use connection::ConnectionServiceImpl;
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
    /// 元数据域。
    pub metadata: MetadataServiceImpl,
    /// 查询域。
    pub query: QueryServiceImpl,
    /// 事务域。
    pub transaction: TransactionServiceImpl,
}

impl Services {
    /// 由共享状态构造全部服务。
    #[must_use]
    pub fn new(
        sessions: Arc<SessionManager>,
        drivers: Arc<DriverRegistry>,
        queries: Arc<QueryRegistry>,
    ) -> Self {
        Self {
            system: SystemServiceImpl::new(Arc::clone(&sessions), drivers),
            connection: ConnectionServiceImpl::new(Arc::clone(&sessions), Arc::clone(&queries)),
            metadata: MetadataServiceImpl::new(),
            query: QueryServiceImpl::new(sessions, queries),
            transaction: TransactionServiceImpl::new(),
        }
    }
}
