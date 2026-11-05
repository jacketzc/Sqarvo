//! 列信息面板的**纯文案层**（#87）。
//!
//! 这一层不创建任何 GTK 对象，也不读数据库：输入是列元数据，输出是界面上要写的
//! 文字。这样「哪个字段该显示什么」可以单元测试，界面只负责把文字贴到控件上。
//!
//! 文案口径直接来自 `docs/api/grpc-conventions.md`「列元数据的存在性语义」：
//!
//! - `default_value` 未设置 = **未知**，不是「无默认值」；
//! - `COLUMN_DEFAULT_NONE` = 明确没有默认值；
//! - `COLUMN_DEFAULT_NULL` = 默认值是 SQL NULL，与空串不同；
//! - `COLUMN_DEFAULT_EXPRESSION` = **未求值**的原文，必须原样展示：
//!   既不能求值，也不能据此推断运行时类型。`'0'`、`''`、`current_timestamp()`
//!   都属于这一态，前端不额外把带引号的文本改成「常量」——协议没有这个状态，
//!   擅自分类会把「默认空串」与「常量 0」这类信息重新揉在一起；
//! - `character_set` / `collation` 未设置 = **不适用**（SQLite 没有列级字符集），
//!   不是「字符集为空」。

use crate::backend::table::TableColumn;
use crate::proto::common::v1::{ColumnDefault, ColumnDefaultKind};

/// 字段未提供时的展示文案（协议要求「不适用」而不是空白）。
pub const NOT_APPLICABLE: &str = "不适用";

/// 「未知」：老服务端没发该字段，或元数据来自结果集。
pub const UNKNOWN: &str = "未知（服务端未提供）";

/// 默认值四态之一（与 `ColumnDefaultKind` 一一对应，`UNSPECIFIED` 归入「未知」）。
///
/// 之所以在前端再定义一份而不是直接存 proto 消息：proto 生成类型不实现 `Eq`，
/// 而 `TableColumn` 需要 `Eq`（草稿层用值比较判断列是否变化）；同时把
/// 「未知」从「没设置」显式化，避免界面层把 `None` 当成「无默认值」。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ColumnDefaultState {
    /// 服务端未提供（`UNSPECIFIED` 或字段缺失）。
    #[default]
    Unknown,
    /// 明确没有默认值（`COLUMN_DEFAULT_NONE`）。
    NoDefault,
    /// 默认值是 SQL NULL（`COLUMN_DEFAULT_NULL`）。
    Null,
    /// 未求值的默认值原文（`COLUMN_DEFAULT_EXPRESSION`）。
    Expression(String),
}

impl ColumnDefaultState {
    /// 从 `ColumnMeta.default_value` 构造。
    ///
    /// `None`、未知枚举值（含 0）都落到 [`Self::Unknown`]：协议说「未指定」时
    /// 服务端不发出该值，收到时按未知处理；越界值同样不能猜成「无默认值」。
    #[must_use]
    pub fn from_proto(value: Option<&ColumnDefault>) -> Self {
        let Some(value) = value else {
            return Self::Unknown;
        };
        match ColumnDefaultKind::try_from(value.kind) {
            Ok(ColumnDefaultKind::ColumnDefaultNone) => Self::NoDefault,
            Ok(ColumnDefaultKind::ColumnDefaultNull) => Self::Null,
            Ok(ColumnDefaultKind::ColumnDefaultExpression) => {
                Self::Expression(value.expression.clone())
            }
            Ok(ColumnDefaultKind::ColumnDefaultUnspecified) | Err(_) => Self::Unknown,
        }
    }

    /// 是否是「未求值原文」这一态（界面据此显示附注）。
    #[must_use]
    pub fn is_expression(&self) -> bool {
        matches!(self, Self::Expression(_))
    }
}

/// 默认值的展示文案。
///
/// 表达式态返回**原文**（不求值、不去引号）。空原文属于服务端数据异常：
/// 明确写出「表达式（服务端未给出文本）」，绝不留空，否则会被读成「无默认值」。
#[must_use]
pub fn default_label(state: &ColumnDefaultState) -> String {
    match state {
        ColumnDefaultState::Unknown => UNKNOWN.to_owned(),
        ColumnDefaultState::NoDefault => "无默认值".to_owned(),
        ColumnDefaultState::Null => "默认 NULL".to_owned(),
        ColumnDefaultState::Expression(text) if text.trim().is_empty() => {
            "表达式（服务端未给出文本）".to_owned()
        }
        ColumnDefaultState::Expression(text) => text.clone(),
    }
}

/// 可空性的展示文案。
#[must_use]
pub fn nullable_label(nullable: bool) -> &'static str {
    if nullable {
        "可空"
    } else {
        "不可空（NOT NULL）"
    }
}

/// 字符集的展示文案：缺失/空白 → 「不适用」，其余原样（去首尾空白）。
#[must_use]
pub fn charset_label(value: Option<&str>) -> String {
    name_or_not_applicable(value)
}

/// 排序规则的展示文案：口径同 [`charset_label`]。
#[must_use]
pub fn collation_label(value: Option<&str>) -> String {
    name_or_not_applicable(value)
}

fn name_or_not_applicable(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| NOT_APPLICABLE.to_owned(), str::to_owned)
}

/// 面板一行的六个字段（顺序即界面列顺序）。
#[must_use]
pub fn column_info_rows(column: &TableColumn) -> [(&'static str, String); 6] {
    [
        ("列名", column.name.clone()),
        ("原始类型", column.type_label.clone()),
        ("可空", nullable_label(column.nullable).to_owned()),
        ("默认值", default_label(&column.default_value)),
        ("字符集", charset_label(column.character_set.as_deref())),
        ("排序规则", collation_label(column.collation.as_deref())),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::common::v1::ColumnDefaultKind;

    fn proto(kind: ColumnDefaultKind, expression: &str) -> ColumnDefault {
        ColumnDefault {
            kind: kind as i32,
            expression: expression.to_owned(),
        }
    }

    fn column(default: ColumnDefaultState) -> TableColumn {
        TableColumn {
            name: "note".to_owned(),
            ordinal: 0,
            kind: crate::proto::common::v1::ColumnTypeKind::ColumnTypeText,
            type_label: "varchar(32)".to_owned(),
            nullable: true,
            unsigned: false,
            with_time_zone: false,
            scale: None,
            primary_key_position: None,
            default_value: default,
            character_set: None,
            collation: None,
        }
    }

    #[test]
    fn missing_default_is_unknown_not_no_default() {
        // 未设置 ≠ 明确没有默认值：两者文案必须不同，否则用户会以为
        // NOT NULL 列可以靠默认值插入。
        assert_eq!(
            ColumnDefaultState::from_proto(None),
            ColumnDefaultState::Unknown
        );
        assert_eq!(default_label(&ColumnDefaultState::Unknown), UNKNOWN);
        assert_ne!(
            default_label(&ColumnDefaultState::Unknown),
            default_label(&ColumnDefaultState::NoDefault)
        );
    }

    #[test]
    fn unspecified_kind_is_unknown() {
        // proto 说「收到 UNSPECIFIED 时按未知处理」；越界枚举值同理，不能猜。
        assert_eq!(
            ColumnDefaultState::from_proto(Some(&proto(
                ColumnDefaultKind::ColumnDefaultUnspecified,
                ""
            ))),
            ColumnDefaultState::Unknown
        );
        let mut bogus = proto(ColumnDefaultKind::ColumnDefaultNone, "");
        bogus.kind = 99;
        assert_eq!(
            ColumnDefaultState::from_proto(Some(&bogus)),
            ColumnDefaultState::Unknown
        );
    }

    #[test]
    fn no_default_has_its_own_label() {
        assert_eq!(
            ColumnDefaultState::from_proto(Some(&proto(ColumnDefaultKind::ColumnDefaultNone, ""))),
            ColumnDefaultState::NoDefault
        );
        assert_eq!(default_label(&ColumnDefaultState::NoDefault), "无默认值");
    }

    #[test]
    fn null_default_is_distinct_from_no_default_and_empty_string() {
        assert_eq!(
            ColumnDefaultState::from_proto(Some(&proto(ColumnDefaultKind::ColumnDefaultNull, ""))),
            ColumnDefaultState::Null
        );
        assert_eq!(default_label(&ColumnDefaultState::Null), "默认 NULL");
        // 空串默认值是 Expression("''")，与 NULL 不是一回事。
        assert_eq!(
            default_label(&ColumnDefaultState::Expression("''".to_owned())),
            "''"
        );
        assert_ne!(default_label(&ColumnDefaultState::Null), "''");
    }

    #[test]
    fn constant_default_is_shown_verbatim() {
        // 常量默认值在协议里同样是 Expression：前端只照抄原文，不求值、不去引号。
        let state = ColumnDefaultState::from_proto(Some(&proto(
            ColumnDefaultKind::ColumnDefaultExpression,
            "'pending'",
        )));
        assert_eq!(
            state,
            ColumnDefaultState::Expression("'pending'".to_owned())
        );
        assert_eq!(default_label(&state), "'pending'");
        assert!(state.is_expression());
    }

    #[test]
    fn expression_default_is_shown_verbatim_without_evaluation() {
        let state = ColumnDefaultState::from_proto(Some(&proto(
            ColumnDefaultKind::ColumnDefaultExpression,
            "current_timestamp()",
        )));
        assert_eq!(default_label(&state), "current_timestamp()");
        // 大小写与括号都不做归一。
        let mixed = ColumnDefaultState::Expression("CURRENT_TIMESTAMP".to_owned());
        assert_eq!(default_label(&mixed), "CURRENT_TIMESTAMP");
    }

    #[test]
    fn empty_expression_text_is_explicit_instead_of_blank() {
        let state = ColumnDefaultState::Expression("   ".to_owned());
        let label = default_label(&state);
        assert!(!label.trim().is_empty());
        assert!(label.contains("服务端未给出文本"), "{label}");
    }

    #[test]
    fn charset_and_collation_are_not_applicable_when_absent() {
        // SQLite：协议永远不设这两个字段，界面必须写「不适用」而不是空白。
        assert_eq!(charset_label(None), NOT_APPLICABLE);
        assert_eq!(collation_label(None), NOT_APPLICABLE);
        assert_eq!(charset_label(Some("  ")), NOT_APPLICABLE);
        assert_eq!(collation_label(Some("")), NOT_APPLICABLE);
        assert!(!charset_label(None).is_empty());
    }

    #[test]
    fn charset_and_collation_names_pass_through() {
        assert_eq!(charset_label(Some("utf8mb4")), "utf8mb4");
        assert_eq!(
            collation_label(Some("utf8mb4_general_ci")),
            "utf8mb4_general_ci"
        );
        assert_eq!(charset_label(Some(" utf8mb4 ")), "utf8mb4");
    }

    #[test]
    fn nullable_label_distinguishes_the_two_states() {
        assert_eq!(nullable_label(true), "可空");
        assert_eq!(nullable_label(false), "不可空（NOT NULL）");
        assert_ne!(nullable_label(true), nullable_label(false));
    }

    #[test]
    fn rows_cover_every_field_in_panel_order() {
        let rows = column_info_rows(&column(ColumnDefaultState::NoDefault));
        let keys: Vec<&str> = rows.iter().map(|(key, _)| *key).collect();
        assert_eq!(
            keys,
            ["列名", "原始类型", "可空", "默认值", "字符集", "排序规则"]
        );
        assert_eq!(rows[2].1, "可空");
        assert_eq!(rows[3].1, "无默认值");
        // SQLite：字符集/排序规则两行都是「不适用」，不是空串。
        assert_eq!(rows[4].1, NOT_APPLICABLE);
        assert_eq!(rows[5].1, NOT_APPLICABLE);
    }
}
