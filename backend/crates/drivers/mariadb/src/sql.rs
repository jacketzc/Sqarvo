//! 单语句检测与会隐式提交的语句识别。
//!
//! MySQL 协议族在连接时固定开启 `CLIENT_MULTI_STATEMENTS`（`mysql_async` 无法关闭），
//! 服务端会真的执行 `SELECT 1; DROP TABLE t`。v0.2 的能力描述符声明
//! `multiple_statements = false`，因此必须在**发送前**拒绝多条语句，
//! 而不是执行完第一条再报错——那已经造成了副作用。
//!
//! 这里用保守的字符扫描器判断「是否存在第二条有内容的语句」：
//! 识别单引号、双引号、反引号（含反斜杠转义与成对引号）、`--` 与 `#` 行注释、
//! `/* */` 块注释。它不解析 SQL 语法，只关心顶层分号。
//!
//! 事务内还要识别「会隐式提交」的语句：MariaDB 遇到 DDL / `TRUNCATE` / 管理命令时
//! 会先把当前事务提交掉再执行，`ROLLBACK` 因此什么也回滚不了（实测见
//! `docs/database/mariadb.md`「事务」）。这类语句在事务中一律**发送前拒绝**，
//! 绝不让服务端静默提交。

/// 是否包含多条语句。
pub(crate) fn has_multiple_statements(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut state = State::Normal;
    let mut statements = 0usize;
    let mut current_has_content = false;
    let mut index = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        match state {
            State::Normal => match byte {
                b'\'' => {
                    state = State::SingleQuote;
                    current_has_content = true;
                }
                b'"' => {
                    state = State::DoubleQuote;
                    current_has_content = true;
                }
                b'`' => {
                    state = State::Backtick;
                    current_has_content = true;
                }
                b'#' => state = State::LineComment,
                b'-' if bytes.get(index + 1) == Some(&b'-') => state = State::LineComment,
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    state = State::BlockComment;
                    index += 1;
                }
                b';' => {
                    if current_has_content {
                        statements += 1;
                        // 已有两条就不必继续扫描。
                        if statements > 1 {
                            return true;
                        }
                        current_has_content = false;
                    }
                }
                other if other.is_ascii_whitespace() => {}
                _ => current_has_content = true,
            },
            State::SingleQuote | State::DoubleQuote | State::Backtick => {
                let quote = match state {
                    State::SingleQuote => b'\'',
                    State::DoubleQuote => b'"',
                    _ => b'`',
                };
                if byte == b'\\' {
                    // 反斜杠转义：跳过下一个字节。
                    index += 1;
                } else if byte == quote {
                    // 成对引号（'' 或 "" 或 ``）表示转义，仍留在字符串内。
                    if bytes.get(index + 1) == Some(&quote) {
                        index += 1;
                    } else {
                        state = State::Normal;
                    }
                }
            }
            State::LineComment => {
                if byte == b'\n' {
                    state = State::Normal;
                }
            }
            State::BlockComment => {
                if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    state = State::Normal;
                    index += 1;
                }
            }
        }
        index += 1;
    }

    if current_has_content {
        statements += 1;
    }
    statements > 1
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    SingleQuote,
    DoubleQuote,
    Backtick,
    LineComment,
    BlockComment,
}

/// 事务中会隐式提交的语句：返回语句类别用于错误文案，不是这类语句返回 `None`。
///
/// 依据 MariaDB 文档列的「会导致隐式提交的语句」：DDL（`CREATE` / `ALTER` / `DROP` /
/// `RENAME`）、`TRUNCATE`、权限与复制管理命令、`LOCK TABLES`、`FLUSH`、`RESET`、
/// `ANALYZE` / `OPTIMIZE` / `REPAIR TABLE`，以及事务控制语句本身
/// （`BEGIN` / `START TRANSACTION` / `COMMIT` / `ROLLBACK`）与 `SET autocommit`。
///
/// 例外：`CREATE TEMPORARY TABLE` / `DROP TEMPORARY TABLE` **不**隐式提交，
/// 因此放行（临时表本身不可回滚，但不会把已写入的行提交掉）。
///
/// 只按首个关键字判断，不做语法解析：宁可对畸形语句给出明确拒绝，
/// 也不能让一条 DDL 混进事务里把前面的写入静默提交掉。
pub(crate) fn implicit_commit_statement(sql: &str) -> Option<&'static str> {
    let words = leading_words(sql, 3);
    let first = *words.first()?;
    if matches!(first.to_ascii_lowercase().as_str(), "create" | "drop")
        && words
            .get(1)
            .is_some_and(|word| word.eq_ignore_ascii_case("temporary"))
    {
        return None;
    }
    if first.eq_ignore_ascii_case("set") {
        // `SET @@autocommit = 1` / `SET autocommit=1` 会提交当前事务；`SET NAMES`、
        // `SET time_zone` 之类不提交，不能一并拒绝。
        // 这里不按单词判断：`@@autocommit` / `@@session.autocommit` 里的 `@` 与 `.`
        // 会打断单词扫描，所以只在本条语句（首个分号之前）里找变量名。
        let statement = sql.split(';').next().unwrap_or(sql);
        return statement
            .to_ascii_lowercase()
            .contains("autocommit")
            .then_some("SET autocommit");
    }
    [
        ("alter", "ALTER"),
        ("create", "CREATE"),
        ("drop", "DROP"),
        ("rename", "RENAME"),
        ("truncate", "TRUNCATE"),
        ("grant", "GRANT"),
        ("revoke", "REVOKE"),
        ("lock", "LOCK"),
        ("unlock", "UNLOCK"),
        ("flush", "FLUSH"),
        ("reset", "RESET"),
        ("install", "INSTALL"),
        ("uninstall", "UNINSTALL"),
        ("analyze", "ANALYZE"),
        ("optimize", "OPTIMIZE"),
        ("repair", "REPAIR"),
        ("cache", "CACHE"),
        ("change", "CHANGE"),
        ("start", "START"),
        ("stop", "STOP"),
        ("begin", "BEGIN"),
        ("commit", "COMMIT"),
        ("rollback", "ROLLBACK"),
    ]
    .into_iter()
    .find_map(|(keyword, label)| first.eq_ignore_ascii_case(keyword).then_some(label))
}

/// 取语句开头的至多 `limit` 个字母单词，跳过空白与注释。
///
/// 只用于识别第一个关键字，遇到非字母开头的字符（括号、数字、引号）即停止，
/// 因此 `SELECT 'create table'` 不会被误判成 DDL。
fn leading_words(sql: &str, limit: usize) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut words = Vec::with_capacity(limit);
    let mut index = 0usize;
    while words.len() < limit {
        // 跳过空白与注释。
        loop {
            match bytes.get(index) {
                Some(byte) if byte.is_ascii_whitespace() => index += 1,
                Some(b'/') if bytes.get(index + 1) == Some(&b'*') => {
                    index += 2;
                    while index < bytes.len()
                        && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
                    {
                        index += 1;
                    }
                    index = (index + 2).min(bytes.len());
                }
                Some(b'-') if bytes.get(index + 1) == Some(&b'-') => {
                    while index < bytes.len() && bytes[index] != b'\n' {
                        index += 1;
                    }
                }
                Some(b'#') => {
                    while index < bytes.len() && bytes[index] != b'\n' {
                        index += 1;
                    }
                }
                _ => break,
            }
        }
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_alphabetic) {
            index += 1;
        }
        if start == index {
            break;
        }
        words.push(&sql[start..index]);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::has_multiple_statements;

    #[test]
    fn single_statements_are_accepted() {
        assert!(!has_multiple_statements("SELECT 1"));
        assert!(!has_multiple_statements("SELECT 1;"));
        assert!(!has_multiple_statements("SELECT 1;  \n "));
        assert!(!has_multiple_statements("SELECT 1;;"));
        assert!(!has_multiple_statements("  CREATE TABLE t (v INT)  "));
        assert!(!has_multiple_statements(""));
        assert!(!has_multiple_statements("-- 只有注释\n"));
        assert!(!has_multiple_statements("/* 只有注释 */"));
        assert!(!has_multiple_statements("SELECT 1 -- 尾注释"));
    }

    #[test]
    fn semicolons_inside_literals_are_ignored() {
        assert!(!has_multiple_statements("SELECT ';'"));
        assert!(!has_multiple_statements("SELECT \";\""));
        assert!(!has_multiple_statements("SELECT `a;b` FROM t"));
        assert!(!has_multiple_statements("SELECT 'a;b', \"c;d\""));
        assert!(!has_multiple_statements("SELECT 'it''s; fine'"));
        assert!(!has_multiple_statements("SELECT 'esc\\'; still one'"));
        assert!(!has_multiple_statements("SELECT 1 /* ; 注释里的分号 */"));
    }

    #[test]
    fn multiple_statements_are_rejected() {
        assert!(has_multiple_statements("SELECT 1; SELECT 2"));
        assert!(has_multiple_statements("SELECT 1; DROP TABLE t"));
        assert!(has_multiple_statements("SELECT 1;\n-- 注释\nSELECT 2"));
        assert!(has_multiple_statements("INSERT INTO t VALUES (1);COMMIT"));
    }

    #[test]
    fn ddl_and_admin_statements_are_recognized_as_implicit_commit() {
        use super::implicit_commit_statement;

        assert_eq!(
            implicit_commit_statement("CREATE TABLE t (v INT)"),
            Some("CREATE")
        );
        assert_eq!(
            implicit_commit_statement("  /* 前置注释 */\n alter table t add c int"),
            Some("ALTER")
        );
        assert_eq!(implicit_commit_statement("DROP TABLE t"), Some("DROP"));
        assert_eq!(
            implicit_commit_statement("RENAME TABLE a TO b"),
            Some("RENAME")
        );
        assert_eq!(
            implicit_commit_statement("TRUNCATE TABLE t"),
            Some("TRUNCATE")
        );
        assert_eq!(
            implicit_commit_statement("create index i on t (v)"),
            Some("CREATE")
        );
        assert_eq!(
            implicit_commit_statement("LOCK TABLES t WRITE"),
            Some("LOCK")
        );
        assert_eq!(
            implicit_commit_statement("SET @@autocommit = 1"),
            Some("SET autocommit")
        );
        assert_eq!(
            implicit_commit_statement("set @@session.autocommit=1"),
            Some("SET autocommit")
        );
        assert_eq!(
            implicit_commit_statement("set autocommit=1"),
            Some("SET autocommit")
        );
        assert_eq!(implicit_commit_statement("COMMIT"), Some("COMMIT"));
        // 注释掉的前缀不能改变判断。
        assert_eq!(
            implicit_commit_statement("# 注释\n-- 注释\ntruncate t"),
            Some("TRUNCATE")
        );
    }

    #[test]
    fn non_committing_statements_are_allowed_in_transactions() {
        use super::implicit_commit_statement;

        assert_eq!(implicit_commit_statement("SELECT 1"), None);
        assert_eq!(implicit_commit_statement("INSERT INTO t VALUES (1)"), None);
        assert_eq!(
            implicit_commit_statement("UPDATE t SET v = 1 WHERE id = 2"),
            None
        );
        assert_eq!(
            implicit_commit_statement("DELETE FROM t WHERE id = 3"),
            None
        );
        assert_eq!(implicit_commit_statement("SET NAMES utf8mb4"), None);
        assert_eq!(implicit_commit_statement("SET time_zone = '+08:00'"), None);
        // 关键字出现在字符串里不等于语句本身是 DDL。
        assert_eq!(implicit_commit_statement("SELECT 'create table t'"), None);
        assert_eq!(
            implicit_commit_statement("(SELECT 1) UNION (SELECT 2)"),
            None
        );
        // 临时表不隐式提交：放行，避免把合法的临时表用法一起拒绝。
        assert_eq!(
            implicit_commit_statement("CREATE TEMPORARY TABLE t (v INT)"),
            None
        );
        assert_eq!(implicit_commit_statement("DROP TEMPORARY TABLE t"), None);
    }
}
