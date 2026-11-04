//! Sqarvo gRPC 服务层。
//!
//! 该 crate 同时承担两个职责，但边界清晰：
//!
//! - [`services`]：把 gRPC 请求翻译为通用业务调用，并负责错误映射；
//! - [`transport`]：本地 IPC 监听与 token 认证，不包含任何业务逻辑。
//!
//! 生成的 Protobuf/gRPC 代码位于 [`proto`]，由 `build.rs` 在构建时生成。

#![warn(missing_docs)]

pub mod auth;
pub mod error;
pub mod proto;
pub mod services;
pub mod transport;

pub use services::Services;
pub use services::system::PROTOCOL_VERSION;
pub use transport::{LocalListener, serve};
