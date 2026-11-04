# 开发实施计划

本文件定义技术实施顺序、阶段交付物、验收标准与完成条件。它是开发工作的核心依据；
具体任务在 GitHub Issues 中跟踪，本文件不重复维护任务清单。

- 版本目标见 [roadmap.md](../product/roadmap.md) 与 [release-plan.md](release-plan.md)。
- 阶段锚点见 [milestones.md](milestones.md)（M0–M7 与本文 S0–S7 一一对应）。
- 风险见 [risk-register.md](risk-register.md)。

## 总体顺序

```text
S0 骨架 → S1 契约冻结 → S2 SQLite 垂直切片        （均已完成）
                          │
          ┌───────────────┴───────────────┐
          ▼                               ▼
  S3 凭据 + PostgreSQL            S4 元数据 + 事务
          │                               │
          │                               ▼
          │                 S7 导入导出与结构管理（v0.5）
          │                               │
          └───────────────┬───────────────┘
                          ▼
        S5 第二驱动（MySQL）+ 一致性矩阵（v1.0）
                          │
                          ▼
     S6 前端接入 + 性能/安全硬化（v0.2 起持续）
```

小节顺序按依赖排列；阶段编号按引入顺序（S7 为后续补充的阶段）。

依赖原则：

- S1 未完成不写任何驱动，避免把错误的 SPI 形状固化；
- S2 必须先于 S3/S4：没有可运行会话，凭据、元数据、事务都无从验证；
- S3 与 S4 可并行，但都依赖 S2 的会话与查询通路；
- 每个驱动的元数据/事务能力在 S4 统一定义，避免两套抽象；
- S7 依赖 S4 的元数据（结构对比需要列信息与 DDL）与 S2 的游标分批（导出复用），
  其契约前置（参数化执行）必须在 S7 开工前冻结，理由同 S1；
- S7 与 S5 互不依赖，但 v0.5 先于 v1.0 发布，因此 S7 先于 S5 交付；
- S6 是持续型阶段，不排队在最后：前端接入从 v0.2 开始，硬化随每个版本推进。

## S0：骨架（已完成）

交付物：Rust workspace、5 个域服务的 Protobuf 契约、本地 IPC（UDS + token）、
错误模型、Driver SPI 类型、会话管理、查询登记、元数据分页、三端 UI 骨架、文档与 CI。

验证记录（2026-11-04，Linux）：

- `cargo fmt --all -- --check` 通过；
- `cargo clippy --all-targets -- -D warnings` 通过；
- `cargo test` 56 通过 / 0 失败（含真实 UDS 的 gRPC 端到端测试）；
- `cargo build --release -p sqarvo-backend-service` 通过；
- 实际运行二进制：socket 0600、端点文件 0600、SIGTERM 清理、日志无 token。

已知限制：无任何数据库驱动；SQL 执行、元数据、事务 RPC 返回 `UNIMPLEMENTED`。

## S1：契约冻结（Driver SPI v0.2）

目标：把 proto 已经表达、但 SPI 还缺失的能力补齐，并建立驱动测试基架。

工作内容：

1. `driver-api` 结果模型：
   - `ExecutionStart`（`statements[]` + 首批数据）替代单个 `ResultBatch` 返回；
   - `StatementSummary`：语句序号、SQL 预览、列元数据、影响行数、是否有结果集；
   - `fetch(query_id, statement_index, cursor, max_rows)`，游标由驱动持有；
   - 单元格截断标记（每格上限，例如 64 KiB），与 proto `ColumnValue.truncated` 对齐。
2. 事务接口：`begin(isolation, read_only) -> TransactionId`、`commit`、`rollback`，
   不支持的驱动必须返回 `capability.unsupported`。
3. 元数据接口：独立 `MetadataProvider` trait（`list_children` / `columns` / `ddl`），
   不塞进 `DriverSession`，避免强迫所有驱动实现。
4. 会话执行通路：
   - `SessionManager::acquire(session_id)` 返回可用于执行的句柄；
   - 会话级串行化（单条在途查询），驱动可假设非并发调用；
   - 查询与会话绑定，断连时清理该会话全部查询。
5. proto 增量字段（保持向后兼容，不改既有编号）：
   - `FetchBatchRequest.statement_index`；
   - 如需要，`ExecuteQueryRequest.timeout_millis`。
6. 新增 `backend/crates/driver-testkit`：所有驱动复用的符合性测试
   （类型往返、分页稳定、取消幂等、错误分类、资源清理），先用假驱动跑通。
7. ADR：游标所有权、多语句错误策略、截断策略、每会话并发。

交付物：SPI v0.2、proto 增量、testkit、4 份 ADR、更新后的
`docs/database/driver-spi.md` 与 `docs/api/grpc-conventions.md`。

验收标准：

- 全部 crate 编译通过，`fmt` / `clippy -D warnings` / `test` 全绿；
- testkit 可对假驱动运行并通过；
- 现有 gRPC 冒烟测试不回归；
- SPI 变更评审通过（PM/架构角色确认），无「先写着看」的临时类型。

完成条件：S1 的接口与 ADR 合并入库，后续驱动可以只依赖 SPI 实现。

实际完成（2026-11-04）：SPI v0.2、proto 增量、`SessionManager::acquire` 与会话锁、
`driver-testkit`、ADR 0005–0009 已完成；`cargo test` 66 项通过，`clippy -D warnings` 通过。

## S2：SQLite 垂直切片

目标：证明「前端/客户端 → gRPC → 会话 → 驱动 → 数据库 → 分批返回」全链路可用。

工作内容：

1. 新增 `backend/crates/drivers/sqlite`（workspace 成员）：
   - 连接：文件路径、只读模式；凭据无需求（SQLite 无密码）；
   - 执行：`rusqlite`（bundled，避免系统依赖），同步调用必须走阻塞线程池；
   - 取消：`InterruptHandle`，取消必须幂等且可跨线程触发；
   - 游标：持有 prepared statement，禁止用 `OFFSET` 重查（结果必须稳定）；
   - 关闭：释放语句、连接，登记表清理。
2. `QueryService` 真正接线：`Execute` / `FetchBatch` / `Cancel` / `Close`，
   批次上限、语句摘要、错误映射、查询登记与回收。
3. gRPC 层端到端测试：临时数据库 → CREATE/INSERT/SELECT → 分两批取回 →
   取消长查询 → 关闭 → 会话/查询计数归零。
4. 类型往返测试：NULL vs `''`、INTEGER/REAL/TEXT/BLOB、i64 边界、空 BLOB、
   UTF-8、超长文本截断标记；SQLite 无原生日期类型 → 保留 TEXT + 原始类型名。
5. CI 增加 SQLite 作业（无需外部服务）。

验收标准：

- 真实 gRPC 客户端（UDS）可完成上述流程；
- 单批行数不超过上限，分页无重复/丢失（同一快照内）；
- 取消在 2 秒内生效，重复取消不报错；
- 断开连接后 `active_count == 0`，无残留查询；
- 测试全部在 CI 运行，不依赖本机文件。

完成条件：后端可对外提供 SQLite 查询能力，二进制度量（启动、内存）有初步记录。

实际完成（2026-11-04）：任务 #9–#17 全部关闭，发布说明见 [v0.1](../releases/v0.1.md)。

验证记录（2026-11-04，Linux）：

- `cargo test`：95 通过 / 0 失败。其中
  - `sqarvo-driver-sqlite`：12 个单元用例 + 12 个集成用例（`tests/type_mapping.rs`，
    覆盖 NULL / `''` / 0 区分、i64 边界、NUMERIC 亲和性、日期类保留 TEXT、二进制、
    64 KiB 截断与非法 UTF-8）；
  - `sqarvo-rpc-server`：5 个 gRPC 冒烟用例 + 3 个端到端用例（`tests/grpc_sqlite_e2e.rs`，
    覆盖 UDS 生命周期、取消 2 秒内生效、批次上限 5000、断连后会话与查询登记归零）。
- `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings` 通过。
- `cargo build --release -p sqarvo-backend-service` 通过。
- CI（GitHub Actions，`CI` / `backend` 作业）在 main 上通过；作业内除 `cargo test` 外，
  还显式执行 `cargo test -p sqarvo-driver-sqlite --test type_mapping` 与
  `cargo test -p sqarvo-rpc-server --test grpc_sqlite_e2e`，SQLite 覆盖不允许被静默跳过。
- 真实二进制运行（Linux，release 构建，`XDG_RUNTIME_DIR` 指向临时目录）：

  ```bash
  XDG_RUNTIME_DIR=$TMP/runtime target/release/sqarvo-backend \
      --socket $TMP/runtime/backend.sock --log-filter info &
  stat -c '%A %n' $TMP/runtime/backend.sock $XDG_RUNTIME_DIR/sqarvo/backend-endpoint.json
  grep -qF "$TOKEN" backend.log   # 期望不命中
  kill -TERM $PID; wait $PID
  ```

  实测结果：socket `srw-------`（600）、端点文件 `-rw-------`（600）、目录 `drwx------`（700）；
  端点文件含 64 位 token 且日志中未出现 token 原文；`SIGTERM` 后退出码 0，
  socket 与端点文件均被删除；端点声明 `transport=unix protocol=v1 service=0.1.0`。

遗留：启动时长与空闲内存尚未测量（`REQ-PERF-001`），按计划归入 S6 基准，不在本阶段声称已达标。

## S3：凭据存储 + PostgreSQL

目标：接入真实网络数据库，验证 SPI 对第二种数据库成立。

工作内容：

1. 凭据存储：三平台抽象 + Linux Secret Service 实现（优先），
   测试使用内存实现；缺失时返回 `credential.unavailable`，绝不回退到明文配置。
2. PostgreSQL 驱动：
   - 连接参数、TLS 模式映射（disable/prefer/require/verify-ca/verify-full）；
   - 类型映射：numeric、timestamptz、uuid、jsonb、数组、bytea、无符号缺失说明；
   - 取消：服务端 cancel 请求；
   - 元数据与事务能力按 S4 的接口实现（可先留 `capability.unsupported`）。
3. 驱动选型 ADR：`tokio-postgres` vs `sqlx`，评估协议控制、取消、COPY、TLS、
   维护状态与许可证。
4. 集成测试：`tests/integration/docker-compose.yml` 的 PostgreSQL 实例，
   合成数据；CI 可选运行（无服务时跳过并明确标注）。

验收标准：

- 从真实 PostgreSQL 执行查询并分批返回；
- 错误分类正确（认证失败、超时、断线、权限不足、取消）；
- 密码不出现在日志/错误/端点文件中；
- 驱动通过 testkit 的全部用例。

完成条件：至少两种数据库（SQLite、PostgreSQL）共享同一 SPI 与 gRPC 行为。

## S4：元数据 + 事务（跨驱动）

目标：把对象树、列信息、DDL 与显式事务做成驱动无关能力。

工作内容：

1. `MetadataProvider` 实现：SQLite（`sqlite_schema` + `PRAGMA`）与 PostgreSQL
   （`information_schema` / `pg_catalog`）；
2. `MetadataService` 接线：分页、层级（catalog/schema）、原始类型保留、DDL 原文标记；
3. 事务：`TransactionService` 接线，单会话单事务，失败状态只允许回滚，
   只读事务，隔离级别能力声明；
4. UI 契约补充：对象树懒加载所需字段、事务状态字段（如缺失则增量添加）；
5. 测试：分页稳定性、类型保真、DDL 一致性、事务提交/回滚副作用、
   断连时事务清理。

验收标准：

- 两驱动下对象树可分页展开，列信息含原始类型与可空性；
- 事务内执行与回滚行为可被测试观察；
- 不支持的能力返回明确错误，不返回空列表冒充成功。

完成条件：后端具备「连接 → 浏览 → 执行 → 事务」完整闭环。

## S7：导入导出与结构管理（v0.5）

目标：让结果集能安全写出、外部数据能安全写入，并能生成结构对比脚本。
本阶段以「不损坏用户数据」为第一约束：所有写入都可取消、可定位失败、可清理残留。

前置契约变更（必须先冻结，理由同 S1：避免把错误的形状固化）：

1. `DriverSession` 增加参数化执行（`ExecuteRequest` 携带绑定参数，或新增批量写入方法）。
   导入**禁止**拼接 SQL 字面量，否则引号、类型与控制字符都会成为注入与数据损坏来源；
2. 能力描述符增加参数化/批量写入能力（命名以 ADR 为准），未实现时如实为 `false`，
   调用方拿到的必须是 `capability.unsupported`，不得静默退化为字符串拼接；
3. proto 新增 `DataTransferService`（`proto/services/v1/data_transfer.proto`）：
   任务创建、进度查询、取消、终态与错误摘要；按 AGENTS.md §7 定义取消与终态语义；
4. ADR：参数化执行与批量写入、流式导入导出、结构对比「只生成脚本」。

工作内容：

1. 导出：复用 `QueryService` 的游标分批读取，按 CSV / JSON / SQL 流式写出；
   `Cell.truncated` 与大字段、二进制、NULL 与空串的写法有明确定义；
2. 导入：CSV / JSON → 参数化批量 INSERT，分批提交；失败可定位到行号与列名
   （错误信息不含数据值）；提供「遇错停止 / 跳过并计数」两种策略；
3. 路径与覆盖安全：规范化目标路径、拒绝路径穿越、默认不覆盖已有文件、
   覆盖需显式开关；失败与取消后清理临时文件；
4. 资源上限：单文件大小、单批行数、内存上限与背压；超限返回明确错误，不 OOM；
5. 结构对比：读取两侧连接的元数据与 DDL，产出差异报告与迁移脚本；**只生成，不执行**；
6. 任务生命周期：可取消、可查进度、终态必须释放游标、文件句柄与临时文件。

交付物：`DataTransferService` 及其 crate、SPI 参数化执行、3 份 ADR、
`docs/database/` 的格式与差异说明、v0.5 支持矩阵条目。

验收标准：

- 10 万行 CSV 导入与导出在内存上限内完成，有基准记录，不一次性读入内存；
- 导入/导出中途取消生效，临时文件与游标被清理；
- 路径穿越、覆盖已有文件、超过大小上限三类输入均被明确拒绝，并有测试；
- 结构对比只产出脚本，任何代码路径都不会自动执行 DDL；
- 不支持参数化执行的驱动返回 `capability.unsupported`，不静默降级。

完成条件：导入、导出、结构对比三项在后端可用并可通过 gRPC 调用，失败语义与资源上限有测试；
界面由 S6 接入。

## S5：第二驱动（MySQL）+ 一致性矩阵

工作内容：

1. MySQL 驱动（评估 `mysql_async` / `sqlx`）：无符号整数、catalog=schema 语义、
   字符集/排序规则、`ZEROFILL`、`BIT` 等差异；
2. 三驱动跑同一套 testkit，形成能力矩阵（事务、取消、流式、分页、元数据层级）；
3. CI 增加 MySQL 服务作业（或标注为手动验证）；
4. 更新 `docs/database/` 的数据库差异说明与支持矩阵。

验收标准：同一测试套件在三驱动下通过（或明确记录不支持项与错误语义）。

完成条件：Driver SPI 被证明可扩展到三个数据库，且行为差异有文档。

## S6：前端接入 + 硬化

工作内容：

1. 前端启动后端、读取端点文件、校验协议版本、token 认证、退出清理（三端各自实现）；
2. 查询超时、结果集内存上限、背压、取消传播到驱动；
3. 可观测性：tracing span（会话/查询/驱动）、关键计数与耗时；
4. 性能基准：冷启动、空闲内存、10 万行结果集、200 列宽表，形成可复现脚本；
5. 安全复核：日志脱敏测试、端点文件权限、UDS 访问控制、错误信息审计；
6. 平台构建验证：Windows / macOS 至少完成一次真实构建并记录结果。

验收标准：三端可连接后端并完成一次查询；基准数据入库；安全清单全部通过。

完成条件：达到 v1.0 的发布门槛（见 release-plan.md）。

## 全局完成定义（Definition of Done）

任何阶段/任务完成，必须同时满足：

- 代码通过 `cargo fmt --check`、`clippy -D warnings`、`cargo test`；
- 新增/变更行为有测试，驱动变更通过 testkit；
- 相关文档（架构、API、驱动、需求状态、里程碑）已更新；
- 无凭据、真实数据、生成产物或临时文件进入提交；
- PR 关联 Issue 与里程碑，报告包含验证命令与结果；
- 未验证的平台/场景在 PR 与里程碑中明确标注，不声称已通过。

## 估算与排期

本计划只定义顺序与依赖，不承诺日期。粗略量级（单人 + Agent 协作，供排期参考）：

- S1：1–2 天；
- S2：2–3 天；
- S3：3–5 天（含依赖选型与集成测试）；
- S4：3–5 天；
- S7：3–5 天（格式编解码、路径安全、结构对比）；
- S5：3–5 天；
- S6：5–10 天（跨平台构建不可控因素最多）。

估算不是承诺；实际排期以里程碑与 Issue 为准。
