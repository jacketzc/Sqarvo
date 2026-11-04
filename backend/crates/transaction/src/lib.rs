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

#![warn(missing_docs)]

pub mod state;

pub use state::{IsolationLevel, Transaction, TransactionState};
