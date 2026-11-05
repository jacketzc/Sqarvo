//! 显式事务状态机。
//!
//! 该 crate 只建模与数据库无关的事务状态与非法状态转换；实际 `BEGIN` /
//! `COMMIT` / `ROLLBACK` 由驱动会话执行。
//!
//! 关键约束：
//!
//! - 事务对象一旦提交或回滚即被消耗，重复提交在编译期就无法表达；
//! - 处于失败状态的事务只允许回滚，不允许提交——这与 PostgreSQL
//!   「事务已中止」的语义一致；
//! - 不支持事务的驱动必须由上层返回 `capability.unsupported`，
//!   不得降级为自动提交。
//!
//! 接线位置（#44）：`sqarvo-connection` 的会话句柄持有本 crate 的
//! [`Transaction`]，每个会话最多一个；事务 ID 来自驱动，状态机负责在
//! 调用驱动之前拦截失败态提交，并由 [ADR 0017 §3] 规定幂等语义
//! （重复提交/回滚返回成功，写入路径才要求事务必须存在）。
//!
//! [ADR 0017 §3]: ../../../docs/decisions/0017-table-data-and-row-edit.md

#![warn(missing_docs)]

pub mod state;

pub use state::{IsolationLevel, Transaction, TransactionState};
