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

## 元数据（v0.2.1 最小子集）

实现：`backend/crates/drivers/sqlite/src/metadata.rs`。

| 请求 | 数据来源 | 说明 |
|---|---|---|
| `list_children(None, page)` | `sqlite_master` | `type IN ('table','view')` 且排除 `sqlite_*` 内部表，`ORDER BY name`；`name` / `type` 原样映射到节点 |
| `columns(object)` | `pragma_table_info(name)` | 声明类型（`type`）、可空（`notnull`）、列序号（`cid`） |
| `primary_key(object)` | `pragma_table_info(name).pk` | `pk > 0` 的行按 `pk` 排序；`pk` 就是**主键内顺序**（1 开始），不是表内列序号 |
| `ddl(object)` | — | 显式 `capability.unsupported`，文案写明「v0.2.2 提供」 |

层级范围：SQLite 没有 catalog / schema 层级，根节点（`parent = None`）直接返回当前数据库文件的
表与视图。带 `catalog` 或 `schema` 的引用、以及 `Catalog` / `Schema` / `Sequence` 等上层或更深层级
一律返回 `capability.unsupported`，**不返回空列表**冒充「没有对象」。

对象名解析按 SQLite 语义大小写不敏感（`COLLATE NOCASE`），`pragma_table_info` 使用同一个名字；
返回节点的名字取自 `sqlite_master` 中的原始大小写。`pragma_table_info` 通过**绑定参数**调用
（`pragma_table_info(?1)`），不拼接标识符。

无主键表返回空列表（浏览仍可用但置只读 `NoPrimaryKey`/`Unstable`，编辑被拒绝并返回
`table_data.no_primary_key`，见 #46/#47）；视图同样没有主键，返回空列表而不是报错。
表/视图不存在返回 `sqlite.object_not_found`（`NotFound`），不会返回空列。

### 分页 token 与失效条件

与 MariaDB 驱动共用 `sqarvo-metadata::paginate_by_key`，token 为
`v1:<offset>:<16 位十六进制指纹>`：

- 指纹是有序对象名集合的 FNV-1a 64 位摘要，翻页时重新校验；
- token 结构非法、版本未知、偏移量越界 → `config.invalid`（`INVALID_ARGUMENT`），绝不回退第一页；
- 翻页期间对象名集合变化（新建/删除/改名）→ `metadata.snapshot_changed`
  （`FAILED_PRECONDITION`），应重新从第一页读取；
- 同名对象的 `DROP` + `CREATE`（名字集合不变）不会被指纹区分。

一次请求会读取当前库的全部对象名再在内存切页（每页上限 500）；对象极多时应改为服务端游标（待定）。

### 已知缺口

- `proto.common.v1.ColumnMeta` 没有默认值、字符集、排序规则字段（契约已冻结）。SQLite 的
  `pragma_table_info.dflt_value` 能读到默认值，但传不到前端；补齐需要改 proto，见 #45 的「待定」；
- SQLite 没有列级字符集；列排序规则只写在建表 SQL 里，`pragma_table_info` 不提供；
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
| `ddl_generation` | `false` | 计划 v0.2.2 |
| `server_side_pagination` | `false` | 游标由驱动持有 |
