# ADR 0033：SQL Parser 只读安全策略（sqlparser-rs / MySqlDialect）

- 状态：已接受（2026-11-17，随 Issue #208 实施）
- 日期：2026-11-17
- 关联：任务书 V3.1 §6（EXPLAIN 流程）、§8（SQL AST 与只读安全）、§12（Audit / SQL Fingerprint）；
  [ADR 0006](0006-multi-statement-policy.md)（单语句口径）、ADR 0031（SQL Parser 选型本体，
  并行泳道落档，本 ADR 不重复选型比较，只固化**策略语义**）；
  AGENTS.md §9（安全与隐私）、§15（依赖引入确认单）
- 上游：Issue #208；阶段 A 实测 spike（170+ 输入判定矩阵，`scratch/mcp-spike-sqlparser/report.md`）
- 实现落点：`backend/apps/sqarvo-mcp/src/policy/{mod,ast,fingerprint}.rs`；
  负面回归合同：`backend/apps/sqarvo-mcp/tests/policy_snapshots.rs`

## 背景

MCP 通道对外暴露 `execute_readonly_query` / `explain_query`，Agent 提交的 SQL 必须在
下发前判定为**单条只读查询**。V3.1 §8 要求 AST 级判定（不得只查首关键字），拒绝清单
包括 DML/DDL/CALL/SET/USE/SHOW/事务控制/锁定读/一切 INTO 形态/多语句/危险函数，
并要求「无法识别或不支持的形态采取保守策略」（fail-closed）。

判定依赖 `sqlparser-rs` 0.64.0 + `MySqlDialect`（选型见 ADR 0031）。本 ADR 记录
策略层的语义决定、方言差异清单与已知边界——这些都是负面测试断言的一部分。

## 备选方案

1. **首关键字 / 正则黑名单**：被 §8 明文排除；`SELECT ... INTO OUTFILE`、数据修改 CTE、
   嵌套锁读、注释/大小写变体全部可绕。
2. **手写 AST 递归（顶层 + CTE）**：spike 实测漏掉 FROM 派生表与 WHERE 子查询里的
   `INTO`/`SLEEP()`（`SELECT * FROM (SELECT 1 INTO @x) t` 被放行），是最小反例。
3. **sqlparser `visitor` 全树遍历**（本 ADR 采用）：`pre_visit_statement/query/select/expr`
   四钩子一次遍历覆盖任意深度；上游新增 AST 节点默认被 visitor 走到，白名单 `_ =>`
   兜底拒绝未来新增语句类型。
4. **解析失败视为可放行（best-effort 兼容 MariaDB 语法）**：直接违背 fail-closed 要求，
   `LOCK IN SHARE MODE` / `INTO OUTFILE` / `ANALYZE SELECT` 恰恰全靠解析失败兜底。

## 决策

1. **入口与错误码分界**（公开签名 `check_readonly` / `check_explain` 为跨泳道契约）：
   - `Parser::parse_sql(&MySqlDialect{}, sql)` Err → `mcp.sql_parse`（消息含 ParserError
     原文，中文说明 fail-closed；`category=InvalidArgument`）；
   - 语句数 ≠ 1 → `mcp.readonly_violation`（多语句，ADR 0006）；
   - 顶层 match 穷举：仅 `Statement::Query` 进入深检，其余（含 `Statement::Explain` /
     `Analyze` / 全部 DML/DDL，以及未来新增变体）→ `mcp.readonly_violation`，
     消息列出被拒语句类型名；禁止按首关键字判断。
2. **深检 = 一次 visitor 全树遍历**，覆盖：嵌套非 Query 语句（数据修改 CTE，
   `SetExpr::Insert/Update/Delete`）、`Query.locks`（FOR UPDATE/SHARE/NOWAIT/SKIP LOCKED/OF
   全拒）、`Select.into`（任何 targets 一律拒）、`Select.optimizer_hints`（非空即拒——
   hint 是不可静态验证的任意计划指令）、危险函数、`:=` 会话变量赋值。
3. **危险函数默认表**：`LOAD_FILE / SLEEP / BENCHMARK / GET_LOCK / RELEASE_LOCK /
   RELEASE_ALL_LOCKS`（V3.1 §8.3 钉死），另按 §8.3「其他已确认的副作用函数」补
   `NEXTVAL`（推进序列 = 状态修改；spike 曾放行，本策略收紧）。匹配规则：`ObjectName`
   **末段** `to_lowercase`（覆盖 `db.fn`、反引号、任意大小写）；动态名
   （`ObjectNamePart::Function`）与空函数名无法静态判定 → 保守拒绝。表为模块内常量，
   扩展属策略配置，新增条目必须同 PR 附负面测试。
4. **递归深度**：保持 sqlparser 默认保护（limit=50，约 24 层派生表触顶），触顶按
   `mcp.sql_parse` 拒绝——刻意不调高：这是与 10s 超时、行数上限叠加的 DoS 边界；
   测试固化「10 层放行、40 层拒绝、100 个平铺 CTE 不受影响」。
5. **EXPLAIN 只有一条路**：`check_explain(sql)` = `check_readonly(sql)` 通过后拼
   `EXPLAIN ` 前缀；指纹按**原 SQL** 计算（审计不归因前缀）。用户自带 `EXPLAIN` /
   `ANALYZE` 形态在顶层白名单就拒；`ANALYZE SELECT`（MariaDB 会真实执行）靠解析失败
   兜底拒。不存在「先拼前缀再检查」的旁路，负面矩阵经 `check_explain` 重跑一遍。
6. **Fingerprint（§12）**：visitor `visit_expressions_mut` 把数据字面量替换为
   `Value::Placeholder("?")` → `Display` unparse → SHA-256 小写 hex。unparse 天然丢弃
   注释、归一空白与引号形态，`normalized` 幂等（测试固化同形变体同指纹、再解析不动点）。
   **IN 列表长度参与指纹**（3 元素与 2 元素是不同指纹）——与 pt-fingerprint 不同，
   记为已知行为：V1 审计按语句形态聚合，长度收敛如需再做属指纹层增量，不破坏哈希合同。
7. **错误消息规范**：中文、面向 Agent、含拒绝原因分类词（「INTO 子句」「锁定读」
   「危险函数」「优化器 hint」「会话变量赋值」「只读白名单」「解析失败」），
   不回显连接信息与凭据；`mcp.*` 子码复用 common 既有注册，未新增错误码。
8. **版本与依赖面**（选型合同补充）：sqlparser 精确评估版本 **0.64.0**；许可证
   **Apache-2.0 单一许可**（非 Apache/MIT 双许可，发布物需履行 NOTICE 义务——归 #212
   文档/打包泳道落 NOTICE 文件）；官方**无 MSRV 声明**（Cargo.toml 无 `rust-version`），
   实测 workspace rust-version（1.99.1）编译通过；运行时依赖仅 `log` + `recursive`
   （→ `stacker`/`psm`/`libc`，`psm` 含汇编产物，**Windows CI 构建待 #211 验证**，
   为已知风险）；`visitor` feature 只增加编译期 proc-macro。升级策略：workspace 锁
   `0.64`（semver 内小版本变化也必须重跑 `policy_snapshots` 全套，理由见「重估条件」）。
9. **MySqlDialect ≠ MariaDB**：0.64.0 不存在 `MariaDbDialect`，按任务书以 `MySqlDialect`
   为准。解析失败（→ `mcp.sql_parse` 拒）的 MariaDB 合法语法清单（负面矩阵逐项固化）：
   `LOCK IN SHARE MODE`（含大小写变体）、派生表内 `FOR UPDATE`、
   `INTO OUTFILE/DUMPFILE`（含 `FIELDS` 变体、`FROM DUAL ... INTO`）、
   `ANALYZE SELECT` / `ANALYZE FORMAT=JSON SELECT`、`ALTER SEQUENCE`、
   `INDEX ... INVISIBLE`（CREATE/ALTER 两形态）、`PROCEDURE ANALYSE()`。
   前六类目标语义本就属应拒类别（误杀无害）；**`PROCEDURE ANALYSE()` 是唯一真正的
   只读误杀**（合法、只读、极罕见），接受该误杀，不做例外放行。
10. **fail-closed 边界 = 跟进合同**：上述「靠解析失败拒」的四类（LOCK IN SHARE MODE /
    INTO OUTFILE / INTO DUMPFILE / ANALYZE SELECT）在测试里以 `mcp.sql_parse` 精确断言并
    写明——若上游未来能解析它们，测试必然变红，届时**必须**在 `ast.rs` 补显式策略拒绝、
    移桶，禁止顺手改成放行。顶层 `_ =>` 与 visitor 穷举保证新语句形态默认拒。

## 代价与权衡

- **误杀换边界确定性**：方言差异清单（决策 9）里的语句全部拒。换来的是一条可测试、
  可穷举的静态边界；AST 检查也不是最终防线——只读账号、只读事务、超时、行数/字节
  上限继续叠加（V3.1 §8.4）。
- **每次判定两次遍历**（visitor 扫描 + 指纹掩码）+ 一次 AST clone：spike 实测 5000 元素
  IN 列表解析+判定为毫秒级，且被 10s 超时与递归上限双重罩住，不值得为省一次 clone
  引入共享可变 visitor 复杂度。
- **危险函数按名匹配不看语义**：自定义 UDF 恰好叫 `SLEEP` 也拒（宁可误拒）；反之
  新增副作用函数不在表内则依赖只读账号权限兜底（表是纵深之一，非全部）。
- **hint 一律拒**：牺牲了 Agent 用 `MAX_EXECUTION_TIME` 自救的可能——但 hint 文本不解析、
  不可静态验证，V1 不开口子。
- **多语句只给一个错误**：`SELECT 1; SELECT 2` 拒整条，不做「截取第一条」——静默改写
  Agent 输入比拒绝更危险。

## 验收证据

- `tests/policy_snapshots.rs`：正向 61 例放行、策略拒绝 66 例（逐条断言
  `mcp.readonly_violation`）、fail-closed 13 例（逐条断言 `mcp.sql_parse`）、
  EXPLAIN 旁路矩阵（负向 79 例经 `check_explain` 重跑同码拒绝）、消息分类词断言、
  递归触顶/平铺 CTE、指纹合同（同形同指纹、幂等、IN 长度参与、注释/大小写/空白归一）。
- 单元 13 项 + 集成 8 项全绿；`cargo fmt --all -- --check`、
  `cargo clippy -p sqarvo-mcp --all-targets` 零警告；测试全离线（无网络/无 DB）。

## 重估条件

- **sqlparser 版本升级（含 0.64.x 内小版本）**：解析失败清单会漂移——`policy_snapshots`
  的 fail-closed 桶变红即是信号，逐项评审「可解析后是否必须显式拒」，并按 0.64 的
  教训补 NOTICE/依赖检查。
- **上游出现 MariaDbDialect 或需要 `derive_dialect!` 派生**：决策 9 的整张差异清单
  需重测；在此之前不得宣称支持 MariaDB 方言。
- **审计要求跨长度聚合 IN 列表**（pt-fingerprint 式收敛）：改 `fingerprint.rs` 的
  normalized 形态即改变全部既有指纹值，需修订本 ADR 并评估审计数据的断代点。
- **危险函数表需要运行期配置**（mcp.json 扩展）：当前为编译期常量；若开放配置必须
  只允许**增项**、禁止删项，且走配置校验（`config.invalid`）。
- **Windows CI 上 `psm`/`stacker` 构建失败**：回退方案为 features 中去掉
  recursive-protection 并自行设语句长度/超时边界——需新 ADR 修订，因为 fail-closed
  语义从「递归上限」变成「输入规模上限」。
