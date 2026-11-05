//! MariaDB 元数据：`information_schema`。
//!
//! v0.2.1 只覆盖「当前数据库（catalog）里的表与视图」这一层：
//!
//! - [`MetadataProvider::list_children`] 只接受根节点，返回 `information_schema.TABLES`
//!   里的表与视图（`TABLE_TYPE` 原样保留到 [`ObjectKind`]），按表名排序后分页；
//!   Catalog / Schema / Index / Sequence 等层级显式报 `capability.unsupported`；
//! - [`MetadataProvider::columns`] 用 `information_schema.COLUMNS` 读取原始类型名
//!   （`COLUMN_TYPE`，含长度/无符号）、可空、精度与标度；
//! - [`MetadataProvider::primary_key`] 用 `information_schema.KEY_COLUMN_USAGE`
//!   （`CONSTRAINT_NAME = 'PRIMARY'`），`ORDINAL_POSITION` 就是复合主键内的顺序；
//! - [`MetadataProvider::ddl`] 仍是显式未实现（计划 v0.2.2）。
//!
//! 所有标识符都通过 **绑定参数** 传给服务端（`TABLE_SCHEMA = ? AND TABLE_NAME = ?`），
//! 因此不存在把库名/表名拼进 SQL 或反引号转义的问题。表名大小写按服务端
//! `information_schema` 的比较规则处理：`lower_case_table_names = 0` 时区分大小写，
//! 结果以服务端返回的 `TABLE_NAME` 为准。

use async_trait::async_trait;
use mysql_async::prelude::*;
use mysql_async::{Conn, Row as MyRow};
use sqarvo_common::{Error, ErrorCode, Result, SessionId};
use sqarvo_driver_api::{
    ColumnMeta, ColumnType, Ddl, DriverSession, MetadataProvider, ObjectKind, ObjectNode,
    ObjectRef, Page, PageRequest, PrimaryKey, PrimaryKeyColumn, QualifiedName,
};
use sqarvo_metadata::paginate_by_key;

use crate::error::{NO_DATABASE, OBJECT_NOT_FOUND, map_error};
use crate::session::MariaDbSession;

/// 元数据请求。由 [`MetadataProvider`] 构造，交给会话任务执行。
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

/// 在会话任务（独占连接）上执行元数据请求。
pub(crate) async fn handle(conn: &mut Conn, request: MetadataRequest) -> Result<MetadataReply> {
    match request {
        MetadataRequest::List { parent, page } => Ok(MetadataReply::Nodes(
            list_children(conn, parent.as_ref(), &page).await?,
        )),
        MetadataRequest::Columns(object) => {
            let counts = columns(conn, &object).await?;
            Ok(MetadataReply::Columns(counts))
        }
        MetadataRequest::PrimaryKey(object) => {
            Ok(MetadataReply::PrimaryKey(primary_key(conn, &object).await?))
        }
    }
}

#[async_trait]
impl MetadataProvider for MariaDbSession {
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
        "MariaDB 的 DDL 查看将在 v0.2.2 提供；本版不返回 `SHOW CREATE TABLE` 之外的拼装文本",
    )
}

pub(crate) fn validate_session(session_id: SessionId, object: &ObjectRef) -> Result<()> {
    if object.session_id != session_id {
        return Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "对象引用不属于当前会话",
        ));
    }
    Ok(())
}

/// 当前连接选中的数据库。未选数据库时显式失败：不能把 `NULL` 当成「没有表」。
pub(crate) async fn current_database(conn: &mut Conn) -> Result<String> {
    // 注意用 `Option<String>` 读取：`DATABASE()` 在未选库时是 NULL，
    // 直接取 `String` 会因为 NULL 转换失败而 panic（mysql_common 的 FromValue 语义）。
    let rows: Vec<MyRow> = conn
        .exec("SELECT DATABASE()", ())
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 当前数据库失败"))?;
    let database = rows
        .first()
        .and_then(|row| row.get::<Option<String>, _>(0))
        .flatten();
    database
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            Error::failed_precondition(
                NO_DATABASE,
                "当前连接没有选择数据库，无法列出对象；请先选择数据库再浏览元数据",
            )
        })
}

/// 校验对象引用的层级/种类，并返回（catalog, 对象名）。
///
/// `catalog` 必须等于当前数据库（本版不支持跨库浏览）；MariaDB 没有独立 schema 层。
pub(crate) fn object_parts<'a>(object: &'a ObjectRef, database: &str) -> Result<&'a str> {
    if object.qualified_name.schema.is_some() {
        return Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "MariaDB 没有独立的 schema 层级，该对象引用不属于本驱动",
        ));
    }
    if let Some(catalog) = object.qualified_name.catalog.as_deref()
        && catalog != database
    {
        return Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            format!("本版只支持当前数据库（{database}）内的对象，不支持跨库浏览"),
        ));
    }
    if !matches!(
        object.kind,
        ObjectKind::Table | ObjectKind::View | ObjectKind::Other(_)
    ) {
        return Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            format!(
                "MariaDB 元数据本版只支持表与视图，不支持 {}",
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

async fn list_children(
    conn: &mut Conn,
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
    let database = current_database(conn).await?;
    let rows: Vec<MyRow> = conn
        .exec(
            "SELECT TABLE_NAME, TABLE_TYPE, TABLE_COMMENT FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
            (database.clone(),),
        )
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 表列表失败"))?;

    let mut nodes = Vec::with_capacity(rows.len());
    for row in &rows {
        let name = required_text(row, 0, "TABLE_NAME")?;
        let raw_kind = required_text(row, 1, "TABLE_TYPE")?;
        let kind = match raw_kind.as_str() {
            "BASE TABLE" => ObjectKind::Table,
            "VIEW" | "SYSTEM VIEW" => ObjectKind::View,
            other => ObjectKind::Other(other.to_owned()),
        };
        let comment = optional_text(row, 2).filter(|text| !text.trim().is_empty());
        nodes.push(ObjectNode {
            qualified_name: QualifiedName::new(Some(database.clone()), None, name.clone()),
            kind,
            display_name: name,
            has_children: true,
            comment,
        });
    }

    paginate_by_key(nodes, page, |node| node.qualified_name.name.as_str())
}

pub(crate) async fn columns(conn: &mut Conn, object: &ObjectRef) -> Result<Vec<ColumnMeta>> {
    let database = current_database(conn).await?;
    let name = object_parts(object, &database)?;
    let Some(table) = find_table(conn, &database, name).await? else {
        return Err(object_not_found(name));
    };

    let rows: Vec<MyRow> = conn
        .exec(
            "SELECT COLUMN_NAME, ORDINAL_POSITION, COLUMN_TYPE, DATA_TYPE, IS_NULLABLE, \
                    CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, NUMERIC_SCALE \
             FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
             ORDER BY ORDINAL_POSITION",
            (database.clone(), table.clone()),
        )
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 列信息失败"))?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let column_name = required_text(row, 0, "COLUMN_NAME")?;
        let ordinal = required_u64(row, 1, "ORDINAL_POSITION")?;
        let original_type = required_text(row, 2, "COLUMN_TYPE")?;
        let data_type = required_text(row, 3, "DATA_TYPE")?;
        let nullable = required_text(row, 4, "IS_NULLABLE")?.eq_ignore_ascii_case("YES");
        let length = optional_u64(row, 5);
        let precision = optional_u64(row, 6);
        let scale = optional_i64(row, 7);

        columns.push(ColumnMeta {
            name: column_name,
            // `ORDINAL_POSITION` 从 1 开始，SPI 的 ordinal 与结果集一致按 0 开始。
            ordinal: u32::try_from(ordinal.saturating_sub(1)).unwrap_or(u32::MAX),
            column_type: map_column_type(&data_type, &original_type, length, precision, scale),
            nullable,
            original_type_name: Some(original_type),
        });
    }

    if columns.is_empty() {
        // 已确认表存在却读不到列：通常是权限过滤（information_schema 只展示可见对象）。
        return Err(object_not_found(name));
    }
    Ok(columns)
}

pub(crate) async fn primary_key(conn: &mut Conn, object: &ObjectRef) -> Result<PrimaryKey> {
    let database = current_database(conn).await?;
    let name = object_parts(object, &database)?;
    let Some(table) = find_table(conn, &database, name).await? else {
        return Err(object_not_found(name));
    };

    let rows: Vec<MyRow> = conn
        .exec(
            "SELECT COLUMN_NAME, ORDINAL_POSITION FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY' \
             ORDER BY ORDINAL_POSITION",
            (database, table),
        )
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 主键失败"))?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let column_name = required_text(row, 0, "COLUMN_NAME")?;
        let position = required_u64(row, 1, "ORDINAL_POSITION")?;
        columns.push(PrimaryKeyColumn {
            name: column_name,
            position: u32::try_from(position).unwrap_or(u32::MAX),
        });
    }
    Ok(PrimaryKey::new(columns))
}

/// 查询表/视图是否存在，返回服务端给出的规范表名。
///
/// 权限不足的对象在 `information_schema` 里不可见，因此这里表现为「不存在」：
/// 这是 MySQL 协议族的语义，不额外猜测权限（文档已说明）。
pub(crate) async fn find_table(
    conn: &mut Conn,
    database: &str,
    name: &str,
) -> Result<Option<String>> {
    let row: Option<MyRow> = conn
        .exec_first(
            "SELECT TABLE_NAME FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
            (database, name),
        )
        .await
        .map_err(|error| map_error(error, "查询 MariaDB 对象是否存在失败"))?;
    row.as_ref()
        .map(|row| required_text(row, 0, "TABLE_NAME"))
        .transpose()
}

pub(crate) fn object_not_found(name: &str) -> Error {
    Error::not_found(OBJECT_NOT_FOUND, format!("表或视图 {name} 不存在"))
}

fn required_text(row: &MyRow, index: usize, column: &'static str) -> Result<String> {
    row.get_opt::<String, _>(index)
        .ok_or_else(|| missing_column(column))?
        .map_err(|_| Error::internal(format!("MariaDB 元数据列 {column} 不是预期的文本类型")))
}

fn optional_text(row: &MyRow, index: usize) -> Option<String> {
    row.get::<Option<String>, _>(index).flatten()
}

fn required_u64(row: &MyRow, index: usize, column: &'static str) -> Result<u64> {
    row.get::<Option<u64>, _>(index)
        .flatten()
        .ok_or_else(|| missing_column(column))
}

fn optional_u64(row: &MyRow, index: usize) -> Option<u64> {
    row.get::<Option<u64>, _>(index).flatten()
}

fn optional_i64(row: &MyRow, index: usize) -> Option<i64> {
    row.get::<Option<i64>, _>(index).flatten()
}

/// `information_schema` 的列必须存在；缺列说明查询与服务端版本不匹配。
fn missing_column(column: &'static str) -> Error {
    Error::internal(format!("MariaDB 元数据结果缺少列 {column}"))
}

/// `DATA_TYPE` + `COLUMN_TYPE` → 逻辑类型。
///
/// 与结果集映射（`value.rs::logical_type`）保持一致：
///
/// - `BOOLEAN` 在 MariaDB 里就是 `tinyint(1)`，按 `Integer{8}` 返回，不伪造成布尔；
/// - `TIMESTAMP` 保留墙上时间，声明 `with_time_zone = false`；
/// - `enum` / `set` / `geometry` / `bit` / `year` 等无法无损映射的类型走 `Unsupported`
///   并保留 `COLUMN_TYPE` 原文（含长度），不静默降级成字符串。
fn map_column_type(
    data_type: &str,
    original_type: &str,
    length: Option<u64>,
    precision: Option<u64>,
    scale: Option<i64>,
) -> ColumnType {
    let unsigned = original_type.to_ascii_lowercase().contains("unsigned");
    let bits = match data_type.to_ascii_lowercase().as_str() {
        "tinyint" => 8,
        "smallint" => 16,
        "mediumint" => 24,
        "int" | "integer" => 32,
        "bigint" => 64,
        _ => 0,
    };
    if bits > 0 {
        return ColumnType::Integer { bits, unsigned };
    }
    match data_type.to_ascii_lowercase().as_str() {
        "decimal" | "numeric" => ColumnType::Decimal {
            precision: precision.and_then(|value| u32::try_from(value).ok()),
            scale: scale.and_then(|value| i32::try_from(value).ok()),
        },
        "float" => ColumnType::Float { bits: 32 },
        "double" | "real" => ColumnType::Float { bits: 64 },
        "char" | "varchar" => ColumnType::Text {
            length: length.and_then(|value| u32::try_from(value).ok()),
        },
        "tinytext" | "text" | "mediumtext" | "longtext" => ColumnType::Text { length: None },
        "binary" | "varbinary" => ColumnType::Binary {
            length: length.and_then(|value| u32::try_from(value).ok()),
        },
        "tinyblob" | "blob" | "mediumblob" | "longblob" => ColumnType::Binary { length: None },
        "date" => ColumnType::Date,
        "time" => ColumnType::Time {
            with_time_zone: false,
        },
        "datetime" => ColumnType::DateTime,
        "timestamp" => ColumnType::Timestamp {
            with_time_zone: false,
        },
        "year" => ColumnType::Integer {
            bits: 16,
            unsigned: true,
        },
        "json" => ColumnType::Json,
        _ => ColumnType::Unsupported {
            type_name: original_type.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_and_decimal_keep_shape() {
        assert_eq!(
            map_column_type("bigint", "bigint unsigned", None, Some(20), Some(0)),
            ColumnType::Integer {
                bits: 64,
                unsigned: true
            }
        );
        assert_eq!(
            map_column_type("decimal", "decimal(10,2)", None, Some(10), Some(2)),
            ColumnType::Decimal {
                precision: Some(10),
                scale: Some(2)
            }
        );
        // BOOLEAN 是 tinyint(1) 的别名：按整数返回，不伪造布尔。
        assert_eq!(
            map_column_type("tinyint", "tinyint(1)", None, Some(3), Some(0)),
            ColumnType::Integer {
                bits: 8,
                unsigned: false
            }
        );
    }

    #[test]
    fn text_and_unknown_types_preserve_original_name() {
        assert_eq!(
            map_column_type("varchar", "varchar(64)", Some(64), None, None),
            ColumnType::Text { length: Some(64) }
        );
        assert_eq!(
            map_column_type("longtext", "longtext", Some(4_294_967_295), None, None),
            ColumnType::Text { length: None }
        );
        assert_eq!(
            map_column_type("enum", "enum('a','b')", Some(1), None, None),
            ColumnType::Unsupported {
                type_name: "enum('a','b')".to_owned()
            }
        );
    }
}
