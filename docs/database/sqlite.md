# SQLite 驱动

实现：`backend/crates/drivers/sqlite`（`rusqlite` 0.41，bundled）。
选型依据见 [ADR 0009](../decisions/0009-sqlite-driver.md)，通用约定见 [Driver SPI](./driver-spi.md)。

- 已验证：`rusqlite` bundled 版本（Linux CI 与本地一致）；声明最低 SQLite 3.35（表值 `pragma_*` 函数与
  `RETURNING` 之前的稳定基线）。
- 每个会话一个专用阻塞线程（actor），独占 `Connection`、当前语句与游标；异步侧只通过命令 channel
  交互。阻塞线程不占用 Tokio worker。

## 类型与值

SQLite 只有 5 种存储类，列上的声明类型只是「亲和性提示」，实际存储可能与声明不一致：

| 实际存储类 | `Value` |
|---|---|
| NULL | `Null` |
| INTEGER | `Int(i64)` |
| REAL | `Float(f64)` |
| TEXT | `Text(String)`（非法 UTF-8 时退回 `Bytes`，不用 `U+FFFD` 静默替换） |
| BLOB | `Bytes` |

声明类型写入 `ColumnMeta.original_type_name`。`NUMERIC` / `DECIMAL` / 日期类**不伪造**具体类型：
它们可能以 INTEGER/REAL/TEXT 存储，统一按 `ColumnType::Unsupported` 返回并保留声明原文（ADR 0009）。
超过 64 KiB 的文本/二进制按 ADR 0007 截断并置 `Cell::truncated`。

## 行变更批次取消（#70）

`ApplyRowChanges` 的取消**不走** `sqlite3_interrupt`：中断会把 DML 撕在半路，SQLite 随即隐式
回滚整个事务，事务模式下列就不能取消、外层事务也废了（ADR 0018）。

- 驱动在行循环里**每条之前**读一次 `RowChangeCancel`；命中后不再执行后续变更；
- 自动提交模式：整批 `ROLLBACK`，库里 0 行变化；
- 事务模式：`ROLLBACK TO SAVEPOINT sqarvo_row_changes` + `RELEASE`，外层事务继续可用；
- 返回 `cancelled = true`、`executed_changes = 已执行条数`、`results = []`。

端到端证据（`crates/rpc-server/tests/grpc_row_cancel_e2e.rs`）：500 行批次用
`BEFORE UPDATE` 触发器（递归 CTE，25 000 层，约 10 ms/行）拖慢，在批次进行中取消，
实测「执行 8 行后取消、库里 0 行改动、取消耗时约 1.7 ms」——取消耗时远小于单行耗时，
说明它没有排在批次/执行锁后面。

## 事务

- `BEGIN IMMEDIATE`（读写）/ `BEGIN DEFERRED`（`read_only = true`），`COMMIT` / `ROLLBACK`；
- 一个会话同时最多一个打开事务，重复 `Begin` 返回 `transaction.already_active`；
- 只读事务用连接级 `PRAGMA query_only` 由 SQLite 自己拒绝写入，不依赖解析 SQL；
- SQLite 在部分错误（被中断、磁盘满、IO 错误）后会**自动回滚**整个事务，驱动把簿记切到失败态，
  只允许回滚，绝不把后续语句当成仍在事务里执行；
- 重复提交/回滚与未知事务 ID 幂等成功（ADR 0017 §3）。

与 MariaDB 的差异（隐式提交、只读事务、隔离级别、断连、死锁）见
[mariadb.md](./mariadb.md#与-sqlite-的差异)，逐维度的两驱动对照表见
[driver-spi.md](./driver-spi.md#事务实现现状v021)。

## 只读连接（`read_only`）

`ConnectionParams.properties["read_only"] = "true"` 让连接以 `SQLITE_OPEN_READONLY` 打开
（见 `session.rs` 的 `open_connection`）：文件不存在时连接失败（`sqlite.open_failed`，
**不会**创建文件），`":memory:"` 与只读互斥（`config.invalid`）。

只读连接对表数据的影响（#60）：

- **浏览照常**：`BrowseTable` 仍按主键升序、仍签发键集游标、`row_order` 不变。只读限制
  的是写，不是读，因此不做「只读就不许翻页」这类猜测性降级；
- **浏览即置只读**：`read_only = true` 且 `read_only_reason = READ_ONLY_REASON_CONNECTION_READ_ONLY`。
  驱动用 `sqlite3_db_readonly`（rusqlite 的 `Connection::is_readonly`）读连接的**真实**
  标志，而不是靠参数回显；
- **写入在准备语句前就被拒**：`ApplyRowChanges` 返回 `table_data.read_only`
  （`FAILED_PRECONDITION`），不把 `SQLITE_READONLY` 透给用户；被拒的批次不留任何副作用；
- **与无主键同时成立时报「连接只读」**：它影响该连接上的所有表，也是用户唯一能改变的
  条件；「无稳定排序」仍由 `row_order = ROW_ORDER_UNSTABLE` 单独表达，信息不丢；
- **只读事务不算「连接只读」**：`PRAGMA query_only` 是会话级临时开关，由事务徽标与写入
  拒绝负责表达，事务结束后不该继续显示「连接只读」。因此只读事务期间的浏览仍是
  `read_only_reason = UNSPECIFIED`（可编辑），提交时才被拒。

MariaDB 没有「只读连接」概念，该原因在 MariaDB 上不会出现，见
[mariadb.md](./mariadb.md#连接级只读不适用)。

## 元数据（v0.2.1 最小子集）

实现：`backend/crates/drivers/sqlite/src/metadata.rs`。

| 请求 | 数据来源 | 说明 |
|---|---|---|
| `list_children(None, page)` | `sqlite_master` | `type IN ('table','view')` 且排除 `sqlite_*` 内部表，`ORDER BY name`；`name` / `type` 原样映射到节点 |
| `columns(object)` | `pragma_table_info(name)` | 声明类型（`type`）、可空（`notnull`）、列序号（`cid`）、默认值（`dflt_value`） |
| `primary_key(object)` | `pragma_table_info(name).pk` | `pk > 0` 的行按 `pk` 排序；`pk` 就是**主键内顺序**（1 开始），不是表内列序号 |
| `ddl(object)` | — | 显式 `capability.unsupported`，文案写明「v0.2.2 提供」；v0.2.2 契约已冻结（#81，ADR 0019）：实现时取 `sqlite_master.sql`，给 `exact = true` + `source = DDL_SOURCE_DATABASE` + 来源说明 |

层级范围：SQLite 没有 catalog / schema 层级，根节点（`parent = None`）直接返回当前数据库文件的
表与视图。带 `catalog` 或 `schema` 的引用、以及 `Catalog` / `Schema` / `Sequence` 等上层或更深层级
一律返回 `capability.unsupported`，**不返回空列表**冒充「没有对象」。

对象名解析按 SQLite 语义大小写不敏感（`COLLATE NOCASE`），`pragma_table_info` 使用同一个名字；
返回节点的名字取自 `sqlite_master` 中的原始大小写。`pragma_table_info` 通过**绑定参数**调用
（`pragma_table_info(?1)`），不拼接标识符。

无主键表返回空列表（浏览仍可用但置只读 `NoPrimaryKey`/`Unstable`，编辑被拒绝并返回
`table_data.no_primary_key`，见 #46/#47）；视图同样没有主键，返回空列表而不是报错。
表/视图不存在返回 `sqlite.object_not_found`（`NotFound`），不会返回空列。

### 列默认值、字符集与排序规则（#59）

`ColumnMeta` 的四个默认值状态全部来自 `pragma_table_info.dflt_value`，驱动不猜：

| `dflt_value` | `ColumnMeta.default_value` | 说明 |
|---|---|---|
| SQL NULL | `NoDefault` | 没有 `DEFAULT` 子句。可空列也**不**记录隐式默认值，所以这里是「没有默认值」，不是「默认 NULL」 |
| 文本 `NULL` | `Null` | 显式 `DEFAULT NULL`，与「没有默认值」不同 |
| 其余文本 | `Expression(原文)` | **未求值的 SQL 文本**：`'x'`、`''`、`CURRENT_TIMESTAMP`、`1 + 1` 都原样交付 |

要点：

- `dflt_value` 是建表语句里的 SQL 文本，不是求值结果。`DEFAULT 1+1` 给出 `1 + 1`，
  `DEFAULT CURRENT_TIMESTAMP` 给出 `CURRENT_TIMESTAMP`；驱动不求值、不去引号、不做归一。
  （SQLite 自己会去掉最外层括号，驱动只透传它拿到的文本。）
- 默认空串 `DEFAULT ''` 是 `Expression("''")`，不是 `NoDefault`，也不是空串占位。
- **SQLite 没有列级字符集**：`character_set` 永远为 `None`（proto 字段不设置）。
  排序规则只写在建表 SQL 里，`pragma_table_info` 不提供，本版也不解析 DDL，
  因此 `collation` 同样为 `None`。前端必须把 `None` 显示为「不适用」，而不是「空字符集」。
- 结果集路径（`SELECT` 返回的列）没有默认值信息，`default_value` 是 `Unspecified`（未知），
  字符集/排序规则为 `None`。需要权威信息时用 `MetadataService.GetColumns`。

### 分页 token 与失效条件

与 MariaDB 驱动共用 `sqarvo-metadata::paginate_by_key`，token 为
`v1:<offset>:<16 位十六进制指纹>`：

- 指纹是有序对象名集合的 FNV-1a 64 位摘要，翻页时重新校验；
- token 结构非法、版本未知、偏移量越界 → `config.invalid`（`INVALID_ARGUMENT`），绝不回退第一页；
- 翻页期间对象名集合变化（新建/删除/改名）→ `metadata.snapshot_changed`
  （`FAILED_PRECONDITION`），应重新从第一页读取；
- 同名对象的 `DROP` + `CREATE`（名字集合不变）不会被指纹区分。

一次请求会读取当前库的全部对象名再在内存切页（每页上限 500）；对象极多时应改为服务端游标（待定）。

### v0.2.2 契约（#81；插入行已实现，过滤与 DDL 见 #82 / #84）

`ListNodes.filter` 与 `InsertRows` 的契约已冻结（ADR 0019），SQLite 侧实现见 #82 / #83：

- **过滤下推**：`name.contains` → `sqlite_master.name LIKE '%' || ? || '%' ESCAPE '\'`
  （`%`、`_`、`\` 全部转义为字面量）；`name.exact` → `name = ?`；`kinds` → `type IN (...)`
  （`table` / `view`）。大小写按 SQLite `LIKE` 的 ASCII 不敏感行为，**不得**额外 `lower()`
  （会让 `sqlite_master` 上的名字比较退化为全扫且与 MariaDB 行为不一致）；
- **过滤条件进 token 指纹**：过滤条件的规范化字符串必须与对象名集合一起进指纹，换条件后旧
  token 报 `config.invalid`；
- **插入（已实现，#85）**：`INSERT INTO t (给出的列) VALUES (?1, ?2, ...)`，只绑给出的列，
  一列都不给时用 `INSERT INTO t DEFAULT VALUES`；自增主键用 `last_insert_rowid()` 定位后
  **回读**主键列（而不是拿 rowid 冒充键值），复合主键在全部给出时按主键顺序回传；
  整批隐式事务（`BEGIN IMMEDIATE`）/ savepoint（`sqarvo_row_insert`），任一行失败整批回滚
  （`rolled_back = true`）；错误码：唯一冲突 → `table_data.unique_violation`、
  显式 NULL → `table_data.not_null_violation`、省略且无默认值 →
  `table_data.not_null_without_default`、生成列 → `table_data.column_not_insertable`、
  CHECK / 外键 / STRICT 类型不匹配 → `config.invalid`（v0.2.2 未登记类型专用码）；
- `list_children_filtered` 与 DDL 仍走默认实现返回 `capability.unsupported`
  （绝不返回空列表）；插入已由 #85 实现，`supports_row_insertion()` 为 `true`。

### 已知缺口

- 列排序规则只写在建表 SQL 里，`pragma_table_info` 不提供，本版不解析 DDL，
  因此 `collation` 恒为 `None`（见上一节）；
- 可空性按声明的 `NOT NULL` 原样返回。`INTEGER PRIMARY KEY` 即使没写 `NOT NULL`，插入 NULL 时
  SQLite 会自动分配 rowid——本版不按主键反推，避免猜测；
- 虚拟表（FTS 等）的 shadow 表也会作为普通表出现在列表里，这是 `sqlite_master` 的真实内容；
- 没有对象级权限概念；文件级权限在打开连接时由 `sqlite.open_failed` / `io.error` 分类；
- 有未读完的结果游标时元数据请求返回 `sqlite.query_active`（`FailedPrecondition`），
  避免在游标持有的语句之外再准备语句；
- DDL 查看计划 v0.2.2；本版不把 `sqlite_master.sql` 当作「数据库返回的原始 DDL」交付。

## 能力描述符（现状）

| 能力 | 值 | 说明 |
|---|---|---|
| `transactions` | `true` | 见「事务」 |
| `query_cancellation` | `true` | `InterruptHandle`，幂等 |
| `streaming_results` | `true` | 驱动侧分批 + 游标 |
| `multiple_statements` | `false` | 发送前拒绝 |
| `catalogs` / `schemas` | `false` | SQLite 没有这两个层级 |
| `row_editing` | `true` | `table_data()` 提供浏览、行编辑（update / delete）与插入（#85），见 ADR 0017 / 0019 |
| `parameterized_statements` | `true` | `execute_parameterized`：值经 `rusqlite` 绑定，不拼进 SQL 文本 |
| `ddl_generation` | `false` | 计划 v0.2.2 |
| `server_side_pagination` | `false` | 游标由驱动持有 |
