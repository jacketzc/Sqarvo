//! 脚本切分（摘要 / 守卫用）与会隐式提交的语句识别。
//!
//! MySQL 协议族在连接时固定开启 `CLIENT_MULTI_STATEMENTS`（`mysql_async` 无法关闭），
//! 服务端会真的按序执行 `SELECT 1; DROP TABLE t`。#188 起驱动声明
//! `multiple_statements = true`：整段脚本**原样直发**，切分与执行都由服务端完成，
//! 驱动不自写 SQL 切分器去改写语句。
//!
//! 这里的保守字符扫描器只产出「顶层分号切出的语句段」，用于三件事：
//! 判断是否多语句脚本、`statements[]` 全量摘要（序号 / SQL 预览）、
//! 事务内**逐条**套用隐式提交守卫。识别单引号、双引号、反引号
//! （含反斜杠转义与成对引号）、`--` 与 `#` 行注释、`/* */` 块注释，
//! 只关心顶层分号；空段与纯注释段不计，尾随分号不产生额外段。
//!
//! 事务内还要识别「会隐式提交」的语句：MariaDB 遇到 DDL / `TRUNCATE` / 管理命令时
//! 会先把当前事务提交掉再执行，`ROLLBACK` 因此什么也回滚不了（实测见
//! `docs/database/mariadb.md`「事务」）。这类语句在事务中一律**发送前拒绝**，
//! 绝不让服务端静默提交。

use sqarvo_common::{Error, ErrorCode, StatementFailure};

/// 按顶层分号把脚本切成语句段（去掉首尾空白；空段 / 纯注释段不计）。
///
/// **不是执行用的切分器**：整段脚本原样发给服务端，由服务端切分执行
/// （`CLIENT_MULTI_STATEMENTS`）。返回的段只用于摘要、逐条隐式提交守卫
/// 与「是否多语句」判定，因此口径必须与服务端一致：引号 / 注释里的分号
/// 不算分隔符，尾随分号不算第二条。
pub(crate) fn split_statements(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut state = State::Normal;
    let mut segments: Vec<&str> = Vec::new();
    let mut start = 0usize;
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
                        push_segment(sql, start, index, &mut segments);
                        current_has_content = false;
                    }
                    start = index + 1;
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
        push_segment(sql, start, bytes.len(), &mut segments);
    }
    segments
}

/// 收一个语句段：去首尾空白，空段（含纯注释段）不计。
fn push_segment<'a>(sql: &'a str, start: usize, end: usize, segments: &mut Vec<&'a str>) {
    let segment = sql[start..end].trim();
    if !segment.is_empty() {
        segments.push(segment);
    }
}

/// 脚本缓冲行数超预算（ADR 0006 修订，#188；错误码与文案口径对齐 #187）。
pub(crate) fn script_too_large(failed_index: u32) -> Error {
    Error::resource_exhausted(
        ErrorCode::QUERY_SCRIPT_TOO_LARGE,
        "多语句脚本缓冲行数超过预算，请拆分脚本或给查询加 LIMIT",
    )
    .with_statement_failure(StatementFailure {
        failed_index,
        completed_statements: failed_index,
    })
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

/// 语句的首个关键字是否为 `USE`（ADR 0030 的库上下文缓存失效判据）。
///
/// 用户在编辑器里自己写 `USE db` 时，会话当前库会变而驱动的缓存不知道；下次执行前
/// 必须重读 `SELECT DATABASE()` 再决定是否发 `USE`，否则会出现「缓存说 A、服务端在 B、
/// 请求也是 A → 跳过切换 → 语句落到 B」这种落错库。
///
/// 与 `split_statements` / `implicit_commit_statement` 同一口径：只按首个关键字
/// 判断，不做语法解析。判断错方向的代价不对称——判成 `USE` 只多一次轻量查询，
/// 漏判则可能用到错的库。
pub(crate) fn starts_with_use(sql: &str) -> bool {
    leading_words(sql, 1)
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("use"))
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
    use super::split_statements;

    #[test]
    fn single_statements_stay_single() {
        assert_eq!(split_statements("SELECT 1"), vec!["SELECT 1"]);
        assert_eq!(split_statements("SELECT 1;"), vec!["SELECT 1"]);
        assert_eq!(split_statements("SELECT 1;  \n "), vec!["SELECT 1"]);
        assert_eq!(split_statements("SELECT 1;;"), vec!["SELECT 1"]);
        assert_eq!(
            split_statements("  CREATE TABLE t (v INT)  "),
            vec!["CREATE TABLE t (v INT)"]
        );
        assert_eq!(split_statements(""), Vec::<String>::new());
        assert_eq!(split_statements("-- 只有注释\n"), Vec::<String>::new());
        assert_eq!(split_statements("/* 只有注释 */"), Vec::<String>::new());
        assert_eq!(
            split_statements("SELECT 1 -- 尾注释"),
            vec!["SELECT 1 -- 尾注释"]
        );
    }

    #[test]
    fn semicolons_inside_literals_are_ignored() {
        assert_eq!(split_statements("SELECT ';'"), vec!["SELECT ';'"]);
        assert_eq!(split_statements("SELECT \";\""), vec!["SELECT \";\""]);
        assert_eq!(
            split_statements("SELECT `a;b` FROM t"),
            vec!["SELECT `a;b` FROM t"]
        );
        assert_eq!(
            split_statements("SELECT 'a;b', \"c;d\""),
            vec!["SELECT 'a;b', \"c;d\""]
        );
        assert_eq!(
            split_statements("SELECT 'it''s; fine'"),
            vec!["SELECT 'it''s; fine'"]
        );
        assert_eq!(
            split_statements("SELECT 'esc\\'; still one'"),
            vec!["SELECT 'esc\\'; still one'"]
        );
        assert_eq!(
            split_statements("SELECT 1 /* ; 注释里的分号 */"),
            vec!["SELECT 1 /* ; 注释里的分号 */"]
        );
    }

    #[test]
    fn multiple_statements_are_split_in_order() {
        assert_eq!(
            split_statements("SELECT 1; SELECT 2"),
            vec!["SELECT 1", "SELECT 2"]
        );
        assert_eq!(
            split_statements("SELECT 1; DROP TABLE t"),
            vec!["SELECT 1", "DROP TABLE t"]
        );
        assert_eq!(
            split_statements("SELECT 1;\n-- 注释\nSELECT 2"),
            vec!["SELECT 1", "-- 注释\nSELECT 2"]
        );
        assert_eq!(
            split_statements("INSERT INTO t VALUES (1);COMMIT"),
            vec!["INSERT INTO t VALUES (1)", "COMMIT"]
        );
        // 空段 / 纯注释段不计；段保留原文（含前置注释），切分不改写 SQL。
        assert_eq!(
            split_statements("; ;SELECT 1;/* 段注释 */; SELECT 2;"),
            vec!["SELECT 1", "SELECT 2"]
        );
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

    #[test]
    fn use_statements_are_recognised_for_cache_invalidation() {
        use super::starts_with_use;

        assert!(starts_with_use("USE db1"));
        assert!(starts_with_use("  use `db 1`"));
        assert!(starts_with_use("\n\tUSE db1;"));
        assert!(starts_with_use("/* 注释 */ USE db1"));
        assert!(starts_with_use("-- 换库\nUSE db1"));

        // 首关键字不是 use：不重读缓存（避免每条语句都多一次查询）。
        assert!(!starts_with_use("SELECT * FROM config_info"));
        assert!(!starts_with_use("UPDATE t SET user = 1"));
        assert!(!starts_with_use(""));
        // 只在字符串里出现 `use` 不算。
        assert!(!starts_with_use("SELECT 'use db1'"));
    }
}
