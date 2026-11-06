# 里程碑

里程碑是稳定的阶段锚点，不因单个任务延期而修改。完成标准变化必须有明确理由并记录。
具体任务与进度在 GitHub Issues / Projects 中跟踪。

| 里程碑 | 对应阶段 | 目标 | 对应版本 | 状态 |
|---|---|---|---|---|
| M0 骨架 | S0 | 可编译、可测试的仓库骨架与本地 IPC | v0.1 | 已完成 |
| M1 契约冻结 | S1 | Driver SPI v0.2、proto 增量、驱动测试基架 | v0.1 | 已完成 |
| M2 首个驱动切片 | S2 | SQLite 连接 → 执行 → 分批 → 取消 → 关闭 | v0.1 | 已完成（#9–#17） |
| M3 凭据与 MariaDB | S3 | 系统凭据存储 + MariaDB 驱动 | v0.2 | 已完成（#20–#25；TLS 握手 2026-11-06 逐档验证，见「M3 凭据与 MariaDB（实际完成）」备注） |
| M4 元数据与事务 | S4 | 对象树、列信息、DDL、显式事务（跨驱动） | v0.2.1 / v0.2.2 | 已完成（v0.2.1：事务 / 元数据最小子集 / 表数据；v0.2.2：逐层对象树 / 跨库 / DDL / 搜索，2026-11-06） |
| M5 第二网络驱动与一致性 | S5 | PostgreSQL 驱动 + 三驱动一致性矩阵 | v0.2.4 | 未开始 |
| M6 前端接入与硬化 | S6 | 三端接入、性能、安全、跨平台构建 | v0.2 起持续 | 进行中（Linux 首端接入完成；Windows / macOS 未验证） |
| M7 导入导出与结构管理 | S7 | 流式导入导出、路径与覆盖保护、结构对比脚本 | v0.2.3 | 未开始 |

## 完成标准

### M0 骨架（已完成）

- workspace 编译通过，`fmt` / `clippy -D warnings` / `test` 全绿；
- 真实 UDS + token 的 gRPC 端到端测试通过；
- 二进制运行后 socket 0600、端点文件 0600、SIGTERM 清理、日志无 token；
- 文档：架构、IPC、gRPC 约定、Driver SPI、ADR、路线图齐全。

实际完成：2026-11-04（Linux），验证记录见 `development-plan.md` 的 S0。

### M1 契约冻结（完成标准）

- SPI v0.2 覆盖多语句、影响行数、截断、事务、元数据；
- proto 仅做增量修改并重新生成；
- testkit 可运行；
- ADR 合并（0005–0009：游标、多语句、截断、并发、SQLite 选型）。

### M1 契约冻结（实际完成）

实际完成：2026-11-04（Linux）

- SPI v0.2：`ExecutionStart` / `StatementSummary`、按 `statement_index` 的游标、
  事务接口、`MetadataProvider`、`Cell.truncated` 与 64 KiB 上限；
- proto 增量：`FetchBatchRequest.statement_index = 4`、`ExecuteQueryRequest.timeout_millis = 5`；
- `SessionManager::acquire` + 会话级执行锁 + 断连清理查询登记；
- 新增 `sqarvo-driver-testkit`（脚本化会话 + `smoke` 符合性检查）；
- ADR 0005–0009：游标所有权、多语句策略、截断、并发、SQLite 选型；
- 验证：`cargo fmt --check`、`clippy -D warnings`、`cargo test`（66 通过）通过，
  UDS gRPC 冒烟测试不回归。

### M2 首个驱动切片

- 真实 gRPC 客户端完成 SQLite 查询与分批拉取；
- 取消 2 秒内生效且幂等；
- 断开连接后会话/查询计数归零；
- CI 运行 SQLite 测试。

### M2 首个驱动切片（实际完成）

实际完成：2026-11-04（Linux）。任务：#9–#17，发布说明见
[v0.1](../releases/v0.1.md)。

| 完成标准 | 证据 |
|---|---|
| 真实 gRPC 客户端完成 SQLite 查询与分批拉取 | `sqarvo-rpc-server/tests/grpc_sqlite_e2e.rs::sqlite_lifecycle_over_uds`：真实 UDS + 临时库，7 行按每批 3 行拉成 3 批并逐值校验 |
| 取消 2 秒内生效且幂等 | `::cancel_interrupts_running_fetch_within_deadline`：取批进行中 `Cancel`，实测耗时断言 `< 2s`，重复取消仍成功；错误码 `query.cancelled` |
| 断开连接后会话/查询计数归零 | 每个 e2e 用例结束时断言 `SessionManager::active_count() == 0` 且 `QueryRegistry::active() == 0` |
| CI 运行 SQLite 测试 | 作业 `41786163030`（commit 98c57a6）成功；显式步骤输出 `type_mapping` 12 用例、`grpc_sqlite_e2e` 3 用例全部通过，无 ignored/skipped |

配套验证（同一次运行）：`cargo test` 95 通过 / 0 失败，`fmt --check` 与
`clippy -D warnings` 通过，release 构建通过，真实二进制的权限/清理/日志脱敏实测通过。
命令与输出摘要见 [development-plan.md](development-plan.md) 的 S2 验证记录。

遗留（不属 M2 范围，已登记）：启动与空闲内存基准（M6 / `REQ-PERF-001`）；
Windows、macOS 未构建验证；事务与元数据 RPC（M4）；取消无法作用于「尚未返回首批」的执行
（契约层改进候选）。

### M3 凭据与 MariaDB

- 凭据只存系统凭据库，缺失时明确报错；
- MariaDB 查询、TLS、取消、错误分类可用；
- 通过 testkit。

### M3 凭据与 MariaDB（实际完成）

实际完成：2026-11-04（Linux）。任务 #20–#25；发布说明见 [v0.2](../releases/v0.2.md)，
支持矩阵见 [release-plan.md](release-plan.md)。PR 编号从 #31 开始，因此 #20（MariaDB 驱动骨架）
直接落在 main（`0c2ac4e`），没有 PR 可引用。

| 完成标准 | 证据 |
|---|---|
| 凭据只存系统凭据库，缺失时明确报错 | Linux Secret Service 实现 + 内存替身（#21，PR #31 / `692a917`）：`cargo test -p sqarvo-local-store` → 33 个单测 + `tests/credential_store.rs` 4 个用例通过；无会话总线 / 密钥环锁定 / D-Bus 权限拒绝 → `credential.unavailable`，引用不存在 → `credential.not_found`；`UnavailableCredentialStore` 仍是默认装配，代码路径不存在回退明文。gRPC 层（#27，PR #37 / `409dcbb`）：`cargo test -p sqarvo-rpc-server --test grpc_credential` → 11 通过（Store/Delete 幂等、空密码被拒且不覆盖原值、存储不可用、未知引用、密码不出现在引用与错误文案）。`credential_ref` 在 `Connect` / `TestConnection` 前解析（#23，PR #33 / `b186681`），解析失败不连库。**未验证**：真实密钥环用例 `secret_service_semantics` 由 `SQARVO_SECRET_SERVICE_TEST=1` 门控，本次验证未运行；Windows Credential Manager 与 macOS Keychain 未实现（装配为「不可用」）。 |
| MariaDB 查询、TLS、取消、错误分类可用 | 查询与分批：`cargo test -p sqarvo-driver-mariadb --test integration` → 21 通过（真实 MariaDB 11.4，含 `test_connection_reports_version`、`query_batches_follow_the_cursor`）。取消：同文件 `cancel_interrupts_long_query_and_is_idempotent`、`cancel_between_batches_fails_the_next_fetch_and_keeps_the_session_usable`、`cancelled_query_leaves_no_running_statement_on_the_server`（用 `information_schema.PROCESSLIST` 断言 `SELECT SLEEP(30)` 取消后消失），2 秒内生效、重复取消成功。错误分类：`wrong_password_is_reported_as_auth_failure`、`database_without_privileges_is_reported_as_permission_denied`、`unreachable_host_fails_within_timeout`，以及 `cargo test -p sqarvo-driver-mariadb` 的 27 个单测覆盖错误码映射。**TLS（2026-11-06，#106）**：新增 `crates/drivers/mariadb/tests/tls_handshake.rs`，8 个环境变量门控用例对 MariaDB 11.4.13 逐档取证，判据是服务端会话变量 `SHOW SESSION STATUS LIKE 'Ssl_cipher'` 非空（不是「连接成功」也不是错误码）：`disabled` 为空、`preferred` / `required` 为 `TLS_AES_256_GCM_SHA384`；`verify-ca`（固定根证书）与 `verify-full` 在证书带 `DNS:localhost, IP:127.0.0.1` SAN 的监听端同样为 `TLS_AES_256_GCM_SHA384`，`tls_server_name=localhost` 覆盖亦通过；`verify-full` 对无 SAN 的证书以 `mariadb.connection_failed` / `Unavailable` 拒绝；`preferred` 对无 TLS 监听端回退明文（`Ssl_cipher` 为空）并带 `mariadb.tls_fallback` 告警，`required` 同场景直接报 `mariadb.connection_failed`。运行命令与真实输出见 `docs/database/mariadb.md`「TLS 逐档验证（#106）」。**未验证**：客户端证书/双向 TLS、MySQL 8.x、远程后端；`verify-ca` 目前等同 `verify-full`（上游 `mysql_async` 0.37.1 的 `skip_domain_validation` 在 rustls 0.23 下失效，已作为已知缺口登记）；CI 尚未接入该测试文件。 |
| 通过 testkit | `crates/drivers/mariadb/tests/integration.rs::driver_passes_testkit_smoke`（`sqarvo_driver_testkit::smoke`）与 `::driver_passes_testkit_invariants`（五项不变量：#24 的取消幂等、多语句拒绝、超长单元格截断、分批无丢失、关闭幂等），随上述 21 个集成用例一起执行并通过；SQLite 侧同五项由 `passes_driver_testkit_invariants` 覆盖（`cargo test -p sqarvo-driver-sqlite` 13 通过）。 |

配套验证（2026-11-04，main `c6b9ab4`）：`cd backend && SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test_30' cargo test --workspace`
→ **219 通过 / 0 失败**（含 MariaDB 集成 21 与真实库 gRPC 凭据用例 `grpc_mariadb_credentials` 5；
测试账号为专用合成值，明文见 `tests/integration/docker-compose.yml`，此处按文档惯例写成 `***`）；
`cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings` 由 CI 作业 `后端（Linux）` 执行通过。
CI（两个作业 `后端（Linux）` / `MariaDB 集成测试（服务容器）` 均 pass）：
#21 → run `41797844064`、#22 → `41799321128`、#23 → `41801300984`、#25 → `41802430943`、
#24 → `41803584777`；main 合并后 run `41818532913`。

遗留（不属 M3 范围，已登记）：TLS 的客户端证书/双向 TLS 与 MySQL 8.x 兼容性（M5 / `docs/database/mariadb.md` 未支持清单与「TLS 逐档验证」未覆盖）；
显式事务与元数据（M4）；Windows / macOS 凭据库（M6 / v0.2.4）；「尚未返回首批」的查询无法用 `Cancel`
打断（契约缺口，见 [v0.2](../releases/v0.2.md) 已知限制）。

### M4 元数据与事务

- 两驱动下对象树可分页展开，列信息保留原始类型；
- 事务提交/回滚/失败态行为可测试；
- 不支持能力返回明确错误。

### M4 元数据与事务（实际完成）

实际完成：2026-11-05（Linux）。任务 #41–#48（PR #50–#57），发布说明见
[v0.2.1](../releases/v0.2.1.md)，支持矩阵见 [release-plan.md](release-plan.md)。
对象树只实现「当前连接库的表/视图 + 列 + 主键」一层；Catalog / Schema / Index / Sequence、
跨库浏览与 DDL 查看明确不在本版，留给 v0.2.2。

| 完成标准 | 证据 |
|---|---|
| 两驱动下对象树可分页展开，列信息保留原始类型 | SQLite：`cargo test -p sqarvo-driver-sqlite --test metadata` → 8 通过（`lists_tables_and_views_with_stable_pagination`、`columns_preserve_declared_type_and_nullability`、`primary_key_handles_single_composite_and_missing_keys`、`missing_objects_and_closed_sessions_are_classified`）；MariaDB：`SQARVO_MARIADB_URL=… cargo test -p sqarvo-driver-mariadb --test integration` → 48 通过，其中 `metadata_lists_tables_and_views_with_stable_pagination`、`metadata_columns_keep_original_types_and_nullability`（`COLUMN_TYPE` 原文、精度/标度/长度）、`metadata_primary_key_handles_single_composite_and_missing_keys`、`metadata_missing_table_is_not_found`、`metadata_without_selected_database_is_rejected` 覆盖元数据语义；gRPC：`cargo test -p sqarvo-rpc-server --test grpc_metadata_e2e` → 1 通过（`metadata_rpcs_are_wired_and_not_unimplemented`）；PR #54 / `100f43d`（CI run `41960916141`） |
| 事务提交/回滚/失败态行为可测试 | SQLite：`--test transactions` → 9 通过（`passes_transaction_invariants`、`only_serializable_isolation_is_accepted`、`read_only_transaction_rejects_writes_and_restores_writability`、`interrupted_write_puts_transaction_into_failed_state`、`statement_error_keeps_transaction_active`、`closing_session_rolls_back_open_transaction`、`raw_sql_begin_counts_as_open_transaction`），PR #51 / `fa2e747`（run `41955959769`）；MariaDB：集成 48 通过，含四档隔离级别的 `@@transaction_isolation` 断言、`BEGIN` → `CREATE TABLE` → `ROLLBACK` 的隐式提交拒绝、死锁进失败态与断连 `mariadb.transaction_lost`，PR #52 / `b24eef9`（run `41957364237`）；gRPC：`--test grpc_transactions_e2e` → 8 通过（`committed_changes_are_visible_to_later_sessions`、`rolled_back_changes_are_discarded`、`failed_transaction_rejects_commit_and_allows_rollback`、`second_begin_reports_already_active`、`repeated_commit_and_rollback_are_idempotent`、`session_status_reports_open_transaction`、`disconnect_forces_rollback_and_clears_registry`、`driver_without_transactions_reports_capability_unsupported`），PR #53 / `79ae6fd`（run `41958744849`） |
| 不支持能力返回明确错误 | `GetDdl` 在两驱动都取数据库原文（`ddl_returns_sqlite_master_text_verbatim`、`metadata_ddl_returns_show_create_text_verbatim`），拿不到原文的对象（SQLite 隐式索引、MariaDB 索引/触发器）返回登记过的错误码而非空串（`ddl_reports_internal_objects_without_stored_text`、`metadata_ddl_reports_unsupported_kinds_and_missing_objects`）；上层层级与跨库引用被拒而不返回空列表（`list_rejects_unknown_levels_tokens_and_oversized_pages_are_clamped`、`metadata_rejects_cross_catalog_and_bad_tokens`）；无主键表编辑返回 `table_data.no_primary_key`（`read_only_targets_and_invalid_keys_are_rejected_before_writing`）；未声明事务的驱动在 gRPC 层返回 `capability.unsupported`（`driver_without_transactions_reports_capability_unsupported`）；分页 token 非法 → `config.invalid`、对象集合变化 → `metadata.snapshot_changed`，都不回退第一页 |

配套验证（2026-11-05，证据基线 main `47da472`）：`cd backend && SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test' cargo test --workspace`
→ **382 通过 / 0 失败 / 0 ignored**（v0.2 基线 219）；`cargo fmt --all -- --check` 与
`cargo clippy --all-targets -- -D warnings` 通过；`cd frontend/linux && cargo test` → **94 通过**
（基线 66）；`bash scripts/check-tracking.sh --state all` → 「39 个 Issue：失败 0 项，警告 0 项」。
CI（两个作业 `后端（Linux）` / `MariaDB 集成测试（服务容器）` 均 pass）：
#50 → `41954735456`、#51 → `41955959769`、#52 → `41957364237`、#53 → `41958744849`、
#54 → `41960916141`、#55 → `41963333169`、#56 → `41965284410`、#57 → `41968317577`。

收尾批次（同日，#59–#62 / PR #63–#66，`c99a313`…`b4cc9a1`）：补齐 `ColumnMeta` 默认值/字符集/排序规则、
驱动能力位、`ReadOnlyReason` 的连接只读、写入路径会话执行锁与四处测试债。收尾后
`cargo test --workspace` → 408 通过、`frontend/linux cargo test` → 98 通过。

遗留处置：

- 已转入 v0.2.2 跟踪（Issue #81–#90）：契约增量、上层层级与跨库、DDL 查看、对象搜索、插入新行（#81–#85）；
  对象树/搜索/DDL 面板、列面板、插入行与隔离级别入口、取消提交（#86–#89）；发布文档（#90）。
  其中「取消提交」的后端语义已在 #70/#77 落地，前端只是还没有发起入口。
- 仍不属 v0.2.2：非主键排序、无主键表的受限定位（需产品口径）；
  事务「失败态」徽标目前只有 QA 注入证据（真实服务端失败难以确定性构造）；
  Windows / macOS 前端（M6 / v0.2.4）。

契约修订（2026-11-05，#68 / PR 见 Issue 评论）：`RowChangeResult.rows_affected` 与 proto 注释
对齐——一律填驱动实际影响行数（`CONFLICT` 取真实值，可能 `> 1`；`ROW_NOT_FOUND` 为 0），
`total_rows_affected` = Σ 逐行值并包含冲突行；判定语义不变（仍是「影响 1 行才算成功」）。
证据：`cargo test -p sqarvo-rpc-server --test grpc_row_failures_e2e` → 4 通过
（新增 `mixed_batch_totals_include_conflict_rows`；把 rpc 映射回退到基线时这两条断言失败：
`left: 0, right: 1`）；`cargo test --workspace`（带 DSN）→ 410 通过、MariaDB 集成 50 条真实执行
（无「跳过：未设置 SQARVO_MARIADB_URL」输出）。

### M4 元数据与事务（v0.2.2 收尾）

实际完成：2026-11-06（Linux）。任务 #81–#85、#87，#86/#88/#89 为前端闭环；发布说明见
[v0.2.2](../releases/v0.2.2.md)，支持矩阵见 [release-plan.md](release-plan.md)，证据基线 main `9f3d2ce`。

| 完成标准 | 证据 |
|---|---|
| 两驱动下对象树可分页展开，列信息保留原始类型 | SQLite：`cargo test -p sqarvo-driver-sqlite --test metadata_hierarchy` → 8 通过（含跨库往返、含引号/空格/中文的 catalog、按（种类, 名字）分页无重无漏）；`--test metadata` → 13 通过。MariaDB：`SQARVO_MARIADB_URL=… cargo test -p sqarvo-driver-mariadb --test integration` → 69 通过，含 `metadata_catalog_layer_lists_every_visible_database`、`metadata_catalog_children_are_scoped_to_that_database`、`metadata_table_children_include_columns_indexes_and_triggers`、`metadata_hierarchy_pagination_has_no_duplicates_or_gaps`；PR #94 / `43373e7`（CI run `42049499508`） |
| DDL 可查看且来源可信 | SQLite：`--test metadata` 的 5 个 `ddl_*` 用例（`ddl_returns_sqlite_master_text_verbatim`、`ddl_is_not_truncated_at_cell_limits`、`ddl_reads_attached_database_by_catalog` 等）；隐式索引无原文 → `capability.unsupported`。MariaDB：4 个 `metadata_ddl_*`（表/视图列选择/序列/不支持种类）；`grpc_metadata_e2e` 1 通过（断言 `exact=true`、`source=DDL_SOURCE_DATABASE`）。执行锁：`grpc_session_locks_e2e` 8 通过含 `ddl_holds_lock_against_apply` / `ddl_waits_for_apply`；PR #95 / `0a8e21f`（run `42052752392`） |
| 不支持能力返回明确错误 | `ListNodes` 的 schema / sequence 层 → `capability.unsupported`；叶子/种类不适用 → `metadata.object_kind_unsupported`；库/表不存在 → `metadata.object_not_found`；无权限 → `metadata.permission_denied`；过滤非法 → `metadata.filter_invalid`；MariaDB 索引/触发器 DDL → `metadata.object_kind_unsupported`（不拼装）；插入的 `WITHOUT ROWID` / 省略非自增主键 → `capability.unsupported`。均有单元或集成用例，且前端按错误码分文案 |
| 对象搜索可用（v0.2.2 追加） | SQLite `--test metadata_filter` 13 通过 / 1 ignored、MariaDB `--test metadata_filter` 9 通过 / 1 ignored；转义（`%`/`_`/`\` 字面量）、过滤下推、换条件旧 token 失效均有断言；PR #96 / `8e7c661`（run `42059959553`） |

判定：**完成**。M4 的原完成标准对两个内置驱动都满足；以下不阻塞完成，但如实记录：

- **schema 层没有实现**：SQLite / MariaDB 都没有独立 schema（请求即 `capability.unsupported`），
  PostgreSQL 驱动未实现，因此没有 schema 层的正例证据。M4 的目标「对象树」以 Catalog →
  表/视图/序列 → 列/索引/触发器 的层级交付，不把不存在的层伪装出来。
- **MariaDB 索引/触发器没有独立 DDL**：`ObjectRef` 不携带所属表，无法唯一定位索引；按契约
  明确报 `metadata.object_kind_unsupported`。支持它需要 proto 增量，留后续版本。
- **搜索的规模证据不对称**：SQLite 10 万对象 9.3 ms；MariaDB 只测 1 万对象 7.2 ms（10 万未实测，
  原因见 [v0.2.2 发布说明](../releases/v0.2.2.md)）。
- **前端闭环**：对象树/DDL 在两个驱动上真机跑过；搜索与插入的 UI 闭环本轮只在 SQLite 上复跑。

### M5 第二网络驱动与一致性

- PostgreSQL 驱动可用；
- 同一 testkit 覆盖三驱动（SQLite / MariaDB / PostgreSQL），差异有文档。

### M6 前端接入与硬化

- 三端至少一端完成真实连接与查询（其余记录状态）；
- 启动/内存/大结果集基准可复现；
- 安全清单通过；
- Windows / macOS 至少各完成一次真实构建并记录。

### M6 前端接入与硬化（首端进度）

状态：**进行中**，不标记完成。Linux 首端接入完成（2026-11-04）；Windows / macOS **未构建、
未验证**；三端接入、跨平台构建与 release 构建基准尚未满足，见本节末尾的剩余条件。

Linux 已完成（证据均为 PR / commit / CI run 或可直接执行的命令）：

| 能力 | 证据 |
|---|---|
| 启动后端、读端点文件、校验 `protocol_version`、token 握手、退出清理 | PR #36 / `d4d278d`（CI run `41809894166`）；`cd frontend/linux && cargo test` → 66 通过（端点解析、协议主版本不兼容、pid 存活、清理路径的单元测试在此集合内）；启动顺序见 `frontend/linux/README.md` |
| 连接对话框、密码只写系统凭据库、测试连接、会话列表 | PR #37 / `409dcbb`（run `41811993419`）；配置文件 `$XDG_CONFIG_HOME/sqarvo/Sqarvo/connections.json`（目录 0700、文件 0600，仅 `credential_ref`，无密码字段） |
| SQL 执行、取消、每批 500 行预取、`GtkColumnView` 虚拟化、类型显示、取消/断连状态 | PR #38 / `835373f`（run `41815806079`） |
| 客户端结果集上限、背压、可复现基准脚本 | PR #39 / `c6b9ab4`（run `41818263864`）；数值见 [v0.2](../releases/v0.2.md)「性能基准」 |
| 构建修复：后端 `build.rs` 监听整个 `proto/` 目录 | `8e967cb`（run `41812812519`），避免新增 `.proto` 不触发重新生成 |
| 对象树、搜索与 DDL 面板（v0.2.2） | PR #98 / `a6008c1`（run `42063985353`）；真机 SQLite `--qa-tree-expand main --qa-tree-open orders --qa-tree-ddl orders` 与 MariaDB `--qa-tree-expand sqarvo_test_90` 均 exit 0，断言读控件文本；截图（GTK 自渲染）`90-tree-ddl.png`、`90-tree-ddl-mariadb.png`、`90-search-jump.png` |
| 列信息浮层（默认值四态 / 字符集 / 排序规则） | PR #92 / `bcb12c4`（run `42047837242`）；`--qa-column-info` 点真实列头并断言六项文本 |
| 插入新行与隔离级别入口（v0.2.2） | PR #97 / `a59d78d`；SQLite 真机插入 + 外部 `sqlite3` 复核、唯一冲突整批回滚且外部计数不变；`--qa-isolation-dialog` 输出「可用 2 项 / 禁用 3 项（附原因）」 |
| 提交中取消（在途批次登记） | PR #99 / `9f3d2ce`（run `42067912917`）；`cargo test` 192 通过（含 13 项状态机/文案用例）；#89 的真机日志：`CancelRowChanges` 往返 1 ms、已执行 128 条、外部 SQL 0 行变化 |

可复核命令（2026-11-04，Linux，main `c6b9ab4`）：

```bash
cd frontend/linux && cargo test          # 66 passed / 0 failed
cd frontend/linux && ./scripts/bench.sh  # 冷启动 / 空闲内存 / 10 万行 / 200 列宽表
```

v0.2.2 复核（2026-11-06，main `9f3d2ce`）：

```bash
cd frontend/linux && cargo test          # 192 passed / 0 failed
cd frontend/linux && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings  # 均 exit 0
```

Windows / macOS 现状：`frontend/windows` 与 `frontend/macos` 只有窗口骨架，**未接后端、
未在本机或 CI 构建、未做任何运行验证**（`.github/workflows/ci.yml` 末尾注明两端待接入后
再增加构建作业）。M6 的「三端至少一端完成真实连接与查询」由 Linux 满足，但这不等于
Windows / macOS 可用。

M6 剩余完成条件：

1. Windows、macOS 各完成一次真实构建并接入后端（至少握手 + 一次查询），CI 增加对应构建作业；
2. 三端连接/执行/取消/结果虚拟化语义一致，差异有记录；
3. **release 构建**的冷启动与内存基准（当前数值全部来自 debug 构建）；
4. 安全清单在三端各自复跑（日志脱敏、端点/管道权限、IPC 仅本机）；
5. 宽表内存分配点定位与客户端结果集上限的计量口径修正（`REQ-RESULT-005` 的已知偏差）。
6. v0.2.2 新增的搜索与插入 UI 闭环需要在 MariaDB 上各跑一次（本轮 MariaDB 只覆盖对象树/DDL）。

### M7 导入导出与结构管理

- CSV / JSON / SQL 导入导出流式处理，10 万行场景有内存上限与基准记录；
- 导入失败可定位到行/列，取消后临时文件与游标被清理；
- 路径穿越、覆盖已有文件、超限输入三类情况均被明确拒绝；
- 结构对比只生成脚本，任何路径都不自动执行 DDL；
- 参数化执行在 SPI 与 proto 中冻结，不支持的驱动返回 `capability.unsupported`。

## 范围调整记录

- 2026-11-05：版本序列整体改标到 0.2 线：v0.3（数据浏览与编辑）→ **v0.2.1**、
  v0.4（对象管理）→ **v0.2.2**、v0.5（导入导出）→ **v0.2.3**、v1.0（首个完整可用版本）→ **v0.2.4**。
  各版本范围与退出标准不变；0.2 线改为以 `0.2.x` 增量交付，暂不设 1.0 / 兼容承诺节点。
  M4 对应版本随之改为 v0.2.1 / v0.2.2。同步更新：`release-plan.md`、`roadmap.md`、
  `requirements.md`（`REQ-SQL-004` 再评估时点改记 v0.2.2+）、`development-plan.md`（S5 / S7 版本标注）、
  `docs/planning/README.md`、`docs/releases/v0.2.md`、`README.md`，以及 ADR 0010 的版本标注
  （保留原记号，只补改标说明）。
- 2026-11-04：首个网络数据库由 PostgreSQL 改为 MariaDB（S3 / M3），第二个网络数据库由
  MySQL 改为 PostgreSQL（S5 / M5），v0.2.4 三驱动矩阵为 SQLite / MariaDB / PostgreSQL。
  理由：MariaDB 驱动基于 MySQL 协议，可在不新增第二套协议栈的前提下覆盖 MySQL 兼容场景，
  同时把与 SQLite 差异更大的 PostgreSQL 留作第三个驱动来验证 SPI 的扩展性。
  同步更新：`development-plan.md`、`roadmap.md`、`release-plan.md`、`risk-register.md`、
  `docs/database/driver-spi.md`、`tests/integration/`，以及 GitHub 里程碑 M3 / M5 标题；
  决策记录见 [ADR 0010](../decisions/0010-database-onboarding-order.md)。

## 变更规则

- 任务延期不改里程碑；只有范围变化或技术前提失效才调整完成标准；
- 调整必须记录在 PR 或 ADR 中，并同步 `development-plan.md` 与 `release-plan.md`；
- 里程碑完成时在本文追加「实际完成」日期与验证证据，不删除原标准。
