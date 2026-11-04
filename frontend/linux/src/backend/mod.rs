//! 与本地后端进程交互的封装。
//!
//! 分层：
//!
//! - [`endpoint`]：端点描述文件的定位、解析与协议版本校验；
//! - [`client`]：gRPC Channel、token 认证与 `SystemService` 调用；
//! - [`connection`]：连接草稿、`CredentialService`/`ConnectionService` 调用与错误分类；
//! - [`supervisor`]：后端可执行文件查找、启动、等待就绪与退出清理；
//! - [`worker`]：后台线程与消息通道，UI 只与本层交互；
//! - [`error`]：可直接展示给用户的错误文案（绝不含 token）。
//!
//! UI 层不得直接调用 socket、spawn 进程或读写端点文件，一律经 [`worker`]。

pub mod client;
pub mod connection;
pub mod endpoint;
pub mod error;
pub mod query;
pub mod supervisor;
pub mod worker;
