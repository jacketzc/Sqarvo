//! stdio MCP server 生命周期（#207 实现）。
//!
//! 骨架阶段只固定入口签名。#207 的实现约束（均有阶段 A spike 实证依据）：
//! - `rmcp::transport::stdio()` + `serve_with_ct`，SIGINT/SIGTERM 自接优雅关停
//!   （SDK 不装信号处理器，默认直接杀进程，无清理）；
//! - stdin EOF → 正常退出（exit 0）；
//! - 退出路径必须调用 `SessionManager::close_all()` 并回收 `QueryRegistry` 登记。

use std::path::Path;

use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};

/// 加载配置并运行 stdio MCP server，直到客户端断开或收到终止信号。
#[allow(clippy::unused_async)] // 骨架占位：真实实现在 #207 填入（serve_with_ct + 信号接线）。
pub async fn run_stdio(_config_path: Option<&Path>) -> Result<()> {
    Err(Error::new(
        ErrorCode::INTERNAL,
        ErrorCategory::Internal,
        "sqarvo-mcp 骨架：stdio server 实现待 #207 落地",
    ))
}
