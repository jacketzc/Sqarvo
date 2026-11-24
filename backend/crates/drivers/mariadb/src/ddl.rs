//! MariaDB rendering rules for structured schema changes.

use sqarvo_driver_api::ddl::{ColumnType, IndexScope, Syntax};

pub(crate) fn syntax() -> Syntax {
    Syntax {
        delimiter: '`',
        index_scope: IndexScope::Table,
        table_suffix: " ENGINE=InnoDB",
        inline_identity: false,
        quote: crate::table_data::quote_identifier,
        column_type,
        identity: |kind| {
            matches!(kind, ColumnType::Integer | ColumnType::Bigint).then_some(" AUTO_INCREMENT")
        },
    }
}

fn column_type(kind: &ColumnType) -> String {
    match kind {
        ColumnType::Integer => "INT".into(),
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

use mysql_async::{Conn, prelude::Queryable};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::ddl::{
    Acknowledgment, Action, Change, Index, Inspection, Table, TableColumn,
};

use crate::error::map_error;
use crate::metadata::quote_identifier;

const SQL_MODE: &str = "STRICT_ALL_TABLES,NO_ENGINE_SUBSTITUTION,NO_ZERO_DATE,NO_ZERO_IN_DATE";
const LOCK: &str = "sqarvo_managed_ddl_v1";

pub(crate) async fn configure(conn: &mut Conn) -> Result<()> {
    let server: Option<(String, String, u8)> = conn
        .query_first("SELECT VERSION(), @@global.init_connect, @@in_transaction")
        .await
        .map_err(metadata_error)?;
    if !server.is_some_and(|(version, init, transaction)| {
        version.starts_with("11.8.6-MariaDB") && init.is_empty() && transaction == 0
    }) {
        return Err(unsupported());
    }
    conn.query_drop("SET NAMES utf8mb4")
        .await
        .map_err(metadata_error)?;
    conn.exec_drop("SET SESSION sql_mode = ?", (SQL_MODE,))
        .await
        .map_err(metadata_error)?;
    conn.query_drop(
        "SET SESSION lock_wait_timeout = 5, innodb_lock_wait_timeout = 5,
        explicit_defaults_for_timestamp = 1, foreign_key_checks = 1, autocommit = 1",
    )
    .await
    .map_err(metadata_error)?;
    let settings: Option<(String, u8, u8, u8, u8)> = conn
        .query_first(
            "SELECT @@sql_mode, @@explicit_defaults_for_timestamp, @@foreign_key_checks,
         @@lower_case_table_names, @@autocommit",
        )
        .await
        .map_err(metadata_error)?;
    if !settings.is_some_and(|(mode, timestamp, keys, case, autocommit)| {
        mode.split(',').count() == SQL_MODE.split(',').count()
            && SQL_MODE
                .split(',')
                .all(|setting| mode.split(',').any(|value| value == setting))
            && (timestamp, keys, case, autocommit) == (1, 1, 0, 1)
    }) {
        return Err(unsupported());
    }
    Ok(())
}

pub(crate) async fn inspect(conn: &mut Conn, change: &Change, preview: bool) -> Result<Inspection> {
    let current: Option<Option<String>> = conn
        .query_first("SELECT DATABASE()")
        .await
        .map_err(metadata_error)?;
    if current.flatten().as_deref() != Some(change.database.as_str()) {
        return Err(unsupported());
    }
    if !preview {
        let locked: Option<Option<u8>> = conn
            .exec_first("SELECT GET_LOCK(?, 0)", (LOCK,))
            .await
            .map_err(metadata_error)?;
        if locked.flatten() != Some(1) {
            return Err(Error::failed_precondition(
                ErrorCode::MCP_DDL_BUSY,
                "This MariaDB instance already has a schema change in progress",
            ));
        }
    }

    // Both views are installed by the DBA. The eligibility view checks the
    // entire instance's dependency boundary using a privileged locked definer.
    let eligible: Option<u64> = conn
        .exec_first(
            "SELECT COUNT(*) FROM sqarvo_guard.ddl_eligibility
           WHERE BINARY schema_name = ? AND policy_version = 1 AND eligible = 1",
            (&change.database,),
        )
        .await
        .map_err(|_| unsupported())?;
    if eligible != Some(1) {
        return Err(unsupported());
    }
    let table = object(conn, &change.database, &change.table).await?;
    let mut destination_exists = match &change.action {
        Action::RenameTable { new_table } => {
            object(conn, &change.database, new_table).await?.is_some()
        }
        _ => table.is_some(),
    };
    let table = match table {
        Some((kind, Some(engine))) if kind == "BASE TABLE" && engine == "InnoDB" => {
            let table = table_metadata(conn, &change.database, &change.table).await?;
            if let Action::CreateIndex { index, .. } = &change.action {
                destination_exists = table
                    .indexes
                    .iter()
                    .any(|item| item.name.eq_ignore_ascii_case(index));
            }
            Some(table)
        }
        Some(_) => return Err(unsupported()),
        None => None,
    };
    Ok(Inspection {
        table,
        destination_exists,
        dependencies_known: true,
    })
}

async fn object(
    conn: &mut Conn,
    catalog: &str,
    name: &str,
) -> Result<Option<(String, Option<String>)>> {
    let mut objects: Vec<(String, Option<String>)> = conn
        .exec(
            "SELECT object_type, engine FROM sqarvo_guard.ddl_objects
         WHERE BINARY schema_name = ? AND BINARY object_name = ? LIMIT 2",
            (catalog, name),
        )
        .await
        .map_err(|_| unsupported())?;
    if objects.len() > 1 {
        return Err(unsupported());
    }
    Ok(objects.pop())
}

async fn table_metadata(conn: &mut Conn, catalog: &str, name: &str) -> Result<Table> {
    let partitioned: Option<u64> = conn
        .exec_first(
            "SELECT COUNT(*) FROM information_schema.PARTITIONS
         WHERE BINARY TABLE_SCHEMA = ? AND BINARY TABLE_NAME = ? AND PARTITION_NAME IS NOT NULL",
            (catalog, name),
        )
        .await
        .map_err(metadata_error)?;
    if partitioned != Some(0) {
        return Err(unsupported());
    }
    let command = format!(
        "SHOW CREATE TABLE {}.{}",
        quote_identifier(catalog),
        quote_identifier(name)
    );
    let definition: Option<(String, String)> =
        conn.query_first(command).await.map_err(metadata_error)?;
    let (_, definition) = definition
        .filter(|(_, sql)| sql.len() <= 32_768)
        .ok_or_else(unsupported)?;
    let raw: Vec<(String, String, String, String)> = conn
        .exec(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, IS_GENERATED
         FROM information_schema.COLUMNS WHERE BINARY TABLE_SCHEMA = ? AND BINARY TABLE_NAME = ?
         ORDER BY ORDINAL_POSITION LIMIT 65",
            (catalog, name),
        )
        .await
        .map_err(metadata_error)?;
    if raw.is_empty()
        || raw.len() > 64
        || raw.iter().any(|(_, _, _, generated)| generated != "NEVER")
    {
        return Err(unsupported());
    }
    let mut columns: Vec<_> = raw
        .into_iter()
        .map(|(name, kind, nullable, _)| TableColumn {
            name,
            kind,
            nullable: nullable == "YES",
            primary_key: 0,
        })
        .collect();
    let indexes = indexes(conn, catalog, name, &mut columns).await?;
    Ok(Table {
        definition,
        columns,
        indexes,
    })
}

async fn indexes(
    conn: &mut Conn,
    catalog: &str,
    name: &str,
    columns: &mut [TableColumn],
) -> Result<Vec<Index>> {
    type Entry = (String, u32, u8, Option<String>, Option<u64>, String);
    let entries: Vec<Entry> = conn
        .exec(
            "SELECT INDEX_NAME, SEQ_IN_INDEX, NON_UNIQUE, COLUMN_NAME, SUB_PART, INDEX_TYPE
         FROM information_schema.STATISTICS WHERE BINARY TABLE_SCHEMA = ? AND BINARY TABLE_NAME = ?
         ORDER BY INDEX_NAME, SEQ_IN_INDEX LIMIT 1025",
            (catalog, name),
        )
        .await
        .map_err(metadata_error)?;
    if entries.len() > 1024 {
        return Err(unsupported());
    }
    let mut indexes: Vec<Index> = Vec::new();
    for (index, position, non_unique, column, prefix, kind) in entries {
        let column = column.ok_or_else(unsupported)?;
        if kind != "BTREE" || prefix.is_some() || !(1..=16).contains(&position) {
            return Err(unsupported());
        }
        if index == "PRIMARY" {
            columns
                .iter_mut()
                .find(|entry| entry.name == column)
                .ok_or_else(unsupported)?
                .primary_key = position;
        }
        if position == 1 {
            indexes.push(Index {
                name: index.clone(),
                columns: Vec::new(),
                unique: non_unique == 0,
                independent: non_unique == 1 && index != "PRIMARY",
            });
        }
        let item = indexes
            .last_mut()
            .filter(|entry| entry.name == index)
            .ok_or_else(unsupported)?;
        item.columns.push(column);
    }
    if indexes.len() > 64 {
        return Err(unsupported());
    }
    Ok(indexes)
}

pub(crate) async fn execute(conn: &mut Conn, sql: &str) -> Acknowledgment {
    match conn.query_drop(sql).await {
        Ok(()) => Acknowledgment::Confirmed,
        Err(error @ mysql_async::Error::Server(_))
            if !crate::error::is_query_interrupted(&error) =>
        {
            Acknowledgment::Rejected(map_error(error, "MariaDB rejected the schema change"))
        }
        Err(error) => Acknowledgment::Unconfirmed(map_error(
            error,
            "MariaDB did not return a schema change response",
        )),
    }
}

fn metadata_error(error: mysql_async::Error) -> Error {
    map_error(error, "MariaDB DDL metadata check failed")
}

pub(crate) fn unsupported() -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "Could not verify the MariaDB DDL deployment, object, or dependencies",
    )
}
