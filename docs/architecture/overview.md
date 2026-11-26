# 架构总览

[前端平台状态](../../frontend/platforms.toml)只决定主线开发与发布范围。Windows 重写独立推进，
macOS 保留规划。Rust 后端和 MCP 的跨操作系统兼容性独立维护。

## 进程模型（Phase 1）

```text
┌──────────────────────────┐        gRPC over 本地 IPC        ┌──────────────────────────┐
│ 原生前端（三平台各自实现） │  ──────────────────────────────► │ 后端进程 sqarvo-backend   │
│ 原生 UI（当前 GTK4）      │  ◄────────────────────────────── │ Rust + Tokio + tonic      │
└──────────────────────────┘   token: x-sqarvo-token         └────────────┬─────────────┘
        │                                                                  │ Driver SPI
        │ 系统能力：凭据存储、文件选择、通知                                 ▼
        └──────────────────────────────────────────────────►  ┌──────────────────────┐
                                                              │ 数据库驱动（内置 crate）│
                                                              └──────────────────────┘

┌──────────────────────────┐   spawn 子进程 + stdio（JSON-RPC）  ┌──────────────────────────┐
│ MCP 客户端（AI Agent 宿主）│ ───────────────────────────────► │ sqarvo-mcp（独立进程）    │
└──────────────────────────┘ ◄── stdout 只归协议 / stderr 日志 ── └────────────┬─────────────┘
                                                                              │ 同一套 Driver SPI
                                                                              ▼
                                                                      数据库（MariaDB / SQLite）
```

- 前端负责窗口、编辑器、结果网格、平台集成与后端生命周期；
- 后端负责连接、会话、执行、取消、元数据、事务与结果集管理；
- 前端**不实现**任何数据库协议，也不直连数据库；
- 后端**不包含**任何平台 UI 依赖；
- MCP 客户端启动独立的 `sqarvo-mcp` 进程。该进程自行持有数据库会话，stdin 关闭时释放会话并退出。
  GTK 经 `ConnectionProfileService`，MCP 与管理 CLI 直接经 `ProfileStore`，访问同一份共享档案文档。
  [MCP 架构](mcp.md) · [共享档案与接口](../api/connection-profiles.md)。

## 后端 crate 依赖方向

```text
rpc-server ──► connection ──► drivers ──► driver-api ──► common
     │              │                          ▲
     │              └──► local-store           │
     ├──► local-store（档案/策略、凭据写入）
     ├──► query-engine                          │
     ├──► metadata ─────────────────────────────┘
     └──► transaction ──► common

apps/sqarvo-mcp ──► connection / query-engine / local-store / drivers / driver-api / common
                └─► rmcp（stdio 传输与 tools/resources 协议面）
```

约束：

- `driver-api` 不依赖 tonic、tokio 之外的业务 crate，不泄露第三方驱动类型；
- `rpc-server` 只做协议翻译与错误映射，业务规则在各自 crate；
- `sqarvo-mcp` **不得**依赖 `rpc-server`，也不得把 prost/tonic 生成类型带进工具接口
  （ADR 0032）；
- 具体驱动以 workspace crate 形式编译进后端，暂不引入 Rust 动态库 ABI。

> 现状说明：`metadata` 与 `transaction` crate 已实现可测试的通用逻辑，但尚未被
> `rpc-server` 直接调用——对应 RPC 当前返回 `UNIMPLEMENTED`，等驱动能力接入后再连线。
> 图中这两条边属于目标形态。

## 数据流：执行一次查询（目标形态）

1. 前端提交 SQL 编辑器的本次执行范围，调用 `QueryService.Execute(session_id, sql)`；
2. 后端 `QueryRegistry` 登记查询（并发上限、批次上限），获取 `query_id`；
3. `SessionManager` 找到对应 `DriverSession`，由驱动执行；
4. 驱动返回首批行 + 游标；后端只把首批 + 列元数据返回前端；
5. 前端滚动到尾部时调用 `FetchBatch(query_id, page_token)`；后端向驱动拉下一批；
6. 用户取消 → `Cancel(query_id)` → 幂等取消，错误分类为 `cancelled`；
7. 结果读完或前端关闭 → `Close(query_id)` → 释放驱动游标与内存。

## 类型与结果集原则

- `NULL`、空字符串、零值三者必须可区分；
- Decimal 以「未缩放整数 + 标度」传输，绝不转 `double`；
- 时间戳区分「带时区」与「无时区」；
- 二进制与超大文本默认截断或延迟加载，并明确标注 `truncated`；
- 无法无损映射的类型保留原始类型名与文本表示，前端标注「未完全支持」。

## 错误模型

统一错误包含稳定错误码与分类。gRPC 层把分类映射为状态码，并把错误码放入
trailing metadata `sqarvo-error-code`；前端据此分支处理，而不是解析文案。
详见 [gRPC 约定](../api/grpc-conventions.md)。

## 生命周期与清理

- 每个查询、会话、游标都必须有清理路径；
- 后端收到 SIGINT/SIGTERM 后停止接受连接、关闭会话、删除 socket 与端点文件；
- 前端收到 `SIGTERM` / `SIGINT` / `SIGHUP` 时走**与关窗口相同**的清理路径
  （通知后端优雅退出 → 超时再 `SIGKILL` → 删除端点文件），不会留下孤儿后端（#146）；
- 前端启动后端时追加内部参数 `--parent-pid <前端 pid>`：后端据此监视父进程
  （Linux：`PR_SET_PDEATHSIG` + 500 ms `getppid()` 轮询；Windows：`OpenProcess` +
  进程创建时间校验 + `WaitForSingleObject`，实现见 `backend/crates/parent-watch`，
  语义见 ADR 0002 附记），前端被 `SIGKILL`（来不及清理）时后端自行退出；
  没有该参数的后端（手工启动、多前端共用）**不**监视父进程，因此不会被误杀；
- 崩溃残留：启动时只清理「确认为 socket」的残留文件，绝不删除未知文件；
- 前端断开后，后端仍需回收未释放的查询与会话（后续阶段实现租约）。

## 尚未实现的部分

- 驱动、执行、元数据查询、事务绑定；
- 凭据存储；
- 前端与后端的连接；
- Windows Named Pipe、macOS 传输方案评估。

这些内容在 `docs/product/roadmap.md` 中跟踪，且不会以「可用」状态出现在 UI 中。

## 相关文档

- MCP 通道（独立 stdio 进程、只读工具 / 资源、协议兼容矩阵）：[mcp.md](mcp.md)、
  [工具与配置参考](../api/mcp-tools.md)。
- 第三方依赖的署名与许可义务（含 `sqlparser-rs` 的 NOTICE）：[licenses.md](../licenses.md)。
