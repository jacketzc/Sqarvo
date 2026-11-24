//! Database target and predicate checks performed inside a write transaction.

use std::collections::BTreeSet;

use sqarvo_common::{Error, ErrorCode, QueryId, Result};
use sqarvo_connection::SessionHandle;
use sqarvo_driver_api::{Filter, ParameterizedExecuteRequest, Value};

use crate::policy::write::{WriteAction, WritePlan, reference_predicate};

/// Scope observed in the inspection snapshot, before any write.
#[derive(Clone, Copy)]
pub enum WriteScope {
    /// A bounded candidate count or proposed INSERT input count.
    Bounded(u64),
    /// The bounded probe found at least this many matches.
    OverLimit(u64),
    /// Matching rows cover the entire target.
    EntireTable(u64),
}

impl WriteScope {
    /// Enforce predicate scope before the write is sent to the driver.
    pub fn allowed_rows(self) -> Result<u64> {
        match self {
            Self::Bounded(count) => Ok(count),
            Self::OverLimit(_) => Err(Error::resource_exhausted(
                ErrorCode::MCP_INVALID_ARGUMENT,
                "The write conditions match more rows than allowed",
            )),
            Self::EntireTable(_) => Err(Error::failed_precondition(
                ErrorCode::MCP_INVALID_ARGUMENT,
                "The write conditions match the entire target table",
            )),
        }
    }
}

/// Verify a SQLite target while its write transaction holds the database write reservation.
///
/// Only ordinary tables in `main` are eligible. Views, virtual and shadow tables, triggers,
/// and related foreign keys can introduce writes outside the DML row count.
pub async fn sqlite(
    handle: &SessionHandle,
    plan: &WritePlan,
    query_id: QueryId,
) -> Result<WriteScope> {
    if plan.database != "main" || plan.table.to_ascii_lowercase().starts_with("sqlite_") {
        return Err(ineligible());
    }
    let eligible = scalar(
        handle,
        query_id,
        "SELECT EXISTS (SELECT 1 FROM pragma_table_list WHERE
             schema = 'main' AND type = 'table' AND name COLLATE NOCASE = ?)
         AND NOT EXISTS (SELECT 1 FROM main.sqlite_schema
             WHERE type = 'trigger' AND tbl_name COLLATE NOCASE = ?)
         AND NOT EXISTS (
             SELECT 1 FROM pragma_table_list AS t
             JOIN pragma_foreign_key_list(t.name, 'main') AS fk
             WHERE t.schema = 'main' AND t.type = 'table'
               AND (t.name COLLATE NOCASE = ? OR fk.\"table\" COLLATE NOCASE = ?)
         )",
        vec![Value::Text(plan.table.clone()); 4],
    )
    .await?;
    if eligible != 1 {
        return Err(ineligible());
    }

    for column in referenced_columns(&plan.action) {
        let exists = scalar(
            handle,
            query_id,
            "SELECT EXISTS (SELECT 1 FROM pragma_table_xinfo(?, 'main')
                            WHERE name COLLATE NOCASE = ? AND hidden = 0)",
            vec![
                Value::Text(plan.table.clone()),
                Value::Text(column.to_owned()),
            ],
        )
        .await?;
        if exists != 1 {
            return Err(Error::invalid_argument(
                ErrorCode::MCP_INVALID_ARGUMENT,
                "The specified columns are unavailable in the target table",
            ));
        }
    }

    match &plan.action {
        WriteAction::Insert(rows) => Ok(WriteScope::Bounded(rows.len() as u64)),
        WriteAction::Update { filter, .. } | WriteAction::Delete(filter) => {
            predicate_scope(handle, plan, filter, query_id).await
        }
    }
}

/// Verify a MariaDB target on an instance with DBA-managed DDL isolation.
///
/// The caller must require explicit opt-in for the current profile revision. The DBA
/// eligibility view must account for triggers and all related foreign keys, including
/// references from schemas that are invisible to the DML account.
pub async fn mariadb(
    handle: &SessionHandle,
    plan: &WritePlan,
    query_id: QueryId,
) -> Result<WriteScope> {
    if !mariadb_target_segment(&plan.database) || !mariadb_target_segment(&plan.table) {
        return Err(ineligible());
    }
    let target = || {
        vec![
            Value::Text(plan.database.clone()),
            Value::Text(plan.table.clone()),
        ]
    };
    let eligible = scalar(
        handle,
        query_id,
        "SELECT count(*) FROM sqarvo_guard.write_eligibility
         WHERE BINARY schema_name = ? AND BINARY table_name = ? AND eligible = 1",
        target(),
    )
    .await?;
    if eligible != 1 {
        return Err(ineligible());
    }
    let base_table = scalar(
        handle,
        query_id,
        "SELECT count(*) FROM information_schema.TABLES
         WHERE BINARY TABLE_SCHEMA = ? AND BINARY TABLE_NAME = ?
         AND TABLE_TYPE = 'BASE TABLE' AND ENGINE = 'InnoDB'",
        target(),
    )
    .await?;
    if base_table != 1 {
        return Err(ineligible());
    }
    for column in referenced_columns(&plan.action) {
        let mut params = target();
        params.push(Value::Text(column.to_owned()));
        if scalar(
            handle,
            query_id,
            "SELECT count(*) FROM information_schema.COLUMNS
             WHERE BINARY TABLE_SCHEMA = ? AND BINARY TABLE_NAME = ?
             AND BINARY COLUMN_NAME = ? AND IS_GENERATED = 'NEVER'",
            params,
        )
        .await?
            != 1
        {
            return Err(Error::invalid_argument(
                ErrorCode::MCP_INVALID_ARGUMENT,
                "The specified columns are unavailable in the target table",
            ));
        }
    }
    match &plan.action {
        WriteAction::Insert(rows) => Ok(WriteScope::Bounded(rows.len() as u64)),
        WriteAction::Update { filter, .. } | WriteAction::Delete(filter) => {
            predicate_scope(handle, plan, filter, query_id).await
        }
    }
}

pub(crate) fn mariadb_target_segment(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn referenced_columns(action: &WriteAction) -> BTreeSet<&str> {
    let mut columns = BTreeSet::new();
    match action {
        WriteAction::Insert(rows) => {
            columns.extend(
                rows.iter()
                    .flat_map(|row| row.values.iter().map(|value| value.column.as_str())),
            );
        }
        WriteAction::Update { changes, filter } => {
            columns.extend(changes.iter().map(|change| change.column.as_str()));
            columns.extend(
                filter
                    .conditions
                    .iter()
                    .map(|condition| condition.column.as_str()),
            );
        }
        WriteAction::Delete(filter) => {
            columns.extend(
                filter
                    .conditions
                    .iter()
                    .map(|condition| condition.column.as_str()),
            );
        }
    }
    columns
}

async fn predicate_scope(
    handle: &SessionHandle,
    plan: &WritePlan,
    filter: &Filter,
    query_id: QueryId,
) -> Result<WriteScope> {
    let quote = |name: &str| handle.quote_identifier(name).unwrap_or_default();
    let target = format!("{}.{}", quote(&plan.database), quote(&plan.table));
    let mut params = Vec::new();
    let predicate = reference_predicate(filter, &quote, &mut params)?;
    let params: Vec<Value> = params.into_iter().cloned().collect();
    let cap = plan.action.max_rows() as u64;
    let mut bounded = params.clone();
    bounded.push(Value::UInt(cap + 1));
    let matched = scalar(
        handle, query_id,
        &format!("SELECT count(*) FROM (SELECT 1 FROM {target} WHERE {predicate} LIMIT ?) AS matched_rows"),
        bounded,
    )
    .await?;
    if matched > cap {
        return Ok(WriteScope::OverLimit(matched));
    }
    if matched > 0
        && scalar(
            handle,
            query_id,
            &format!("SELECT EXISTS (SELECT 1 FROM {target} WHERE ({predicate}) IS NOT TRUE)"),
            params,
        )
        .await?
            == 0
    {
        return Ok(WriteScope::EntireTable(matched));
    }
    Ok(WriteScope::Bounded(matched))
}

async fn scalar(
    handle: &SessionHandle,
    query_id: QueryId,
    sql: &str,
    params: Vec<Value>,
) -> Result<u64> {
    let result = handle
        .execute_parameterized(ParameterizedExecuteRequest {
            query_id,
            sql: sql.to_owned(),
            params,
            max_rows: Some(1),
            timeout: None,
        })
        .await?;
    if let Some(batch) = result.first_batch
        && !batch.has_more
        && let [row] = batch.rows.as_slice()
        && let [cell] = row.as_slice()
        && !cell.truncated
    {
        return match cell.value {
            Value::UInt(value) => Ok(value),
            Value::Int(value) => u64::try_from(value).map_err(|_| ineligible()),
            _ => Err(ineligible()),
        };
    }
    let _ = handle.close_query(&query_id).await;
    Err(ineligible())
}

fn ineligible() -> Error {
    Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "Could not verify that the target table meets controlled write requirements",
    )
}
