//! 本地存储：路径解析、IPC 端点描述、会话 token、凭据引用。
//!
//! 安全边界：
//!
//! - 数据库密码永远不进入这里。这里只保存「凭据引用」，真实凭据由操作系统
//!   凭据存储（Windows Credential Manager / macOS Keychain / Linux Secret
//!   Service）保管，见 [`credentials`]；
//! - 端点描述文件包含会话 token，因此必须以当前用户独占权限（Unix 0600）
//!   写入，且位于用户私有运行目录；
//! - 日志、错误信息与配置中都不允许出现 token 原文。

#![warn(missing_docs)]

pub mod credentials;
pub mod endpoint;
pub mod paths;
pub mod token;

#[cfg(target_os = "linux")]
pub use credentials::SecretServiceCredentialStore;
pub use credentials::{
    CredentialStore, MemoryCredentialStore, Secret, UnavailableCredentialStore,
    open_credential_store, open_default_credential_store, validate_reference,
};
pub use endpoint::{EndpointInfo, TransportKind, read_endpoint_file, write_endpoint_file};
pub use paths::AppPaths;
pub use paths::restrict_to_owner;
pub use token::AuthToken;
