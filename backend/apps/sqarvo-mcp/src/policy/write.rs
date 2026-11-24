//! Structured MCP write input. SQL rendering and transaction eligibility belong to the driver.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{ColumnChange, Filter, FilterCondition, FilterOperator, InsertRow, Value};
use sqlparser::ast::Ident;
use sqlparser::dialect::{Dialect, MySqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};

use crate::access::OperationType;

/// Maximum serialized write arguments accepted by either transport.
pub const MAX_ARGUMENT_BYTES: usize = 128 * 1024;
const MAX_COLUMNS: usize = 64;
const MAX_CONDITIONS: usize = 32;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 255;

/// Write parameters shared by execution and preview.
#[derive(Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum WriteArguments {
    /// Insert one or more records into a single table.
    Insert {
        /// Connection profile name or UUID.
        connection_alias: String,
        /// Database containing the target table.
        database: String,
        /// Target table name.
        table: String,
        /// Records containing the same set of columns.
        rows: Vec<Record>,
    },
    /// Update records matching all conditions.
    Update {
        /// Connection profile name or UUID.
        connection_alias: String,
        /// Database containing the target table.
        database: String,
        /// Target table name.
        table: String,
        /// Values to assign.
        changes: Record,
        /// Comparisons combined with AND.
        conditions: Vec<Condition>,
    },
    /// Delete records matching all conditions.
    Delete {
        /// Connection profile name or UUID.
        connection_alias: String,
        /// Database containing the target table.
        database: String,
        /// Target table name.
        table: String,
        /// Comparisons combined with AND.
        conditions: Vec<Condition>,
    },
}

type Record = BTreeMap<String, Scalar>;

/// Scalar values the write interface can bind without lossy conversion.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Scalar {
    /// Boolean value.
    Bool(bool),
    /// Signed 64-bit integer.
    Signed(i64),
    /// Unsigned 64-bit integer.
    Unsigned(u64),
    /// Finite floating-point value.
    Float(f64),
    /// UTF-8 text.
    Text(String),
    /// SQL NULL.
    Null,
}

impl Scalar {
    fn bind(self) -> Result<Value> {
        match self {
            Self::Bool(value) => Ok(Value::Bool(value)),
            Self::Signed(value) => Ok(Value::Int(value)),
            Self::Unsigned(value) => Ok(Value::UInt(value)),
            Self::Float(value) if value.is_finite() => Ok(Value::Float(value)),
            Self::Text(value) if value.len() <= MAX_TEXT_BYTES => Ok(Value::Text(value)),
            Self::Null => Ok(Value::Null),
            _ => Err(invalid("The write value is out of the supported range")),
        }
    }
}

/// A comparison between a column and a scalar or an explicit null check.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    column: String,
    operator: Comparison,
    value: Option<Scalar>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Comparison {
    Eq,
    Lt,
    Lte,
    Gt,
    Gte,
    IsNull,
    IsNotNull,
}

impl From<Comparison> for FilterOperator {
    fn from(value: Comparison) -> Self {
        match value {
            Comparison::Eq => Self::Eq,
            Comparison::Lt => Self::Lt,
            Comparison::Lte => Self::Le,
            Comparison::Gt => Self::Gt,
            Comparison::Gte => Self::Ge,
            Comparison::IsNull => Self::IsNull,
            Comparison::IsNotNull => Self::IsNotNull,
        }
    }
}

/// Write target and validated input, containing no SQL fragments.
pub struct WritePlan {
    /// Connection profile name or UUID.
    pub connection_alias: String,
    /// Database containing the target table.
    pub database: String,
    /// Target table name.
    pub table: String,
    /// Operation and bound values.
    pub action: WriteAction,
}

/// Validated input in the existing driver value and filter representations.
pub enum WriteAction {
    /// Insert validated records.
    Insert(Vec<InsertRow>),
    /// Update rows matching the filter.
    Update {
        /// Column assignments.
        changes: Vec<ColumnChange>,
        /// Nonempty row filter.
        filter: Filter,
    },
    /// Delete rows matching the filter.
    Delete(Filter),
}

impl WriteAction {
    /// Request authorization classification.
    pub const fn operation(&self) -> OperationType {
        match self {
            Self::Insert(_) => OperationType::Insert,
            Self::Update { .. } => OperationType::Update,
            Self::Delete(_) => OperationType::Delete,
        }
    }

    /// Maximum direct target rows allowed before commit.
    pub const fn max_rows(&self) -> usize {
        match self {
            Self::Insert(_) => 100,
            Self::Update { .. } => 50,
            Self::Delete(_) => 20,
        }
    }
}

impl WritePlan {
    /// Decode a bounded argument object and reject operation mismatches or unsafe input shapes.
    pub fn decode(arguments: serde_json::Value, expected: Option<OperationType>) -> Result<Self> {
        if serde_json::to_vec(&arguments)
            .map_err(|_| invalid("Failed to encode write arguments"))?
            .len()
            > MAX_ARGUMENT_BYTES
        {
            return Err(invalid("Write arguments exceed the size limit"));
        }
        let input = serde_json::from_value::<WriteArguments>(arguments)
            .map_err(|_| invalid("Invalid write argument format"))?;
        let (connection_alias, database, table, action) = match input {
            WriteArguments::Insert {
                connection_alias,
                database,
                table,
                rows,
            } => {
                if rows.is_empty() || rows.len() > 100 {
                    return Err(invalid("The insert must contain 1 to 100 rows"));
                }
                let columns = rows[0].keys().collect::<Vec<_>>();
                if rows
                    .iter()
                    .any(|row| row.keys().ne(columns.iter().copied()))
                {
                    return Err(invalid("All rows in one insert must use the same columns"));
                }
                let rows = rows
                    .into_iter()
                    .map(|row| changes(row).map(InsertRow::new))
                    .collect::<Result<_>>()?;
                (connection_alias, database, table, WriteAction::Insert(rows))
            }
            WriteArguments::Update {
                connection_alias,
                database,
                table,
                changes: record,
                conditions,
            } => (
                connection_alias,
                database,
                table,
                WriteAction::Update {
                    changes: changes(record)?,
                    filter: filter(conditions)?,
                },
            ),
            WriteArguments::Delete {
                connection_alias,
                database,
                table,
                conditions,
            } => (
                connection_alias,
                database,
                table,
                WriteAction::Delete(filter(conditions)?),
            ),
        };
        if expected.is_some_and(|expected| expected != action.operation()) {
            return Err(invalid("The write type does not match the tool"));
        }
        if !identifier(&connection_alias) || !identifier(&database) || !identifier(&table) {
            return Err(invalid("Invalid connection or target name"));
        }
        Ok(Self {
            connection_alias,
            database,
            table,
            action,
        })
    }
}

pub use sqarvo_driver_api::WriteDialect;

/// Verify that the driver statement and ordered bindings exactly represent the write plan.
///
/// The reference statement contains only identifiers escaped by sqlparser and placeholders.
/// Parsing both statements and comparing their ASTs rejects every extra clause, including
/// conflict handlers, functions, joins, and returning clauses.
pub fn check_generated_sql(
    plan: &WritePlan,
    dialect: WriteDialect,
    sql: &str,
    bindings: &[Value],
) -> Result<()> {
    let (parser, quote): (&dyn Dialect, char) = match dialect {
        WriteDialect::Sqlite => (&SQLiteDialect {}, '"'),
        WriteDialect::MariaDb => (&MySqlDialect {}, '`'),
    };
    let failure = || invalid("The generated SQL does not match the controlled write plan");
    let quoted = |name: &str| Ident::with_quote(quote, name).to_string();
    let (expected, values) = render(plan, dialect, &quoted)?;
    if !values.into_iter().eq(bindings) {
        return Err(failure());
    }
    // Executable MariaDB comments may be ignored by the parser but interpreted by the server.
    let tokens = Tokenizer::new(parser, sql)
        .tokenize()
        .map_err(|_| failure())?;
    if tokens.iter().any(|token| {
        matches!(
            token,
            Token::Whitespace(
                Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_)
            )
        )
    }) {
        return Err(failure());
    }
    let expected = Parser::parse_sql(parser, &expected).map_err(|_| failure())?;
    let supplied = Parser::parse_sql(parser, sql).map_err(|_| failure())?;
    if expected.len() != 1 || supplied != expected {
        return Err(failure());
    }
    Ok(())
}

/// Build a parameterized statement with a driver-provided identifier quoting function.
/// The caller must verify the statement with [`check_generated_sql`] before execution.
pub fn prepare_sql(
    plan: &WritePlan,
    dialect: WriteDialect,
    quote_identifier: impl Fn(&str) -> String,
) -> Result<(String, Vec<Value>)> {
    let (sql, values) = render(plan, dialect, &quote_identifier)?;
    Ok((sql, values.into_iter().cloned().collect()))
}

fn render<'a>(
    plan: &'a WritePlan,
    dialect: WriteDialect,
    quoted: &impl Fn(&str) -> String,
) -> Result<(String, Vec<&'a Value>)> {
    let target = format!("{}.{}", quoted(&plan.database), quoted(&plan.table));
    let mut values = Vec::new();
    // SQLite column constraints may declare ON CONFLICT REPLACE, which deletes a
    // conflicting row without including that deletion in the direct DML count.
    let conflict = match dialect {
        WriteDialect::Sqlite => " OR ABORT",
        WriteDialect::MariaDb => "",
    };
    let sql = match &plan.action {
        WriteAction::Insert(rows) => {
            let first = rows
                .first()
                .filter(|row| !row.values.is_empty())
                .ok_or_else(|| invalid("An insert requires at least one row and one column"))?;
            let columns = first.columns();
            if rows.iter().any(|row| row.columns() != columns) {
                return Err(invalid("The write plan has an invalid set of columns"));
            }
            let names = columns
                .iter()
                .map(|name| quoted(name))
                .collect::<Vec<_>>()
                .join(", ");
            let placeholders = vec!["?"; columns.len()].join(", ");
            let tuples = vec![format!("({placeholders})"); rows.len()].join(", ");
            values.extend(
                rows.iter()
                    .flat_map(|row| row.values.iter().map(|change| &change.value)),
            );
            format!("INSERT{conflict} INTO {target} ({names}) VALUES {tuples}")
        }
        WriteAction::Update { changes, filter } => {
            if changes.is_empty() {
                return Err(invalid("The write plan has an invalid set of columns"));
            }
            let assignments = changes
                .iter()
                .map(|change| {
                    values.push(&change.value);
                    format!("{} = ?", quoted(&change.column))
                })
                .collect::<Vec<_>>()
                .join(", ");
            let predicate = reference_predicate(filter, quoted, &mut values)?;
            format!("UPDATE{conflict} {target} SET {assignments} WHERE {predicate}")
        }
        WriteAction::Delete(filter) => {
            let predicate = reference_predicate(filter, quoted, &mut values)?;
            format!("DELETE FROM {target} WHERE {predicate}")
        }
    };
    Ok((sql, values))
}

pub(crate) fn reference_predicate<'a>(
    filter: &'a Filter,
    quoted: &impl Fn(&str) -> String,
    values: &mut Vec<&'a Value>,
) -> Result<String> {
    if filter.conditions.is_empty() {
        return Err(invalid(
            "The generated write statement lacks a valid condition",
        ));
    }
    filter
        .conditions
        .iter()
        .map(|condition| {
            let operation = match (condition.operator, &condition.value) {
                (FilterOperator::IsNull, None) => "IS NULL",
                (FilterOperator::IsNotNull, None) => "IS NOT NULL",
                (operator, Some(value)) if *value != Value::Null => {
                    let comparison = match operator {
                        FilterOperator::Eq => "= ?",
                        FilterOperator::Lt => "< ?",
                        FilterOperator::Le => "<= ?",
                        FilterOperator::Gt => "> ?",
                        FilterOperator::Ge => ">= ?",
                        _ => {
                            return Err(invalid(
                                "The generated write statement contains an unsupported condition",
                            ));
                        }
                    };
                    values.push(value);
                    comparison
                }
                _ => {
                    return Err(invalid(
                        "The generated write statement contains an invalid condition value",
                    ));
                }
            };
            Ok(format!("{} {operation}", quoted(&condition.column)))
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join(" AND "))
}

fn changes(record: Record) -> Result<Vec<ColumnChange>> {
    if record.is_empty() || record.len() > MAX_COLUMNS {
        return Err(invalid("The number of write columns is out of range"));
    }
    record
        .into_iter()
        .map(|(column, value)| {
            if !identifier(&column) {
                return Err(invalid("Invalid column name"));
            }
            Ok(ColumnChange {
                column,
                value: value.bind()?,
            })
        })
        .collect()
}

fn filter(conditions: Vec<Condition>) -> Result<Filter> {
    if conditions.is_empty() || conditions.len() > MAX_CONDITIONS {
        return Err(invalid(
            "A write requires a nonempty, bounded list of conditions",
        ));
    }
    let conditions = conditions
        .into_iter()
        .map(|condition| {
            let operator = FilterOperator::from(condition.operator);
            let value = condition.value.map(Scalar::bind).transpose()?;
            if !identifier(&condition.column)
                || operator.requires_value() != value.is_some()
                || value == Some(Value::Null)
            {
                return Err(invalid("Invalid write conditions"));
            }
            Ok(FilterCondition {
                column: condition.column,
                operator,
                value,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Filter::new(conditions))
}

pub(crate) fn identifier(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && !value.chars().any(char::is_control)
}

fn invalid(message: &'static str) -> Error {
    Error::invalid_argument(ErrorCode::MCP_INVALID_ARGUMENT, message)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn generated_sql_requires_exact_target_predicate_and_bindings() {
        let plan = WritePlan::decode(
            json!({
                "operation": "update", "connection_alias": "db", "database": "main",
                "table": "orders", "changes": {"note": "bound"},
                "conditions": [{"column": "id", "operator": "eq", "value": 17}]
            }),
            None,
        )
        .unwrap();
        let bindings = [Value::Text("bound".into()), Value::Int(17)];
        for (dialect, quote) in [(WriteDialect::Sqlite, '"'), (WriteDialect::MariaDb, '`')] {
            let target = format!("{quote}main{quote}.{quote}orders{quote}");
            let conflict = if matches!(dialect, WriteDialect::Sqlite) {
                " OR ABORT"
            } else {
                ""
            };
            let set = format!("UPDATE{conflict} {target} SET {quote}note{quote} = ?");
            let sql = format!("{set} WHERE {quote}id{quote} = ?");
            check_generated_sql(&plan, dialect, &sql, &bindings).unwrap();
            for altered in [
                set.clone(),
                format!("{set} WHERE 1=1"),
                format!("{sql} OR 1=1"),
                format!("{sql}; DELETE FROM {target}"),
                format!("{sql} /*! OR 1=1 */"),
                format!("{sql} /*M! OR 1=1 */"),
                format!(
                    "UPDATE {target} SET {quote}note{quote} = upper(?) WHERE {quote}id{quote} = ?"
                ),
                sql.replace("orders", "other"),
                sql.replace("UPDATE", "UPDATE OR REPLACE"),
            ] {
                assert!(check_generated_sql(&plan, dialect, &altered, &bindings).is_err());
            }
            assert!(check_generated_sql(&plan, dialect, &sql, &[Value::Int(17)]).is_err());
        }
    }

    #[test]
    fn generated_sql_checks_insert_and_delete_ast() {
        let insert = WritePlan::decode(
            json!({
                "operation": "insert", "connection_alias": "db", "database": "main",
                "table": "orders", "rows": [{"id": 1}, {"id": 2}]
            }),
            None,
        )
        .unwrap();
        let values = [Value::Int(1), Value::Int(2)];
        let sql = "INSERT INTO `main`.`orders` (`id`) VALUES (?), (?)";
        check_generated_sql(&insert, WriteDialect::MariaDb, sql, &values).unwrap();
        assert!(
            check_generated_sql(
                &insert,
                WriteDialect::MariaDb,
                &format!("{sql} ON DUPLICATE KEY UPDATE `id` = 2"),
                &values,
            )
            .is_err()
        );

        let delete = WritePlan::decode(
            json!({
                "operation": "delete", "connection_alias": "db", "database": "main",
                "table": "orders", "conditions": [{"column": "id", "operator": "is_null"}]
            }),
            None,
        )
        .unwrap();
        let sql = r#"DELETE FROM "main"."orders" WHERE "id" IS NULL"#;
        check_generated_sql(&delete, WriteDialect::Sqlite, sql, &[]).unwrap();
        assert!(
            check_generated_sql(
                &delete,
                WriteDialect::Sqlite,
                &format!("{sql} RETURNING *"),
                &[],
            )
            .is_err()
        );
    }

    #[test]
    fn allows_data_as_values_and_rejects_operation_mismatch() {
        let args = json!({
            "operation": "insert", "connection_alias": "db", "database": "main", "table": "orders",
            "rows": [{"note": "'); DELETE FROM orders; --", "amount": null}]
        });
        let plan = WritePlan::decode(args.clone(), Some(OperationType::Insert)).unwrap();
        let WriteAction::Insert(rows) = plan.action else {
            panic!("expected insert")
        };
        assert_eq!(rows[0].values[0].value, Value::Null);
        assert_eq!(
            rows[0].values[1].value,
            Value::Text("'); DELETE FROM orders; --".into())
        );
        assert!(WritePlan::decode(args, Some(OperationType::Delete)).is_err());
    }

    #[test]
    fn requires_structured_nonempty_conditions() {
        for conditions in [
            json!([]),
            json!("1=1"),
            json!([{"column": "id", "operator": "eq", "value": null}]),
            json!([{"column": "id", "operator": "is_null", "value": 3}]),
            json!([{"column": "id", "operator": "eq", "value": {"sql": "NOW()"}}]),
        ] {
            assert!(
                WritePlan::decode(
                    json!({
                        "operation": "delete", "connection_alias": "db", "database": "main",
                        "table": "orders", "conditions": conditions
                    }),
                    None
                )
                .is_err()
            );
        }
    }

    #[test]
    fn rejects_unknown_fields_invalid_identifiers_and_excess_bytes() {
        let base = json!({
            "operation": "update", "connection_alias": "db", "database": "main",
            "table": "orders", "changes": {"note": "ok"},
            "conditions": [{"column": "id", "operator": "lte", "value": 2}]
        });
        let plan = WritePlan::decode(base.clone(), Some(OperationType::Update)).unwrap();
        assert_eq!(plan.action.max_rows(), 50);

        for (field, value) in [
            ("table", json!("invalid\u{0000}name")),
            ("changes", json!({"note": "x".repeat(MAX_ARGUMENT_BYTES)})),
            ("sql", json!("UPDATE orders SET note = 1")),
        ] {
            let mut args = base.clone();
            args[field] = value;
            assert!(WritePlan::decode(args, None).is_err(), "{field}");
        }
    }

    #[test]
    fn rejects_excess_rows_and_mismatched_columns() {
        for rows in [
            json!(vec![json!({"id": 1}); 101]),
            json!([{"id": 1}, {"other": 2}]),
        ] {
            assert!(
                WritePlan::decode(
                    json!({
                        "operation": "insert", "connection_alias": "db", "database": "main",
                        "table": "orders", "rows": rows
                    }),
                    None
                )
                .is_err()
            );
        }
    }
}
