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
#[derive(Debug)]
pub struct Transaction {
    id: TransactionId,
    isolation: IsolationLevel,
    read_only: bool,
    state: TransactionState,
}

impl Transaction {
    /// 开始新事务。调用方必须先确认驱动支持事务。
    #[must_use]
    pub fn begin(isolation: IsolationLevel, read_only: bool) -> Self {
        Self {
            id: TransactionId::new(),
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

    /// 标记事务失败（例如某条语句执行出错）。
    pub fn mark_failed(&mut self) {
        self.state = TransactionState::Failed;
    }

    /// 提交。失败状态下返回 `failed_precondition`。
    pub fn commit(self) -> Result<()> {
        match self.state {
            TransactionState::Active => Ok(()),
            TransactionState::Failed => Err(Error::failed_precondition(
                ErrorCode::TRANSACTION_FAILED,
                "事务已失败，只能回滚",
            )),
        }
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
}
