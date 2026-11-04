//! 前端访问本地后端所需的错误类型。
//!
//! 所有错误文案都会直接展示在界面上，因此：
//!
//! - 必须可操作：告诉用户下一步该做什么，而不是只抛底层错误；
//! - **不得包含会话 token 或任何凭据**；路径、gRPC 状态码与后端自述文案可以出现，
//!   后端保证其错误消息不含凭据（见 `backend/crates/rpc-server/src/error.rs`）。

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// 前端与后端交互过程中的错误。
#[derive(Debug)]
pub enum BackendError {
    /// 端点描述文件存在但内容不可用（格式、字段或 token 形状非法）。
    EndpointInvalid { path: PathBuf, detail: String },
    /// 端点描述的协议主版本与客户端不一致。
    ProtocolIncompatible { client: String, backend: String },
    /// 协议版本字符串无法解析（例如缺少 `v` 前缀）。
    ProtocolVersionUnparsable { raw: String },
    /// 找不到后端可执行文件。
    BackendBinaryNotFound { searched: Vec<PathBuf> },
    /// 后端进程无法启动。
    BackendSpawnFailed { binary: PathBuf, detail: String },
    /// 后端进程在写出端点描述前就退出了。
    BackendExited { status: String },
    /// 等待端点描述文件超时。
    EndpointTimeout {
        path: PathBuf,
        waited: Duration,
        last_error: Option<String>,
    },
    /// 建立 gRPC 通道失败。
    ConnectFailed { transport: String, detail: String },
    /// gRPC 调用返回错误。
    Rpc { code: String, message: String },
    /// 关闭后端进程时未能确认其退出。
    CleanupFailed { detail: String },
    /// 运行环境不满足要求（例如无法确定用户运行时目录）。
    Runtime { detail: String },
    /// 操作因应用退出而取消。
    Cancelled,
}

impl BackendError {
    /// 用于「已断开连接」等单行场景的简短描述。
    #[must_use]
    pub fn short(&self) -> String {
        match self {
            Self::Rpc { code, message } => format!("后端返回错误 [{code}]：{message}"),
            Self::ConnectFailed { detail, .. } => detail.clone(),
            other => other
                .to_string()
                .lines()
                .next()
                .unwrap_or("未知错误")
                .to_owned(),
        }
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndpointInvalid { path, detail } => write!(
                formatter,
                "端点描述文件不可用：{}\n{detail}\n如果后端已退出，删除该文件后重试；也可能是旧版本后端遗留的文件。",
                path.display()
            ),
            Self::ProtocolIncompatible { client, backend } => write!(
                formatter,
                "协议版本不兼容：后端是 {backend}，客户端支持 {client} 主版本。\n\
                 请把 Sqarvo 前端与后端升级/降级到同一主版本后重启应用。"
            ),
            Self::ProtocolVersionUnparsable { raw } => write!(
                formatter,
                "端点描述的 protocol_version（{raw}）无法解析，期望形如 v1 的版本号。\n\
                 该文件可能来自不兼容的后端版本，请升级 Sqarvo 或删除后重试。"
            ),
            Self::BackendBinaryNotFound { searched } => {
                let list = searched
                    .iter()
                    .map(|path| format!("  {}", path.display()))
                    .collect::<Vec<_>>()
                    .join("\n");
                write!(
                    formatter,
                    "找不到后端可执行文件 sqarvo-backend。已尝试：\n{list}\n\
                     请设置环境变量 SQARVO_BACKEND_BIN 指向该可执行文件，\
                     或在 backend/ 下执行 `cargo build -p sqarvo-backend-service` 后再启动前端。"
                )
            }
            Self::BackendSpawnFailed { binary, detail } => write!(
                formatter,
                "无法启动后端进程 {}：{detail}\n\
                 请确认该文件可执行，或设置 SQARVO_BACKEND_BIN 指向正确的二进制。",
                binary.display()
            ),
            Self::BackendExited { status } => write!(
                formatter,
                "后端进程在完成握手前退出（{status}）。\n\
                 请查看启动前端的终端中后端输出的日志；\
                 若刚重新构建后端，请确认 SQARVO_BACKEND_BIN 指向可运行的二进制。"
            ),
            Self::EndpointTimeout {
                path,
                waited,
                last_error,
            } => {
                let last = last_error
                    .as_deref()
                    .map(|detail| format!("\n最近一次读取错误：{detail}"))
                    .unwrap_or_default();
                write!(
                    formatter,
                    "等待端点描述文件超时（{} 秒）：{}{last}\n\
                     后端可能启动失败或无权写入运行时目录；请查看终端中的后端日志。",
                    waited.as_secs(),
                    path.display()
                )
            }
            Self::ConnectFailed { transport, detail } => write!(
                formatter,
                "无法建立到后端的 {transport} 连接：{detail}\n\
                 后端可能已退出或 socket 权限不足；请重启应用重试。"
            ),
            Self::Rpc { code, message } => {
                write!(formatter, "后端返回错误 [{code}]：{message}")
            }
            Self::CleanupFailed { detail } => write!(
                formatter,
                "关闭后端进程时清理未完成：{detail}\n\
                 可能有残留的 socket 或端点文件，请手动检查运行时目录。"
            ),
            Self::Runtime { detail } => write!(formatter, "运行环境不可用：{detail}"),
            Self::Cancelled => write!(formatter, "操作已取消"),
        }
    }
}

impl std::error::Error for BackendError {}
