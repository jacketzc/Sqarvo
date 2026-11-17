//! `sqarvo_common::Error` → MCP 工具错误的映射（#207 实现；V3.1 §11）。
//!
//! 分类与 `mcp.*` / 既有子码复用现有体系（common 的 `mcp_error_codes_are_stable_and_unique`
//! 已锁定文本），本模块只做映射与 `isError` 语义，**不得另立错误码目录**；
//! 映射表可参照 rpc-server `error.rs` 的 category→code 处理，但不依赖它。
//! 错误消息面向 Agent：明确、可操作、不含凭据与敏感连接串。
