# ADR 0035 共享连接档案与 MCP 全局访问

- 状态：已接受（2026-11-18，改动确认单 #2）
- 关联：REQ-CONN-009/010、REQ-MCP-007/008/009，[M10](https://github.com/jacketzc/Sqarvo/milestone/11)。
- 上游：[ADR 0008](0008-session-concurrency.md)、[ADR 0012](0012-credential-reference.md)、
  [ADR 0016](0016-credential-write-rpc.md)、[ADR 0032](0032-mcp-standalone-process.md)、
  [ADR 0033](0033-sql-parser-readonly-policy.md)。

## 背景

GTK、Windows 和 MCP 原来各有连接档案。同一数据库的连接参数与凭据引用需要在多个入口重复维护，
用户也需要在 GUI 和终端撤销外部 Agent 对数据库的访问。MCP 的独立 stdio 进程、物理连接所有权和
V1 SQL 只读边界继续沿用。

## 备选方案

| 方案 | 取舍 |
|---|---|
| 继续由各入口维护档案 | 配置隔离，重复录入，跨进程授权依赖各配置的更新时机。 |
| GUI 后端持有档案，MCP 经 gRPC 查档 | 写入口集中，MCP 启停与 GUI 后端绑定。 |
| 共享档案，保留独立进程 | 档案和策略由唯一的存储实现负责，各进程独立管理数据库会话。需要文件锁与迁移。 |

## 决策

采用共享档案。同步 `sqarvo-local-store::ProfileStore` 负责
`AppPaths::config_dir()/connections.json`，schema v2。GTK 经本地 token 保护的
`ConnectionProfileService` 调用，MCP 和管理 CLI 直接复用存储实现。异步调用方在阻塞池处理文件 I/O。
档案 ID 为 UUID v4，编辑与删除以档案 revision 做乐观并发控制。冲突沿用
`FailedPrecondition`，子码为 `profile.revision_conflict`。

读操作使用共享锁，写操作使用独占锁。锁文件保持常驻，写者在锁内重读、合并、校验和推进 revision，
再同步唯一临时文件并原子替换。Unix 用 `flock`、rename 和父目录同步，Windows 用
`LockFileEx` 和 `MoveFileEx`，沿用已有依赖和 Rust MSRV。审计追加复用文件锁实现，
跨进程轮转仍保持原有约束。

顶层 `mcp.enabled` 默认关闭，`mcp.read_only` 默认开启。开启访问授权当前及以后新增的档案。
调用工具或读取资源时重新读取策略与档案，存储不可读时拒绝请求。连接定位优先精确 UUID，
其次唯一名称。窗口或 CLI 关闭访问后，MCP 在下一次需要授权的请求中拒绝访问并撤销缓存会话。
空闲物理连接随撤销关闭，已经获准开始执行的操作完成后释放连接。删除或修改档案时，请求级检查
也使旧会话失效。新增或修改的档案需重启 MCP 才能建连。
协议发现与数据库访问分别处理，Agent 只能调用 V1 只读能力。关闭只读开关仍保留 ADR 0033 的 AST 策略。

GTK、Windows v1 与旧 MCP 文件由同一迁移实现处理。规范化源路径与 SHA-256 指纹共同标识导入，
档案获得稳定 ID，凭据引用和备份保留。迁移忽略旧白名单的 `enabled` 值，绝不自动开启全局访问。
档案删除与凭据删除保持独立。

## 取舍与重新评估条件

- 请求级读取增加本地文件读取成本，政策变化对后续请求生效。未来若引入授权缓存，需要等价的撤销保证与测试。
- 新增或修改连接参数需重启 MCP；有明确的无中断需求时再评估会话热加载。
- GUI 和 CLI 的策略变更不进入 MCP 审计。当前审计重点仍是 Agent 的工具调用。
- Windows 前端接入冻结。此次交付契约、旧格式 fixture 与 Rust 侧迁移覆盖，Windows 和 macOS 实机未验证。
- 引入写工具前须另行评审权限、事务、确认交互、审计与数据库侧最小权限。

格式、迁移与 RPC 细节见 [共享连接档案接口](../api/connection-profiles.md)。
