//! SQLite 元数据：`sqlite_master` + `pragma_table_info`。
//!
//! v0.2.1 只覆盖「当前数据库文件里的表与视图」这一层：
//!
//! - [`MetadataProvider::list_children`] 只接受根节点（父节点为空），返回表与视图，
//!   按对象名排序后分页；Catalog / Schema / Index / Sequence 等层级显式报
//!   `capability.unsupported`；
//! - [`MetadataProvider::columns`] 用 `pragma_table_info` 读取声明类型、可空与默认值；
//! - [`MetadataProvider::primary_key`] 用 `pragma_table_info.pk`（主键内顺序）；
//! - [`MetadataProvider::ddl`] 仍是显式未实现（计划 v0.2.2）。
//!
//! 所有查询都在会话的专用线程上执行：`Connection` 归它独占，元数据请求与
//! 查询走同一条命令通道。存在在途结果游标时元数据请求会被拒绝
//! （`sqlite.query_active`），不会与游标交错。

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension};
use sqarvo_common::{Error, ErrorCode, Result, SessionId};
use sqarvo_driver_api::{
    ColumnMeta, ColumnType, Ddl, DriverSession, MetadataProvider, ObjectKind, ObjectNode,
    ObjectRef, Page, PageRequest, PrimaryKey, PrimaryKeyColumn, QualifiedName,
};
use sqarvo_metadata::paginate_by_key;

use crate::error::{OBJECT_NOT_FOUND, map_sqlite_error};
use crate::session::SqliteSession;

/// 元数据请求。由 [`MetadataProvider`] 构造，交给会话线程执行。
pub(crate) enum MetadataRequest {
    /// 列出子节点；`None` 表示根。
    List {
        parent: Option<ObjectRef>,
        page: PageRequest,
    },
    /// 列出列信息。
    Columns(ObjectRef),
    /// 读取主键定义。
    PrimaryKey(ObjectRef),
}

/// 元数据回复，与 [`MetadataRequest`] 一一对应。
pub(crate) enum MetadataReply {
    Nodes(Page<ObjectNode>),
    Columns(Vec<ColumnMeta>),
    PrimaryKey(PrimaryKey),
}

/// 在会话线程上执行元数据请求。
pub(crate) fn handle(connection: &Connection, request: MetadataRequest) -> Result<MetadataReply> {
    match request {
        MetadataRequest::List { parent, page } => Ok(MetadataReply::Nodes(list_children(
            connection,
            parent.as_ref(),
            &page,
        )?)),
        MetadataRequest::Columns(object) => {
            Ok(MetadataReply::Columns(columns(connection, &object)?))
        }
        MetadataRequest::PrimaryKey(object) => {
            Ok(MetadataReply::PrimaryKey(primary_key(connection, &object)?))
        }
    }
}

#[async_trait]
impl MetadataProvider for SqliteSession {
    async fn list_children(
        &self,
        parent: Option<ObjectRef>,
        page: &PageRequest,
    ) -> Result<Page<ObjectNode>> {
        if let Some(parent) = &parent {
            validate_session(self.session_id(), parent)?;
        }
        match self
            .metadata_call(MetadataRequest::List {
                parent,
                page: page.clone(),
            })
            .await?
        {
            MetadataReply::Nodes(page) => Ok(page),
            _ => Err(mismatched_reply()),
        }
    }

    async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnMeta>> {
        validate_session(self.session_id(), object)?;
        match self
            .metadata_call(MetadataRequest::Columns(object.clone()))
            .await?
        {
            MetadataReply::Columns(columns) => Ok(columns),
            _ => Err(mismatched_reply()),
        }
    }

    async fn ddl(&self, _object: &ObjectRef) -> Result<Ddl> {
        Err(ddl_unavailable())
    }

    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey> {
        validate_session(self.session_id(), object)?;
        match self
            .metadata_call(MetadataRequest::PrimaryKey(object.clone()))
            .await?
        {
            MetadataReply::PrimaryKey(primary_key) => Ok(primary_key),
            _ => Err(mismatched_reply()),
        }
    }
}

fn mismatched_reply() -> Error {
    Error::internal("元数据回复与请求不匹配")
}

/// DDL 查看的统一文案（v0.2.2 提供）。
pub(crate) fn ddl_unavailable() -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "SQLite 的 DDL 查看将在 v0.2.2 提供；本版不返回由声明拼装的近似文本",
    )
}

fn validate_session(session_id: SessionId, object: &ObjectRef) -> Result<()> {
    if object.session_id != session_id {
        return Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "对象引用不属于当前会话",
        ));
    }
    Ok(())
}

/// 校验对象层级与种类，返回对象名。
fn object_name(object: &ObjectRef) -> Result<&str> {
    if object.qualified_name.catalog.is_some() || object.qualified_name.schema.is_some() {
        return Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "SQLite 没有 catalog / schema 层级，本版只支持当前数据库文件内的对象",
        ));
    }
    if !matches!(
        object.kind,
        ObjectKind::Table | ObjectKind::View | ObjectKind::Other(_)
    ) {
        return Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            format!(
                "SQLite 元数据本版只支持表与视图，不支持 {}",
                object.kind.as_str()
            ),
        ));
    }
    let name = object.qualified_name.name.trim();
    if name.is_empty() {
        return Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "对象名不能为空",
        ));
    }
    Ok(name)
}

fn list_children(
    connection: &Connection,
    parent: Option<&ObjectRef>,
    page: &PageRequest,
) -> Result<Page<ObjectNode>> {
    if parent.is_some() {
        return Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "本版只支持列出当前数据库的表与视图（根节点）；\
             Catalog / Schema / Index / Sequence 等层级在 v0.2.2 提供",
        ));
    }

    let mut statement = connection
        .prepare(
            "SELECT name, type FROM sqlite_master \
             WHERE type IN ('table','view') AND name NOT GLOB 'sqlite_*' \
             ORDER BY name",
        )
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 对象列表查询失败"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 对象列表失败"))?;

    let mut nodes = Vec::new();
    for row in rows {
        let (name, raw_kind) =
            row.map_err(|error| map_sqlite_error(error, "读取 SQLite 对象行失败"))?;
        let kind = match raw_kind.as_str() {
            "table" => ObjectKind::Table,
            "view" => ObjectKind::View,
            // `sqlite_master` 还会出现 index / trigger，这里已用 WHERE 排除。
            other => ObjectKind::Other(other.to_owned()),
        };
        nodes.push(ObjectNode {
            qualified_name: QualifiedName::new(None, None, name.clone()),
            kind,
            display_name: name,
            // 表与视图都能再展开列；本版通过 `GetColumns` 读取。
            has_children: true,
            comment: None,
        });
    }

    paginate_by_key(nodes, page, |node| node.qualified_name.name.as_str())
}

fn columns(connection: &Connection, object: &ObjectRef) -> Result<Vec<ColumnMeta>> {
    let name = object_name(object)?;
    if !object_exists(connection, name)? {
        return Err(object_not_found(name));
    }

    let mut statement = connection
        .prepare("SELECT cid, name, type, \"notnull\" FROM pragma_table_info(?1) ORDER BY cid")
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 列查询失败"))?;
    let rows = statement
        .query_map([name], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 列信息失败"))?;

    let mut columns = Vec::new();
    for row in rows {
        let (cid, column_name, declared, not_null) =
            row.map_err(|error| map_sqlite_error(error, "读取 SQLite 列行失败"))?;
        let declared = declared.trim();
        columns.push(ColumnMeta {
            name: column_name,
            ordinal: u32::try_from(cid).unwrap_or(u32::MAX),
            column_type: declared_type(declared),
            // SQLite 只记录 `NOT NULL` 声明；这里如实原样返回，不按主键反推。
            nullable: not_null == 0,
            original_type_name: (!declared.is_empty()).then(|| declared.to_owned()),
        });
    }

    if columns.is_empty() {
        // 已确认对象存在，但读不到任何列：说明对象不是可枚举列的表/视图。
        return Err(object_not_found(name));
    }
    Ok(columns)
}

fn primary_key(connection: &Connection, object: &ObjectRef) -> Result<PrimaryKey> {
    let name = object_name(object)?;
    if !object_exists(connection, name)? {
        return Err(object_not_found(name));
    }

    let mut statement = connection
        .prepare("SELECT name, pk FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk")
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 主键查询失败"))?;
    let rows = statement
        .query_map([name], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|error| map_sqlite_error(error, "读取 SQLite 主键失败"))?;

    let mut columns = Vec::new();
    for row in rows {
        let (column_name, position) =
            row.map_err(|error| map_sqlite_error(error, "读取 SQLite 主键行失败"))?;
        columns.push(PrimaryKeyColumn {
            name: column_name,
            // `pragma_table_info.pk` 就是主键内顺序，1 开始。
            position: u32::try_from(position).unwrap_or(u32::MAX),
        });
    }
    Ok(PrimaryKey::new(columns))
}

/// 表或视图是否存在（SQLite 的标识符比较不区分大小写）。
fn object_exists(connection: &Connection, name: &str) -> Result<bool> {
    let found: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM sqlite_master \
             WHERE name = ?1 COLLATE NOCASE AND type IN ('table','view')",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| map_sqlite_error(error, "查询 SQLite 对象失败"))?;
    Ok(found.is_some())
}

fn object_not_found(name: &str) -> Error {
    Error::not_found(OBJECT_NOT_FOUND, format!("表或视图 {name} 不存在"))
}

/// 把声明类型映射为逻辑类型，并保留长度/精度信息。
///
/// 与结果集映射（`session.rs::map_declared_type`）保持一致：SQLite 的类型是
/// 「亲和性 + 声明文本」，`NUMERIC`/`DECIMAL`/日期类可能以任意存储类保存，
/// 因此不伪造 `Decimal`/日期类型，只保留原始声明文本（ADR 0009）。
fn declared_type(declared: &str) -> ColumnType {
    if declared.is_empty() {
        return ColumnType::Binary { length: None };
    }
    let (base, arguments) = split_declared_type(declared);
    let upper = base.to_ascii_uppercase();
    if upper.contains("INT") {
        ColumnType::Integer {
            bits: 64,
            unsigned: false,
        }
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        ColumnType::Text {
            length: arguments.first().copied(),
        }
    } else if upper.contains("BLOB") {
        ColumnType::Binary {
            length: arguments.first().copied(),
        }
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        ColumnType::Float { bits: 64 }
    } else {
        ColumnType::Unsupported {
            type_name: declared.to_owned(),
        }
    }
}

/// 拆出 `VARCHAR(64)` 的基名与整数参数。参数无法解析时只保留基名。
fn split_declared_type(declared: &str) -> (&str, Vec<u32>) {
    let trimmed = declared.trim();
    let Some((base, rest)) = trimmed.split_once('(') else {
        return (trimmed, Vec::new());
    };
    let arguments = rest
        .trim_end_matches(')')
        .split(',')
        .map(str::trim)
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap_or_default();
    (base.trim(), arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_types_keep_shape_without_faking_sqlite_affinity() {
        assert_eq!(
            declared_type("VARCHAR(64)"),
            ColumnType::Text { length: Some(64) }
        );
        assert_eq!(declared_type("BLOB"), ColumnType::Binary { length: None });
        assert_eq!(
            declared_type("INTEGER"),
            ColumnType::Integer {
                bits: 64,
                unsigned: false
            }
        );
        assert_eq!(
            declared_type("DECIMAL(10,2)"),
            ColumnType::Unsupported {
                type_name: "DECIMAL(10,2)".to_owned()
            }
        );
        assert_eq!(declared_type(""), ColumnType::Binary { length: None });
    }

    #[test]
    fn declared_type_arguments_are_parsed_defensively() {
        assert_eq!(split_declared_type("varchar( 32 )").1, vec![32]);
        assert_eq!(split_declared_type("decimal(10,x)").1, Vec::<u32>::new());
        assert_eq!(
            split_declared_type("double precision").0,
            "double precision"
        );
    }
}
