//! 事务状态实现。

use sqarvo_common::{Error, ErrorCode, Result, TransactionId};

/// 隔离级别。定义在 Driver SPI 中，事务状态机直接复用，避免两套枚举漂移。
pub use sqarvo_driver_api::IsolationLevel;

/// 事务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    /// 正在进行，可执行语句。
    Active,
    /// 已失败，只能回滚。
    Failed,
}

/// 显式事务句柄。
///
/// `Clone` 只用于「调驱动之前留一份快照」：驱动调用失败时登记要能原样恢复，
/// 不允许通过克隆制造第二个可提交的句柄——结束事务只有 [`Transaction::commit`] /
/// [`Transaction::rollback`] 两条路径，且都会消耗自身。
#[derive(Debug, Clone)]
pub struct Transaction {
    id: TransactionId,
    isolation: IsolationLevel,
    read_only: bool,
    state: TransactionState,
}

impl Transaction {
    /// 开始新事务。调用方必须先确认驱动支持事务。
    ///
    /// 事务 ID 由本函数生成，只适用于「没有驱动参与」的场景；真实会话必须用
    /// [`Transaction::begin_with_id`] 采纳驱动签发的 ID，否则登记与驱动簿记对不上，
    /// 提交时会被当成「不属于当前事务」而幂等丢弃。
    #[must_use]
    pub fn begin(isolation: IsolationLevel, read_only: bool) -> Self {
        Self::begin_with_id(TransactionId::new(), isolation, read_only)
    }

    /// 以指定 ID 开始新事务。
    ///
    /// 事务 ID 的事实来源是驱动（`DriverSession::begin_transaction` 的返回值）：
    /// 通用层不自行生成 ID 再去驱动里「认领」，避免两套 ID 不一致。
    #[must_use]
    pub fn begin_with_id(id: TransactionId, isolation: IsolationLevel, read_only: bool) -> Self {
        Self {
            id,
            isolation,
            read_only,
            state: TransactionState::Active,
        }
    }

    /// 事务 ID。
    #[must_use]
    pub const fn id(&self) -> TransactionId {
        self.id
    }

    /// 隔离级别。
    #[must_use]
    pub const fn isolation(&self) -> IsolationLevel {
        self.isolation
    }

    /// 是否只读。
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// 当前状态。
    #[must_use]
    pub const fn state(&self) -> TransactionState {
        self.state
    }

    /// 标记事务失败（例如驱动报告事务已被数据库中止）。
    pub fn mark_failed(&mut self) {
        self.state = TransactionState::Failed;
    }

    /// 校验是否允许提交；失败态返回 `transaction.failed`。
    ///
    /// 单独暴露给上层：调用驱动之前先用状态机拦截，避免向「已被数据库回滚的事务」
    /// 发一次可能变成隐式提交空事务的 `COMMIT`。
    pub fn ensure_committable(&self) -> Result<()> {
        match self.state {
            TransactionState::Active => Ok(()),
            TransactionState::Failed => Err(Error::failed_precondition(
                ErrorCode::TRANSACTION_FAILED,
                "事务已失败，只能回滚",
            )),
        }
    }

    /// 提交。失败状态下返回 `failed_precondition`。
    pub fn commit(self) -> Result<()> {
        self.ensure_committable()
    }

    /// 回滚。活跃与失败状态都允许。
    pub fn rollback(self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_transaction_can_commit() {
        let transaction = Transaction::begin(IsolationLevel::ReadCommitted, false);
        let id = transaction.id();
        assert_eq!(id.to_string().split('_').next(), Some("txn"));
        assert!(transaction.commit().is_ok());
    }

    #[test]
    fn failed_transaction_cannot_commit() {
        let mut transaction = Transaction::begin(IsolationLevel::Serializable, true);
        transaction.mark_failed();
        let error = transaction.commit().unwrap_err();
        assert_eq!(
            error.category(),
            sqarvo_common::ErrorCategory::FailedPrecondition
        );
    }

    #[test]
    fn failed_transaction_can_rollback() {
        let mut transaction = Transaction::begin(IsolationLevel::Snapshot, false);
        transaction.mark_failed();
        assert_eq!(transaction.state(), TransactionState::Failed);
        assert!(transaction.rollback().is_ok());
    }

    /// 驱动签发的 ID 必须被原样采纳：登记表与驱动簿记用同一个 ID，
    /// 否则提交会被当成「不属于当前事务」而幂等丢弃。
    #[test]
    fn begin_with_id_adopts_driver_id() {
        let id = TransactionId::new();
        let transaction = Transaction::begin_with_id(id, IsolationLevel::Serializable, true);
        assert_eq!(transaction.id(), id);
        assert_eq!(transaction.isolation(), IsolationLevel::Serializable);
        assert!(transaction.is_read_only());
        assert_eq!(transaction.state(), TransactionState::Active);
    }

    #[test]
    fn ensure_committable_rejects_failed_state() {
        let mut transaction = Transaction::begin(IsolationLevel::Serializable, false);
        assert!(transaction.ensure_committable().is_ok());
        transaction.mark_failed();
        let error = transaction
            .ensure_committable()
            .expect_err("失败态必须拒绝提交");
        assert_eq!(error.code(), ErrorCode::TRANSACTION_FAILED);
        assert_eq!(
            error.category(),
            sqarvo_common::ErrorCategory::FailedPrecondition
        );
        // 校验不消耗事务：失败态仍可回滚。
        assert!(transaction.rollback().is_ok());
    }
}
