//! 名称过滤的 SQL 下推支持（v0.2.2，#84）。
//!
//! 契约要求过滤**下推**到数据库（ADR 0019），两个驱动的做法只有引擎差异，规则本身完全
//! 一样，因此集中在这里：
//!
//! 1. [`like_escape`] 把用户原文里的 `%`、`_` 与转义符自身变成字面量。客户端不拼通配符，
//!    驱动也不许把 `%` 当通配符透传（搜 `user_log` 不能命中 `userXlog`）；
//! 2. [`sqlite_name_condition`] / [`mariadb_name_condition`] 产出**带占位符**的条件片段与
//!    绑定值。片段进 `WHERE`，绑定值进参数——用户输入永远不进 SQL 文本；
//! 3. [`paginate_children_filtered`](crate::paginate_children_filtered) 在分页 token 里钉住
//!    过滤条件，换条件后旧 token 直接 `config.invalid`。
//!
//! 大小写敏感性**不**在这里统一：SQLite 的 `LIKE` 只对 ASCII 不敏感，MariaDB 取决于列的
//! 排序规则。统一成一种语义会让其中一个驱动撒谎（ADR 0019 的「代价」）。

use sqarvo_driver_api::{NameMatch, ObjectFilter};

/// `LIKE` 的转义字符。SQL 里的 `ESCAPE` 子句必须与这里一致。
pub const LIKE_ESCAPE_CHAR: char = '\\';

/// 把原文转成 `LIKE` 模式里的**字面量**片段（不含 `%` 包裹）。
///
/// 转义符自身也要转义：否则用户搜 `a\%` 时 `\` 会吃掉后面的 `%`，语义就变了。
///
/// ```
/// # use sqarvo_metadata::like_escape;
/// assert_eq!(like_escape("user_log"), "user\\_log");
/// assert_eq!(like_escape("100%"), "100\\%");
/// assert_eq!(like_escape("a\\b"), "a\\\\b");
/// ```
#[must_use]
pub fn like_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    for character in value.chars() {
        if character == '%' || character == '_' || character == LIKE_ESCAPE_CHAR {
            escaped.push(LIKE_ESCAPE_CHAR);
        }
        escaped.push(character);
    }
    escaped
}

/// SQLite 的名称条件：`<列> LIKE '%' || ? || '%' ESCAPE '\'` / `<列> = ?`。
///
/// 返回值是 `(条件片段, 绑定值)`；片段**不带** `WHERE` / `AND` 前缀，由调用方决定接在
/// 哪里；`filter.name` 为空时是 `("", None)`。
/// `contains` 把通配符包在 SQL 里（`'%' || ? || '%'`），绑定值只有转义后的字面量；
/// `exact` 用 `=` 且绑定**原文**（`=` 不做通配符解释，转义会改变字面量）。
///
/// 大小写：`LIKE` 对 ASCII 不敏感、`=` 按列上的 BINARY 排序规则比较（区分大小写）。
/// 这是 SQLite 的真实行为，驱动不加 `lower()` 去「统一」（文档已写明差异）。
#[must_use]
pub fn sqlite_name_condition(filter: &ObjectFilter, column: &str) -> (String, Option<String>) {
    match filter.name.as_ref() {
        None => (String::new(), None),
        Some(NameMatch::Contains(value)) => (
            format!("{column} LIKE '%' || ? || '%' ESCAPE '\\'"),
            Some(like_escape(value)),
        ),
        Some(NameMatch::Exact(value)) => (format!("{column} = ?"), Some(value.clone())),
    }
}

/// MariaDB 的名称条件：`<列> LIKE CONCAT('%', ?, '%')` / `<列> = ?`。
///
/// 与 SQLite 的差别只有串接写法：MariaDB 的 `||` 默认是逻辑或（除非
/// `PIPES_AS_CONCAT`），所以用 `CONCAT`。转义规则相同（`%`、`_`、`\` 变字面量），
/// 且**不**写 `ESCAPE` 子句——MySQL 协议族的 `LIKE` 默认转义符就是反斜杠，显式写
/// `ESCAPE '\'` 反而在 `NO_BACKSLASH_ESCAPES` 下会把两个字面量当成转义符而报错。
///
/// 大小写：由列的排序规则决定（`utf8mb4_general_ci` 下不敏感，`_bin` / `_as_cs` 下敏感），
/// 驱动不加 `LOWER()`。
#[must_use]
pub fn mariadb_name_condition(filter: &ObjectFilter, column: &str) -> (String, Option<String>) {
    match filter.name.as_ref() {
        None => (String::new(), None),
        Some(NameMatch::Contains(value)) => (
            format!("{column} LIKE CONCAT('%', ?, '%')"),
            Some(like_escape(value)),
        ),
        Some(NameMatch::Exact(value)) => (format!("{column} = ?"), Some(value.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_and_the_escape_char_become_literals() {
        assert_eq!(like_escape("user_log"), "user\\_log");
        assert_eq!(like_escape("100%"), "100\\%");
        assert_eq!(like_escape("a\\b"), "a\\\\b");
        // 已经是转义序列的输入不能被「再理解一次」：反斜杠先变成两个，`%` 再加一个。
        assert_eq!(like_escape("\\%"), "\\\\\\%");
    }

    #[test]
    fn ordinary_names_pass_through_unchanged() {
        for name in ["users", "订单", "a b", "a.b", "Ünïcode"] {
            assert_eq!(like_escape(name), name);
        }
    }

    fn filter(name: Option<NameMatch>) -> ObjectFilter {
        ObjectFilter {
            name,
            kinds: Vec::new(),
        }
    }

    /// 条件片段必须是「占位符 + ESCAPE」的形态：断言的是真正拼进 `WHERE` 的 SQL，
    /// 而不是「过滤之后的结果看起来对」。把下推退回内存过滤，这条会跟着改红。
    #[test]
    fn sqlite_conditions_are_parameterized_and_escaped() {
        let contains = filter(Some(NameMatch::Contains("user_log".to_owned())));
        let (clause, value) = sqlite_name_condition(&contains, "name");
        assert_eq!(clause, "name LIKE '%' || ? || '%' ESCAPE '\\'");
        assert_eq!(value.as_deref(), Some("user\\_log"));

        let exact = filter(Some(NameMatch::Exact("user_log".to_owned())));
        let (clause, value) = sqlite_name_condition(&exact, "name");
        assert_eq!(clause, "name = ?");
        // `=` 不解释通配符，绑定原文；转义反而会去找带反斜杠的名字。
        assert_eq!(value.as_deref(), Some("user_log"));

        assert_eq!(sqlite_name_condition(&ObjectFilter::none(), "name").0, "");
    }

    #[test]
    fn mariadb_conditions_use_concat_and_placeholders() {
        let contains = filter(Some(NameMatch::Contains("100%".to_owned())));
        let (clause, value) = mariadb_name_condition(&contains, "TABLE_NAME");
        assert_eq!(clause, "TABLE_NAME LIKE CONCAT('%', ?, '%')");
        assert_eq!(value.as_deref(), Some("100\\%"));

        let exact = filter(Some(NameMatch::Exact("100%".to_owned())));
        let (clause, value) = mariadb_name_condition(&exact, "TABLE_NAME");
        assert_eq!(clause, "TABLE_NAME = ?");
        assert_eq!(value.as_deref(), Some("100%"));

        assert_eq!(
            mariadb_name_condition(&ObjectFilter::none(), "TABLE_NAME").0,
            ""
        );
    }
}
