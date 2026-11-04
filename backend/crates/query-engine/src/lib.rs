//! 查询生命周期管理。
//!
//! 该 crate 负责与具体驱动无关的部分：
//!
//! - 查询登记与并发上限；
//! - 幂等取消（重复取消不报错、不崩溃）；
//! - 批次大小约束（由上层与驱动共同遵守）。
//!
//! SQL 执行本身由驱动会话完成。当前阶段尚未接入任何驱动，因此
//! `QueryService.Execute` 会明确返回未实现，而不是伪造结果。

#![warn(missing_docs)]

pub mod registry;

pub use registry::{CancelOutcome, QueryLimits, QueryRegistry, QueryState};
