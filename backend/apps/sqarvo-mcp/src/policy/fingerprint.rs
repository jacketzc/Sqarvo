//! SQL 规范化指纹（V3.1 §12；spike §5 实证路径）。
//!
//! 实现：AST 级 `visit_expressions_mut` 把数据字面量原地替换为 `?` 占位符 →
//! `Display` unparse → SHA-256 小写十六进制。审计与错误回显只出现指纹，不落原始 SQL。
//!
//! 已知设计点（ADR 0032）：
//! - `IN (1, 2, 3)` 的**列表长度参与指纹**（不折叠为定长形态）——不同长度的同类查询
//!   是不同指纹，属可接受的已知行为；
//! - unparse 会丢弃注释、归一化空白与操作符间距 → 天然形态归一；
//! - 幂等性：`normalized` 再解析 → 再掩码 → 再 unparse 结果不变（单测固化）。

use super::SqlFingerprint;
use sha2::{Digest as _, Sha256};
use sqlparser::ast::{Expr, Statement, Value, visit_expressions_mut};
use std::fmt::Write as _;
use std::ops::ControlFlow;

/// 掩码后的占位符文本（MySqlDialect 可再解析，spike §5 实测 `reparsed_ok`）。
const PLACEHOLDER: &str = "?";

/// 计算已通过策略语句的指纹（字面量脱敏 + SHA-256）。
pub(crate) fn fingerprint_of(statement: &Statement) -> SqlFingerprint {
    let mut masked = statement.clone();
    let _: ControlFlow<()> = visit_expressions_mut(&mut masked, |expr| {
        if let Expr::Value(value) = expr
            && is_data_literal(&value.value)
        {
            value.value = Value::Placeholder(PLACEHOLDER.to_string());
        }
        ControlFlow::Continue(())
    });
    let normalized = masked.to_string();
    let sha256_hex = sha256_hex(normalized.as_bytes());
    SqlFingerprint {
        normalized,
        sha256_hex,
    }
}

/// 携带用户数据（可能敏感）的字面量形态都要掩码；`Null` / `Boolean` / `Placeholder`
/// 是结构信息，保留。非 MySQL 方言的字符串族（dollar / raw / triple / quote-delimited）
/// 一并掩码，作防御纵深。
fn is_data_literal(value: &Value) -> bool {
    matches!(
        value,
        Value::Number(..)
            | Value::SingleQuotedString(..)
            | Value::DoubleQuotedString(..)
            | Value::HexStringLiteral(..)
            | Value::NationalStringLiteral(..)
            | Value::EscapedStringLiteral(..)
            | Value::UnicodeStringLiteral(..)
            | Value::SingleQuotedByteStringLiteral(..)
            | Value::DoubleQuotedByteStringLiteral(..)
            | Value::TripleSingleQuotedString(..)
            | Value::TripleDoubleQuotedString(..)
            | Value::TripleSingleQuotedByteStringLiteral(..)
            | Value::TripleDoubleQuotedByteStringLiteral(..)
            | Value::SingleQuotedRawStringLiteral(..)
            | Value::DoubleQuotedRawStringLiteral(..)
            | Value::TripleSingleQuotedRawStringLiteral(..)
            | Value::TripleDoubleQuotedRawStringLiteral(..)
            | Value::DollarQuotedString(..)
            | Value::QuoteDelimitedStringLiteral(..)
            | Value::NationalQuoteDelimitedStringLiteral(..)
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in &Sha256::digest(bytes) {
        // write! 到 String 不会失败（无 IO），忽略 Result 只用于格式化。
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ast::parse_readonly_statement;

    fn fp(sql: &str) -> SqlFingerprint {
        fingerprint_of(&parse_readonly_statement(sql).expect("指纹测试输入必须先过策略"))
    }

    #[test]
    fn same_shape_variants_share_fingerprint() {
        let a = fp("SELECT * FROM users WHERE id = 123 AND name = 'alice'");
        let b = fp("SELECT * FROM users WHERE id = 456 AND name = 'bob'");
        assert_eq!(
            a.normalized,
            "SELECT * FROM users WHERE id = ? AND name = ?"
        );
        assert_eq!(a, b, "同形变体必须同指纹（spike §5）");
    }

    #[test]
    fn fingerprint_is_idempotent_under_reparse() {
        let first = fp("SELECT `col` FROM `db`.`tbl` WHERE id IN (1, 2, 3) ORDER BY 1");
        let second = fp(&first.normalized);
        assert_eq!(first, second, "normalized 再走一遍掩码+unparse 必须不动点");
    }

    #[test]
    fn in_list_length_participates_in_fingerprint() {
        let three = fp("SELECT a FROM t WHERE x IN (1, 2, 3) ORDER BY a");
        let two = fp("SELECT a FROM t WHERE x IN (7, 8) ORDER BY a");
        assert_ne!(
            three.sha256_hex, two.sha256_hex,
            "IN 列表长度参与指纹是已锁定行为（ADR 0032）"
        );
    }

    #[test]
    fn digest_is_lowercase_hex_sha256_of_normalized() {
        let f = fp("SELECT 1");
        assert_eq!(f.sha256_hex, sha256_hex(f.normalized.as_bytes()));
        assert_eq!(f.sha256_hex.len(), 64);
        assert!(
            f.sha256_hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }
}
