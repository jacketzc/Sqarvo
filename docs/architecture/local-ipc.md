# 本地 IPC 与访问控制

## 目标

前端与后端是两个进程，通信必须满足：

1. 只允许本机当前用户访问；
2. 不把数据库相关接口暴露到网络；
3. 传输实现可替换，业务层不感知 socket / pipe / TCP 差异；
4. 前端能可靠发现后端地址与凭据（启动顺序无关紧要）。

## 当前实现

### Linux / macOS：Unix 域套接字

- 路径：`$XDG_RUNTIME_DIR/sqarvo/backend.sock`（无 `XDG_RUNTIME_DIR` 时退化为数据目录 `run/`）；
- 父目录权限 `0700`，socket 文件权限 `0600`；
- 路径长度限制约 104 字节，启动前校验，超长直接报错而不是截断；
- 启动时若存在残留 socket 文件则删除；若被普通文件占用则拒绝启动，绝不删除未知文件；
- 正常退出时删除 socket 与端点文件。

### Windows（回环 TCP + 显式 DACL）

- 绑定 `127.0.0.1:0`（随机端口），**拒绝任何非回环地址**；
- 端点信息写入用户运行时目录下的 JSON 文件（`ProjectDirs` 解析，通常位于
  `%LOCALAPPDATA%`，与 Linux 的运行时目录同位）；
- 会话 token 必填；
- 访问控制：`local-store/src/win_acl.rs` 用 `SetNamedSecurityInfoW` 写入**显式
  DACL**——DACL 受保护（`PROTECTED_DACL_SECURITY_INFORMATION`，断开从父目录继承
  的 `Users` / `Authenticated Users` 等 ACE），唯一一条 ACE 是「当前用户 SID
  完全控制」。端点文件与 config/data/runtime 三个目录都收紧；目录的 ACE 带
  `OI/CI`，因此目录内新建的端点文件继承「仅当前用户」，而不是继承到空 DACL；
- 收紧失败返回可分类错误（`PermissionDenied` / `NotFound` / `Internal`）并让后端
  拒绝启动。端点文件里是 token，「先写出去、权限以后再说」等于把 token 交给同机
  其他用户；错误文案只含路径与 Win32 错误码，不含 token；
- 写入顺序：建空临时文件 → 收紧 DACL → 写入 token → `sync_all` → 原子 rename。
  这样不存在「token 已落盘、但仍按继承 ACL 对其他用户可读」的窗口期。

Named Pipe + ACL 仍是目标方案（可省掉 token 文件与端口），但需要与 gRPC 传输层
组合验证。

**Windows 侧验证状态**：本仓库的 CI 与开发机是 Linux，上述 DACL 代码只经过
`cargo check -p sqarvo-local-store --target x86_64-pc-windows-msvc`（含
`--all-targets`，即 `cfg(windows)` 的单测模块也通过类型检查）与 `win_acl` 的纯逻辑
单测（SDDL 形态、错误分类、文案不含 token）。`SetNamedSecurityInfoW` 的**真实效果**
必须在 Windows 实机上用 `icacls` / `GetNamedSecurityInfoW` 读回核对后，才能对外声称
「Windows 上已收紧」；在那之前按「已实现、待实机复核」表述。

## 会话 token

- 每个后端进程启动时生成 256 位随机 token（两个 UUIDv4 拼接，十六进制）；
- token 写入端点描述文件，文件权限 0600；
- 所有 RPC 必须携带 metadata `x-sqarvo-token`；
- 服务端以固定长度、无提前返回的方式比较（降低时序侧信道），不使用普通 `==`；
- token 不写入日志；`Debug` 实现固定输出 `<redacted>`。

这不是用户登录认证，而是本地进程访问控制。它保证：即使 socket 权限被放宽、
或 Windows 上使用回环 TCP，其他本机进程也无法调用后端。

## 端点描述文件

```json
{
  "transport": "unix",
  "socket_path": "/run/user/1000/sqarvo/backend.sock",
  "token": "……64 位十六进制……",
  "pid": 12345,
  "protocol_version": "v1",
  "service_version": "0.1.0",
  "created_at": "2026-11-04T02:00:00Z"
}
```

写入策略：临时文件 + `sync_all` + 原子 rename，避免读到半截 JSON。
读取方必须校验 `protocol_version` 主版本，并在 `pid` 已不存在时视为残留。

权限：Unix 上文件 `0600`、父目录 `0700`；Windows 上文件与父目录都由
`SetNamedSecurityInfoW` 写入显式 DACL（仅当前用户完全控制、断开继承），临时文件在
写入 token 之前就完成收紧。两侧失败都返回可分类错误并阻止后端启动——端点文件是
token 的唯一载体，权限没收紧就不能继续。

凭据（数据库密码）**不在此文件中**：配置里只有引用 `sqarvo:conn/<uuid-v4>`，密码存
系统凭据库（Linux Secret Service / Windows Credential Manager，见
`crates/local-store/src/credentials/` 与 ADR 0012）。Windows 侧条目是
`CRED_TYPE_GENERIC`、target name 为引用原文、`UserName` 固定 `Sqarvo`、blob 为密码
的 UTF-8 字节、`CRED_PERSIST_LOCAL_MACHINE`，单条上限 2560 字节。

## 威胁模型

| 威胁 | 缓解 |
|---|---|
| 同机其他用户直接连接 socket | 目录 0700 + socket 0600 |
| 同机其他用户读取端点文件 | 文件 0600；Windows 显式 DACL（仅当前用户、断开继承） |
| 同机其他用户抢先创建 runtime 目录/文件 | 目录 0700；Windows 目录 DACL + `OI/CI`，子对象继承「仅当前用户」 |
| 权限被误放宽后调用后端 | token 拦截器 |
| 误配监听 `0.0.0.0` | `bind_tcp` 拒绝非回环地址 |
| 崩溃残留文件被利用 | 只删除确认为 socket 的文件；端点文件包含 pid 供前端判断 |
| 日志泄露 token | 日志只输出端点路径与传输方式；token 类型 Debug 脱敏；ACL 错误文案不含 token |
| 日志泄露数据库密码 | 凭据只在系统凭据库；`Secret` 的 Debug 固定 `<redacted>`；错误链只带 Win32/D-Bus 错误码 |

## 待办

- Windows Named Pipe（DACL 已落地，管道化未做）；
- Windows 实机复核：显式 DACL 读回、凭据管理器往返（清单见
  `docs/planning/windows-frontend-parity-plan.md` WP1 与
  `crates/local-store/src/credentials/mod.rs` 模块文档）；
- macOS 是否改用 Unix socket 以外的机制（评估 XPC）；
- 前端崩溃后后端会话的租约回收；
- 端点文件被替换的检测（前端应校验文件所有者）。
