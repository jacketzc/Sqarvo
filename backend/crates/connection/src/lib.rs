//! 连接与会话管理。
//!
//! 职责边界：本 crate 只管理「用户连接 → 驱动会话」的生命周期与并发计数，
//! 不包含 SQL 执行、结果集分页或数据库协议实现。
//!
//! 并发约定：连接/断开可能并发发生，因此使用 `RwLock` 保护会话表；
//! 但任何情况下都不在持有锁时 `.await`，避免把慢速数据库握手变成全局阻塞。
//!
//! 事务约定（#44）：每个会话最多一个打开事务，登记状态由 `sqarvo-transaction`
//! 的状态机表达；幂等与失败态语义见 ADR 0017 §3。

#![warn(missing_docs)]

pub mod manager;
pub mod row_changes;

pub use manager::{SessionHandle, SessionManager, SessionSummary, TransactionStatus};
pub use row_changes::{
    MAX_CONCURRENT_ROW_CHANGE_BATCHES, RowChangeBatches, RowChangeCancelOutcome,
};
