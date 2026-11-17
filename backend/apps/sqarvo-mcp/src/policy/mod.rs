//! SQL 只读策略与指纹（#208）。
//!
//! **本文件公开的签名是跨泳道契约**（见 `lib.rs` 模块表）：签名不得变更；
//! #209 的 `execute_readonly_query` / `explain_query` 按此消费。
//!
//! 策略要点（任务书 V3.1 §8，spike 实证见 scratch/mcp-spike-sqlparser/report.md）：
//! - 仅放行单条 `SELECT` / 只读 `WITH`；解析失败、多语句、未知形态一律拒绝；
//! - visitor 全树遍历覆盖嵌套 DML（数据修改 CTE）、`Query.locks`、`Select.into`
//!   （一切 INTO 形式）、`optimizer_hints`、危险函数表；
//! - 审计只存 [`SqlFingerprint`]（字面量脱敏 + SHA-256），不存原始 SQL。
//!
//! 判定实现在 [`ast`]（白名单 + visitor 扫描 + 错误码分界），指纹在 [`fingerprint`]。
//! AST 检查不是最终安全边界：只读账号、只读事务、超时、行数/字节上限继续叠加
//! （V3.1 §8.4）。

mod ast;
mod fingerprint;

use sqarvo_common::Result;

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
pub fn check_readonly(sql: &str) -> Result<CheckedSql> {
    let statement = ast::parse_readonly_statement(sql)?;
    Ok(CheckedSql {
        sql: sql.to_owned(),
        fingerprint: fingerprint::fingerprint_of(&statement),
    })
}

/// 检查 `sql` 并构造 EXPLAIN 语句（V3.1 §6）：先走与 [`check_readonly`] 完全相同的
/// 策略，再由本模块拼 `EXPLAIN ` 前缀——用户直接提交的 `EXPLAIN` / `ANALYZE`
/// 形态在策略层就应被拒，不存在第二条路。返回的 [`CheckedSql::sql`] 是已拼好前缀、
/// 可直接交给 `SessionHandle::execute` 的文本。
///
/// 仅对支持 EXPLAIN 语义的驱动调用（V1 = mariadb）；SQLite 由工具层先行返回
/// `capability.unsupported`，不得走到这里。
pub fn check_explain(sql: &str) -> Result<CheckedSql> {
    // 指纹按**原 SQL** 计算（审计不归因 EXPLAIN 前缀）；任何策略错误原样透出，
    // 不存在「先拼 EXPLAIN 再检查」的旁路。
    let checked = check_readonly(sql)?;
    Ok(CheckedSql {
        sql: format!("EXPLAIN {}", checked.sql),
        fingerprint: checked.fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_common::ErrorCode;

    #[test]
    fn plain_select_passes_with_fingerprint() {
        let checked = check_readonly("SELECT 1").expect("单条 SELECT 必须放行");
        assert_eq!(checked.sql, "SELECT 1", "策略只验证、不改写原文");
        assert_eq!(
            checked.fingerprint.normalized, "SELECT ?",
            "数字字面量必须脱敏"
        );
        assert_eq!(checked.fingerprint.sha256_hex.len(), 64);
    }

    #[test]
    fn literals_never_leak_into_fingerprint() {
        let checked = check_readonly("SELECT * FROM users WHERE id = 123 AND name = 'alice'")
            .expect("只读查询必须放行");
        assert!(!checked.fingerprint.normalized.contains("alice"));
        assert!(!checked.fingerprint.normalized.contains("123"));
    }

    #[test]
    fn non_readonly_statements_keep_distinct_error_codes() {
        assert_eq!(
            check_readonly("INSERT INTO t VALUES (1)")
                .expect_err("DML 必须拒绝")
                .code(),
            ErrorCode::MCP_READONLY_VIOLATION
        );
        assert_eq!(
            check_readonly("SELECT 1; SELECT 2")
                .expect_err("多语句必须拒绝")
                .code(),
            ErrorCode::MCP_READONLY_VIOLATION
        );
        assert_eq!(
            check_readonly("SELECT FROM WHERE")
                .expect_err("解析失败必须 fail-closed 拒绝")
                .code(),
            ErrorCode::MCP_SQL_PARSE
        );
    }

    #[test]
    fn explain_wraps_prefix_but_fingerprints_original() {
        let checked = check_explain("SELECT id FROM t WHERE id = 1").expect("只读查询可 EXPLAIN");
        assert_eq!(checked.sql, "EXPLAIN SELECT id FROM t WHERE id = 1");
        assert_eq!(
            checked.fingerprint.normalized, "SELECT id FROM t WHERE id = ?",
            "审计指纹不得带上 EXPLAIN 前缀"
        );
    }

    #[test]
    fn explain_cannot_bypass_policy() {
        // 用户自带 EXPLAIN 前缀、或借 EXPLAIN 提交副作用查询，都走同一策略。
        assert_eq!(
            check_explain("EXPLAIN SELECT 1")
                .expect_err("用户不得自带 EXPLAIN")
                .code(),
            ErrorCode::MCP_READONLY_VIOLATION
        );
        assert_eq!(
            check_explain("SELECT * FROM t FOR UPDATE")
                .expect_err("锁读不得借 EXPLAIN 绕过")
                .code(),
            ErrorCode::MCP_READONLY_VIOLATION
        );
        assert_eq!(
            check_explain("ANALYZE SELECT 1")
                .expect_err("ANALYZE SELECT 可能真实执行，必须拒绝")
                .code(),
            ErrorCode::MCP_SQL_PARSE
        );
    }
}
