//! MCP 工具集（#209 实现；V3.1 §5/§6）。
//!
//! 工具清单与描述口径见 Issue #209。约定：
//! - 所有工具先过 access 授权与 `enabled_tools` 检查，再过 policy（SQL 类）；
//! - 参数类型规避 `u32` / `Option` 在 schemars 下的非标准 schema 形态
//!   （阶段 A 实测：`format:"uint32"`、`type:[T,"null"]` 有可移植性警告）；
//! - 行级截断（策略行数/字节上限）与单元格级截断（`Cell.truncated`，ADR 0007）
//!   是**两个独立标记**，不得压成一个布尔。
