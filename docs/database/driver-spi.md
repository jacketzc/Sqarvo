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

pub trait MetadataProvider: Send + Sync {
    async fn list_children(&self, parent: Option<ObjectRef>, page: &PageRequest)
        -> Result<Page<ObjectNode>>;
    async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnMeta>>;
    async fn ddl(&self, object: &ObjectRef) -> Result<Ddl>;
}
```

- 元数据能力独立于 `DriverSession`，不支持时 `metadata()` 返回 `None`；
- MariaDB / MySQL 的 database 映射为 `catalog`，没有独立 `schema`；
- PostgreSQL 有 database + schema；
- 不要为了统一而丢弃层级，也不要伪造不存在的层级；
- `PageRequest` / `Page` 与会话无关，定义在 `driver-api`，分页上限 500。

## 值模型

见 `driver-api/src/value.rs`：

- `Value::Null`、`Value::Text("")`、`Value::Int(0)` 必须可区分；
- `Decimal` 用「未缩放整数 + 标度」表示；
- 时间区分 `Date`、`Time`、`DateTime`（无时区）、`Timestamp`（带偏移）；
- 数据库特有类型使用 `Value::Unsupported { type_name, text }`；
- 行是 `Vec<Cell>`，`Cell { value, truncated }` 明确标记大字段截断（ADR 0007）；
- 默认单格上限 `DEFAULT_MAX_CELL_BYTES = 64 KiB`，文本必须按 UTF-8 字符边界截断
  （`truncate_text` 提供实现）。

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
    async fn primary_key(&self, object: &ObjectRef) -> Result<PrimaryKey>;
    async fn browse(&self, object: &ObjectRef, page: &PageRequest) -> Result<TablePage>;
    async fn apply_row_changes(&self, request: RowChangeRequest) -> Result<RowChangeReport>;
}
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
- **批次钳制**：`RowChangeRequest::effective_changes()` 截到 500 行，
  `deferred_changes()` 告诉调用方还剩多少行，不得静默丢弃；
- **能力如实声明**：`table_data()` 为 `None` 表示完全不支持浏览；
  `supports_row_edits()` 为 `false` 表示只读。`Capabilities` 未加字段（新增字段会破坏两个
  驱动现有的穷举字面量），因此 `ListDrivers` 暂不展示这两项能力。


## 新增驱动清单

1. 新建 `backend/crates/drivers/<db>/` crate，`Cargo.toml` 继承 workspace 设置；
2. 实现 `Driver` / `DriverSession`（按需实现 `MetadataProvider` 与事务方法），
   并在 `DriverDescriptor` 中如实填写版本范围与能力；
3. 在 `DriverRegistry::with_builtin_drivers()` 注册；
4. 使用 `sqarvo-driver-testkit` 做符合性检查：除 `smoke`（执行、分批、游标不变量）外，
   还需通过 5 项不变量检查——取消幂等、`close` 幂等与关闭后拒绝执行、多语句拒绝、
   单元格截断、分批无丢失；检查本身与方言无关，SQL 由驱动的集成测试按本驱动语法提供；
5. 编写集成测试：连接、类型往返（含 NULL/Decimal/UUID/JSON/数组）、取消、错误分类、资源清理；
6. 更新本文件与 README 的支持矩阵；
7. 在 Linux 上至少跑通单元测试与 SQLite/容器化集成测试。

## 尚未实现

- PostgreSQL 驱动（连接请求返回 `driver.not_found`）；
- MariaDB 驱动的显式事务与元数据（接口已定义，M4 接线；能力描述符当前为 `false`）；
- 表数据浏览与行编辑的驱动实现：SPI 与 proto 已冻结（ADR 0017），
  `table_data()` 目前全部返回 `None`，`supports_row_edits()` 为 `false`，
  请求时返回 `capability.unsupported`；
- 参数化执行（`execute_parameterized`）的驱动实现：SPI 已冻结，默认返回
  `capability.unsupported`；对外仍无参数化执行的 RPC（留给 S7）；
- MariaDB 驱动的单次执行超时（请求时明确返回 `capability.unsupported`）；
- 驱动级元数据遍历的具体实现（接口已定义）；
- 事务在 gRPC 层的接线（接口已定义，M4 完成）；
- 会话租约与断线重连。

