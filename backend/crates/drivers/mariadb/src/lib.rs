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
//! - 显式事务在同一会话连接上执行 `START TRANSACTION` / `COMMIT` / `ROLLBACK`；
//!   事务中会隐式提交的 DDL / `TRUNCATE` 等语句在发送前拒绝，绝不静默提交
//!   （见 `docs/database/mariadb.md`「事务」）；
//! - 元数据（#45、#82、#83）：表/视图/序列列表、列、主键、DDL（`SHOW CREATE ...` 原文）
//!   全部读 `information_schema` / `SHOW CREATE`；索引与触发器没有独立 DDL，明确报
//!   `metadata.object_kind_unsupported`（见 `docs/database/mariadb.md`）。

#![warn(missing_docs)]

mod config;
mod driver;
pub mod error;
mod metadata;
mod session;
mod sql;
mod table_data;
mod value;

pub use driver::{DRIVER_ID, MariaDbDriver};

#[cfg(test)]
mod tests;
