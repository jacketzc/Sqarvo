//! `mcp.json` 配置模型与启动期校验（#207 实现；V3.1 §3.1/§8）。
//!
//! 契约要点（改动确认单 #1 §4）：
//! - 默认落 `AppPaths::config_dir()` 下的 `mcp.json`（以 API 返回值为准，
//!   paths.rs 注释与 `ProjectDirs` 实际命名不一致，勿照抄注释），`--config` 覆盖；
//! - 字段映射 driver-api `ConnectionParams`（driver.rs:137-156，无密码字段）；
//! - 启动期驱动白名单预检：mariadb 仅 `ssl_ca`/`ssl_cert`/`ssl_key`/
//!   `connect_timeout_ms`，sqlite 仅 `read_only` 且禁 `username`/`credential_ref`/
//!   TLS/port≠0 组合；
//! - `alias`：`[a-z0-9_-]{1,64}`，全局唯一；
//! - `enabled_tools` 三态：缺失 = 全部启用；空数组 = 全部禁用（警告不拒启）；
//!   未知名 = 配置错误拒绝启动；
//! - `query_limits` 覆盖值一律钳制在硬上限内（200/1000/10s/1MiB/并发≤4）。
