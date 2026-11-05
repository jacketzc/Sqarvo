//! MariaDB 表数据浏览：主键升序 + 键集（keyset）游标（ADR 0017）。
//!
//! 分页 SQL 用**展开的字典序比较**（不用行构造器 `(a,b) > (?,?)`）：
//!
//! ```sql
//! SELECT `c1`,`c2` FROM `db`.`t`
//!  WHERE (`pk1` > ?1) OR (`pk1` = ?2 AND `pk2` > ?3)
//!  ORDER BY `pk1` ASC,`pk2` ASC
//!  LIMIT ?4
//! ```
//!
//! 选展开形式而不选行构造器，理由与注意点：
//!
//! - **索引与计划**：展开形式是 `InnoDB` 主键范围扫描的教科书形式，优化器稳定地
//!   使用 `range` 访问；行构造器在部分 MariaDB 版本/统计信息下会退化为全索引扫描，
//!   本版不赌版本差异。
//! - **NULL**：主键列 `NOT NULL` 是定义的一部分，因此不会出现「`NULL` 参与比较得
//!   `NULL`、整行被漏掉」三值逻辑陷阱；`rowid`/ctid 类近似定位同样不使用。
//! - **类型与排序规则**：值走 prepared statement 的二进制协议绑定，`DECIMAL` 以
//!   十进制文本、`DATETIME(6)` 以年月日时分秒微秒绑定，不经过 `f64`；字符串按**列
//!   自身的排序规则**比较，与 `ORDER BY` 用的是同一套规则，所以键集边界与排序一致。
//!   `CHAR` 的 PAD SPACE 语义对两边同样生效，不会造成跳行。
//! - **代价**：展开形式会重复绑定前缀列（n 列主键需要 n(n+1)/2 个参数）。主键列数
//!   在实际 schema 里很小，参数上限（65535）不是约束；换来的是可预测的执行计划。
//!
//! 仍然遵守两条硬约束：**没有 OFFSET**；只有标识符进入 SQL 文本（反引号引用、
//! 内部反引号翻倍），所有值都绑定。

use async_trait::async_trait;
use futures_util::StreamExt;
use mysql_async::prelude::*;
use mysql_async::{Conn, Params, Value as MyValue};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{
    Cell, ColumnMeta, DriverSession, MetadataProvider, ObjectRef, PageRequest, PrimaryKey,
    ReadOnlyReason, Row, RowOrder, TableDataProvider, TablePage, Value,
};
use sqarvo_metadata::{decode_table_cursor, encode_table_cursor};

use crate::error::map_error;
use crate::metadata;
use crate::session::MariaDbSession;
use crate::value::{map_columns, map_row};

/// 表数据请求。由 [`TableDataProvider`] 构造，交给会话任务执行。
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

/// 在会话任务（独占连接）上执行表数据请求。
pub(crate) async fn handle(conn: &mut Conn, request: TableDataRequest) -> Result<TableDataReply> {
    match request {
        TableDataRequest::Browse { object, page } => {
            Ok(TableDataReply::Page(browse(conn, &object, &page).await?))
        }
    }
}

#[async_trait]
impl TableDataProvider for MariaDbSession {
    /// 行编辑由 #47 实现；在此之前声明为不支持，服务端据此拒绝一切写入。
    fn supports_row_edits(&self) -> bool {
        false
    }

    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey> {
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

async fn browse(conn: &mut Conn, object: &ObjectRef, page: &PageRequest) -> Result<TablePage> {
    let database = metadata::current_database(conn).await?;
    let name = metadata::object_parts(object, &database)?;
    // `find_table` 同时给出服务端规范表名（大小写以 `information_schema` 为准），
    // 并用「不存在」表达权限不可见——与 GetColumns 的错误分类保持一致。
    let Some(table) = metadata::find_table(conn, &database, name).await? else {
        return Err(metadata::object_not_found(name));
    };

    // 列元数据复用 #45 的 GetColumns 路径；SELECT 的列顺序与它完全一致。
    let columns = metadata::columns(conn, object).await?;
    let primary_key = metadata::primary_key(conn, object).await?;
    let page_size = usize::try_from(page.effective_page_size()).unwrap_or(100);
    let qualified = format!(
        "{}.{}",
        quote_identifier(&database),
        quote_identifier(&table)
    );
    let select = columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ");

    if primary_key.is_empty() {
        return browse_without_primary_key(conn, &qualified, select, columns, page_size).await;
    }

    let cursor = match page.page_token.as_deref() {
        None | Some("") => None,
        Some(token) => Some(decode_table_cursor(token, object, primary_key.len())?),
    };
    let order = primary_key
        .column_names()
        .iter()
        .map(|column| format!("{} ASC", quote_identifier(column)))
        .collect::<Vec<_>>()
        .join(", ");

    let mut params: Vec<MyValue> = Vec::new();
    let sql = match &cursor {
        None => {
            params.push(MyValue::Int(page_limit(page_size)));
            format!("SELECT {select} FROM {qualified} ORDER BY {order} LIMIT ?")
        }
        Some(key) => {
            let where_clause = keyset_condition(&primary_key, &mut params, key)?;
            params.push(MyValue::Int(page_limit(page_size)));
            format!(
                "SELECT {select} FROM {qualified} WHERE {where_clause} ORDER BY {order} LIMIT ?"
            )
        }
    };

    let mut collected = run_query(
        conn,
        &sql,
        params,
        columns.len(),
        page_size.saturating_add(1),
    )
    .await?;

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
        read_only: true,
        read_only_reason: Some(ReadOnlyReason::DriverUnsupported),
        ordering: RowOrder::PrimaryKeyAscending,
    })
}

/// 执行一次分页查询并收集最多 `collect_limit` 行。
///
/// 有主键时传入 `page_size + 1`：多读的一行只用来判断 `has_more`，随后被截掉；
/// 无主键时传入 `page_size`。两种情况下 `LIMIT` 都已经在 SQL 里兜住行数，
/// 因此内存上限就是「一页行数 × 单格上限」。
async fn run_query(
    conn: &mut Conn,
    sql: &str,
    params: Vec<MyValue>,
    expected_columns: usize,
    collect_limit: usize,
) -> Result<Vec<Row>> {
    let mut result = conn
        .exec_iter(sql, Params::Positional(params))
        .await
        .map_err(|error| map_error(error, "执行 MariaDB 表数据查询失败"))?;
    let protocol_columns = result.columns();
    let Some(protocol_columns) = protocol_columns.filter(|columns| !columns.is_empty()) else {
        return Err(Error::internal("MariaDB 表数据查询没有返回结果集"));
    };
    let (_, plans) = map_columns(&protocol_columns);
    if plans.len() != expected_columns {
        return Err(Error::internal(
            "MariaDB 表数据结果集的列数与列元数据不一致",
        ));
    }

    let mut stream = result
        .stream::<mysql_async::Row>()
        .await
        .map_err(|error| map_error(error, "读取 MariaDB 表数据结果集失败"))?
        .ok_or_else(|| Error::internal("MariaDB 表数据查询没有返回行流"))?;

    let mut collected: Vec<Row> = Vec::with_capacity(collect_limit);
    while collected.len() < collect_limit {
        let Some(row) = stream
            .next()
            .await
            .transpose()
            .map_err(|error| map_error(error, "读取 MariaDB 表数据行失败"))?
        else {
            break;
        };
        collected.push(map_row(&row, &plans));
    }
    // LIMIT 已经兜住行数，这里继续读到结束只是为了把结果集消费干净，
    // 避免下一条命令撞上未读完的游标。
    while stream
        .next()
        .await
        .transpose()
        .map_err(|error| map_error(error, "清空 MariaDB 表数据结果集失败"))?
        .is_some()
    {}

    Ok(collected)
}

/// 无主键：只给一页扫描顺序的预览，明确不提供继续语义（不签发游标）。
async fn browse_without_primary_key(
    conn: &mut Conn,
    qualified: &str,
    select: String,
    columns: Vec<ColumnMeta>,
    page_size: usize,
) -> Result<TablePage> {
    let sql = format!("SELECT {select} FROM {qualified} LIMIT ?");
    let collected = run_query(
        conn,
        &sql,
        vec![MyValue::Int(page_limit(page_size))],
        columns.len(),
        page_size,
    )
    .await?;

    Ok(TablePage {
        columns,
        rows: collected,
        next_page_token: None,
        has_more: false,
        read_only: true,
        read_only_reason: Some(ReadOnlyReason::NoPrimaryKey),
        ordering: RowOrder::Unstable,
    })
}

/// 字典序展开条件：`(p1 > ?) OR (p1 = ? AND p2 > ?) OR ...`。
///
/// 参数按 SQL 里 `?` 出现的顺序推入（前缀列会重复绑定），与占位符严格对应。
fn keyset_condition(
    primary_key: &PrimaryKey,
    params: &mut Vec<MyValue>,
    key: &[Value],
) -> Result<String> {
    if key.len() != primary_key.len() || key.is_empty() {
        return Err(Error::internal("主键值与主键定义不匹配"));
    }
    let names = primary_key.column_names();
    let mut terms = Vec::with_capacity(names.len());
    for index in 0..names.len() {
        let mut parts = Vec::with_capacity(index + 1);
        for prefix_index in 0..index {
            parts.push(format!("{} = ?", quote_identifier(names[prefix_index])));
            params.push(bind_value(&key[prefix_index])?);
        }
        parts.push(format!("{} > ?", quote_identifier(names[index])));
        params.push(bind_value(&key[index])?);
        terms.push(format!("({})", parts.join(" AND ")));
    }
    Ok(terms.join(" OR "))
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

/// 主键值 → MariaDB 绑定参数。
///
/// 走二进制协议：`DECIMAL` 以十进制文本、`DATETIME(6)` 以年月日时分秒微秒绑定，
/// 不经过浮点，也不拼进 SQL 文本。
fn bind_value(value: &Value) -> Result<MyValue> {
    let bound = match value {
        Value::Null => return Err(Error::internal("主键值不能为 NULL")),
        Value::Bool(value) => MyValue::Int(i64::from(u8::from(*value))),
        Value::Int(value) => MyValue::Int(*value),
        Value::UInt(value) => MyValue::UInt(*value),
        Value::Float(value) => MyValue::Double(*value),
        Value::Decimal(value) => MyValue::Bytes(value.to_string().into_bytes()),
        Value::Text(value) | Value::Json(value) => MyValue::Bytes(value.clone().into_bytes()),
        Value::Bytes(value) => MyValue::Bytes(value.clone()),
        Value::Date(value) => MyValue::Date(
            u16::try_from(value.year()).map_err(|_| unsupported_pk("date"))?,
            u8::from(value.month()),
            value.day(),
            0,
            0,
            0,
            0,
        ),
        Value::Time(value) => MyValue::Time(
            false,
            0,
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        Value::DateTime(value) => MyValue::Date(
            u16::try_from(value.year()).map_err(|_| unsupported_pk("datetime"))?,
            u8::from(value.month()),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        // MariaDB 的 `TIMESTAMP` 列在结果集里按墙上时间映射为 `DateTime`；
        // 若上游确实给了带偏移类型，按墙上时间绑定（与驱动其余路径一致）。
        Value::Timestamp(value) => MyValue::Date(
            u16::try_from(value.year()).map_err(|_| unsupported_pk("timestamp"))?,
            u8::from(value.month()),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.nanosecond() / 1_000,
        ),
        other => return Err(unsupported_pk(other.kind_name())),
    };
    Ok(bound)
}

fn unsupported_pk(kind: &str) -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        format!("MariaDB 本版不支持以 {kind} 类型的主键翻页"),
    )
}

/// 方言安全的标识符引用：反引号包裹，内部反引号翻倍。
///
/// 只有对象名与列名走这里；任何**值**都不进入 SQL 文本，一律参数绑定。
fn quote_identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_driver_api::PrimaryKeyColumn;

    fn primary_key(names: &[&str]) -> PrimaryKey {
        PrimaryKey::new(
            names
                .iter()
                .enumerate()
                .map(|(index, name)| PrimaryKeyColumn {
                    name: (*name).to_owned(),
                    position: u32::try_from(index).unwrap() + 1,
                })
                .collect(),
        )
    }

    #[test]
    fn identifiers_are_backtick_quoted_and_escaped() {
        assert_eq!(quote_identifier("orders"), "`orders`");
        assert_eq!(quote_identifier("we`ird"), "`we``ird`");
        assert_eq!(quote_identifier("a; DROP TABLE t"), "`a; DROP TABLE t`");
    }

    #[test]
    fn single_column_key_uses_one_placeholder() {
        let mut params = Vec::new();
        let sql = keyset_condition(&primary_key(&["id"]), &mut params, &[Value::Int(5)]).unwrap();
        assert_eq!(sql, "(`id` > ?)");
        assert_eq!(params, vec![MyValue::Int(5)]);
    }

    #[test]
    fn composite_key_expands_to_lexicographic_terms() {
        let mut params = Vec::new();
        let sql = keyset_condition(
            &primary_key(&["tenant", "id"]),
            &mut params,
            &[Value::Int(1), Value::Int(7)],
        )
        .unwrap();
        assert_eq!(sql, "(`tenant` > ?) OR (`tenant` = ? AND `id` > ?)");
        // 前缀列按占位符顺序重复绑定。
        assert_eq!(
            params,
            vec![MyValue::Int(1), MyValue::Int(1), MyValue::Int(7)]
        );
    }

    #[test]
    fn key_shape_mismatch_is_rejected() {
        let mut params = Vec::new();
        assert!(
            keyset_condition(&primary_key(&["a", "b"]), &mut params, &[Value::Int(1)]).is_err()
        );
    }

    #[test]
    fn datetime_binding_keeps_microseconds_without_float() {
        use time::{Date, Month, PrimitiveDateTime, Time};
        let value = Value::DateTime(PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::November, 5).unwrap(),
            Time::from_hms_micro(1, 2, 3, 456_789).unwrap(),
        ));
        assert_eq!(
            bind_value(&value).unwrap(),
            MyValue::Date(2026, 11, 5, 1, 2, 3, 456_789)
        );
    }

    #[test]
    fn decimal_binds_as_exact_text() {
        let value =
            Value::Decimal(sqarvo_driver_api::Decimal::new("123456".to_owned(), 3).unwrap());
        assert_eq!(
            bind_value(&value).unwrap(),
            MyValue::Bytes(b"123.456".to_vec())
        );
    }
}
