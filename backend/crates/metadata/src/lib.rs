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
//!
//! v0.2.2（#82）起两个内置驱动都用 [`hierarchy::paginate_children`] 给每一层的
//! 子节点分页：排序、键唯一化与「父节点作用域」都收在一处，两个驱动不会各写一套。
//!
//! v0.2.2（#84）起过滤也收在这里：[`filter`] 提供 `LIKE` 的转义与两套引擎的条件片段，
//! [`hierarchy::paginate_children_filtered`] 把过滤条件钉进分页 token（`v2:` 版本），
//! 换条件后旧 token 以 `config.invalid` 失效，而不是继续按旧偏移量翻页。

#![warn(missing_docs)]

pub mod filter;
pub mod hierarchy;
pub mod pagination;
pub mod table_cursor;

pub use filter::{LIKE_ESCAPE_CHAR, like_escape, mariadb_name_condition, sqlite_name_condition};
pub use hierarchy::{
    child_key, kind_order, paginate_children, paginate_children_filtered, sort_children,
};
pub use pagination::{
    Page, PageRequest, filter_tag, paginate, paginate_by_key, paginate_by_key_filtered,
};
pub use table_cursor::{decode as decode_table_cursor, encode as encode_table_cursor};
