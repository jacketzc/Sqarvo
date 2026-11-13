# gRPC / Protobuf 约定

## 事实来源

`proto/` 下的 `.proto` 文件是前后端契约的唯一事实来源。任何语言绑定必须由构建过程生成，
生成代码不提交、不手工修改。

## 命名与版本

- package 使用 `sqarvo.<domain>.v1`：`sqarvo.common.v1`、`sqarvo.services.v1`；
- 服务按业务域拆分：`SystemService`、`ConnectionService`、`CredentialService`、
  `MetadataService`、`QueryService`、`TransactionService`、`TableDataService`；
  不要把新操作堆进「万能服务」；
- 不删除或复用已发布的字段编号；删除字段时使用 `reserved`；
- 新增字段必须有合理默认语义（proto3 默认值必须被解释为「未设置」，不能与业务默认值冲突）。

## 类型映射

| Protobuf | 语义 |
|---|---|
| `VALUE_KIND_NULL` | SQL NULL，`value` oneof 为空 |
| `DecimalValue` | `unscaled_value * 10^-scale`，绝不使用 `double` |
| `DateTimeValue` | 本地日期/时间分量（DATE / TIME / DATETIME） |
| `TimestampValue` | UTC 纳秒 + 显示偏移（TIMESTAMPTZ） |
| `ColumnValue.truncated` | 大字段被截断，前端必须提示 |
| `ColumnValue.original_type_name` | 数据库原始类型名，始终保留 |

## 错误语义

- 错误通过 gRPC status 返回：状态码表示分类，`status message` 是**可直接展示**的中文文案；
- 稳定错误码放在 trailing metadata `sqarvo-error-code`，例如 `driver.not_found`、
  `query.limit_reached`、`transaction.failed`、`capability.unsupported`；
- `UNIMPLEMENTED` 表示「该能力尚未实现」，与「执行失败」严格区分；
- 错误信息禁止包含凭据、完整连接串、内部堆栈与私有路径。

凭据相关错误（ADR 0012 / 0015 / 0016）：

| 错误码 | gRPC 状态码 | 含义 |
|---|---|---|
| `credential.not_found` | `NOT_FOUND` | 配置里的引用在系统凭据库中不存在（密码已被用户删除） |
| `credential.unavailable` | `UNAVAILABLE` / `FAILED_PRECONDITION` | 系统凭据库不可用、已锁定或用户拒绝授权；**永不回退明文** |
| `config.invalid` | `INVALID_ARGUMENT` | 引用格式非法，或 `StoreCredential` 提交了空密码 |

`CredentialService.StoreCredential` 是唯一携带数据库密码的 RPC：密码只在请求体内存在，
不得写入日志、错误文案、端点文件或配置；连接配置只保存它返回的 `credential_ref`。

表数据与事务（ADR 0017）：

| 错误码 | gRPC 状态码 | 含义 |
|---|---|---|
| `table_data.no_primary_key` | `FAILED_PRECONDITION` | 表没有主键：无法稳定定位行，编辑被拒绝；浏览仍可用但为只读且无稳定排序 |
| `table_data.read_only` | `FAILED_PRECONDITION` | 目标对象只读（视图、驱动只读模式，或驱动未声明行编辑能力） |
| `table_data.cursor_invalid` | `INVALID_ARGUMENT` | 浏览游标非法、被篡改或与目标表/排序不匹配；**不得**回退到第一页 |
| `table_data.rows_affected_mismatch` | `FAILED_PRECONDITION` | 影响行数与 `expected_rows_affected` 不符（乐观并发冲突） |
| `table_data.row_deleted` | `NOT_FOUND` | 目标行已不存在（已被其它会话删除） |
| `table_data.confirmation_required` | `FAILED_PRECONDITION` | 预计影响行数超过风险阈值（默认 100 行）且请求未带 `acknowledge_risk` |
| `transaction.not_found` | `NOT_FOUND` | 写入指向的事务不存在或已结束；**不得**退化为自动提交 |
| `transaction.already_active` | `ALREADY_EXISTS` | 会话已有打开的事务（一个会话同时最多一个） |
| `transaction.failed` | `FAILED_PRECONDITION` | 事务处于失败态，只能回滚 |
| `query.id_in_use` | `ALREADY_EXISTS` | 行变更批次的 `query_id` 已有在途批次，同一 ID 不得并发复用（`table_data.proto:304`、`common/src/error.rs:95`；ADR 0018） |

对象树、DDL 与插入行（ADR 0019，v0.2.2）：

| 错误码 | gRPC 状态码 | 含义 |
|---|---|---|
| `metadata.object_not_found` | `NOT_FOUND` | 目标对象不存在（表/视图/索引已删除或名字拼错） |
| `metadata.object_kind_unsupported` | `INVALID_ARGUMENT` | 对象种类不能用于该操作（对列取 DDL、把表当 schema 的父节点）；与「驱动尚未实现该层级」不同 |
| `metadata.permission_denied` | `PERMISSION_DENIED` | 数据库拒绝读取元数据（`information_schema` 里不可见）；**不得**降级成 `object_not_found` |
| `metadata.filter_invalid` | `INVALID_ARGUMENT` | 过滤条件非法：`contains` 与 `exact` 同时给、`kinds` 重复或含 `UNSPECIFIED`、匹配串为空 |
| `table_data.unique_violation` | `ALREADY_EXISTS` | 插入违反唯一约束/主键（逐行，不中断整批） |
| `table_data.not_null_violation` | `INVALID_ARGUMENT` | 显式 NULL 写进 NOT NULL 列 |
| `table_data.not_null_without_default` | `INVALID_ARGUMENT` | 省略 NOT NULL 列且该列没有默认值可兜底 |
| `table_data.column_not_insertable` | `INVALID_ARGUMENT` | 列由数据库生成（`GENERATED ALWAYS` 身份列、计算列）；普通 `AUTO_INCREMENT` 不在此列 |
| `table_data.permission_denied` | `PERMISSION_DENIED` | 缺少 INSERT 权限 |
| `table_data.truncated_value` | `INVALID_ARGUMENT` | 提交的是被截断的前缀值，不能写回（`table_data.proto:380`、`rpc-server/src/services/mapping.rs:199-206`；ADR 0007） |

两条 NOT NULL 错误**不能合并**：`not_null_violation` 的处置是「去掉这一列或改值」，
`not_null_without_default` 的处置是「补一个值」，前端提示必须不同。

「按行失败」与「整批失败」的分工：插入的逐行错误走 `InsertRowResult.error_code`（response 内），
只有**整批无法执行**的情形（无权限、对象不存在、事务不存在、驱动未实现）才用 `Err` 返回上面的码。

`BrowseTable.read_only_reason` 取值（枚举**只追加**，不复用数字、不重排）：

| 取值 | 数值 | 含义 | 前端处置 |
|---|---|---|---|
| `READ_ONLY_REASON_UNSPECIFIED` | 0 | 可编辑（`read_only = false`） | 单元格可进入编辑态 |
| `READ_ONLY_REASON_NO_PRIMARY_KEY` | 1 | 表/视图没有主键，无法稳定定位行；`ordering` 为 `ROW_ORDER_UNSTABLE` | 显示「无主键」文案，禁用编辑 |
| `READ_ONLY_REASON_DRIVER_UNSUPPORTED` | 2 | 驱动未声明行编辑能力（`capability.unsupported`） | 显示「驱动不支持」文案，禁用编辑 |
| `READ_ONLY_REASON_CONNECTION_READ_ONLY` | 3 | 连接以只读方式打开（SQLite `read_only`），写入必被拒 | 显示「连接只读」文案，禁用编辑；提示改用可写连接 |

每条原因都必须有**对应文案**，不能只说「只读」：三种原因的用户处置完全不同（换表 / 换驱动 /
换连接）。前端只翻译服务端给出的枚举，不自行猜测；旧客户端遇到不认识的取值会落到
`UNSPECIFIED`，此时仍以 `read_only = true` 为准禁用编辑，不会显示可编辑。

重复 `Commit` / `Rollback` 是幂等的：对已结束或未知事务返回 `OK`（见 ADR 0017 §3）。
「事务必须存在」的约束落在写入路径（`table_data` 的 `transaction_id`）上。

`RowChangeResult.rows_affected` 的取值（ADR 0017 补记，#68）：

| `outcome` | `rows_affected` | 说明 |
|---|---|---|
| `CHANGE_OUTCOME_APPLIED` | 应用行数（= `expected_rows_affected`） | 唯一算成功的取值 |
| `CHANGE_OUTCOME_CONFLICT` | 驱动**实际**影响的行数，可能 `> 1` | 判定为冲突；`> 1` 表示主键匹配到多行（主键定义损坏/不唯一） |
| `CHANGE_OUTCOME_ROW_NOT_FOUND` | `0` | 该分支的判定条件就是实际影响 0 行 |

`total_rows_affected` = Σ 逐行 `rows_affected`，**包含**冲突行（逐行冲突只改变 `outcome`，
不回滚已执行的 DML）。因此「成功与否」只能看 `outcome`，不能用 `rows_affected` 反推；
前端展示「已提交 N 行 · 失败 M 行」时，N 是驱动实际改动行数，M 是失败条数，两者不互斥。

会话上的事务状态通过既有 `ConnectionService.ListSessions` 暴露（#44）：
`SessionSummary.transaction`（`TransactionStatus`：`transaction_id` / `isolation` /
`read_only` / `state`）在每次读取时现取，未打开事务时为空。该字段与
`TransactionInfo.state` 都是**追加**字段，不改动既有字段号与语义；前端据此展示事务状态，
不需要新增独立 RPC。断连由后端强制回滚，清理结果计入进程内计数并以日志记录。

分类映射（后端 `ErrorCategory` → gRPC code）：

| 分类 | gRPC 状态码 |
|---|---|
| invalid_argument | `INVALID_ARGUMENT` |
| not_found | `NOT_FOUND` |
| already_exists | `ALREADY_EXISTS` |
| failed_precondition | `FAILED_PRECONDITION` |
| unsupported | `UNIMPLEMENTED` |
| unauthenticated | `UNAUTHENTICATED` |
| permission_denied | `PERMISSION_DENIED` |
| resource_exhausted | `RESOURCE_EXHAUSTED` |
| cancelled | `CANCELLED` |
| timeout | `DEADLINE_EXCEEDED` |
| unavailable | `UNAVAILABLE` |
| internal | `INTERNAL` |

### 错误码的分层（proto 之外的码）

上面各表登记的是**服务端返回**的稳定错误码：它们不在 `proto/` 契约里
（AGENTS.md §4.3：Protobuf 是前后端通信契约的唯一真值），而是放在 gRPC
trailing metadata `sqarvo-error-code`（`proto/common/v1/common.proto:7`、
`frontend/windows/Sqarvo.Core/Protocol/RpcErrorMapper.cs:18`）；前端
`failure_from_status` 读该键再按码分支（`frontend/linux/src/backend/query.rs:285-300`）。
此外还有一批码由**客户端本地构造**或**驱动告警**产生，同样不属于 proto 契约，
按下表分层（Linux 前端路径相对 `frontend/linux/src/backend/`，后端相对 `backend/crates/`）：

| 类别 | 错误码 | 产生位置与去向 |
|---|---|---|
| 服务端返回 | `session.not_found`、`query.not_found`、`query.cancelled`、`query.limit_reached`、`transport.failed` | 后端常量 `common/src/error.rs:87,89,91,93,161`；经 trailing metadata `sqarvo-error-code` 返回 |
| 客户端本地构造 | `table_data.insert_failed` | `table.rs:536`：逐行错误码为空时的本地兜底 |
| 客户端本地构造 | `table_data.not_open` | `worker.rs:2232`：未打开表时的本地事件 |
| 客户端本地构造 | `backend.unavailable` | `query.rs:264`、`table.rs:743`、`connection.rs:528`：`UNAVAILABLE` 的本地分类 |
| 客户端本地构造 | `transport.timeout` | `SqarvoFailure.cs:78`（Windows）、`connection.rs:441`：连接超时的本地映射 |
| 客户端本地构造 | `query.timeout` | `query.rs:261`：超时的本地分类 |
| 客户端本地构造 | `result.limit_reached` | `query.rs:262`、:204-207：前端为保护内存主动停止拉取，明确「**不是服务端错误**」 |
| 驱动告警 | `mariadb.*`：`mariadb.auth_failed`、`mariadb.connect_timeout`、`mariadb.transaction_lost` 等 | 驱动侧稳定码，作为 `sqarvo-error-code` 返回（`connection.rs:520`、`SqarvoFailure.cs:72,79`、`connection/src/manager.rs:1058`） |
| 驱动告警 | `mariadb.tls_fallback` 等 | 非致命告警走响应内 `Warning`（`common.proto:44-47`、`query.proto:46`；`mariadb.md:673` 的输出里即 `Warning { code: "mariadb.tls_fallback" }`），前端 `client.rs:443-447` 格式化展示，不得丢弃 |

`session.not_found` / `query.not_found` / `query.cancelled` / `transport.failed` 在前端也有
同名的本地分类兜底（`query.rs:257-264`、`table.rs:742-743`），但主来源是服务端 trailer；
上表按**主要来源**归类，避免把这些码误读成 proto 字段或新增契约。

`sqarvo-failed-statement=<index>` 仅见于 `docs/decisions/0006-multi-statement-policy.md:25`
（全仓仅此一处），是 **ADR 规划项，当前未实现**，不是现有契约；v0.1 多语句本就被
`capability.unsupported` 拒绝（ADR 0006:26-27），不会走到这条 metadata。

## 认证

所有 RPC 必须携带 metadata `x-sqarvo-token`；缺失或错误返回 `UNAUTHENTICATED`。
token 来自端点描述文件（0600），详见 [本地 IPC](../architecture/local-ipc.md)。

## 驱动能力位（SystemService.ListDrivers）

`DriverInfo.capabilities`（`proto/services/v1/system.proto:52`）是字符串列表；
`system.proto:49-50` 只举例 4 项，完整清单以 `Capabilities::names()`
（`backend/crates/driver-api/src/capabilities.rs:65-104`）的 12 项为准，含义取自
同文件各字段注释（:15-59）：

| 能力名 | 含义 |
|---|---|
| `transactions` | 支持显式事务 |
| `query_cancellation` | 支持取消正在执行的查询 |
| `streaming_results` | 支持分批 / 流式读取结果集 |
| `multiple_statements` | 支持一次提交多条语句（ADR 0006） |
| `ddl_generation` | 支持生成 DDL |
| `server_side_pagination` | 支持服务端分页（而不是拉全量后切分） |
| `catalogs` | 存在 Catalog 层级 |
| `schemas` | 存在 Schema 层级 |
| `row_editing` | 支持行编辑：`ApplyRowChanges` 可提交 update / delete（ADR 0017） |
| `parameterized_statements` | 支持参数化语句：值经绑定占位符进入语句，不拼进 SQL 文本 |
| `tls_verify_ca_chain_only` | 支持「只校链、不校主机名」的 TLS 模式（ADR 0020） |
| `table_browse_sort_filter_total` | 表浏览支持排序、筛选与精确总数（ADR 0022） |

名称是稳定标识，**只追加**：不改名、不改变既有项含义、既有顺序不动
（`system.proto:50-51`、`capabilities.rs:128`）；前端按「未出现即不支持」解释
（`system.proto:51-52`），不得尝试执行后再判断。
`table_browse_sort_filter_total` 在 `Capabilities::default()` 里仍为 `false`
（`capabilities.rs:57-59,171-175`），但两个内置驱动已翻为 `true`：
`sqlite/src/driver.rs:65`、`mariadb/src/driver.rs:70`（#110 / #111）。

## 结果集与分页

- `QueryService.Execute` 只返回首批数据（默认 200 行，单批上限 5000 行）——**当前实现默认值，
  非 proto 契约**：`query.proto:22-23` 只写「0 表示服务端默认值，超过上限压到上限」，未写死数字；
- `ExecuteQueryResponse.statements[]` / `StatementSummary`（`query.proto:29-47`）在**当前单语句
  下已实际接线，不是「闲置」**：后端 `rpc-server/src/services/mapping.rs:53,75-86` 把
  `ExecutionStart.statements` 逐条映射，`ExecutionStart::single` 只填 1 条
  （`driver-api/src/driver.rs:285-287`），`statement_index` 恒为 0
  （`rpc-server/tests/grpc_sqlite_e2e.rs:272`）；Linux `frontend/linux/src/backend/client.rs:439`
  与 Windows `frontend/windows/Sqarvo.Core/Clients/QueryHandle.cs:55` 都读取该字段，三端已接线。
  ADR 0006 定 **v0.1 单语句约束**：检测到多语句返回 `capability.unsupported`（:26-27），
  多语句能力由 `multiple_statements` 能力位声明，长期方案才让 `statements[]` 承载多条；
- 后续批次通过 `FetchBatch(query_id, page_token, max_rows, statement_index)` 拉取
  （`query.proto:49-55`：字段号 1 / 2 / 3 / 4，按此顺序），token 不透明；
- `ExecuteQueryRequest.timeout_millis` 为执行超时，0 表示由服务端默认策略决定；
- `FetchBatchRequest.statement_index` 标识结果集所属语句，v0.1 恒为 0（单语句）；
- 元数据浏览使用 `PageRequest`/`PageInfo`，单页默认 100、上限 500——**当前实现默认值，
  非 proto 契约**：`common.proto:27-28` 只写「0 表示使用服务端默认值」，未写死数字；
- 表数据浏览（`TableDataService.BrowseTable`）默认按主键升序，支持 **双模分页并列一等**
  （`table_data.proto:13-18`，ADR 0022）：不透明 `page_token` 的 cursor 连续翻页与 `offset`
  页码跳页是两种并列方式，**不是**「只保留 v0.2.2 的 cursor 兼容路径」。
  互斥只发生在 `offset > 0` 且 `page_token` 非空时，返回 `config.invalid`
  （`table_data.proto:161-163,166-167`；`rpc-server/src/services/table_data.rs:42,807,1162`
  也只列 offset 与 page_token 并用）；`sort` 与 `page_token` **是合法组合**——文件头
  :17-18 只是说「不带 sort、用 page_token 的连续翻页才稳定」，带 `sort` 仍可用 token，
  但页间稳定性取决于排序列是否唯一（`table_data.proto:87-90`）；
- 无主键表返回 `read_only=true` + `ordering=ROW_ORDER_UNSTABLE`（排序 / 筛选仍可用）；
- 行变更单批上限 500 行，超限由服务端钳制并在响应里给出 `clamped` / `deferred_changes`；
- 不允许通过提高 gRPC message 大小上限来解决大结果集问题。

## 对象树与搜索（v0.2.2，ADR 0019）

> 本节即 `proto/services/v1/metadata.proto:132` 引用的「对象层级」小节（标题沿用「对象树与
> 搜索」），该处对目录层请求方式的引用指向本节；不改 proto。

- `ListNodes` **逐层懒加载**：`parent` 为空表示根（当前连接库的对象）；一次只返回直接子节点，
  禁止一次拉整棵树；
- **目录层怎么请求**（v0.2.2，#82）：`parent.kind = OBJECT_KIND_CATALOG` 且不带名字
  （`catalog` / `schema` / `name` 全空）表示「列出全部 Catalog」；带名字表示展开该目录。
  目录节点的 `catalog` 与 `name` **都**是库名，客户端把节点引用原样回传即可展开。
  该读法已在 `proto/services/v1/metadata.proto:130-132` **明确写入**（`ca0df3c`，#102）：
  `parent.kind = OBJECT_KIND_CATALOG` 且 `catalog` / `schema` / `name` 全空即「列出全部
  Catalog」，不再是「待定」。冻结的 `ObjectKind` 注释（:27-57）只描述层级展开，
  独立表达方式写在 `ListNodesRequest.parent` 的字段注释里；
- **子节点顺序**：先按种类（`ObjectKind` 声明顺序）、再按名字。v0.2.1 的纯字母序不再是契约，
  客户端不要自己重排（会让分页顺序与 token 语义不一致）；
- 层级由 `ObjectKind` + `ObjectRef(catalog, schema, name)` 共同表达，展开规则写在
  `proto/services/v1/metadata.proto` 的 `ObjectKind` 注释里（CATALOG → SCHEMA/TABLE…，
  SCHEMA → TABLE/VIEW/…，TABLE → COLUMN/INDEX/TRIGGER，VIEW → COLUMN，其余为叶子）；
- **跨库/跨 schema 必须显式**：`catalog` / `schema` 为空只表示「该数据库没有这一层」，
  服务端不得隐式切到连接默认库；
- 未实现的层级返回 `capability.unsupported`（`UNIMPLEMENTED`），**不得**返回空列表：
  空列表的唯一含义是「这一层确实没有对象」；
- 过滤字段（`ListNodesRequest.filter`，新增 3 号）：

| 条件 | 表达 | 说明 |
|---|---|---|
| 名称包含 | `filter.name.contains` | `%` / `_` 按字面量匹配（驱动转义），客户端不拼通配符 |
| 名称精确 | `filter.name.exact` | 与 `contains` 互斥（同一 oneof） |
| 种类 | `filter.kinds` | 集合语义，顺序无关；空表示不过滤 |

- 名称与种类取交集；过滤**必须下推**到数据库（SQLite `LIKE ... ESCAPE`、MariaDB
  `information_schema` 的 `LIKE`），不做「全量读进内存再过滤」；
- **大小写敏感性跟随数据库**，契约不提供开关：同一关键词在 SQLite 与 MariaDB 上可能给出不同
  结果，前端不得再叠加一层过滤来「统一」；
- 过滤条件的规范化字符串（`name=<contains|exact>:<原文>` + `kinds=<排序后的种类>`）进入分页
  token 的指纹：换条件后旧 token 必须失效（`config.invalid`），不得拿旧 token 继续翻页；
- **服务端不得静默忽略非空过滤**：驱动未实现下推时返回 `capability.unsupported`。
  #82 起 rpc-server 已把非空 `filter` 接线到 SPI 的 `list_children_filtered`：结构非法的过滤
  （空匹配串、`kinds` 重复或含 `OBJECT_KIND_UNSPECIFIED`、未知枚举值）先报
  `metadata.filter_invalid`；**未实现下推的驱动**（例如将来新增的）返回
  `UNIMPLEMENTED` + `capability.unsupported`，绝不静默返回未过滤列表。两个内置驱动自
  #84 起已下推（SQLite `LIKE ... ESCAPE`、MariaDB `information_schema` 的 `LIKE`）；
- **过滤结果的分页 token** 是 `v2:<offset>:<集合指纹>:<过滤标签>`（不透明字符串，客户端
  只回传）。标签来自 `ObjectFilter::fingerprint_input()` 的规范化字符串：换关键词、换
  `contains`/`exact`、增删 `kinds` 都会让旧 token 报 `config.invalid`（`INVALID_ARGUMENT`），
  客户端必须从第一页重搜；结果集自身变化仍是 `metadata.snapshot_changed`。
  空 `filter` 走与 `list_children` 相同的路径（token 仍是 `v1:`）。

## DDL 查看（v0.2.2，ADR 0019）

`GetDdl(GetDdlRequest) → GetDdlResponse{ddl, exact, source, source_detail, generator_version}`：

| 来源 | `exact` | `source` | `source_detail` | `generator_version` |
|---|---|---|---|---|
| SQLite `sqlite_master.sql` | `true` | `DDL_SOURCE_DATABASE` | 必有，例如 `sqlite_master.sql` | 空 |
| MariaDB `SHOW CREATE TABLE/VIEW/SEQUENCE` | `true` | `DDL_SOURCE_DATABASE` | 必有，例如 `SHOW CREATE TABLE` | 空 |
| 服务端按元数据拼装 | `false` | `DDL_SOURCE_SYNTHESIZED` | **必有**：说明缺了什么 | **必有**：生成器版本 |

- 不变式：`exact == (source == DDL_SOURCE_DATABASE)`；拼装却说不清来源/版本的结果，rpc-server
  以 `internal.error` 拒绝返回（不能把「看不出是拼装的 DDL」当原文交给用户）；
- `source = DDL_SOURCE_UNSPECIFIED` 表示**未说明**，客户端只能退回看 `exact`，不得假定原文；
- 只有数据库给不出原文时才拼装；能拿到原文就必须给原文（拼装永远有信息损失）；
- **v0.2.2 的实现没有拼装路径**：两驱动都只返回原文，因此 `exact` 恒为 `true`、
  `generator_version` 恒为空。拿不到原文的对象（SQLite 隐式索引、MariaDB 索引/触发器）
  返回 `metadata.object_kind_unsupported` 或 `capability.unsupported`，**不返回空串、
  不伪造 DDL**——`Ddl::synthesized` 目前只有契约层用例覆盖；
- **不截断**：`GetDdlResponse` 没有 `truncated` 字段，截断后调用方无法判断（不能截了还说
  `exact = true` 全量），因此 DDL 原样返回。ADR 0007 的 64 KiB 上限只约束单元格；
  实际可用长度由 gRPC 消息上限（tonic 默认 4 MiB）决定，超限时请求失败而非返回半截文本；
- 错误码：对象不存在 → `metadata.object_not_found`（`NOT_FOUND`）；种类不适用
  （列、MariaDB 索引/触发器）→ `metadata.object_kind_unsupported`（`INVALID_ARGUMENT`）；
  数据库拒绝读取 → `metadata.permission_denied`（`PERMISSION_DENIED`）；
- `GetDdl` 纳入会话执行锁（ADR 0008），与浏览、写入共用同一条物理连接。

## 插入新行（v0.2.2，ADR 0019）

`TableDataService.InsertRows(InsertRowsRequest) → InsertRowsResponse`：

- **单行插入 = `rows` 只有一条**，不另立 RPC：单行与批量必须共用风险阈值与原子性；
- **省略列 ≠ NULL**：不在 `values` 里的列交给数据库默认值（自增、`DEFAULT` 表达式、
  `DEFAULT NULL`）；`VALUE_KIND_NULL` 是显式写 SQL NULL。服务端不把「省略」改写成默认值字面量；
- **主键回传**：`InsertRowResult.key` 是插入后的**完整**主键值（自增/默认值生成的主键列取真实值），
  顺序同 `GetPrimaryKey`；驱动拿不到完整主键就不能声明支持插入（不得部分回传）；
- **原子性**：与 `ApplyRowChanges` 同一信封——无 `transaction_id` 时整批成功才提交（一个隐式
  事务），指定事务时用 `SAVEPOINT`；任一行失败 → 继续执行完其余行以报全失败，然后整批回滚，
  `rolled_back = true`、`total_rows_affected = 0`、逐行结果仍然返回；
- 前端判成功**必须**看 `rolled_back`，不能只看逐行 `INSERT_OUTCOME_INSERTED`；
  `inserted_rows` 是「语句层成功了几行」，`total_rows_affected` 是「库里的净变化」；
- **风险阈值**：沿用 `acknowledge_risk` 与 `DEFAULT_RISK_CONFIRMATION_ROWS = 100`；
  预计影响行数 = 本批行数（插入恒 1 行/行）。超过阈值未确认 → `table_data.confirmation_required`
  （整批拒绝，一行都不执行）。**不另立插入专用阈值**；
- **批次上限 500**（与行变更同值）：超限钳制，回 `clamped` / `deferred_rows`，客户端必须续传；
- **本版不可取消**：没有 `query_id` 字段；后续需要时追加字段并复用 `CancelRowChanges`；
- 事务指向不存在/已结束的事务 → `transaction.not_found`，绝不退化成自动提交；
- 驱动未实现插入 → `capability.unsupported`（`UNIMPLEMENTED`），绝不返回「插入了 0 行」。

## 单元格与截断

- 每个单元格带 `truncated` 标记；文本/二进制默认上限 64 KiB（ADR 0007）；
- 截断值仍是原类型（TEXT/BLOB），前端必须显示「已截断」状态；
- 无法无损映射的数据库类型使用 `VALUE_KIND_UNSUPPORTED` 并保留原始类型名，
  不得伪装成 TEXT 或其它类型。

## 取消与幂等

- `QueryService.Cancel`（`query.proto:15`）必须幂等：重复取消不报错；
- 已完成、已取消或不存在的查询返回成功（`OK`），避免前端重复取消变成错误提示；
- 取消后仍可能收到最后一批已产生的数据，前端应忽略；
- `QueryService.Close`（`query.proto:16`）是与 `Cancel` 并列的独立 RPC（文件头 :8
  「取消、关闭」；`CancelQueryRequest` / `CloseQueryRequest` 都只带 `query_id`，:65-71）；
  前端 `QueryHandle.cs:51` 注明 `Cancel` / `Close` 都需要 `QueryId`；
- 行变更批次的取消走独立的 `TableDataService.CancelRowChanges`（`table_data.proto:57`），
  **不复用** `QueryService.Cancel`（`table_data.proto:43-44`）；它同样幂等——重复取消、
  取消已完成 / 不存在的批次都返回成功（`table_data.proto:48,359-363`），驱动未实现取消时
  返回 `capability.unsupported`（`table_data.proto:56`）。

## 列元数据的存在性语义

`common.v1.ColumnMeta` 的扩展字段（`default_value` / `character_set` / `collation`）
用 proto3 的 presence 表达「未设置」，它**不等于**「值为空」：

| 字段状态 | 含义 | 前端必须 |
|---|---|---|
| `default_value` 未设置 | 未知：老服务端不提供，或该列来自结果集（结果集没有默认值信息） | 显示「未知」，不得显示为「无默认值」 |
| `default_value.kind = COLUMN_DEFAULT_NONE` | 明确没有默认值 | 显示「无默认值」 |
| `default_value.kind = COLUMN_DEFAULT_NULL` | 默认值是 SQL NULL | 显示「默认 NULL」，与空串区分 |
| `default_value.kind = COLUMN_DEFAULT_EXPRESSION` | 未求值的默认值文本（`expression`） | 当表达式展示，不求值、不推断类型 |
| `character_set` / `collation` 未设置 | 不适用（例如 SQLite 没有列级字符集）或未知 | 显示「不适用」，不得显示空字符集 |

理由：默认值、字符集、排序规则在数据库之间的可读性差异很大（AGENTS.md §6.2）。用空串
顶替「没有默认值」会让「默认空串」与「没有默认值」不可区分；用空串顶替「不适用」
会让前端把 SQLite 显示成「字符集为空」，两者都是伪造事实。

## 协议演进

- 前端启动时校验 `SystemService.GetVersion.protocol_version` 主版本；
- 破坏性变更必须提升主版本并提供迁移说明；
- 新增字段/服务必须补充契约测试与文档。
