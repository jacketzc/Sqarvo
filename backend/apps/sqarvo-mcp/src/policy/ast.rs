//! sqlparser AST 只读判定（V3.1 §8；spike 实证见 scratch/mcp-spike-sqlparser/report.md）。
//!
//! 判定流程（固化 spike 结论，不重新发明）：
//! 1. `Parser::parse_sql(&MySqlDialect{}, sql)`——Err 一律 `mcp.sql_parse`（fail-closed，
//!    含默认递归保护 `limit=50` 触顶；深度边界本身是 `DoS` 缓解，刻意不调高）；
//! 2. 语句数 ≠ 1 → `mcp.readonly_violation`（多语句，ADR 0006 单语句口径）；
//! 3. 顶层 match 穷举：仅 `Statement::Query` 进入深检，其余语句类型（含未来 sqlparser
//!    新增变体）由 `_` 兜底拒绝——禁止按首个关键字判断；
//! 4. 一次 `visitor` 全树遍历（`pre_visit_statement/query/select/expr` 四钩子）覆盖
//!    任意深度的：嵌套非 Query 语句（数据修改 CTE）、`Query.locks`（锁定读）、
//!    `Select.into`（一切 INTO 形态）、`optimizer_hints`、危险函数名、`:=` 会话变量赋值。

use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use sqlparser::ast::{BinaryOperator, Expr, ObjectName, Query, Select, Statement, Visit, Visitor};
use sqlparser::dialect::{Dialect, MySqlDialect};
use sqlparser::parser::{Parser, ParserError};
use std::ops::ControlFlow;

/// 危险函数默认表（小写键，匹配时对 AST 中的名字 `to_lowercase`）。
///
/// 前六项是 V3.1 §8.3 钉死的默认集合；`nextval` 是 §8.3「其他已确认的副作用函数」的
/// 落地（推进序列对象 = 状态修改），风险按表说明。扩展该表属策略配置，见 ADR 0033。
const DANGEROUS_FUNCTIONS: &[(&str, &str)] = &[
    (
        "load_file",
        "读取数据库服务器侧文件（需 FILE 权限，越出查询边界）",
    ),
    ("sleep", "人为延长查询，占用连接与超时预算"),
    ("benchmark", "CPU 循环基准，资源耗尽型副作用"),
    ("get_lock", "获取会话级咨询锁，影响其他会话"),
    ("release_lock", "释放会话级咨询锁，改变锁状态"),
    (
        "release_all_locks",
        "释放当前会话持有的全部咨询锁，改变锁状态",
    ),
    ("nextval", "推进序列对象取值，属于状态修改而非只读访问"),
];

/// 解析并检查 `sql`：通过策略则返回唯一的顶层语句（供指纹复用，避免二次解析）。
pub(crate) fn parse_readonly_statement(sql: &str) -> Result<Statement> {
    // 保持 sqlparser 默认递归保护（limit=50）：触顶返回 ParserError → mcp.sql_parse，
    // 深度边界本身是 DoS 缓解，刻意不调高（ADR 0033）。
    parse_readonly_statement_with_dialect(sql, &MySqlDialect {}, false)
}

pub(super) fn parse_readonly_statement_with_dialect(
    sql: &str,
    dialect: &dyn Dialect,
    sqlite_plan: bool,
) -> Result<Statement> {
    let statements = Parser::parse_sql(dialect, sql).map_err(|error| {
        if sqlite_plan {
            Error::invalid_argument(ErrorCode::MCP_SQL_PARSE, "SQLite 查询计划 SQL 解析失败")
        } else {
            parse_error(&error)
        }
    })?;
    let statement = match statements.as_slice() {
        [single] => single,
        [] => {
            return Err(readonly_violation(
                "没有可执行的 SQL 语句：策略要求输入解析为单条只读查询。",
            ));
        }
        many => {
            return Err(readonly_violation(format!(
                "仅允许单条 SQL 语句：本次输入解析出 {} 条语句（多语句一律拒绝，请拆分后逐条提交）。",
                many.len()
            )));
        }
    };
    // 顶层白名单：只有查询（SELECT / 只读 WITH）可以进入深检，其余语句类型拒绝。
    let query = match statement {
        Statement::Query(query) => query,
        other => {
            return Err(readonly_violation(format!(
                "只读白名单仅允许单条 SELECT / 只读 WITH 查询，本次输入是语句类型 {}。",
                statement_kind(other)
            )));
        }
    };
    let mut scanner = ReadOnlyScanner {
        violations: Vec::new(),
    };
    // 一次全树遍历覆盖所有嵌套深度（手写「顶层+CTE」递归实测会漏 FROM 派生表）。
    let _: ControlFlow<()> = query.visit(&mut scanner);
    if sqlite_plan {
        let mut sqlite = SqlitePlanScanner;
        if let ControlFlow::Break(()) = query.visit(&mut sqlite) {
            return Err(readonly_violation(
                "SQLite 查询计划禁止加载扩展或调用文件操作函数",
            ));
        }
    }
    if !scanner.violations.is_empty() {
        return Err(readonly_violation(format!(
            "只读策略拒绝：{}",
            scanner.violations.join("；")
        )));
    }
    Ok(statement.clone())
}

struct SqlitePlanScanner;

impl Visitor for SqlitePlanScanner {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        if let Expr::Function(function) = expr
            && let Some(name) = function.name.0.last().and_then(|part| part.as_ident())
            && ["load_extension", "writefile", "readfile", "edit"]
                .iter()
                .any(|denied| name.value.eq_ignore_ascii_case(denied))
        {
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}

/// visitor 全树扫描器：只记录违规，不在中途 `Break`——一次遍历给出全部拒绝理由，
/// 便于 Agent 一轮修完。
struct ReadOnlyScanner {
    violations: Vec<String>,
}

impl ReadOnlyScanner {
    fn deny(&mut self, reason: impl Into<String>) {
        self.violations.push(reason.into());
    }

    /// 危险函数判定：取限定名**最后一段**小写比较（覆盖 `db.fn`、`` `SLEEP` `` 形态）；
    /// 动态名段（`ObjectNamePart::Function`）与空名无法静态判定 → 保守拒绝。
    fn check_function_name(&mut self, name: &ObjectName) {
        let Some(last) = name.0.last() else {
            self.deny(format!(
                "禁止无法静态判定函数名的调用：{name}（空函数名，按保守策略拒绝）。"
            ));
            return;
        };
        let Some(ident) = last.as_ident() else {
            self.deny(format!(
                "禁止动态函数名：{name}（函数名由运行期表达式产生，无法静态审计，按保守策略拒绝）。"
            ));
            return;
        };
        let lowered = ident.value.to_lowercase();
        if let Some((_, risk)) = DANGEROUS_FUNCTIONS
            .iter()
            .find(|(denied, _)| *denied == lowered)
        {
            self.deny(format!("禁止危险函数：{name}——{risk}。"));
        }
    }
}

impl Visitor for ReadOnlyScanner {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
        // 根节点已是 Query（顶层白名单保证）；遍历中再出现 Statement 即为数据修改 CTE
        // （`SetExpr::Insert/Update/Delete`）一类，任何非 Query 形态都拒绝。
        if !matches!(statement, Statement::Query(_)) {
            self.deny(format!(
                "查询内部嵌套了非 SELECT 语句（类型 {}，如数据修改 CTE），只读场景不允许。",
                statement_kind(statement)
            ));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        for lock in &query.locks {
            self.deny(format!(
                "禁止锁定读：{lock}（FOR UPDATE / FOR SHARE / NOWAIT / SKIP LOCKED / OF 列表一律拒绝）。"
            ));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        if let Some(into) = &select.into {
            let targets: Vec<String> = into.targets.iter().map(ToString::to_string).collect();
            self.deny(format!(
                "禁止 INTO 子句：V1 一律拒绝任何形式的 SELECT ... INTO（会话变量 / 列清单 / 表 / OUTFILE / DUMPFILE），目标：{}。",
                targets.join(", ")
            ));
        }
        if !select.optimizer_hints.is_empty() {
            let hints: Vec<String> = select
                .optimizer_hints
                .iter()
                .map(ToString::to_string)
                .collect();
            self.deny(format!(
                "禁止优化器 hint：{}（hint 是不可静态验证的任意执行计划指令，V1 一律拒绝）。",
                hints.join(" ")
            ));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        match expr {
            Expr::Function(function) => self.check_function_name(&function.name),
            // MySQL 在查询投影里用 `:=` 给会话变量赋值——是会话状态副作用（§8.2
            // 「能修改会话状态的形态」）；比较运算用 `=`，不受此规则影响。
            Expr::BinaryOp {
                op: BinaryOperator::Assignment,
                ..
            } => self.deny("禁止会话变量赋值（:=）：该写法修改会话状态，不是只读查询。"),
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

fn statement_kind(statement: &Statement) -> String {
    let debug = format!("{statement:?}");
    let head: String = debug.chars().take(140).collect();
    match head.find(['(', '{']) {
        Some(cut) => head[..cut].trim_end().to_string(),
        None => head,
    }
}

fn readonly_violation(message: impl Into<String>) -> Error {
    // 分类按任务书 V3.1 §11：只读违规=策略拒绝 → PermissionDenied（不是参数非法；
    // 语句本身语法可以合法，拒绝原因是「这条通道不允许它」）。
    Error::new(
        ErrorCode::MCP_READONLY_VIOLATION,
        ErrorCategory::PermissionDenied,
        message,
    )
}

fn parse_error(error: &ParserError) -> Error {
    let detail: String = error
        .to_string()
        .replace('\n', " ")
        .chars()
        .take(300)
        .collect();
    Error::new(
        ErrorCode::MCP_SQL_PARSE,
        ErrorCategory::InvalidArgument,
        format!(
            "解析失败：{detail}。已按 fail-closed 原则拒绝（原语句未必非法，但当前策略只接受 sqlparser MySqlDialect 可解析的单条只读查询，MariaDB 特有语法请改写后重试）。"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::{Ident, ObjectNamePart, ObjectNamePartFunction};

    fn scan_name(parts: &[&str]) -> Vec<String> {
        let name = ObjectName(
            parts
                .iter()
                .map(|segment| ObjectNamePart::Identifier(Ident::new(*segment)))
                .collect(),
        );
        let mut scanner = ReadOnlyScanner {
            violations: Vec::new(),
        };
        scanner.check_function_name(&name);
        scanner.violations
    }

    /// `ObjectNamePart::Function`（动态函数名）在 `MySqlDialect` 下没有可解析的字面量入口，
    /// 用手工构造的 AST 固化保守拒绝行为（spike §3 保守条款）。
    #[test]
    fn dynamic_function_name_is_denied() {
        let name = ObjectName(vec![ObjectNamePart::Function(ObjectNamePartFunction {
            name: Ident::new("some_fn"),
            args: Vec::new(),
        })]);
        let mut scanner = ReadOnlyScanner {
            violations: Vec::new(),
        };
        scanner.check_function_name(&name);
        assert_eq!(scanner.violations.len(), 1, "动态函数名必须保守拒绝");
        assert!(scanner.violations[0].contains("动态函数名"));
    }

    #[test]
    fn empty_function_name_is_denied() {
        let mut scanner = ReadOnlyScanner {
            violations: Vec::new(),
        };
        scanner.check_function_name(&ObjectName(Vec::new()));
        assert_eq!(scanner.violations.len(), 1, "空函数名必须保守拒绝");
    }

    #[test]
    fn dangerous_function_matching_rules() {
        // 大小写不敏感 + 限定名取最后段（spike §3；反引号只影响 quote_style，不影响 value）。
        for parts in [
            &["SLEEP"][..],
            &["sLeEp"],
            &["mysql", "SLEEP"],
            &["db", "func", "SLEEP"],
            &["LOAD_FILE"],
            &["NextVal"],
        ] {
            assert_eq!(
                scan_name(parts).len(),
                1,
                "危险函数 {} 必须命中拒绝表",
                parts.join(".")
            );
        }
        // 普通函数不误报。
        assert_eq!(
            scan_name(&["DATE_FORMAT"]),
            Vec::<String>::new(),
            "普通函数不得误报"
        );
        assert!(
            scan_name(&["insert"]).is_empty(),
            "关键词型函数 INSERT() 不得误报"
        );
    }

    #[test]
    fn top_level_entry_rejects_non_query_with_explicit_codes() {
        // 解析失败与策略拒绝的错误码分界在入口固化。
        assert_eq!(
            parse_readonly_statement("SELECT * FROM t LOCK IN SHARE MODE")
                .expect_err("锁定读形态必须被拒")
                .code(),
            ErrorCode::MCP_SQL_PARSE
        );
        assert_eq!(
            parse_readonly_statement("INSERT INTO t VALUES (1)")
                .expect_err("DML 必须被拒")
                .code(),
            ErrorCode::MCP_READONLY_VIOLATION
        );
        assert_eq!(
            parse_readonly_statement("SELECT 1; SELECT 2")
                .expect_err("多语句必须被拒")
                .code(),
            ErrorCode::MCP_READONLY_VIOLATION
        );
        let statement = parse_readonly_statement("SELECT 1").expect("单条 SELECT 应放行");
        assert!(matches!(statement, Statement::Query(_)));
        // 指纹路径可 Display 化（回归保护）。
        assert_eq!(statement.to_string(), "SELECT 1");
    }
}
