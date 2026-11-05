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

## 能力描述符（现状）

| 能力 | 值 | 说明 |
|---|---|---|
| `query_cancellation` | `true` | `KILL QUERY` 路径，见上 |
| `streaming_results` | `true` | 行流 + 驱动侧分批 |
| `multiple_statements` | `false` | 发送前拒绝 |
| `transactions` | `true` | 会话内 `START TRANSACTION` / `COMMIT` / `ROLLBACK`，见「事务」 |
| `catalogs` / `schemas` | `false` | `database` 映射为 catalog，但对象树尚未实现 |
| `ddl_generation` | `false` | 计划用 `SHOW CREATE TABLE` 原文 |
| `server_side_pagination` | `false` | 游标由驱动持有 |

## 未支持清单

- 事务状态展示与 gRPC 接线（驱动层已实现，见「事务」；服务层接线未完成）；
- 元数据（对象树、列、DDL）：`metadata()` 返回 `None`；
- 单次执行超时：`ExecuteRequest.timeout` 非空时返回 `capability.unsupported`，不静默忽略；
- 参数化执行、预编译语句复用；
- 大字段按需加载（目前只截断前缀，`Cell::truncated` 标记）；
- 断线重连与会话租约；
- 压缩、`LOAD DATA LOCAL INFILE`、TLS 客户端证书的实测（代码路径存在，未在测试库验证）；
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
