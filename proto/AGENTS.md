# proto/AGENTS.md — 通信契约细则

作用域：`proto/` 全部 `.proto`（`proto/common/v1/`、`proto/services/v1/`）。相关消费方：
`backend/crates/rpc-server`（服务端实现，运行时细则见 `backend/AGENTS.md` §7.3/7.4）、
实际受影响的 Active 前端生成客户端、`tests/contract/`。命名与结构约定真值：`docs/api/grpc-conventions.md`；
结果游标/多语句/会话并发决策：ADR 0005/0006/0008。
契约变更几乎总是跨模块任务：同时读根 §4/§7/§15 与受影响消费方的 AGENTS.md。

## 7.1 服务设计

- 按业务域拆分服务，不做一个万物集合的 Universal Service：ConnectionService / MetadataService / QueryService / TransactionService / DataTransferService / SystemService——实际名称以 `proto/services/v1/` 为准；新增服务前先确认现有域能否扩展。
- 每个 RPC 定义：请求/响应类型、错误语义、取消行为、长任务的进度与终态。
- 错误响应 = 稳定错误码/类别 + 适合展示的安全信息；绝不把数据库密码、可能含密的连接串、内部堆栈返回给 UI。

## 7.2 字段与消息演进（兼容性硬约束）

- 显式 package 与版本名（如 `product.query.v1`）。
- 已发布的字段号**永不复用**；删除字段标 `reserved`。避免不必要的字段类型变更。
- 新增字段必须有合理默认语义，老客户端行为可推理。
- 修改前检查所有调用方：rpc-server 实现、实际受影响的 Active 前端生成客户端、契约测试、`docs/api/`。
- 不兼容变更属根 §15.2 先确认项；改动后重新生成受影响的公共代码与 Active 前端客户端代码并跑契约测试（根 §13）。
- 协议字段承载什么由本目录定义；不把 Rust 内部结构或第三方驱动类型映射进公共消息（根 §4）。

## 7.3 查询结果传输语义（实现侧约束在 `backend/AGENTS.md`）

- 默认禁止把大结果集整体序列化返回。两种访问模式分开定义：
  1. **SQL 查询结果**：Query ID、结果集元数据（列类型、精度、可空性、展示提示）、批量拉取或流式、取消、资源关闭、背压。
  2. **表浏览**：分页/游标拉取；排序与过滤能力显式声明；分页排序稳定性与顺序规则有定义。
- 多语句、多结果集、update count 的协议表达明确（ADR 0006）；超大字段延迟检索。
- 消息大小、并发查询数、缓冲区在契约/配置中有合理上限；大结果问题用流式解决，不得靠无限放大 gRPC 消息上限。

## 7.4 取消与错误语义

- 用户取消必须能传播到驱动（协议层给出取消 RPC/信号语义）；取消幂等，重复取消不得致进程崩溃。
- 错误类别区分：连接丢失、查询失败、超时、取消、权限不足、能力不支持。
- 客户端断开后的 Session / Cursor / 临时文件清理语义有定义，服务端退出时统一回收。
- 错误文案可诊断，但不泄露凭据、完整连接串、私有本机路径（根 §9）。

## 生成代码管理

- 所有语言的 gRPC/Protobuf 代码一律由契约生成；生成文件不手改（根 §15.5）；「顺手修一下生成物」视为缺陷。
- 生成流程（构建期或脚本）改动时同步更新 `docs/api/` 说明。

## MCP 工具面（独立契约，非 gRPC）

- MCP 工具清单与入出参契约：`docs/api/mcp-tools.md`；进程模型：ADR 0032 与 `docs/architecture/mcp.md`；SQL 只读判定：ADR 0033。改动工具面同样要跑契约/回归测试（`backend/apps/sqarvo-mcp/tests/`）。
