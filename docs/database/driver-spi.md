# Driver SPI

驱动 SPI 位于 `backend/crates/driver-api`。它是通用业务层与数据库实现之间的唯一契约。

## 设计原则

1. **与具体驱动库解耦**：`sqlx`、`tokio-postgres` 等库的类型不得出现在公共接口；
2. **内置 crate，不做动态库 ABI**：第一阶段所有驱动编译进后端；
3. **容器随数据库 crate 独立**：每个驱动一个 crate，自带集成测试；
4. **显式能力**：不支持的能力必须声明为 `false` 并返回 `capability.unsupported`，
   禁止「先试试看」或静默降级；
5. **不丢失精度**：NULL、Decimal、带时区时间戳、无符号整数、二进制、数组都必须可表达。

## 核心类型

```rust
pub trait Driver: Send + Sync {
    fn descriptor(&self) -> &DriverDescriptor;
    async fn test_connection(&self, params: &ConnectionParams) -> Result<ConnectionTestReport>;
    async fn connect(&self, params: &ConnectionParams) -> Result<Box<dyn DriverSession>>;
}

pub trait DriverSession: Send + Sync {
    fn session_id(&self) -> SessionId;
    async fn execute(&self, request: ExecuteRequest) -> Result<ResultBatch>;
    async fn fetch(&self, query_id: &QueryId, cursor: Option<&str>, max_rows: u32) -> Result<ResultBatch>;
    async fn cancel(&self, query_id: &QueryId) -> Result<()>;
    async fn close(&self) -> Result<()>;
}
```

约定：

- 同一会话上的调用由上层串行化；
- `cancel` 幂等；
- 阻塞型驱动必须使用阻塞任务池（如 `spawn_blocking`），不得阻塞 Tokio worker；
- 会话关闭前必须释放游标、连接与临时资源。

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
`CredentialStore` 解析后直接交给驱动，且不得进入日志或错误信息。

## 元数据

```rust
pub struct QualifiedName { catalog: Option<String>, schema: Option<String>, name: String }
pub enum ObjectKind { Catalog, Schema, Table, View, MaterializedView, Column, Index,
                      Sequence, Function, Procedure, Trigger, Type, Other(String) }
```

- MySQL 的 database 通常映射为 `catalog`，没有独立 `schema`；
- PostgreSQL 有 database + schema；
- 不要为了统一而丢弃层级，也不要伪造不存在的层级。

## 值模型

见 `driver-api/src/value.rs`：

- `Value::Null`、`Value::Text("")`、`Value::Int(0)` 必须可区分；
- `Decimal` 用「未缩放整数 + 标度」表示；
- 时间区分 `Date`、`Time`、`DateTime`（无时区）、`Timestamp`（带偏移）；
- 数据库特有类型使用 `Value::Unsupported { type_name, text }`；
- 大字段由驱动先返回截断前缀并置 `truncated`，或提供游标延迟加载。

## 新增驱动清单

1. 新建 `backend/crates/drivers/<db>/` crate，`Cargo.toml` 继承 workspace 设置；
2. 实现 `Driver` / `DriverSession`，并在 `DriverDescriptor` 中如实填写版本范围与能力；
3. 在 `DriverRegistry::with_builtin_drivers()` 注册；
4. 编写集成测试：连接、类型往返（含 NULL/Decimal/UUID/JSON/数组）、取消、错误分类、资源清理；
5. 更新本文件与 README 的支持矩阵；
6. 在 Linux 上至少跑通单元测试与 SQLite/容器化集成测试。

## 尚未实现

- 任何具体驱动；
- 驱动级元数据遍历（`list_nodes` / `columns` / `ddl`）；
- 事务控制接口（当前 `DriverSession` 没有 begin/commit/rollback）；
- 会话租约与断线重连。
