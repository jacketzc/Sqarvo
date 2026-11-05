//! Sqarvo 驱动 SPI（Service Provider Interface）。
//!
//! 该 crate 是「通用业务层」与「具体数据库实现」之间的唯一契约：
//!
//! - 不含任何具体数据库协议、SQL 方言或连接池实现；
//! - 不把第三方驱动的类型泄露到公共接口；
//! - 用显式的能力描述符表达「不支持」，绝不静默模拟成功；
//! - 结果集以「语句摘要 + 分批行」表达，游标由驱动持有（ADR 0005）。
//!
//! 具体驱动 crate（例如将来的 `sqarvo-driver-postgres`）实现
//! [`Driver`] 与 [`DriverSession`]，由 `sqarvo-drivers` 中的注册表统一装载。

#![warn(missing_docs)]

pub mod capabilities;
pub mod credential;
pub mod driver;
pub mod metadata;
pub mod table_data;
pub mod value;

pub use capabilities::Capabilities;
pub use credential::Secret;
pub use driver::{
    ConnectionParams, ConnectionTestReport, DatabaseFlavor, Driver, DriverDescriptor, DriverId,
    DriverSession, ExecuteRequest, ExecutionStart, IsolationLevel, MAX_SQL_PREVIEW_CHARS,
    ParameterizedExecuteRequest, ResultBatch, StatementSummary, TlsMode, TransactionOptions,
    Warning, sql_preview,
};
pub use metadata::{
    DEFAULT_PAGE_SIZE, Ddl, MAX_PAGE_SIZE, MetadataProvider, ObjectKind, ObjectNode, ObjectRef,
    Page, PageRequest, QualifiedName,
};
pub use table_data::{
    ChangeOutcome, ColumnChange, DEFAULT_RISK_CONFIRMATION_ROWS, MAX_ROW_CHANGES_PER_BATCH,
    PrimaryKey, PrimaryKeyColumn, ReadOnlyReason, RowChange, RowChangeReport, RowChangeRequest,
    RowChangeResult, RowKey, RowOrder, TableDataProvider, TablePage,
};
pub use value::{
    Cell, ColumnMeta, ColumnType, DEFAULT_MAX_CELL_BYTES, Decimal, Row, Value, truncate_text,
};
