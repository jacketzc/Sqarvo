//! MariaDB 驱动（MySQL 协议族）。
//!
//! 设计依据见 ADR 0011：
//!
//! - 使用 `mysql_async` 的文本协议执行，值以字节返回，由本 crate 按列类型解析；
//! - 结果集以行流读取，SPI 的 `fetch(max_rows)` 由驱动累积成批（游标由驱动持有，ADR 0005）；
//! - MySQL 协议没有独立取消通道，取消通过**另一条控制连接**执行 `KILL QUERY <id>`，
//!   取消幂等，被中断的会话连接可以继续复用；
//! - v0.2 只支持单条语句：`multiple_statements = false`，检测到多条语句时明确拒绝，
//!   不把「只执行了第一条」当成成功；
//! - 事务与元数据在 M4 接线，未实现时按 SPI 默认实现返回 `capability.unsupported`。

#![warn(missing_docs)]

mod config;
mod driver;
pub mod error;
mod session;
mod sql;
mod value;

pub use driver::{DRIVER_ID, MariaDbDriver};

#[cfg(test)]
mod tests;
