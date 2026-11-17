# MCP Server 架构（`sqarvo-mcp`）

适用范围：`backend/apps/sqarvo-mcp`（V1）。事实来源是代码与测试，本文不引入代码里没有的行为；
口径与代码冲突时以代码为准，并把它记在 §10「与代码不符项」。

- 决策记录：[ADR 0032](../decisions/0032-mcp-standalone-process.md)（独立进程与连接模型）、
  [ADR 0033](../decisions/0033-sql-parser-readonly-policy.md)（SQL 只读策略）。
- 对外契约（工具、资源、`mcp.json` 每个字段、错误子码）：
  [docs/api/mcp-tools.md](../api/mcp-tools.md)。
- 需求条目：`REQ-MCP-001 … 006`，见 [产品需求](../product/requirements.md) §9。

## 1. 定位与不变式

MCP Server 是**给外部 AI Agent 用的第二个入口**，不是第二套数据库系统。它复用与 GUI 后端
完全相同的连接、驱动、会话与查询生命周期代码，只在外面换了一层协议（stdio JSON-RPC）、一层
授权模型（`mcp.json` 自有的连接档案）和一层安全策略（SQL AST 只读判定）。

四条不许动摇的不变式：

1. **只读**：V1 不暴露任何写路径。SQL 必须先过 AST 只读策略才可能到驱动；结构发现类工具与
   资源只读元数据。
2. **不依赖 `sqarvo-rpc-server`，不碰 proto 类型**：`Cargo.toml` 的依赖清单里
   `sqarvo-common` / `sqarvo-connection` / `sqarvo-driver-api` / `sqarvo-drivers` /
   `sqarvo-local-store` / `sqarvo-query-engine` 才是合法依赖，`prost` / `tonic` /
   `sqarvo-rpc-server` 一个都不许出现（否则 gRPC DTO 会变成内部业务模型，ADR 0032 方案 B
   就是这么被否掉的）。
3. **密码不落盘、不回显**：配置里只有 `credential_ref`；错误文案、日志、审计三层都不许出现
   密码或完整连接串。
4. **stdout 只归协议**：日志一律 stderr（阶段 A spike 实测：`tracing_subscriber` 默认 writer
   是 stdout，会把 INFO 行插进协议流并让严格客户端断连）。

## 2. 进程模型

```text
┌────────────────────────────┐        spawn + stdio pipes        ┌────────────────────────────┐
│ MCP 客户端（Agent 宿主）    │  ───────────────────────────────► │ 子进程 sqarvo-mcp           │
│ Cherry Studio / Inspector /│  ◄── stdout：JSON-RPC（仅协议） ── │ Rust + Tokio + rmcp 3.10.0  │
│ 各类 Agent 运行时           │  ◄── stderr：tracing 日志 ─────── │ mcp.json 自有连接档案        │
└────────────────────────────┘                                   └──────┬────────────┬────────┘
                                                                        │            │
                                                     SessionManager ────┘            └──── AuditWriter
                                                     （Driver SPI 内置驱动）                       │
                                                                        │                          ▼
                                                                        ▼                 data_dir()/mcp/audit.jsonl
                                                        数据库（MariaDB / SQLite 文件）
```

要点：

- **与 GUI 完全独立**：不读 GUI 的连接档案，不借用 GUI 后端的 `SessionManager`，GUI 没开也能
  跑；反过来 MCP 进程崩了不影响 GUI。客户端自己拉起子进程、自己关（stdin 关闭即退出）。
- **传输只有 stdio**：没有网络监听端口，不新增 IPC 通道，因此本地 IPC 的访问控制模型
  （[ADR 0002](../decisions/0002-local-ipc-transport.md)、
  [local-ipc.md](local-ipc.md)）不适用于它——它的访问边界就是「谁能启动这个进程 + `mcp.json`
  里授权了什么」。
- **两种运行形态**（同一个二进制）：无参数 = stdio server；`credentials set|delete` = 管理模式，
  不启动 server、不发协议消息，只跟系统密钥环打交道。

### 2.1 启动与退出

| 阶段 | 行为 | 失败时 |
|---|---|---|
| 1. 装配日志 | subscriber writer 钉死 stderr；过滤级别默认 `warn,sqarvo_mcp=info`，可用 `SQARVO_MCP_LOG` 覆盖 | — |
| 2. 读配置 | `--config <path>` 优先，否则默认路径（§4） | 文件不存在 = 空连接列表启动 + stderr 警告；文件存在但非法 = **拒绝启动** |
| 3. 语义校验 | alias、凭据引用格式、驱动白名单、`enabled_tools`、限额钳制 | 拒绝启动，stderr 一行原因，**exit 1**，stdout 保持空 |
| 4. 装配运行时 | `SessionManager` + `QueryRegistry` + 进程信号量 + 审计写入器 | 拒绝启动（同上） |
| 5. serve | `rmcp` 走 stdio，等 `initialize` 或 2026-07-28 的无握手 `_meta` 请求 | 握手前对端关闭 = 按 EOF 正常收尾，**exit 0**（#220） |
| 6. 退出 | stdin EOF / `SIGINT` / `SIGTERM` → `close_all()`（清查询登记 + 关全部会话）→ **exit 0** | 主循环异常仍走 exit 0 收尾路径，异常只进日志 |

- 退出等待有上限：`main` 手写 Tokio runtime 并 `shutdown_timeout(2s)`。rmcp 的 stdio 读线程是
  阻塞线程，`Runtime` 默认 drop 会无限期等它，于是出现「优雅关停已完成、进程却挂着不走」。
- 客户端关窗后进程必须自己退出（没有守护化、没有 detach）。

## 3. crate 依赖方向

```text
apps/sqarvo-mcp ──► rmcp（stdio + tools/resources 协议面）
      │
      ├──► connection ──► drivers ──► driver-api ──► common
      ├──► query-engine（QueryRegistry / QueryLimits）
      ├──► local-store（AppPaths、credential_ref 校验、系统凭据库）
      └──► common（Error / ErrorCategory / ErrorCode 稳定子码）

禁止：apps/sqarvo-mcp ──► rpc-server、apps/sqarvo-mcp ──► proto/tonic/prost 生成类型
```

包内模块分工（每个模块只允许对应泳道改，跨泳道只经公开签名）：

| 模块 | Issue | 职责 |
|---|---|---|
| `main.rs` | #207 | CLI 形态、runtime 装配、stderr 日志、退出码 |
| `config.rs` | #207 | `mcp.json` 模型、启动期校验、限额钳制、工具开关 |
| `access.rs` | #207 | alias 授权、懒连接状态机、并发许可、会话失效重试 |
| `credentials_cmd.rs` | #207 | 密钥环 `set` / `delete`（交互终端独占） |
| `server.rs` | #207 + #209/#210 | `ServerContext` 装配、stdio 生命周期、`tools/list` 过滤、`resources/*` 路由 |
| `error.rs` | #207 | `Error` → 工具级 `isError` / 协议级 JSON-RPC error 两通道映射 |
| `audit.rs` | #207 | JSONL 审计写入、轮转、跨进程锁与退化路径 |
| `policy/` | #208 | SQL AST 只读判定 + 规范化指纹（**公开签名是跨泳道契约**） |
| `tools/` | #209 | 8 个工具与 `Value` → JSON 单点序列化 |
| `resources/` | #210 | 4 类资源、URI 编解码、授权 |
| `tests/` | #211 | 协议兼容矩阵、策略快照、审计并发与脱敏 E2E |
| 文档 | #212 | 本文 + [docs/api/mcp-tools.md](../api/mcp-tools.md) + 需求状态回填 |

## 4. 配置与凭据的三层分离

秘密 ≠ 引用 ≠ 授权。三层各自独立存放、独立撤销：

| 层 | 载体 | 内容 | 谁写 |
|---|---|---|---|
| 秘密 | 操作系统凭据库（Linux Secret Service / Windows Credential Manager） | 数据库密码本体 | `sqarvo-mcp credentials set`（终端交互，不回显，两次确认） |
| 引用 | `mcp.json` 的 `connections[].credential_ref` | `sqarvo:conn/<uuid v4>`（ADR 0012 格式） | 用户手工粘贴 |
| 授权 | `mcp.json` 的 `enabled` + `enabled_tools` + `query_limits` | 哪个连接可用、哪些工具可用、限额多严 | 用户 |

配置文件 `mcp.json` 的**默认路径以 `AppPaths::config_dir()` 实测返回值为准**（Linux 本机实测：
`~/.config/sqarvo/mcp.json`；数据目录实测 `~/.local/share/sqarvo/`，审计在其下 `mcp/`）。
注意 `crates/local-store/src/paths.rs` 的模块注释写的是 `ProjectDirs::from("io","sqarvo","Sqarvo")`
语义（macOS 才是 `io.sqarvo.Sqarvo`），Linux 上 `directories` 6.0.0 会把项目路径**小写化并丢掉
qualifier/organization**，实测目录名是 `sqarvo`——注释与实现有出入（ADR 0032 已知限制 2），
文档与排障一律以实际返回值为准。`--config` 可指向任意路径（推荐 Agent 客户端就这么配）。

三层各自的失效行为：

- 密钥环不可用（没装 Secret Service、会话被锁）→ `credential.unavailable`，**绝不回退明文、
  绝不回退到内存或磁盘缓存**；服务照常启动，只是该连接连不上。
- 引用格式非法 → 启动期拒绝，错误文案**不回显引用原文**。
- 授权撤销 → 把 `enabled` 改成 `false` 重启进程即可，Agent 侧立刻从 `list_connections` 里看不
  到这条连接，调用返回 `mcp.connection_denied`。

字段逐项含义、类型、默认值、非法处理见 [docs/api/mcp-tools.md §6](../api/mcp-tools.md)。

## 5. Lazy 连接状态机

启动一个数据库都不连。每个 alias 一个槽位，状态迁移：

```text
                首次工具调用（授权通过后）
  not_connected ────────────────────────────► connected
        ▲                                       │
        │            连接失败                    │ 会话被 disconnect / 驱动报会话已消失
        ├───────────────────────────────────────┤
        │                                       ▼
        │            下次调用仍重试           failed（记子码 + 分类）
        └───────────────────────────────────────┘
```

- `not_connected`：`list_connections` 与 `database://connections` 的初始状态；**读状态不触发
  网络**，这是「Agent 想数连接数」不会打爆数据库的前提。
- `connected`：缓存 `(ConnectionId, SessionId)`，但句柄每次回查 `SessionManager`——缓存
  `Arc<SessionHandle>` 会让「已被 disconnect」对缓存不可见。
- `failed`：只存**稳定子码 + 分类**（如 `credential.not_found` / `unavailable`），不存驱动原始
  文案（可能含主机与用户名），也不是终态：下次调用照旧重试。
- 同一 alias 至多一条会话：建连过程在 per-alias `tokio::sync::Mutex` 里并二次检查，否则两个
  并发首调会各建一条真实连接、留下无人引用的会话。锁粒度只在单个 alias 内，跨 alias 不互相
  阻塞（AGENTS §11「不跨 await 持锁」的定点例外，理由即此）。
- 断连**只重试一次**，且只认明确信号：`session.not_found`、`transport.failed`、分类
  `Unavailable`、驱动自定义的 `*.session_closed`。语法错误、权限拒绝一律原样返回——把语法
  错误当断连重试，等于让「同一条 SQL 跑两遍」变成默认行为。

## 6. 并发模型（三层，互不替代）

| 层 | 落点 | 默认 / 上限 | 满了怎么办 |
|---|---|---|---|
| 1. 进程级信号量 | `AccessService` 的 `tokio::sync::Semaphore` | 2 / 4 | **排队等待**，不是报错 |
| 2. 查询登记表 | `sqarvo-query-engine::QueryRegistry`（`max_concurrent` 与信号量**同源**） | 同上 | 登记失败即返回错误 |
| 3. 会话执行锁 | `SessionHandle` 内部 `Arc<tokio::Mutex>`（[ADR 0008](../decisions/0008-session-concurrency.md)） | 每 alias 同时 1 条在途语句 | 后来的调用排队 |

两层数值必须同源（`EffectiveLimits::query_limits()` 一份算出两处），否则登记表先放行、信号量
后阻塞，等于把并发上限抬到 `max_concurrent` 之外。

「同一连接上的查询串行执行」是**语义**而不是故障，因此许可耗尽时排队而不是立刻失败；这条口径
同时写进了 server 的 `instructions` 与每个工具的描述，免得 Agent 把等待误判成系统错误去重试。

结果取消链：客户端断开或发 `notifications/cancelled` → rmcp 的请求取消令牌 → `run_sql` 里
`select!` 命中 → 先 `SessionHandle::cancel`（MariaDB 是 `KILL QUERY`，SQLite 是 interrupt）、
释放登记 → 返回 `query.cancelled`。超时链是同一条路：外层 `tokio::time::timeout` 到点 → 取消
→ `mcp.query_timeout`。

**驱动级单次执行超时不可用**（已知限制，不是设计缺口）：`ExecuteRequest.timeout` 传非 `None`
时 MariaDB 驱动在 `serve_query` 里显式返回 `capability.unsupported`（SPI 没有「支持单次超时」
的能力位），SQLite 也不保证等价语义，因此 MCP 与 `rpc-server` 走同一条既有路径——外层计时 +
`cancel` 真打断。超时仍然由服务端强制，不依赖 Agent 自觉。

## 7. SQL 安全分层（纵深，缺一不可）

AST 判定只是第一层，它的定位是「把明显的危险形态在进驱动前挡住」，不是最终边界：

```text
Agent 提交的 SQL
   │
   ├─ ① 授权：alias 存在且 enabled ──────────── 否 → mcp.connection_not_found / _denied
   ├─ ② AST 只读策略（sqlparser 0.64 + MySqlDialect）
   │      解析失败 → mcp.sql_parse（fail-closed）
   │      语句数 ≠ 1、非 Query 顶层、INTO、锁读、hint、危险函数、:= 赋值 → mcp.readonly_violation
   ├─ ③ 服务端限额：200 行默认 / 1000 硬上限、1 MiB、10 s（超时即取消）
   ├─ ④ 驱动层：标识符引用、参数化、只读事务与会话语义
   └─ ⑤ 数据库账号：只给 SELECT 的专用账号（运维侧，见 mcp-tools.md 的 SQL 模板）
```

判定要点（完整矩阵与理由见 [ADR 0033](../decisions/0033-sql-parser-readonly-policy.md)）：

- 顶层 match **穷举**：只有 `Statement::Query`（SELECT / 只读 WITH）进入深检，其余一律拒绝，
  `_ =>` 兜底保证 sqlparser 将来新增语句类型默认被拒。**禁止按首关键字判断**。
- 一次 `visitor` 全树遍历（`pre_visit_statement/query/select/expr` 四钩子）覆盖任意深度：
  手写「顶层 + CTE」递归实测会漏 `FROM` 派生表里的 `INTO` 与子查询里的 `SLEEP()`。
- 拒绝面：一切 `SELECT ... INTO`（会话变量 / 列清单 / 表 / `OUTFILE` / `DUMPFILE`）、锁定读
  （`FOR UPDATE` / `FOR SHARE` / `NOWAIT` / `SKIP LOCKED` / `OF`）、优化器 hint、数据修改 CTE、
  `:=` 会话变量赋值、危险函数表。
- 危险函数表（按 `ObjectName` **末段**小写匹配，覆盖 `db.fn` 与反引号变体）：`LOAD_FILE` /
  `SLEEP` / `BENCHMARK` / `GET_LOCK` / `RELEASE_LOCK` / `RELEASE_ALL_LOCKS` / `NEXTVAL`。动态
  函数名与空函数名无法静态判定 → 保守拒绝。
- 递归深度保持 sqlparser 默认保护（limit=50，约 24 层派生表触顶），触顶按解析失败拒——刻意
  不调高，这条深度边界与 10 s 超时、行数上限叠加构成 DoS 缓解。
- **MySqlDialect ≠ MariaDB**：0.64 没有 `MariaDbDialect`。若干 MariaDB 合法语法靠「解析失败」
  被拒（`LOCK IN SHARE MODE`、`INTO OUTFILE/DUMPFILE`、`ANALYZE SELECT`、`ALTER SEQUENCE`、
  `INDEX ... INVISIBLE`、派生表内 `FOR UPDATE`）。其中 `PROCEDURE ANALYSE()` 是**唯一真正的
  只读误杀**，接受它，不做例外放行。
- fail-closed 边界是跟进合同：上述靠解析失败拒的形态在测试里逐条断言 `mcp.sql_parse`，上游
  一旦能解析它们测试就变红，届时**必须**在 `ast.rs` 补显式拒绝、不许顺手放行。

`explain_query` 只有一条路：先过与 `execute_readonly_query` **完全相同**的策略，再由策略层拼
`EXPLAIN ` 前缀。用户自带 `EXPLAIN` / `ANALYZE` 在顶层白名单就被拒，`ANALYZE SELECT`（MariaDB
会真实执行）靠解析失败兜底拒。绝不构造 `EXPLAIN ANALYZE`。

## 8. 审计

| 项 | 值 |
|---|---|
| 路径 | `AppPaths::data_dir()/mcp/audit.jsonl`（Linux 实测 `~/.local/share/sqarvo/mcp/audit.jsonl`） |
| 权限 | 文件 0600、目录 0700（已存在的宽权限文件会被收紧，收紧失败直接报错而不是继续写） |
| 轮转 | 10 MiB × 5，**只在完整记录边界**轮转（绝不出现半行），超限丢弃最旧一份 |
| 记录范围 | 每次 `tools/call`，成功与失败都记；**V1 资源读取（`resources/*`）不写审计** |
| 字段 | `timestamp` / `request_id` / `client_info` / `tool_name` / `connection_alias` / `database` / `sql_fingerprint` / `execution_time_ms` / `returned_rows` / `truncated` / `success` / `error_code`（分类） / `error_subcode`（稳定子码） |
| 默认不记 | 完整 SQL、查询结果、密码、Token、`credential_ref` 指向的秘密、错误文案 |

设计取向：

- **结构上不可能泄漏 SQL**：`AuditRecord` 只有 `sql_fingerprint` 一个字符串字段，没有放原始
  SQL 或结果集的入口，调用方无法「顺手多记一点」。
- 分类与子码**分开存**：`error_code` 放分类（`permission_denied`）、`error_subcode` 放稳定子码
  （`mcp.readonly_violation`），按分类统计与按子码定位两种用途都不用二次映射。
- 审计写失败要可见：成功路径写不进审计 → 结果不交付，返回 `mcp.internal`；失败路径写不进审计
  只记日志，不覆盖原本要回给 Agent 的错误。
- 指纹 = 字面量掩码成 `?` → AST unparse → SHA-256 小写 hex。同形变体同指纹、再解析不动点；
  `IN` 列表**长度参与指纹**（3 元素与 2 元素是不同指纹，与 pt-fingerprint 不同，属已锁定行为）。

## 9. 协议兼容矩阵（实测结论）

实测载体：`tests/protocol_matrix.rs` 用裸 JSON-RPC 行驱动真实二进制（不经 SDK 客户端，才能发
「缺 `_meta`」这类形状），另有 MCP Inspector `@2.13` 手工跑通 `tools/list` 与 `resources/list`，
`--strict` 0 errors / 3 advisory warnings。

| 场景 | 结果 |
|---|---|
| 旧握手 `2024-11-05` / `2025-03-26` / `2025-11-25` | `initialize` → `notifications/initialized` → `tools/list` → `resources/list` → `tools/call` → `resources/read` → EOF 全流程通过，exit 0 |
| `2026-07-28` + `server/discover`（带 `_meta`） | 返回 `supportedVersions`（含 `2026-07-28`、`2025-11-25`、`2024-11-05`）与 capabilities；此后 EOF 走「握手前对端关闭」收尾，**exit 0**（#220 修复前是非零） |
| `2026-07-28` 无握手：每请求带裸 `_meta` | `tools/list`、`resources/list`、`tools/call` 直接可用；EOF 后正常 `quit_reason=Closed`、exit 0 |
| 裸请求缺 `_meta` | JSON-RPC `-32602`，消息点名全部缺失键（`io.modelcontextprotocol/protocolVersion`、`io.modelcontextprotocol/clientCapabilities`） |
| 裸请求 `_meta.protocolVersion = 2099-01-01` | `-32022` + `data.supported` 列表 + `data.requested` |
| `initialize(2026-07-28)` | 协商降为 `2025-11-25`（会答 initialize 的服务器不能自称 LATEST）；降级会话不再播 2026 的缓存字段 |
| `initialize(未知版本)` | 不报错，回落 `LATEST_WITH_INITIALIZE = 2025-11-25` |
| 未知工具名 | 协议级 `-32602`（`tool not found`）；被 `enabled_tools` 禁用的工具**仍可路由**，返回工具级 `mcp.tool_disabled`，而不是「名字消失」 |
| 握手前 EOF（连上管道就走） | exit 0 + info 级收尾日志（V3.1 §13） |
| 握手后 `SIGTERM` / stdin 关闭 | exit 0，日志含「优雅关停」与「会话已全部关闭」 |

`2026-07-28` 的缓存字段（`ttlMs` / `cacheScope`）只在协商版本 ≥ 该版本时才播；`resultType` 等
形状在两种版本下保持一致，避免同一工具被描述成两种东西。

## 10. 已知限制（全集）

分三类：刻意的设计代价、rmcp/依赖造成的缺口、未验证面。

### 10.1 刻意的设计代价

1. **审计多实例不是强一致**：`flock(LOCK_EX|LOCK_NB)` 只串行化「追加一行」，不保护跨进程轮转。
   同用户跑两个实例时，第二个退化写 `audit.<pid>.jsonl`（不阻塞、不丢记录），但轮转窗口内两个
   实例仍可能互相覆盖。V1 **不宣称**多实例安全。
2. **审计只到指纹级**：事后复盘看不到 Agent 具体查了哪一行，换来审计文件本身不是敏感数据副本。
3. **轮转参数不可配**：10 MiB × 5 是代码常量，测试靠构造器注入小阈值。开放配置需同时补配置文档与钳制规则。
4. **V1 资源读取不写审计**：只有 `tools/call` 记审计。资源面同样能读结构，因此这是已知缺口，
   不是「资源不需要审计」。
5. **懒连接的代价**：首个工具调用要付一次握手延迟（可能还要等用户解锁密钥环）。
6. **同一连接串行**：并行度受 ADR 0008 约束，Agent 侧表现为排队；要并发就配多条 alias（各自
   独立会话），而不是指望单连接并发。
7. **`explain_query` 仅 MariaDB**：Driver SPI 没有 Explain 方法，V1 不扩 SPI，在 MCP 层按
   驱动白名单（`EXPLAIN_DRIVERS = ["mariadb"]`，**V1 已知例外**）生成普通 `EXPLAIN`；SQLite
   返回 `mcp.unsupported`（`EXPLAIN QUERY PLAN` 语义不同，本版不代执行）。后续应以驱动能力位取代。
8. **`enabled_tools` 的未知名判定依赖 `config::V1_TOOL_NAMES`**：注册表与校验用同一份常量，
   漂移会让合法配置被拒或多出无法发现的死名字。
9. **`outputSchema` 里单元格是空对象 schema**：`rows` 的单元格写成 `{"items":{"items":{}}}`
   而不是 derive 形态的布尔 `true`——JSON Schema 2020-12 里布尔合法，但 MCP 的 outputSchema
   语境要求对象形态（Inspector `--strict` 会判 invalid）。运行时序列化不受影响。
10. **SQL 策略的只读误杀**：`PROCEDURE ANALYSE()` 会被拒（§7）。

### 10.2 rmcp / 依赖造成的缺口

11. **握手前收到 `SIGTERM` 仍走系统默认处置**：信号处理器在 `serve()` 返回**之后**才注册，而
    `serve()` 会一直等 initialize。这段窗口里进程被立即终止——此时既无会话也无在途查询，无需
    清理，但不是「优雅关停」。要覆盖它需要 `tokio-util` 的 `CancellationToken`（本包按改动
    确认单只能用 workspace 既有依赖，故改用 `serve()` + `RunningService::cancellation_token()`
    + `waiting()` 这条等价路径），把 `tokio-util` 纳入 workspace 依赖才能修。
12. **Windows 没有可捕获的 SIGTERM**：只有 Ctrl-C（与 ADR 0002 附记同口径）。
13. **Windows 审计跨进程锁跳过**：`flock` 只在 unix 目标启用，Windows 侧不加锁直接写（best-effort）。
14. **`psm`/`stacker` 含汇编产物**：`sqlparser` 的递归保护依赖它，Windows 构建风险由 #218
    跟踪的 Windows Rust CI 作业负责暴露；失败回退方案（去掉 recursive-protection 特性 + 自设
    输入规模边界）需要新 ADR，因为那会把 fail-closed 语义从「递归上限」换成「输入规模上限」。

### 10.3 未验证面（诚实清单）

15. **Windows / macOS 未实机验证**：本机只有 Linux。macOS 未进 CI，Windows 只有前端 .NET 作业
    在 CI 里（Rust 侧 #218 未落）。
16. **Cherry Studio GUI 协商 = 人工验收项**：协议层用裸 JSON-RPC 与 Inspector 覆盖，但真实 GUI
    客户端的展示与断连行为需要人工确认。
17. **真实 OS 密钥环的失败路径 E2E 本机不可用**：没有可用的 Secret Service 会话，改用双断言
    替代（装配层返回 `credential.unavailable` + 非交互 stdin 在打开密钥环**之前**就报错）。
18. **`--config` 指向不存在文件与默认路径缺失是两条不同告警文案**，都按「空连接列表启动」处理，
    未在真实客户端里逐一确认展示效果。

验收证据与复现命令集中在 [docs/api/mcp-tools.md §10](../api/mcp-tools.md)。

## 11. 相关文档

- [docs/api/mcp-tools.md](../api/mcp-tools.md)——8 个工具、4 类资源、`mcp.json` 逐字段、
  凭据 CLI、只读账号 SQL 模板、客户端配置、错误对照表、构建运行命令。
- [ADR 0032](../decisions/0032-mcp-standalone-process.md)——独立进程与连接模型的备选方案与取舍。
- [ADR 0033](../decisions/0033-sql-parser-readonly-policy.md)——SQL 只读策略的十条决策与代价。
- [ADR 0008](../decisions/0008-session-concurrency.md)、
  [ADR 0007](../decisions/0007-cell-truncation.md)、
  [ADR 0006](../decisions/0006-multi-statement-policy.md)、
  [ADR 0012](../decisions/0012-credential-reference.md)。
- [架构总览](overview.md)、[本地 IPC](local-ipc.md)、
  [第三方依赖许可声明](../licenses.md)。
