//! 元数据浏览辅助逻辑。
//!
//! 元数据必须按层、按页获取，禁止一次性把整库对象树读进内存。
//! 本 crate 提供与数据库无关的分页实现；具体对象检索由驱动实现
//! [`sqarvo_driver_api`] 的元数据能力后接入。
//!
//! v0.2.1 起两个内置驱动都实现了 [`sqarvo_driver_api::MetadataProvider`] 的表/视图
//! 子集：`ListNodes`（当前库的表与视图）、`GetColumns`、`GetPrimaryKey`。
//! `GetDdl` 仍是显式未实现（计划 v0.2.2）。
//!
//! 分页 token 约定见 [`pagination::paginate_by_key`]：不透明字符串，非法 token
//! 一律报错，绝不把错误吞掉后回退到第一页。

#![warn(missing_docs)]

pub mod pagination;

pub use pagination::{Page, PageRequest, paginate, paginate_by_key};
