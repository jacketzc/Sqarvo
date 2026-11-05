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

重复 `Commit` / `Rollback` 是幂等的：对已结束或未知事务返回 `OK`（见 ADR 0017 §3）。
「事务必须存在」的约束落在写入路径（`table_data` 的 `transaction_id`）上。

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

## 认证

所有 RPC 必须携带 metadata `x-sqarvo-token`；缺失或错误返回 `UNAUTHENTICATED`。
token 来自端点描述文件（0600），详见 [本地 IPC](../architecture/local-ipc.md)。

## 结果集与分页

- `QueryService.Execute` 只返回首批数据（默认 200 行，单批上限 5000 行）；
- 后续批次通过 `FetchBatch(query_id, statement_index, page_token)` 拉取，token 不透明；
- `ExecuteQueryRequest.timeout_millis` 为执行超时，0 表示由服务端默认策略决定；
- `FetchBatchRequest.statement_index` 标识结果集所属语句，v0.1 恒为 0（单语句）；
- 元数据浏览使用 `PageRequest`/`PageInfo`，单页默认 100、上限 500；
- 表数据浏览（`TableDataService.BrowseTable`）固定按主键升序，使用同语义的不透明
  `page_token`（ADR 0017）：默认 100、上限 500、超限钳制；无主键表返回
  `read_only=true` + `ordering=ROW_ORDER_UNSTABLE`，不得改用 `OFFSET` 分页；
- 行变更单批上限 500 行，超限由服务端钳制并在响应里给出 `clamped` / `deferred_changes`；
- 不允许通过提高 gRPC message 大小上限来解决大结果集问题。

## 单元格与截断

- 每个单元格带 `truncated` 标记；文本/二进制默认上限 64 KiB（ADR 0007）；
- 截断值仍是原类型（TEXT/BLOB），前端必须显示「已截断」状态；
- 无法无损映射的数据库类型使用 `VALUE_KIND_UNSUPPORTED` 并保留原始类型名，
  不得伪装成 TEXT 或其它类型。

## 取消与幂等

- `Cancel` 必须幂等：重复取消不报错；
- 已完成、已取消或不存在的查询返回成功（`OK`），避免前端重复取消变成错误提示；
- 取消后仍可能收到最后一批已产生的数据，前端应忽略。

## 协议演进

- 前端启动时校验 `SystemService.GetVersion.protocol_version` 主版本；
- 破坏性变更必须提升主版本并提供迁移说明；
- 新增字段/服务必须补充契约测试与文档。
