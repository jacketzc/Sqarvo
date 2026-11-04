# 里程碑

里程碑是稳定的阶段锚点，不因单个任务延期而修改。完成标准变化必须有明确理由并记录。
具体任务与进度在 GitHub Issues / Projects 中跟踪。

| 里程碑 | 对应阶段 | 目标 | 对应版本 | 状态 |
|---|---|---|---|---|
| M0 骨架 | S0 | 可编译、可测试的仓库骨架与本地 IPC | v0.1 | 已完成 |
| M1 契约冻结 | S1 | Driver SPI v0.2、proto 增量、驱动测试基架 | v0.1 | 已完成 |
| M2 首个驱动切片 | S2 | SQLite 连接 → 执行 → 分批 → 取消 → 关闭 | v0.1 | 已完成（#9–#17） |
| M3 凭据与 PostgreSQL | S3 | 系统凭据存储 + PostgreSQL 驱动 | v0.2 | 未开始 |
| M4 元数据与事务 | S4 | 对象树、列信息、DDL、显式事务（跨驱动） | v0.3 / v0.4 | 未开始 |
| M5 第二驱动与一致性 | S5 | MySQL 驱动 + 三驱动一致性矩阵 | v1.0 | 未开始 |
| M6 前端接入与硬化 | S6 | 三端接入、性能、安全、跨平台构建 | v0.2 起持续 | 未开始 |

## 完成标准

### M0 骨架（已完成）

- workspace 编译通过，`fmt` / `clippy -D warnings` / `test` 全绿；
- 真实 UDS + token 的 gRPC 端到端测试通过；
- 二进制运行后 socket 0600、端点文件 0600、SIGTERM 清理、日志无 token；
- 文档：架构、IPC、gRPC 约定、Driver SPI、ADR、路线图齐全。

实际完成：2026-11-04（Linux），验证记录见 `development-plan.md` 的 S0。

### M1 契约冻结（完成标准）

- SPI v0.2 覆盖多语句、影响行数、截断、事务、元数据；
- proto 仅做增量修改并重新生成；
- testkit 可运行；
- ADR 合并（0005–0009：游标、多语句、截断、并发、SQLite 选型）。

### M1 契约冻结（实际完成）

实际完成：2026-11-04（Linux）

- SPI v0.2：`ExecutionStart` / `StatementSummary`、按 `statement_index` 的游标、
  事务接口、`MetadataProvider`、`Cell.truncated` 与 64 KiB 上限；
- proto 增量：`FetchBatchRequest.statement_index = 4`、`ExecuteQueryRequest.timeout_millis = 5`；
- `SessionManager::acquire` + 会话级执行锁 + 断连清理查询登记；
- 新增 `sqarvo-driver-testkit`（脚本化会话 + `smoke` 符合性检查）；
- ADR 0005–0009：游标所有权、多语句策略、截断、并发、SQLite 选型；
- 验证：`cargo fmt --check`、`clippy -D warnings`、`cargo test`（66 通过）通过，
  UDS gRPC 冒烟测试不回归。

### M2 首个驱动切片

- 真实 gRPC 客户端完成 SQLite 查询与分批拉取；
- 取消 2 秒内生效且幂等；
- 断开连接后会话/查询计数归零；
- CI 运行 SQLite 测试。

### M2 首个驱动切片（实际完成）

实际完成：2026-11-04（Linux）。任务：#9–#17，发布说明见
[v0.1](../releases/v0.1.md)。

| 完成标准 | 证据 |
|---|---|
| 真实 gRPC 客户端完成 SQLite 查询与分批拉取 | `sqarvo-rpc-server/tests/grpc_sqlite_e2e.rs::sqlite_lifecycle_over_uds`：真实 UDS + 临时库，7 行按每批 3 行拉成 3 批并逐值校验 |
| 取消 2 秒内生效且幂等 | `::cancel_interrupts_running_fetch_within_deadline`：取批进行中 `Cancel`，实测耗时断言 `< 2s`，重复取消仍成功；错误码 `query.cancelled` |
| 断开连接后会话/查询计数归零 | 每个 e2e 用例结束时断言 `SessionManager::active_count() == 0` 且 `QueryRegistry::active() == 0` |
| CI 运行 SQLite 测试 | 作业 `41786163030`（commit 98c57a6）成功；显式步骤输出 `type_mapping` 12 用例、`grpc_sqlite_e2e` 3 用例全部通过，无 ignored/skipped |

配套验证（同一次运行）：`cargo test` 95 通过 / 0 失败，`fmt --check` 与
`clippy -D warnings` 通过，release 构建通过，真实二进制的权限/清理/日志脱敏实测通过。
命令与输出摘要见 [development-plan.md](development-plan.md) 的 S2 验证记录。

遗留（不属 M2 范围，已登记）：启动与空闲内存基准（M6 / `REQ-PERF-001`）；
Windows、macOS 未构建验证；事务与元数据 RPC（M4）；取消无法作用于「尚未返回首批」的执行
（契约层改进候选）。

### M3 凭据与 PostgreSQL

- 凭据只存系统凭据库，缺失时明确报错；
- PostgreSQL 查询、TLS、取消、错误分类可用；
- 通过 testkit。

### M4 元数据与事务

- 两驱动下对象树可分页展开，列信息保留原始类型；
- 事务提交/回滚/失败态行为可测试；
- 不支持能力返回明确错误。

### M5 第二驱动与一致性

- MySQL 驱动可用；
- 同一 testkit 覆盖三驱动，差异有文档。

### M6 前端接入与硬化

- 三端至少一端完成真实连接与查询（其余记录状态）；
- 启动/内存/大结果集基准可复现；
- 安全清单通过；
- Windows / macOS 至少各完成一次真实构建并记录。

## 变更规则

- 任务延期不改里程碑；只有范围变化或技术前提失效才调整完成标准；
- 调整必须记录在 PR 或 ADR 中，并同步 `development-plan.md` 与 `release-plan.md`；
- 里程碑完成时在本文追加「实际完成」日期与验证证据，不删除原标准。
