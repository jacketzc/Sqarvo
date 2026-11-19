//! Sqarvo MCP Server V1：独立 stdio MCP 进程。
//!
//! 本包是**新增的程序入口**，不是第二套数据库系统：连接、驱动、查询生命周期与
//! 凭据全部复用现有 crates（改动确认单 #1 §2；任务书 V3.1 §2）。它不依赖
//! `sqarvo-rpc-server`，也不得把 prost / tonic 生成类型带进工具接口（V3.1 §2.2）。
//!
//! 模块与 Issue 对应（并行泳道的文件所有权边界）：
//! - `config` / `access` / `audit` / `credentials_cmd` / `server` / `error` → #207
//! - `policy` → #208。**`policy` 公开的函数与类型签名是跨泳道契约**，#208 只替换实现，
//!   不改签名；#209/#210 按签名消费。
//! - `tools` → #209，`resources` → #210，协议 E2E（tests/）→ #211，文档 → #212。

#![warn(missing_docs)]

pub mod access;
pub mod audit;
pub mod config;
pub mod credentials_cmd;
pub mod error;
pub mod policy;
pub mod resources;
pub mod server;
pub mod tools;

#[cfg(test)]
extern crate self as sqarvo_mcp;

#[cfg(test)]
#[path = "../tests/support/profile_access.rs"]
mod profile_access;
