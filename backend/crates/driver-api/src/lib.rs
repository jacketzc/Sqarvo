//! Sqarvo 驱动 SPI（Service Provider Interface）。
//!
//! 该 crate 是「通用业务层」与「具体数据库实现」之间的唯一契约：
//!
//! - 不含任何具体数据库协议、SQL 方言或连接池实现；
//! - 不把第三方驱动的类型泄露到公共接口；
//! - 用显式的能力描述符表达「不支持」，绝不静默模拟成功。
//!
//! 具体驱动 crate（例如将来的 `sqarvo-driver-postgres`）实现
//! [`Driver`] 与 [`DriverSession`]，由 `sqarvo-drivers` 中的注册表统一装载。

#![warn(missing_docs)]

pub mod capabilities;
pub mod driver;
pub mod metadata;
pub mod value;

pub use capabilities::Capabilities;
pub use driver::{
    ConnectionParams, ConnectionTestReport, Driver, DriverDescriptor, DriverId, DriverSession,
    ExecuteRequest, ResultBatch, TlsMode, Warning,
};
pub use metadata::{ObjectKind, ObjectNode, QualifiedName};
pub use value::{ColumnMeta, ColumnType, Decimal, Row, Value};
