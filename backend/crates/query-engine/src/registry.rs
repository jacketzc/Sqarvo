//! 查询登记表。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use sqarvo_common::{Error, ErrorCode, QueryId, Result, SessionId};

/// 批次与并发限制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryLimits {
    /// 默认每批行数。
    pub default_batch_rows: u32,
    /// 单批行数上限；超出时由服务端压缩到上限。
    pub max_batch_rows: u32,
    /// 同时活跃的查询上限。
    pub max_concurrent: usize,
}

impl Default for QueryLimits {
    fn default() -> Self {
        Self {
            default_batch_rows: 200,
            max_batch_rows: 5_000,
            max_concurrent: 8,
        }
    }
}

impl QueryLimits {
    /// 把调用方请求的行数约束到合法区间。
    #[must_use]
    pub fn clamp_batch_rows(&self, requested: Option<u32>) -> u32 {
        requested
            .unwrap_or(self.default_batch_rows)
            .clamp(1, self.max_batch_rows)
    }
}

/// 查询状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryState {
    /// 执行中。
    Running,
    /// 已请求取消，等待驱动确认。
    Cancelling,
    /// 已取消。
    Cancelled,
}

/// 取消结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// 本次调用触发了取消。
    Cancelled,
    /// 之前已经取消，本次调用是幂等的。
    AlreadyCancelled,
}

struct Entry {
    session_id: SessionId,
    state: QueryState,
    started_at: Instant,
}

/// 活跃查询登记表。
///
/// 使用 `std::sync::Mutex` 而非异步锁：临界区只做内存操作，不会跨 `.await`。
pub struct QueryRegistry {
    limits: QueryLimits,
    entries: Mutex<HashMap<QueryId, Entry>>,
}

impl QueryRegistry {
    /// 构造登记表。
    #[must_use]
    pub fn new(limits: QueryLimits) -> Self {
        Self {
            limits,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 限制参数。
    #[must_use]
    pub const fn limits(&self) -> QueryLimits {
        self.limits
    }

    /// 登记新查询。超过并发上限时返回 `resource_exhausted`。
    pub fn register(&self, session_id: SessionId) -> Result<QueryId> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= self.limits.max_concurrent {
            return Err(Error::resource_exhausted(
                ErrorCode::QUERY_LIMIT_REACHED,
                format!("并发查询已达上限（{}）", self.limits.max_concurrent),
            ));
        }
        let id = QueryId::new();
        entries.insert(
            id,
            Entry {
                session_id,
                state: QueryState::Running,
                started_at: Instant::now(),
            },
        );
        Ok(id)
    }

    /// 请求取消。幂等：重复取消返回 [`CancelOutcome::AlreadyCancelled`]。
    pub fn cancel(&self, id: &QueryId) -> Result<CancelOutcome> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = entries.get_mut(id).ok_or_else(|| {
            Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                format!("查询 {id} 不存在或已结束"),
            )
        })?;
        match entry.state {
            QueryState::Running => {
                entry.state = QueryState::Cancelled;
                Ok(CancelOutcome::Cancelled)
            }
            QueryState::Cancelling | QueryState::Cancelled => Ok(CancelOutcome::AlreadyCancelled),
        }
    }

    /// 结束并移除查询。
    pub fn complete(&self, id: &QueryId) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.remove(id).map(|_| ()).ok_or_else(|| {
            Error::not_found(
                ErrorCode::QUERY_NOT_FOUND,
                format!("查询 {id} 不存在或已结束"),
            )
        })
    }

    /// 会话断开时清理其全部查询，返回移除数量。
    ///
    /// 驱动侧资源由会话关闭负责；这里只清理后端登记，避免悬挂查询。
    pub fn remove_session(&self, session_id: &SessionId) -> usize {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = entries.len();
        entries.retain(|_, entry| entry.session_id != *session_id);
        before - entries.len()
    }

    /// 查询状态。
    #[must_use]
    pub fn state(&self, id: &QueryId) -> Option<QueryState> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .map(|entry| entry.state)
    }

    /// 查询所属会话。
    #[must_use]
    pub fn session_of(&self, id: &QueryId) -> Option<SessionId> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .map(|entry| entry.session_id)
    }

    /// 查询已运行的时长。
    #[must_use]
    pub fn elapsed(&self, id: &QueryId) -> Option<std::time::Duration> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .map(|entry| entry.started_at.elapsed())
    }

    /// 活跃查询数量。
    #[must_use]
    pub fn active(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_rows_are_clamped() {
        let limits = QueryLimits {
            default_batch_rows: 100,
            max_batch_rows: 500,
            max_concurrent: 2,
        };
        assert_eq!(limits.clamp_batch_rows(None), 100);
        assert_eq!(limits.clamp_batch_rows(Some(0)), 1);
        assert_eq!(limits.clamp_batch_rows(Some(50)), 50);
        assert_eq!(limits.clamp_batch_rows(Some(9_999)), 500);
    }

    #[test]
    fn cancel_is_idempotent() {
        let registry = QueryRegistry::new(QueryLimits::default());
        let id = registry.register(SessionId::new()).unwrap();
        assert_eq!(registry.cancel(&id).unwrap(), CancelOutcome::Cancelled);
        assert_eq!(
            registry.cancel(&id).unwrap(),
            CancelOutcome::AlreadyCancelled
        );
        assert_eq!(registry.state(&id), Some(QueryState::Cancelled));
        registry.complete(&id).unwrap();
        assert!(registry.cancel(&id).is_err());
    }

    #[test]
    fn concurrent_limit_is_enforced() {
        let limits = QueryLimits {
            max_concurrent: 1,
            ..QueryLimits::default()
        };
        let registry = QueryRegistry::new(limits);
        let first = registry.register(SessionId::new()).unwrap();
        let error = registry.register(SessionId::new()).unwrap_err();
        assert_eq!(
            error.category(),
            sqarvo_common::ErrorCategory::ResourceExhausted
        );
        registry.complete(&first).unwrap();
        assert!(registry.register(SessionId::new()).is_ok());
    }

    #[test]
    fn remove_session_drops_only_that_session() {
        let registry = QueryRegistry::new(QueryLimits::default());
        let first = SessionId::new();
        let second = SessionId::new();
        let a = registry.register(first).unwrap();
        let b = registry.register(first).unwrap();
        let c = registry.register(second).unwrap();

        assert_eq!(registry.remove_session(&first), 2);
        assert_eq!(registry.active(), 1);
        assert!(registry.state(&a).is_none());
        assert!(registry.state(&b).is_none());
        assert!(registry.state(&c).is_some());
        assert_eq!(registry.remove_session(&first), 0);
    }
}
