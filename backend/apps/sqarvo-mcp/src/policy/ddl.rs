//! Input validation for the structured DDL tools.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Map, Value};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::ddl::{Action, Change, Column, ColumnType, Literal};
use uuid::Uuid;

const MAX_INPUT: usize = 32 * 1024;
const MAX_COLUMNS: usize = 64;
const MAX_KEYS: usize = 16;

#[derive(Debug)]
pub(crate) struct Request {
    pub alias: String,
    pub id: Option<Uuid>,
    pub change: Change,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    CreateTable {
        columns: Vec<ColumnInput>,
        #[serde(default)]
        primary_key: Vec<String>,
        #[serde(default)]
        unique_constraints: Vec<Vec<String>>,
        #[serde(default)]
        foreign_keys: Vec<Value>,
        #[serde(default)]
        if_not_exists: bool,
    },
    AddColumn {
        column: ColumnInput,
    },
    DropColumn {
        column_name: String,
    },
    RenameColumn {
        column_name: String,
        new_name: String,
    },
    RenameTable {
        new_table: String,
    },
    DropTable {
        #[serde(default)]
        if_exists: bool,
    },
    CreateIndex {
        index: String,
        columns: Vec<String>,
        #[serde(default)]
        unique: bool,
    },
    DropIndex {
        index: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ColumnInput {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    length: Option<u16>,
    precision: Option<u8>,
    scale: Option<u8>,
    nullable: Option<bool>,
    #[serde(default, deserialize_with = "constant")]
    default: Option<Value>,
    #[serde(default)]
    primary_key: bool,
    #[serde(default)]
    unique: bool,
    #[serde(default, alias = "identity")]
    auto_increment: bool,
}

fn constant<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(input).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    operation: String,
    arguments: Map<String, Value>,
}

pub(crate) fn decode(tool: &str, arguments: Map<String, Value>) -> Result<Request> {
    if serde_json::to_vec(&arguments).map_or(true, |json| json.len() > MAX_INPUT) {
        return Err(invalid("DDL 参数超过大小限制"));
    }
    let (tool, mut arguments, preview) = if tool == "preview_schema_change" {
        let input: Preview = serde_json::from_value(Value::Object(arguments))
            .map_err(|_| invalid("DDL 预览参数无效"))?;
        (input.operation, input.arguments, true)
    } else {
        (tool.to_owned(), arguments, false)
    };
    let id = if preview {
        if arguments.contains_key("operation_id") {
            return Err(invalid("预览不接受 operation_id"));
        }
        None
    } else {
        Some(
            take_string(&mut arguments, "operation_id")?
                .parse::<Uuid>()
                .ok()
                .filter(|id| {
                    id.get_version_num() == 7 && id.get_variant() == uuid::Variant::RFC4122
                })
                .ok_or_else(|| invalid("operation_id 必须是 UUIDv7"))?,
        )
    };
    let alias = take_string(&mut arguments, "connection_alias")?;
    if alias.trim() != alias || alias.len() > 256 || alias.chars().any(char::is_control) {
        return Err(invalid("connection_alias 无效"));
    }
    let database = take_string(&mut arguments, "database")?;
    let table = take_string(&mut arguments, "table")?;
    if arguments.get("new_table").and_then(Value::as_str) == Some(table.as_str()) {
        return Err(invalid("DDL 重命名要求新名称不同"));
    }
    name(&database)?;
    name(&table)?;
    match tool.as_str() {
        "alter_table" => {
            let action = arguments.get("action").and_then(Value::as_str);
            if !matches!(
                action,
                Some("add_column" | "drop_column" | "rename_column" | "rename_table")
            ) {
                return Err(unsupported("不支持该 ALTER TABLE 动作"));
            }
        }
        "create_table" | "drop_table" | "create_index" | "drop_index" => {
            if arguments
                .insert("action".into(), Value::String(tool))
                .is_some()
            {
                return Err(invalid("DDL 参数含多余的 action 字段"));
            }
        }
        _ => return Err(unsupported("不支持该 DDL 工具")),
    }
    let input = serde_json::from_value(Value::Object(arguments))
        .map_err(|_| invalid("DDL 参数不符合结构化契约"))?;
    Ok(Request {
        alias,
        id,
        change: Change {
            database,
            table,
            action: normalize(input)?,
        },
    })
}

fn normalize(input: Input) -> Result<Action> {
    Ok(match input {
        Input::CreateTable {
            columns,
            primary_key,
            unique_constraints,
            foreign_keys,
            if_not_exists,
        } => {
            return normalize_create(
                columns,
                primary_key,
                unique_constraints,
                &foreign_keys,
                if_not_exists,
            );
        }
        Input::AddColumn { column } => {
            if column.primary_key
                || column.unique
                || column.auto_increment
                || column.default.is_some()
                || column.nullable == Some(false)
            {
                return Err(unsupported("仅可添加可空且无默认值的普通列"));
            }
            Action::AddColumn {
                column: column.normalize(false)?,
            }
        }
        Input::DropColumn { column_name } => {
            name(&column_name)?;
            Action::DropColumn { column_name }
        }
        Input::RenameColumn {
            column_name,
            new_name,
        } => {
            rename(&column_name, &new_name)?;
            Action::RenameColumn {
                column_name,
                new_name,
            }
        }
        Input::RenameTable { new_table } => {
            name(&new_table)?;
            Action::RenameTable { new_table }
        }
        Input::DropTable { if_exists } => Action::DropTable { if_exists },
        Input::CreateIndex {
            index,
            columns,
            unique,
        } => {
            name(&index)?;
            check_key(&columns, None)?;
            Action::CreateIndex {
                index,
                columns,
                unique,
            }
        }
        Input::DropIndex { index } => {
            name(&index)?;
            Action::DropIndex { index }
        }
    })
}

fn normalize_create(
    columns: Vec<ColumnInput>,
    mut primary_key: Vec<String>,
    mut unique_constraints: Vec<Vec<String>>,
    foreign_keys: &[Value],
    if_not_exists: bool,
) -> Result<Action> {
    if !foreign_keys.is_empty() {
        return Err(unsupported("DDL 暂不支持创建外键"));
    }
    if columns.is_empty() || columns.len() > MAX_COLUMNS {
        return Err(invalid("DDL 列数超出范围"));
    }
    let inline: Vec<_> = columns.iter().filter(|column| column.primary_key).collect();
    if !inline.is_empty() {
        if inline.len() != 1 || !primary_key.is_empty() {
            return Err(invalid("主键声明冲突"));
        }
        primary_key.push(inline[0].name.clone());
    }
    unique_constraints.extend(
        columns
            .iter()
            .filter(|column| column.unique)
            .map(|column| vec![column.name.clone()]),
    );
    if unique_constraints.len() > MAX_KEYS {
        return Err(invalid("唯一约束数量超限"));
    }
    let names: HashSet<_> = columns.iter().map(|column| column.name.as_str()).collect();
    if names.len() != columns.len() {
        return Err(invalid("列名重复"));
    }
    let mut constraints = HashSet::new();
    if !primary_key.is_empty() {
        check_key(&primary_key, Some(&names))?;
        constraints.insert(key_members(&primary_key));
    }
    for key in &unique_constraints {
        check_key(key, Some(&names))?;
        if !constraints.insert(key_members(key)) {
            return Err(invalid("主键或唯一约束重复"));
        }
    }
    let columns = columns
        .into_iter()
        .map(|column| {
            let primary = primary_key.contains(&column.name);
            if column.auto_increment && (!primary || primary_key.len() != 1) {
                return Err(invalid("自动编号要求单列整数主键"));
            }
            column.normalize(primary)
        })
        .collect::<Result<_>>()?;
    Ok(Action::CreateTable {
        columns,
        primary_key,
        unique_constraints,
        if_not_exists,
    })
}

impl ColumnInput {
    fn normalize(self, primary: bool) -> Result<Column> {
        name(&self.name)?;
        let kind = match (self.kind.as_str(), self.length, self.precision, self.scale) {
            ("integer", None, None, None) => ColumnType::Integer,
            ("bigint", None, None, None) => ColumnType::Bigint,
            ("varchar", Some(length @ 1..=4096), None, None) => ColumnType::Varchar(length),
            ("text", None, None, None) => ColumnType::Text,
            ("decimal", None, Some(precision @ 1..=38), Some(scale)) if scale <= precision => {
                ColumnType::Decimal { precision, scale }
            }
            ("boolean", None, None, None) => ColumnType::Boolean,
            ("date", None, None, None) => ColumnType::Date,
            ("datetime", None, None, None) => ColumnType::Datetime,
            ("timestamp", None, None, None) => ColumnType::Timestamp,
            ("blob", None, None, None) => ColumnType::Blob,
            _ => return Err(invalid("DDL 字段类型或尺寸无效")),
        };
        if primary && self.nullable == Some(true) {
            return Err(invalid("主键列不可声明为可空"));
        }
        let nullable = !primary && self.nullable.unwrap_or(true);
        if self.auto_increment
            && (!matches!(kind, ColumnType::Integer | ColumnType::Bigint) || self.default.is_some())
        {
            return Err(invalid("自动编号要求没有默认值的整数列"));
        }
        let default = self
            .default
            .as_ref()
            .map(|value| normalize_default(&kind, nullable, value))
            .transpose()?;
        Ok(Column {
            name: self.name,
            kind,
            nullable,
            default,
            identity: self.auto_increment,
        })
    }
}

fn normalize_default(kind: &ColumnType, nullable: bool, value: &Value) -> Result<Literal> {
    if value.is_null() && nullable {
        return Ok(Literal::Null);
    }
    let field = |name| {
        value
            .as_object()
            .filter(|object| object.len() == 1)
            .and_then(|object| object.get(name))
            .and_then(Value::as_str)
    };
    match kind {
        ColumnType::Integer | ColumnType::Bigint => {
            let integer = value
                .as_i64()
                .filter(|value| value.unsigned_abs() <= 9_007_199_254_740_991)
                .or_else(|| {
                    field("integer").and_then(|text| {
                        text.parse::<i64>()
                            .ok()
                            .filter(|number| number.to_string() == text)
                    })
                });
            if let Some(integer) = integer
                && (*kind == ColumnType::Bigint || i32::try_from(integer).is_ok())
            {
                return Ok(Literal::Integer(integer));
            }
        }
        ColumnType::Boolean => {
            if let Some(value) = value.as_bool() {
                return Ok(Literal::Boolean(value));
            }
        }
        ColumnType::Text | ColumnType::Varchar(_) => {
            if let Some(value) = value.as_str()
                && value.len() <= 1024
                && !value.chars().any(char::is_control)
                && !matches!(kind, ColumnType::Varchar(length) if value.chars().count() > usize::from(*length))
            {
                return Ok(Literal::Text(value.to_owned()));
            }
        }
        ColumnType::Decimal { precision, scale } => {
            if let Some(text) = field("decimal")
                && decimal_fits(text, *precision, *scale)
            {
                return Ok(Literal::Decimal(text.to_owned()));
            }
        }
        _ => {}
    }
    Err(invalid("DDL 默认值必须是与列类型相符的常量"))
}

fn decimal_fits(text: &str, precision: u8, scale: u8) -> bool {
    let magnitude = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = magnitude.split_once('.').unwrap_or((magnitude, ""));
    !whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
        && (!magnitude.contains('.') || !fraction.is_empty())
        && whole.trim_start_matches('0').len() <= usize::from(precision - scale)
        && fraction.len() <= usize::from(scale)
}

fn key_members(key: &[String]) -> Vec<&str> {
    let mut sorted: Vec<_> = key.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted
}

fn check_key(key: &[String], columns: Option<&HashSet<&str>>) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEYS {
        return Err(invalid("键的列数超出范围"));
    }
    let mut seen = HashSet::new();
    for column in key {
        name(column)?;
        if !seen.insert(column) || columns.is_some_and(|names| !names.contains(column.as_str())) {
            return Err(invalid("键引用了重复或不存在的列"));
        }
    }
    Ok(())
}

fn take_string(arguments: &mut Map<String, Value>, field: &'static str) -> Result<String> {
    match arguments.remove(field) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value),
        _ => Err(invalid("DDL 缺少必填字符串参数")),
    }
}

fn rename(old: &str, new: &str) -> Result<()> {
    name(old)?;
    name(new)?;
    if old == new {
        return Err(invalid("DDL 重命名要求新名称不同"));
    }
    Ok(())
}

fn name(value: &str) -> Result<()> {
    let mut bytes = value.bytes();
    if !matches!(bytes.next(), Some(b'a'..=b'z' | b'_'))
        || value.len() > 64
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || value.starts_with("sqlite_")
    {
        return Err(invalid("DDL 标识符无效或保留"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> Error {
    Error::invalid_argument(ErrorCode::MCP_INVALID_ARGUMENT, message)
}

fn unsupported(message: &'static str) -> Error {
    Error::unsupported(ErrorCode::CAPABILITY_UNSUPPORTED, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn preview(operation: &str, fields: Value) -> Result<Request> {
        let Value::Object(mut arguments) = fields else {
            panic!("expected test object");
        };
        arguments.extend(
            json!({"connection_alias":"dev","database":"main","table":"users"})
                .as_object()
                .expect("common fields")
                .clone(),
        );
        decode(
            "preview_schema_change",
            json!({"operation":operation, "arguments":arguments})
                .as_object()
                .expect("preview")
                .clone(),
        )
    }

    #[test]
    fn decodes_typed_defaults_and_normalizes_primary_key() {
        let request = preview("create_table", json!({"columns":[
            {"name":"id","type":"integer","primary_key":true,"identity":true},
            {"name":"note","type":"text","default":"x'; DROP TABLE users; --"},
            {"name":"price","type":"decimal","precision":5,"scale":2,"default":{"decimal":"-123.45"}}
        ]})).expect("constant text stays data");
        assert_eq!(request.alias, "dev");
        assert!(request.id.is_none());
        let Action::CreateTable {
            columns,
            primary_key,
            ..
        } = request.change.action
        else {
            panic!("expected CREATE TABLE");
        };
        assert_eq!(primary_key, ["id"]);
        assert!(columns[0].identity && !columns[0].nullable);
        assert_eq!(columns[2].default, Some(Literal::Decimal("-123.45".into())));
    }

    #[test]
    fn rejects_unknown_fields_and_unsafe_shapes() {
        for column in [
            json!({"name":"a.b","type":"text"}),
            json!({"name":"a","type":"text","default":{"sql":"now()"}}),
            json!({"name":"a","type":"blob","length":12}),
            json!({"name":"a","type":"mystery"}),
            json!({"name":"a","type":"integer","identity":true,"auto_increment":true}),
            json!({"name":"a","type":"integer","primary_key":true,"nullable":true}),
        ] {
            assert!(preview("create_table", json!({"columns":[column]})).is_err());
        }
        assert!(preview("drop_table", json!({"sql":"DROP TABLE users"})).is_err());
        assert!(
            preview(
                "alter_table",
                json!({"action":"drop_table","if_exists":true})
            )
            .is_err()
        );
        assert!(preview("create_index", json!({"index":"i","columns":["a","a"]})).is_err());
    }
}

/// Validated SQL and its display-only representation.
pub(crate) struct Compiled {
    pub sql: String,
    pub redacted_sql: String,
}

pub(crate) fn compile(
    change: &Change,
    syntax: &sqarvo_driver_api::ddl::Syntax,
) -> Result<Compiled> {
    let delimiter = syntax.delimiter;
    let (dialect, backslash): (&dyn sqlparser::dialect::Dialect, bool) = match delimiter {
        '"' if change.database == "main" => (&sqlparser::dialect::SQLiteDialect {}, false),
        '`' if !matches!(
            change.database.as_str(),
            "information_schema" | "mysql" | "performance_schema" | "sys" | "sqarvo_guard"
        ) =>
        {
            (&sqlparser::dialect::MySqlDialect {}, true)
        }
        _ => return Err(unsupported("DDL 目标库或驱动不受支持")),
    };
    let sql = render(change, syntax, &syntax.quote, backslash, false)?;

    // Independently quote the normalized identifiers for the reference AST.
    // Equality includes modifiers, options, column expressions, and all nested nodes.
    let reference = render(
        change,
        syntax,
        &|name| {
            format!(
                "{delimiter}{}{delimiter}",
                name.replace(delimiter, &format!("{delimiter}{delimiter}"))
            )
        },
        backslash,
        false,
    )?;
    let parse = |sql: &str| {
        sqlparser::parser::Parser::parse_sql(dialect, sql)
            .map_err(|_| unsupported("DDL 生成语句无法通过方言 AST 验证"))
    };
    let actual = parse(&sql)?;
    let expected = parse(&reference)?;
    if actual.len() != 1 || actual != expected || !expected_statement(change, &actual[0]) {
        return Err(unsupported("DDL 生成语句超出操作白名单"));
    }
    Ok(Compiled {
        sql,
        redacted_sql: render(change, syntax, &syntax.quote, backslash, true)?,
    })
}

fn expected_statement(change: &Change, statement: &sqlparser::ast::Statement) -> bool {
    use sqlparser::ast::{ObjectType, Statement};

    let table = [change.database.as_str(), change.table.as_str()];
    match (&change.action, statement) {
        (Action::CreateTable { columns, .. }, Statement::CreateTable(create)) => {
            ast_name(&create.name, &table)
                && !create.temporary
                && !create.if_not_exists
                && create.columns.len() == columns.len()
                && create
                    .columns
                    .iter()
                    .zip(columns)
                    .all(|(actual, expected)| actual.name.value == expected.name)
                && ddl_table_shape(create)
        }
        (
            Action::CreateIndex {
                index,
                columns,
                unique,
            },
            Statement::CreateIndex(create),
        ) => {
            use sqlparser::ast::Expr;
            let sqlite = create
                .name
                .as_ref()
                .is_some_and(|name| ast_name(name, &[change.database.as_str(), index]))
                && ast_name(&create.table_name, &[&change.table]);
            let maria = create
                .name
                .as_ref()
                .is_some_and(|name| ast_name(name, &[index]))
                && ast_name(&create.table_name, &table);
            (sqlite || maria) && create.unique == *unique && !create.if_not_exists
                && create.using.is_none() && create.predicate.is_none()
                && create.index_options.is_empty() && create.alter_options.is_empty()
                && create.columns.len() == columns.len()
                && create.columns.iter().zip(columns).all(|(actual, expected)| {
                    matches!(&actual.column.expr, Expr::Identifier(name) if name.value == *expected)
                })
        }
        (
            action,
            Statement::Drop {
                object_type,
                names,
                if_exists: false,
                cascade: false,
                restrict: false,
                purge: false,
                temporary: false,
                table: None,
            },
        ) => {
            let [name] = names.as_slice() else {
                return false;
            };
            match (action, object_type) {
                (Action::DropTable { .. }, ObjectType::Table) => ast_name(name, &table),
                (Action::DropIndex { index }, ObjectType::Index) => {
                    ast_name(name, &[change.database.as_str(), index])
                }
                _ => false,
            }
        }
        (action, Statement::AlterTable(alter)) => {
            ast_name(&alter.name, &table)
                && !alter.if_exists
                && !alter.only
                && alter.location.is_none()
                && alter.on_cluster.is_none()
                && alter.table_type.is_none()
                && expected_alter(action, &alter.operations)
        }
        _ => false,
    }
}

fn ast_name(name: &sqlparser::ast::ObjectName, expected: &[&str]) -> bool {
    name.0.len() == expected.len()
        && name.0.iter().zip(expected).all(|(part, expected)| {
            part.as_ident()
                .is_some_and(|ident| ident.value == *expected && ident.quote_style.is_some())
        })
}

fn expected_alter(action: &Action, operations: &[sqlparser::ast::AlterTableOperation]) -> bool {
    use sqlparser::ast::{AlterTableOperation as Alter, RenameTableNameKind};
    match (action, operations) {
        (
            Action::AddColumn { column },
            [
                Alter::AddColumn {
                    if_not_exists: false,
                    column_def,
                    column_position: None,
                    ..
                },
            ],
        ) => column_def.name.value == column.name && column_def.options.is_empty(),
        (
            Action::DropColumn { column_name },
            [
                Alter::DropColumn {
                    column_names,
                    if_exists: false,
                    drop_behavior: None,
                    ..
                },
            ],
        ) => matches!(column_names.as_slice(), [name] if name.value == *column_name),
        (
            Action::RenameColumn {
                column_name,
                new_name,
            },
            [
                Alter::RenameColumn {
                    old_column_name,
                    new_column_name,
                },
            ],
        ) => old_column_name.value == *column_name && new_column_name.value == *new_name,
        (
            Action::RenameTable { new_table },
            [
                Alter::RenameTable {
                    table_name: RenameTableNameKind::To(name),
                },
            ],
        ) => ast_name(name, &[new_table]),
        (Action::DropIndex { index }, [Alter::DropIndex { name }]) => name.value == *index,
        _ => false,
    }
}

fn render(
    change: &Change,
    syntax: &sqarvo_driver_api::ddl::Syntax,
    quote: &dyn Fn(&str) -> String,
    backslash: bool,
    redact: bool,
) -> Result<String> {
    use sqarvo_driver_api::ddl::IndexScope;
    let catalog = quote(&change.database);
    let table = quote(&change.table);
    let qualified = format!("{catalog}.{table}");
    let key = |columns: &[String]| {
        columns
            .iter()
            .map(|name| quote(name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let column_sql = |column: &Column| -> Result<String> {
        let mut result = format!(
            "{} {}",
            quote(&column.name),
            (syntax.column_type)(&column.kind)
        );
        if !column.nullable {
            result.push_str(" NOT NULL");
        }
        if column.identity {
            result.push_str(
                (syntax.identity)(&column.kind)
                    .ok_or_else(|| unsupported("驱动不支持该字段的自动编号"))?,
            );
        }
        if let Some(default) = &column.default {
            result.push_str(" DEFAULT ");
            result.push_str(&literal_sql(default, backslash, redact));
        }
        Ok(result)
    };
    let sql = match &change.action {
        Action::CreateTable {
            columns,
            primary_key,
            unique_constraints,
            ..
        } => {
            let mut definitions = columns.iter().map(column_sql).collect::<Result<Vec<_>>>()?;
            if !(primary_key.is_empty()
                || syntax.inline_identity && columns.iter().any(|column| column.identity))
            {
                definitions.push(format!("PRIMARY KEY ({})", key(primary_key)));
            }
            definitions.extend(
                unique_constraints
                    .iter()
                    .map(|columns| format!("UNIQUE ({})", key(columns))),
            );
            format!(
                "CREATE TABLE {qualified} ({}){}",
                definitions.join(", "),
                syntax.table_suffix
            )
        }
        Action::AddColumn { column } => {
            format!("ALTER TABLE {qualified} ADD COLUMN {}", column_sql(column)?)
        }
        Action::DropColumn { column_name } => {
            format!("ALTER TABLE {qualified} DROP COLUMN {}", quote(column_name))
        }
        Action::RenameColumn {
            column_name,
            new_name,
        } => format!(
            "ALTER TABLE {qualified} RENAME COLUMN {} TO {}",
            quote(column_name),
            quote(new_name)
        ),
        Action::RenameTable { new_table } => {
            format!("ALTER TABLE {qualified} RENAME TO {}", quote(new_table))
        }
        Action::DropTable { .. } => format!("DROP TABLE {qualified}"),
        Action::CreateIndex {
            index,
            columns,
            unique,
        } => {
            let modifier = if *unique { "UNIQUE " } else { "" };
            let index = quote(index);
            let target = match syntax.index_scope {
                IndexScope::Catalog => format!("{catalog}.{index} ON {table}"),
                IndexScope::Table => format!("{index} ON {qualified}"),
            };
            format!("CREATE {modifier}INDEX {target} ({})", key(columns))
        }
        Action::DropIndex { index } => {
            let index = quote(index);
            match syntax.index_scope {
                IndexScope::Catalog => format!("DROP INDEX {catalog}.{index}"),
                IndexScope::Table => format!("ALTER TABLE {qualified} DROP INDEX {index}"),
            }
        }
    };
    if sql.len() > 16 * 1024 {
        return Err(invalid("DDL 生成语句超过大小限制"));
    }
    Ok(sql)
}

fn literal_sql(default: &Literal, backslash: bool, redact: bool) -> String {
    if redact {
        return "?".into();
    }
    match default {
        Literal::Null => "NULL".into(),
        Literal::Boolean(value) => u8::from(*value).to_string(),
        Literal::Integer(value) => value.to_string(),
        Literal::Decimal(value) => value.clone(),
        Literal::Text(value) => {
            let text = value.replace('\'', "''");
            if backslash {
                format!("'{}'", text.replace('\\', "\\\\"))
            } else {
                format!("'{text}'")
            }
        }
    }
}

/// A no-op is decided from the validated live snapshot, before dispatch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    Apply,
    AlreadyExists,
    AlreadyAbsent,
}

pub(crate) fn check_inspection(
    change: &Change,
    inspection: &sqarvo_driver_api::ddl::Inspection,
    syntax: &sqarvo_driver_api::ddl::Syntax,
) -> Result<Effect> {
    use sqarvo_driver_api::ddl::Table;
    let unverified = || unsupported("无法确认 DDL 目标的结构或依赖");
    let missing = || Error::not_found(ErrorCode::MCP_INVALID_ARGUMENT, "DDL 目标对象不存在");
    let occupied =
        || Error::failed_precondition(ErrorCode::MCP_INVALID_ARGUMENT, "DDL 目标名称已被占用");
    let table: &Table = match (&change.action, inspection.table.as_ref()) {
        (
            Action::CreateTable {
                if_not_exists: true,
                ..
            },
            Some(_),
        ) => {
            return Ok(Effect::AlreadyExists);
        }
        (Action::CreateTable { .. }, Some(_)) => return Err(occupied()),
        (Action::CreateTable { .. }, None) if inspection.destination_exists => {
            return Err(occupied());
        }
        (Action::CreateTable { .. }, None) if inspection.dependencies_known => {
            return Ok(Effect::Apply);
        }
        (Action::CreateTable { .. }, None) => return Err(unverified()),
        (Action::DropTable { if_exists: true }, None) if !inspection.destination_exists => {
            return Ok(Effect::AlreadyAbsent);
        }
        (_, Some(table)) => table,
        (_, None) => return Err(missing()),
    };
    if !inspection.dependencies_known || !ordinary_definition(&table.definition, syntax.delimiter) {
        return Err(unverified());
    }
    let column = |name: &str| {
        table
            .columns
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(name))
    };
    match &change.action {
        Action::AddColumn { column: added } if column(&added.name).is_some() => {
            return Err(occupied());
        }
        Action::DropColumn { column_name } => {
            let target = column(column_name).ok_or_else(missing)?;
            if table.columns.len() <= 1
                || target.primary_key != 0
                || table.indexes.iter().any(|index| {
                    index
                        .columns
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(column_name))
                })
            {
                return Err(unverified());
            }
        }
        Action::RenameColumn {
            column_name,
            new_name,
        } => {
            column(column_name).ok_or_else(missing)?;
            if column(new_name).is_some() {
                return Err(occupied());
            }
        }
        Action::RenameTable { .. } if inspection.destination_exists => return Err(occupied()),
        Action::CreateIndex { columns, .. } => {
            if inspection.destination_exists {
                return Err(occupied());
            }
            for name in columns {
                column(name).ok_or_else(missing)?;
            }
        }
        Action::DropIndex { index } => {
            let index = table
                .indexes
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(index))
                .ok_or_else(missing)?;
            if !index.independent || index.unique {
                return Err(unverified());
            }
        }
        _ => {}
    }
    Ok(Effect::Apply)
}

fn ordinary_definition(sql: &str, delimiter: char) -> bool {
    use sqlparser::ast::Statement;
    use sqlparser::dialect::{Dialect, MySqlDialect, SQLiteDialect};
    use sqlparser::parser::Parser;
    let dialect: &dyn Dialect = if delimiter == '"' {
        &SQLiteDialect {}
    } else {
        &MySqlDialect {}
    };
    let Ok(statements) = Parser::parse_sql(dialect, sql) else {
        return false;
    };
    let [Statement::CreateTable(table)] = statements.as_slice() else {
        return false;
    };
    ddl_table_shape(table)
}

fn ddl_table_shape(table: &sqlparser::ast::CreateTable) -> bool {
    use sqlparser::ast::{ColumnOption, Expr, TableConstraint, UnaryOperator};

    fn literal(expr: &Expr) -> bool {
        match expr {
            Expr::Value(_) => true,
            Expr::Nested(value) => literal(value),
            Expr::UnaryOp {
                op: UnaryOperator::Plus | UnaryOperator::Minus,
                expr,
            } => {
                matches!(expr.as_ref(), Expr::Value(value) if matches!(&value.value, sqlparser::ast::Value::Number(..)))
            }
            _ => false,
        }
    }
    !table.temporary
        && !table.external
        && !table.or_replace
        && table.query.is_none()
        && table.like.is_none()
        && table.partition_by.is_none()
        && table.constraints.iter().all(|constraint| {
            matches!(
                constraint,
                TableConstraint::PrimaryKey(_)
                    | TableConstraint::Unique(_)
                    | TableConstraint::Index(_)
            )
        })
        && table
            .columns
            .iter()
            .flat_map(|column| &column.options)
            .all(|option| match &option.option {
                ColumnOption::Null
                | ColumnOption::NotNull
                | ColumnOption::PrimaryKey(_)
                | ColumnOption::Unique(_)
                | ColumnOption::Collation(_)
                | ColumnOption::Comment(_)
                | ColumnOption::CharacterSet(_) => true,
                ColumnOption::Default(value) => literal(value),
                ColumnOption::DialectSpecific(tokens) => {
                    tokens.len() == 1
                        && matches!(
                            tokens[0].to_string().as_str(),
                            "AUTO_INCREMENT" | "AUTOINCREMENT"
                        )
                }
                _ => false,
            })
}
