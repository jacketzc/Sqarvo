//! SQLite 表数据浏览：主键升序 + 键集（keyset）游标（ADR 0017）。
//!
//! 分页 SQL 固定为
//!
//! ```sql
//! SELECT "c1","c2" FROM "t"
//!  WHERE ("pk1","pk2") > (?1,?2)
//!  ORDER BY "pk1" ASC,"pk2" ASC
//!  LIMIT ?3
//! ```
//!
//! 三条约束：
//!
//! - **没有 OFFSET**：游标里是上一页最后一个主键值，翻页代价与页序号无关；
//! - **值一律绑定**：只有标识符（表名、列名）进入 SQL 文本，并且用双引号引用、
//!   内部双引号翻倍；主键值与页大小都是参数；
//! - **无主键只读**：按扫描顺序给一页预览，`ordering = Unstable`、
//!   `read_only_reason = NoPrimaryKey`，并且**不签发游标**——不提供 rowid 近似定位。
//!
//! 行值比较 `(a,b) > (?,?)` 走 SQLite 的行值语义（3.15+）：先比 `a`，相等再比 `b`，
//! 与 `ORDER BY a, b` 完全一致；主键列不允许 NULL，因此不会碰到「NULL 排在末尾」
//! 与比较结果不一致的情况。主键是 TEXT 时按列自身的排序规则（BINARY 或声明的
//! COLLATE）比较，与 `ORDER BY` 用的是同一套规则。

use async_trait::async_trait;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params_from_iter};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{
    Cell, ColumnMeta, DriverSession, MetadataProvider, ObjectRef, PageRequest, PrimaryKey,
    ReadOnlyReason, Row, RowOrder, TableDataProvider, TablePage, Value,
};
use sqarvo_metadata::{decode_table_cursor, encode_table_cursor};

use crate::error::map_sqlite_error;
use crate::metadata;
use crate::session::SqliteSession;

/// 表数据请求。由 [`TableDataProvider`] 构造，交给会话线程执行。
pub(crate) enum TableDataRequest {
    /// 浏览一页。
    Browse {
        object: ObjectRef,
        page: PageRequest,
    },
}

/// 表数据回复，与 [`TableDataRequest`] 一一对应。
pub(crate) enum TableDataReply {
    Page(TablePage),
}

/// 在会话线程上执行表数据请求。
pub(crate) fn handle(connection: &Connection, request: TableDataRequest) -> Result<TableDataReply> {
    match request {
        TableDataRequest::Browse { object, page } => {
            Ok(TableDataReply::Page(browse(connection, &object, &page)?))
        }
    }
}

#[async_trait]
impl TableDataProvider for SqliteSession {
    /// 行编辑由 #47 实现；在此之前声明为不支持，服务端据此拒绝一切写入。
    fn supports_row_edits(&self) -> bool {
        false
    }

    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey> {
        // 与 `MetadataService.GetPrimaryKey` 读同一份 `pragma_table_info`：
        // 两处必须给出相同结果，否则编辑定位与浏览顺序会不一致。
        MetadataProvider::primary_key(self, object).await
    }

    async fn browse(&self, object: &ObjectRef, page: &PageRequest) -> Result<TablePage> {
        metadata::validate_session(self.session_id(), object)?;
        match self
            .table_data_call(TableDataRequest::Browse {
                object: object.clone(),
                page: page.clone(),
            })
            .await?
        {
            TableDataReply::Page(page) => Ok(page),
        }
    }
}

fn browse(connection: &Connection, object: &ObjectRef, page: &PageRequest) -> Result<TablePage> {
    let name = metadata::object_name(object)?;
    // 列元数据复用 #45 的 GetColumns 路径：同一份 `pragma_table_info`、同一个顺序，
    // 因此 `columns` 与每行 cells 的下标严格对应。
    let columns = metadata::columns(connection, object)?;
    let primary_key = metadata::primary_key(connection, object)?;
    let page_size = usize::try_from(page.effective_page_size()).unwrap_or(100);

    if primary_key.is_empty() {
        return browse_without_primary_key(connection, name, columns, page_size);
    }

    let cursor = match page.page_token.as_deref() {
        None | Some("") => None,
        Some(token) => Some(decode_table_cursor(token, object, primary_key.len())?),
    };

    let select = columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let order = primary_key
        .column_names()
        .iter()
        .map(|column| format!("{} ASC", quote_identifier(column)))
        .collect::<Vec<_>>()
        .join(", ");

    let mut params: Vec<SqlValue> = Vec::new();
    let sql = match &cursor {
        None => {
            params.push(SqlValue::Integer(page_limit(page_size)));
            format!(
                "SELECT {select} FROM {} ORDER BY {order} LIMIT ?1",
                quote_identifier(name)
            )
        }
        Some(key) => {
            let placeholders = (1..=key.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let bound = key
                .iter()
                .map(bind_value)
                .collect::<Result<Vec<SqlValue>>>()?;
            params.extend(bound);
            params.push(SqlValue::Integer(page_limit(page_size)));
            let left = primary_key
                .column_names()
                .iter()
                .map(|column| quote_identifier(column))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "SELECT {select} FROM {} WHERE ({left}) > ({placeholders}) ORDER BY {order} LIMIT ?{}",
                quote_identifier(name),
                key.len() + 1
            )
        }
    };

    let read_only = true;
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 表数据查询失败"))?;
    let mut rows = statement
        .query(params_from_iter(params))
        .map_err(|error| map_sqlite_error(error, "执行 SQLite 表数据查询失败"))?;

    // 只多读一行用于判断 `has_more`：不在服务端预取下一页，也不用 COUNT。
    let mut collected: Vec<Row> = Vec::with_capacity(page_size.saturating_add(1));
    while collected.len() <= page_size {
        match rows
            .next()
            .map_err(|error| map_sqlite_error(error, "读取 SQLite 表数据行失败"))?
        {
            Some(row) => collected.push(crate::session::map_row(row, &columns)?),
            None => break,
        }
    }
    let has_more = collected.len() > page_size;
    collected.truncate(page_size);

    let next_page_token = if has_more {
        let last = collected
            .last()
            .ok_or_else(|| Error::internal("表数据游标缺少上一页最后一行"))?;
        Some(encode_table_cursor(
            object,
            &primary_key_values(last, &columns, &primary_key)?,
        )?)
    } else {
        None
    };

    Ok(TablePage {
        columns,
        rows: collected,
        next_page_token,
        has_more,
        read_only,
        read_only_reason: Some(ReadOnlyReason::DriverUnsupported),
        ordering: RowOrder::PrimaryKeyAscending,
    })
}

/// 无主键：只给一页扫描顺序的预览，明确不提供继续语义。
///
/// 不签发游标（`has_more = false`）：无主键就没有稳定的「下一页从这里开始」，
/// 用 rowid 近似定位会在 `VACUUM` / `INSERT OR REPLACE` 后指向别的行（ADR 0017）。
fn browse_without_primary_key(
    connection: &Connection,
    name: &str,
    columns: Vec<ColumnMeta>,
    page_size: usize,
) -> Result<TablePage> {
    let select = columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {select} FROM {} LIMIT ?1", quote_identifier(name));
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| map_sqlite_error(error, "准备 SQLite 表数据查询失败"))?;
    let mut rows = statement
        .query([page_limit(page_size)])
        .map_err(|error| map_sqlite_error(error, "执行 SQLite 表数据查询失败"))?;

    let mut collected: Vec<Row> = Vec::with_capacity(page_size);
    while collected.len() < page_size {
        match rows
            .next()
            .map_err(|error| map_sqlite_error(error, "读取 SQLite 表数据行失败"))?
        {
            Some(row) => collected.push(crate::session::map_row(row, &columns)?),
            None => break,
        }
    }

    Ok(TablePage {
        columns,
        rows: collected,
        next_page_token: None,
        has_more: false,
        read_only: true,
        read_only_reason: Some(ReadOnlyReason::NoPrimaryKey),
        // 扫描顺序不受契约保证：UI 必须展示「无稳定排序」，且禁止据此定位行。
        ordering: RowOrder::Unstable,
    })
}

fn page_limit(page_size: usize) -> i64 {
    i64::try_from(page_size.saturating_add(1)).unwrap_or(i64::MAX)
}

/// 从一行里取出主键列的值（按主键顺序）。
fn primary_key_values(
    row: &Row,
    columns: &[ColumnMeta],
    primary_key: &PrimaryKey,
) -> Result<Vec<Value>> {
    let mut values = Vec::with_capacity(primary_key.len());
    for name in primary_key.column_names() {
        let index = columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::internal("主键列不在浏览结果的列集合中"))?;
        let cell: &Cell = row
            .get(index)
            .ok_or_else(|| Error::internal("浏览结果缺少主键列"))?;
        if cell.truncated {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                "主键值超过单元格上限，无法生成翻页游标；请缩小该列或改用代理主键",
            ));
        }
        values.push(cell.value.clone());
    }
    Ok(values)
}

/// 主键值 → SQLite 绑定参数。只转换游标里可能出现的标量类型。
fn bind_value(value: &Value) -> Result<SqlValue> {
    let bound = match value {
        Value::Null => {
            return Err(Error::internal("主键值不能为 NULL"));
        }
        Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        Value::Int(value) => SqlValue::Integer(*value),
        Value::UInt(value) => {
            let Ok(value) = i64::try_from(*value) else {
                return Err(Error::unsupported(
                    ErrorCode::CAPABILITY_UNSUPPORTED,
                    "SQLite 无法表示超过 i64 范围的无符号主键值",
                ));
            };
            SqlValue::Integer(value)
        }
        Value::Float(value) => SqlValue::Real(*value),
        Value::Text(value) | Value::Json(value) => SqlValue::Text(value.clone()),
        Value::Bytes(value) => SqlValue::Blob(value.clone()),
        Value::Decimal(value) => SqlValue::Text(value.to_string()),
        other => {
            return Err(Error::unsupported(
                ErrorCode::CAPABILITY_UNSUPPORTED,
                format!("SQLite 本版不支持以 {} 类型的主键翻页", other.kind_name()),
            ));
        }
    };
    Ok(bound)
}

/// 方言安全的标识符引用：双引号包裹，内部双引号翻倍。
///
/// 只有对象名与列名走这里；任何**值**都不进入 SQL 文本，一律参数绑定。
fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_quoted_and_escaped() {
        assert_eq!(quote_identifier("orders"), "\"orders\"");
        assert_eq!(quote_identifier("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_identifier("a; DROP TABLE t"), "\"a; DROP TABLE t\"");
    }

    #[test]
    fn limit_reads_one_extra_row_to_detect_more() {
        assert_eq!(page_limit(100), 101);
        assert_eq!(page_limit(0), 1);
    }

    #[test]
    fn null_and_unsigned_overflow_are_rejected() {
        assert!(bind_value(&Value::Null).is_err());
        assert!(bind_value(&Value::UInt(u64::MAX)).is_err());
        assert_eq!(bind_value(&Value::UInt(7)).unwrap(), SqlValue::Integer(7));
    }
}
