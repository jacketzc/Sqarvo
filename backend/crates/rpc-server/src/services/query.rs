//! 查询域实现。
//!
//! `Execute` 取得会话句柄、登记查询、调用驱动并把结果映射为 Protobuf；
//! `FetchBatch` 按游标继续拉取；`Cancel` 直接中断驱动（幂等）；`Close`
//! 释放游标并从登记表移除。超时由服务端计时并在触发时取消驱动查询。

use std::sync::Arc;
use std::time::Duration;

use sqarvo_common::{Error, ErrorCode, QueryId, SessionId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::ExecuteRequest;
use sqarvo_query_engine::QueryRegistry;
use tonic::{Request, Response, Status};

use crate::error::to_status;
use crate::proto::common::v1::Empty;
use crate::proto::services::v1::{
    CancelQueryRequest, CloseQueryRequest, ExecuteQueryRequest, ExecuteQueryResponse,
    FetchBatchRequest, FetchBatchResponse, query_service_server::QueryService,
};
use crate::services::mapping;

/// `QueryService` 实现。
pub struct QueryServiceImpl {
    sessions: Arc<SessionManager>,
    queries: Arc<QueryRegistry>,
}

impl QueryServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new(sessions: Arc<SessionManager>, queries: Arc<QueryRegistry>) -> Self {
        Self { sessions, queries }
    }
}

#[tonic::async_trait]
impl QueryService for QueryServiceImpl {
    async fn execute(
        &self,
        request: Request<ExecuteQueryRequest>,
    ) -> Result<Response<ExecuteQueryResponse>, Status> {
        let request = request.into_inner();
        if request.sql.trim().is_empty() {
            return Err(Status::invalid_argument("SQL 不能为空"));
        }
        let session_id: SessionId = request
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("会话 ID 格式非法"))?;

        let handle = self
            .sessions
            .acquire(&session_id)
            .await
            .map_err(to_status)?;
        let query_id = self.queries.register(session_id).map_err(to_status)?;

        let max_rows = self
            .queries
            .limits()
            .clamp_batch_rows(Some(request.max_rows_per_batch).filter(|rows| *rows > 0));
        // `auto_commit` 在 v0.1 无意义：SQLite 默认自动提交，事务由 M4 接线。
        let execution = ExecuteRequest {
            query_id,
            sql: request.sql,
            max_rows: Some(max_rows),
            // 超时由服务端统一计时，驱动不需要重复实现。
            timeout: None,
        };

        let outcome = if let Some(limit) = timeout_from_millis(request.timeout_millis) {
            let Ok(result) = tokio::time::timeout(limit, handle.execute(execution)).await else {
                let _ = handle.cancel(&query_id).await;
                let _ = self.queries.complete(&query_id);
                return Err(Status::deadline_exceeded("查询超时，已请求取消"));
            };
            result
        } else {
            handle.execute(execution).await
        };

        let start = match outcome {
            Ok(start) => start,
            Err(error) => {
                let _ = self.queries.complete(&query_id);
                return Err(to_status(error));
            }
        };

        let response = match mapping::execute_response(query_id, &start) {
            Ok(response) => response,
            Err(status) => {
                let _ = handle.close_query(&query_id).await;
                let _ = self.queries.complete(&query_id);
                return Err(status);
            }
        };

        // 没有后续批次时立即释放登记，避免空占并发名额。
        if !start
            .first_batch
            .as_ref()
            .is_some_and(|batch| batch.has_more)
        {
            let _ = self.queries.complete(&query_id);
        }

        Ok(Response::new(response))
    }

    async fn fetch_batch(
        &self,
        request: Request<FetchBatchRequest>,
    ) -> Result<Response<FetchBatchResponse>, Status> {
        let request = request.into_inner();
        let query_id = parse_query_id(&request.query_id)?;
        let session_id = self.queries.session_of(&query_id).ok_or_else(|| {
            // 与其他路径一致：错误必须带稳定错误码，客户端才能分支处理。
            to_status(Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                "查询不存在或已结束",
            ))
        })?;
        let handle = self
            .sessions
            .acquire(&session_id)
            .await
            .map_err(to_status)?;

        let max_rows = self
            .queries
            .limits()
            .clamp_batch_rows(Some(request.max_rows).filter(|rows| *rows > 0));
        let cursor = (!request.page_token.is_empty()).then_some(request.page_token.as_str());

        let batch = match handle
            .fetch(&query_id, request.statement_index, cursor, max_rows)
            .await
        {
            Ok(batch) => batch,
            Err(error) => {
                if error.code() == ErrorCode::QUERY_NOT_FOUND {
                    let _ = self.queries.complete(&query_id);
                }
                return Err(to_status(error));
            }
        };

        let response = mapping::fetch_response(&batch)?;
        if !batch.has_more {
            let _ = self.queries.complete(&query_id);
        }
        Ok(Response::new(response))
    }

    async fn cancel(
        &self,
        request: Request<CancelQueryRequest>,
    ) -> Result<Response<Empty>, Status> {
        let query_id = parse_query_id(&request.into_inner().query_id)?;

        // 幂等：已完成、已取消或不存在的查询都视为「无需再取消」。
        if self.queries.cancel(&query_id).is_err() {
            return Ok(Response::new(Empty {}));
        }

        if let Some(session_id) = self.queries.session_of(&query_id)
            && let Ok(handle) = self.sessions.acquire(&session_id).await
        {
            // 查询可能刚好自行结束：驱动侧取消保持幂等。
            handle.cancel(&query_id).await.map_err(to_status)?;
        }
        Ok(Response::new(Empty {}))
    }

    async fn close(&self, request: Request<CloseQueryRequest>) -> Result<Response<Empty>, Status> {
        let query_id = parse_query_id(&request.into_inner().query_id)?;

        if let Some(session_id) = self.queries.session_of(&query_id)
            && let Ok(handle) = self.sessions.acquire(&session_id).await
        {
            // 会话可能已关闭；此时游标已随会话释放。
            let _ = handle.close_query(&query_id).await;
        }
        // 幂等：未知或已结束的查询同样返回成功。
        let _ = self.queries.complete(&query_id);
        Ok(Response::new(Empty {}))
    }
}

fn timeout_from_millis(millis: u32) -> Option<Duration> {
    (millis > 0).then(|| Duration::from_millis(u64::from(millis)))
}

fn parse_query_id(raw: &str) -> Result<QueryId, Status> {
    raw.parse()
        .map_err(|_| Status::invalid_argument("查询 ID 格式非法"))
}
