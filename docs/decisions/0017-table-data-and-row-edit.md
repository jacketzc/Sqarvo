# ADR 0017：表数据浏览与行编辑契约

- 状态：已接受
- 日期：2026-11-05
- 关联：Issue #41；REQ-EDIT-001、REQ-EDIT-002、REQ-TXN-001、REQ-META-001/002/005；
  ADR 0005（游标所有权）、ADR 0007（单元格截断）、ADR 0011（MariaDB 驱动）

## 背景

v0.2.1 要交付「表数据浏览 + 安全编辑 + 事务状态」。这一步的成本不在界面，而在契约：
一旦上线，四件事就固定下来，后续任何改动都要同时改三个前端、两套驱动与 proto。

1. **分页语义**：决定翻页是否稳定、驱动能否给出稳定顺序；
2. **行的身份**：决定「改的是哪一行」依据什么；
3. **变更的表达**：决定注入面、类型保真度与驱动的实现成本；
4. **事务边界**：决定失败态、断连与重复调用时的可见行为。

另有一项跨阶段的连带决定：参数化执行原计划在 S7（导入导出）引入。表编辑必须先有
参数化 DML，否则「安全编辑」只能靠拼接 SQL 文本，与本项目的安全底线冲突。因此参数化的
SPI 与语义被提前到本版冻结。

## 备选方案

### 浏览分页

1. **OFFSET / LIMIT 重查**：每次翻页重跑 `... LIMIT n OFFSET m`。
   - 优点：实现最简单，驱动无需持有状态。
   - 缺点：翻页期间表变化会重复或丢失行（与 ADR 0005 已否决的理由相同）；偏移越大代价越高；
     大表末页退化为全表扫描。**否决**。
2. **一次拉全量再切片**：违反「大结果集不一次性进内存」，且表可能是千万行。**否决**。
3. **主键升序 + 键集（keyset）游标**：把上一页最后一行的主键值放进不透明 token。
   - 优点：`WHERE pk > ? ORDER BY pk LIMIT n`，代价与偏移无关；游标由驱动签发，客户端不解析。
   - 缺点：只能按主键顺序浏览；无主键表无法给出稳定顺序。**采用**。

### 行的定位

1. **rowid / ctid 近似定位**：SQLite 的 `rowid` 会因 `VACUUM`、`INSERT OR REPLACE` 变化；
   MariaDB/InnoDB 没有稳定 `ctid`（`ROWID` 语义随页分裂变化）；视图可能根本没有。
   把这类值暴露给前端，等于把方言实现细节写进跨平台契约。**否决**。
2. **全列值做 WHERE 条件**：大字段、浮点比较、大小写/尾空格不敏感排序规则会让定位既不准也
   可能命中多行，且要传输整行。**否决**。
3. **完整主键值**：复合主键必须给全部列，顺序由数据库决定。**采用**。

### 变更的表达

1. **前端拼 SQL 文本**：注入与转义责任落在三个前端；类型精度（Decimal、无符号整数、
   二进制）在字符串拼接中必然丢失；服务端无法做影响行数核对与审计。**否决**。
2. **前端传 SQL 模板 + 参数**：看起来安全，实际把 WHERE 子句交给前端，等价于方案 1。**否决**。
3. **强类型行变更（主键值 + 列变更），驱动内部 prepared statement 绑定**：**采用**。

### 参数化执行的时机

1. **等 S7**：表编辑要么退回拼接 SQL（已否决），要么把整个 v0.2.1 推迟到 S7 之后。
2. **本版冻结 SPI + 语义，驱动在本版实现绑定层**：**采用**。代价是被迫同时改两个驱动，
   见「代价」。

## 决策

### 1. 浏览

- RPC：`TableDataService.BrowseTable(object, page_size, page_token)` → 列元数据 + 行 +
  `next_page_token` + `has_more` + `read_only` + `read_only_reason` + `ordering`。
- 顺序固定为**主键升序**，`ordering = ROW_ORDER_PRIMARY_KEY_ASCENDING`。
- 游标**不透明**：由驱动签发，服务端只做透传与校验。语义与 ADR 0005 一致（客户端原样回传、
  不得解析、不得自行构造）；实现复用 `sqarvo-metadata` 既有分页 token 的约定
  （`PageRequest.page_token` / `Page::next_page_token`：空表示第一页，非法 token 显式报错而
  不是回到第一页），错误码为 `table_data.cursor_invalid`。
  元数据分页的非法 token 走 `config.invalid`；表数据游标单独成码，便于前端区分
  「翻页游标坏了」与「配置错误」。
- 页大小默认 100、单页上限 500（与元数据分页同值），超限**钳制**，不返回错误。
- **无主键表**：`read_only = true`、`read_only_reason = READ_ONLY_REASON_NO_PRIMARY_KEY`、
  `ordering = ROW_ORDER_UNSTABLE`，即「无稳定排序」。浏览允许，编辑一律拒绝
  （`table_data.no_primary_key`），不提供 rowid / ctid 类近似定位。
- 驱动未声明行编辑能力时：`read_only_reason = READ_ONLY_REASON_DRIVER_UNSUPPORTED`，
  编辑返回 `capability.unsupported`。

### 2. 行编辑

- RPC：`TableDataService.ApplyRowChanges`，一次提交一批 update / delete。
  每行 = 完整主键值（`RowKey`）+ 列变更（`ColumnChange{column, ColumnValue}`）+
  `expected_rows_affected`（0 表示未设置，按 1 处理）。
- 通用层与 proto 中**没有 SQL 字段**：SQL 由驱动按方言构造，值一律走 prepared statement 绑定。
  `Value` 复用结果集的强类型模型（NULL / Decimal / 无符号整数 / 二进制 / 时间戳不丢失）。
- 逐行返回 `RowChangeResult`（`APPLIED` / `ROW_NOT_FOUND` / `CONFLICT`），单行失败不吞掉整批结果。
  影响行数与期望不符 → `table_data.rows_affected_mismatch`；目标行已不存在 → `table_data.row_deleted`。
- **风险确认**：预计影响行数超过阈值（默认 **100 行**，`DEFAULT_RISK_CONFIRMATION_ROWS`）
  且 `acknowledge_risk = false` → `table_data.confirmation_required`（FAILED_PRECONDITION），
  由前端提示后重试。阈值由服务端决定，客户端不得自行放宽。
- **批次上限 500 行**（`MAX_ROW_CHANGES_PER_BATCH`）。超限由服务端**钳制**而不是报错：
  只执行前 500 条，回 `clamped = true` 与 `deferred_changes`，客户端必须续传。
  钳制必须显式可见——静默丢弃用户编辑是不可接受的。

### 3. 事务

- 一个 session 同时最多一个打开的事务：已有事务时 `Begin` → `transaction.already_active`
  （`ALREADY_EXISTS`）。
- 失败态只允许回滚：`Commit` → `transaction.failed`（`FAILED_PRECONDITION`）。
  状态机已存在于 `crates/transaction/src/state.rs`，本 ADR 只是把语义写进契约。
- 断连由**后端**强制回滚，不依赖前端发送 `Rollback`；服务端在会话清理路径上兜底。
- **重复 Commit / Rollback 幂等**：对已结束或已不存在的事务，`Commit` / `Rollback` 返回成功。
  理由：
  1. 前端在断线重连、超时重试、双击时无法可靠区分「已提交」与「从未收到」，幂等把这些竞态
     变成无害操作；返回错误会让用户看到一条无法处置的失败提示；
  2. 与既有的 `Cancel` 语义一致（`docs/api/grpc-conventions.md`「取消与幂等」）；
  3. 「事务必须存在」的真实需求在**写入**路径上：`ApplyRowChanges.transaction_id` 非空而会话
     没有该事务时返回 `transaction.not_found`（`NOT_FOUND`），**绝不退化成自动提交**——
     否则用户以为在事务里，实际已经落库。
  
  测试意图：同一事务 `Commit` 两次 → 两次成功；`ApplyRowChanges(未知 transaction_id)` →
  `transaction.not_found`；失败态 `Commit` → `transaction.failed`。

### 4. 参数化执行（SPI 提前冻结）

- SPI：`DriverSession::execute_parameterized(ParameterizedExecuteRequest { query_id, sql, params: Vec<Value>, max_rows, timeout })`，
  默认实现返回 `capability.unsupported`；`supports_parameterized_statements()` 默认 `false`。
- 本版**不新增**参数化执行的 gRPC RPC：查询侧仍是 `QueryService.Execute`（只接受 SQL 文本，
  由 `QueryService` 的使用者自行承担文本语义），参数化的对外形态是表编辑这一层的
  「值不进 SQL」。查询侧的参数化 RPC 仍留给 S7，届时复用同一 SPI。
- 能力声明：`TableDataProvider::supports_row_edits()` 默认 `false`。驱动必须在驱动侧测试
  （参数绑定往返、影响行数核对、主键定位）通过后才声明为 `true`。

### 5. 主键查询：独立 RPC

`MetadataService.GetPrimaryKey(ObjectRef) → repeated PrimaryKeyColumn{name, position}`，
`position` 是**主键内**顺序（从 1 开始），不是表内列序号。

不采用「给 `ColumnMeta` 加 `primary_key` 布尔标志」的理由：

1. 布尔标志无法表达复合主键的列顺序，而编辑必须按主键顺序提交主键值；
2. `ColumnMeta` 与查询结果集共用（`common/v1/value.proto`），结果集里的列不具备主键语义，
   加标志会让「结果集列是某表主键」成为可被误读的字段；
3. 空列表自然表示「无主键」，不需要额外的 `has_primary_key` 字段与 proto3 默认值博弈。

## 代价

**SPI 变更（对通用层是增量，对驱动是必需工作）**

- `DriverSession` 增加三个默认方法（`table_data()`、`supports_parameterized_statements()`、
  `execute_parameterized()`）和一个新 trait `TableDataProvider`。既有驱动不改一行也能编译，
  但「能编译」不等于「能用」：v0.2.1 的退出标准要求两个驱动显式实现并声明能力。
- `Capabilities` 刻意**没有**扩展：新增字段会让 `drivers/sqlite`、`drivers/mariadb` 现有的
  穷举字面量初始化编译失败，属于跨 Issue 的破坏性改动。能力改由
  `supports_row_edits()` / `supports_parameterized_statements()` 声明。
  **代价**：`SystemService.ListDrivers` 暂时看不到这两项，前端只能从 `BrowseTable` 的
  只读标志判断单个对象是否可编辑。补齐需要一次性改造 `Capabilities` 与两个驱动。

**两个驱动的绑定层代价**（#42 SQLite / #43 MariaDB）

- SQLite（`rusqlite`）：需要为 `Value` 实现 `ToSql`/`FromSql` 的对偶映射；`UInt` 超出 `i64`
  范围、`Decimal`（TEXT/NUMERIC）、`Date`/`Time`/`DateTime`/`Timestamp` 的绑定格式必须与
  现有类型映射表一致，不能借道字符串；主键值比较要按 SQLite 的存储类语义处理。
- MariaDB（`mysql_async`）：`Value` → `mysql_common::Value` 需区分文本/二进制协议；`UInt`、
  `Decimal`（不经 `f64`）、`DATETIME(6)`、`TIME(6)`、`BIT`、`ENUM`/`SET` 不得退化成字符串；
  占位符是 `?`，参数顺序必须与列变更顺序严格对应；一次批次内复用 prepared statement，
  逐行核对影响行数。
- 两者都要新增：参数绑定往返、影响行数不符、行已被删除、主键前缀（不加主键条件）必须 0 行
  的用例。行编辑能力在测试通过前保持 `false`。

**产品与实现**

- 无主键表不能编辑，用户会感知为功能缺口；本 ADR 明确不打算用 rowid 补。
- 键集分页只在 `ORDER BY 主键` 时稳定：翻页期间并发插入/删除会让「下一页」与「上一页」
  不是同一快照。可接受——`OFFSET` 的重复与丢失更难解释。
- 500 行钳制要求前端实现续传循环；未打开事务时，一个批次被钳制意味着「前 500 行已提交」，
  前端必须在提示中说明，并引导用户在事务中提交大批量编辑。

## 重新评估条件

- 出现必须支持无主键表编辑的真实需求：届时评估「全列匹配 + 影响行数必须为 1」的受限定位，
  而不是 rowid / ctid；
- 需要按非主键列排序浏览：排序键必须进 cursor，稳定性下降，需要单独 ADR；
- 驱动实测 prepared statement 在数百行批次下开销不可接受：届时评估多行 `VALUES` 批量语句，
  但参数仍必须绑定，不得拼接；
- `ListDrivers` 必须展示行编辑能力：一次性扩展 `Capabilities` 并同步改两个驱动的字面量；
- 引入 PostgreSQL（S5）时若 `sqlx` 能让两套绑定层合并，按 ADR 0011 的结论重新评估驱动库。
