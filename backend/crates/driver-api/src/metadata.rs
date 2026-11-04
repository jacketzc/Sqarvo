//! 元数据模型。
//!
//! 不同数据库的层级语义不同：Catalog、Schema、Namespace 不能合并成一个
//! 模糊的字段。这里保留各自的层级，由驱动决定哪些字段有值。

use std::fmt;

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
}
