# 发布计划

- 本文件定义版本范围与退出标准；阶段与依赖见 [development-plan.md](development-plan.md)。
- 版本号遵循语义化版本：`0.x` 期间 API 与数据格式可能变化；0.2 线以 `0.2.x` 增量交付，
  尚未进入兼容承诺阶段（兼容策略见文末「兼容与弃用」）。
- **不承诺发布日期**；版本以能力与质量门槛为准。
- 版本与阶段的对应关系由 [milestones.md](milestones.md) 维护（M0–M7 ↔ S0–S7 ↔ v0.x）；
  本文件只定义每个版本的范围与退出标准。

## 版本概览

| 版本 | 主题 | 包含能力 | 不包含 |
|---|---|---|---|
| v0.1 | 基础架构与数据库连接（后端） | 本地 IPC + token、gRPC 契约、Driver SPI、SQLite 连接/执行/分批/取消 | 前端可用界面、网络数据库（MariaDB/PostgreSQL）、元数据浏览、事务 UI |
| v0.2 | SQL 编辑器与查询结果 | Linux 前端接入（握手、连接对话框、SQL 执行/取消/结果虚拟化）；凭据存储（Linux Secret Service）；MariaDB 驱动 | Windows/macOS 接入、数据编辑、对象管理、导入导出 |
| v0.2.1 | 数据浏览与编辑 | 表数据浏览、安全编辑、事务控制与状态展示 | 对象管理、结构对比、ER 图 |
| v0.2.2 | 数据库对象管理 | 对象树、列信息、DDL 查看、对象搜索 | 导入导出、结构同步 |
| v0.2.3 | 导入导出与结构管理 | CSV/JSON/SQL 导入导出、结构对比与迁移脚本 | ER 图、数据同步 |
| v0.2.4 | 首个完整可用版本 | 三平台一致、多驱动（SQLite/MariaDB/PostgreSQL）、性能与安全验收、打包 | 远程后端、团队协作 |

> 版本内容会随需求评审调整；调整必须同步 `roadmap.md`、`milestones.md` 与本文件。

## 各版本退出标准

### v0.1

- M0、M1、M2 完成；
- 后端可通过真实 gRPC 客户端连接 SQLite 并完成执行、分批、取消、关闭；
- 所有检查（fmt/clippy/test/doc）通过；
- 明确声明「无前端、无网络数据库支持」。

完成记录（2026-11-04，Linux）：以上四条全部满足。`cargo test` 95 通过，
`fmt --check` / `clippy -D warnings` / release 构建通过，CI 作业 41786163030 成功，
真实二进制运行的权限、日志脱敏与退出清理已实测。发布说明见
[v0.1](../releases/v0.1.md)。

### v0.2

- 至少一个平台前端可启动后端、完成 token 握手并执行 SQL；
- MariaDB 驱动通过 testkit；
- 凭据存储可用且缺失时明确报错；
- 结果集虚拟化与分批拉取在 10 万行场景可用（有基准记录）。

完成记录（2026-11-04，Linux）：以上四条全部满足，逐条证据如下。范围仅 Linux 前端；
Windows / macOS 前端仍是窗口骨架，未构建、未验证。

| 退出标准 | 证据 |
|---|---|
| 至少一个平台前端可启动后端、完成 token 握手并执行 SQL | Linux 前端（GTK4）：PR #36 / `d4d278d`（握手与进程生命周期，CI run `41809894166`）、PR #37 / `409dcbb`（连接对话框与凭据写入，run `41811993419`）、PR #38 / `835373f`（执行/取消/虚拟化，run `41815806079`）；`cd frontend/linux && cargo test` → 66 通过；真实 MariaDB 上的 SQL 执行由 `./scripts/bench.sh` 的 10 万行 / 200 列场景（同一个 UI 路径拉起后端、执行并拉取结果）以及 `frontend/linux/README.md` 记载的 `--qa-sql-file` + `--qa-execute` 复现 |
| MariaDB 驱动通过 testkit | `SQARVO_MARIADB_URL=… cargo test -p sqarvo-driver-mariadb --test integration` → 21 通过，含 `driver_passes_testkit_smoke` 与 `driver_passes_testkit_invariants`（五项不变量） |
| 凭据存储可用且缺失时明确报错 | `cargo test -p sqarvo-local-store`（33 单测 + 4 集成）、`cargo test -p sqarvo-rpc-server --test grpc_credential`（11 通过）；缺失 → `credential.not_found`，存储不可用 → `credential.unavailable`，无回退明文路径；Windows / macOS 凭据库未实现（装配为不可用） |
| 结果集虚拟化与分批拉取在 10 万行场景可用（有基准记录） | `cd frontend/linux && ./scripts/bench.sh`：10 万行首批 500 行 953 ms、全量 2808 ms、RSS 191.6 MB；不滚动 3 秒仍 500 行 / 0 批（背压）。完整数值与测量口径见 [v0.2 发布说明](../releases/v0.2.md) |

口径与未验证项：性能数值来自 **debug** 构建（Ryzen 7 8745H / 24.8 GiB），release 构建未测量；
真实 TLS 握手与 MySQL 8.x 兼容性未验证；Windows / macOS 无任何构建或运行验证。
逐条已知限制见 [v0.2 发布说明](../releases/v0.2.md)。

### v0.2.1

- 表数据浏览与编辑可提交/回滚，脏数据可见；
- 事务状态在 UI 中可见，断连时事务被清理；
- 编辑操作的风险提示与只读模式可验证。

完成记录（2026-11-05，Linux）：以上三条全部满足，逐条证据如下。范围仍是 Linux 前端 + 本地后端 +
SQLite / MariaDB 两个驱动；Windows / macOS 前端未构建、未验证。证据基线 main `47da472`
（其后只有发布文档提交 `3796915`，未改任何代码）。

| 退出标准 | 证据 |
|---|---|
| 表数据浏览与编辑可提交/回滚，脏数据可见 | **驱动层**：`cd backend && cargo test -p sqarvo-driver-sqlite --test row_edits` → 10 通过（含 `edits_round_trip_every_scalar_type_bit_for_bit`、`batch_inside_explicit_transaction_uses_savepoint_and_leaves_tx_usable`、`missing_row_and_mismatched_expectation_are_explicit`、`read_only_targets_and_invalid_keys_are_rejected_before_writing`）、`--test table_data` → 9 通过；`SQARVO_MARIADB_URL=… cargo test -p sqarvo-driver-mariadb --test integration` → 48 通过（含 `row_edits_detect_conflicts_and_roll_back_failed_batches`、`row_edits_inside_explicit_transaction_use_savepoint`、`row_edits_preserve_mariadb_types_bit_for_bit`）。**gRPC**：`cargo test -p sqarvo-rpc-server --test grpc_table_data_e2e` → 2 通过（`browse_table_pages_over_grpc_without_duplicates` 轮询翻页无重复行、`apply_row_changes_edits_rows_and_enforces_guards` 含类型化编辑、截断值拒绝、空批次拒绝等 35+ 断言）。**前端（真实后端 + MariaDB `sqarvo_test_48`）**：`sqarvo-desktop --qa-profile … --qa-data-table demo_items --qa-data-edit "2:name:beta-hermes" --qa-data-commit --qa-data-reload` → 日志「提交批次：1 更新 / 0 删除」「行变更已提交：影响 1 行，失败 0 行」「剩余脏行 0」，外部 `SELECT` 复核 id=3 的 name 由 gamma 变为 beta-hermes；回滚：`--qa-data-begin-txn --qa-data-edit "0:name:should-not-persist" --qa-data-rollback --qa-data-reload --qa-auto-confirm-risk` → 「事务已回滚」「事务结束后丢弃 1 行未提交的界面更改」，外部复核 id=1 仍为 alpha-committed。**脏数据可见**：截图 `data-page-dirty-txn.png`（行首红点脏标记、右上「脏行 1 · 待提交 1 条」、「提交更改 (1)」高亮） |
| 事务状态在 UI 中可见，断连时事务被清理 | **UI**：右上徽标四态（自动提交 / 事务中 / 失败态 / 已断开），`data-page-dirty-txn.png` 显示「事务中」高亮、`data-page-readonly.png` 显示「自动提交」。**gRPC**：`cargo test -p sqarvo-rpc-server --test grpc_transactions_e2e` → 8 通过，其中 `disconnect_forces_rollback_and_clears_registry` 断言 `active_count()==0`、`forced_rollback_count()==1`、`cleanup_failure_count()==0`，并用新会话读到 0 行证明驱动侧真回滚。**真实进程**：后端退出日志「会话清理已强制回滚打开的事务 transaction=txn_6585cc8f… reason="后端退出"」（前端 e2e 实际触发；前端日志「断连清理：丢弃 1 行未提交变更，事务由后端回滚」）。**未验证**：前端进程崩溃（gRPC 流断开）不触发自动回滚——会话与传输流解耦，只有显式 `Disconnect` 与后端退出走强制回滚 |
| 编辑操作的风险提示与只读模式可验证 | **风险提示**：>100 行或含删除必须显式确认（`DEFAULT_RISK_CONFIRMATION_ROWS = 100`），服务端以 `table_data.confirmation_required` 兜底；实测不加 `--qa-auto-confirm-risk` 时点「回滚事务」停在确认对话框、事务确实未回滚，加上后才回滚；gRPC e2e 中删除单行弹出确认对话框，确认后影响 1 行。**只读**：无主键表 `demo_notes` → 「已打开 demo_notes：1 行，只读=true（NoPrimaryKey），排序=Unstable，主键=[]」，尝试编辑 → 「编辑失败：只读：该表没有主键…」，三个编辑按钮在代码层被禁用（`frontend/linux/src/data_page.rs:2092-2095`），截图 `data-page-readonly.png`。**服务端拒绝**：`table_data.no_primary_key` / `table_data.read_only` / 会话只读事务写入被拒均有用例（SQLite `read_only_targets_and_invalid_keys_are_rejected_before_writing`、`read_only_transaction_rejects_row_edits`；MariaDB `row_edits_reject_read_only_targets_and_invalid_keys`） |

交付顺序（全部 squash 合并进 main，Issue 已关闭；每个 PR 的两个作业 `后端（Linux）` 与
`MariaDB 集成测试（服务容器）` 均 pass）：

| Issue | 交付物 | PR | squash 提交 | CI run |
|---|---|---|---|---|
| #41 | ADR 0017 + proto + 参数化 SPI 类型 | #50 | `76ce23d` | `41954735456` |
| #42 | SQLite 显式事务 | #51 | `fa2e747` | `41955959769` |
| #43 | MariaDB 显式事务 | #52 | `b24eef9` | `41957364237` |
| #44 | 事务生命周期接线与 `TransactionService` | #53 | `79ae6fd` | `41958744849` |
| #45 | 两驱动元数据最小子集 | #54 | `100f43d` | `41960916141` |
| #46 | 表数据浏览（主键序游标） | #55 | `8775dda` | `41963333169` |
| #47 | 行编辑提交（参数化 DML） | #56 | `cdd3142` | `41965284410` |
| #48 | Linux 数据页 | #57 | `47da472` | `41968317577` |

同日收尾批次（#41–#48 合并后复核发现的工程债，同样逐条关 Issue）：

| Issue | 交付物 | PR | squash 提交 | CI run |
|---|---|---|---|---|
| #59 | `ColumnMeta` 补默认值/字符集/排序规则 + 驱动能力位 `row_editing`/`parameterized_statements` | #64 | `c78d6b6` | `41975329411` |
| #60 | `ReadOnlyReason` 增加「连接只读」，SQLite 只读连接不再误报可编辑 | #63 | `c99a313` | `41975913097` |
| #61 | 表数据写入路径持会话执行锁（校验事务 → 写批次 → 失败态观察同处一锁） | #65 | `afd2827` | `41976832129` |
| #62 | 测试债：逐行失败面板 / 截断列 e2e、失败态徽标、数据页三态截图 | #66 | `b4cc9a1` | `41978029512` |

收尾批次后的基线（main `b4cc9a1`）：`cargo test --workspace` → **408 passed / 0 failed**
（#41–#48 时为 382）；`cd frontend/linux && cargo test` → **98 passed**（原 94）；
fmt 与 clippy 仍干净；`scripts/check-tracking.sh --state all` 仍「失败 0 项，警告 0 项」。

配套基线（证据基线 main `47da472`，Linux；其后的 `3796915` 只改文档）：`cd backend && cargo fmt --all -- --check` 无输出 / exit 0；
`cargo clippy --all-targets -- -D warnings` 无警告 / exit 0；
`SQARVO_MARIADB_URL=… cargo test --workspace` → **382 passed / 0 failed / 0 ignored**（v0.2 基线 219）；
`cd frontend/linux && cargo fmt --all -- --check` exit 0、`cargo test` → **94 passed / 0 failed**
（v0.2 基线 66）；`bash scripts/check-tracking.sh --state all` → 「39 个 Issue：失败 0 项，警告 0 项」。

口径与未验证项：性能数字来自 **debug** 构建（Ryzen 7 8745H / 24.8 GiB），release 构建未测量；
10 万行翻页耗时（SQLite 全表 200 页 100.8 ms、MariaDB 499.0 ms）只是同机观察值，**不是发布承诺**。
真实 TLS 握手、MySQL 8.x、MariaDB < 10.6 未验证；Windows / macOS 前端未构建、未运行。
前端进程崩溃（gRPC 流断开）不触发自动回滚；逐行失败面板（`ROW_NOT_FOUND` / `CONFLICT`）只有单元测试，
事务「失败态」徽标未用 e2e 触发，截断列禁编只有代码判定 + 单元测试，数据页 Loading / Empty / Error
三态未逐态截图。本版**不包含**插入新行、对象树全量、DDL 查看、导入导出与非主键排序。
逐条已知限制与问题清单见 [v0.2.1 发布说明](../releases/v0.2.1.md)。

### v0.2.2

范围（对应 Issue #81–#90）：对象树上层与跨库、DDL 查看、对象搜索、插入新行，以及 Linux 前端的对象树/DDL 面板/列面板/插入行/隔离级别入口。

- 对象树可逐层展开且分页稳定：两驱动下库（SQLite 为 `main`/`temp`）、表、视图、索引可取；
  翻页无重复无遗漏；**不支持的层级必须显式报错，不得返回空列表冒充**。
- DDL 可查看且来源可信：表/视图/索引的 DDL 可取，`exact` 标志与真实来源一致
  （数据库原文为 true，客户端拼装为 false）；权限不足、对象不存在、不支持的对象种类各有明确错误码；
  `GetDdl` 必须与并发写入串行（纳入会话执行锁）。
- 对象搜索可用：按名称片段搜索，大小写差异在文档中写清，`%`/`_` 正确转义；
  结果分页与非过滤列表共用同一套 token 语义，换过滤条件后旧 token 被拒。
- 插入行可用且安全：自增主键、显式主键、全默认值三条路径正确；唯一冲突与 NOT NULL 无默认值有明确错误码；
  批量插入的原子性与现有行编辑一致；**值一律参数化绑定，有 grep 证据证明无字符串拼接**。
- Linux 前端闭环：能用界面完成「展开对象树 → 查看 DDL → 搜索对象 → 插入一行」，
  并有 `--qa-*` 无人值守证据与 GTK 自渲染截图。

完成记录（2026-11-06，Linux）：以上五条全部满足，逐条证据如下。范围仍是 Linux 前端 + 本地后端 +
SQLite / MariaDB 两个驱动；Windows / macOS 前端未构建、未验证。证据基线 main `9f3d2ce`（#89 的合并提交；
其后没有代码提交）。

| 退出标准 | 证据 |
|---|---|
| 对象树可逐层展开且分页稳定 | **SQLite**：`cargo test -p sqarvo-driver-sqlite --test metadata_hierarchy` → 8 通过（含 `nodes_from_another_database_round_trip_to_columns_and_primary_key`、`catalog_names_with_quotes_spaces_and_cjk_are_quoted`、`mixed_kinds_with_duplicate_names_page_without_gaps`）；`--test metadata` → 13 通过。**MariaDB**：`SQARVO_MARIADB_URL=… cargo test -p sqarvo-driver-mariadb --test integration` → 69 通过，含 `metadata_catalog_layer_lists_every_visible_database`、`metadata_catalog_children_are_scoped_to_that_database`、`metadata_table_children_include_columns_indexes_and_triggers`、`metadata_hierarchy_pagination_has_no_duplicates_or_gaps`。**前端（真机 UI）**：SQLite `--qa-tree-expand main` → 日志「level=2 main 返回 4 条（耗时 2 ms，累计 4 条，has_more=false）」，打开 `orders` 后「已打开 orders：2 行…主键=["id"]」，断言 `health=Ready`、`selected=orders`；MariaDB `--qa-tree-expand sqarvo_test_90` 同一路径 exit 0（截图 `90-tree-ddl.png`、`90-tree-ddl-mariadb.png`）。**口径**：schema 层两个数据库都不存在，显式 `capability.unsupported`；MariaDB 受限账号无法区分「库不存在 / 无权限」 |
| DDL 可查看且来源可信 | **SQLite**：`--test metadata` 的 5 个 `ddl_*` 用例通过（`ddl_returns_sqlite_master_text_verbatim`、`ddl_is_not_truncated_at_cell_limits`、`ddl_reads_attached_database_by_catalog` 等）；`sqlite_autoindex_*` 无原文 → `capability.unsupported`。**MariaDB**：集成 69 通过中含 `metadata_ddl_returns_show_create_table_text`、`metadata_ddl_picks_the_ddl_column_for_views`、`metadata_ddl_supports_sequences`、`metadata_ddl_reports_unsupported_kinds_and_missing_objects`。**执行锁**：`cargo test -p sqarvo-rpc-server --test grpc_session_locks_e2e` → 8 通过，含 `ddl_holds_lock_against_apply`、`ddl_waits_for_apply`。**gRPC**：`--test grpc_metadata_e2e` → 1 通过（断言 `exact=true`、`source=DDL_SOURCE_DATABASE`、正文以 `CREATE TABLE` 开头）。**前端**：两驱动均以 `--qa-tree-ddl` 打开面板并断言 `provenance=原文 / state=ready / copy=on`；日志「exact=true，source_detail=sqlite_master.sql（数据库 main，对象种类 table）」与「source_detail=SHOW CREATE TABLE（sqarvo_test_90.sqarvo_t_smoke）」 |
| 对象搜索可用 | **SQLite**：`cargo test -p sqarvo-driver-sqlite --test metadata_filter` → 13 通过 / 1 ignored（`underscore_is_a_literal_not_a_wildcard`、`filter_applies_before_the_page_is_cut`、`changing_the_filter_rejects_the_old_token` 等）。**MariaDB**：`--test metadata_filter` → 9 通过 / 1 ignored（含 `permission_denied_is_not_reported_as_an_empty_search`、`changing_the_filter_rejects_the_old_token`）。**前端**：`--qa-tree-search ord` → 日志「搜索「ord」返回 3 个结果」「跳转成功：main.order_items」，断言 `search=results`、`first=order_items`、`selected=order_items`、`page=tree`（截图 `90-search-jump.png`）。**性能**：SQLite 10 万对象 9.3 ms、MariaDB 1 万对象 7.2 ms（均来自 #84 的 release `#[ignore]` 基准，本轮未重跑夹具；MariaDB 10 万未实测） |
| 插入行可用且安全 | **SQLite**：`cargo test -p sqarvo-driver-sqlite --test insert_rows` → 9 通过（自增/显式/复合主键、省略列 vs 显式 NULL、唯一冲突整批回滚、savepoint、类型位级往返、500 行钳制）。**MariaDB**：集成 69 通过中含 8 个 `insert_rows_*`（`insert_rows_return_autoincrement_and_apply_defaults`、`insert_rows_unique_conflict_and_length_error_are_per_row_and_atomic`、`insert_rows_savepoint_keeps_the_outer_transaction_usable` 等）。**gRPC**：`cargo test -p sqarvo-rpc-server --test grpc_insert_rows_e2e` → 2 通过。**参数化**：`grep -rn 'push_str("INSERT\|format!("INSERT' backend/crates/drivers backend/crates/connection` 的生产路径只有 `INSERT INTO … VALUES (?1..?n)` / `DEFAULT VALUES` / `INSERT INTO t () VALUES ()` 模板，其余命中全在 `tests/` 夹具；没有把值拼进 SQL 的路径。**前端（SQLite 真机）**：`--qa-data-insert --qa-data-insert-set code:qa-new-1 --qa-data-insert-null amount --qa-data-commit --qa-data-reload --qa-expect-row code=qa-new-1` → 「整批回滚=false」「新行主键：id=1」，外部 `sqlite3` 复核 1 行、`created_at` 由默认值生成、`amount` 为 NULL；重复写同一 `code` → 「整批回滚=true」「`table_data.unique_violation`」「库里 0 行变化」，外部计数仍为 1（截图 `90-insert-row.png`、`90-insert-rollback.png`） |
| Linux 前端闭环 | 本轮在 SQLite（`/tmp/qa90.db`）上按「展开 `main` → 打开 `orders` → 查看 DDL → 搜索 `ord` → 跳转 `order_items` → 插入一行并回读」跑通四条 `--qa-*` 序列，退出码均为 0，断言读取控件文本；MariaDB 另跑「展开 `sqarvo_test_90` → 打开 `sqarvo_t_smoke` → 查看 DDL」，同样 exit 0。隔离级别卡片（`--qa-isolation-dialog`）在 SQLite 上输出「可用 2 项（默认、可序列化）；禁用 3 项（附原因）」。截图（GTK 自渲染，仓库外）：`~/sqarvo-v022-shots/90-tree-ddl.png`、`90-tree-ddl-mariadb.png`、`90-search-jump.png`、`90-insert-row.png`、`90-insert-rollback.png`、`90-isolation-sqlite.png`。**限制**：Wayland 下无 OS 级截屏与真实指针注入，点击走控件几何 + 同一处理函数；搜索与插入的 UI 闭环只在 SQLite 上跑，MariaDB 的 UI 证据只覆盖对象树/DDL |

交付顺序（全部 squash 合并进 main，Issue 已关闭）：

| Issue | 交付物 | PR | squash 提交 | CI run |
|---|---|---|---|---|
| #81 | 契约增量（proto + driver-api + 错误码） | #91 | `b59bad7` | `42044516828` |
| #87 | 前端列面板（默认值/字符集/排序规则） | #92 | `bcb12c4` | `42047837242` |
| #82 | 上层层级与跨库浏览 | #94 | `43373e7` | `42049499508` |
| #85 | 插入新行（两驱动 + gRPC） | #93 | `8c7316b` | `42050177579` |
| #83 | DDL 查看（两驱动 + 执行锁） | #95 | `0a8e21f` | `42052752392` |
| #84 | 对象搜索与过滤下推 | #96 | `8e7c661` | `42059959553` |
| #86 | 前端对象树、搜索与 DDL 面板 | #98 | `a6008c1` | `42063985353` |
| #88 | 前端插入行与隔离级别入口 | #97 | `a59d78d` | `42065190556`（MariaDB 作业失败，见下） |
| #89 | 前端在途批次登记与取消提交 | #99 | `9f3d2ce` | `42067912917` |

> #88 的合并提交 run `42065190556`：`后端（Linux）` 与 `前端（Linux）` 作业 success，
> `MariaDB 集成测试（服务容器）` 作业 1 例失败——`metadata_lists_tables_and_views_with_stable_pagination`
> 翻页时得到 `config.invalid`「分页 token 超出数据范围」（68 passed / 1 failed）；该提交未重跑。
> 随后合入的 #89 合并提交 run `42067912917` 三个作业全绿；本轮本地在同一测试库复跑 `--test integration`
> 69 通过，失败未复现。最可能的原因是集成用例并行执行时共用同一个库、对象集合在翻页途中被其它用例改变；
> 未做隔离复现，因此这里只记录事实，不宣称「已修复」。

配套基线（2026-11-06，Linux，证据基线 main `9f3d2ce`）：`cd backend && cargo fmt --all -- --check` → exit 0；
`cargo clippy --all-targets -- -D warnings` → 0 warning；
`SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test_90' cargo test --workspace`
→ **539 passed / 0 failed / 2 ignored**（54 个测试块；基线 v0.2.1 为 382，v0.2.2 前的 #84 已是 539）；
MariaDB 集成 **69 passed**，日志中「跳过：未设置 SQARVO_MARIADB_URL」出现 **0** 次；
`cd frontend/linux && cargo fmt --all -- --check` exit 0、`cargo clippy --all-targets -- -D warnings` 0 warning、
`cargo test` → **192 passed / 0 failed**（v0.2.1 基线 94）；
`bash scripts/check-tracking.sh --state all` → 「60 个 Issue：失败 0 项，警告 0 项」。

口径与未验证项：

- **TLS / 远程网络部署未验证**：测试容器未启用 TLS，`require` / `verify-ca` / `verify-full` 与客户端证书都没有真实握手记录；远程后端不在本版范围。
- **Windows / macOS 前端仍是窗口骨架**：未接后端、未构建、未运行验证、CI 无对应作业。
- **release 构建性能未系统测量**：除 #84 的搜索基准用 `cargo test --release` 外，启动、内存与大结果集数值仍来自 debug 构建。
- **MariaDB 10 万对象搜索未实测**：只跑 1 万对象（7.18 ms，release）；SQLite 10 万表为 9.28 ms。数据库侧建表夹具成本高（MariaDB 约 41 ms/张），不作为发布承诺。
- **无权限（`metadata.permission_denied`）的前端真实场景未跑**：测试账号权限过全，只有前端单测与驱动集成用例（MariaDB 侧用测试账号读 `mysql` 库）。
- **自增列在 UI 只能「默认留空 + 说明」**：契约缺 `ColumnMeta.auto_increment`，无法禁止用户显式填写自增值（数据库仍会接受）。
- **插入成功后省略列不回读真实值**：协议没有「按主键回读单行」RPC，界面显示「数据库生成」，需手动刷新。
- **前端截图是 GTK 自渲染**：Wayland 下不是 OS 级截屏，也没有真实指针注入；点击取证走控件几何与同一处理函数。
- **取消提交（#89）本轮未重跑**：其取消语义的证据来自 #89 的日志与截图（`CancelRowChanges` 往返、`executed_changes=128 < 500`、外部 SQL 0 行变化），不计入本轮独立复跑。
- **搜索范围**：Linux 只搜当前库根层、只展示首页 500 条；`contains` 与 `exact` 的大小写语义不对称（跟随数据库）。
- **仍不支持**：跨库表数据浏览/行编辑（元数据可跨库）、MariaDB 索引/触发器 DDL、`InsertRows` 批次取消、`Snapshot` 隔离级别、schema 层。
- 逐条已知限制见 [v0.2.2 发布说明](../releases/v0.2.2.md)。

### v0.2.3

- 导入导出流式处理，路径校验与覆盖保护；
- 结构对比只生成脚本、不自动执行；
- 大文件场景有内存上限与错误处理。

### v0.2.4

- 三平台构建、安装与基本查询流程可用（每平台有验证记录）；
- 三驱动能力矩阵完成，未支持项明确标注；
- 冷启动、空闲内存、大结果集基准数据入库；
- 安全清单（凭据、日志、IPC、错误信息、路径）全部通过；
- 无 P0/P1 未解决缺陷；已知限制写入发布说明。

## 发布检查清单

每次发布前逐项确认，结果写入 PR 或发布说明：

1. `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 通过；
2. 目标平台完成真实构建与启动验证（无法验证的平台明确标注）；
3. `docs/product/requirements.md` 的状态列与实现一致；
4. 支持矩阵（数据库 × 能力）更新，未支持项写明错误语义；
5. 安全复核：日志无 token/密码、端点文件 0600、IPC 仅本机、错误信息无敏感内容；
6. 升级/兼容：proto 无破坏性变更，或有主版本升级说明；
7. 打包产物与安装步骤在干净环境验证；
8. 发布说明包含：新增能力、行为变化、已知限制、升级注意事项。

## 支持矩阵（v0.2.2）

只填写「已实现并有测试」的能力；未实现的能力必须写明错误语义，不允许空白或「部分支持」。
本表的 SQLite / MariaDB 列按 v0.2.2（main `9f3d2ce`）的代码与测试校正；驱动细节见
`docs/database/sqlite.md`、`docs/database/mariadb.md` 与 `docs/database/driver-spi.md`。

| 能力 | SQLite（v0.1 起；事务 / 元数据 / 表数据自 v0.2.1，对象树 / DDL / 搜索 / 插入自 v0.2.2） | MariaDB（v0.2 新增；事务 / 元数据 / 表数据自 v0.2.1，对象树 / DDL / 搜索 / 插入自 v0.2.2） | PostgreSQL |
|---|---|---|---|
| 连接 / 测试连接 | 已实现并有测试：本地文件、`read_only` 模式、`SELECT sqlite_version()` 作为版本探测 | 已实现并有测试：真实 MariaDB 11.4 连接与测试连接（延迟 + 服务端版本）；5 档 TLS 模式映射，`preferred` 在服务端无 TLS 时回退并给出 `mariadb.tls_fallback` 告警；真实 TLS 握手未验证 | 未实现：未注册驱动，`Connect` / `TestConnection` 返回 `driver.not_found` |
| 执行 / 分批结果 | 已实现并有测试：单语句、prepared statement 游标分批、服务端批次上限 5000 行 | 已实现并有测试：`execute` 交付摘要 + 首批行，`max_rows` 钳制 `1..=5000`；游标 = 已交付行数、多读一行判 `has_more`、不用 `OFFSET` 重查；游标不匹配 → `mariadb.cursor_invalid`，游标未读完再 `execute` → `mariadb.query_active` | 未实现：`driver.not_found` |
| 类型映射 | 已实现并有测试：NULL/INTEGER/REAL/TEXT/BLOB 按实际存储类映射；NUMERIC 与 DATE/DATETIME 保留原始类型名、不伪造 Decimal 与时间戳；非法 UTF-8 按字节返回 | 已实现并有测试：有/无符号整数、`DECIMAL` 精度与标度（不经 `f64`）、`DATE`/`DATETIME`/`TIMESTAMP`（会话时区语义、0–6 位微秒）、`TIME`、`BIT`、`ENUM`/`SET`、`YEAR`、TEXT/BLOB 系列、`JSON`→`Text`（longtext 别名，不声明原生 JSON）；零日期与超范围 `TIME` 为值级 `Unsupported`，保留原始文本 | 未实现：`driver.not_found` |
| 大字段截断 | 已实现并有测试：单格 64 KiB、文本按字符边界截断并置 `truncated`；延迟加载未实现 | 已实现并有测试：64 KiB、UTF-8 字符边界回退、恰好等于上限不标记；延迟加载未实现 | 未实现：`driver.not_found` |
| 取消 | 已实现并有测试：`InterruptHandle` 中断、幂等、2 秒内生效 | 已实现并有测试：独立控制连接 `KILL QUERY`、幂等、2 秒内生效；批次之间的取消在下一次 `fetch` 立即返回 `query.cancelled`；`PROCESSLIST` 断言无残留执行 | 未实现：`driver.not_found` |
| 多语句 | 未实现：单语句，第二条语句返回 `capability.unsupported`（ADR 0006） | 未实现：能力声明 `multiple_statements=false`，发送前拒绝，返回 `capability.unsupported`，第二条语句不产生副作用 | 未实现：`driver.not_found` |
| 事务 | 已实现并有测试：`BEGIN IMMEDIATE`（读写）/ `BEGIN DEFERRED` + `PRAGMA query_only`（只读）；表数据写入路径持会话执行锁（#61）；一个会话最多一个事务（重复 `Begin` → `transaction.already_active`），失败态只允许回滚，重复提交/回滚与未知事务 ID 按 ADR 0017 处置；`--test transactions` 9 通过 | 已实现并有测试：`START TRANSACTION [READ ONLY]` / `COMMIT` / `ROLLBACK`；四档隔离级别逐档下发并有 `@@transaction_isolation` 断言，`Snapshot` 明确拒绝；事务中 DDL / `TRUNCATE` 等隐式提交语句**发送前**拒绝（`mariadb.implicit_commit_rejected`）；死锁 → 失败态，断连 → `mariadb.transaction_lost`；集成用例覆盖提交可见、回滚丢弃、断连回滚 | 未实现：`driver.not_found` |
| 元数据（表/视图列表、列、主键、默认值） | 已实现并有测试：`sqlite_master` 的表/视图、`pragma_table_info` 的列与主键（`pk` 即主键内顺序）、默认值四态（无默认 / 默认 NULL / 未求值表达式，SQLite 无列级字符集故留空）；分页 token 带对象名集合指纹，非法 token → `config.invalid`，集合变化 → `metadata.snapshot_changed`；`--test metadata` 13 通过 | 已实现并有测试：`information_schema` 的 `TABLES` / `COLUMNS` / `KEY_COLUMN_USAGE`；原始类型取 `COLUMN_TYPE`（含长度与 `unsigned`）、精度/标度/可空性、默认值、字符集与排序规则；`metadata_*` 集成用例覆盖分页、跨 catalog 拒绝、缺库与缺表分类 | 未实现：`driver.not_found`（请以「对象树」行的层级说明为准） |
| 表数据浏览 / 行编辑 | 已实现并有测试：主键升序 + 不透明游标（禁止 `OFFSET`），`--test table_data` 10 通过；行编辑走参数化 DML + 影响行数核对 + 批内 savepoint，`--test row_edits` 11 通过；无主键表只读（`table_data.no_primary_key`） | 已实现并有测试：同一游标语义与只读态；`row_edits_preserve_mariadb_types_bit_for_bit`（逐类型位级往返）、`row_edits_detect_conflicts_and_roll_back_failed_batches`、`row_edits_inside_explicit_transaction_use_savepoint`、`row_edits_reject_read_only_targets_and_invalid_keys` | 未实现：`driver.not_found` |
| 对象树（逐层展开 / 跨库） | 已实现并有测试：根 = `main` 的表/视图；目录层 `main` / `temp` / `ATTACH`；表 → 列/索引/触发器；视图 → 列；schema / sequence 层显式 `capability.unsupported`；跨库必须显式 catalog；每层按（种类, 名字）分页，`--test metadata_hierarchy` 8 通过、`--test metadata` 13 通过 | 已实现并有测试：database 即 catalog（`SCHEMATA`）；表 → 列/索引（含 `PRIMARY`）/触发器；序列作为叶子（`TABLE_TYPE='SEQUENCE'`）；schema 层显式 `capability.unsupported`；集成 69 通过（含 `metadata_catalog_*` / `metadata_hierarchy_*` 等层级与跨库用例） | 未实现：`driver.not_found` |
| DDL 查看 | 已实现并有测试：表/视图/索引/触发器读 `sqlite_master.sql` 原文（`exact=true`、`source=DDL_SOURCE_DATABASE`）；`sqlite_autoindex_*` 无原文 → `capability.unsupported`；`--test metadata` 含 5 个 `ddl_*` 用例，不截断 | 已实现并有测试：表/视图/序列 `SHOW CREATE` 原文（第 2 列，校验 `CREATE` 前缀）；索引/触发器 → `metadata.object_kind_unsupported`；4 个 `metadata_ddl_*` 集成用例，不截断 | 未实现：`driver.not_found` |
| 对象搜索 | 已实现并有测试：`LIKE … ESCAPE '\'` 下推、`%` / `_` / `\` 字面量；`--test metadata_filter` 13 通过 / 1 ignored；10 万对象 9.3 ms（release，`#[ignore]`，非承诺值） | 已实现并有测试：`LIKE CONCAT('%', ?, '%')` 下推、`Rows_sent` 增量断言；`--test metadata_filter` 9 通过 / 1 ignored；1 万对象 7.2 ms；10 万未实测 | 未实现：`driver.not_found` |
| 插入行 | 已实现并有测试：省略列 / 显式 NULL 区分、自增与复合主键回传、整批原子、`WITHOUT ROWID` 或省略非自增主键 → `capability.unsupported`；`--test insert_rows` 9 通过、`grpc_insert_rows_e2e` 2 通过；批次不可取消 | 已实现并有测试：`LAST_INSERT_ID` 回传、savepoint 原子性、唯一冲突 / NOT NULL 逐行码；8 个 `insert_rows_*` 集成用例；批次不可取消 | 未实现：`driver.not_found` |

> 驱动能力位（#59）：`ListDrivers` 现在会列出 `row_editing` 与 `parameterized_statements`，两个驱动均为 true 且有断言；未实现能力的驱动不会出现这两项。

> TLS 说明：MariaDB 的 TLS 只有「模式映射与参数校验」有单元测试，测试容器未启用 TLS，
> `require` / `verify-ca` / `verify-full` 与客户端证书**未验证**；不得据此声称「TLS 可用」。
> MySQL 8.x 兼容性同样未验证（`docs/database/mariadb.md` 未支持清单）。

### 验证方式（v0.2.2）

矩阵中的每一格都能用下列命令复核（除标注未验证的 TLS 外）：

```bash
cd backend
# SQLite：类型映射、截断、取消、分批、testkit 五项不变量
cargo test -p sqarvo-driver-sqlite
cargo test -p sqarvo-driver-sqlite --test type_mapping
cargo test -p sqarvo-rpc-server --test grpc_sqlite_e2e

# v0.2.1 / v0.2.2：事务、元数据、对象树、DDL、搜索、插入、表数据与行编辑（SQLite，无需外部数据库）
cargo test -p sqarvo-driver-sqlite --test transactions   # 9 通过
cargo test -p sqarvo-driver-sqlite --test metadata       # 13 通过（含 5 个 ddl_*）
cargo test -p sqarvo-driver-sqlite --test metadata_hierarchy  # 8 通过
cargo test -p sqarvo-driver-sqlite --test metadata_filter     # 13 通过 / 1 ignored
cargo test -p sqarvo-driver-sqlite --test insert_rows         # 9 通过
cargo test -p sqarvo-driver-sqlite --test table_data     # 10 通过
cargo test -p sqarvo-driver-sqlite --test row_edits      # 11 通过
cargo test -p sqarvo-rpc-server --test grpc_transactions_e2e  # 8 通过
cargo test -p sqarvo-rpc-server --test grpc_table_data_e2e    # 2 通过
cargo test -p sqarvo-rpc-server --test grpc_metadata_e2e      # 1 通过（含 GetDdl exact/source 断言）
cargo test -p sqarvo-rpc-server --test grpc_session_locks_e2e # 8 通过（含 DDL 执行锁两例）
cargo test -p sqarvo-rpc-server --test grpc_insert_rows_e2e   # 2 通过

# MariaDB：连接/测试连接/分批/取消/错误分类/testkit/事务/元数据/行编辑（需要真实实例）
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-driver-mariadb --test integration   # 69 通过
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-driver-mariadb --test metadata_filter  # 9 通过 / 1 ignored

# 凭据：Store/Delete、缺失与不可用语义（内存存储 + 真实 MariaDB）
cargo test -p sqarvo-rpc-server --test grpc_credential
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-rpc-server --test grpc_mariadb_credentials
```

前端（UI 逻辑单测；对象树/DDL/搜索/插入与数据页的真机复现见
`frontend/linux/README.md` 的「对象树、搜索与 DDL 面板」「插入新行与隔离级别的取证」两节）：

```bash
cd frontend/linux && cargo test    # 192 通过
```

CI 对应作业（`.github/workflows/ci.yml`）：`后端（Linux）`（fmt / clippy / `cargo test` /
SQLite 类型映射 / SQLite UDS 端到端）、`MariaDB 集成测试（服务容器）`（断言发现用例数 ≠ 0、
通过数 = 发现数、未进入跳过分支）与 `前端（Linux）`（fmt / clippy / `cargo test`）。
v0.2.2 的 9 个合并提交中 8 个三作业全绿；#88 的合并提交 run `42065190556` 的 MariaDB 作业
1 例失败（见上「交付顺序」脚注）。最后一个 PR #99 → run `42067912917`（squash 提交 `9f3d2ce`）。

## 兼容与弃用

- `0.x` 期间允许破坏性变更，但必须在发布说明中列出迁移方式；
- gRPC 字段编号一旦发布不得复用；删除字段使用 `reserved`；
- 端点文件格式变更必须提升 `protocol_version`，前端需能识别不兼容并提示升级；
- 配置格式变更需要迁移逻辑或明确的手动步骤。
