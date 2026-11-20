//! SQLite rendering rules for structured schema changes.

use sqarvo_driver_api::ddl::{ColumnType, IndexScope, Syntax};

pub(crate) fn syntax() -> Syntax {
    Syntax {
        delimiter: '"',
        index_scope: IndexScope::Catalog,
        table_suffix: "",
        inline_identity: true,
        quote: crate::table_data::quote_identifier,
        column_type,
        identity: |kind| matches!(kind, ColumnType::Integer).then_some(" PRIMARY KEY"),
    }
}

fn column_type(kind: &ColumnType) -> String {
    match kind {
        ColumnType::Integer => "INTEGER".into(),
        ColumnType::Bigint => "BIGINT".into(),
        ColumnType::Varchar(length) => format!("VARCHAR({length})"),
        ColumnType::Text => "TEXT".into(),
        ColumnType::Decimal { precision, scale } => format!("DECIMAL({precision}, {scale})"),
        ColumnType::Boolean => "BOOLEAN".into(),
        ColumnType::Date => "DATE".into(),
        ColumnType::Datetime => "DATETIME".into(),
        ColumnType::Timestamp => "TIMESTAMP".into(),
        ColumnType::Blob => "BLOB".into(),
    }
}

use rusqlite::{Connection, OptionalExtension, Params, Row};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::ddl::{Action, Change, Index, Inspection, Table, TableColumn};

pub(crate) fn inspect(connection: &Connection, change: &Change) -> Result<Inspection> {
    if change.database != "main" {
        return Err(unsupported());
    }
    let table = &change.table;
    let name = match &change.action {
        Action::RenameTable { new_table } => new_table,
        Action::CreateIndex { index, .. } => index,
        _ => table,
    };
    let is_index = matches!(change.action, Action::CreateIndex { .. });
    let destination_exists = scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name = ? COLLATE NOCASE
          AND (? = 1 OR type IN ('table','view','index')))",
        (name, is_index),
    )?;
    let target = connection
        .query_row(
            "SELECT t.type, CASE WHEN length(CAST(s.sql AS BLOB)) <= 32768 THEN s.sql END
          FROM pragma_table_list AS t JOIN main.sqlite_schema AS s
            ON s.name = t.name COLLATE NOCASE AND s.type IN ('table','view')
          WHERE t.schema = 'main' AND t.name = ? COLLATE NOCASE",
            [table],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(metadata_error)?;
    let target = match target {
        Some((kind, Some(definition))) if kind == "table" => {
            Some(table_metadata(connection, table, definition)?)
        }
        Some(_) => return Err(unsupported()),
        None => None,
    };
    // Table changes can affect implicit references in views and trigger bodies.
    // Require their absence across main for any operation creating or changing a table.
    let table_change = !matches!(change.action, Action::CreateIndex { .. });
    let dependencies_known = !scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema
           WHERE (?1 AND type IN ('trigger', 'view'))
              OR (type = 'trigger' AND tbl_name = ?2 COLLATE NOCASE))
         OR (SELECT count(*) FROM (SELECT 1 FROM main.sqlite_schema LIMIT 2049)) > 2048
         OR EXISTS (
             SELECT 1 FROM pragma_table_list AS t
             JOIN pragma_foreign_key_list(t.name, 'main') AS fk
             WHERE t.schema = 'main' AND t.type = 'table'
             AND (t.name = ?2 COLLATE NOCASE OR fk.\"table\" = ?2 COLLATE NOCASE
                  OR fk.\"table\" = ?3 COLLATE NOCASE)
         )",
        (table_change, table, name),
    )?;
    Ok(Inspection {
        table: target,
        destination_exists,
        dependencies_known,
    })
}

fn table_metadata(connection: &Connection, table: &str, definition: String) -> Result<Table> {
    let columns = bounded(
        connection,
        "SELECT CASE WHEN length(CAST(name AS BLOB)) <= 256 THEN name END,
                CASE WHEN length(CAST(type AS BLOB)) <= 256 THEN type END,
                \"notnull\", pk, hidden
           FROM pragma_table_xinfo(?, 'main') LIMIT 65",
        [table],
        |row| {
            Ok((
                TableColumn {
                    name: row.get(0)?,
                    kind: row.get(1)?,
                    nullable: !row.get::<_, bool>(2)?,
                    primary_key: row.get(3)?,
                },
                row.get::<_, u32>(4)?,
            ))
        },
        64,
    )?;
    if columns.is_empty() || columns.iter().any(|(_, hidden)| *hidden != 0) {
        return Err(unsupported());
    }
    let mut columns: Vec<_> = columns.into_iter().map(|(column, _)| column).collect();
    let indexes = bounded(
        connection,
        "SELECT CASE WHEN length(CAST(name AS BLOB)) <= 256 THEN name END,
                \"unique\", origin, partial
           FROM pragma_index_list(?, 'main') LIMIT 65",
        [table],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        },
        64,
    )?;
    // In legacy rowid tables, a non-integer PRIMARY KEY can be nullable.
    // INTEGER PRIMARY KEY without a PK index aliases the always-present rowid.
    if !indexes.iter().any(|(_, _, origin, _)| origin == "pk")
        && let Some(column) = columns
            .iter_mut()
            .find(|column| column.primary_key == 1 && column.kind.eq_ignore_ascii_case("integer"))
    {
        column.nullable = false;
    }
    let indexes = indexes
        .into_iter()
        .map(|(name, unique, origin, partial)| {
            let columns = bounded(
                connection,
                "SELECT CASE WHEN length(CAST(name AS BLOB)) <= 256 THEN name END
             FROM pragma_index_xinfo(?, 'main') WHERE key = 1 ORDER BY seqno LIMIT 17",
                [&name],
                |row| row.get::<_, Option<String>>(0),
                16,
            )?;
            if partial || columns.iter().any(Option::is_none) {
                return Err(unsupported());
            }
            Ok(Index {
                name,
                columns: columns.into_iter().flatten().collect(),
                unique,
                independent: origin == "c" && !unique,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Table {
        definition,
        columns,
        indexes,
    })
}

fn bounded<T>(
    connection: &Connection,
    sql: &str,
    params: impl Params,
    row: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    limit: usize,
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(sql).map_err(metadata_error)?;
    let rows = statement
        .query_map(params, row)
        .map_err(metadata_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(metadata_error)?;
    if rows.len() > limit {
        return Err(unsupported());
    }
    Ok(rows)
}

fn scalar(connection: &Connection, sql: &str, params: impl Params) -> Result<bool> {
    connection
        .query_row(sql, params, |row| row.get(0))
        .map_err(metadata_error)
}

fn metadata_error(error: rusqlite::Error) -> Error {
    crate::error::map_sqlite_error(error, "SQLite DDL 元数据检查失败")
}

fn unsupported() -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "无法验证 SQLite 对象结构或依赖",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_common::QueryId;
    use sqarvo_driver_api::{DriverSession, ExecuteRequest, IsolationLevel, TransactionOptions};

    #[tokio::test]
    async fn ddl_commit_can_complete_after_its_acknowledgment_receiver_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("schema.db");
        let observer = Connection::open(&path).unwrap();
        let session = crate::session::SqliteSession::connect_existing(
            path.to_str().unwrap().to_owned(),
            false,
        )
        .await
        .unwrap();
        let count = || {
            observer
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name = 'fixture'",
                    [],
                    |row| row.get::<_, u32>(0),
                )
                .unwrap()
        };
        let begin = TransactionOptions {
            isolation: IsolationLevel::Serializable,
            read_only: false,
        };
        let execute = || ExecuteRequest {
            query_id: QueryId::new(),
            sql: "CREATE TABLE main.fixture(id INTEGER)".into(),
            max_rows: Some(1),
            timeout: None,
        };
        let first = session.begin_transaction(begin).await.unwrap();
        assert!(matches!(
            session.dispatch_ddl(execute()).unwrap().await,
            sqarvo_driver_api::ddl::Acknowledgment::Confirmed
        ));
        assert_eq!(count(), 0);
        session.rollback_transaction(&first).await.unwrap();
        assert_eq!(count(), 0);
        assert!(matches!(
            session.dispatch_ddl_commit(first).unwrap().await,
            sqarvo_driver_api::ddl::Acknowledgment::Rejected(_)
        ));

        let second = session.begin_transaction(begin).await.unwrap();
        assert!(matches!(
            session.dispatch_ddl(execute()).unwrap().await,
            sqarvo_driver_api::ddl::Acknowledgment::Confirmed
        ));
        drop(session.dispatch_ddl_commit(second).unwrap());
        session.close().await.unwrap();
        assert_eq!(count(), 1);
    }

    #[tokio::test]
    async fn ddl_open_requires_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing.sqlite");
        for preview in [false, true] {
            let opened = crate::session::SqliteSession::connect_existing(
                path.to_str().unwrap().to_owned(),
                preview,
            )
            .await;
            assert!(opened.is_err());
            assert!(!path.exists());
        }
    }
}
