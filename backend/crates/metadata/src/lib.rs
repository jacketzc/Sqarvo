//! 元数据浏览辅助逻辑。
//!
//! 元数据必须按层、按页获取，禁止一次性把整库对象树读进内存。
//! 本 crate 提供与数据库无关的分页实现，具体对象检索由驱动实现
//! [`sqarvo_driver_api`] 的元数据能力后接入。
//!
//! 当前状态：分页逻辑已完成；驱动级元数据查询尚未实现，因此
//! `MetadataService.ListNodes` / `GetColumns` / `GetDdl` 在 gRPC 层返回
//! `UNIMPLEMENTED`，不会返回伪造数据。

#![warn(missing_docs)]

pub mod pagination;

pub use pagination::{Page, PageRequest, paginate};
