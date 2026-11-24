//! SQLite 多语句脚本的引擎切分（ADR 0006 修订，#187）。
//!
//! 不用文本扫描器找分号：`CREATE TRIGGER … BEGIN … END` 体内的分号、字符串与
//! 注释里的分号都会切错。这里直接让 SQLite 引擎自己切——`sqlite3_prepare_v2`
//! 的 `pzTail` 指向第一条语句之后的剩余文本，循环 prepare → 取切片 → finalize
//! 即可得到与引擎语义完全一致的语句边界。prepare 只编译不执行，探测无副作用。
//!
//! `unsafe` 只出现在 [`split_first`] 一个小函数里：裸指针全部来自同一次
//! `sqlite3_prepare_v2` 调用，生命周期不超过该函数。
//!
//! # 为什么这里允许 unsafe
//!
//! 语句切分必须调用 `sqlite3_prepare_v2` 的 `pzTail` 输出，rusqlite 的安全
//! API 只 prepare 第一条语句、不暴露 tail，没有安全封装可复用。仓库默认
//! `unsafe_code = "deny"`，此处按 AGENTS.md §11 的要求**局部**放宽并说明不变量：
//! 每个 `unsafe` 块旁边都有 `SAFETY` 注释；连接句柄由会话线程独占借用，
//! `c_sql` 缓冲区在整个调用期间有效，`pzTail` 指针不越出该缓冲区，
//! prepare 出的语句恰好 finalize 一次。上层（[`probe`] / [`next_segment`]）
//! 是安全函数，切分语义由单测包围（尤其 trigger 体内分号）。

#![allow(unsafe_code)]

use std::ffi::{CStr, CString};

use rusqlite::Connection;
use rusqlite::ffi;
use sqarvo_common::{Error, ErrorCode, Result};

use crate::error::map_sqlite_error;

/// 引擎切分出的一条真实语句（空段 / 纯注释段已被跳过）。
#[derive(Debug)]
pub(crate) struct Segment {
    /// 语句 SQL 切片，含终止分号（若有）。
    pub sql: String,
    /// 下一条语句（或空段）在原脚本中的起始字节偏移。
    pub next: usize,
}

/// 脚本探测结果。
#[derive(Debug)]
pub(crate) enum ScriptProbe {
    /// 至多一条真实语句（含空脚本 / 纯注释）：走既有单语句路径，行为不变。
    Single,
    /// 至少两条真实语句：走多语句缓冲路径，`first` 是第一条。
    Multiple { first: Segment },
}

/// 探测脚本是单语句还是多语句。
///
/// 只 prepare、不 step，因此对写语句也无副作用。第二条语句可能暂时 prepare
/// 失败（例如 `CREATE TABLE t …; INSERT INTO t …` 里表尚未创建）——那也说明
/// 存在第二条语句，真正的错误留到顺序执行、前序语句生效后再报。
pub(crate) fn probe(connection: &Connection, sql: &str) -> Result<ScriptProbe> {
    // 含 NUL 字节的 SQL 交给既有 prepare 路径报错，保持单语句行为完全不变。
    if sql.contains('\0') {
        return Ok(ScriptProbe::Single);
    }
    let Some(first) = next_segment(connection, sql, 0)? else {
        return Ok(ScriptProbe::Single);
    };
    match next_segment(connection, sql, first.next) {
        Ok(None) => Ok(ScriptProbe::Single),
        Ok(Some(_)) | Err(_) => Ok(ScriptProbe::Multiple { first }),
    }
}

/// Verify the native statement boundary without preparing any trailing text.
pub(crate) fn validate_plan(connection: &Connection, sql: &str) -> Result<()> {
    use sqlparser::dialect::SQLiteDialect;
    use sqlparser::tokenizer::{Token, Tokenizer};

    let rejected = || {
        Error::new(
            ErrorCode::MCP_READONLY_VIOLATION,
            sqarvo_common::ErrorCategory::PermissionDenied,
            "SQLite allows only a single read-only query plan statement",
        )
    };
    if !sql.starts_with("EXPLAIN QUERY PLAN ") || sql.contains('\0') {
        return Err(rejected());
    }
    let (present, consumed) = split_first(connection, sql, true)?;
    let tail = sql.get(consumed..).ok_or_else(rejected)?;
    let tokens = Tokenizer::new(&SQLiteDialect {}, tail)
        .tokenize()
        .map_err(|_| rejected())?;
    if !present
        || !tokens
            .iter()
            .all(|token| matches!(token, Token::Whitespace(_) | Token::SemiColon))
    {
        return Err(rejected());
    }
    Ok(())
}

/// 从 `start` 偏移起切出下一条真实语句；剩余全是空段 / 注释时返回 `None`。
pub(crate) fn next_segment(
    connection: &Connection,
    sql: &str,
    start: usize,
) -> Result<Option<Segment>> {
    let mut cursor = start;
    while cursor < sql.len() {
        let remainder = sql.get(cursor..).ok_or_else(|| {
            Error::internal("SQL statement boundary does not fall on a character boundary")
        })?;
        let (has_statement, consumed) = split_first(connection, remainder, false)?;
        // 引擎必须前进：空段也返回非零 tail，否则视为内部错误而不是死循环。
        if consumed == 0 {
            return Err(Error::internal(
                "SQLite statement splitting made no progress",
            ));
        }
        let next = cursor + consumed;
        if has_statement {
            let text = sql.get(cursor..next).ok_or_else(|| {
                Error::internal("SQL statement boundary does not fall on a character boundary")
            })?;
            return Ok(Some(Segment {
                sql: text.to_owned(),
                next,
            }));
        }
        cursor = next;
    }
    Ok(None)
}

/// 让引擎编译 `sql` 的第一条语句，返回（是否有真实语句, 消费的字节数）。
///
/// 空段 / 纯注释段：`sqlite3_prepare_v2` 返回 `SQLITE_OK` 且 `ppStmt` 为 NULL，
/// `pzTail` 仍指向剩余文本——据此跳过。语句 prepare 失败时按既有分类映射错误。
fn split_first(connection: &Connection, sql: &str, require_plan: bool) -> Result<(bool, usize)> {
    let c_sql = CString::new(sql).map_err(|_| Error::internal("SQL contains a NUL byte"))?;
    let mut statement: *mut ffi::sqlite3_stmt = std::ptr::null_mut();
    let mut tail: *const std::ffi::c_char = std::ptr::null();

    // SAFETY: `connection.handle()` 返回该连接有效的 `sqlite3*`（rusqlite 保证
    // 连接存活期间句柄有效，且本函数在会话线程内独占使用连接）；`c_sql` 是合法
    // NUL 终止字符串；`statement` / `tail` 是栈上输出参数，用 `&raw mut` 直接
    // 取址得到有效的原始指针（不经过中间借用）。
    // 调用不 step；部分 PRAGMA 可在 prepare 阶段生效，因此计划路径不 prepare 尾部。
    let rc = unsafe {
        ffi::sqlite3_prepare_v2(
            connection.handle(),
            c_sql.as_ptr(),
            -1,
            &raw mut statement,
            &raw mut tail,
        )
    };

    if rc != ffi::SQLITE_OK {
        // SAFETY: rc != SQLITE_OK 时连接句柄仍有效；sqlite3_errmsg 返回的指针
        // 由连接持有，立即复制为 owned String，不跨调用保存。
        let message = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(connection.handle())) }
            .to_string_lossy()
            .into_owned();
        let error = rusqlite::Error::SqliteFailure(ffi::Error::new(rc), Some(message));
        return Err(map_sqlite_error(
            error,
            "Failed to prepare SQLite statement",
        ));
    }

    let has_statement = !statement.is_null();
    if has_statement {
        // SAFETY: statement is a live prepared statement until finalize below.
        let valid_plan = !require_plan
            || unsafe {
                ffi::sqlite3_stmt_readonly(statement) != 0
                    && ffi::sqlite3_stmt_isexplain(statement) == 2
            };
        // SAFETY: statement 非空且来自上面成功的 prepare，finalize 恰好一次。
        let rc = unsafe { ffi::sqlite3_finalize(statement) };
        if rc != ffi::SQLITE_OK {
            // prepare 成功而 finalize 失败极罕见（如内存不足）；仍按既有分类上报。
            let error = rusqlite::Error::SqliteFailure(ffi::Error::new(rc), None);
            return Err(map_sqlite_error(
                error,
                "Failed to release SQLite statement",
            ));
        }
        if !valid_plan {
            return Err(Error::new(
                ErrorCode::MCP_READONLY_VIOLATION,
                sqarvo_common::ErrorCategory::PermissionDenied,
                "The SQLite engine could not verify that the statement is a read-only query plan",
            ));
        }
    }

    // SAFETY: prepare 成功时 pzTail 指向 c_sql 缓冲区内部（或末尾的 NUL），
    // 与 as_ptr() 同源；指针差即消费的字节数，不会越过缓冲区。
    let consumed = unsafe { tail.offset_from(c_sql.as_ptr()) };
    let consumed = usize::try_from(consumed)
        .map_err(|_| Error::internal("Invalid SQL statement split offset"))?;
    Ok((has_statement, consumed))
}

/// 脚本缓冲行数超预算（ADR 0006 修订，#187）。
pub(crate) fn script_too_large(failed_index: u32) -> Error {
    Error::resource_exhausted(
        ErrorCode::QUERY_SCRIPT_TOO_LARGE,
        "The buffered row count exceeds the budget for a multi-statement script. Split the script or add LIMIT to the query",
    )
    .with_statement_failure(sqarvo_common::StatementFailure {
        failed_index,
        completed_statements: failed_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        Connection::open_in_memory().expect("打开内存库")
    }

    fn segments(connection: &Connection, sql: &str) -> Vec<Segment> {
        let mut out = Vec::new();
        let mut cursor = 0;
        while let Some(segment) = next_segment(connection, sql, cursor).expect("切分成功") {
            cursor = segment.next;
            out.push(segment);
        }
        out
    }

    /// 单语句 / 空脚本 / 尾分号 / 纯注释都探测为 Single，走既有路径。
    #[test]
    fn single_statement_shapes_probe_single() {
        let connection = connection();
        for sql in [
            "SELECT 1",
            "SELECT 1;",
            "SELECT 1; ",
            "SELECT 1; -- 尾注释",
            "",
            "   ",
            "-- 只有注释",
            "/* 块注释 */",
            "; ;",
            "SELECT ';';",
        ] {
            assert!(
                matches!(
                    probe(&connection, sql).expect("探测成功"),
                    ScriptProbe::Single
                ),
                "应探测为单语句: {sql:?}"
            );
        }
    }

    /// 两条语句探测为 Multiple，第一条切片含分号、偏移正确。
    #[test]
    fn two_statements_probe_multiple() {
        let connection = connection();
        let ScriptProbe::Multiple { first } =
            probe(&connection, "SELECT 1; SELECT 2").expect("探测成功")
        else {
            panic!("应探测为多语句");
        };
        assert_eq!(first.sql, "SELECT 1;");
        assert_eq!(first.next, "SELECT 1;".len());
    }

    /// trigger 体内的分号不切错：整条 CREATE TRIGGER 是一个切片。
    #[test]
    fn trigger_body_semicolons_are_not_split() {
        let connection = connection();
        connection
            .execute_batch("CREATE TABLE t(x INTEGER)")
            .expect("建表");
        let sql = "CREATE TRIGGER tr AFTER INSERT ON t BEGIN \
                   UPDATE t SET x = x + 1; \
                   UPDATE t SET x = x + 2; \
                   END; SELECT 1";
        let ScriptProbe::Multiple { first } = probe(&connection, sql).expect("探测成功") else {
            panic!("应探测为多语句");
        };
        assert!(first.sql.starts_with("CREATE TRIGGER"));
        assert!(first.sql.ends_with("END;"));
        assert_eq!(
            first.sql.matches(';').count(),
            3,
            "体内两个分号 + END 后一个"
        );
        // 切片必须能作为单语句重新 prepare（引擎认可的完整语句）。
        connection
            .prepare(&first.sql)
            .expect("trigger 切片可 prepare");

        let rest = segments(&connection, &sql[first.next..]);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].sql.trim(), "SELECT 1");
    }

    /// 空段 / 纯注释段 / 尾分号被跳过，只留真实语句。
    #[test]
    fn empty_and_comment_segments_are_skipped() {
        let connection = connection();
        let sql = "SELECT 1; ; -- 注释\n; /* 块 */ ; SELECT 2;";
        let parts = segments(&connection, sql);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].sql.contains("SELECT 1"));
        assert!(parts[1].sql.contains("SELECT 2"));
    }

    /// DDL→DML：第二条暂时 prepare 失败（表未建）仍探测为多语句。
    #[test]
    fn ddl_then_dml_probes_multiple_despite_peek_error() {
        let connection = connection();
        let sql = "CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1)";
        let ScriptProbe::Multiple { first } = probe(&connection, sql).expect("探测成功") else {
            panic!("应探测为多语句");
        };
        // 顺序执行时第二条才能 prepare 成功：切分随执行推进，
        // 先执行第一条，再切分剩余部分（与 serve_script 的顺序一致）。
        connection.execute_batch(&first.sql).expect("建表");
        let rest = segments(&connection, &sql[first.next..]);
        assert_eq!(rest.len(), 1);
        connection
            .prepare(&rest[0].sql)
            .expect("建表后 INSERT 可 prepare");
    }

    /// 第一条就有语法错误：按既有分类上报（io.error / Internal）。
    #[test]
    fn syntax_error_in_first_statement_is_classified() {
        let connection = connection();
        let error = probe(&connection, "SELEC 1").expect_err("应报错");
        assert_eq!(error.code(), ErrorCode::IO);
    }

    /// 含 NUL 的 SQL 不在切分层处理，交回既有 prepare 路径。
    #[test]
    fn nul_sql_falls_through_to_single() {
        let connection = connection();
        assert!(matches!(
            probe(&connection, "SELECT 1\0; SELECT 2").expect("探测成功"),
            ScriptProbe::Single
        ));
    }

    /// 每个切片都能独立 prepare，且列数与原语句一致。
    #[test]
    fn every_segment_reprepares_individually() {
        let connection = connection();
        let parts = segments(&connection, "SELECT 1; SELECT 2, 3; SELECT 'x'");
        assert_eq!(parts.len(), 3);
        for (part, columns) in parts.iter().zip([1, 2, 1]) {
            let statement = connection.prepare(&part.sql).expect("切片可 prepare");
            assert_eq!(statement.column_count(), columns);
        }
    }
}
