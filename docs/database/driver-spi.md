# Driver SPI

驱动 SPI 位于 `backend/crates/driver-api`。它是通用业务层与数据库实现之间的唯一契约。

## 设计原则

1. **与具体驱动库解耦**：`sqlx`、`mysql_async`、`tokio-postgres` 等库的类型不得出现在公共接口；
2. **内置 crate，不做动态库 ABI**：第一阶段所有驱动编译进后端；
3. **容器随数据库 crate 独立**：每个驱动一个 crate，自带集成测试；
4. **显式能力**：不支持的能力必须声明为 `false` 并返回 `capability.unsupported`，
   禁止「先试试看」或静默降级；
5. **不丢失精度**：NULL、Decimal、带时区时间戳、无符号整数、二进制、数组都必须可表达。

## 核心类型

```rust
pub trait Driver: Send + Sync {
    fn descriptor(&self) -> &DriverDescriptor;
    async fn test_connection(
        &self,
        params: &ConnectionParams,
        secret: Option<&Secret>, // 已解析的密码，只在此调用栈上存在
    ) -> Result<ConnectionTestReport>;
    async fn connect(
        &self,
        params: &ConnectionParams,
        secret: Option<&Secret>,
    ) -> Result<Box<dyn DriverSession>>;
}

pub trait DriverSession: Send + Sync {
    fn session_id(&self) -> SessionId;

    async fn execute(&self, request: ExecuteRequest) -> Result<ExecutionStart>;
    async fn fetch(
        &self,
        query_id: &QueryId,
        statement_index: u32,
        cursor: Option<&str>,
        max_rows: u32,
    ) -> Result<ResultBatch>;
    async fn cancel(&self, query_id: &QueryId) -> Result<()>;
    async fn close(&self) -> Result<()>;

    fn metadata(&self) -> Option<&dyn MetadataProvider> { None }
    async fn begin_transaction(&self, options: TransactionOptions) -> Result<TransactionId>;
    async fn commit_transaction(&self, id: &TransactionId) -> Result<()>;
    async fn rollback_transaction(&self, id: &TransactionId) -> Result<()>;
}
```

`ExecutionStart` = 语句摘要（`StatementSummary`，含列元数据、影响行数、是否有结果集）
  首个结果集的首批数据。后续批次按 `statement_index` 定位，游标由驱动持有。

约定：

- 同一会话的 `execute` / `fetch` 由上层串行化（`SessionHandle` 执行锁），驱动可假设非并发；
- `cancel` 幂等，**不经过**执行锁，必须能在执行进行中从其它任务调用；
- 阻塞型驱动必须使用阻塞任务池（如 `spawn_blocking`），不得阻塞 Tokio worker；
- 会话关闭前必须释放游标、连接与临时资源；
- 未实现的能力使用默认实现返回 `capability.unsupported`，且能力描述符必须为 `false`。


## 连接参数

```rust
pub struct ConnectionParams {
    pub driver_id: DriverId,
    pub host: String,
    pub port: u16,
    pub database: Option<String>,
    pub username: Option<String>,
    pub credential_ref: Option<String>, // 只是引用，不是密码
    pub tls: TlsMode,
    pub tls_server_name: Option<String>,
    pub properties: BTreeMap<String, String>,
}
```

**禁止**在 `ConnectionParams` 中增加 `password` 字段。凭据由 `sqarvo-local-store` 的
`CredentialStore` 解析成 `Secret` 后，作为 `Driver::connect` / `test_connection` 的
`Option<&Secret>` 参数传入（ADR 0013）；解析点由 `SessionManager` 负责（ADR 0015）；
驱动只消费、不保存，且不得进入日志或错误信息。

## 元数据

```rust
pub struct QualifiedName { catalog: Option<String>, schema: Option<String>, name: String }
pub enum ObjectKind { Catalog, Schema, Table, View, MaterializedView, Column, Index,
                      Sequence, Function, Procedure, Trigger, Type, Other(String) }
pub struct ObjectRef { session_id: SessionId, qualified_name: QualifiedName, kind: ObjectKind }

pub struct ObjectFilter { name: Option<NameMatch>, kinds: Vec<ObjectKind> }   // v0.2.2
pub enum NameMatch { Contains(String), Exact(String) }                       // v0.2.2

pub trait MetadataProvider: Send + Sync {
    async fn list_children(&self, parent: Option<ObjectRef>, page: &PageRequest)
        -> Result<Page<ObjectNode>>;
    async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnMeta>>;
    async fn ddl(&self, object: &ObjectRef) -> Result<Ddl>;
    // 主键读取（ADR 0017 的 `GetPrimaryKey`）；默认实现返回 capability.unsupported。
    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey>;
    // 名称/种类过滤（v0.2.2，ADR 0019）；默认实现返回 capability.unsupported。
    async fn list_children_filtered(&self, parent: Option<ObjectRef>,
        filter: &ObjectFilter, page: &PageRequest) -> Result<Page<ObjectNode>>;
}
```

- 元数据能力独立于 `DriverSession`，不支持时 `metadata()` 返回 `None`；
- MariaDB / MySQL 的 database 映射为 `catalog`，没有独立 `schema`；
- PostgreSQL 有 database + schema；
- 不要为了统一而丢弃层级，也不要伪造不存在的层级；
- `PageRequest` / `Page` 与会话无关，定义在 `driver-api`，分页上限 500；
- `primary_key` 与 `TableDataProvider::primary_key` 读同一份主键定义，两者必须一致；
  无主键返回空 `PrimaryKey`，不得返回 rowid / ctid 之类的近似主键。

### 对象过滤（v0.2.2，ADR 0019）

`ListNodesRequest.filter` 的 SPI 形态是 `ObjectFilter`（名称条件 + 种类集合，交集语义）。
`list_children_filtered` 的**默认实现返回 `capability.unsupported`**，不会退化成
「先 `list_children` 再内存过滤」：

- 内存过滤会把整个子树读进内存（冻结口径明确否决），大库直接 OOM；
- 更糟的是它「看起来实现了搜索」：调用方拿到的结果没有经过数据库的排序规则、转义规则，
  与将来真正下推时的行为不同，前端无法预期。

实现要求：

| 条件 | 下推方式 | 注意 |
|---|---|---|
| `name.contains` | SQLite `name LIKE '%' || ? || '%' ESCAPE '\'`；MariaDB `TABLE_NAME LIKE CONCAT('%', ?, '%')` | `%` / `_` / 转义符本身必须转义成字面量 |
| `name.exact` | `name = ?` | 与数据库 `=` 一致（含尾空格语义） |
| `kinds` | `type IN (...)` / `TABLE_TYPE IN (...)` | 空集合 = 不按种类过滤；`UNSPECIFIED` 非法 |

- 大小写敏感性跟随数据库，驱动**不得**自行 `LOWER()` 归一化（那会让 SQLite 与 MariaDB 的
  行为被强行拉齐，代价是全表扫描与索引失效）；
- 过滤条件的规范化字符串（`ObjectFilter::fingerprint_input()`）必须折进分页 token 的指纹，
  条件变化 → 旧 token 以 `config.invalid` 失效；
- 非法条件由 `ObjectFilter::validate()` 在进入驱动前拒绝（`metadata.filter_invalid`）。

### DDL 的来源（v0.2.2，ADR 0019）

```rust
pub enum DdlSource { Database, Synthesized }
pub struct Ddl {
    pub text: String,
    pub exact: bool,                       // ≡ source == DdlSource::Database
    pub source: DdlSource,
    pub source_detail: Option<String>,     // 原文取自哪里 / 拼装缺什么（两种情况都必须非空）
    pub generator_version: Option<String>, // 仅 Synthesized 有意义，且必须非空
}
```

- `Ddl::from_database(text, "sqlite_master.sql")` / `Ddl::synthesized(text, detail, version)`；
- `Ddl::is_consistent()` 断言 `exact` 与 `source` 不打架、拼装路径有说明与版本；
- rpc-server 在返回前检查，不自洽报 `internal.error`（不能把拼装结果当原文）；

### 分页 token（v0.2.1 起）

对象列表用 `sqarvo-metadata::paginate_by_key`：token 是**不透明字符串**
`v1:<offset>:<16 位十六进制指纹>`，指纹为有序对象名集合的 FNV-1a 64 位摘要。

- 调用方只能原样回传；结构非法、版本未知、偏移越界 → `config.invalid`，
  **不得**静默回退到第一页；
- 对象名集合在翻页期间变化 → `metadata.snapshot_changed`（`failed_precondition`）；
- 同名对象整体替换（名字集合不变）不会被指纹区分，文档必须写明这一边界。

## 元数据实现现状（v0.2.2，#82）

两个内置驱动都实现了完整层级与跨库浏览：`ListNodes`（根 / Catalog / 表 / 视图 逐层展开）、
`GetColumns`、`GetPrimaryKey`。

| 驱动 | 目录层（Catalog） | 表内层 | 列 | 主键 | DDL |
|---|---|---|---|---|---|
| SQLite | `PRAGMA database_list`（`main` / `temp` / `ATTACH`） | 列 / 索引 / 触发器（`pragma_table_info` + `sqlite_master`） | `pragma_table_info` | `pragma_table_info.pk` | `sqlite_master.sql` 原文（表/视图/索引/触发器，#83） |
| MariaDB | `information_schema.SCHEMATA` | 列 / 索引 / 触发器（`COLUMNS` / `STATISTICS` / `TRIGGERS`） | `information_schema.COLUMNS` | `information_schema.KEY_COLUMN_USAGE` | `SHOW CREATE TABLE/VIEW/SEQUENCE` 原文（索引不支持，#83） |

两个驱动共同遵守的约定（细节见驱动文档）：

- **子节点顺序**：先按种类（`ObjectKind` 声明顺序）、再按限定名。v0.2.1 的「只按名字排序」
  已作废，客户端不得假设全局字母序；
- **分页键**：`sqarvo-metadata::paginate_children`，键含种类与父节点作用域（列与索引可以同名；
  不同库可以有同名表），token 仍是不透明 `v1:<offset>:<指纹>`；
- **子节点填满 catalog**：节点引用可以原样回传给 `GetColumns` / `GetPrimaryKey` / `GetDdl`；
- **不存在的层级不返回空列表**：SQLite / MariaDB 都没有 schema 层（`capability.unsupported`），
  SQLite 没有序列；种类用错（对列展开、把视图当表）是
  `metadata.object_kind_unsupported`（`INVALID_ARGUMENT`）；
- **过滤下推**（`list_children_filtered`）：默认实现仍返回 `capability.unsupported`
  （绝不在 SPI 层退化成内存过滤）；两个内置驱动已各自下推（#84）——SQLite 用
  `LIKE ... ESCAPE '\\'`，MariaDB 用 `information_schema` 的 `LIKE CONCAT(...)`。
  过滤结果的分页 token 是 `v2:<offset>:<集合指纹>:<过滤标签>`，换条件即 `config.invalid`。
- **DDL**（`MetadataProvider::ddl`，#83）：两驱动都只交付数据库原文
  （`Ddl::from_database`，`exact = true`）；本版**没有**拼装路径，`Ddl::synthesized`
  只有契约层用例覆盖。拿不到原文时（SQLite 隐式索引、MariaDB 索引/触发器）明确报错，
  不返回空串、不伪造文本。DDL 读取经 `SessionHandle::ddl` 在会话执行锁内执行。

## 事务实现现状（v0.2.1）

两个内置驱动都通过会话自己的连接实现显式事务，`TransactionService` 已接线（不再返回
`UNIMPLEMENTED`）。共同不变量由 `sqarvo-driver-testkit` 覆盖：原子性、一个会话最多一个事务、
重复提交/回滚幂等、有打开事务时未知事务 ID 必须 `NotFound`。

| 维度 | SQLite | MariaDB |
|---|---|---|
| 开始语句 | `BEGIN IMMEDIATE`（读写）/ `BEGIN DEFERRED`（只读） | `START TRANSACTION [READ ONLY]` |
| 只读事务 | 连接级 `PRAGMA query_only` 由数据库拒绝写入 | 服务端拒绝（1792 → `mariadb.read_only_transaction`） |
| 隔离级别 | 只接受 `Serializable`，其余 `capability.unsupported` | 四档逐档下发；`Snapshot` 拒绝 |
| 隐式提交 | DDL 在事务内合法、可回滚 | DDL / `TRUNCATE` 会先提交当前事务，驱动**发送前**拒绝（`mariadb.implicit_commit_rejected`） |
| 失败态 | 中断 / 磁盘满 / IO 错误后 SQLite 自动回滚，驱动切失败态 | 死锁（1213）后服务端回滚整个事务，驱动切失败态 |
| 断连 | 由连接错误分类表达 | `mariadb.transaction_lost`（`Unavailable`） |
| 行编辑批内原子性 | savepoint | savepoint |

对外错误语义：重复 `Begin` → `transaction.already_active`；失败态提交 → `transaction.failed`；
行变更指向已结束的事务 → `transaction.not_found`，绝不退化成自动提交；重复提交/回滚幂等
（ADR 0017 §3）。逐条细节见 [SQLite](./sqlite.md#事务) 与 [MariaDB](./mariadb.md#事务)
（后者含「与 SQLite 的差异」表）。

## 值模型

见 `driver-api/src/value.rs`：

- `Value::Null`、`Value::Text("")`、`Value::Int(0)` 必须可区分；
- `Decimal` 用「未缩放整数 + 标度」表示；
- 时间区分 `Date`、`Time`、`DateTime`（无时区）、`Timestamp`（带偏移）；
- 数据库特有类型使用 `Value::Unsupported { type_name, text }`；
- 行是 `Vec<Cell>`，`Cell { value, truncated }` 明确标记大字段截断（ADR 0007）；
- 默认单格上限 `DEFAULT_MAX_CELL_BYTES = 64 KiB`，文本必须按 UTF-8 字符边界截断
  （`truncate_text` 提供实现）；
- 列元数据的默认值是四态 `ColumnDefault`：`Unspecified`（未知）、`NoDefault`（明确没有）、
  `Null`（默认 NULL）、`Expression(未求值 SQL 文本)`。`character_set` / `collation` 为
  `Option<String>`，`None` 表示「不适用或未知」（SQLite 没有列级字符集），不得用空串顶替。

MariaDB 驱动的完整类型映射表、`Unsupported` 清单、TIMESTAMP 会话时区处理与取消语义
见 [mariadb.md](./mariadb.md)。


## 表数据与参数化执行（ADR 0017）

```rust
pub trait DriverSession: Send + Sync {
    // …既有方法不变…

    fn table_data(&self) -> Option<&dyn TableDataProvider> { None }
    fn supports_parameterized_statements(&self) -> bool { false }
    async fn execute_parameterized(&self, request: ParameterizedExecuteRequest)
        -> Result<ExecutionStart>; // 默认 capability.unsupported
}

pub struct ParameterizedExecuteRequest {
    pub query_id: QueryId,
    pub sql: String,       // 含占位符，方言由驱动决定
    pub params: Vec<Value>, // 顺序与占位符一致；不得含 Value::Unsupported
    pub max_rows: Option<u32>,
    pub timeout: Option<Duration>,
}

pub trait TableDataProvider: Send + Sync {
    fn supports_row_edits(&self) -> bool { false }
    fn supports_row_change_cancellation(&self) -> bool { false }   // #70
    fn supports_row_insertion(&self) -> bool { false }             // #81
    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey>;
    async fn browse(&self, object: &ObjectRef, page: &PageRequest) -> Result<TablePage>;
    async fn apply_row_changes(
        &self,
        request: RowChangeRequest,
        cancel: &RowChangeCancel,   // #70：行边界协作式取消令牌
    ) -> Result<RowChangeReport>;
    // 插入新行（#81）：默认 capability.unsupported，既有驱动零改动。
    async fn insert_rows(&self, request: InsertRowsRequest) -> Result<InsertReport>;
}

// 插入行的 SPI 类型（#81）
pub struct InsertRow { pub values: Vec<ColumnChange> }         // 省略的列 = 交给数据库默认值
pub struct InsertRowsRequest { pub object: ObjectRef, pub rows: Vec<InsertRow> }
pub struct InsertRowResult {                                   // 逐行结果
    pub index: usize,
    pub outcome: InsertOutcome,          // Inserted{rows_affected} | Failed{code}
    pub key: Option<RowKey>,             // 插入后的完整主键值
    pub message: Option<String>,
}
pub struct InsertReport { pub results: Vec<InsertRowResult>, pub deferred: usize, pub rolled_back: bool }
```

约定：

- **浏览按主键升序 + 不透明游标**：`page.page_token` 只能由上一页的
  `TablePage::next_page_token` 原样传入；无法解析时返回 `table_data.cursor_invalid`，
  **不得**回退到第一页，也不得用 `OFFSET` 重查（ADR 0005 / 0017）；
- **无主键表**：`primary_key` 返回空的 `PrimaryKey`；`browse` 仍可用，但必须置
  `read_only = true`、`read_only_reason = NoPrimaryKey`、`ordering = Unstable`；
  `apply_row_changes` 必须拒绝（`table_data.no_primary_key`）；不支持 rowid / ctid 近似定位；
- **行变更只用值**：`RowKey` 给出完整主键值（顺序同 `PrimaryKey::columns`），
  `ColumnChange` 给出列与新值；驱动内部用 prepared statement 绑定参数执行，
  通用层不拼接 SQL；
- **影响行数核对**：`expected_rows_affected == 0` 解释为 1；不符时该行返回
  `ChangeOutcome::Conflict`，目标行不存在时返回 `ChangeOutcome::RowNotFound`；
- **影响行数上报**（ADR 0017 补记）：`RowChangeResult::rows_affected()` 一律是驱动 DML 的
  真实影响行数，**不因 outcome 而清零**——`Conflict` 取 `actual`（主键不唯一时可能 `> 1`），
  `RowNotFound` 恒为 0。`RowChangeReport::rows_affected()` = Σ 逐行值，因此**包含**冲突行
  （逐行冲突不回滚已执行的语句，这些行确实被改动了）。驱动实现只把
  `changes()` / `affected_rows()` 交给 `classify_rows_affected`，不要在映射层二次加工或抹零；
- **批次钳制**：`RowChangeRequest::effective_changes()` 截到 500 行，
  `deferred_changes()` 告诉调用方还剩多少行，不得静默丢弃；
- **批次可取消**（#70，ADR 0018）：`apply_row_changes` 必须在**每条变更之前**读一次
  `RowChangeCancel::is_cancelled()`；命中后不再执行后续变更，并**回滚本批已写入的行**
  （自动提交 → 整批 `ROLLBACK`；事务模式 → `ROLLBACK TO SAVEPOINT`，外层事务保持可用），
  返回 `RowChangeReport { cancelled: true, executed_changes: 已执行条数, results: [] }`。
  取消只在行边界生效，不得打断正在执行的 DML（那会把事务撕掉）。声称支持取消的驱动必须让
  `supports_row_change_cancellation()` 返回 `true`；不返回 `true` 时服务端不会把批次登记为可取消，
  `CancelRowChanges` 明确报 `capability.unsupported`，绝不静默成功。两个内置驱动都支持；
- **能力如实声明**：`table_data()` 为 `None` 表示完全不支持浏览；
  `supports_row_edits()` 为 `false` 表示只读。
- **能力位与实现必须一致**：`Capabilities::row_editing` 对应
  `table_data().is_some() && supports_row_edits()`；`Capabilities::parameterized_statements`
  对应 `DriverSession::supports_parameterized_statements()` 为 `true` 且
  `execute_parameterized` 有实现。两个内置驱动（SQLite / MariaDB）都声明为 `true`，
  `SystemService.ListDrivers` 直接展示 `Capabilities::names()`，因此前端能一次拿到
  「这个驱动能不能编辑、能不能绑定参数」，不必逐个对象试。
  未实现的驱动（测试替身、最小驱动）由 `Capabilities::default()` 保持 `false`。
- **档位类能力只允许向更严退化，且必须显式声明**（#107，ADR 0020）：
  `Capabilities::tls_verify_ca_chain_only` 表示「支持只校链、不校名」，默认 `false`；MariaDB
  因上游限制把它归一化为 `verify-full`，故为 `false`（SQLite 无 TLS 档位，不涉及）。驱动实现
  不了某一档时**只能收紧**（`verify-ca` → `verify-full`），**不得放松**（校验档 → `required` /
  `preferred`），并且必须同时体现在能力位与驱动文档里——静默退化不可接受。前端按能力位标注
  档位含义，不按驱动名特判；该位由 `Capabilities::names()` 自动出现在 `ListDrivers` /
  `ConnectResponse` 的 `repeated string capabilities` 中。


### 插入新行（#81，ADR 0019）

- **默认实现即拒绝**：`supports_row_insertion()` 默认 `false`、`insert_rows()` 默认返回
  `capability.unsupported`。既有驱动一行不改也能编译，但拿不到「假成功」；
- **省略 ≠ NULL**：只把给出的列放进 `INSERT` 的列清单，未给出的列交给数据库默认值；
  代码里不要「补默认值」——`AUTO_INCREMENT`、`now()`、`'0'` 只有数据库能求值；
- **完整主键回传**：成功行必须填 `InsertRowResult::key`（自增取
  `last_insert_rowid` / `LAST_INSERT_ID`，复合主键回传全部列，顺序同 `PrimaryKey`）。
  拿不到完整主键的驱动**不能**声明支持插入；
- **整批成功才提交**：自动提交 → 一个隐式事务覆盖整批；事务模式 → `SAVEPOINT`；
  任一行失败 → 继续执行完其余行（把失败一次报全），然后整批回滚，`rolled_back = true`、
  净影响 0；`InsertReport::rows_affected()` 已经按这个规则返回 0，驱动不要自己抹；
- **逐行失败用 `InsertOutcome::Failed{code}`**，不要用 `Err` 打断整批：`Err` 只用于
  「整批根本无法执行」（无权限、对象不存在、事务不存在）；
- 批次上限 `MAX_ROWS_PER_INSERT_BATCH = 500`（与行变更同值）；风险阈值沿用
  `DEFAULT_RISK_CONFIRMATION_ROWS = 100`，预计影响行数 = 本批行数，**不另立阈值**。

## 新增驱动清单

1. 新建 `backend/crates/drivers/<db>/` crate，`Cargo.toml` 继承 workspace 设置；
2. 实现 `Driver` / `DriverSession`（按需实现 `MetadataProvider` 与事务方法），
   并在 `DriverDescriptor` 中如实填写版本范围与能力；
3. 在 `DriverRegistry::with_builtin_drivers()` 注册；
4. 使用 `sqarvo-driver-testkit` 做符合性检查：除 `smoke`（执行、分批、游标不变量）外，
   还需通过 5 项不变量检查——取消幂等、`close` 幂等与关闭后拒绝执行、多语句拒绝、
   单元格截断、分批无丢失；声明 `capabilities.transactions` 的驱动还必须通过事务不变量
   （提交可见、回滚丢弃、重复提交/回滚幂等、一个会话最多一个事务、语句失败后仍可回滚）。
   testkit 提供两组互补的事务检查：`TransactionFixture`（表数据可见性组，SQLite 接入）与
   `TransactionProbe`（事务状态机组：原子性、一个会话最多一个事务、重复提交/回滚幂等、
   有打开事务时未知 ID 必须 `NotFound`，MariaDB 接入）。
   检查本身与方言无关，SQL 由驱动的集成测试按本驱动语法提供；
5. 编写集成测试：连接、类型往返（含 NULL/Decimal/UUID/JSON/数组）、取消、错误分类、资源清理；
6. 更新本文件与 README 的支持矩阵；
7. 在 Linux 上至少跑通单元测试与 SQLite/容器化集成测试。

## 尚未实现

- PostgreSQL 驱动（连接请求返回 `driver.not_found`）；
- 元数据的 schema 层级、函数/存储过程/类型列表与 DDL：两驱动都没有 schema 层
  （`capability.unsupported`），`FUNCTION` / `PROCEDURE` / `TYPE` 父节点同样显式报错；
  `GetDdl` 已由 #83 实现（表/视图/索引/触发器取原文，见上「元数据实现现状」）。层级展开与
  跨库浏览已于 #82 实现；`ListNodes.filter` 的下推在 #84，驱动未实现前 rpc-server
  对非空过滤明确返回 `UNIMPLEMENTED`，不静默忽略；
- 表数据浏览与行编辑：两驱动已实现（`table_data()` 返回 provider，`supports_row_edits()`
  为 `true`），`Capabilities.row_editing` 已在 `ListDrivers` 中展示；**插入新行**
  （`insert_rows`，契约见 #81 / ADR 0019）已由 #85 在两驱动落地：
  `supports_row_insertion()` 为 `true`，参数化 `INSERT`、自增/默认值、唯一与 NOT NULL
  错误码、整批原子（隐式事务 / savepoint）见各驱动文档；仅**非主键排序**仍未实现；
- 参数化执行的 gRPC RPC：SPI（`execute_parameterized`）两驱动已实现，查询侧的参数化 RPC
  留给 S7；
- MariaDB 驱动的单次执行超时（请求时明确返回 `capability.unsupported`）；
- 服务端元数据分页（目前把当前库对象名一次读入内存后切页）；
- 会话租约与断线重连。

