# ADR 0009：SQLite 驱动选型与类型策略

- 状态：已接受
- 日期：2026-11-04
- 关联：Issue #9–#15；风险 R-03、R-04

## 背景

v0.1 需要第一个真实驱动来验证 SPI 与 gRPC 全链路。SQLite 无服务器、无凭据，
适合作为基线；但其类型系统与游标语义有特殊性，选型会固化实现模型。

## 驱动库备选

| 方案 | 优点 | 问题 |
|---|---|---|
| `rusqlite`（bundled） | 同步、API 贴近 SQLite、可控、依赖小 | 需要自行处理阻塞与游标生命周期 |
| `sqlx` SQLite | 异步接口、连接池、流式 RowStream | 依赖树大，行为封装多，游标控制受限 |
| 直接 `libsqlite3-sys` | 最大控制力 | 需要大量 unsafe，违背最小 unsafe 原则 |

## 决策

采用 `rusqlite`（`bundled` feature，避免系统库差异），配合**每会话一个专用阻塞线程（actor）**：

- actor 持有 `Connection`、当前语句与游标；命令通过有界 channel 进入；
- 这解决了 `Statement` 借用 `Connection` 导致的游标生命周期问题（不能跨 `spawn_blocking` 调用持有）；
- 取消通过 `Connection::get_interrupt_handle` 跨线程触发，不经过执行锁；
- 阻塞线程不占用 Tokio worker（符合 AGENTS.md 6.4）。

### 类型策略

SQLite 只有 5 种存储类，声明类型与实际存储可能不一致：

- 按实际存储类映射：NULL / INTEGER → `Int` / REAL → `Float` / TEXT → `Text` / BLOB → `Bytes`；
- 声明类型写入 `ColumnMeta.original_type_name`；
- `NUMERIC`/`DECIMAL` 若以 REAL 存储则映射为 `Float`，**不得伪造 `Decimal`**；
- `DATE`/`DATETIME` 以 TEXT 存储时返回 `Text` + 原始类型名，**不猜测解析**；
- 超出 64 KiB 的文本/二进制按 ADR 0007 截断并置标记。

### 能力声明（v0.1）

- `transactions = true`（SQLite 支持，但事务 RPC 接线在 M4 完成；SPI 接口先提供）
- `streaming_results = true`（游标分批）
- `query_cancellation = true`
- `multiple_statements = false`
- `ddl_generation = false`、`catalogs = false`、`schemas = false`
- 元数据能力在 v0.1 不实现（`metadata()` 返回 `None`）

## 取舍

- 自建 actor 增加代码量，但换来确定的游标与取消语义；
- 单连接意味着无并发（ADR 0008）；
- 类型映射会暴露 SQLite 的能力缺陷，这是诚实且有价值的：SPI 必须能表达「无法无损映射」。

## 重新评估条件

- `sqlx` 提供可控的游标与取消语义，且依赖成本可接受；
- 需要连接池或并发读（届时评估多连接模型）；
- SQLite 官方扩展类型（JSONB 等）需要在 v0.2 之后纳入映射。
