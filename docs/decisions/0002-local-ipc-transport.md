# ADR 0002：本地 IPC 与访问控制

- 状态：已接受（Windows 方案待验证）
- 日期：2026-11-04

## 背景

前后端是本地两个进程。数据库接口一旦被同机其他用户或误暴露到网络访问，等同于
把用户数据库交给攻击者。必须在第一阶段就确定传输与认证方案。

## 备选方案

| 方案 | 优点 | 问题 |
|---|---|---|
| 回环 TCP（无认证） | 实现最简单 | 任何本地进程可访问；误配 0.0.0.0 即暴露 |
| 回环 TCP + token | 跨平台简单 | 端口可能被其他程序占用/探测；需随机端口 |
| Unix 域套接字 + 文件权限 | 文件系统级访问控制，无端口 | Windows 无此机制；路径长度限制 |
| Named Pipe + ACL | Windows 原生、ACL 精确 | 与 gRPC 传输组合需额外适配 |
| XPC / D-Bus | 平台原生 | 绑定平台，抽象成本高 |

## 决策

- Linux / macOS：Unix 域套接字，父目录 0700、socket 0600；
- 所有平台：额外要求 256 位随机 token（metadata `x-sqarvo-token`），端点文件 0600；
- Windows：暂用 `127.0.0.1:0` + token，显式拒绝非回环地址；Named Pipe + ACL 列为后续工作；
- 传输实现隐藏在 `LocalListener` 之后，业务层与前端不感知具体机制。

## 理由

- Unix socket 的文件权限由内核强制，不依赖应用逻辑；
- token 提供纵深防御：即使权限被放宽或 Windows 使用 TCP，未持有 token 的进程仍无法调用；
- 拒绝非回环地址是硬性检查，避免配置错误导致数据库凭据暴露到局域网。

## 取舍

- 多一层 token 管理（生成、写入、读取、失效）；
- Windows 方案仍有 ACL 缺口，文档与 SECURITY.md 中明确标注；
- 前端需要处理「后端未启动 / 端点文件残留 / 协议版本不匹配」等状态。

## 重新评估条件

- Named Pipe 与 gRPC 的组合验证通过后，Windows 切换并删除回环 TCP 默认路径；
- 若出现同机多用户共享运行目录的场景（例如远程桌面服务器），需引入每用户命名空间隔离。

## 附记：Windows 实现（2026-11-11）

上面的决策（Windows 暂用 `127.0.0.1:0` + token）不变。本附记只追加**进程生命周期与
端点文件归属**，因为「Windows 上没有 `SIGTERM`」会直接改变「谁负责收尾」的答案。
原文不改写，冲突时以本附记为准。

### 职责划分

| 事项 | 负责方 | 说明 |
|---|---|---|
| 前端退出后不留下孤儿后端 | 前端 | 前端创建 Job Object 并设置 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，把后端放进该 Job；前端进程一结束，内核连带结束后端。后端**不自建** Job：手工启动的后端不应被某个无关 Job 连带结束。**此项目前尚未实现**，属于 Windows 前端任务。 |
| 父进程消失检测 | 后端 | `crates/parent-watch`：`OpenProcess(SYNCHRONIZE \| PROCESS_QUERY_LIMITED_INFORMATION)` + 创建时间校验（防 pid 复用）+ `WaitForSingleObject(handle, INFINITE)`；句柄打不开即按「父已消失」处理，且只判一次，不重试、不空转。 |
| Ctrl-C | 后端 | Windows 控制台 `CTRL_C_EVENT` 由 `tokio::signal::ctrl_c` 捕获，与 Unix 的 `SIGINT` 走同一条清理路径。 |
| `TerminateProcess` | 无 | 后端**不会**执行任何清理：不 `sessions.close_all()`、不删端点文件。Win32 没有可被普通进程捕获的等价 `SIGTERM`，这是语义而非缺陷。调用 `TerminateProcess` 的一方负责收尾。 |
| 端点文件删除 | 后端（正常退出）+ 前端（兜底） | 后端活着退出的三条路径（Ctrl-C / 父进程消失 / 服务自行结束）都删自己的端点文件；被 `TerminateProcess` 之后残留的文件由前端删除，或由前端凭文件中的 `pid` 判活后当作残留忽略。 |

### 端点路径由显式参数决定

Windows 没有 `XDG_RUNTIME_DIR`，靠环境变量推导会让「后端写哪儿」和「前端读哪儿」依赖
两套约定恰好一致。因此后端新增显式参数：

```text
sqarvo-backend --parent-pid <前端 pid> --endpoint-file <绝对路径>
```

优先级：`--endpoint-file` > `SQARVO_ENDPOINT_FILE` > 默认运行时目录。`SQARVO_ENDPOINT_FILE`
保留为 **Unix 兼容口径**（Linux 前端历史上依赖它 + `XDG_RUNTIME_DIR` 拉起后端），新代码
不应再用它传路径。Windows 前端必须显式传 `--endpoint-file`；目录由前端创建，后端写入时
仍会把目录收紧到 0700、文件收紧到 0600（Unix）并依赖用户目录 ACL 继承（Windows 的显式
DACL 仍未实现，见 R-07）。

### 仍待 Windows 实机验证

- `OpenProcess` + 创建时间校验 + `WaitForSingleObject` 的监视行为。仓库 CI 只在 Linux 上跑，
  当前唯一的跨平台证据是 `cargo check -p sqarvo-parent-watch --target x86_64-pc-windows-msvc`
  （类型检查，不执行）。
- `TerminateProcess` 之后端点文件残留与前端清理路径；
- Job Object 的 `KILL_ON_JOB_CLOSE` 是否覆盖后端可能派生的辅助进程（当前后端没有子进程，
  预期够用，但需要在真实 Windows 上确认）。
