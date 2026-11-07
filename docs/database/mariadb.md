# MariaDB 驱动

实现：`backend/crates/drivers/mariadb`（`mysql_async` 0.37，MIT/Apache-2.0）。
选型依据见 [ADR 0011](../decisions/0011-mariadb-driver.md)，通用约定见 [Driver SPI](./driver-spi.md)。

- 已验证服务端：`11.4.13-MariaDB`（Linux，容器）；声明支持范围 `10.6`（LTS 下限）– `11.4`。
- 未验证：MySQL 8.x（认证插件、二进制 JSON、默认排序规则均不同）、MariaDB 10.6 以下。
  驱动标识是 `mariadb`，不要把它当作「MySQL 已支持」的依据。
- 每个会话占用两条服务端连接：一条执行语句，一条仅在取消时用于 `KILL QUERY`。

## 值表示：先按列类型，再解析字节

`mysql_async` 用文本协议执行，**所有值都以 `Value::Bytes` 返回**（整数、DECIMAL、DATETIME、
BIT、ENUM、SET 都是），类型只体现在列定义上。因此驱动按「列类型 + 列标志 + 字符集」解析，
绝不按 Rust 值变体猜类型。

两个必须记住的协议陷阱：

1. **`BINARY_FLAG` 不等于二进制数据。** 采用 binary 排序规则的文本列也会置该标志
   （MariaDB 的 `JSON` 列就是 `longtext ... COLLATE utf8mb4_bin`）。判断真正的二进制列用
   `/字符集 == 63/`（binary）。用 `BINARY_FLAG` 会把 JSON/文本整列变成 `Value::Bytes`。
2. **TEXT/BLOB 的档位在协议里不可区分。** MariaDB 把 `tinytext`/`text`/`mediumtext`/`longtext`
   以及对应 BLOB 全部报成协议类型 `MYSQL_TYPE_BLOB`，长度字段还受字符集影响
   （`text` 在 utf8mb4 下列长为 262140）。因此 `original_type_name` 对这类列只到
   `text` / `blob` 粒度；需要精确档位时查 `information_schema.columns`。

## 类型映射表

实测于 MariaDB 11.4.13（`docs/decisions/0011-mariadb-driver.md` 记录了探针结论）。

| MariaDB 类型 | `ColumnType` | `Value` | 说明 |
|---|---|---|---|
| `tinyint` / `smallint` / `mediumint` / `int` / `bigint` | `Integer { bits: 8/16/24/32/64, unsigned }` | `Int(i64)` 或 `UInt(u64)` | 无符号列一律 `UInt`；`BIGINT UNSIGNED` 上限 `18446744073709551615` 不溢出 |
| `tinyint(1)` / `BOOLEAN` | `Integer { bits: 8, unsigned: false }` | `Int(0/1)` | 协议无法把 `BOOLEAN` 与 `tinyint(1)` 区分，不伪造 `Value::Bool` |
| `decimal(M,D)` | `Decimal { precision, scale }` | `Decimal` | 见下节「DECIMAL 精度」；永不经过 `f64` |
| `float` / `double` | `Float { bits: 32/64 }` | `Float(f64)` | 整数值也保持浮点（`2` → `2.0`） |
| `bit(n)` | `Binary` | `Bytes` | 原始字节（`BIT(64)` 为 8 字节），见「BIT」 |
| `date` | `Date` | `Date` | 零日期见「Unsupported」 |
| `datetime` / `datetime(n)` | `DateTime` | `DateTime` | 保留 0–6 位微秒 |
| `timestamp` | `Timestamp { with_time_zone: false }` | `DateTime` | 见「TIMESTAMP 与会话时区」 |
| `time` / `time(n)` | `Time { with_time_zone: false }` | `Time` 或 `Unsupported` | 只支持 `00:00:00`–`23:59:59.999999` |
| `year` | `Integer { bits: 16, unsigned: true }` | `UInt` | 协议恒为无符号（`UNSIGNED_FLAG\|ZEROFILL_FLAG`） |
| `enum(...)` | `Text` | `Text` | `original_type_name = "enum"`；不展开取值域 |
| `set(...)` | `Text` | `Text` | 逗号分隔原文，`original_type_name = "set"` |
| `char` / `varchar`（字符集 ≠ 63） | `Text` | `Text` | binary 排序规则仍是文本 |
| `binary` / `varbinary` | `Binary` | `Bytes` | 字符集 63 |
| `tinytext` … `longtext` | `Text` | `Text` | 档位不可区分，见上文 |
| `tinyblob` … `longblob` | `Binary` | `Bytes` | 字符集 63 |
| `json` | `Text` | `Text` | MariaDB 的别名，见「JSON」 |
| `geometry` | `Binary` | `Bytes` | WKB 原样，不做几何解析 |
| 其它（含 `MYSQL_TYPE_NULL`） | `Unsupported { type_name }` | `Unsupported` | 保留原文类型名 |

`NULL`、`Text("")`、`Int(0)` 是三份不同的值，任何一对都不折叠；零 `DECIMAL` 保留标度
（`0.00` 而不是 `0`）。

### DECIMAL 精度

协议只给列宽与标度（`decimals`）。`DECIMAL(M,D)` 的列宽 = `M + 小数点位(D>0) + 符号位(有符号)`，
据此推导精度并做范围校验（`1..=65` 且不小于标度）；不符预期时 `precision = None`，
不猜一个可能错误的数字。实测：`DECIMAL(65,30)` 列宽 67 → 精度 65；`DECIMAL(20,6)` 列宽 22 →
精度 20；`DECIMAL(10,2) UNSIGNED` 列宽 11 → 精度 10。

## 时间、日期与零日期

- `DATETIME` 与 `TIMESTAMP` 都映射为 `Value::DateTime`（无时区墙上时间）。
- `DATE`/`DATETIME`/`TIME` 的精度（0–6 位小数）完整保留，`TIME(6)` 不丢微秒。
- **TIMESTAMP 与会话时区**：MariaDB 在写入和读取时都按**会话时区**换算 `TIMESTAMP`，`DATETIME`
  不换算。驱动不改变会话时区（默认 `SYSTEM`），也不做隐式转换，只是把服务端返回的墙上时间
  原样交付，并在列类型上声明 `with_time_zone: false`。同一个 `TIMESTAMP` 值在不同会话时区的
  连接里读出的文本不同 —— 需要绝对时间时应由调用方显式 `SET time_zone` 并记录该上下文。
- **零日期**：容器默认 `sql_mode = STRICT_TRANS_TABLES,...`（不含 `NO_ZERO_DATE`），
  `0000-00-00` 与 `0000-00-00 00:00:00` 可以写入。`time::Date` 无法表示 0 年 0 月，
  驱动返回 `Value::Unsupported`，`text` 保留原始字符串，绝不伪造成合法日期。

## JSON 是 longtext 别名

MariaDB 没有独立的 JSON 存储类型：`JSON` 列在 `information_schema.columns` 里
`data_type = longtext`，`SHOW CREATE TABLE` 是 ``longtext ... CHECK (json_valid(`js`))``，
排序规则 `utf8mb4_bin`。所以：

- 列类型是 `Text`（不是 `ColumnType::Json`），值是 `Value::Text`，保留原始格式（不重排、不压缩）；
- 不声明原生 JSON 能力；需要 JSON 语义时由前端/查询层用 `json_valid`、`JSON_*` 函数处理；
- MySQL 8 的 `JSON` 是二进制类型，这条映射**不适用**于 MySQL。

## 文本、二进制与截断

- 文本列按 UTF-8 解码；解码失败（非 utf8mb4 字符集或损坏字节）时返回 `Value::Bytes`，
  **不做 `U+FFFD` 有损替换** —— 数据不能因为字符集不认识就消失。
- 单格上限 `DEFAULT_MAX_CELL_BYTES = 64 KiB`（ADR 0007）。超限时只交付前缀并置
  `Cell::truncated = true`；文本按 UTF-8 字符边界回退（3 字节字符不会被截成半个），
  二进制按字节截断。恰好等于上限不标记截断。
- 截断只影响显示值，不改变列元数据；大字段的按需加载尚未实现（见「未支持清单」）。

## BIT

`BIT(1)`/`BIT(8)`/`BIT(64)` 一律返回 `Value::Bytes` 原始字节（文本协议给的就是字节，
`BIT(1)` 是 `[0x01]`，`BIT(64)` 是 8 字节），列类型 `Binary`，`original_type_name` 带位数
（`bit(8)`）。

不做整数解释的理由：位宽只存在于列定义里，`BIT(1)` 与 `BIT(8)` 的字节含义不同；转成整数会
丢掉前导零与位序信息，也让「本列是位域」这一事实在值上消失。需要数值时由调用方按列宽自行换算。

## Unsupported 清单与理由

| 情形 | 返回 | 理由 |
|---|---|---|
| `TIME` 为负（`-01:00:00`） | `Value::Unsupported { type_name: "time", text }` | MariaDB 允许 `-838:59:59`–`838:59:59`，`time::Time` 只表示 0–24 小时内的时刻；`text` 保留原文（如 `-01:00:00.000000`） |
| `TIME` 超过一天（`838:59:59`） | 同上 | 同上 |
| 零日期（`date` / `datetime` / `timestamp`） | `Value::Unsupported`，`text` = 原文 | 0 年 0 月不是合法日历日期 |
| 非法日期（如 `2026-02-30`，非严格模式可写入） | 同上 | 同上 |
| 无法无损表示的其它协议类型 | `Value::Unsupported`，`text` = 原始文本（可能截断） | 保留原始类型名，前端必须显式展示「未完全支持」 |
| 数值列内容不是数字 | `Value::Bytes` 原始字节 | 比伪造一个数字更诚实 |

`Unsupported` 永远是**值级**标记：列元数据仍然给出真实类型名，不做「静默降级成字符串」。

## 分批、游标与错误语义

- `execute` 交付语句摘要 + 首批行；`max_rows` 缺省 200，钳制到 `1..=5000`。
- 游标 = 已交付行数（十进制字符串）。读取时多读一行判断 `has_more`，多出的行留给下一批，
  不使用 `OFFSET` 重查（ADR 0005）。
- 游标不匹配 → `mariadb.cursor_invalid`（`InvalidArgument`）；查询已结束还在取 →
  `query.not_found`；游标未读完又发 `execute` → `mariadb.query_active`（`FailedPrecondition`）。
- 语句不产生结果集（INSERT/DDL 等）时返回完整摘要与 `rows_affected`，`has_result_set = false`。
- 列可空性只按协议 `NOT_NULL_FLAG` 判断，没有该标志一律按可空处理。

## 取消语义

MySQL 协议没有取消通道：驱动用**另一条控制连接**执行 `KILL QUERY <connection_id>`，
`connection_id` 在连接建立后用 `Conn::id()` 读取并缓存。取消不经过会话执行锁，可以在查询
执行期间从其它任务调用。

| 场景 | 行为 | 结果 |
|---|---|---|
| 从未执行查询时 `cancel` | 空操作 | `Ok` |
| 查询已结束（含已读完、已报错）时 `cancel` | 空操作 | `Ok` |
| 查询执行中（首批未交付）`cancel` | 控制连接 `KILL QUERY` | 2 秒内 `query.cancelled`（`Cancelled`）；被中断连接可继续使用 |
| 重复 `cancel` | `KILL` 的 1094/1317 视为成功 | `Ok` |
| 游标中途（`has_more = true` 的两次 `fetch` 之间）`cancel` | 只记录取消，**不发 `KILL`** | `Ok`；下一次 `fetch` 立即返回 `query.cancelled`，游标释放 |
| 取消后再 `fetch` | 查询已不存在 | `query.not_found` |
| 取消后执行新查询 | 排空未读结果后复用同一连接 | `Ok`（第一条语句可能多花排空时间，见下） |

批次之间服务端并没有正在执行的语句，此时 `KILL QUERY` 只会空转（实测：能让下一批继续正常
返回行），所以只记录取消状态，由下一次 `fetch` 立刻报取消并丢弃游标。

**连接复用与排空**：被取消的游标会丢弃，但连接上仍有未读行。`mysql_async` 在下一条命令前
（`clean_dirty` → `drop_result`）自动排空它们，因此会话无需重连、也不会丢会话状态；代价是下一
条语句要等排空完成（实测 20 万行 × 100 字节约 0.3 秒）。若取消发生在执行中（`KILL QUERY`），
服务端已经中断，排空几乎是即时的。

取消后服务端不残留执行中的语句：集成测试用 `information_schema.PROCESSLIST` 断言
`SELECT SLEEP(30)` 在取消前存在、取消后消失。

## 事务

显式事务在会话自己的那条连接上执行：`START TRANSACTION [READ ONLY]` / `COMMIT` / `ROLLBACK`。
驱动不改会话的 `autocommit`（保持默认 `1`），也不为事务额外开连接。

### 一个会话一个事务

同一个会话同时最多一个打开的事务（ADR 0017 §3）：已有事务时再 `begin_transaction` 返回
`transaction.already_active`（`AlreadyExists`）。回滚后可以立即开启新事务。

事务状态由驱动维护，不靠服务端变量推断：

| 状态 | 含义 | `execute` | `commit` | `rollback` |
|---|---|---|---|---|
| 无 | 没有打开的事务 | 正常（自动提交） | `Ok`（幂等） | `Ok`（幂等） |
| 可用 | 事务打开 | 正常 | 提交，`Ok` | 回滚，`Ok` |
| 失败 | 服务端已回滚该事务（死锁） | `transaction.failed` | `transaction.failed` | `Ok`（空操作） |
| 丢失 | 连接断开，提交结果未知 | `mariadb.transaction_lost` | `mariadb.transaction_lost` | `Ok`（服务端随连接关闭回滚） |

事务未读完的结果游标仍在连接上时，`begin` / `commit` / `rollback` 返回 `mariadb.query_active`
（`FailedPrecondition`）：先读完或 `close_query`，不要与游标交错。

### 隔离级别

| `IsolationLevel` | 下发语句 | 事务内 `@@transaction_isolation` |
|---|---|---|
| `Unspecified` | 恢复连接建立时读到的默认级别 | 服务端默认值（容器实测 `REPEATABLE-READ`） |
| `ReadUncommitted` | `SET SESSION TRANSACTION ISOLATION LEVEL READ UNCOMMITTED` | `READ-UNCOMMITTED` |
| `ReadCommitted` | `… READ COMMITTED` | `READ-COMMITTED` |
| `RepeatableRead` | `… REPEATABLE READ` | `REPEATABLE-READ` |
| `Serializable` | `… SERIALIZABLE` | `SERIALIZABLE` |
| `Snapshot` | 不下发 | 无此级别，`begin_transaction` 返回 `capability.unsupported` |

**为什么用 `SESSION` 作用域**：实测 MariaDB 11.4.13（会话 `autocommit = 1`）下，不带作用域的
`SET TRANSACTION ISOLATION LEVEL READ COMMITTED` 对下一个事务**不生效**——事务内
`@@transaction_isolation` 仍是 `REPEATABLE-READ`，跨会话可见性也仍然是 REPEATABLE READ 的行为。
带 `SESSION` 作用域则两者都正确生效。代价是设置会留在会话上，因此每次开启事务都显式下发目标级别；
`Unspecified` 时恢复连接建立时读到的默认值，不会继承上一次显式设置。

`Snapshot` 不静默降级成 REPEATABLE READ：InnoDB 的可重复读接近快照读，但不是 SQL 标准意义上的
快照隔离（写冲突语义不同），给一个「差不多」的级别会让并发行为与用户预期不一致（AGENTS.md §6.2）。

验证方式：集成用例在同一事务内读 `@@transaction_isolation` 断言上表的取值；另有两个会话的用例
断言 `READ COMMITTED` 能看到对方刚提交的行，而 `REPEATABLE READ` 保持自己的快照。

### 只读事务

`TransactionOptions.read_only = true` 下发 `START TRANSACTION READ ONLY`。写入由服务端拒绝
（错误 1792），映射为 `mariadb.read_only_transaction`（`FailedPrecondition`）。
被拒绝的写入不会以任何方式落库；事务仍可读、可继续执行查询、可回滚。

例外：MariaDB 允许在只读事务里写**临时表**（服务端行为，驱动不额外拦截）。

### 连接级只读不适用

MariaDB 协议里没有「以只读方式打开连接」这一概念，驱动也不提供该开关：

- 连接参数里的 `read_only` 会被 `MariaDbConfig::from_params` 以 `config.invalid` **明确
  拒绝**（`INVALID_ARGUMENT`），不会静默忽略，避免用户以为连接已经只读；
- 服务端也没有连接级只读开关；只读事务是会话级能力（见上一节），要限制账号的写权限
  应使用数据库授权（例如只授予 `SELECT`），这时写入会以服务端权限错误返回；
- 因此 `BrowseTable.read_only_reason = READ_ONLY_REASON_CONNECTION_READ_ONLY` 在 MariaDB 上
  **不会出现**，浏览的只读原因只有「无主键」与「驱动未声明行编辑」两种（#60）。
  前端无需特判驱动：它只显示服务端给出的原因。

### 隐式提交：事务中一律拒绝

MariaDB 在执行 DDL、`TRUNCATE` 和管理命令前会**先提交当前事务**，`ROLLBACK` 之后就什么也回滚
不了。实测（11.4.13）：事务里 `INSERT` 一行 → `CREATE TABLE` → `ROLLBACK`，那一行仍在库里，
新表也留了下来。

驱动在**发送前**识别这类语句并拒绝，返回 `mariadb.implicit_commit_rejected`
（`FailedPrecondition`），文案带上语句类别：

```
事务中不允许执行会隐式提交的语句（TRUNCATE）：服务端会先提交当前事务再执行，
已写入的数据将无法回滚。请先提交或回滚事务
```

语句**不会**发到服务端，因此被拒绝后事务与其中的写入保持原样，可以继续执行或回滚。

拒绝范围（按首个关键字识别，跳过空白、`--` / `#` 行注释与 `/* */` 块注释）：

- DDL：`CREATE` / `ALTER` / `DROP` / `RENAME`；
- `TRUNCATE`；
- 权限与维护命令：`GRANT` / `REVOKE` / `FLUSH` / `RESET` / `ANALYZE` / `OPTIMIZE` / `REPAIR` /
  `CACHE` / `INSTALL` / `UNINSTALL` / `CHANGE` / `START` / `STOP`；
- 锁与事务控制：`LOCK` / `UNLOCK` / `BEGIN` / `COMMIT` / `ROLLBACK`、`SET autocommit`。

例外：`CREATE TEMPORARY TABLE` / `DROP TEMPORARY TABLE` 不隐式提交，放行（临时表本身不可回滚，
但不会把已写入的行提交掉）。`SET NAMES`、`SET time_zone` 等不提交的 `SET` 也放行。

识别只做关键字扫描、不做完整语法解析：宁可对畸形语句给出明确拒绝，也不能让一条 DDL 混进事务
把前面的写入静默提交掉。

### 事务内取消

取消仍走控制连接的 `KILL QUERY`，只杀**当前语句**，事务不受影响：

- 取消后 `query.cancelled` 返回给该语句，事务状态仍是「可用」；
- 事务里已写入的行仍然可见，可以继续执行语句并提交；
- 取消不会把事务变成失败态（`query.cancelled` 不是死锁，也不是连接错误）。

集成用例：`BEGIN` → `INSERT` → 取消 `SELECT SLEEP(30)` → 仍能读到自己的行 → 再写一行 →
`COMMIT` → 另一个会话看到两行。

### 行变更批次的取消（#70）

`ApplyRowChanges` 的取消**不用** `KILL QUERY`（ADR 0018）：它会把语句打断在半路，事务是否存活
取决于服务端状态，做不到「取消后外层事务仍可用」。改为行边界协作：

- 驱动在 `execute_plan` 的每行之前读一次 `RowChangeCancel`；命中后不再执行后续变更；
- 自动提交模式：`ROLLBACK`（驱动自开的整批事务），库里 0 行变化；
- 事务模式：`ROLLBACK TO SAVEPOINT sqarvo_row_changes` + `RELEASE SAVEPOINT`，外层事务继续可用；
- 返回 `cancelled = true`、`executed_changes = 已执行条数`、`results = []`。

端到端证据（`crates/rpc-server/tests/grpc_mariadb_row_cancel.rs`，需要 `SQARVO_MARIADB_URL`）：
500 行批次带 `BEFORE UPDATE ... DO SLEEP(0.003)` 触发器，实测每行约 4.8 ms；在批次进行中取消，
「执行 16 行后取消、库里 0 行改动、取消耗时 1.2 ms」。时序靠服务端 `SLEEP` 而不是本机调度，
因此对机器速度不敏感。

### 失败态与断连

| 触发 | 驱动状态 | `commit` | `rollback` | 后续 `execute` |
|---|---|---|---|---|
| 死锁（1213，InnoDB 回滚整个事务） | 失败 | `transaction.failed` | `Ok`（空操作） | `transaction.failed` |
| 连接被 `KILL` / 网络断开 / 提交超时 | 丢失 | `mariadb.transaction_lost`（`Unavailable`，可重试） | `Ok` | `mariadb.transaction_lost` |
| 用户主动 `rollback` | 无 | `Ok`（幂等） | `Ok`（幂等） | 正常 |

- **断连必须区别于主动回滚**：提交时连接已断，驱动无法确认事务是否提交过，因此 `commit`
  返回 `mariadb.transaction_lost`（`Unavailable`）而不是成功；原始连接错误保留在 `source` 里供
  日志诊断。`rollback` 是收尾操作，服务端会随连接关闭回滚，因此返回 `Ok`，不再制造第二个
  无法处置的错误。
- **死锁**：MariaDB 已把整个事务回滚掉，驱动把状态置为失败态。把服务端的「已回滚」当成「还在
  事务里」是这类实现最容易出的错——用户会以为提交成功或继续往里写。
- **其它语句错误不改变事务状态**：语法错误、唯一键冲突、锁等待超时（1205）只让当前语句失败，
  事务仍可提交。把这些错误当成失败态会白白丢掉可提交的工作。

### 幂等

- 对已结束或不存在的事务重复 `commit` / `rollback` → `Ok`（ADR 0017 §3）；
- 事务结束后又开启了新事务时，用旧 ID 重试仍返回 `Ok`，且不会提交/回滚当前的新事务
  （驱动记住最近结束的那个 ID，只对「已结束」的事务幂等，对未知 ID 仍然报错）；
- 会话里有事务时，用一个**未知**的事务 ID 提交/回滚 → `transaction.not_found`（`NotFound`），
  绝不退化成自动提交；
- 失败态 `commit` → `transaction.failed`（`FailedPrecondition`）。

### 与 SQLite 的差异

`sqarvo-driver-testkit` 的事务不变量（原子性、最多一个事务、重复提交/回滚幂等、未知事务 ID
拒绝）由两个驱动共用；下面这些只在 MariaDB 侧存在：

| 差异 | MariaDB | SQLite |
|---|---|---|
| 隐式提交 | DDL / `TRUNCATE` 会提交当前事务，驱动发送前拒绝 | DDL 在事务内合法、可回滚 |
| 只读事务 | `START TRANSACTION READ ONLY`，服务端拒绝写入（1792） | 只读由连接打开方式决定 |
| 隔离级别 | 四档逐档下发，`@@transaction_isolation` 可验证 | 无分级语义，`IsolationLevel` 只是建议 |
| 断连 | 事务丢失 → `mariadb.transaction_lost`（`Unavailable`） | 由连接错误分类表达 |
| 死锁 | 服务端回滚整个事务 → 失败态，只能回滚 | 无死锁检测，表现为 `SQLITE_BUSY` |

## 行编辑：同值更新也计 1 行（`CLIENT_FOUND_ROWS`）

连接固定开启 `CLIENT_FOUND_ROWS`（`src/config.rs` 的 `OptsBuilder::client_found_rows(true)`），
因此 `UPDATE` / `DELETE` 上报的是**匹配**行数而不是实际变化行数，与 SQLite 的
`sqlite3_changes()` 语义一致：

| 情形 | 服务端报告 | 驱动 `ChangeOutcome` |
|---|---|---|
| 把某列更新成它已有的值 | 1 行（匹配 1 行、变化 0 行） | `Applied { rows_affected: 1 }` |
| 主键没有匹配到行 | 0 行 | `RowNotFound` |

去掉这个 flag，MariaDB 会按「实际变化行数」上报，一次无害的重复提交（用户把同一份编辑
提交两次、或多端同时写相同的值）就会得到 0 行并被判成 `ROW_NOT_FOUND`，界面显示
「这行已被删除」——一个看起来像数据损坏的假冲突（ADR 0017「更新/删除必须影响 1 行」）。

锁定这条语义的用例：`backend/crates/drivers/mariadb/tests/integration.rs` 的
`row_edits_count_same_value_update_as_applied`（预读 → 同值更新 → 断言 1 行，再加
「更新不存在的行 → `ROW_NOT_FOUND`」作为对照）。**删掉 `client_found_rows(true)`
这条用例必须变红**，改这一行前先跑它。SQLite 侧对照见
`backend/crates/drivers/sqlite/tests/row_edits.rs` 的 `same_value_update_counts_as_applied`。

## 关闭

- `close()` 幂等；有在途查询时先 `KILL QUERY` 再发送关闭命令，实测 2 秒内返回。
- 关闭后 `execute` / `fetch` / `cancel` / `close_query` 返回 `mariadb.session_closed`
  （`FailedPrecondition`）。
- 关闭时不等待未读结果排空：会话任务丢弃连接，排空与 `COM_QUIT` 由 `mysql_async` 在后台完成，
  避免大结果游标把 `close()` 拖住。
- 控制连接只在取消时建立，`close()` 时一并释放。

## 多语句限制

协议族的 `CLIENT_MULTI_STATEMENTS` 在 `mysql_async` 中恒为开启，服务端会真的执行
`SELECT 1; DROP TABLE t`。能力描述符声明 `multiple_statements = false`，驱动在**发送前**用
引号/注释感知的扫描器拒绝多条语句，返回 `capability.unsupported`；不会「只执行第一条」再报错。
集成测试用 `information_schema.tables` 断言第二条语句没有产生任何副作用。

## 元数据（v0.2.2：完整层级 + 跨库，ADR 0019 / #82）

实现：`backend/crates/drivers/mariadb/src/metadata.rs`，全部读 `information_schema`。

### 层级与数据来源

| 父节点 | 子节点 | 数据来源 |
|---|---|---|
| 空（根） | 当前数据库的表 / 视图 / 序列 | `information_schema.TABLES`（`TABLE_SCHEMA = DATABASE()`） |
| `CATALOG`（名字为空 = 目录层本身） | 本连接可见的库（database） | `information_schema.SCHEMATA` |
| `CATALOG`（名字为库名） | 该库的表 / 视图 / 序列 | `information_schema.TABLES` |
| `TABLE` | 列 / 索引 / 触发器 | `COLUMNS` / `STATISTICS`（按 `INDEX_NAME` 去重，含 `PRIMARY`）/ `TRIGGERS` |
| `VIEW` | 列 | `COLUMNS` |
| 列 / 索引 / 触发器 / 序列（叶子） | 无；请求子节点报 `metadata.object_kind_unsupported` | — |

- **`database` 就是 catalog**，**没有 schema 层级**：带 `schema` 的引用和 `SCHEMA` 父节点一律
  `capability.unsupported`，不伪造空 schema 层；
- **序列**：MariaDB 10.3+ 的 `CREATE SEQUENCE` 在 `information_schema.TABLES` 里就是
  `TABLE_TYPE = 'SEQUENCE'` 的一行，因此序列作为叶子节点列出（`has_children = false`），
  不需要额外的 `SHOW SEQUENCES`；
- **触发器**：`information_schema.TRIGGERS` 只展示当前账号有 `TRIGGER` 权限的行，
  所以这里可能比真实触发器少，但不会伪造；
- 根节点仍是「当前库的对象」（v0.2.1 行为），目录层要显式请求：`parent.kind = CATALOG`
  且不带名字（见 [grpc-conventions](../api/grpc-conventions.md)）。

### 跨库与权限

跨库引用必须显式给出 `catalog`（库名）；省略 = 当前连接库（`DATABASE()`），**不隐式切库**。
库名解析顺序：

1. `information_schema.SCHEMATA` 里可见 → 返回服务端规范名（比较大小写不敏感）；
2. 不可见 → 用 `SHOW CREATE DATABASE <反引号引用并转义的库名>` 探测：
   `1049`（ER_BAD_DB_ERROR）→ `metadata.object_not_found`，
   `1044` / `1142` / `1143` → `metadata.permission_denied`。

**已知限制**：`SCHEMATA` 已按权限过滤，且没有全局权限的账号在两种情形下都只拿到 `1044`
（实测 MariaDB 11.4：`SHOW CREATE DATABASE mysql` 与 `SHOW CREATE DATABASE <不存在的库>`
都是 1044，`USE <不存在的库>` 同样是 1044）。此时驱动按服务端原话报
`metadata.permission_denied`，消息里写明「无法与库不存在区分」，**不猜成**
`metadata.object_not_found`。1049 的分支由单元测试 `probe_codes_map_to_metadata_errors` 锁定
（需要带全局权限的账号才能真实触发）。

库名/表名进 `information_schema` 查询时全部是**绑定参数**
（`TABLE_SCHEMA = ? AND TABLE_NAME = ?`），因此没有拼接与转义问题；唯一把库名当**标识符**用的
位置是上面的 `SHOW CREATE DATABASE` 探测，那里用反引号引用、内部反引号成对转义
（反引号、空格、中文都是字面量）。集成用例
`metadata_object_names_with_backticks_spaces_and_cjk_round_trip` 用含反引号/空格/中文的
表名、列名、索引名锁定「不需要转义也不会被破坏」。

表名大小写按服务端 `information_schema` 的比较规则（`lower_case_table_names`）处理，
返回的 `TABLE_NAME` 是服务端给出的规范名。

### 子节点顺序与分页

每一层都由 `sqarvo_metadata::paginate_children` 分页，token 仍是 `v1:<offset>:<16 位指纹>`：

- **顺序**：先按种类（表 → 视图 → 列 → 索引 → 序列/触发器…，即契约 `ObjectKind` 的声明顺序），
  再按限定名。**v0.2.1 的「只按名字排序」不再是规则**：视图排在表之后，前端不得假设全局字母序；
- **键**：`<父节点作用域>␟<种类>:<限定名>`；种类进键（列与索引可以同名），作用域进键
  （跨库/同名表不会互相串页）；
- 非法 token / 越界 → `config.invalid`；翻页期间集合变化 → `metadata.snapshot_changed`。

### 错误分类

| 情形 | 错误码 | gRPC |
|---|---|---|
| 库不存在（服务端回 1049） | `metadata.object_not_found` | `NOT_FOUND` |
| 库不可读（1044/1142/1143，含「不存在但无全局权限」） | `metadata.permission_denied` | `PERMISSION_DENIED` |
| 表/视图不存在（`information_schema` 不可见） | `metadata.object_not_found` | `NOT_FOUND` |
| 该数据库没有这一层（schema） | `capability.unsupported` | `UNIMPLEMENTED` |
| 种类不适用于该操作（序列取列、叶子展开、视图当表展开） | `metadata.object_kind_unsupported` | `INVALID_ARGUMENT` |
| `GetDdl` 对象不存在（`information_schema` 不可见） | `metadata.object_not_found` | `NOT_FOUND` |
| `GetDdl` 的 `SHOW CREATE` 被服务端拒绝（1044/1142/1143） | `metadata.permission_denied` | `PERMISSION_DENIED` |
| `GetDdl` 对索引 / 触发器 / 列等种类 | `metadata.object_kind_unsupported` | `INVALID_ARGUMENT` |

元数据浏览用 `metadata.object_not_found`；表数据浏览/行编辑仍用
`mariadb.object_not_found`（`BrowseTable` 的既有契约），两者由驱动显式翻译。

### 列默认值、字符集与排序规则（#59）

`COLUMN_DEFAULT` 是**文本**（10.2.7 起字符串默认值带引号），三态因此可以直接区分，
不需要按 `IS_NULLABLE` 反推：

| `COLUMN_DEFAULT` | `ColumnMeta.default_value` | 说明 |
|---|---|---|
| SQL NULL | `NoDefault` | 没有默认值（NOT NULL 且未写 DEFAULT；`AUTO_INCREMENT` 列也是这种） |
| 文本 `NULL` | `Null` | 默认值是 SQL NULL。可空列即使没写 `DEFAULT NULL`，隐式默认值也是 NULL，服务端同样报 `NULL` 文本 |
| 其余文本 | `Expression(原文)` | **未求值**的默认值文本：`'x'`、`''`、`current_timestamp()` 原样交付 |

在 MariaDB 11.4 测试库实测（集成测试锁定）：

```sql
label VARCHAR(32) NOT NULL DEFAULT 'x'      -- COLUMN_DEFAULT = 'x'                 → Expression("'x'")
blank VARCHAR(8)  NOT NULL DEFAULT ''       -- COLUMN_DEFAULT = ''                  → Expression("''")
note  VARCHAR(32) DEFAULT NULL              -- COLUMN_DEFAULT = NULL（文本）        → Null
bare  VARCHAR(32)                           -- COLUMN_DEFAULT = NULL（文本，隐式）  → Null
amount INT NOT NULL                         -- COLUMN_DEFAULT = SQL NULL            → NoDefault
created TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
                                            -- COLUMN_DEFAULT = current_timestamp() → Expression(...)
```

要点：

- 驱动不去引号、不求值、不做大小写归一。前端必须把 `Expression` 当表达式展示，
  不能当成已求值的字面量。
- `character_set` / `collation` 取 `CHARACTER_SET_NAME` / `COLLATION_NAME`：字符型列（含
  `ENUM`/`SET`）非空，数值/二进制/时间列为 `None`（不适用），驱动不填 `binary` 之类的推测值。
- 结果集路径（`SELECT` 返回的列）没有默认值信息，`default_value` 是 `Unspecified`（未知）；
  协议里的字符集只有编号、没有名字，本版不映射，`character_set` / `collation` 均为 `None`。

### 分页 token 与失效条件

分页由 `sqarvo-metadata::paginate_by_key` 生成不透明 token，格式为
`v1:<offset>:<16 位十六进制指纹>`：

- 指纹是**有序对象名集合**的 FNV-1a 64 位摘要，翻页时重新校验；
- token 结构非法、版本未知、偏移量越界 → `config.invalid`（`INVALID_ARGUMENT`），
  **绝不回退到第一页**；
- 对象名集合在翻页期间发生变化（新建/删除/改名）→ `metadata.snapshot_changed`
  （`FAILED_PRECONDITION`），调用方应从第一页重新读取；
- 对象数量不变但内容整体替换且名字相同（例如 `DROP` 后重建同名表）不会被指纹区分，
  会被当成同一对象继续翻页。

已知代价：本版一次性读取当前库的全部对象名再在内存里切页（上限 500 行/页），
不做 `LIMIT/OFFSET` 服务端分页；库内对象数量极大时应改用服务端游标（待定）。

### v0.2.2 契约（#81；插入行 #85、DDL #83 已实现，过滤下推见 #84）

`ListNodes.filter`、`GetDdl` 与 `InsertRows` 的契约已冻结（ADR 0019），MariaDB 侧实现见
#84 / #83 / #85：

- **过滤下推**：`information_schema.TABLES.TABLE_NAME` 上用
  `LIKE CONCAT('%', ?, '%')`（`%`、`_`、`\` 先转义成字面量）实现 `contains`，`=` 实现
  `exact`，`TABLE_TYPE IN (...)` 实现 `kinds`；大小写取决于列/连接的排序规则
  （`utf8mb4_general_ci` 下不敏感、`_bin` 下敏感），驱动不得额外 `LOWER()`；
- **过滤条件进 token 指纹**：条件变化 → 旧 token 报 `config.invalid`（`metadata.filter_invalid`
  只用于条件本身非法）；
- **DDL（已实现，#83）**：`SHOW CREATE TABLE` / `SHOW CREATE VIEW` / `SHOW CREATE SEQUENCE`
  原文，`exact = true`、`source = DDL_SOURCE_DATABASE`、`source_detail` 写明用的是哪条
  `SHOW CREATE`；MariaDB 的 `SHOW CREATE` 会补全 `AUTO_INCREMENT=`、字符集等，属数据库
  原文，驱动不要改写；
- **插入（已实现，#85）**：只绑给出的列（空列清单用 `INSERT INTO t () VALUES ()`）；
  自增主键用 `LAST_INSERT_ID()` 回传，复合主键回传写入值与生成值，非自增主键列被省略时
  整批拒绝（`capability.unsupported`，不编造主键）；整批隐式事务 / savepoint
  （`sqarvo_row_insert`），任一行失败整批回滚（`rolled_back = true`）；
  唯一冲突 → `table_data.unique_violation`，显式 NULL → `table_data.not_null_violation`
  （1048），省略且无默认值 → `table_data.not_null_without_default`（1364 / 预校验），
  生成列 → `table_data.column_not_insertable`（1906 / `EXTRA` 预判定），
  长度与取值越界（1406 / 1264 / 1265 / 1366 / 1292 / 1690）与 CHECK / 外键 → `config.invalid`
  （v0.2.2 未登记类型专用码）；
- 过滤已由 #84 实现（见下「对象搜索」）；插入已由 #85 实现，
  `supports_row_insertion()` 为 `true`；DDL 已由 #83 实现（见下）。

### 对象搜索（#84）

`ListNodes.filter` 的过滤**下推到 `information_schema` 的 SQL**：驱动不把整层读回来再筛，
过滤后的结果集才进 `paginate_children_filtered` 切页。每层实际发出的语句：

| 父节点 | 数据来源 | 名称条件 | 种类条件 |
|---|---|---|---|
| 空（根） / `CATALOG`（带库名） | `information_schema.TABLES` | `AND TABLE_NAME LIKE CONCAT('%', ?, '%')`（contains）或 `AND TABLE_NAME = ?`（exact） | `TABLE_TYPE IN ('BASE TABLE','VIEW','SYSTEM VIEW','SEQUENCE')` |
| `CATALOG`（不带库名） | `information_schema.SCHEMATA` | `WHERE SCHEMA_NAME LIKE CONCAT('%', ?, '%')` | 不含 `CATALOG` → 直接空结果 |
| `TABLE` / `VIEW` | `COLUMNS` / `STATISTICS` / `TRIGGERS` | `AND COLUMN_NAME LIKE CONCAT('%', ?, '%')` 等 | `kinds` 排除的种类连查询都不发 |

- 名称与种类都只以**绑定参数**进 SQL；`contains` 的 `%` 包裹用 `CONCAT`（MariaDB 的 `||`
  默认是逻辑或，除非启用 `PIPES_AS_CONCAT`），绑定值是**已转义**的字面量；
- **转义**：`%`、`_` 与 `\` 自身变成字面量。MySQL 协议族的 `LIKE` 默认转义符就是反斜杠，
  因此**不**写 `ESCAPE` 子句（写了在 `NO_BACKSLASH_ESCAPES` 下会把 `''` 当成两个字符而
  报错）；已知限制：`NO_BACKSLASH_ESCAPES` 打开时默认转义符失效，本版不处理（见「待定」）；
- **大小写与重音跟着列的排序规则，驱动不加 `LOWER()`**（本机 MariaDB 11.4 实测）：
  - `information_schema.TABLES.TABLE_NAME` 是 `utf8mb3_general_ci`：
    - `LIKE`（contains）**不区分** ASCII 大小写，**且折叠重音**（`cafe` 命中 `café`）；
    - `=`（exact）在驱动把字符串按字节串绑定后是**二进制比较**：`USER_LOG` 不命中
      `user_log`。这与 `contains` 不对称，也与 SQLite 的 `exact` 一致（那边是 BINARY
      排序规则）；要跨库一致的大小写语义，得单独立 ADR（ADR 0019「重新评估条件」）；
  - 表名本身的大小写取决于 `lower_case_table_names`，与这里的**比较**语义是两件事；
- **分页 token**：过滤结果用 `v2:<offset>:<16 位集合指纹>:<16 位过滤标签>`，标签取自
  `ObjectFilter::fingerprint_input()`。换关键词 / 换匹配方式 / 加 `kinds` → 旧 token
  `config.invalid`；结果集变化仍是 `metadata.snapshot_changed`；空过滤与
  `list_children` 完全一致（token 仍是 v1）；
- **空结果 vs 无权限**：没搜到是合法空页；库不可读（1044 / 1142 / 1143）沿既有路径报
  `metadata.permission_denied`（`PERMISSION_DENIED`），**不用空列表冒充**。
  `information_schema` 会按权限隐藏行（这是 MariaDB 的可见性规则），但「整个库不可读」
  仍由 `SHOW CREATE DATABASE` 探测给出明确错误；
- **性能**（本机实测，`#[ignore]` 用例 `search_on_large_schema_bench`，release）：
  同一个库里 1 万张表时，`contains` 命中 3 个 **7.2 ms**，不过滤首页 **153 ms**；
  同时读 `SHOW SESSION STATUS LIKE 'Rows_sent'` 的前后差值来证明下推——**只增加 4**
  （若把整层读回再筛，这个差值会等于对象总数）。10 万张表没有跑：InnoDB 建表约
  41 ms/张（1 万张实测 412 s 建、333 s 删），10 万张要两小时以上，且测试库与其它 lane
  共用；SQL 形状与过滤路径与规模无关，10 万只影响数据库侧扫描量。
  `LIKE CONCAT('%', ?, '%')` 不能用索引，代价在数据库侧扫 `information_schema`。

### DDL 查看（#83）

| 种类 | 语句 | 结果 |
|---|---|---|
| `TABLE` | `SHOW CREATE TABLE` | 原文，2 列中的第 2 列（`Create Table`） |
| `VIEW` | `SHOW CREATE VIEW` | 原文，**4 列**中的第 2 列（`Create View`）；第 3/4 列是 `character_set_client` / `collation_connection`，绝不能当 DDL |
| `SEQUENCE` | `SHOW CREATE SEQUENCE` | 原文（MariaDB 10.3+ 序列以 `TABLE_TYPE='SEQUENCE'` 出现在 `TABLES`） |
| `INDEX` | — | `metadata.object_kind_unsupported`：见下 |
| `TRIGGER` / `COLUMN` / `CATALOG` | — | `metadata.object_kind_unsupported`（`SHOW CREATE TRIGGER` 的权限要求本版未覆盖） |

- **列的选择**：按**下标 1**（第 2 列）取，取完再校验文本以 `CREATE ` 开头且包含
  ` TABLE ` / ` VIEW ` / ` SEQUENCE `——列错位（版本差异、误取字符集列）会报
  `internal.error`，不会把非 DDL 文本当原文交付；
- **索引没有独立 DDL，本版明确报错而不拼装**：MariaDB 的索引定义是 `CREATE TABLE`
  的一部分，`ObjectRef` 又不携带所属表（`catalog` / `schema` / `name` / `kind`），
  无法唯一定位同库中重名的索引；拼出来的 `CREATE INDEX` 会丢前缀长度、索引类型、
  表达式索引、可见性等信息，比报错更危险。前端要展示索引定义时应读所属表的 DDL；
- **权限与不存在**：先经 `information_schema` 判存在（不可见 → `metadata.object_not_found`），
  库不可读（1044）→ `metadata.permission_denied`；`SHOW CREATE` 自身返回 1044/1142/1143
  → `metadata.permission_denied`，1146/1051 → `metadata.object_not_found`；
- **长 DDL 不截断**：与 SQLite 一致，驱动原样返回；实际上限由 gRPC 消息上限（tonic 默认
  4 MiB）决定，超限时请求失败而不是返回半截文本。ADR 0007 的 64 KiB 是单元格规则，
  不适用于 DDL。

### 已知缺口

- `COLUMN_TYPE` 是服务端原文，MariaDB 11.4 会带显示宽度（例如 `bigint(20) unsigned`），
  驱动不裁剪，前端应按原样展示；
- `ENUM` / `SET` / `GEOMETRY` / `BIT` 无法无损映射到逻辑类型，返回
  `ColumnType::Unsupported { type_name: COLUMN_TYPE }`；
- 权限不足的对象在 `information_schema` 里不可见，因此元数据查询表现为
  `mariadb.object_not_found`（`NotFound`）。这是 MySQL 协议族的语义；连接级无权限
  （1044）仍由「错误映射」表归类为 `mariadb.permission_denied`。`GetDdl` 是例外：
  库不可读（`SHOW CREATE DATABASE` 探测到 1044/1142）与 `SHOW CREATE` 自身被拒绝
  都报 `metadata.permission_denied`——这时「是权限不是不存在」可以说清；
- 连接没有选择数据库时列出对象返回 `mariadb.no_database`（`FAILED_PRECONDITION`），
  不把 `DATABASE() IS NULL` 当成「库里没有表」；
- 有未读完的结果游标时元数据请求返回 `mariadb.query_active`，避免与游标交错。

## TLS 逐档验证（#106）

判据是**服务端会话变量** `SHOW SESSION STATUS LIKE 'Ssl_cipher'` 是否非空，而不是「连接成功」
或错误码：只有非空才能证明这条会话确实完成了 TLS 握手，而不是悄悄回退到了明文。

2026-11-06 在 MariaDB 11.4.13 上逐档实测（`crates/drivers/mariadb/tests/tls_handshake.rs`，8 个用例，
全部由环境变量门控，缺变量时打印跳过原因）：

| 档位 | 结果 | `Ssl_cipher` | 用例 |
|---|---|---|---|
| `disabled` | 连接成功 | 空（明文） | `disabled_tls_connects_without_cipher` |
| `preferred` | 连接成功 | `TLS_AES_256_GCM_SHA384` | `preferred_tls_negotiates_cipher` |
| `required` | 连接成功 | `TLS_AES_256_GCM_SHA384` | `required_tls_negotiates_cipher` |
| `verify-ca`（固定根证书，证书带 SAN） | 连接成功（驱动归一化为 `verify-full`，ADR 0020） | `TLS_AES_256_GCM_SHA384` | `verify_ca_trusts_pinned_root_certificate` |
| `verify-full`（证书带 SAN） | 连接成功 | `TLS_AES_256_GCM_SHA384` | `verify_full_accepts_matching_certificate` |
| `verify-full`（证书无 SAN，主机 `127.0.0.1`） | 拒绝 | — | `verify_full_rejects_hostname_mismatch` → `mariadb.connection_failed` / `Unavailable` |
| `preferred`（服务端无 TLS） | 回退明文成功 + `mariadb.tls_fallback` 告警 | 空 | `preferred_falls_back_to_plaintext_and_warns` |
| `required`（服务端无 TLS） | 失败 | — | `required_fails_on_server_without_tls` → `mariadb.connection_failed` |

### 证书从哪来

`docker.io/library/mariadb:11.4` 的测试容器（`sqarvo-mariadb-test`，`have_ssl=YES`）**不落盘证书**：
`/var/lib/mysql` 下没有 `ca.pem` / `server-cert.pem` / `server-key.pem`，容器内 `find / -name '*.pem'`
也只找得到系统 CA。服务端在启动时自动生成自签证书并只在内存里使用：`CN=MariaDB Server`、
无 `subjectAltName`、自签（`issuer` 同名）、有效期到 2036-11-01。所以「复制容器里的 `ca.pem`」不可行；
根证书从一次真实握手里取出——自签叶证书就是它自己的信任锚：

```bash
openssl s_client -starttls mysql -connect 127.0.0.1:53306 -showcerts </dev/null 2>/dev/null \
  | openssl x509 -out /tmp/sqarvo-mariadb-ca.pem
openssl x509 -in /tmp/sqarvo-mariadb-ca.pem -noout -subject -fingerprint -sha256
# subject=CN=MariaDB Server
# sha256 Fingerprint=EE:6D:61:46:2E:37:22:4E:D4:A5:99:50:E3:19:E3:57:15:DC:D6:BF:FC:87:24:86:59:19:7F:C3:44:F9:1B:47
```

这张证书没有 SAN，`verify-ca` / `verify-full` 的**成功**路径没法在它上面取证
（`NotValidForNameContext { expected: IpAddress(127.0.0.1), presented: [] }`）。成功路径改用临时容器：
自建 CA 签一张带 `DNS:localhost, IP:127.0.0.1` SAN 的服务端证书（一次性，30 天）：

```bash
mkdir -p /tmp/sqarvo-tls106-certs && cd /tmp/sqarvo-tls106-certs
openssl req -x509 -newkey rsa:2048 -nodes -keyout ca-key.pem -out ca.pem -days 30 \
  -subj "/CN=Sqarvo Test CA 106" -addext "basicConstraints=critical,CA:TRUE"
openssl req -newkey rsa:2048 -nodes -keyout server-key.pem -out server.csr -subj "/CN=sqarvo-tls106"
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\nkeyUsage=critical,digitalSignature,keyEncipherment\n' > ext.cnf
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial \
  -out server-cert.pem -days 30 -extfile ext.cnf
chmod 644 ./*.pem
```

### 三个监听端

```bash
# 1) 共享测试库（TLS，自签自动证书）：sqarvo-mariadb-test，127.0.0.1:53306，常驻

# 2) 无 TLS：preferred 回退与 required 反向对照
podman run -d --rm --name sqarvo-tls106-notls \
  -e MARIADB_USER=sqarvo_test -e MARIADB_PASSWORD=sqarvo_test_only \
  -e MARIADB_DATABASE=test_106 -e MARIADB_ROOT_PASSWORD=sqarvo_root_only \
  -p 127.0.0.1:53307:3306 docker.io/library/mariadb:11.4 --skip-ssl

# 3) 带 SAN 证书：verify-ca / verify-full 成功路径
podman run -d --rm --name sqarvo-tls106-san \
  -e MARIADB_USER=sqarvo_test -e MARIADB_PASSWORD=sqarvo_test_only \
  -e MARIADB_DATABASE=test_106 -e MARIADB_ROOT_PASSWORD=sqarvo_root_only \
  -v /tmp/sqarvo-tls106-certs:/certs:ro -p 127.0.0.1:53308:3306 \
  docker.io/library/mariadb:11.4 \
  --ssl-ca=/certs/ca.pem --ssl-cert=/certs/server-cert.pem --ssl-key=/certs/server-key.pem
```

- `--skip-ssl` 生效的判据是 `SHOW VARIABLES LIKE 'have_ssl'` 变成 `DISABLED`（默认 `YES`）；
  证书挂载进去后 `SHOW VARIABLES LIKE 'ssl_cert'` 应为容器内路径。
- MariaDB 11.4 的**客户端**默认要求 TLS，手工核对无 TLS 监听端时要加 `--skip-ssl`，
  否则客户端自己报 `SSL is required, but the server does not support it`（不是服务端的问题）。
- 临时容器用 `podman rm -f sqarvo-tls106-notls sqarvo-tls106-san` 清理（`--rm` 在停止时删除）。

### 运行与真实输出

```bash
cd backend
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test' \
SQARVO_MARIADB_CA=/tmp/sqarvo-mariadb-ca.pem \
SQARVO_MARIADB_NO_TLS_URL='mysql://sqarvo_test:***@127.0.0.1:53307/test_106' \
SQARVO_MARIADB_SAN_URL='mysql://sqarvo_test:***@127.0.0.1:53308/test_106' \
SQARVO_MARIADB_SAN_CA=/tmp/sqarvo-tls106-certs/ca.pem \
  cargo test -p sqarvo-driver-mariadb --test tls_handshake -- --nocapture --test-threads=1
```

```text
running 8 tests
test disabled_tls_connects_without_cipher ... ok
test preferred_falls_back_to_plaintext_and_warns ... preferred 回退：version=Some("11.4.13-MariaDB-ubu2404") warnings=[Warning { code: "mariadb.tls_fallback", message: "TLS 不可用，已按「优先 TLS」模式回退为明文连接" }]
ok
test preferred_tls_negotiates_cipher ... preferred：Ssl_cipher = TLS_AES_256_GCM_SHA384
ok
test required_fails_on_server_without_tls ... required（服务端无 TLS）：code=mariadb.connection_failed category=Internal message=连接 MariaDB 失败
ok
test required_tls_negotiates_cipher ... required：Ssl_cipher = TLS_AES_256_GCM_SHA384
ok
test verify_ca_trusts_pinned_root_certificate ... verify-ca：Ssl_cipher = TLS_AES_256_GCM_SHA384
ok
test verify_full_accepts_matching_certificate ... verify-full：Ssl_cipher = TLS_AES_256_GCM_SHA384
ok
test verify_full_rejects_hostname_mismatch ... verify-full（主机名不匹配）：code=mariadb.connection_failed category=Unavailable message=数据库连接中断
ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
```

该输出是 2026-11-06 的实测记录，原样保留。其中 `required` 的 `category=Internal` 与
`verify-full` 的 `message=数据库连接中断` 已由 ADR 0020 修正为 `Unavailable` 与新的证书错误
文案（见下「已知边界与修正记录」）；下次实测时更新这段输出，不在此处改写历史记录。

2026-11-07（ADR 0020 实现后）本地复跑：三个监听端同前（53306 自签自动证书、53307 `--skip-ssl`、
53308 自建 CA 签的带 SAN 证书），`cargo test -p sqarvo-driver-mariadb --test tls_handshake --
--nocapture --test-threads=1` → **9 passed; 0 failed**（新增 `verify_ca_is_normalized_to_verify_full`）。
与旧记录相比的差异：`required_fails_on_server_without_tls` 输出 `category=Unavailable`、
`message=服务端不支持 TLS（连接要求 TLS）`；`verify_ca_is_normalized_to_verify_full` 与
`verify_full_rejects_hostname_mismatch` 都输出 `code=mariadb.connection_failed`、
`category=Unavailable`、`message=服务端证书与主机名不匹配`。这是本地门控证据（未接入 CI），
53306 的根证书是用 `openssl s_client -starttls mysql` 从一次真实握手里取出的。

`tls_server_name` 覆盖同样实测：`SQARVO_MARIADB_SAN_SERVER_NAME=localhost`（证书的 `DNS:` SAN）时
`verify_full_accepts_matching_certificate` 通过；不设时按 URL 主机 `127.0.0.1` 匹配 `IP:` SAN。

不设任何环境变量时用例逐条打印跳过原因并返回（不是静默通过）：`cargo test -p sqarvo-driver-mariadb`
输出 90 行 `跳过：未设置 …`。

### 已知边界与修正记录（#106 实测 + ADR 0020 处置）

1. **`verify-ca` 归一化为 `verify-full`（设计决定，ADR 0020，不再算缺口）**：
   `mysql_async` 0.37.1 用 `e.to_string().contains("NotValidForName")` 判断是否跳过主机名校验
   （`src/io/tls/rustls_io.rs` 的 `DangerousVerifier`），而 rustls 0.23 的 `Display` 输出是
   `invalid peer certificate: certificate not valid for name …`，永远匹配不到，
   `with_danger_skip_domain_validation(true)` 因此是死参数（#106 实测：对无 SAN 的自签证书用
   `verify-ca` + 固定根证书，仍被 `InvalidCertificate(NotValidForNameContext { expected:
   IpAddress(127.0.0.1), presented: [] })` 拒绝）。驱动因此把 `verify-ca` **归一化**为
   `verify-full`（固定根证书 + 主机名校验），并在能力位 `tls_verify_ca_chain_only = false` 上
   声明「本驱动不支持只校链、不校名」，前端连接对话框据此标注。
   **用户出路**：证书没有匹配连接地址的 SAN 时，用既有字段 `tls_server_name` 指定证书里确实
   存在的名字（`DNS:` SAN），`verify-full` 即可通过——已实测（`SQARVO_MARIADB_SAN_SERVER_NAME=localhost`
   时 `verify_full_accepts_matching_certificate` 通过）。证书既无 SAN 也无可用名字时，MariaDB
   上就没有「只校链」这一档，只能换证书或改用不校验证书的档位。
   方向是**更严**：归一化只会多拒，不会放行本该拒绝的证书。
2. **证书校验失败的文案与分类**：已按 ADR 0020 改为强类型判定——从
   `IoError::Io(io)` 的 `io.get_ref().downcast_ref::<rustls::Error>()` 取回 rustls 错误，
   按 `CertificateError` 变体分档，不做字符串匹配。三档文案（错误码都是
   `mariadb.connection_failed`）：

   | 情形 | 文案 | 分类 |
   |---|---|---|
   | 证书主机名不匹配（`NotValidForName` / `NotValidForNameContext`） | 服务端证书与主机名不匹配 | `Unavailable` |
   | 证书不受信 / 已失效 / 用途不符（`UnknownIssuer`、`Expired*`、`InvalidPurpose*`、`BadSignature` 等） | 服务端证书未被信任或已失效 | `Unavailable` |
   | 其他 TLS 错误（`IoError::Tls` 分支） | TLS 握手失败 | `Unavailable` |

   归一化后，无 SAN 证书在 `verify-ca` 下与 `verify-full` 一样被拒，落到第一档——报错要读成
   「换个校验名」而不是「换个证书」。
3. **`required` 撞上无 TLS 服务端**：已修（ADR 0020）。分类从 `Internal` 改为 `Unavailable`，文案为
   「服务端不支持 TLS（连接要求 TLS）」，错误码仍是 `mariadb.connection_failed`。真实变体是
   `DriverError::NoClientSslFlagFromServer`（`display`：`Client asked for SSL but server does not
   have this capability`），不是 #106 时推测的 `Error::Other`；`error.rs` 按变体精确匹配，不看文案。

### 未覆盖

- 客户端证书 / 双向 TLS（`ssl_cert` + `ssl_key` → `ClientIdentity`）：代码路径存在
  （`config.rs` 的 `build_ssl_opts`），测试库未配置「要求客户端证书」，**未验证**；
- MySQL 8.x 的 TLS 语义（`--ssl-mode`、`caching_sha2_password`）：未实测；
- 远程后端与真实网络（中间人拦截、证书轮换、过期）：未覆盖；
- CI：`.github/workflows/ci.yml` 的 MariaDB 作业仍只跑 `--test integration`，本文件需要额外两个
  监听端，**尚未接入 CI**（也没有验证过 CI 环境能否生成同样的证书），所以目前是本地门控证据。

## 表数据浏览：排序、筛选、offset 与总数（v0.2.3，#111）

`BrowseTable` 的四个新维度（ADR 0022）全部**下推到 SQL**，能力位
`table_browse_sort_filter_total` 已翻为 `true`：

| 请求 | 下推成的 SQL | 说明 |
|---|---|---|
| `sort`（多列） | `ORDER BY` + 按请求顺序的「列 ASC/DESC」 | 列名取 `GetColumns` 的**规范名**；无 `sort` 时仍是主键升序（键集游标路径不变） |
| `filter.conditions`（只 AND） | `WHERE` 逐项比较（`IS NULL` / `IS NOT NULL` 不绑定值） | 每个条件是一个独立项，值一律 `?` 占位符 |
| `offset` | `LIMIT ? OFFSET ?` | 与 `page_token` 互斥；`offset=0` 等同未设置（proto3 无法区分） |
| `include_total` | `SELECT COUNT(*)` 或 `information_schema.TABLES.TABLE_ROWS` | 三态见下 |

硬约束（与行编辑同一条底线）：

- **列名白名单**：排序/筛选的列必须命中 `information_schema.COLUMNS` 返回的列（大小写按
  数据库语义匹配）。命中后拼进 SQL 的是 schema 里的规范名，客户端给的标识符文本**永不**
  进入 SQL。未知列在发送前返回 `config.invalid`（`INVALID_ARGUMENT`），不会让数据库报语法错；
- **值全部参数绑定**：`LIKE` 的匹配串也是参数。驱动不改写通配符，语义由 MariaDB 决定：
  `%` / `_` 是通配符，字面量用 `\%` / `\_`（默认转义符 `\`）；
- **NULL 比较**：`IS NULL` / `IS NOT NULL` 忽略 `value`。比较算子（`=`、`<>`、`<`、`<=`、
  `>`、`>=`、`LIKE`）的值若是 SQL `NULL`，返回 `config.invalid` 并提示改用 `IS NULL`——
  `x = NULL` 恒为未知，静默返回空页会误导用户。

能力边界：

- `page_token` 只描述「主键升序」的下一页：与 `sort` 或 `offset > 0` 并用返回
  `config.invalid`；无主键表不签发游标，也不接受 `page_token`（沿用 v0.2.2 的
  「不提供继续语义」，`has_more` 恒为 false）；
- 无主键表允许排序 / 筛选 / 计数，但 `ordering = UNSTABLE`、`read_only = true`
  （编辑定位依赖主键，ADR 0017 / 0022 §6）；
- offset 分页**不保证跨页稳定**：翻页期间有插入/删除会出现重复或漏行，界面必须在
  「Last Refresh」旁提供刷新入口；连续翻页的稳定性只在「无 sort + `page_token`」时成立。

### 总数三态与阈值

| 条件 | SQL | `total_kind` |
|---|---|---|
| `include_total = false` | 不查询 | `UNKNOWN`，`total_rows` 为空 |
| 有筛选 | `SELECT COUNT(*) ... WHERE <同一份条件>` | `EXACT` |
| 无筛选，`TABLE_ROWS` ≤ 100 000（含 NULL：视图 / 统计未生成） | `SELECT COUNT(*)` | `EXACT` |
| 无筛选，`TABLE_ROWS` > 100 000 | `information_schema.TABLES.TABLE_ROWS` | `ESTIMATE` |

- 阈值常量 `EXACT_COUNT_MAX_ESTIMATED_ROWS = 100_000`（`table_data.rs`）：InnoDB 的
  `COUNT(*)` 要扫一遍，本机容器上 10 万行约数十毫秒、百万行级到秒级；阈值以下给精确值，
  以上退回统计信息，避免「打开表格」被一次计数拖住；
- **估计就是估计**：`TABLE_ROWS` 来自 InnoDB 采样统计，偏差可达数十个百分点，永远以
  `ESTIMATE` 返回，绝不冒称 `EXACT`（实测 15 万行表报 149877）；
- 有筛选时**不用** `TABLE_ROWS`：它反映不了筛选后的行数，拿它当筛选总数就是编数字；
- 精确计数与取页在同一个会话执行锁内（ADR 0008），取消走该会话已有的 `KILL QUERY` 路径；
  浏览请求没有独立的超时字段，`EXACT` 计数在大表上可能较慢（见「未支持清单」）。

### 大 offset 的成本

`LIMIT ? OFFSET ?` 需要先扫过前面 `offset` 行（InnoDB 二级索引范围扫描 + 回表），
成本随 offset 线性增长。实测（本机容器，MariaDB 11.4.13，15 万行 `INT` 主键表）：

| 请求 | 耗时 |
|---|---|
| `offset=0, limit=100` | 约 2 ms |
| `offset=90000, limit=100` | 约 7–17 ms（两次运行：7.4 ms / 16.7 ms） |

上表只是观察值（单次波动较大），不构成性能承诺。v0.2.3 的界面只提供首页 / 上页 / 下页 / 末页，
页码只读；跳转到任意页码需要按主键估算位置的方案，不在本版（ADR 0022 §5）。

## 能力描述符（现状）

| 能力 | 值 | 说明 |
|---|---|---|
| `query_cancellation` | `true` | `KILL QUERY` 路径，见上 |
| `streaming_results` | `true` | 行流 + 驱动侧分批 |
| `multiple_statements` | `false` | 发送前拒绝 |
| `transactions` | `true` | 会话内 `START TRANSACTION` / `COMMIT` / `ROLLBACK`，见「事务」 |
| `catalogs` | `true` | v0.2.2（#82）：`database` 映射为 catalog，目录层与跨库浏览已实现 |
| `schemas` | `false` | MariaDB 没有独立 schema 层级 |
| `row_editing` | `true` | `table_data()` 提供浏览、行编辑（update / delete）与插入（#85），见 ADR 0017 / 0019 |
| `parameterized_statements` | `true` | `execute_parameterized`：值经协议绑定参数执行，不拼进 SQL 文本 |
| `ddl_generation` | `false` | 计划用 `SHOW CREATE TABLE` 原文 |
| `server_side_pagination` | `false` | 游标由驱动持有 |
| `table_browse_sort_filter_total` | `true` | v0.2.3（#111）：排序 / 筛选 / offset / 总数下推到 SQL，见「表数据浏览」 |
| `tls_verify_ca_chain_only` | `false` | 不支持「只校链、不校名」：`verify-ca` 归一化为 `verify-full`（ADR 0020） |

## 未支持清单

- 事务的隔离级别选择界面与 savepoint 对外 API（批内 savepoint 已用于行编辑）；
- 元数据的 schema 层级（MariaDB 没有这一层，显式报 `capability.unsupported`）、
  函数/存储过程/类型列表（`FUNCTION` / `PROCEDURE` / `TYPE` 父节点同样显式报错）；
  DDL 查看（#83）按 v0.2.2 契约用 `SHOW CREATE TABLE` 原文
  （`exact = true` + `source = DDL_SOURCE_DATABASE`，ADR 0019）；
- 跨库的表数据浏览/行编辑：元数据可以跨库，`BrowseTable` 仍只作用于当前库；
- 表数据浏览的任意页码跳转与「按主键估算位置」（ADR 0022 §5）：v0.2.3 只有首页 / 上页 /
  下页 / 末页，大 offset 的成本见「表数据浏览」；
- 表数据浏览的**独立超时**：浏览请求没有超时字段，`include_total` 的精确计数与取页共用
  会话执行锁，取消只能走会话级 `KILL QUERY`；
- 排序/筛选的**类型兼容性**预校验：值按 `ColumnType` 之外不加限制，比较语义交给 MariaDB
  隐式转换（例如 `int_col = 'abc'` 会按 0 比较），由客户端保证值的类型正确；
- 单次执行超时：`ExecuteRequest.timeout` 非空时返回 `capability.unsupported`，不静默忽略；
- 参数化执行的对外 RPC（SPI 的 `execute_parameterized` 已实现，查询侧 RPC 留给 S7）
  与预编译语句复用；
- 大字段按需加载（目前只截断前缀，`Cell::truncated` 标记）；
- 断线重连与会话租约；
- 压缩、`LOAD DATA LOCAL INFILE`；TLS 客户端证书（双向 TLS）的实测（代码路径存在，未在测试库验证；
  单/双向的其余档位见上文「TLS 逐档验证（#106）」）；
- MySQL 8.x 兼容性（`caching_sha2_password`、二进制 JSON）：未实测，不声明支持。

## 错误映射

用户可见文案不拼接服务端原文（可能含 SQL 片段、库表名），原始错误只作为 `source` 留给日志。

| 服务端错误 | 错误码 | 分类 |
|---|---|---|
| 1045 / 1698 | `mariadb.auth_failed` | `Unauthenticated` |
| 1044 / 1142 / 1143 | `mariadb.permission_denied` | `PermissionDenied` |
| 1049 | `mariadb.unknown_database` | `InvalidArgument` |
| 1064 | `mariadb.syntax_error` | `InvalidArgument` |
| 1146 / 1054 / 1051 | `mariadb.object_not_found` | `NotFound` |
| 1317 | `query.cancelled` | `Cancelled` |
| 1205 | `mariadb.server_error` | `Timeout` |
| 1213 | `mariadb.deadlock` | `FailedPrecondition` |
| 1792 | `mariadb.read_only_transaction` | `FailedPrecondition` |
| 1927 / 1053 | `mariadb.connection_failed` | `Unavailable` |
| 其它服务端错误 | `mariadb.server_error` | `Internal` |
| 连接建立超时 | `mariadb.connect_timeout` | `Timeout` |

驱动自己产生的错误码（不对应服务端错误）：

| 错误码 | 分类 | 触发 |
|---|---|---|
| `mariadb.implicit_commit_rejected` | `FailedPrecondition` | 事务中执行会隐式提交的语句，发送前拒绝 |
| `mariadb.transaction_lost` | `Unavailable` | 连接断开导致事务丢失，无法确认是否提交 |
| `mariadb.query_active` | `FailedPrecondition` | 有未读完的结果游标时操作事务 |
| `transaction.already_active` | `AlreadyExists` | 同一会话已有打开的事务 |
| `transaction.not_found` | `NotFound` | 事务 ID 与当前会话的事务不符 |
| `transaction.failed` | `FailedPrecondition` | 失败态（死锁后）提交或继续执行 |

错误信息中绝不包含密码或完整连接串；`MariaDbConfig` 的 `Debug` 只打印 TLS 模式与超时。

TLS 相关（详见「TLS 逐档验证」的「已知边界与修正记录」与 ADR 0020）：证书校验失败一律
`mariadb.connection_failed` / `Unavailable`，按原因分三档文案——主机名不匹配 →「服务端证书与
主机名不匹配」；证书不受信 / 已失效 / 用途不符 →「服务端证书未被信任或已失效」；其他 TLS 错误
→「TLS 握手失败」。档位要求 `required` 而服务端不支持 TLS → `mariadb.connection_failed` /
`Unavailable`，文案「服务端不支持 TLS（连接要求 TLS）」。

## 测试与复现

```bash
# 测试库（rootless podman）
podman run -d --name sqarvo-mariadb-test \
  -e MARIADB_USER=sqarvo_test -e MARIADB_PASSWORD=sqarvo_test_only \
  -e MARIADB_DATABASE=sqarvo_test_22 -e MARIADB_ROOT_PASSWORD=*** \
  -p 127.0.0.1:53306:3306 docker.io/library/mariadb:11.4

cd backend
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test_22' \
  cargo test -p sqarvo-driver-mariadb --test integration -- --test-threads=1
```

未设置 `SQARVO_MARIADB_URL` 时集成测试打印跳过原因并返回，不会静默通过。
测试表按主题使用前缀并在用例结束时清理：`sqarvo_t22_`（类型映射、长文本）、`sqarvo_t43_`（事务）。

表浏览的排序/筛选/offset/总数下推有独立用例（不与 `integration.rs` 抢文件，`#111`）：

```bash
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test_22' \
  cargo test -p sqarvo-driver-mariadb --test browse_pushdown
```

用例自建/自删表，表名带进程级随机后缀（`bp111_<hex>_<case>`），可与其他用例并行；
其中 `large_table_total_is_an_estimate_and_large_offset_is_timed` 会临时建一张 15 万行的表
（`seq_1_to_150000`）并 `ANALYZE`，用完后 DROP，耗时约 1 秒。
