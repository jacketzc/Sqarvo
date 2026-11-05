//! 在途行变更批次的取消登记表（ADR 0018）。
//!
//! `ApplyRowChanges` 在一批（≤ 500 行）执行期间把 `query_id` 与取消令牌登记在这里，
//! `CancelRowChanges` 通过它找到令牌并置位。**这张表刻意与会话执行锁正交**：
//!
//! - 登记表用标准库互斥锁保护，临界区只有「插入 / 查找 / 移除一个 `HashMap` 条目」，
//!   不做 I/O、不 `.await`；
//! - 取消路径只做「取令牌 → 原子写一次」，**不 acquire 会话**、不取会话执行锁。
//!   否则「取消一个正在跑的批次」会排在批次后面等它跑完，等于没取消（ADR 0008）。
//!
//! 令牌本身是 [`RowChangeCancel`]（`Arc<AtomicBool>`），先登记再执行，因此取消请求
//! 与批次执行之间不存在「检查了但还没登记」的窗口。

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use sqarvo_common::{Error, ErrorCode, QueryId, Result, SessionId};
use sqarvo_driver_api::RowChangeCancel;

/// 同时登记的在途批次上限。
///
/// 同一会话最多只有一批在途（执行锁串行化），因此这个上限实际约束的是「跨会话并发写」。
/// 取 64：远高于单机桌面客户端的正常并发，又能在异常客户端刷批次时兜底
/// （AGENTS.md §10：并发必须有上限）。
pub const MAX_CONCURRENT_ROW_CHANGE_BATCHES: usize = 64;

/// 取消的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowChangeCancelOutcome {
    /// 本次调用首次置位，批次会在下一个行边界停下。
    Cancelled,
    /// 之前已取消；本次调用是幂等的。
    AlreadyCancelled,
}

struct Entry {
    session_id: SessionId,
    /// `None` 表示驱动未实现取消：`CancelRowChanges` 必须明确报
    /// `capability.unsupported`，不能假装取消成功。
    cancel: Option<RowChangeCancel>,
}

/// 在途行变更批次登记表。
pub struct RowChangeBatches {
    max_concurrent: usize,
    entries: Mutex<HashMap<QueryId, Entry>>,
}

impl RowChangeBatches {
    /// 以给定的并发上限构造。
    #[must_use]
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            max_concurrent,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 用默认上限构造（[`MAX_CONCURRENT_ROW_CHANGE_BATCHES`]）。
    #[must_use]
    pub fn with_default_limit() -> Self {
        Self::new(MAX_CONCURRENT_ROW_CHANGE_BATCHES)
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<QueryId, Entry>> {
        // 毒化只可能来自持锁期间 panic；登记表本身仍是一致的 `HashMap`，按 inner 继续。
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 登记一个在途批次。
    ///
    /// `cancel` 为 `None` 表示驱动未实现取消（该批次不可取消，但会把「不可取消」
    /// 如实告诉 `CancelRowChanges`）。
    ///
    /// # Errors
    ///
    /// - 同一 `query_id` 已在执行：`query.id_in_use`（否则取消会打到别人的批次上）；
    /// - 在途批次已达上限：`query.limit_reached`。
    pub fn register(
        &self,
        id: QueryId,
        session_id: SessionId,
        cancel: Option<RowChangeCancel>,
    ) -> Result<()> {
        let mut entries = self.entries();
        if entries.contains_key(&id) {
            return Err(Error::already_exists(
                ErrorCode::QUERY_ID_IN_USE,
                "该 query_id 已有在途的行变更批次，请换一个 ID",
            ));
        }
        if entries.len() >= self.max_concurrent {
            return Err(Error::resource_exhausted(
                ErrorCode::QUERY_LIMIT_REACHED,
                format!(
                    "在途行变更批次已达上限（{}），请等待已有批次结束后重试",
                    self.max_concurrent
                ),
            ));
        }
        entries.insert(id, Entry { session_id, cancel });
        Ok(())
    }

    /// 批次结束时移除登记。幂等：未知 ID 视为已结束。
    pub fn finish(&self, id: &QueryId) {
        self.entries().remove(id);
    }

    /// 请求取消。幂等：重复取消返回 [`RowChangeCancelOutcome::AlreadyCancelled`]。
    ///
    /// 本方法只做一次原子写，不 `.await`、不取会话执行锁——这是「取消正在跑的批次」
    /// 不会死锁的全部原因。
    ///
    /// # Errors
    ///
    /// - 批次未登记（未开始 / 已结束 / 不存在）：`query.not_found`。调用方按幂等处理；
    /// - 驱动未实现取消：`capability.unsupported`（绝不返回成功）。
    pub fn cancel(&self, id: &QueryId) -> Result<RowChangeCancelOutcome> {
        let token = {
            let entries = self.entries();
            let entry = entries.get(id).ok_or_else(|| {
                Error::not_found(
                    ErrorCode::QUERY_NOT_FOUND,
                    format!("行变更批次 {id} 不存在或已结束"),
                )
            })?;
            entry.cancel.clone()
        };
        let Some(token) = token else {
            return Err(sqarvo_driver_api::unsupported_row_change_cancellation());
        };
        // 令牌拷贝出临界区再置位：置位是原子的，不需要持锁。
        Ok(if token.cancel() {
            RowChangeCancelOutcome::Cancelled
        } else {
            RowChangeCancelOutcome::AlreadyCancelled
        })
    }

    /// 会话断开时清理它的在途批次（并请求取消，避免无谓地继续跑完整批）。
    ///
    /// 返回移除的数量。
    pub fn remove_session(&self, session_id: &SessionId) -> usize {
        let mut entries = self.entries();
        let before = entries.len();
        let removed: Vec<QueryId> = entries
            .iter()
            .filter(|(_, entry)| entry.session_id == *session_id)
            .map(|(id, _)| *id)
            .collect();
        for id in &removed {
            if let Some(entry) = entries.remove(id)
                && let Some(cancel) = entry.cancel
            {
                // 会话已断：让驱动在下一个行边界停下，别把整批白跑完。
                let _ = cancel.cancel();
            }
        }
        before - entries.len()
    }

    /// 在途批次数量。
    #[must_use]
    pub fn active(&self) -> usize {
        self.entries().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register(batches: &RowChangeBatches, session: SessionId) -> (QueryId, RowChangeCancel) {
        let id = QueryId::new();
        let token = RowChangeCancel::new();
        batches
            .register(id, session, Some(token.clone()))
            .expect("登记批次");
        (id, token)
    }

    #[test]
    fn cancel_sets_token_and_is_idempotent() {
        let batches = RowChangeBatches::with_default_limit();
        let session = SessionId::new();
        let (id, token) = register(&batches, session);

        assert_eq!(
            batches.cancel(&id).expect("首次取消"),
            RowChangeCancelOutcome::Cancelled
        );
        assert!(token.is_cancelled(), "取消必须落到驱动读的那枚令牌上");
        assert_eq!(
            batches.cancel(&id).expect("重复取消"),
            RowChangeCancelOutcome::AlreadyCancelled
        );

        batches.finish(&id);
        assert_eq!(batches.active(), 0);
        // 已结束的批次：调用方按幂等处理（不是 panic，也不是「还在取消」）。
        assert_eq!(
            batches.cancel(&id).expect_err("已结束").code(),
            ErrorCode::QUERY_NOT_FOUND
        );
        batches.finish(&id);
    }

    #[test]
    fn duplicate_id_is_rejected_so_cancel_cannot_hit_the_wrong_batch() {
        let batches = RowChangeBatches::with_default_limit();
        let (id, _token) = register(&batches, SessionId::new());
        let error = batches
            .register(id, SessionId::new(), Some(RowChangeCancel::new()))
            .expect_err("重复 ID 必须拒绝");
        assert_eq!(error.code(), ErrorCode::QUERY_ID_IN_USE);
        assert_eq!(batches.active(), 1);
    }

    #[test]
    fn unsupported_driver_reports_unsupported_instead_of_silent_success() {
        let batches = RowChangeBatches::new(4);
        let id = QueryId::new();
        batches
            .register(id, SessionId::new(), None)
            .expect("登记不可取消的批次");
        let error = batches.cancel(&id).expect_err("不可取消必须报错");
        assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
        assert_eq!(
            error.category(),
            sqarvo_common::ErrorCategory::Unsupported,
            "静默成功会让用户以为批次已停，其实还在写"
        );
    }

    #[test]
    fn session_cleanup_removes_and_cancels_only_its_batches() {
        let batches = RowChangeBatches::new(4);
        let first = SessionId::new();
        let second = SessionId::new();
        let (a, token_a) = register(&batches, first);
        let (b, _token_b) = register(&batches, first);
        let (c, token_c) = register(&batches, second);

        assert_eq!(batches.remove_session(&first), 2);
        assert!(token_a.is_cancelled(), "断连的会话不再白跑整批");
        assert!(!token_c.is_cancelled(), "别的会话不受影响");
        assert_eq!(batches.active(), 1);
        assert_eq!(
            batches.cancel(&c).expect("另一个会话仍可取消"),
            RowChangeCancelOutcome::Cancelled
        );
        assert_eq!(
            batches.cancel(&a).expect_err("已清理").code(),
            ErrorCode::QUERY_NOT_FOUND
        );
        assert_eq!(
            batches.cancel(&b).expect_err("已清理").code(),
            ErrorCode::QUERY_NOT_FOUND
        );
    }

    #[test]
    fn concurrency_limit_is_enforced() {
        let batches = RowChangeBatches::new(1);
        let (_id, _token) = register(&batches, SessionId::new());
        let error = batches
            .register(
                QueryId::new(),
                SessionId::new(),
                Some(RowChangeCancel::new()),
            )
            .expect_err("超限必须拒绝");
        assert_eq!(error.code(), ErrorCode::QUERY_LIMIT_REACHED);
    }
}
