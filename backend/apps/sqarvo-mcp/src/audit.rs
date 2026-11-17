//! 调用审计（#207 实现；V3.1 §12）。
//!
//! 契约要点：
//! - `AppPaths::data_dir()` 下 `mcp/audit.jsonl`，0600，记录级落盘策略与轮转
//!   （10 MiB × 5，只在完整记录边界轮转）；
//! - 每行 JSON 字段：`timestamp` / `request_id` / `client_info`（仅协议实际提供
//!   且可验证时）/ `tool_name` / `connection_alias` / `database` /
//!   `sql_fingerprint`（见 [`crate::policy::SqlFingerprint`]）/
//!   `execution_time_ms` / `returned_rows` / `truncated` / `success` /
//!   `error_code` / `error_subcode`；
//! - 默认不记录：完整 SQL、查询结果、密码、Token、`credential_ref` 指向的秘密；
//! - 多实例共享文件为 best-effort：flock 争用退化 `audit.<pid>.jsonl`，
//!   已知限制写入架构文档。
