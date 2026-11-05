//! 事务域实现：`Begin` / `Commit` / `Rollback`（#44）。
//!
//! 三个 RPC 都是薄翻译层：会话与事务状态由 `sqarvo-connection` 持有，
//! 本模块只做「Protobuf ↔ 领域类型」和「领域错误 → gRPC 状态」。
//!
//! 语义要点（ADR 0017 §3）：
//!
//! - 一个会话同时最多一个打开事务，重复 `Begin` 返回 `transaction.already_active`；
//! - 失败态只允许回滚，`Commit` 返回 `transaction.failed`；
//! - 重复 `Commit` / `Rollback` 幂等（对已结束或未知事务返回 OK）——前端在断线重连、
//!   超时重试、双击时无法区分「已提交」与「从未收到」，返回错误只会制造无法处置的提示；
//! - 驱动不支持事务时返回 `capability.unsupported`，**绝不**降级为自动提交。

use std::sync::Arc;

use sqarvo_common::{SessionId, TransactionId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::TransactionOptions;
use tonic::{Request, Response, Status};

use crate::error::to_status;
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{
    BeginTransactionRequest, TransactionInfo, TransactionRef,
    transaction_service_server::TransactionService,
};
use crate::services::mapping;

/// `TransactionService` 实现。
pub struct TransactionServiceImpl {
    sessions: Arc<SessionManager>,
}

impl TransactionServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new(sessions: Arc<SessionManager>) -> Self {
        Self { sessions }
    }
}

#[tonic::async_trait]
impl TransactionService for TransactionServiceImpl {
    async fn begin(
        &self,
        request: Request<BeginTransactionRequest>,
    ) -> Result<Response<TransactionInfo>, Status> {
        let request = request.into_inner();
        let session_id: SessionId = request
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("会话 ID 格式非法"))?;
        let isolation = mapping::isolation_from_proto(request.isolation)?;

        // 会话不存在 → session.not_found；能力与重复打开由连接层判定并给出稳定错误码。
        let status = self
            .sessions
            .begin_transaction(
                &session_id,
                TransactionOptions {
                    isolation,
                    read_only: request.read_only,
                },
            )
            .await
            .map_err(to_status)?;

        Ok(Response::new(TransactionInfo {
            transaction_id: status.transaction_id.to_string(),
            session_id: session_id.to_string(),
            isolation: mapping::isolation_to_proto(status.isolation) as i32,
            read_only: status.read_only,
            state: mapping::transaction_state_to_proto(status.state) as i32,
        }))
    }

    async fn commit(&self, request: Request<TransactionRef>) -> Result<Response<Empty>, Status> {
        let transaction_id = parse_transaction_id(&request.into_inner().transaction_id)?;
        self.sessions
            .commit_transaction(&transaction_id)
            .await
            .map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }

    async fn rollback(&self, request: Request<TransactionRef>) -> Result<Response<Empty>, Status> {
        let transaction_id = parse_transaction_id(&request.into_inner().transaction_id)?;
        self.sessions
            .rollback_transaction(&transaction_id)
            .await
            .map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }
}

/// 解析事务 ID。
///
/// 空串或格式错误属于客户端 bug，返回 `INVALID_ARGUMENT`；
/// 格式合法但未知的 ID 才是幂等路径（由连接层返回成功）。
fn parse_transaction_id(raw: &str) -> Result<TransactionId, Status> {
    if raw.trim().is_empty() {
        return Err(Status::invalid_argument("事务 ID 不能为空"));
    }
    raw.parse()
        .map_err(|_| Status::invalid_argument("事务 ID 格式非法"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_malformed_transaction_id() {
        assert!(parse_transaction_id("").is_err());
        assert!(parse_transaction_id("   ").is_err());
        assert!(parse_transaction_id("not-a-txn").is_err());
    }

    #[test]
    fn accepts_valid_transaction_id() {
        let id = TransactionId::new();
        assert_eq!(parse_transaction_id(&id.to_string()).unwrap(), id);
    }
}
