# ADR 0032：MCP 独立进程与连接模型

- 状态：已接受（2026-11-17，维护者确认改动确认单 #1）
- 日期：2026-11-17
- 关联：AGENTS.md §2.2（统一后端与传输）、§4（架构边界）、§6（Driver SPI）、§9（安全与隐私）、
  §11（Rust 规范）、§13（测试）、§15.0（改动确认单）；
  [ADR 0002](0002-local-ipc-transport.md)（本地 IPC 与访问控制）、
  [ADR 0006](0006-multi-statement-policy.md)（单语句限制）、
  [ADR 0007](0007-cell-truncation.md)（单元格截断与行级截断分离）、
  [ADR 0008](0008-session-concurrency.md)（会话并发：单会话单在途语句）、
  [ADR 0012](0012-credential-reference.md)（凭据引用格式与生命周期）、
  [ADR 0013](0013-credential-to-driver.md)（凭据传给驱动）、
  [ADR 0016](0016-credential-write-grpc.md)（写凭据的 gRPC 门面）
- 上游：Issue #207（MCP 配置、连接授权与 stdio 骨架）、REQ-MCP-001 / REQ-MCP-002

## 背景

外部 AI Agent 需要以受控方式发现数据库结构并执行只读查询。Sqarvo 现有能力几乎全部可以直接复用：

- `sqarvo-connection`：`SessionManager` 负责会话表、凭据解析、强制回滚与 `close_all()`；
- `sqarvo-driver-api` + `sqarvo-drivers`：`ConnectionParams`、`DriverId`、内置驱动注册表；
- `sqarvo-query-engine`：`QueryRegistry` / `QueryLimits`（登记、并发上限、幂等取消）；
- `sqarvo-local-store`：`AppPaths`、`credential_ref` 校验、操作系统凭据存储；
- `sqarvo-common`：`Error` / `ErrorCategory` / `ErrorCode`（稳定子码）。

但有三件事现有代码里**没有**答案，必须在这里定下来：

1. 后端是无状态服务，GUI 的连接档案由前端持有；MCP 需要一个**属于自己的**持久连接档案。
2. GUI 的 `connection_id` 是每次 Connect 生成的临时会话标识，不能作为 Agent 侧稳定标识。
3. MCP 的进程形态、协议生命周期与错误通道与 gRPC 完全不同：stdio、EOF、信号、stdout 纯净性。

## 备选方案

| 方案 | 优点 | 问题 |
|---|---|---|
| A. 寄生在 backend-service 内（加一路 gRPC/MCP 转发） | 复用进程与会话表；无需第二份连接档案 | MCP 客户端要求「由自己拉起的子进程说 stdio」；把外部 Agent 流量混进 GUI 后端的会话命名空间；GUI 退出即断；stdout 与 gRPC 日志共存难保纯净 |
| B. 独立 `sqarvo-mcp` 进程，经 gRPC 调 backend-service | 不重复装配驱动 | 违反 V3.1 §2.2：MCP 不得依赖 `sqarvo-rpc-server`，也不得让 prost/tonic DTO 变成业务模型；仍需 GUI 后端在跑 |
| C. 独立 `sqarvo-mcp` 进程，直接依赖业务 crates | 无 GUI/后端进程依赖；连接档案与授权完全自有；可按 MCP 语义裁剪并发与限额 | 需要第二份连接配置（`mcp.json`）与单独的凭据录入入口 |
| D. 独立进程 + 内嵌 SQL 子集解释器 | — | 与项目定位冲突：不得复制查询引擎 |

## 决策

采用 **C：独立二进制 `apps/sqarvo-mcp`，直接依赖 `sqarvo-connection` / `sqarvo-drivers` /
`sqarvo-query-engine` / `sqarvo-local-store` / `sqarvo-common`，传输只有 stdio。**

明确不做的事（与 V3.1 §2.2 一致）：不依赖 `sqarvo-rpc-server`；不复制驱动、连接管理、
查询引擎或凭据机制；不读 GUI 选中的连接；不通过 gRPC 借用 GUI 的 `SessionManager`。

### 1. 三层分离：设备凭据 ≠ 配置 ≠ 授权

| 层 | 载体 | 内容 | 谁写 |
|---|---|---|---|
| 秘密 | 操作系统凭据库（Secret Service / Credential Manager） | 数据库密码 | `sqarvo-mcp credentials set`（终端交互，不回显） |
| 引用 | `mcp.json` 的 `credential_ref` | `sqarvo:conn/<uuid v4>`（ADR 0012 格式） | 用户手填（或把 `set` 打印的引用粘进去） |
| 授权 | `mcp.json` 的 `connections[].enabled` + `enabled_tools` + `query_limits` | 哪些连接、哪些工具、多严的限额 | 用户 |

要点：

- `mcp.json` **没有**密码字段，`deny_unknown_fields` 让 `password` 之类的字段直接导致启动失败，
  而不是被静默忽略（`config.rs`）。
- 引用缺失/格式错在启动期就拒绝；引用格式错时**不回显原文**（`validate_reference` 的既有口径）。
- `properties` 里出现 `password` / `passwd` / `secret` / `token` 一类键名，对**任何驱动**都拒绝：
  驱动的参数白名单只管自己认识的键，这条是跨驱动的兜底。
- 凭据写入是唯一需要交互终端的路径；非 TTY 在打开密钥环**之前**就报错退出。

启动期还会做按驱动的参数预检（`sqlite` 只认 `read_only`、禁 `username`/`credential_ref`/TLS/port≠0；
`mariadb` 只认 `ssl_ca`/`ssl_cert`/`ssl_key`/`connect_timeout_ms`、必须有用户名与非零端口）。
这些规则镜像驱动自己的 `from_params`（`crates/drivers/*/src/*.rs`），作用是「把拼错的键在启动时
暴露」；**驱动仍是连接期的唯一权威**，这里不做提前接受、也不复制握手逻辑。未知 `driver_id` 一律
拒绝启动（fail-closed），内置驱动集合变化由单元测试 `builtin_driver_ids_match_rules` 提醒。

### 2. Lazy 会话与状态机

启动只做四件事：读配置 → 语义校验 → 装配 `SessionManager`/`QueryRegistry` → 打开审计写入器。
**不连任何数据库**；一个库连不上不影响服务起来。

状态机（`access::ConnectionState`）：`not_connected → connected / failed`。

- `not_connected`：`list_connections` 看到的初始状态；**不触发网络**。
- `connected`：首次工具调用成功建立会话后缓存 `(ConnectionId, SessionId)`。
- `failed`：记录错误**子码 + 分类**（如 `credential.not_found` / `unavailable`），不记录凭据、
  不把驱动原始文案透给 Agent；下一次调用仍会重试（不是终态）。

同一 alias 至多一个会话：建连过程放在 per-alias 的 `tokio::sync::Mutex` 里并二次检查状态，
避免两个并发首调各建一条真实连接。取句柄时一律回查 `SessionManager::acquire`，因为「会话被
disconnect」对缓存的 `Arc<SessionHandle>` 不可见。

断连只重试**一次**，且只认明确表示「会话已消失」的错误（`session.not_found`、`transport.failed`、
分类 `Unavailable`、驱动自定义的 `*.session_closed`）。把语法错误也当断连重试，会让「同一条 SQL
跑两遍」变成默认行为。

### 3. 并发模型（受 ADR 0008 约束）

三层，互不替代：

1. **进程级信号量**：`EffectiveLimits::max_concurrency` 个许可（默认 2、硬上限 4）；
   许可耗尽时**排队**而不是报错——「同一连接串行」是语义，不是故障（V3.1 §9.2）。
2. **`QueryRegistry`**：查询登记、幂等取消、同时活跃查询数上限；`max_concurrent` 与信号量**同源**
   （`EffectiveLimits::query_limits()`），避免两层各算一套。
3. **会话执行锁**：`SessionHandle` 内部的 `Arc<tokio::Mutex>`（ADR 0008）保证同一 alias 同时最多
   一条在途语句。MCP **不**新建会话、**不**引入每会话连接池去绕开它。

限额默认值与硬上限（`config.rs` 常量）：普通查询 200 行、硬性最大 1000 行、超时 10 s、
单次结果 1 MiB、进程并发 2（上限 4）。`mcp.json` 的 `query_limits` 只能往下调或调到上限，
越界一律钳制并告警（不静默生效）。

### 4. stdio 生命周期与退出码

- **stdout 只归协议**：`tracing_subscriber` 显式 `.with_writer(std::io::stderr)`；
  包内禁用 `println!`/`print!`/`dbg!`（唯一例外是 `credentials` 管理模式的人类可读输出，
  该路径不启动 server）。阶段 A spike 实测：默认 writer 会把 INFO 行混进协议流并直接破坏
  initialize。
- **EOF**：stdin 关闭 → serve 循环自然结束（`quit_reason=Closed`）→ `close_all()` → **exit 0**。
- **SIGINT/SIGTERM**：`serve()` 之后取 `RunningService::cancellation_token()`，由一个信号任务
  取消它 → 循环 `quit_reason=Cancelled` → `close_all()` → **exit 0**。Windows 只有 Ctrl-C
  （没有可捕获的 SIGTERM，与 ADR 0002 附记同口径）。
- **退出清理**：`AccessService::close_all()` = 清 `QueryRegistry` 登记 + `SessionManager::close_all()`。
- **退出等待有上限**：rmcp 的 stdio 读线程是 Tokio 阻塞线程，stdin 未关闭时永远读不完；
  `Runtime` 默认 drop 会无限期等它，因此二进制手写 runtime 并在最后调 `shutdown_timeout(2s)`。
  否则「优雅关停已完成、进程却挂着不走」。
- 启动失败（配置非法、非法工具名、未知驱动）→ stderr 一行错误 → **exit 1**，stdout 保持为空。

### 5. rmcp 锁版策略

- 精确锁 `rmcp = "=3.10.0"`（`backend/Cargo.toml`，骨架提交已落）。上游发版极快，且 3.x 内已有
  API 断代（`StdioTransport` → `transport::stdio()`）；升级必须作为独立评估项，跟本次改动解耦。
- 依赖面：`transport-io` + `server` + `macros` + `schemars`；client/HTTP/auth 不启用。
- MSRV / edition / license 与 workspace 完全一致（1.88 / 2024 / Apache-2.0），阶段 A 已实测。

### 6. 错误通道

业务失败走**工具级** `isError: true`（`error::ToolFailure`，附结构化 `{code, category, retryable}`），
因为协议级 JSON-RPC error 在客户端往往只显示一句笼统提示。协议级错误只留给「未知工具 / 请求
形状非法 / 服务不可用」。子码全部复用 `sqarvo_common::ErrorCode`，本次不新增 `mcp.*` 码。

## 论证：为什么不是 A / B

- MCP 客户端（Cherry Studio、Inspector、各类 Agent 运行时）的标准形态是「自己拉起子进程、只用
  stdio 说话」。寄生在 backend-service（A）意味着把外部 Agent 的流量塞进 GUI 后端的会话命名空间，
  既绕不开「GUI 必须开着」这个前提，也让 stdout 的归属含糊。
- B 让 MCP 依赖 rpc-server 与 prost 生成类型，等于把 gRPC DTO 变成新的内部业务模型（V3.1 §2.2 明确
  禁止），而且仍然要求 GUI 后端在跑。
- C 的唯一代价是第二份连接档案。这份档案必须是独立的：GUI 档案归前端管、`connection_id` 是临时的，
  两者语义不同，强行共用只会把「谁有权访问哪个库」混在一起。

## 取舍

- **多一份配置要维护**：`mcp.json` 与 GUI 连接档案不共享。这是有意的：MCP 的授权边界必须能单独
  审计与撤销（关掉 `enabled` 立即失效）。
- **审计只到「指纹」级**：只记 SQL 指纹（规范化 + SHA-256）与元数据，不记原始 SQL 与结果，因此
  事后复盘看不到「Agent 具体查了哪一行」。换来的是审计文件本身不再是敏感数据副本。
- **审计多实例不是强一致**：同进程用 `std::sync::Mutex` 串行化，跨进程用 `flock(LOCK_EX|LOCK_NB)`
  并在拿不到锁时退化到 `audit.<pid>.jsonl`。两个实例各自轮转同一目录仍可能互相覆盖（见「已知限制」）。
- **信号窗口期**：信号处理器在 `serve()` 返回后才注册（`serve()` 会一直等 initialize）。客户端连上
  但尚未握手时收到 SIGTERM，进程走系统默认处置立即终止。此时既无会话也无在途查询，无需清理。
- **懒连接的代价**：首个工具调用要付一次握手延迟。相对「启动就连所有库」的脆弱性（一个库挂掉、
  或等待用户解锁密钥环），这个代价值得付。
- **Windows 未实机验证**：本机只有 Linux。审计文件锁在 Windows 上是刻意跳过（best-effort）；
  Ctrl-C 路径复用 `tokio::signal::ctrl_c`，与 backend-service 同口径但未在 Windows 上跑过。

## 已知限制

1. **审计多实例**：`flock` 只串行化「追加一行」，不保护跨进程轮转。同用户跑两个 `sqarvo-mcp` 时，
   第二个实例会退化写 `audit.<pid>.jsonl`；轮转窗口内两实例仍可能互相覆盖。V1 不宣称多实例安全。
2. **`AppPaths` 注释漂移**：`crates/local-store/src/paths.rs` 的模块注释写 `~/.config/sqarvo`，
   实现用 `ProjectDirs::from("io","sqarvo","Sqarvo")`。本 ADR 与代码一律以 `AppPaths` 返回值为准，
   注释修正留给 local-store 的维护者。
3. **GUI 协商行为未实测**：阶段 A 只验证了 rmcp + Inspector 与裸 JSON-RPC 客户端；Cherry Studio
   的真实协商版本、工具调用展示需要人工 GUI 操作（spike REPORT.md §7）。协议层以「旧握手
   （2024-11-05 ~ 2025-11-25）+ 2026-07-28 无握手」的实测结论为准。
4. **`serve_with_ct` 未直接使用**：它需要 `tokio_util::sync::CancellationToken`，而本包按确认单
   只允许引用 workspace 已有依赖（`tokio-util` 不在其中）。改用等价的「`serve()` +
   `RunningService::cancellation_token().cancel()` + `waiting()`」，语义与 `close()` 一致。
   若要覆盖「握手期间收到信号」，需要把 `tokio-util` 加进 workspace 依赖。
5. **V1 只有骨架**：`tools/list` / `resources/list` 在本 Issue 返回空列表，`tools/call` 走 SDK 默认的
   `-32601`。工具与资源分别由 #209 / #210 接入，安全策略（SQL AST）由 #208 接入。
6. **`enabled_tools` 的未知名判定依赖 `config::V1_TOOL_NAMES`**：#209 注册工具时必须使用同一份常量，
   否则要么合法配置被拒、要么出现无法发现的死名字。
7. **审计轮转参数暂不可配**：10 MiB × 5 是代码常量（`RotationPolicy::default()`），测试通过构造器注入
   小阈值。若要开放给用户，需要同时更新配置文档与钳制规则。

## 重新评估条件

- Cherry Studio / Inspector 实测出现与 spike 不同的协商行为 → 重新评估协议版本策略与 capabilities 声明。
- 需要「多实例同时写同一审计目录」成为一个受支持场景 → 换用单一写入者（守护进程）或按实例分目录，
  而不是继续加固 flock。
- 需要覆盖「握手期间收到 SIGTERM」→ 把 `tokio-util` 纳入 workspace 依赖并改用 `serve_with_ct`。
- 需要把审计内容升级为「可复盘的具体 SQL」→ 必须同时定义脱敏与保留策略（与 AGENTS §9 冲突评估）。
- rmcp 出现安全通告或破坏性升级 → 按本 ADR §5 的锁版策略单独评估，不与业务改动混合。
