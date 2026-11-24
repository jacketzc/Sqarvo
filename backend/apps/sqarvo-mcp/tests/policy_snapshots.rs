//! #208 只读策略快照矩阵（表驱动；V3.1 §8 / §6 / §12，spike 判定矩阵固化）。
//!
//! 分三个桶：
//! - [`POSITIVE`]：必须放行的只读形态（≥20 例，取自 spike 报告 §2.1 全量 ALLOW 矩阵）；
//! - [`NEGATIVE_VIOLATION`]：解析成功但被策略显式拒绝 → `mcp.readonly_violation`；
//! - [`NEGATIVE_FAIL_CLOSED`]：当前 sqlparser 0.64 解析失败 → `mcp.sql_parse`（fail-closed）。
//!
//! 另含 EXPLAIN 旁路矩阵、递归深度触顶、指纹合同（同形同指纹 / unparse 幂等）。
//! 全部离线，不依赖网络与数据库。

use sqarvo_common::ErrorCode;
use sqarvo_mcp::policy::{check_explain, check_readonly};

/// 正向放行矩阵（spike §2.1 / §6「常规 BI/应用只读查询零误杀」）。
const POSITIVE: &[&str] = &[
    "SELECT 1",
    "SELECT u.id, o.total FROM users u JOIN orders o ON u.id = o.uid WHERE u.a > 1 GROUP BY u.id HAVING COUNT(*) > 1 ORDER BY u.id LIMIT 10 OFFSET 5",
    "WITH cte AS (SELECT id FROM users WHERE id = 1) SELECT * FROM cte",
    "WITH RECURSIVE seq AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 10) SELECT * FROM seq",
    "SELECT * FROM t WHERE id IN (SELECT uid FROM orders) AND EXISTS (SELECT 1 FROM x)",
    "SELECT a FROM t UNION ALL SELECT b FROM t2",
    "(SELECT a FROM t) UNION ALL (SELECT b FROM t2)",
    "SELECT ROW_NUMBER() OVER (PARTITION BY a ORDER BY b) FROM t",
    "SELECT SUM(x) OVER (ORDER BY a RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM t",
    "SELECT * FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = 'app_db'",
    "SELECT 1;",
    "SeLeCt 1",
    "/*block*/ SELECT 1",
    "SELECT 1 -- trailing comment",
    "SELECT 1;-- 尾随注释",
    "SELECT/**/1",
    "SELECT `col` FROM `db`.`tbl` WHERE id = 1",
    "SELECT * FROM JSON_TABLE('[1]', '$[*]' COLUMNS(v INT PATH '$')) jt",
    "SELECT * FROM t WHERE MATCH(a) AGAINST('x' IN BOOLEAN MODE)",
    "SELECT col->'$.key', col->>'$.a' FROM t",
    "SELECT * FROM t WHERE a <=> b",
    "SELECT HIGH_PRIORITY a FROM t",
    "SELECT STRAIGHT_JOIN a FROM t",
    "SELECT 1 FROM t1 STRAIGHT_JOIN t2 ON t1.a = t2.b",
    "SELECT * FROM t USE INDEX (i) WHERE a = 1",
    "SELECT * FROM t IGNORE INDEX FOR ORDER BY (i)",
    "SELECT * FROM t USE KEY (i)",
    "SELECT /*M!50701 SELECT 2 */ 1",
    "SELECT b'101', 0xFF",
    "SELECT b'010' + 0, 0x41 + 0",
    "SELECT * FROM t WHERE a LIKE '%x%' ESCAPE '!'",
    "SELECT 1 FROM t WHERE a LIKE '%\\\\_%' ESCAPE '\\\\'",
    "SELECT CONVERT(a, CHAR) FROM t",
    "SELECT * FROM t WHERE CONVERT(a USING utf8mb4) = 'x'",
    "SELECT CAST('a' AS DECIMAL(10,2))",
    "SELECT * FROM t LIMIT 1, 2",
    "SELECT 1 FROM DUAL",
    "SELECT a FROM t GROUP BY a WITH ROLLUP",
    "SELECT * FROM t1 JOIN t2 USING (col)",
    "SELECT * FROM (t1 JOIN t2 ON t1.a = t2.a) x",
    "SELECT * FROM t1 NATURAL JOIN t2",
    "SELECT * FROM t WHERE a RLIKE 'x'",
    "SELECT * FROM t ORDER BY a DESC, b",
    "SELECT * FROM t WHERE a BETWEEN 1 AND 2",
    "SELECT DISTINCT(a) FROM t",
    "SELECT * FROM t WHERE _utf8'a' = a",
    "SELECT 1 FROM t WHERE a = ANY (SELECT b FROM c)",
    "SELECT a FROM t ORDER BY 1",
    "SELECT 1 FROM t HAVING a",
    "SELECT @x",
    "SELECT NOW() + INTERVAL 1 DAY",
    "SELECT ROW(1,2) = (SELECT a,b FROM t LIMIT 1)",
    "SELECT TRIM(BOTH 'x' FROM a) FROM t",
    "SELECT EXTRACT(YEAR FROM d) FROM t",
    "SELECT GROUP_CONCAT(x ORDER BY y SEPARATOR ',') FROM t GROUP BY z",
    "SELECT COUNT(DISTINCT a) FROM t",
    // 关键词型函数名不得被当成语句类型误杀（spike ROUND3 / §2.1）。
    "SELECT INSERT(a,1,2,'x') FROM t",
    "SELECT REPLACE(a,'x','y'), LEFT(a,2), RIGHT(a,2), IFNULL(a,0) FROM t",
    "SELECT SCHEMA(), DATABASE(), CURRENT_USER",
    "SELECT * FROM t WHERE 1 MEMBER OF ('[1,2]')",
    "SELECT COUNT(*) FROM t GROUP BY a ORDER BY COUNT(*) DESC",
];

/// 解析成功、由策略层显式拒绝（`mcp.readonly_violation`）。
const NEGATIVE_VIOLATION: &[&str] = &[
    // —— DML ——
    "INSERT INTO t VALUES (1)",
    "INSERT INTO t SELECT * FROM u",
    "UPDATE t SET a = 1",
    "DELETE FROM t WHERE a = 1",
    "REPLACE INTO t VALUES (1)",
    // —— DDL ——
    "CREATE TABLE t (a INT)",
    "CREATE INDEX i ON t (a)",
    "CREATE VIEW v AS SELECT 1",
    "CREATE SEQUENCE s",
    "DROP TABLE t",
    "ALTER TABLE t ADD COLUMN b INT",
    "TRUNCATE TABLE t",
    "GRANT SELECT ON db.* TO u",
    // —— 存储过程 / 会话 / 事务 / 锁表 ——
    "CALL p()",
    "SET x = 1",
    "SET @@global.max_connections = 100",
    "USE db",
    "SHOW TABLES",
    "BEGIN",
    "START TRANSACTION",
    "COMMIT",
    "ROLLBACK",
    "LOCK TABLES t READ",
    // —— 多语句（ADR 0006 单语句口径）——
    "SELECT 1; SELECT 2",
    "SELECT 1; INSERT INTO t VALUES (1)",
    // —— INTO 全形态（AST 检出；含递归位置）——
    "SELECT 1 INTO @x",
    "SELECT 1 INTO var",
    "SELECT 1 INTO a, b",
    "SELECT 1 INTO @a, @b",
    "SELECT 1 INTO @a FROM DUAL",
    "SELECT * FROM (SELECT 1 INTO @x) t",
    "WITH c AS (SELECT 1 INTO @x) SELECT * FROM c",
    "SELECT 1 UNION SELECT 2 INTO @x",
    "SELECT * FROM t WHERE id IN (SELECT 1 INTO @x)",
    // —— 数据修改 CTE（嵌套 Statement）——
    "WITH c AS (INSERT INTO t VALUES (1)) SELECT 1",
    "WITH c AS (UPDATE t SET a = 1) SELECT 1",
    "WITH c AS (DELETE FROM t) SELECT * FROM c",
    // —— 锁定读全变体（Query.locks；含嵌套 IN 子查询）——
    "SELECT * FROM t FOR UPDATE",
    "SELECT * FROM t FOR SHARE",
    "SELECT * FROM t FOR UPDATE SKIP LOCKED",
    "SELECT * FROM t FOR UPDATE NOWAIT",
    "SELECT a FROM t FOR SHARE NOWAIT",
    "SELECT * FROM t FOR UPDATE OF t",
    "SELECT * FROM t WHERE id IN (SELECT id FROM u FOR UPDATE)",
    // —— 危险函数：默认表 × 大小写 × 限定名 × 反引号 × 嵌套位置 ——
    "SELECT LOAD_FILE('/etc/passwd')",
    "SELECT SLEEP(100)",
    "SELECT BENCHMARK(100000000, MD5('test'))",
    "SELECT GET_LOCK('test_lock', 10)",
    "SELECT RELEASE_LOCK('test_lock')",
    "SELECT RELEASE_ALL_LOCKS()",
    "SELECT load_file('/x')",
    "SELECT SLeEp(1)",
    "SELECT `SLEEP`(1)",
    "SELECT mysql.SLEEP(1)",
    "SELECT * FROM (SELECT SLEEP(1)) x",
    "SELECT * FROM t WHERE LOAD_FILE('/x') IS NOT NULL",
    "WITH c AS (SELECT SLEEP(1)) SELECT * FROM c",
    "SELECT IF(1,2,SLEEP(3))",
    "SELECT COUNT(*) FROM t1 JOIN t2 ON GET_LOCK('g',1)",
    // —— 副作用扩展表（§8.3「其他已确认的副作用函数」+ spike 偏差项，见 ADR 0033）——
    "SELECT NEXTVAL(s)",
    "SELECT @a := 1",
    // —— 优化器 hint ——
    "SELECT /*+ MAX_EXECUTION_TIME(1000) */ 1",
    "SELECT /*+ JOIN_ORDER(t2, t1) */ * FROM t1 JOIN t2 ON t1.a = t2.b",
    // —— 执行型语句（解析成功，白名单拒；用户不得自带 EXPLAIN/ANALYZE）——
    "EXPLAIN SELECT 1",
    "EXPLAIN ANALYZE SELECT 1",
    "ANALYZE TABLE t",
];

/// 当前依赖版本（sqlparser 0.64.0）解析失败 → fail-closed 拒绝（`mcp.sql_parse`）。
///
/// **跟进合同**：以下每一项在真实 MariaDB 上都是可执行语句。今天靠「解析失败」兜底
/// 拒绝；若上游 sqlparser 未来能解析这些形态，本测试会红——届时必须在策略层
/// （`ast.rs`）补显式拒绝，而**不是**把期望改成放行。其中前三类（LOCK IN SHARE MODE /
/// INTO OUTFILE / INTO DUMPFILE）与 `ANALYZE SELECT`（可能真实执行查询）属于
/// 「语义上本就应拒」的类别，被解析出来也必须继续拒绝。
const NEGATIVE_FAIL_CLOSED: &[&str] = &[
    // 锁定读变体（MySQL/MariaDB 合法；锁语义未建模，解析失败兜底）。
    "SELECT * FROM t LOCK IN SHARE MODE",
    "sElEcT * fRoM t LoCk In ShArE mOdE",
    "SELECT * FROM (SELECT 1 FOR UPDATE) x",
    // INTO OUTFILE / DUMPFILE 家族（SelectInto 无对应字段，解析失败兜底）。
    "SELECT 1 INTO OUTFILE '/tmp/result'",
    "SELECT * INTO DUMPFILE '/tmp/b'",
    "SELECT * INTO OUTFILE '/tmp/a' FIELDS TERMINATED BY ','",
    "SELECT 1 FROM DUAL INTO @a",
    // ANALYZE SELECT 可能真实执行查询（任务书 §6 特别注意项），解析失败兜底。
    "ANALYZE SELECT 1",
    "ANALYZE FORMAT=JSON SELECT * FROM t",
    // MariaDB 特有 DDL/扩展：今天解析失败即拒；若未来可解析，仍是白名单外语句。
    "ALTER SEQUENCE s INCREMENT BY 2",
    "CREATE TABLE t (a INT, INDEX i (a) INVISIBLE)",
    "ALTER TABLE t ALTER INDEX i INVISIBLE",
    // 已知唯一「纯只读误杀」（spike §6 ④）：合法、只读、极罕见的 MariaDB 形态。
    // 固化当前行为（解析失败→拒），若未来可解析需评审是否显式放行——见 ADR 0033。
    "SELECT * FROM t PROCEDURE ANALYSE()",
];

fn deny_code(sql: &str) -> ErrorCode {
    check_readonly(sql)
        .expect_err("负向矩阵条目必须被拒绝")
        .code()
}

#[test]
fn positive_matrix_allowed() {
    assert!(
        POSITIVE.len() >= 20,
        "正向矩阵不足 20 例：{}",
        POSITIVE.len()
    );
    for sql in POSITIVE {
        let checked = check_readonly(sql).unwrap_or_else(|e| {
            panic!(
                "正向条目被误杀：{sql}\n  {}({})",
                e.code().as_str(),
                e.message()
            );
        });
        assert_eq!(checked.sql, *sql, "策略不得改写原始 SQL");
    }
}

#[test]
fn negative_matrix_denied_as_readonly_violation() {
    assert!(
        NEGATIVE_VIOLATION.len() >= 35,
        "负向矩阵不足 35 例：{}",
        NEGATIVE_VIOLATION.len()
    );
    for sql in NEGATIVE_VIOLATION {
        assert_eq!(
            deny_code(sql),
            ErrorCode::MCP_READONLY_VIOLATION,
            "策略拒绝条目错误码不符：{sql}"
        );
    }
}

#[test]
fn fail_closed_matrix_denied_as_parse_error() {
    for sql in NEGATIVE_FAIL_CLOSED {
        assert_eq!(
            deny_code(sql),
            ErrorCode::MCP_SQL_PARSE,
            "fail-closed 条目错误码不符：{sql}（若上游 sqlparser 已能解析该形态，请到 ast.rs 补显式拒绝并移桶，禁止改成放行）"
        );
    }
}

#[test]
fn rejection_messages_are_agent_facing_and_classified() {
    // Error messages use English and identify the reason for rejection.
    let cases: &[(&str, &str)] = &[
        ("SELECT 1 INTO @x", "INTO clause"),
        ("SELECT * FROM t FOR UPDATE", "Locking reads"),
        ("SELECT GET_LOCK('l', 1)", "Disallowed function"),
        ("INSERT INTO t VALUES (1)", "read-only allowlist"),
        ("SELECT 1; SELECT 2", "one SQL statement"),
        (
            "SELECT /*+ MAX_EXECUTION_TIME(1000) */ 1",
            "Optimizer hints",
        ),
        ("SELECT @a := 1", "Session variable assignment"),
    ];
    for (sql, keyword) in cases {
        let message = check_readonly(sql)
            .expect_err("必须拒绝")
            .message()
            .to_string();
        assert!(
            message.contains(keyword),
            "拒绝消息应包含分类词「{keyword}」：{message}"
        );
    }
    let parse_message = check_readonly("SELECT * FROM t LOCK IN SHARE MODE")
        .expect_err("必须拒绝")
        .message()
        .to_string();
    assert!(
        parse_message.contains("Failed to parse SQL"),
        "{parse_message}"
    );
}

#[test]
fn explain_matrix_never_bypasses_policy() {
    // 正向：只读语句可 EXPLAIN，前缀由策略层拼接，指纹仍属原 SQL。
    for sql in ["SELECT 1", "WITH c AS (SELECT 1) SELECT * FROM c"] {
        let checked = check_explain(sql)
            .unwrap_or_else(|e| panic!("EXPLAIN 正向被误杀：{sql}\n  {}", e.message()));
        assert_eq!(checked.sql, format!("EXPLAIN {sql}"));
        assert_eq!(
            checked.fingerprint,
            check_readonly(sql).expect("同句应可过只读策略").fingerprint
        );
    }
    // 负向：全部拒绝条目经 check_explain 同样被拒（错误不得绕过策略）。
    for sql in NEGATIVE_VIOLATION {
        let error = check_explain(sql).expect_err("EXPLAIN 不得绕过策略");
        assert_eq!(error.code(), ErrorCode::MCP_READONLY_VIOLATION, "{sql}");
    }
    for sql in NEGATIVE_FAIL_CLOSED {
        let error = check_explain(sql).expect_err("EXPLAIN 不得绕过 fail-closed");
        assert_eq!(error.code(), ErrorCode::MCP_SQL_PARSE, "{sql}");
    }
}

#[test]
fn deep_nesting_hits_default_recursion_limit() {
    // sqlparser 默认 recursion limit=50（spike §6：约 24 层派生表触顶）。
    // 刻意不调高：这是 DoS 边界（ADR 0033）。触顶按 mcp.sql_parse fail-closed 拒。
    let nested = |depth: usize| {
        let mut sql = String::from("SELECT 1");
        for _ in 0..depth {
            sql = format!("SELECT x FROM ({sql}) AS d");
        }
        sql
    };
    check_readonly(&nested(10)).expect("10 层嵌套属正常查询，必须放行");
    assert_eq!(
        deny_code(&nested(40)),
        ErrorCode::MCP_SQL_PARSE,
        "深嵌套触顶必须按解析失败 fail-closed 拒绝"
    );
}

#[test]
fn many_flat_ctes_not_affected_by_depth_limit() {
    let ctes: Vec<String> = (0..100)
        .map(|i| format!("c{i} AS (SELECT {i} AS n)"))
        .collect();
    check_readonly(&format!("WITH {} SELECT * FROM c99", ctes.join(", ")))
        .expect("100 个平铺 CTE 不触递归上限，必须放行");
}

#[test]
fn fingerprint_contract() {
    // 同形变体同指纹（spike §5 实测对照）。
    let a = check_readonly("SELECT * FROM users WHERE id = 123 AND name = 'alice'")
        .expect("放行")
        .fingerprint;
    let b = check_readonly("SELECT * FROM users WHERE id = 456 AND name = 'bob'")
        .expect("放行")
        .fingerprint;
    assert_eq!(a, b);
    assert_eq!(
        a.normalized,
        "SELECT * FROM users WHERE id = ? AND name = ?"
    );
    // unparse 幂等：指纹文本再过一遍策略，指纹不动点。
    let again = check_readonly(&a.normalized)
        .expect("指纹文本必须可再解析")
        .fingerprint;
    assert_eq!(again, a);
    // IN 列表长度参与指纹（不折叠，ADR 0033 已知行为）。
    let three = check_readonly("SELECT a FROM t WHERE x IN (1, 2, 3) ORDER BY a")
        .expect("放行")
        .fingerprint;
    let two = check_readonly("SELECT a FROM t WHERE x IN (7, 8) ORDER BY a")
        .expect("放行")
        .fingerprint;
    assert_ne!(three.sha256_hex, two.sha256_hex);
    // 注释、大小写、空白在指纹层归一。
    let plain = check_readonly("SELECT 1").expect("放行").fingerprint;
    let noisy = check_readonly("/*c*/ sElEcT\n 1;-- x")
        .expect("放行")
        .fingerprint;
    assert_eq!(plain, noisy);
}
