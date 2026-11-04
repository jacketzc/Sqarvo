# gRPC / Protobuf 约定

## 事实来源

`proto/` 下的 `.proto` 文件是前后端契约的唯一事实来源。任何语言绑定必须由构建过程生成，
生成代码不提交、不手工修改。

## 命名与版本

- package 使用 `sqarvo.<domain>.v1`：`sqarvo.common.v1`、`sqarvo.services.v1`；
- 服务按业务域拆分：`SystemService`、`ConnectionService`、`MetadataService`、
  `QueryService`、`TransactionService`；不要把新操作堆进「万能服务」；
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
- 稳定错误码放在 trailing metadata `sqarvo-error-code`，例如 `driver.not_found`；
- `UNIMPLEMENTED` 表示「该能力尚未实现」，与「执行失败」严格区分；
- 错误信息禁止包含凭据、完整连接串、内部堆栈与私有路径。

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
- 后续批次通过 `FetchBatch(query_id, page_token)` 拉取，token 不透明；
- 元数据浏览使用 `PageRequest`/`PageInfo`，单页默认 100、上限 500；
- 不允许通过提高 gRPC message 大小上限来解决大结果集问题。

## 取消与幂等

- `Cancel` 必须幂等：重复取消不报错；
- 已完成/不存在的查询返回 `NOT_FOUND`；
- 取消后仍可能收到最后一批已产生的数据，前端应忽略。

## 协议演进

- 前端启动时校验 `SystemService.GetVersion.protocol_version` 主版本；
- 破坏性变更必须提升主版本并提供迁移说明；
- 新增字段/服务必须补充契约测试与文档。
