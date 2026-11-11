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

### Windows（当前回退方案）

- 绑定 `127.0.0.1:0`（随机端口），**拒绝任何非回环地址**；
- 端点信息写入用户配置目录下的 JSON 文件；
- 会话 token 必填。

Named Pipe + ACL 是目标方案，但需要与 gRPC 传输层组合验证；在验证完成前不声称支持。

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

路径来源按优先级：显式 `--endpoint-file <PATH>`、环境变量 `SQARVO_ENDPOINT_FILE`
（Unix 兼容口径，Linux 前端历史上依赖它 + `XDG_RUNTIME_DIR`）、默认运行时目录。
Windows 没有 `XDG_RUNTIME_DIR`，前端必须显式传 `--endpoint-file`，否则「后端写哪儿」和
「前端读哪儿」只能靠两套约定恰好一致。目录由后端创建并收紧到 0700（Unix），Windows 上
依赖用户目录的 ACL 继承（显式 DACL 未实现）。

## 威胁模型

| 威胁 | 缓解 |
|---|---|
| 同机其他用户直接连接 socket | 目录 0700 + socket 0600 |
| 同机其他用户读取端点文件 | 文件 0600 |
| 权限被误放宽后调用后端 | token 拦截器 |
| 误配监听 `0.0.0.0` | `bind_tcp` 拒绝非回环地址 |
| 崩溃残留文件被利用 | 只删除确认为 socket 的文件；端点文件包含 pid 供前端判断 |
| 日志泄露 token | 日志只输出端点路径与传输方式；token 类型 Debug 脱敏 |

## 待办

- Windows Named Pipe + 显式 DACL；
- macOS 是否改用 Unix socket 以外的机制（评估 XPC）；
- 前端崩溃后后端会话的租约回收；
- 端点文件被替换的检测（前端应校验文件所有者）。
