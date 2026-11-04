# ADR 0013：凭据如何传给驱动（`Driver::connect` 增加 `Secret` 参数）

- 状态：已接受（第 3 条部分被 [ADR 0015](0015-credential-resolution.md) 取代：`SessionManager`
  不再接受 `Option<&Secret>` 参数，改为内部解析 `credential_ref`；驱动侧签名不变）
- 日期：2026-11-04
- 关联：S3 / M3、#20、#23；ADR 0012（凭据引用格式）、ADR 0011（MariaDB 驱动）

## 背景

SPI 文档一直写着「凭据由 `CredentialStore` 解析后直接交给驱动」，但 `Driver::connect`
与 `Driver::test_connection` 只有 `&ConnectionParams`，而 `ConnectionParams` 被设计为
**不含密码**（`Debug` 可打印、会进入错误上下文与测试快照）。结果是：实现 MariaDB 驱动时，
没有任何契约允许把解析出的密码传到驱动，只能从 `properties` 里偷偷塞——那是安全退化。

## 备选

| 方案 | 问题 |
|---|---|
| 在 `ConnectionParams.properties` 放 `password` | 密码进入可 `Debug`、可序列化、可被测试快照引用的结构；与 AGENTS.md §9 冲突 |
| 驱动自己持有 `CredentialStore` | 驱动依赖平台凭据实现，测试与平台移植都被牵连；SPI 与本地存储耦合 |
| **连接调用增加显式 `Option<&Secret>` 参数** | 需要改所有实现与调用点；但密码只出现在调用栈上，寿命最短 |

## 决策

1. `Driver::connect(&self, params, secret: Option<&Secret>)` 与
   `Driver::test_connection(&self, params, secret: Option<&Secret>)` 增加凭据参数。
2. `Secret` 类型从 `sqarvo-local-store` 移到 `sqarvo-driver-api`（`driver-api/src/credential.rs`），
   因为它是驱动契约的一部分；`sqarvo-local-store` 重新导出同名类型，现有 `use` 路径不变。
   迁移后 `Secret` 的 `Debug` 仍是 `Secret(<redacted>)`，并有单元测试防止回归。
3. `SessionManager::connect` / `test_connection` 同样接受 `Option<&Secret>` 并透传；
   gRPC 层在 #23 完成 `credential_ref` 解析之前传 `None`（网络数据库会得到认证失败，
   而不是伪造成功）。
4. 驱动实现约束：只消费 `secret`，不保存、不记录、不写入错误信息；`DriverDescriptor`
   与能力声明不因凭据变化。

## 取舍

- 签名变更波及 `SQLite` 驱动、注册表假驱动、连接管理器与其测试——一次性成本。
  `SQLite` 忽略该参数并继续拒绝 `credential_ref`（文件数据库无密码）。
- `Option<&Secret>` 允许匿名/空密码账户；空密码是否可接受由服务端决定，驱动不猜测。
- 凭据解析仍在上层（#23），驱动不知道「引用」长什么样，符合 ADR 0012 的分层。

## 重新评估条件

- 需要区分「读密码 / 写密码」或按会话轮换短期凭据（例如云数据库临时 token）；
- 需要把 `Secret` 变成零化内存（`zeroize`）并要求所有驱动遵守；
- 驱动需要多个凭据（例如 TLS 客户端私钥口令），届时评估独立的 `ConnectCredentials` 结构。
