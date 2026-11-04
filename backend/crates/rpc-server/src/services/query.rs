//! 查询域实现。
//!
//! 生命周期管理（查询登记、并发上限、幂等取消）已经可用，但 SQL 执行依赖
//! 驱动会话，当前尚未接入任何驱动，因此 `Execute` 返回 `UNIMPLEMENTED`。
//!
//! `Cancel` / `Close` 对未登记查询返回 `NOT_FOUND`——这是真实语义，不是占位。

use std::sync::Arc;

use sqarvo_common::QueryId;
use sqarvo_query_engine::QueryRegistry;
use tonic::{Request, Response, Status};

use crate::error::to_status;
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{
    CancelQueryRequest, CloseQueryRequest, ExecuteQueryRequest, ExecuteQueryResponse,
    FetchBatchRequest, FetchBatchResponse, query_service_server::QueryService,
};

/// `QueryService` 实现。
pub struct QueryServiceImpl {
    queries: Arc<QueryRegistry>,
}

impl QueryServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new(queries: Arc<QueryRegistry>) -> Self {
        Self { queries }
    }
}

#[tonic::async_trait]
impl QueryService for QueryServiceImpl {
    async fn execute(
        &self,
        _request: Request<ExecuteQueryRequest>,
    ) -> Result<Response<ExecuteQueryResponse>, Status> {
        Err(Status::unimplemented(
            "SQL 执行尚未实现：驱动 SPI 会话接入后可用；当前不返回任何伪造结果",
        ))
    }

    async fn fetch_batch(
        &self,
        request: Request<FetchBatchRequest>,
    ) -> Result<Response<FetchBatchResponse>, Status> {
        let query_id = parse_query_id(&request.into_inner().query_id)?;
        if self.queries.state(&query_id).is_none() {
            return Err(Status::not_found("查询不存在或已结束"));
        }
        Err(Status::unimplemented(
            "结果集分批拉取尚未实现：等待驱动接入",
        ))
    }

    async fn cancel(
        &self,
        request: Request<CancelQueryRequest>,
    ) -> Result<Response<Empty>, Status> {
        let query_id = parse_query_id(&request.into_inner().query_id)?;
        // 幂等：重复取消返回 AlreadyCancelled，不视为错误。
        self.queries.cancel(&query_id).map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }

    async fn close(&self, request: Request<CloseQueryRequest>) -> Result<Response<Empty>, Status> {
        let query_id = parse_query_id(&request.into_inner().query_id)?;
        self.queries.complete(&query_id).map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }
}

fn parse_query_id(raw: &str) -> Result<QueryId, Status> {
    raw.parse()
        .map_err(|_| Status::invalid_argument("查询 ID 格式非法"))
}
