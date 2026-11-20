//! Typed schema changes supported by the MCP DDL policy.

use serde::{Deserialize, Serialize};

/// One change in an explicitly identified catalog.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Change {
    /// Catalog containing the target.
    pub database: String,
    /// Target table.
    pub table: String,
    /// Single schema mutation.
    pub action: Action,
}

/// Closed set of supported schema changes.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)]
pub enum Action {
    CreateTable {
        columns: Vec<Column>,
        primary_key: Vec<String>,
        unique_constraints: Vec<Vec<String>>,
        if_not_exists: bool,
    },
    AddColumn {
        column: Column,
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
        if_exists: bool,
    },
    CreateIndex {
        index: String,
        columns: Vec<String>,
        unique: bool,
    },
    DropIndex {
        index: String,
    },
}

/// Normalized column definition. Table-level primary and unique constraints
/// are represented separately on the create action.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Column {
    /// Column name.
    pub name: String,
    /// Logical type including applicable dimensions.
    pub kind: ColumnType,
    /// Whether the column accepts NULL.
    pub nullable: bool,
    /// Constant default, when supplied.
    pub default: Option<Literal>,
    /// Driver-native integer identity requested on a single-column primary key.
    pub identity: bool,
}

/// Logical types in the DDL compiler's initial allowlist.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum ColumnType {
    Integer,
    Bigint,
    Varchar(u16),
    Text,
    Decimal { precision: u8, scale: u8 },
    Boolean,
    Date,
    Datetime,
    Timestamp,
    Blob,
}

/// Constant defaults only. These values must never enter audit records.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum Literal {
    Null,
    Boolean(bool),
    Integer(i64),
    Decimal(String),
    Text(String),
}

/// Placement of the catalog qualifier for create and drop index statements.
#[derive(Clone, Copy)]
pub enum IndexScope {
    /// Index names belong to a catalog.
    Catalog,
    /// Index names belong to a table.
    Table,
}

/// Driver-selected grammar and rendering rules for simple schema changes.
///
/// The caller must verify schema dependencies and validate the generated SQL
/// before dispatch. Any required session settings are the driver's responsibility.
pub struct Syntax {
    /// Identifier delimiter, used by the compiler to verify quoting.
    pub delimiter: char,
    /// Whether index names share a catalog or a table namespace.
    pub index_scope: IndexScope,
    /// Table suffix chosen by the driver.
    pub table_suffix: &'static str,
    /// Whether identity is rendered as inline primary key.
    pub inline_identity: bool,
    /// Identifier quoting used by the driver.
    pub quote: fn(&str) -> String,
    /// Logical column type mapping.
    pub column_type: fn(&ColumnType) -> String,
    /// Driver's identity declaration.
    pub identity: fn(&ColumnType) -> Option<&'static str>,
}

/// Result of a mutation after it has been accepted by the driver worker.
pub enum Acknowledgment {
    /// The database acknowledged the statement.
    Confirmed,
    /// The database explicitly returned an error for the statement.
    Rejected(sqarvo_common::Error),
    /// The final response is unavailable.
    Unconfirmed(sqarvo_common::Error),
}

/// A dispatched command's reply. Dropping it does not cancel the command.
pub type Dispatch = std::pin::Pin<Box<dyn std::future::Future<Output = Acknowledgment> + Send>>;

/// Bounded metadata from the session which will execute the mutation.
pub struct Inspection {
    /// Table metadata, absent if the target name is unused.
    pub table: Option<Table>,
    /// Whether the destination table or index name is occupied.
    pub destination_exists: bool,
    /// Whether the driver can establish the dependency boundary.
    pub dependencies_known: bool,
}

/// Metadata used by the DDL policy. Definition text is for internal vetting only.
pub struct Table {
    /// Original CREATE statement.
    pub definition: String,
    /// Table columns in declaration order.
    pub columns: Vec<TableColumn>,
    /// Table indexes.
    pub indexes: Vec<Index>,
}

/// Column metadata for a schema change.
pub struct TableColumn {
    /// Database column name.
    pub name: String,
    /// Declared database type.
    pub kind: String,
    /// Whether the column accepts NULL.
    pub nullable: bool,
    /// One-based key position, or zero for a non-key column.
    pub primary_key: u32,
}

/// Index metadata for a schema change.
pub struct Index {
    /// Database index name.
    pub name: String,
    /// Column sequence.
    pub columns: Vec<String>,
    /// Whether the index enforces uniqueness.
    pub unique: bool,
    /// Whether this is a plain, independent index.
    pub independent: bool,
}

impl Action {
    /// Stable operation name used in preview, execution, and audit.
    #[must_use]
    pub const fn operation(&self) -> &'static str {
        match self {
            Self::CreateTable { .. } => "create_table",
            Self::AddColumn { .. } => "add_column",
            Self::DropColumn { .. } => "drop_column",
            Self::RenameColumn { .. } => "rename_column",
            Self::RenameTable { .. } => "rename_table",
            Self::DropTable { .. } => "drop_table",
            Self::CreateIndex { .. } => "create_index",
            Self::DropIndex { .. } => "drop_index",
        }
    }

    /// Index affected by the action, if any.
    #[must_use]
    pub fn index(&self) -> Option<&str> {
        match self {
            Self::CreateIndex { index, .. } | Self::DropIndex { index } => Some(index),
            _ => None,
        }
    }
}
