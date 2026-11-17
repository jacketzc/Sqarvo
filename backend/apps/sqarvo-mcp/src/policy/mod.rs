//! SQL 只读策略与指纹（#208）。
//!
//! **本文件公开的签名是跨泳道契约**（见 `lib.rs` 模块表）：#208 用
//! `sqlparser`（`MySqlDialect`，fail-closed）替换实现，签名不得变更；
//! #209 的 `execute_readonly_query` / `explain_query` 按此消费。
//!
//! 策略要点（任务书 V3.1 §8，spike 实证见 scratch/mcp-spike-sqlparser/report.md）：
//! - 仅放行单条 `SELECT` / 只读 `WITH`；解析失败、多语句、未知形态一律拒绝；
//! - visitor 全树遍历覆盖嵌套 DML（数据修改 CTE）、`Query.locks`、`Select.into`
//!   （一切 INTO 形式）、`optimizer_hints`、危险函数表；
//! - 审计只存 [`SqlFingerprint`]（字面量脱敏 + SHA-256），不存原始 SQL。

use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};

/// 规范化 SQL 指纹：审计与错误回显里唯一允许出现的 SQL 形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlFingerprint {
    /// 字面量替换为占位符后的规范化 SQL（AST unparse 输出，幂等）。
    pub normalized: String,
    /// `normalized` 的 SHA-256 小写十六进制。
    pub sha256_hex: String,
}

/// 一条通过只读策略、可以安全下发的语句。
#[derive(Debug, Clone)]
pub struct CheckedSql {
    /// 原始 SQL 文本（策略只验证、不改写，保留用户格式）。
    pub sql: String,
    /// 语句指纹（审计用）。
    pub fingerprint: SqlFingerprint,
}

/// 检查 `sql` 是否为策略允许的**单条只读语句**（V3.1 §8）。
///
/// 错误语义：解析失败 → `mcp.sql_parse`（fail-closed）；白名单外语句 / INTO /
/// 锁读 / 危险函数 → `mcp.readonly_violation`；多语句 → `mcp.readonly_violation`。
#[allow(unused_variables)] // 骨架占位：#208 实现。
pub fn check_readonly(sql: &str) -> Result<CheckedSql> {
    Err(pending())
}

/// 检查 `sql` 并构造 EXPLAIN 语句（V3.1 §6）：先走与 [`check_readonly`] 完全相同的
/// 策略，再由本模块拼 `EXPLAIN ` 前缀——用户直接提交的 `EXPLAIN` / `ANALYZE`
/// 形态在策略层就应被拒，不存在第二条路。返回的 [`CheckedSql::sql`] 是已拼好前缀、
/// 可直接交给 `SessionHandle::execute` 的文本。
///
/// 仅对支持 EXPLAIN 语义的驱动调用（V1 = mariadb）；SQLite 由工具层先行返回
/// `capability.unsupported`，不得走到这里。
#[allow(unused_variables)] // 骨架占位：#208 实现。
pub fn check_explain(sql: &str) -> Result<CheckedSql> {
    Err(pending())
}

fn pending() -> Error {
    Error::new(
        ErrorCode::MCP_UNSUPPORTED,
        ErrorCategory::Unsupported,
        "SQL 只读策略实现待 #208 落地",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_reports_explicit_pending() {
        let error = check_readonly("SELECT 1").expect_err("骨架不得伪装为放行");
        assert_eq!(error.code(), ErrorCode::MCP_UNSUPPORTED);
        let error = check_explain("SELECT 1").expect_err("骨架不得伪装为放行");
        assert_eq!(error.code(), ErrorCode::MCP_UNSUPPORTED);
    }
}
