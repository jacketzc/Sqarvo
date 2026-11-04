//! 元数据模型。
//!
//! 不同数据库的层级语义不同：Catalog、Schema、Namespace 不能合并成一个
//! 模糊的字段。这里保留各自的层级，由驱动决定哪些字段有值。

use std::fmt;

use async_trait::async_trait;

use sqarvo_common::{Result, SessionId};

use crate::value::ColumnMeta;

/// 完全限定名。缺失的层级为 `None`，例如 MySQL 没有独立的 Schema 层。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualifiedName {
    /// Catalog（例如 MySQL 的 database、PostgreSQL 的 database）。
    pub catalog: Option<String>,
    /// Schema（PostgreSQL schema、Oracle user 等）。
    pub schema: Option<String>,
    /// 对象名。
    pub name: String,
}

impl QualifiedName {
    /// 构造限定名。
    #[must_use]
    pub fn new(catalog: Option<String>, schema: Option<String>, name: impl Into<String>) -> Self {
        Self {
            catalog,
            schema,
            name: name.into(),
        }
    }
}

impl fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = [
            self.catalog.as_deref(),
            self.schema.as_deref(),
            Some(self.name.as_str()),
        ];
        let joined: Vec<&str> = parts.into_iter().flatten().collect();
        f.write_str(&joined.join("."))
    }
}

/// 对象种类。
///
/// `Other` 保留数据库特有对象的原始种类名，避免丢失信息。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    /// Catalog / Database。
    Catalog,
    /// Schema。
    Schema,
    /// 表。
    Table,
    /// 视图。
    View,
    /// 物化视图。
    MaterializedView,
    /// 列。
    Column,
    /// 索引。
    Index,
    /// 序列。
    Sequence,
    /// 函数。
    Function,
    /// 存储过程。
    Procedure,
    /// 触发器。
    Trigger,
    /// 类型。
    Type,
    /// 数据库特有的其他对象。
    Other(String),
}

impl ObjectKind {
    /// 稳定的种类标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Catalog => "catalog",
            Self::Schema => "schema",
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized_view",
            Self::Column => "column",
            Self::Index => "index",
            Self::Sequence => "sequence",
            Self::Function => "function",
            Self::Procedure => "procedure",
            Self::Trigger => "trigger",
            Self::Type => "type",
            Self::Other(name) => name,
        }
    }
}

/// 对象树节点。子节点按需加载，`has_children` 仅表示「可能还有下一层」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectNode {
    /// 完全限定名。
    pub qualified_name: QualifiedName,
    /// 对象种类。
    pub kind: ObjectKind,
    /// 展示名称。
    pub display_name: String,
    /// 是否可能包含子节点。
    pub has_children: bool,
    /// 数据库中的注释原文。
    pub comment: Option<String>,
}

/// 对象引用：会话 + 限定名 + 种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRef {
    /// 所属会话。
    pub session_id: SessionId,
    /// 完全限定名。
    pub qualified_name: QualifiedName,
    /// 对象种类。
    pub kind: ObjectKind,
}

/// 默认页大小。
pub const DEFAULT_PAGE_SIZE: u32 = 100;

/// 单页上限，防止调用方用超大页一次拉全量。
pub const MAX_PAGE_SIZE: u32 = 500;

/// 元数据分页请求。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRequest {
    /// 期望条数；0 表示使用默认值。
    pub page_size: u32,
    /// 上一页返回的 token；`None` 表示第一页。
    pub page_token: Option<String>,
}

impl PageRequest {
    /// 归一化后的页大小。
    #[must_use]
    pub fn effective_page_size(&self) -> u32 {
        if self.page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            self.page_size.min(MAX_PAGE_SIZE)
        }
    }
}

/// 元数据分页结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// 本页数据。
    pub items: Vec<T>,
    /// 下一页 token；`None` 表示没有更多数据。
    pub next_page_token: Option<String>,
    /// 是否因为页大小上限而截断。
    pub truncated: bool,
}

/// DDL 查看结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ddl {
    /// DDL 文本。
    pub text: String,
    /// 是否为数据库返回的原文；`false` 表示由驱动拼装。
    pub exact: bool,
}

/// 元数据能力接口。
///
/// 独立于 [`crate::DriverSession`]，避免不支持元数据的驱动被迫实现空方法。
/// 不支持时由 [`crate::DriverSession::metadata`] 返回 `None`。
#[async_trait]
pub trait MetadataProvider: Send + Sync {
    /// 列出子节点。`parent == None` 表示根节点。
    async fn list_children(
        &self,
        parent: Option<ObjectRef>,
        page: &PageRequest,
    ) -> Result<Page<ObjectNode>>;

    /// 列出列信息。
    async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnMeta>>;

    /// 获取 DDL；数据库不提供时返回 `capability.unsupported`。
    async fn ddl(&self, object: &ObjectRef) -> Result<Ddl>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_name_skips_missing_levels() {
        let name = QualifiedName::new(Some("shop".into()), None, "orders");
        assert_eq!(name.to_string(), "shop.orders");
        let name = QualifiedName::new(Some("shop".into()), Some("public".into()), "orders");
        assert_eq!(name.to_string(), "shop.public.orders");
    }

    #[test]
    fn object_kind_preserves_unknown_kinds() {
        assert_eq!(ObjectKind::Other("package".to_owned()).as_str(), "package");
        assert_eq!(ObjectKind::MaterializedView.as_str(), "materialized_view");
    }

    #[test]
    fn page_size_is_bounded() {
        assert_eq!(
            PageRequest::default().effective_page_size(),
            DEFAULT_PAGE_SIZE
        );
        assert_eq!(
            PageRequest {
                page_size: u32::MAX,
                page_token: None
            }
            .effective_page_size(),
            MAX_PAGE_SIZE
        );
    }
}
