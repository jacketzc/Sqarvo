//! 单语句检测。
//!
//! MySQL 协议族在连接时固定开启 `CLIENT_MULTI_STATEMENTS`（`mysql_async` 无法关闭），
//! 服务端会真的执行 `SELECT 1; DROP TABLE t`。v0.2 的能力描述符声明
//! `multiple_statements = false`，因此必须在**发送前**拒绝多条语句，
//! 而不是执行完第一条再报错——那已经造成了副作用。
//!
//! 这里用保守的字符扫描器判断「是否存在第二条有内容的语句」：
//! 识别单引号、双引号、反引号（含反斜杠转义与成对引号）、`--` 与 `#` 行注释、
//! `/* */` 块注释。它不解析 SQL 语法，只关心顶层分号。

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
}
