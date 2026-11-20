# MCP Server 架构（`sqarvo-mcp`）

适用范围：`backend/apps/sqarvo-mcp`。正文以 stdio V1 为基础，本地 Streamable HTTP 的增量见末节。事实来源是代码与测试，本文不引入代码里没有的行为；
口径与代码冲突时以代码为准，并把它记在 §10「与代码不符项」。

- 决策记录：[ADR 0032](../decisions/0032-mcp-standalone-process.md)（独立进程与连接模型）、
  [ADR 0033](../decisions/0033-sql-parser-readonly-policy.md)（SQL 只读策略）、
  [ADR 0035](../decisions/0035-shared-connection-profiles.md)（共享档案与全局访问）。
- 对外契约（工具、资源、`mcp.json` 每个字段、错误子码）：
  [docs/api/mcp-tools.md](../api/mcp-tools.md)。
- 需求条目：`REQ-MCP-001 … 009`，见 [产品需求](../product/requirements.md) §9。

## 1. 定位与不变式

MCP Server 是**给外部 AI Agent 用的第二个入口**，不是第二套数据库系统。它复用与 GUI 后端
完全相同的连接、驱动、会话与查询生命周期代码，只在外面换了一层协议（stdio JSON-RPC）、一层
授权模型（共享档案中的全局策略）和一层安全策略（SQL AST 只读判定）。

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
│ 各类 Agent 运行时           │  ◄── stderr：tracing 日志 ─────── │ ProfileStore 共享档案        │
└────────────────────────────┘                                   └──────┬────────────┬────────┘
                                                                        │            │
                                                     SessionManager ────┘            └──── AuditWriter
                                                     （Driver SPI 内置驱动）                       │
                                                                        │                          ▼
                                                                        ▼                 data_dir()/mcp/audit.jsonl
                                                        数据库（MariaDB / SQLite 文件）
```

要点：

- **会话由 MCP 进程持有**。GTK 经本地 gRPC 和 MCP 经 `ProfileStore` 访问同一份共享档案。
  两者各自建立物理连接。MCP 客户端管理 stdio 子进程，在 stdin 关闭时触发退出。
- **传输使用 stdio**。客户端流量的访问由共享档案的全局策略控制。GTK 调用管理 RPC 继续使用
  本地 token 鉴权。[本地 IPC](local-ipc.md)。
- 同一二进制支持 stdio 服务和本地命令 `credentials set|delete`、`access`。管理命令单独退出，
  stdout 为终端结果，stdio 服务的 stdout 专用于协议。

### 2.1 启动与退出

| 阶段 | 行为 | 失败时 |
|---|---|---|
| 1. 装配日志 | subscriber writer 钉死 stderr；过滤级别默认 `warn,sqarvo_mcp=info`，可用 `SQARVO_MCP_LOG` 覆盖 | — |
| 2. 迁移并读配置 | 迁移旧档案，读取共享档案及服务设置。可用 `--config <path>` 指定服务设置或旧 MCP 文件 | 迁移、共享档案或显式指定配置无效时退出。默认服务文件缺失时采用默认设置 |
| 3. 语义校验 | 共享档案校验，`enabled_tools` 和限额钳制 | 拒绝启动，stderr 一行原因，**exit 1**，stdout 保持空 |
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
      ├──► local-store（ProfileStore、AppPaths、凭据引用与系统凭据库）
      └──► common（Error / ErrorCategory / ErrorCode 稳定子码）

禁止：apps/sqarvo-mcp ──► rpc-server、apps/sqarvo-mcp ──► proto/tonic/prost 生成类型
```

包内模块分工（每个模块只允许对应泳道改，跨泳道只经公开签名）：

| 模块 | Issue | 职责 |
|---|---|---|
| `main.rs` | #207 | CLI 形态、runtime 装配、stderr 日志、退出码 |
| `config.rs` | #207 | `mcp.json` 模型、启动期校验、限额钳制、工具开关 |
| `access.rs` | #207、#227 | 请求级授权、懒连接状态机、并发许可、会话撤销与失效重试 |
| `access_cmd.rs` | #225 | 终端管理 MCP 全局开关、确认和状态 |
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

| 层 | 载体 | 管理入口 |
|---|---|---|
| 密码 | 操作系统凭据库 | GTK 凭据流程或交互命令 `sqarvo-mcp credentials set` |
| 档案与引用 | `AppPaths::config_dir()/connections.json` 的 `profiles` | GTK 通过档案服务读写，MCP 直接读取 `ProfileStore` |
| 访问与只读 | 同一文件的 `mcp.enabled` 和 `mcp.read_only` | GTK 菜单或本地 `sqarvo-mcp access` |
| 工具开关与限额 | `mcp.json` 的 `enabled_tools` 和 `query_limits` | 用户可选配置，`--config` 可覆盖其路径 |

缺省策略为 `{ "enabled": false, "read_only": true }`。升级迁移旧档案时保留引用，旧连接白名单
不影响全局授权。密钥环不可用将影响建连，错误为 `credential.unavailable`。
[共享存储与迁移](../api/connection-profiles.md) · [MCP 服务配置](../api/mcp-tools.md#6-mcpjson-逐字段)。

### 4.1 请求级授权与撤销

1. 工具调用和 `resources/read` 读取共享档案与策略的同一快照。无权访问、文件损坏、锁等待超时或
   全局访问关闭时失败关闭。静态 `tools/list`、`resources/list` 和资源模板发现维持静态行为。
2. 指定连接时，依次以精确 UUID、唯一名称解析。名称歧义时使用 UUID。每个档案的会话槽位绑定
   MCP 启动时读取的档案 revision。
3. 全局访问关闭或文件无法读取时，请求级检查会撤销缓存会话。档案删除或 revision 变化时同样
   停止授权旧会话。空闲连接当场关闭，已授权且开始执行的操作可以完成，此后释放其物理连接。
   新增或修改的档案需要重启才可建连，相关调用返回 `profile.restart_required`。
4. 只读策略按操作类型检查。V1 只有读操作，始终使用 SQL AST 只读安全策略。

## 5. Lazy 连接状态机

启动一个数据库都不连。每个档案 UUID 一个槽位，状态迁移：

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
- 同一档案至多一条会话。建连过程在该档案的 `tokio::sync::Mutex` 里并二次检查，否则两个
  并发首调会各建一条真实连接、留下无人引用的会话。锁粒度只在单个档案内，跨档案不互相
  阻塞（AGENTS §11「不跨 await 持锁」的定点例外，理由即此）。
- 断连**只重试一次**，且只认明确信号：`session.not_found`、`transport.failed`、分类
  `Unavailable`、驱动自定义的 `*.session_closed`。语法错误、权限拒绝一律原样返回——把语法
  错误当断连重试，等于让「同一条 SQL 跑两遍」变成默认行为。

## 6. 并发模型（三层，互不替代）

| 层 | 落点 | 默认 / 上限 | 满了怎么办 |
|---|---|---|---|
| 1. 进程级信号量 | `AccessService` 的 `tokio::sync::Semaphore` | 2 / 4 | **排队等待**，不是报错 |
| 2. 查询登记表 | `sqarvo-query-engine::QueryRegistry`（`max_concurrent` 与信号量**同源**） | 同上 | 登记失败即返回错误 |
| 3. 会话执行锁 | `SessionHandle` 内部 `Arc<tokio::Mutex>`（[ADR 0008](../decisions/0008-session-concurrency.md)） | 每个档案同时 1 条在途语句 | 后来的调用排队 |

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
   ├─ ④ 驱动层：标识符引用、参数化、SQLite 连接的只读模式
   └─ ⑤ 数据库账号：只给 SELECT 的专用账号（运维侧，见 mcp-tools.md 的 SQL 模板）
```

MCP V1 查询沿用驱动的会话语义。MariaDB 的只读事务由显式事务调用开启，
MCP 查询依靠 AST 策略和数据库账号的最小权限。[只读账号配置](../api/mcp-tools.md#8-mariadb-只读账号模板)

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
| 记录范围 | 每次 `tools/call`。资源读取因全局策略、档案歧义或需要重启而被拒绝时也记录子码。GUI/CLI 策略变更不写 MCP 审计 |
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

1. **审计争用使用分进程文件**。非阻塞独占锁争用时退化到 `audit.<pid>.jsonl`。ADR 0036
   增加稳定的跨进程轮转锁，同版本进程覆盖整个轮转窗口。和只加文件锁的旧版混跑时仍有轮转竞争。
2. **审计只到指纹级**：事后复盘看不到 Agent 具体查了哪一行，换来审计文件本身不是敏感数据副本。
3. **轮转参数不可配**：10 MiB × 5 是代码常量，测试靠构造器注入小阈值。开放配置需同时补配置文档与钳制规则。
4. **资源读取审计范围有限**。全局策略拒绝、名称歧义和需要重启记入审计。普通资源读取沿用 V1 行为。
5. **懒连接的代价**：首个工具调用要付一次握手延迟（可能还要等用户解锁密钥环）。
6. **同一连接串行**：并行度受 ADR 0008 约束，Agent 侧表现为排队；并发发生在不同档案的独立会话之间，而不是指望单连接并发。
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
13. **Windows 审计跨进程锁未实机验证**。共用文件锁在 Unix 使用 `flock`，Windows 使用 `LockFileEx`。
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
- [共享连接档案接口](../api/connection-profiles.md) · [ADR 0035](../decisions/0035-shared-connection-profiles.md)。
- [ADR 0032](../decisions/0032-mcp-standalone-process.md)——独立进程与连接模型的备选方案与取舍。
- [ADR 0033](../decisions/0033-sql-parser-readonly-policy.md)——SQL 只读策略的十条决策与代价。
- [ADR 0008](../decisions/0008-session-concurrency.md)、
  [ADR 0007](../decisions/0007-cell-truncation.md)、
  [ADR 0006](../decisions/0006-multi-statement-policy.md)、
  [ADR 0012](../decisions/0012-credential-reference.md)。
- [架构总览](overview.md)、[本地 IPC](local-ipc.md)、
  [第三方依赖许可声明](../licenses.md)。

## 追加传输：本地 Streamable HTTP

[ADR 0036](../decisions/0036-local-mcp-http.md) 为本页 stdio 流程增加回环路径。无参数启动仍走 stdio。HTTP 子命令启动同一个 `SqarvoMcpServer` 并共享其进程内上下文，SDK 处理 HTTP JSON-RPC、版本协商、会话与 SSE。HTTP 新请求先验证回环 authority、Origin 和 Bearer，数据库工具和资源随后进入既有授权层。令牌轮换与数据库授权撤销分别生效。

GTK 主菜单的 MCP 管理页经本地认证 gRPC 调用 SystemService。Windows 前端保持现状。Linux 伴生后端从自身所在目录定位 `sqarvo-mcp`，监督启动、就绪、停止与回收。实际运行地址来自已绑定的子进程。端口冲突进入 Failed 状态，外部同端口监听保持其自身生命周期。手动启动的 MCP HTTP 进程只响应自己的关闭信号。

管理端口配置位于 `AppPaths::config_dir()/mcp-http-port`，缺省 8765，内容为单个十进制端口。地址固定 `127.0.0.1`，路径固定 `/mcp`，启动默认关闭。HTTP 凭据位于 Linux Secret Service 的独立 profile。新版本 stdio 与 HTTP 通过独立审计锁覆盖轮转，旧版本并行使用仍受历史轮转限制。


## V2 结构化写入

stdio 和 Streamable HTTP 共享同一工具分发、请求级档案授权和审计。工具执行按档案
UUID 固定目标，取得进程并发许可后重验授权。每次写入创建独立物理会话，完成后回收，
断连不自动重试。现有查询仍走只读 AST。V2 又将驱动生成的 DML 和绑定参数与结构化计划
做 AST 核对。预览使用独立的只读事务。认证 HTTP 会话关闭、令牌撤销和服务关停
会发出取消信号。可重连的 SSE 流中断与关闭 MCP 会话是不同操作。

事务内检查表资格、匹配范围和驱动返回的直接影响行数。SQLite 使用现有
`BEGIN IMMEDIATE`，并拒绝可能绕过直接行数统计的目标。MariaDB 使用可串行化事务，
需要精确档案修订的显式部署资格和由 DBA 提供的资格视图。账号权限和 DDL 隔离
的部署边界见 [ADR 0037](../decisions/0037-mcp-bounded-writes.md)。

一次性连接初始化与 BEGIN 可超时或取消。BEGIN 未获确认时，审计的
`not_started` 表示 DML 未发送。事务终结与一次性连接清理各有 5 秒等待预算。MariaDB 在超过预算后终止对应驱动任务，
以中断仍在等待应答的物理连接。提交无法确认时返回 `commit_unknown`，回滚无法
确认时返回 `rollback_unconfirmed`。两种结果都需要核查数据库和审计，客户端停止自动重试。
预览范围无法确定时返回范围状态。预览的连接、超时和权限失败走脱敏错误通道。

写入前持久化审计意图，提交前持久化范围，最后追加提交、回滚或未知状态。
数据库与 JSONL 各自持久化，缺少终态的审计记录需要人工核对。
