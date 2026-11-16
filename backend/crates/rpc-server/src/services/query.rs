//! 查询域实现。
//!
//! `Execute` 取得会话句柄、登记查询、调用驱动并把结果映射为 Protobuf；
//! `FetchBatch` 按游标继续拉取；`Cancel` 直接中断驱动（幂等）；`Close`
//! 释放游标并从登记表移除。超时由服务端计时并在触发时取消驱动查询。
//!
//! 多语句脚本（#187）：一个查询可能携带多个结果集，`first_batch` 只交付第一个
//! 结果集的首批。查询登记必须等到**所有**结果集都交付完毕才释放——单条语句
//! `has_more = false` 不代表查询结束。`FetchBatchResponse.complete` 同样表示
//! 整个查询交付完毕，而非单条语句读完。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;

use sqarvo_common::{Error, ErrorCode, QueryId, SessionId};
use sqarvo_connection::SessionManager;
use sqarvo_driver_api::{ExecuteRequest, ExecutionStart, ResultBatch};
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
    /// 尚未交付完毕的结果集语句序号（按查询，#187）。
    ///
    /// 单语句查询同样经过这张表：首批即读完时不登记、立即释放，行为与
    /// v0.3 完全一致。会话断开时 `QueryRegistry` 会清理登记，这里在下次
    /// 登记时顺带回收悬挂条目（[`Self::track_execution`]）。
    pending: StdMutex<HashMap<QueryId, HashSet<u32>>>,
}

impl QueryServiceImpl {
    /// 构造服务。
    #[must_use]
    pub fn new(sessions: Arc<SessionManager>, queries: Arc<QueryRegistry>) -> Self {
        Self {
            sessions,
            queries,
            pending: StdMutex::new(HashMap::new()),
        }
    }

    /// 登记一次执行的待交付结果集；全部交付完毕（含全部是写语句）时立即释放。
    fn track_execution(&self, query_id: QueryId, start: &ExecutionStart) {
        let mut pending: HashSet<u32> = start
            .statements
            .iter()
            .filter(|statement| statement.has_result_set)
            .map(|statement| statement.index)
            .collect();
        if let Some(batch) = &start.first_batch
            && !batch.has_more
        {
            pending.remove(&batch.statement_index);
        }
        let mut table = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        // 会话可能已断开（registry 已清理）：顺带回收悬挂条目，避免泄漏。
        table.retain(|id, _| self.queries.session_of(id).is_some());
        if pending.is_empty() {
            drop(table);
            let _ = self.queries.complete(&query_id);
        } else {
            table.insert(query_id, pending);
        }
    }

    /// 记录一批交付。返回整个查询是否已交付完毕（`has_more` 时必为 `false`）。
    fn note_final_batch(&self, query_id: &QueryId, batch: &ResultBatch) -> bool {
        if batch.has_more {
            return false;
        }
        let mut table = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let done = match table.get_mut(query_id) {
            Some(set) => {
                set.remove(&batch.statement_index);
                set.is_empty()
            }
            // 无登记（已释放或单语句旧路径）：最后一批即完成，保持既有行为。
            None => true,
        };
        if done {
            table.remove(query_id);
        }
        done
    }

    /// 移除待交付登记（查询结束/关闭时）。
    fn forget(&self, query_id: &QueryId) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(query_id);
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
        // ADR 0030：空串 = 不改变会话当前库（与 v0.3 之前完全一致）。
        let catalog = request.catalog.trim().to_owned();
        let catalog = (!catalog.is_empty()).then_some(catalog);
        // 能力声明先于调用：驱动没声明切库就不发 `USE`，直接告诉客户端不支持。
        if catalog.is_some() && !handle.supports_catalog_switch() {
            let _ = self.queries.complete(&query_id);
            return Err(to_status(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "当前驱动不支持「切换库」，请在 SQL 里写限定名",
            )));
        }
        // `auto_commit` 在 v0.1 无意义：SQLite 默认自动提交，事务由 M4 接线。
        let execution = ExecuteRequest {
            query_id,
            sql: request.sql,
            max_rows: Some(max_rows),
            // 超时由服务端统一计时，驱动不需要重复实现。
            timeout: None,
        };

        let outcome = if let Some(limit) = timeout_from_millis(request.timeout_millis) {
            let Ok(result) = tokio::time::timeout(
                limit,
                handle.execute_in_catalog(execution, catalog.as_deref()),
            )
            .await
            else {
                let _ = handle.cancel(&query_id).await;
                let _ = self.queries.complete(&query_id);
                return Err(Status::deadline_exceeded("查询超时，已请求取消"));
            };
            result
        } else {
            handle
                .execute_in_catalog(execution, catalog.as_deref())
                .await
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

        // 所有结果集都交付完毕时立即释放登记，避免空占并发名额；
        // 多语句脚本还有后续结果集时保持登记（#187）。
        self.track_execution(query_id, &start);

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
                    self.forget(&query_id);
                    let _ = self.queries.complete(&query_id);
                }
                return Err(to_status(error));
            }
        };

        let mut response = mapping::fetch_response(&batch)?;
        // `complete` 表示整个查询交付完毕：多语句脚本里单条语句读完
        // 不代表查询结束（#187）。单语句行为不变：最后一批即 complete。
        let query_done = self.note_final_batch(&query_id, &batch);
        response.complete = query_done;
        if query_done {
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
        self.forget(&query_id);
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
