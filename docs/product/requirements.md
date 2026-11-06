# 产品需求

本文件定义功能范围与优先级，是路线图与里程碑的上游依据。

- 优先级：**P0** 首个可用版本必须；**P1** 后续版本；**P2** 已规划但未排期。
- 状态：**已实现**（代码与测试均存在）、**部分**（契约或部分逻辑已存在，尚不可用）、
  **未实现**。任何未实现能力不得在 UI 中表现为可用。
- 验收标准写在需求行内；具体任务拆解在 GitHub Issues，不在本文件重复维护。

## 1. 连接与凭据

| ID | 需求 | 优先级 | 状态 | 验收要点 |
|---|---|---|---|---|
| REQ-CONN-001 | 配置连接（驱动、主机、端口、库、用户、TLS 模式） | P0 | 部分 | 后端 SQLite（本地文件 + 只读）与 MariaDB（主机/端口/库/用户/5 档 TLS 模式）驱动可用；`credential_ref` 在连接与测试连接前解析（#23，PR #33）；Linux 前端连接对话框（#27，PR #37）只提供 MariaDB，SQLite 无 UI；Windows/macOS 未接入 |
| REQ-CONN-002 | 测试连接（延迟、服务端版本、告警） | P0 | 部分 | SQLite 与 MariaDB 实测可用（`cargo test -p sqarvo-driver-mariadb --test integration` 的 `test_connection_reports_version`；MariaDB 的 TLS 回退给出 `mariadb.tls_fallback` 告警），Linux 前端对话框展示延迟与版本（PR #37）；PostgreSQL 与未注册驱动返回 `driver.not_found` |
| REQ-CONN-003 | 连接 / 断开 / 会话列表 | P0 | 部分 | 后端连接/断开/列表在 SQLite 上有 gRPC 端到端测试（`grpc_sqlite_e2e`），MariaDB 由 `grpc_mariadb_credentials` 的真实库连接覆盖，断连后会话与查询登记归零；Linux 前端（PR #37）可建立会话并用 `ListSessions` 刷新列表，但**没有会话级断开入口**（只有「断开后端」整条连接）；Windows/macOS 无 UI（见 REQ-PLAT-001） |
| REQ-CONN-004 | 操作系统凭据存储（Keychain / Credential Manager / Secret Service） | P0 | 部分 | Linux Secret Service 已实现（#21，PR #31）：`cargo test -p sqarvo-local-store` 33 单测 + 4 集成；`StoreCredential` / `DeleteCredential`（#27，PR #37）：`cargo test -p sqarvo-rpc-server --test grpc_credential` 11 通过；真实密钥环用例由 `SQARVO_SECRET_SERVICE_TEST=1` 门控，本次未运行（未验证）；Windows / macOS 未实现，装配为「不可用」且不回退明文；`credential_ref` 已在连接/测试连接路径解析（#23） |
| REQ-CONN-005 | 连接分组、收藏、最近使用 | P2 | 未实现 | — |
| REQ-CONN-006 | 连接超时、重连与断线状态呈现 | P1 | 部分 | 断线状态可见：Linux 前端每 5 秒 `GetHealth`，失败进入 Disconnected（PR #36）；连接超时由驱动的 `mariadb.connect_timeout` / 客户端 30 秒上限呈现，但**不可配置**；自动重连未实现 |

## 2. 数据库对象与元数据

| ID | 需求 | 优先级 | 状态 | 验收要点 |
|---|---|---|---|---|
| REQ-META-001 | 对象树按需加载（Catalog / Schema / 表 / 视图 / 序列等） | P0 | 部分 | **两驱动已交付 Catalog → 表/视图/序列 → 列/索引/触发器 的逐层懒加载**（#82，PR #94 / `43373e7`）：SQLite 的 catalog 为 `main` / `temp` / `ATTACH`，MariaDB 的 database 即 catalog；跨库引用必须显式给出 catalog，子节点的 catalog 由服务端填满并可原样回传给 `GetColumns` / `GetPrimaryKey`；不存在的库/表 `metadata.object_not_found`、无权限 `metadata.permission_denied`、不存在的层级 `capability.unsupported`、种类不适用 `metadata.object_kind_unsupported`（都不返回空列表）；每层分页按（种类, 名字）排序，键含种类与父节点作用域。`cargo test -p sqarvo-driver-sqlite --test metadata_hierarchy` → 8 通过；`SQARVO_MARIADB_URL=… cargo test -p sqarvo-driver-mariadb --test integration` → 69 通过（含 `metadata_catalog_*` / `metadata_hierarchy_*` 等层级与跨库用例）。Linux 前端可折叠对象树（#86，PR #98 / `a6008c1`）与对象搜索（#84，见 REQ-META-004）已可用。**剩余缺口**：schema 层没有任何驱动实现——SQLite / MariaDB 本就没有独立 schema，PostgreSQL 驱动未实现，请求 schema 一律 `capability.unsupported`；MariaDB 受限账号下「库不存在」与「无权限」都只拿到 1044，只能报 `metadata.permission_denied` 并在文案里说明歧义 |
| REQ-META-002 | 列信息（原始类型、精度、可空、默认值） | P0 | 已实现 | `GetColumns` 返回原始类型名（SQLite 声明类型 / MariaDB `COLUMN_TYPE`，含长度与 `unsigned`）、可空性、MariaDB 的精度与标度、**默认值**（四态：未指定 / 明确无默认 / 默认 NULL / 未求值表达式，空串默认值不等于「无默认值」）、以及 MariaDB 的字符集与排序规则（`information_schema.COLUMNS`）。SQLite 没有列级字符集/排序规则概念，proto 用 `optional` 字段留空表示「不适用」，不用空串冒充。SQLite `columns_preserve_declared_type_and_nullability`、`columns_expose_default_value_and_only_what_sqlite_knows`（逐列断言 NoDefault/Null/Expression）；MariaDB 集成用例覆盖 `COLUMN_TYPE`、默认值与 `CHARACTER_SET_NAME`/`COLLATION_NAME`（#45、#59，PR #54 / `100f43d`、PR #64 / `c78d6b6`）。**前端**：Linux 数据页点列头打开列信息浮层，逐列展示默认值四态 / 字符集 / 排序规则，缺省字段显示「不适用」（#87，PR #92 / `bcb12c4`）。**剩余**：`ENUM` / `SET` / `GEOMETRY` / `BIT` 以 `ColumnType::Unsupported { type_name }` 保留原文；SQLite 的列排序规则仍恒为 `None`（`pragma_table_info` 不提供，且本版不解析 DDL） |
| REQ-META-003 | 查看 DDL | P1 | 已实现 | `GetDdl` 返回 `ddl / exact / source / source_detail / generator_version`；两驱动都只交付数据库原文（`exact=true`、`source=DDL_SOURCE_DATABASE`），**没有客户端拼装路径**；`exact=false` 时 rpc-server 还要求来源与生成器版本（`is_consistent()`），说不清来源就以 `internal.error` 拒绝。SQLite 覆盖表/视图/索引/触发器（`sqlite_master.sql` 原文；`sqlite_autoindex_*` 无原文 → `capability.unsupported`）；MariaDB 覆盖表/视图/序列（`SHOW CREATE` **第 2 列**原文并校验 `CREATE` 前缀；索引/触发器无独立 DDL → `metadata.object_kind_unsupported`）。`GetDdl` 纳入会话执行锁（`grpc_session_locks_e2e` 8 通过，含 `ddl_holds_lock_against_apply` / `ddl_waits_for_apply`）。DDL 不截断，超过 gRPC 消息上限（tonic 默认 4 MiB）时请求失败而非返回半截文本。验证：SQLite `--test metadata` 13 通过（其中 5 个 `ddl_*`）、MariaDB 集成 69 通过（4 个 `metadata_ddl_*`）、`grpc_metadata_e2e` 1 通过（断言 `exact=true` 与来源）；Linux DDL 面板（#86）显式标注「原文 / 由客户端拼装」。**剩余**：MariaDB 索引/触发器 DDL 需要 `ObjectRef` 携带所属表（契约增量，待定）；没有业务层体积上限错误码（超限是传输层失败） |
| REQ-META-004 | 对象搜索与过滤 | P1 | 已实现 | `ListNodesRequest.filter`（`contains` / `exact` 同一 oneof + `kinds`）**下推到数据库**（#84，PR #96 / `8e7c661`）：SQLite `LIKE … ESCAPE '\'`、MariaDB `LIKE CONCAT('%', ?, '%')`，`%` / `_` / `\` 按字面量转义；过滤结果 token 为 `v2:<offset>:<集合指纹>:<过滤标签>`，换关键词 / 换匹配方式 / 增删 kinds → 旧 token `config.invalid`（不回退第一页）。SQLite `--test metadata_filter` 13 通过（+1 ignored 规模基准）、MariaDB `--test metadata_filter` 9 通过（+1 ignored）。**大小写差异（文档写明，不统一）**：`contains`（`LIKE`）在 SQLite 只折叠 ASCII、在 MariaDB 走 `utf8mb3_general_ci`（连重音一起折叠）；`exact`（`=`）两驱动都是二进制比较、区分大小写。**性能**：SQLite 10 万对象命中 3 个 9.3 ms（release，`#[ignore]` 基准，#84 记录）；MariaDB 只测 1 万对象 7.2 ms（10 万未实测，见发布计划「未验证项」）。**剩余**：Linux 搜索只覆盖当前库根层、只展示首页 500 条；`NO_BACKSLASH_ESCAPES` 下 MariaDB 默认转义符失效，本版不处理 |
| REQ-META-005 | 表/视图数据预览 | P1 | 已实现 | `TableDataService.BrowseTable`：固定主键升序 + 不透明 cursor（禁止 `OFFSET`），每页上限 500，游标非法 → `table_data.cursor_invalid` 且不回退第一页；无主键表/视图返回 `read_only=true`（原因 `NoPrimaryKey`、排序 `Unstable`）。SQLite `cargo test -p sqarvo-driver-sqlite --test table_data`（9 用例）、MariaDB 集成用例、gRPC `cargo test -p sqarvo-rpc-server --test grpc_table_data_e2e`（#46，PR #55 / `8775dda`）；Linux 数据页按主键翻页并显示原始类型列头 |

## 3. SQL 编辑器与执行

| ID | 需求 | 优先级 | 状态 | 验收要点 |
|---|---|---|---|---|
| REQ-SQL-001 | 多标签 SQL 编辑器：高亮、行号、选区、缩进、查找替换 | P0 | 未实现 | 当前是纯文本多行等宽编辑器（PR #38），界面明确标注「语法高亮将在后续版本提供」；无多标签、行号、查找替换、历史 |
| REQ-SQL-002 | 执行全部 / 执行当前语句 | P0 | 部分 | 后端执行、语句摘要与分批已端到端验证（SQLite 与 MariaDB）；Linux 前端 `Ctrl+Enter` 执行**整段** SQL（PR #38）；「执行当前语句」未实现（无光标语句识别） |
| REQ-SQL-003 | 取消执行（幂等，传播到驱动） | P0 | 部分 | 驱动中断与 gRPC 幂等取消已端到端验证（2 秒内，MariaDB 用 `KILL QUERY` 并断言服务端无残留）；Linux 前端 `Escape`/按钮取消，重复点击不发第二次 RPC；**契约缺口**：`ExecuteQueryRequest` 不携带客户端 `query_id`，尚未返回首批的语句无法服务端取消（界面 0 ms 进入 Cancelled，服务端可能仍在执行） |
| REQ-SQL-004 | 多语句、多结果集 | P1 | 部分 | 契约与 SPI 已支持 `statement_index`；SQLite 与 MariaDB 两个驱动都**拒绝**多语句（`capability.unsupported`，MariaDB 在发送前拒绝），无驱动实现多结果集（v0.2.2+ 再评估） |
| REQ-SQL-005 | 查询历史与收藏 | P1 | 未实现 | 本地保存，不含敏感数据 |
| REQ-SQL-006 | 自动补全（基于元数据） | P2 | 未实现 | 可关闭；不阻塞输入 |

## 4. 查询结果

| ID | 需求 | 优先级 | 状态 | 验收要点 |
|---|---|---|---|---|
| REQ-RESULT-001 | 分批拉取 + 虚拟化渲染 | P0 | 已实现 | 后端游标分批（批次上限 5000）与 Linux 前端每批 500 行 + `GtkColumnView` 虚拟化（PR #38）；10 万行不滚动 3 秒仍只加载 500 行（背压），全量 2808 ms / RSS 191.6 MB，见 `docs/releases/v0.2.md`；Windows/macOS 无 UI |
| REQ-RESULT-002 | 类型正确展示：NULL、Decimal、时间戳、二进制、JSON、数组 | P0 | 部分 | SQLite 按实际存储类映射（12 用例），MariaDB 全类型矩阵（PR #32，21 集成用例）；前端按 `ColumnValue.kind` 分别显示 NULL/空串/二进制/Decimal/日期时间/截断（PR #38）。未实现：数组（仅 PostgreSQL 有，驱动未实现）、原生 JSON 语义（MariaDB `JSON` 是 longtext 别名，按文本展示）；单元格的 `original_type_name` 后端尚未填充（`mapping.rs`，属后端 TODO） |
| REQ-RESULT-003 | 复制（单元格/行/列）与导出（CSV/JSON/SQL） | P1 | 未实现 | 流式写出，不一次性入内存；Linux 前端目前没有复制/导出入口 |
| REQ-RESULT-004 | 大字段截断标记与延迟加载 | P1 | 部分 | SQLite 与 MariaDB 都实现 `Cell.truncated`、64 KiB 上限与字符边界截断（各有测试）；延迟加载未实现，前端只显示前缀并在 tooltip 说明 |
| REQ-RESULT-005 | 结果集内存上限与背压 | P0 | 已实现 | 客户端行数上限（`SQARVO_RESULT_ROW_LIMIT`，默认 500000）与估算字节上限（`SQARVO_RESULT_BYTES_LIMIT`，默认 512 MiB），触顶停止拉取、保留已加载行并显示 `result.limit_reached`（PR #39）；预取有消费门与单批背压。**已知偏差**：估算不含 GTK 开销，200 列场景实际 604 MB 远高于估算，上限只能防无限增长 |

## 5. 数据编辑与事务

| ID | 需求 | 优先级 | 状态 | 验收要点 |
|---|---|---|---|---|
| REQ-EDIT-001 | 表数据浏览与主键定位 | P1 | 已实现 | 行定位只用完整主键值（复合主键按 `GetPrimaryKey` 顺序给出全部列），驱动内部 prepared statement 绑定参数，不拼接 SQL。无主键表/视图：`primary_key` 返回空、`browse` 置只读并标注「无稳定排序」、编辑拒绝 `table_data.no_primary_key`，**不提供** rowid / ctid 近似定位（ADR 0017）。SQLite `primary_key_handles_single_composite_and_missing_keys`、`table_without_primary_key_is_read_only_and_not_pageable`、`views_browse_as_read_only_without_primary_key`；MariaDB `metadata_primary_key_handles_single_composite_and_missing_keys`（#45/#46，PR #54/#55） |
| REQ-EDIT-002 | 安全编辑：脏标记、提交、回滚、只读态 | P1 | 已实现 | 脏行有行首标记与「脏行 N · 待提交 M 条」计数，放弃更改可撤销；提交只发「主键值 + 列变更」，`expected_rows_affected=1` 由服务端核对（不符 → `ChangeOutcome::Conflict`、目标行不存在 → `RowNotFound`、整批 `table_data.rows_affected_mismatch`），批内以 savepoint 保证原子性且失败后事务仍可用；风险确认阈值 `DEFAULT_RISK_CONFIRMATION_ROWS=100`，>100 行或含删除必须显式确认（服务端 `table_data.confirmation_required` 兜底）；只读态：无主键表 / 驱动未声明行编辑 / 只读事务三种原因都有文字说明并禁用编辑。SQLite `cargo test -p sqarvo-driver-sqlite --test row_edits` → 11 通过；MariaDB `row_edits_detect_conflicts_and_roll_back_failed_batches`、`row_edits_inside_explicit_transaction_use_savepoint`、`row_edits_preserve_mariadb_types_bit_for_bit`；gRPC `grpc_table_data_e2e`（#47，PR #56 / `cdd3142`）；Linux 数据页实测提交/回滚/只读（#48，PR #57 / `47da472`）；只读连接的误报已在 #60 修正（PR #63 / `c99a313`）；逐行 `rows_affected` 已与 proto 注释对齐（#68，`grpc_row_failures_e2e` 4 通过）。**v0.2.2 新增插入新行**（#85，PR #93 / `8c7316b`；前端 #88，PR #97 / `a59d78d`）：`InsertRows` 单行/批量共用同一信封，省略列 ≠ 显式 NULL，自增/默认值生成的主键回传真实值，唯一冲突与 NOT NULL 两类错误码分开，整批原子（失败 → `rolled_back=true`、净影响 0）；前端「新增行」可填值或显式 NULL，失败保留草稿可重试。SQLite `--test insert_rows` 9 通过、MariaDB 集成 8 个 `insert_rows_*`、`grpc_insert_rows_e2e` 2 通过。**剩余缺口**：契约没有 `ColumnMeta.auto_increment`，界面只能把自增列默认「省略」并说明原因，无法禁止用户显式填写；没有「按主键回读单行」RPC，插入成功后省略列只显示「数据库生成」，需刷新才见真实值；`InsertRows` 批次本版不可取消（无 `query_id`） |
| REQ-TXN-001 | 显式事务：开始/提交/回滚与状态展示 | P1 | 已实现 | `TransactionService.Begin` / `Commit` / `Rollback` 已接线（不再返回 `UNIMPLEMENTED`），两个驱动 `transactions=true`；单会话最多一个事务（重复 `Begin` → `transaction.already_active`），失败态只允许回滚，未知事务 ID → `transaction.not_found`（不退化成自动提交）；状态经 `TransactionInfo.state` 与 `SessionSummary.transaction` 暴露，Linux 数据页徽标四态（自动提交 / 事务中 / 失败态 / 已断开）；断连时后端强制回滚并清空登记。`cargo test -p sqarvo-rpc-server --test grpc_transactions_e2e` → 8 通过（含 `disconnect_forces_rollback_and_clears_registry`、`failed_transaction_rejects_commit_and_allows_rollback`、`session_status_reports_open_transaction`）（#44，PR #53 / `79ae6fd`；UI 由 #48，PR #57）。**剩余**：SQL 工作区查询仍是 `auto_commit=true`，手写 `BEGIN` 后数据页徽标不即时刷新（写入被 `transaction.already_active` 拒绝）；「失败态」徽标未做 e2e 触发；表数据写入路径已收进会话执行锁（校验事务 → 写批次 → 失败态观察同处一锁，#61 / PR #65 / `afd2827`） |
| REQ-TXN-002 | 隔离级别与只读事务 | P2 | 已实现 | 只读事务两端验证：SQLite `BEGIN DEFERRED` + `PRAGMA query_only` 由数据库拒绝写入（`read_only_transaction_rejects_writes_and_restores_writability`），MariaDB `START TRANSACTION READ ONLY`（服务端 1792 → `mariadb.read_only_transaction`）；隔离级别 MariaDB 四档逐档下发并有 `@@transaction_isolation` 断言，`Snapshot` 明确拒绝（`capability.unsupported`），SQLite 只接受 `Serializable`、其余拒绝。**v0.2.2 补齐界面入口**（#88，PR #97 / `a59d78d`）：「开始事务」先弹隔离级别卡片，MariaDB 五档全开，SQLite 只启用「默认 + 可序列化」，其余三项禁用并逐项给原因；所选级别经 `BeginTransactionRequest.isolation` 发出；#88 实测同一个外部未提交事务下 `read-uncommitted` 读到脏值、`repeatable-read` 读不到。前端 `cargo test` 192 通过（含 `isolation_options_sqlite_only_allows_serializable`）。**剩余**：`Snapshot` 无驱动支持；savepoint 只在行编辑/插入批次内部使用，未对外暴露；隔离级别是页内模态卡片而非独立窗口 |

## 6. 结构与高级能力

| ID | 需求 | 优先级 | 状态 | 验收要点 |
|---|---|---|---|---|
| REQ-DDL-001 | 表结构查看 | P1 | 已实现 | 由 `MetadataService.GetDdl` 提供（见 REQ-META-003）：表/视图/序列/索引/触发器的数据库原文或明确错误码；Linux 前端 DDL 面板标注来源并可复制（#86，PR #98 / `a6008c1`）。只读，不执行任何 DDL |
| REQ-DDL-002 | 结构修改（执行 DDL） | P2 | 未实现 | 预览 SQL 后执行 |
| REQ-XFER-001 | 数据导入 / 导出（含大文件流式） | P2 | 未实现 | 路径校验、覆盖保护 |
| REQ-DIFF-001 | 结构对比与迁移脚本 | P2 | 未实现 | 只生成脚本，不自动执行 |
| REQ-ER-001 | ER 图 | P2 | 未实现 | — |
| REQ-SYNC-001 | 数据同步 | P2 | 未实现 | 高危操作需二次确认 |

## 7. 平台、安全与质量

| ID | 需求 | 优先级 | 状态 | 验收要点 |
|---|---|---|---|---|
| REQ-PLAT-001 | 三平台原生前端 | P0 | 部分 | Linux（GTK4）已接入后端：握手与进程生命周期（PR #36）、连接对话框（PR #37）、SQL 执行/取消/虚拟化（PR #38）；Windows（WinUI 3）与 macOS（SwiftUI）只有窗口骨架，未接后端、未构建、未在 CI 覆盖——**未验证** |
| REQ-SEC-001 | 本地 IPC 访问控制（0700/0600 + token，拒绝非回环） | P0 | 已实现 | 有单元测试与真实 UDS 冒烟测试；Linux 前端按端点文件与 token 建立连接（PR #36） |
| REQ-SEC-002 | 日志与错误脱敏（token、密码、连接串） | P0 | 部分 | Token/Secret 类型已脱敏；gRPC 凭据用例断言密码不出现在引用、错误文案与日志中（`stored_secret_never_appears_in_reference_or_errors`）；Windows/macOS 前端尚无日志路径可验证 |
| REQ-SEC-003 | 高危操作提示与影响范围说明 | P1 | 未实现 | 删除/覆盖/批量更新 |
| REQ-PERF-001 | 启动与空闲内存基准 | P1 | 已实现 | `cd frontend/linux && ./scripts/bench.sh`：冷启动 3 次中位数 release 235 ms / debug 240 ms、空闲 5 秒 VmRSS release 141.6 MB / debug 149.3 MB（2026-11-06 release、2026-11-04 debug，同一台 Ryzen 7 8745H / 24.8 GiB）；脚本可重复执行并输出机器可读记录，两种构建均已实测（release 三轮复跑见 `docs/releases/v0.2.2.md`） |
| REQ-PERF-002 | 大结果集与宽表性能测试 | P1 | 已实现 | 10 万行 release 2095 ms / 184.6 MB（debug 2808 ms / 191.6 MB）、200 列 × 2000 行 release 2927 ms / 595.6 MB（debug 3554 ms / 603.4 MB），不滚动 3 秒仍 500 行（背压，两种构建一致），见 `docs/releases/v0.2.2.md`；**未达标项**：宽表内存随列数上升且 release 下几乎不变（分配点未定位）；估算字节不含 GTK 渲染开销 |
| REQ-A11Y-001 | 键盘可达与可访问性 | P1 | 未实现 | 常用操作有快捷键 |
| REQ-I18N-001 | 国际化 | P2 | 未实现 | 当前界面文案为中文 |

## 8. 非功能约束（适用于全部需求）

- 数据库密码不得出现在配置、日志、错误信息、崩溃报告与测试快照中；
- 不支持的能力必须返回明确错误（`UNIMPLEMENTED` / `capability.unsupported`），
  不得静默降级或返回空结果；
- 所有长任务必须可取消，取消必须幂等；
- 所有列表/结果集必须有分页或上限；
- 每个驱动必须有集成测试，使用专用测试库与合成数据。

## 变更方式

- 新增/删除需求必须同时更新本文件、`roadmap.md` 与相关里程碑；
- 需求进入开发前，需在 GitHub Issues 建立任务并关联里程碑与验收标准；
- 不得在仓库其他位置维护第二份功能清单。
