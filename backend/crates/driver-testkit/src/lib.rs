//! 驱动符合性测试基架。
//!
//! 目标：所有驱动（SQLite、PostgreSQL、MySQL）共用同一组不变量检查，
//! 避免每个驱动各写一套测试而逐渐漂移。
//!
//! 当前提供：
//!
//! - [`ScriptedSession`]：脚本化测试会话，用于验证 SPI 行为与上层集成；
//! - [`smoke`]：执行一条 SQL、分批取完、校验批次归属与游标推进的最小检查。
//!
//! 真实驱动接入时，只需在集成测试中构造会话并调用 `smoke`，再补充该驱动
//! 特有的类型往返测试。

#![warn(missing_docs)]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use sqarvo_common::{Error, ErrorCode, QueryId, Result, SessionId};
use sqarvo_driver_api::{DriverSession, ExecuteRequest, ExecutionStart, ResultBatch};

/// 脚本化会话：`execute` 返回预设结果，`fetch` 依次弹出预设批次。
///
/// 仅用于测试与上层集成，不连接任何数据库。
pub struct ScriptedSession {
    session_id: SessionId,
    start: ExecutionStart,
    pending: Mutex<VecDeque<ResultBatch>>,
    last_query: Mutex<Option<QueryId>>,
    cancel_calls: AtomicUsize,
    closed: AtomicBool,
}

impl ScriptedSession {
    /// 构造脚本化会话。
    #[must_use]
    pub fn new(start: ExecutionStart, batches: Vec<ResultBatch>) -> Self {
        Self {
            session_id: SessionId::new(),
            start,
            pending: Mutex::new(batches.into()),
            last_query: Mutex::new(None),
            cancel_calls: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        }
    }

    /// 已收到多少次取消请求。
    #[must_use]
    pub fn cancel_calls(&self) -> usize {
        self.cancel_calls.load(Ordering::SeqCst)
    }

    /// 是否已关闭。
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DriverSession for ScriptedSession {
    fn session_id(&self) -> SessionId {
        self.session_id
    }

    async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart> {
        *self
            .last_query
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request.query_id);
        Ok(self.start.clone())
    }

    async fn fetch(
        &self,
        query_id: &QueryId,
        statement_index: u32,
        _cursor: Option<&str>,
        max_rows: u32,
    ) -> Result<ResultBatch> {
        let last = *self
            .last_query
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last != Some(*query_id) {
            return Err(Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                format!("查询 {query_id} 不存在或已结束"),
            ));
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let batch = pending.pop_front().ok_or_else(|| {
            Error::failed_precondition(ErrorCode::QUERY_NOT_FOUND, "脚本化会话没有更多批次")
        })?;
        if batch.statement_index != statement_index {
            return Err(Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                format!("语句 {statement_index} 不存在"),
            ));
        }
        let limit = usize::try_from(max_rows).unwrap_or(usize::MAX);
        if batch.rows.len() > limit {
            return Err(Error::internal("脚本批次超过请求的行数上限"));
        }
        Ok(batch)
    }

    async fn cancel(&self, _query_id: &QueryId) -> Result<()> {
        self.cancel_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// 最小符合性检查的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmokeReport {
    /// 语句数量。
    pub statements: usize,
    /// 取回的批次数量（含首批）。
    pub batches: usize,
    /// 取回的总行数。
    pub rows: usize,
}

/// 执行一条 SQL、按批取完，并校验 SPI 不变量。
///
/// 校验内容：
///
/// - 至少返回一条语句，首条语句序号为 0、SQL 预览非空；
/// - 首批的 `statement_index` 与语句序号一致；
/// - `has_more == false` 时不再有游标，`has_more == true` 时必须提供游标；
/// - 批次行数不超过请求上限。
///
/// 不校验 SQL 语义，由驱动自己的测试负责。
pub async fn smoke(session: &dyn DriverSession, sql: &str, batch_rows: u32) -> Result<SmokeReport> {
    let query_id = QueryId::new();
    let start = session
        .execute(ExecuteRequest {
            query_id,
            sql: sql.to_owned(),
            max_rows: Some(batch_rows),
            timeout: None,
        })
        .await?;

    let statements = start.statements.len();
    let first = start
        .statements
        .first()
        .ok_or_else(|| Error::internal("执行未返回任何语句摘要"))?;
    if first.index != 0 {
        return Err(Error::internal("首条语句序号必须为 0"));
    }
    if first.sql_preview.trim().is_empty() {
        return Err(Error::internal("语句摘要缺少 SQL 预览"));
    }

    let mut batches = 0usize;
    let mut rows = 0usize;
    let mut cursor = None;
    let mut more = false;

    if let Some(batch) = start.first_batch {
        if batch.statement_index != first.index {
            return Err(Error::internal("首批数据与语句序号不一致"));
        }
        if batch.rows.len() > usize::try_from(batch_rows).unwrap_or(usize::MAX) {
            return Err(Error::internal("首批行数超过请求上限"));
        }
        batches += 1;
        rows += batch.rows.len();
        cursor = batch.next_cursor;
        more = batch.has_more;
    }

    while more {
        let token = cursor
            .clone()
            .ok_or_else(|| Error::internal("has_more=true 但缺少游标"))?;
        let batch = session
            .fetch(&query_id, first.index, Some(&token), batch_rows)
            .await?;
        if batch.statement_index != first.index {
            return Err(Error::internal("批次数据与语句序号不一致"));
        }
        if batch.rows.len() > usize::try_from(batch_rows).unwrap_or(usize::MAX) {
            return Err(Error::internal("批次行数超过请求上限"));
        }
        batches += 1;
        rows += batch.rows.len();
        cursor = batch.next_cursor;
        more = batch.has_more;
    }

    Ok(SmokeReport {
        statements,
        batches,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use sqarvo_driver_api::{Cell, ColumnMeta, ColumnType, Row, StatementSummary, Value};

    use super::*;

    fn column() -> ColumnMeta {
        ColumnMeta {
            name: "value".to_owned(),
            ordinal: 0,
            column_type: ColumnType::Integer {
                bits: 64,
                unsigned: false,
            },
            nullable: true,
            original_type_name: Some("INTEGER".to_owned()),
        }
    }

    fn row(value: i64) -> Row {
        vec![Cell::new(Value::Int(value))]
    }

    fn start_with(rows: usize) -> ExecutionStart {
        let batch = ResultBatch {
            statement_index: 0,
            rows: (0..rows)
                .map(|index| row(i64::try_from(index).unwrap()))
                .collect(),
            next_cursor: Some("next".to_owned()),
            has_more: true,
        };
        ExecutionStart::single(
            StatementSummary {
                index: 0,
                sql_preview: "SELECT value FROM t".to_owned(),
                columns: vec![column()],
                rows_affected: None,
                has_result_set: true,
            },
            Some(batch),
        )
    }

    #[tokio::test]
    async fn smoke_drains_all_batches() {
        let session = ScriptedSession::new(
            start_with(2),
            vec![ResultBatch {
                statement_index: 0,
                rows: vec![row(2), row(3)],
                next_cursor: None,
                has_more: false,
            }],
        );

        let report = smoke(&session, "SELECT value FROM t", 2).await.unwrap();
        assert_eq!(report.statements, 1);
        assert_eq!(report.batches, 2);
        assert_eq!(report.rows, 4);
    }

    #[tokio::test]
    async fn missing_cursor_is_rejected() {
        let mut start = start_with(1);
        if let Some(batch) = start.first_batch.as_mut() {
            batch.next_cursor = None;
        }
        let session = ScriptedSession::new(start, Vec::new());
        let error = smoke(&session, "SELECT 1", 10).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::INTERNAL);
    }

    #[tokio::test]
    async fn cancel_is_recorded_and_close_marks_session() {
        let session = ScriptedSession::new(start_with(0), Vec::new());
        let query_id = QueryId::new();
        session.cancel(&query_id).await.unwrap();
        session.cancel(&query_id).await.unwrap();
        assert_eq!(session.cancel_calls(), 2);
        assert!(!session.is_closed());
        session.close().await.unwrap();
        assert!(session.is_closed());
    }

    #[tokio::test]
    async fn batches_are_scoped_to_statement_index() {
        let first = StatementSummary {
            index: 0,
            sql_preview: "SELECT 1".to_owned(),
            columns: vec![column()],
            rows_affected: None,
            has_result_set: true,
        };
        let second = StatementSummary {
            index: 1,
            sql_preview: "SELECT 2".to_owned(),
            columns: vec![column()],
            rows_affected: None,
            has_result_set: true,
        };
        let start = ExecutionStart {
            statements: vec![first, second],
            first_batch: Some(ResultBatch {
                statement_index: 0,
                rows: vec![row(1)],
                next_cursor: None,
                has_more: false,
            }),
        };
        let session = ScriptedSession::new(
            start,
            vec![ResultBatch {
                statement_index: 1,
                rows: vec![row(2)],
                next_cursor: None,
                has_more: false,
            }],
        );

        let query_id = QueryId::new();
        session
            .execute(ExecuteRequest {
                query_id,
                sql: "SELECT 1; SELECT 2".to_owned(),
                max_rows: None,
                timeout: None,
            })
            .await
            .unwrap();

        let batch = session.fetch(&query_id, 1, None, 10).await.unwrap();
        assert_eq!(batch.statement_index, 1);
        assert_eq!(batch.rows.len(), 1);

        let error = session.fetch(&query_id, 1, None, 10).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::QUERY_NOT_FOUND);
    }
}
