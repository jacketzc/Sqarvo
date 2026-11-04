//! 事务域实现。
//!
//! 事务状态机已在 `sqarvo-transaction` 中实现并通过单元测试，但把事务绑定到
//! 具体会话需要驱动支持 `BEGIN`/`COMMIT`/`ROLLBACK`。当前没有可用驱动，
//! 因此三个 RPC 返回 `UNIMPLEMENTED`。
//!
//! 绝不静默降级为自动提交——那会让用户在不知情的情况下破坏事务语义。

use tonic::{Request, Response, Status};

use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{
    BeginTransactionRequest, TransactionInfo, TransactionRef,
    transaction_service_server::TransactionService,
};

/// `TransactionService` 实现。
#[derive(Debug, Default)]
pub struct TransactionServiceImpl;

impl TransactionServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

const NOT_IMPLEMENTED: &str = "显式事务尚未实现：等待驱动 SPI 提供事务能力；不会降级为自动提交";

#[tonic::async_trait]
impl TransactionService for TransactionServiceImpl {
    async fn begin(
        &self,
        _request: Request<BeginTransactionRequest>,
    ) -> Result<Response<TransactionInfo>, Status> {
        Err(Status::unimplemented(NOT_IMPLEMENTED))
    }

    async fn commit(&self, _request: Request<TransactionRef>) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented(NOT_IMPLEMENTED))
    }

    async fn rollback(&self, _request: Request<TransactionRef>) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented(NOT_IMPLEMENTED))
    }
}
