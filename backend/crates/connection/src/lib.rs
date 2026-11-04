//! 连接与会话管理。
//!
//! 职责边界：本 crate 只管理「用户连接 → 驱动会话」的生命周期与并发计数，
//! 不包含 SQL 执行、结果集分页或数据库协议实现。
//!
//! 并发约定：连接/断开可能并发发生，因此使用 `RwLock` 保护会话表；
//! 但任何情况下都不在持有锁时 `.await`，避免把慢速数据库握手变成全局阻塞。

#![warn(missing_docs)]

pub mod manager;

pub use manager::{SessionManager, SessionSummary};
