# 产品需求

本文件定义功能范围与优先级，是路线图与里程碑的上游依据。

- 优先级：**P0** 首个可用版本必须；**P1** 后续版本；**P2** 已规划但未排期。
- 状态：**已实现**（代码与测试均存在）、**部分**（契约或部分逻辑已存在，尚不可用）、
  **未实现**。任何未实现能力不得在 UI 中表现为可用。
- 验收标准写在需求行内；具体任务拆解在 GitHub Issues，不在本文件重复维护。

## 1. 连接与凭据

| ID | 需求 | 优先级 | 状态 | 验收要点 |
|---|---|---|---|---|
| REQ-CONN-001 | 配置连接（驱动、主机、端口、库、用户、TLS 模式） | P0 | 部分 | 后端 SQLite（本地文件 + 只读）与 MariaDB（主机/端口/库/用户/5 档 TLS 模式）驱动可用；5 档 TLS 已在真实服务端逐档验证（#106，2026-11-06：`disabled` 明文、`preferred`/`required` 协商 `TLS_AES_256_GCM_SHA384`、`verify-ca`/`verify-full` 固定根证书 + SAN 证书通过、`verify-full` 主机名不匹配被拒，见 `docs/database/mariadb.md`「TLS 逐档验证」；`verify-ca` 在 MariaDB 驱动下归一化为 `verify-full`（ADR 0020，能力位 `tls_verify_ca_chain_only = false`，连接对话框标注）；客户端证书未验证）；`credential_ref` 在连接与测试连接前解析（#23，PR #33）；Linux 前端连接对话框（#27，PR #37）只提供 MariaDB，SQLite 无 UI；Windows/macOS 未接入 |
| REQ-CONN-002 | 测试连接（延迟、服务端版本、告警） | P0 | 部分 | SQLite 与 MariaDB 实测可用（`cargo test -p sqarvo-driver-mariadb --test integration` 的 `test_connection_reports_version`；MariaDB 的 TLS 回退给出 `mariadb.tls_fallback` 告警——#106 真实回退验证：对无 TLS 监听端回退后会话 `Ssl_cipher` 为空，同一监听端用 `required` 直接报 `mariadb.connection_failed`），Linux 前端对话框展示延迟与版本（PR #37）；PostgreSQL 与未注册驱动返回 `driver.not_found` |
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
| REQ-META-005 | 表/视图数据预览（分页、排序、筛选、总数） | P1 | 已实现 | **已交付（v0.2.2）**：`TableDataService.BrowseTable` 固定主键升序 + 不透明 cursor，每页上限 500，游标非法 → `table_data.cursor_invalid` 且不回退第一页；无主键表/视图返回 `read_only=true`（原因 `NoPrimaryKey`、排序 `Unstable`）。SQLite `cargo test -p sqarvo-driver-sqlite --test table_data`（9 用例）、MariaDB 集成用例、gRPC `cargo test -p sqarvo-rpc-server --test grpc_table_data_e2e`（#46，PR #55 / `8775dda`）；Linux 数据页按主键翻页并显示原始类型列头。**v0.2.3 改写分页口径（ADR 0022）**：请求增 `offset` / `sort` / `filter` / `include_total`，响应增 `total_rows` / `total_kind`（UNKNOWN / ESTIMATE / EXACT 三态必须显式），`offset` 与 `page_token` 互斥；排序与筛选由驱动下推，列名走白名单、值全部参数化绑定；offset 分页跨页不保证稳定（重复/漏行是固有属性，界面提示不修复），大 offset 跳转不提供。**v0.2.3 驱动侧已落地（#110 / #111）**：SQLite 与 MariaDB 均把 `ORDER BY`（多列、方向）/ `WHERE`（9 算子、仅 AND）/ `LIMIT ? OFFSET ?` / `COUNT(*)` 下推到 SQL，列名走 `GetColumns` 白名单（未知列 → `config.invalid`，不发 SQL）、值全部参数化绑定，能力位 `table_browse_sort_filter_total` 翻 `true`。合并提交 `746a78f`（#124）、`17e57dc`（#125）；证据：`cargo test -p sqarvo-driver-sqlite` 131 通过（新增 `table_browse_options`、`busy_timeout`）、`cargo test -p sqarvo-driver-mariadb`（专用库）160 通过（新增 `browse_pushdown` 13），两条 lane 都做了「注释掉排序分支 → 用例变红」的反证；合并后 main 全量 601 通过。**未完成**：前端分页器/列头排序/筛选面板入口在 #118；总数三态在大表上的**可取消与超时**仍是契约缺口（`browse` 无取消令牌），见 #127；MariaDB 计数阈值 `EXACT_COUNT_MAX_ESTIMATED_ROWS = 100_000` 是常量；两驱动都未做「筛选值类型不兼容」的预校验（`int_col = 'abc'` 会得到空结果） **v0.2.3 收口**：偏移分页 + 排序 + 筛选 + 总数三态已由两驱动下推到 SQL 并由前端表格接完（#110/#111/#118/PR #124、#125、#132），能力位 `table_browse_sort_filter_total` 为 true；跨驱动一致性由 testkit 不变量锁住（#112/PR #128）。精确计数改为**有界超时**（`timeout_millis` 默认 2 s、上限 60 s，超时 `UNKNOWN`+`total_skipped`，#127/PR #130，ADR 0022 修订）。**剩余**：客户端主动取消计数不可用（一元 RPC 无取消通道，ADR 0022 方案 B 未采纳）；MariaDB 计数阈值 `EXACT_COUNT_MAX_ESTIMATED_ROWS = 100_000` 为常量；筛选值类型不兼容未预校验（得到空结果而不是错误）。**v0.2.3 增量**：跨库表数据读写已支持——表数据/行变更/插入按 `ObjectRef.catalog` 定位，连接默认库可留空（ADR 0024，#140 驱动 / #141 前端，验收链 `mariadb-nodb` 用例 139 项断言全过） |
| REQ-META-006 | 对象详情（库与表的属性摘要） | P2 | 未实现 | 需要新增 `MetadataService.DescribeNode(ObjectRef) → NodeDescription`，按 kind 分 typed 子消息（库：默认字符集/排序规则、对象计数；表：引擎、行数估计、数据与索引大小、字符集/排序规则），不支持的字段必须显式标注原因而不是留空。**v0.2.3 不实现**：信息面板只显示已有 RPC 能给出的内容（名称、列、主键、索引/触发器列表、DDL、已加载行数），拿不到的字段不出现。计划与 PostgreSQL 驱动（M5）一并设计，因为 PostgreSQL 的 `reltuples` / `pg_class` / 字符集正好同类。能力差异按驱动记录（SQLite 无表大小与引擎概念） |

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
| REQ-RESULT-003 | 复制（单元格/行/列）与导出（CSV/JSON/SQL） | P1 | 部分 | 流式写出，不一次性入内存。**v0.2.3 交付导出当前结果 / 当前页为 CSV**（M7）：流式写出、路径校验、默认不覆盖已有文件、中途取消不留半截文件，10 万行有基准；**未实现**：单元格/行/列复制、JSON 与 SQL 格式、整表导出（按整表逐页拉取导出属于 M8 / v0.2.5） **v0.2.3 已交付**：导出当前查询结果（已加载行）或表当前页为 CSV，流式写出、绝对路径校验、默认不覆盖（`RENAME_NOREPLACE`）、可取消且不留半截文件，10 万行 0.418 s / 峰值 RSS 增量 824 KiB（#120/PR #136）。CSV 公式注入转义与截断值标记**v0.2.3.1 已交付**（#137 / PR #150）：文本类值危险前缀前置单引号（默认开启、可关）、数值类保持原样、截断单元格在文件末尾追加 `# sqarvo: …` 注释行；六类前缀单测 + 开关两种口径。**未实现**：单元格/行/列复制、JSON 与 SQL 格式、整表导出（M8） |
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
| REQ-SEC-003 | 高危操作提示与影响范围说明 | P1 | 部分 | 删除/覆盖/批量更新 **v0.2.3.1 进度**：行删除与批量更新有风险确认并说明影响行数（ADR 0017）、导出覆盖已有文件需二次确认、删除连接需二次确认并说明「配置条目 + 凭据条目」两项副作用并尽力清理密钥条目（#144 / PR #153）。**未实现**：跨表/整库级操作的影响范围预览与统一的高危操作模板（结构修改、整表导出属 M8 后再定） |
| REQ-PERF-001 | 启动与空闲内存基准 | P1 | 已实现 | `cd frontend/linux && ./scripts/bench.sh`：冷启动 3 次中位数 release 235 ms / debug 240 ms、空闲 5 秒 VmRSS release 141.6 MB / debug 149.3 MB（2026-11-06 release、2026-11-04 debug，同一台 Ryzen 7 8745H / 24.8 GiB）；脚本可重复执行并输出机器可读记录，两种构建均已实测（release 三轮复跑见 `docs/releases/v0.2.2.md`） |
| REQ-PERF-002 | 大结果集与宽表性能测试 | P1 | 已实现 | 10 万行 release 2095 ms / 184.6 MB（debug 2808 ms / 191.6 MB）、200 列 × 2000 行 release 2927 ms / 595.6 MB（debug 3554 ms / 603.4 MB），不滚动 3 秒仍 500 行（背压，两种构建一致），见 `docs/releases/v0.2.2.md`；**未达标项**：宽表内存随列数上升且 release 下几乎不变（分配点未定位）；估算字节不含 GTK 渲染开销 |
| REQ-A11Y-001 | 键盘可达与可访问性 | P1 | 部分 | 现有：`Ctrl+Enter` 执行、`Escape` 取消、`Ctrl+Q` 退出、`Ctrl+W` 关闭窗口。**v0.2.3 随紧凑密度一并补齐最小集**（`docs/ui-specification/workspace-model.md` §10）：新建连接 / 新建查询标签 / 关闭标签 / 刷新 / 对象搜索 / 信息面板显隐 / 切换标签；验收要求键盘能走完「树 → 标签 → 表格 → 面板」，紧凑化后焦点环可见、最小点击区 24×24、正文对比度 ≥ 4.5:1。**未实现**：完整的可访问性名称体系与屏幕阅读器验证 **v0.2.3 已落地最小集**：`Ctrl+N` 新建连接、`Ctrl+T` 新建查询标签、`Ctrl+W` 关标签、`Ctrl+I` 信息面板、`Ctrl+F` 聚焦对象树搜索框（`a74ca8f`，验收链经 `win.focus-search` 断言）、`Ctrl+Shift+E` 导出、`F5` 刷新、`Ctrl+Enter` 执行、`Escape` 取消；紧凑密度下键盘可走完「树 → 标签 → 表格 → 面板」。**未实现**：完整的可访问性名称体系与屏幕阅读器验证 |
| REQ-I18N-001 | 国际化 | P2 | 未实现 | 当前界面文案为中文 |

## 8. 界面与工作区

结构定义见 [workspace-model.md](../ui-specification/workspace-model.md)；决策见
ADR 0021（标签与会话）、ADR 0022（分页/排序/筛选/总数）、ADR 0023（三端映射）。
本组条目由 v0.2.3 / M7 交付，落地前状态保持「未实现」。

| ID | 需求 | 优先级 | 状态 | 验收要点 |
|---|---|---|---|---|
| REQ-UI-001 | 工作区模型：六区域、标签、多连接 | P1 | 已实现 | 外壳 / 主工具条 / 对象树 / 标签工作区 / 状态栏 / 信息面板六区域，1200×800 与 960×640 两档无裁切、可拖拽；固定 `Objects` 页 + 表数据标签 + 查询标签；**一标签一会话**（ADR 0021），关闭有脏行的标签必须三选，标签上限可配置并如实报错；对象树并列多连接并按种类分组，未实现的种类分组（Functions / Queries）不出现 **v0.2.3 交付**（#113/#114/#116/PR #123、#126、#129）：六区域外壳与可拖分栏、多连接注册表 + 每连接一条浏览会话、`Tables`/`Views` 种类分组（`filter.kinds` 逐组拉取，验收链断言展开 catalog 的 `ListNodes` 次数=2）、标签工作区（一标签一会话、关闭三选、上限 8）；两档布局自检 0 超限；`Functions`/`Queries` 分组不出现；`Objects` 固定页显示当前选中对象概览（对象名/种类/限定名/关键属性 ≤5 项，与信息面板同一份快照，无选中给空态；#135 / PR #152）
| REQ-UI-002 | 对象命名与作用域呈现 | P1 | 已实现 | 标题 `对象 @ catalog (连接名)`、标签名 `对象 @ catalog`，连接名不重复进标签；事务作用域是**标签会话**而不是表，事务状态必须同时出现在标签与状态栏；一个标签的未提交更改对其它标签不可见时要有提示（ADR 0021） **v0.2.3 交付**（#116/#117/PR #129、#131）：标题 `对象 @ catalog (连接名)`、标签名 `对象 @ catalog`；事务作用域=标签会话，状态同时出现在标签与状态栏；两标签事务互不可见有真机证据；取消/关闭路径按 `tab_id` 归属（`backend/routing.rs` 8 个单测） |
| REQ-UI-003 | 信息面板（连接 / 库 / 表） | P1 | 已实现 | 右侧面板宽 280（可拖 240–520、可折叠、宽度按连接记忆）；连接显示连接属性、库显示库属性、表/视图在面板内切换 `[DDL] / [表信息]`；拿不到的字段不出现，权限 / 对象不存在 / 不支持种类各有文案与错误码；DDL 从底部 revealer、列信息从页内浮层迁入 **v0.2.3 交付**（#119/PR #134）：连接/库/表/列/仅 DDL 六态，表内切换 `[DDL] / [表信息]` 并按标签记忆；底部 DDL 面板与列信息浮层已迁入；折叠省 280px 有反证；拿不到的字段不出现；**剩余**：~~宽表（7 列以上）会把面板压到 ~198px~~ → **v0.2.3.1 已修复**（#133 / PR #151）：类型条带改为 `External` 横滚策略，不再把 200×列数 上报为最小宽度；9 列宽表在 1200×800 下信息面板保持 280px，两档布局自检 0 超限 |

## 9. 非功能约束（适用于全部需求）

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
