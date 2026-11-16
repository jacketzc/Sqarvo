# ADR 0006：多语句执行与错误策略

- 状态：已接受
- 日期：2026-11-04
- 关联：Issue #1、#4；风险 R-02

## 背景

`ExecuteQueryResponse` 已定义 `statements[]`，但 SPI、错误语义与「第 N 条失败后怎么办」
尚未确定。不同策略会影响用户对事务与结果完整性的预期。

## 备选方案

1. **遇错继续执行剩余语句**：一次拿到尽可能多的结果，但用户可能在不知情时执行了后续写操作。
2. **遇错立即停止，返回已完成语句与失败位置**：可预测，但需要定义错误如何携带部分结果。
3. **v0.1 只允许单语句**：范围最小，但契约中的 `statements[]` 暂时闲置。

## 决策

长期采用方案 2，v0.1 采用方案 3：

- 语句按顺序执行；任一语句失败即停止，剩余语句不执行，也不回滚已执行的语句
  （是否回滚取决于事务，不在本层隐式处理）；
- 失败时返回 `FAILED_PRECONDITION`（或更精确分类），在 trailing metadata 写入
  `sqarvo-failed-statement=<index>`，已完成语句的结果集由后端负责关闭；
- **v0.1 的 SQLite 驱动只支持单语句**：检测到多条语句（分号后仍有有效内容）时返回
  `capability.unsupported`，绝不静默只执行第一条；
- 多语句能力由驱动的 `multiple_statements` 能力位声明，前端据此决定是否允许多语句执行。

## 取舍

- v0.1 无法一次执行脚本，用户需逐条执行；换来的是不会出现「执行了一半」的歧义。
- 失败时部分结果不返回给前端，只在 metadata 中给出失败序号；后续可在错误详情中
  携带已完成语句摘要（需要扩展 proto，届时走增量字段）。
- 不隐式回滚：事务边界由用户控制，避免客户端替用户做决定。

## 重新评估条件

- 用户需要「执行脚本」工作流，且前端能清楚展示每条语句结果；
- 驱动（如 PostgreSQL simple query）天然返回多结果集，届时需要重新定义错误携带方式。

## 修订（2026-11-16，Issue #187）

重新评估条件已满足：用户需要「执行脚本」工作流。本节记录对 v0.1 限制的解除与
落地细节；**原决策文本保留不变**，长期策略（方案 2：遇错即停、不隐式回滚）正是
本次实现的行为。

### 解除 v0.1 单语句限制

- SQLite 驱动声明 `multiple_statements = true`，一次 `Execute` 可执行多条语句；
  `MultipleStatement` 拒绝路径退役。MariaDB 等尚未实现的驱动保持
  `multiple_statements = false` 与既有拒绝行为（Issue #188 跟进）。
- 语句切分交给 SQLite 引擎本身（`sqlite3_prepare_v2` 的 `pzTail`），不写文本
  扫描器：trigger 体 `BEGIN … END` 内分号、字符串与注释内分号都不会切错。
- 切分随执行推进：第 k+1 条在第 k 条执行完后才 prepare，DDL→DML 依赖前序副作用
  的脚本因此可用。

### 缓冲模型与预算

- 单语句（含空脚本、尾分号、纯注释）仍走既有流式路径，行为完全不变。
- 多语句顺序急执行：每个结果集的全部行在驱动内缓冲（沿用既有行表示），
  `statements[]` 返回全量摘要（序号/列/rows_affected/has_result_set），
  `first_batch` 是第一个结果集的首批（批大小沿用既有约定）。
- `FetchBatch(statement_index, page_token)` 从缓冲续拉，page_token 编码与既有
  风格一致（已交付行数的十进制字符串）；越界序号 → `invalid_argument`。
  写语句的 fetch 返回空批 `has_more=false`（对齐既有单语句写行为）。
- 缓冲行数**合计**超过 `driver_api::MULTI_STATEMENT_ROW_BUDGET`（100_000 行）→
  `resource_exhausted` + 新错误码 `query.script_too_large`，文案建议拆分脚本或
  给查询加 LIMIT。预算只约束多语句缓冲路径，不影响单语句流式取数。
- 全部结果集交付完毕即释放缓冲；`FetchBatchResponse.complete` 表示**整个查询**
  交付完毕（单条语句读完不等于查询结束），查询登记同步释放。

### 失败位置与 completed metadata

- 第 k 条失败：保留驱动 classify 原样的 code/category（不强制改码），共享错误
  类型附加 `StatementFailure { failed_index: k, completed_statements: k }`
  （0 起始，只计真实语句）；不回滚、丢弃已缓冲结果集、剩余语句不执行。
- rpc-server 把失败位置写入两个 ascii trailing metadata：
  `sqarvo-failed-statement=<index>`、`sqarvo-completed-statements=<count>`，
  与既有 `sqarvo-error-code` 并列。非脚本错误不携带这两个键，旧客户端行为不变。
- 取消：语句间隙检查取消标志，当前语句走既有 interrupt 通道（幂等），返回既有
  `QUERY_CANCELLED` 语义，不附加失败位置。
- 参数化执行仍限单语句：多语句显式 `capability.unsupported` 拒绝，绝不静默只
  执行第一条。
