# ADR 0015：凭据解析放在 `SessionManager`，不再向 gRPC 层透传 `Secret`

- 状态：已接受
- 日期：2026-11-04
- 关联：S3 / M3、#23；ADR 0012（引用格式）、ADR 0013（凭据传给驱动）、ADR 0014（Linux 凭据存储）

## 背景

ADR 0013 让 `Driver::connect` 接受 `Option<&Secret>`，并临时让
`SessionManager::connect / test_connection` 也接受该参数、由 gRPC 层传 `None`，
直到 #23 完成 `credential_ref` 解析。现在需要决定：谁来读凭据存储、在哪个线程读、
以及解析后的 `Secret` 如何传给驱动。

## 备选

| 方案 | 问题 |
| --- | --- |
| gRPC 层解析后把 `Secret` 透传给 `SessionManager` | gRPC 层要持有凭据存储与线程池细节；Service 层从「协议适配」变成「安全边界」，两处（connect / test_connection）容易分叉 |
| 驱动自己读凭据存储 | 每个驱动依赖平台凭据实现，测试与移植被牵连（ADR 0013 已否决） |
| 保留外部 `secret` 参数 + 内部解析两条路径 | 两者同时存在时语义不明：调用方传了 `Some(secret)` 又带着 `credential_ref`，谁赢？任选一种都能绕过配置里的引用审计 |
| **`SessionManager` 持有 `Arc<dyn CredentialStore>`，`credential_ref` 是唯一来源** | 需要改 `SessionManager` 的构造器与 `connect/test_connection` 签名；连接 crate 新增对 `sqarvo-local-store` 的依赖（无环） |

## 决策

1. `SessionManager` 持有 `Arc<dyn CredentialStore>`：
   - `new(registry)` 装配 `UnavailableCredentialStore`（未注入时带引用的连接如实报
     `credential.unavailable`，不会静默用空密码）；
   - `with_credential_store(registry, store)` 供应用与测试注入。
2. 解析发生在 `SessionManager::connect / test_connection`，顺序是：
   `params.validate()` → 查驱动 → 解析凭据 → 调驱动。解析失败时既不连库、也不写入
   会话表，不会留下半死会话。
3. 删除 `SessionManager::connect / test_connection` 的 `secret` 参数：`credential_ref`
   是唯一来源，消除「外部 secret 覆盖配置引用」的歧义路径。
4. 同步的 `CredentialStore` 调用一律包在 `tokio::task::spawn_blocking` 里：
   密钥环 D-Bus 往返（锁定时可能等待用户）不能占住 Tokio worker（ADR 0014）。
   阻塞任务异常结束（panic / cancel）归为 `internal.error`，不伪装成认证失败。
5. `Secret` 只存在于解析调用栈到驱动调用之间：不缓存、不放进 `SessionSummary`、
   不写日志；错误码与分类原样透传凭据存储（`credential.not_found` /
   `credential.unavailable`），错误文案来自存储层，本来就不含密码。
6. 引用格式非法（不是 `sqarvo:conn/<uuid-v4>`）在解析入口被拒绝为 `config.invalid`，
   不会把「看起来像连接串的引用」送到存储或网络。

## 取舍

- `connection` crate 依赖 `local-store`：两者都是后端内部库，依赖方向单一，无环。
- 两个 RPC（Connect / TestConnection）共享同一解析实现，代价是 `SessionManager` 承担了
  「凭据编排」职责；换来的是一处边界、一处测试。
- 没有实现「临时密码（不保存）」的连接流程：proto 也没有该字段。若 #25 之后需要
  「输入密码但不保存」，再评估在 `ConnectionParams` 之外增加一次性凭据通道，而不是
  恢复 `secret` 参数。

## 重新评估条件

- 需要一次性/临时密码（不写密钥环、也不落配置）；
- 需要按会话轮换短期凭据（云数据库 token）；
- 解析耗时成为连接吞吐瓶颈（届时评估批量解析或异步凭据存储）。
