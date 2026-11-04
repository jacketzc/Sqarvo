# ADR 0014：Linux 凭据存储用 zbus blocking API 桥接同步 `CredentialStore`

- 状态：已接受
- 日期：2026-11-04
- 关联：S3 / M3、#21；ADR 0012（引用格式与生命周期）、ADR 0013（凭据传给驱动）

## 背景

`CredentialStore`（`backend/crates/local-store/src/credentials/mod.rs`）的 `get/set/delete`
是同步签名，Linux 侧此前只有 `UnavailableCredentialStore`。ADR 0012 已固定条目属性与错误
语义，但没有回答两个实现问题：

1. 用哪个库访问 `org.freedesktop.secrets`；
2. 同步 trait 与异步 D-Bus 之间怎么桥接，才不会在 Tokio worker 上 `block_on`。

## 备选

| 方案 | 问题 |
| --- | --- |
| `secret-service` crate（高层封装，自带 blocking 模块） | 多一层依赖；错误类型被包成自己的枚举，D-Bus 错误名（`IsLocked` / `NoSuchObject` / `AccessDenied`）的分类要二次映射；prompt 处理逻辑内置，而我们恰恰不需要它 |
| 异步 `zbus` + 专用线程 + 通道回包 | 调用方仍要同步等回包（否则整条上层都得 async），只多出线程与消息协议 |
| **`zbus::blocking::Connection` 直接调 D-Bus** | 会阻塞调用线程；需要自己映射错误、自己决定 prompt 策略 |
| 把 trait 改成 `async` | 三个平台的原生凭据 API 都是阻塞的；async trait 需要 `async-trait`/手工 boxed future，并把 `local-store` 绑到某个运行时 |

## 决策

1. 直接用 `zbus` 5 的 blocking API，不引入 `secret-service` 之类的高层封装。
   `zbus::blocking` 内部用 `async_io::block_on` 在**当前线程**驱动自己的 socket 任务，
   从不调用 `tokio::runtime::Handle::block_on`，因此不依赖 tokio 运行时，也不会触发
   「runtime within runtime」panic；代价是阻塞调用线程。
2. 同步 trait 保持不变。调用方从 async 上下文（gRPC 层，#23）调用时必须包在
   `tokio::task::spawn_blocking` 里；这条约束写进 `credentials` 模块文档与 trait 文档，
   由 code review 保证。
3. D-Bus 错误按名分类，映射到 ADR 0012 的错误码/分类（`IsLocked` → `Unavailable`，
   `NoSuchObject` → `credential.not_found`，`AccessDenied` → `PermissionDenied`，
   其它 → `Internal`）。`zbus::Error` 与 D-Bus 错误正文**不进入** `Error::source`：
   正文由服务实现决定，可能带条目内容；`source` 会顺着 `Debug`/日志流出去。
4. 不驱动 Secret Service 的 prompt。密钥环锁定或需要确认时服务返回 prompt 对象，
   我们按「请先在桌面会话解锁」如实报 `FailedPrecondition`。后端没有 UI，驱动 prompt
   意味着订阅 `Completed` 并无限期等待用户点击，会把 gRPC 请求线程挂死。
5. 会话算法用 `plain`：秘密值以明文字节走本地 `AF_UNIX` 会话总线，内核保证对端是同一
   uid。同一 uid 的进程本来就能读该用户的密钥环，`DH` 的收益有限而实现成本高。

## 取舍

- `plain` 会话不防「同一用户下的进程嗅探 D-Bus 流量」；`DH` 留作后续升级点。
- 不做调用级超时：D-Bus 本地调用通常毫秒级；服务挂死时会阻塞调用线程，由上层
  `spawn_blocking` + 请求级超时兜底（#23）。
- 会话（`OpenSession`）在建连时建立一次并缓存；若密钥环守护进程重启，会话失效会报
  `credential.unavailable`（`NoSession`），需要重建 store，不做自动重连。
- 写入用 `replace=true`，依赖 Secret Service 「属性相同即替换」的语义；GNOME Keyring
  验证通过（`--test credential_store`）。

## 重新评估条件

- 需要防同一用户下的进程嗅探（升级 `DH` 会话）；
- #23 发现 `spawn_blocking` 的线程池在并发连接场景下成为瓶颈（届时评估异步实现 +
  专用线程）；
- 其它桌面环境（KWallet、非 GNOME 的 Secret Service 实现）暴露 prompt 或属性检索差异；
- 需要零化内存（`zeroize`）或按会话轮换短期凭据。
