//! Sqarvo 后端共享的基础类型。
//!
//! 该 crate 只包含与具体数据库、传输方式无关的内容：
//!
//! - [`error`]：可分类、可映射到 gRPC 状态码的错误模型；
//! - [`ids`]：连接、会话、查询、事务的强类型标识符。
//!
//! 这里不依赖任何数据库驱动、异步运行时或 gRPC 实现，以便被所有
//! 后端 crate 与测试复用。

#![warn(missing_docs)]

pub mod error;
pub mod ids;

pub use error::{Error, ErrorCategory, ErrorCode, Result};
pub use ids::{ConnectionId, QueryId, SessionId, TransactionId};
