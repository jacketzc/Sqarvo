# ADR 0012：凭据引用格式与生命周期

- 状态：已接受
- 日期：2026-11-04
- 关联：S3 / M3、#19（本 ADR）、#21、#23；风险 R-06、R-14；`docs/architecture/`

## 背景

`CredentialStore` 接口（`backend/crates/local-store/src/credentials.rs`）只定义了 `get/set/delete`，
`ConnectionParams.credential_ref` 也只有一个不透明字符串。后端的连接路径目前只透传该字段，
没有任何解析（`rpc-server/src/services/connection.rs`）。

引用格式一旦被前端写进连接配置，就会成为兼容负担（配置要跨版本读取），所以必须在实现前固定。
安全约束来自 AGENTS.md §9：密码只进系统凭据库，引用本身不得泄露主机、用户名或连接串。

## 决策

### 引用格式

```text
sqarvo:conn/<uuid-v4>
```

- 前缀固定 `sqarvo:conn/`，后面是随机 UUID v4（小写、带连字符），例如
  `sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4`；
- **禁止**把主机、端口、用户名、库名、数据库类型编进引用：这些信息在连接配置里已有，
  重复编码会让引用变成敏感信息的第二份副本；
- 引用不含密码、token、连接串；不合法引用在写入配置时即被拒绝（`config.invalid`）。

### 系统凭据库中的条目

| 键 | 值 |
|---|---|
| service / 属性 `application` | `io.sqarvo.Sqarvo`（与应用 ID 一致） |
| 属性 `profile` | 用户档案名，默认 `default` |
| 属性 `reference` | 完整引用，如 `sqarvo:conn/<uuid>` |
| label（仅可读描述） | `Sqarvo <驱动显示名>`，例如 `Sqarvo MariaDB`；**不含主机、用户名** |
| secret | 数据库密码原文；不支持在 secret 中放连接串 |

平台侧映射：Linux Secret Service 用上面的属性键；Windows Credential Manager 用 `reference` 作为
target name；macOS Keychain 用 service=`io.sqarvo.Sqarvo`、account=`reference`。

### 生命周期语义

- **写入**：同一引用重复写入覆盖旧值（不做版本化）；
- **读取**：`get` 返回 `Secret`，调用方用完即丢，不缓存到进程级结构；
- **删除**：删除不存在的引用视为成功（幂等）；删除后连接配置中的引用变成悬空引用；
- **悬空引用 / 用户拒绝授权 / 存储不可用** 三类情况必须可区分：
  - 引用不存在（配置还在，凭据已被用户删除）→ 新增错误码 `credential.not_found`；
  - 系统凭据服务不可用（无会话总线、被锁定、无权限）→ 既有 `credential.unavailable`；
  - 用户拒绝授权 → `credential.unavailable`（文案提示重新授权）；
- **禁止回退**：任何情况下都不得回退到明文配置、环境变量或提示用户把密码写进配置文件；
- **进程边界**：密码只在后端进程内存中存在；前端从不接触明文密码，只提交「要保存的密码」到
  后端的一次性 RPC，然后保存引用。

### 错误码增量

在 `sqarvo_common::ErrorCode` 新增：

```rust
pub const CREDENTIAL_NOT_FOUND: Self = Self("credential.not_found");
```

新增错误码属于契约允许的增量（只增不改），gRPC 映射到 `NOT_FOUND`；`credential.unavailable`
保持 `UNAVAILABLE` 语义不变。

## 取舍

- 引用不可读（随机 UUID）意味着配置文件无法自解释：这是刻意的，避免配置泄露连接拓扑；
- 不做引用版本化/轮换：v0.x 阶段够用，若将来支持多设备同步再引入；
- 单一 `service` 前缀下用属性区分档案：Linux Secret Service 支持按属性检索，Windows/macOS 侧
  由该平台实现负责用 `profile:reference` 组合出 target name。

## 落地记录

- 2026-11-07，#145（PR #148）：连接配置的身份从 `credential_ref` 改为**连接目标**——
  `SavedConnection::key()` 服务器型取「驱动 + 主机（大小写不敏感）+ 端口 + 用户名 + 库」，
  文件型取「驱动 + 规范化绝对路径」；`config::upsert` 按身份更新原条目，不再按引用追加。
  「保存并连接」在写凭据之前 `config::find_by_identity` 命中已有条目并复用其引用，密码
  覆盖原引用指向的密钥（同一引用重复写入即覆盖，符合上面的生命周期语义），不在密钥环
  新增条目。加载配置时按同一身份合并旧版本留下的重复条目并记日志（保留最近一次保存）。
  证据：`cargo test` 328 通过；验收链 139 项断言；隔离配置连续两次保存同一 MariaDB 目标后
  `connections.json` 1 条且 `credential_ref` 相同，Secret Service 按该引用只检索到 1 个条目
  （`sqarvo-v023-shots/145/verify-keyring.txt`）。
- 2026-11-11：Windows Credential Manager 落地（`crates/local-store/src/credentials/`）。
  条目形态：`CRED_TYPE_GENERIC`、**target name = 引用原文**（本节「取舍」里写的
  `profile:reference` 组合未采用——引用在配置里必须唯一确定一条凭据，拼档案名会引入
  隐式命名空间；当前只有 `default` 档案，非默认档案在 Windows 上不产生独立命名空间）、
  `UserName` 固定 `Sqarvo`（不放数据库用户名）、blob 为密码的 UTF-8 字节、
  `CRED_PERSIST_LOCAL_MACHINE`。单条上限 2560 字节（`CRED_MAX_CREDENTIAL_BLOB_SIZE`）：
  超限按 `credential.unavailable` + `FailedPrecondition` 明确拒绝，**不截断**。
  错误映射：`ERROR_NOT_FOUND`(1168) → `credential.not_found`；
  `ERROR_ACCESS_DENIED`(5)/`ERROR_PRIVILEGE_NOT_HELD`(1314) → `PermissionDenied`；
  `ERROR_NO_SUCH_LOGON_SESSION`(1312) → `Unavailable`；blob 非 UTF-8 / 类型不匹配 →
  `FailedPrecondition`；其它 → `Internal`。删除不存在视为成功（幂等）。
  **未验证**：P/Invoke 只在本机做了 `cargo check --target x86_64-pc-windows-msvc`
  （含 `--all-targets`）与纯逻辑单测；真实往返、凭据管理器 UI 里的条目形状、重启后持久化、
  超限拒绝、无登录会话分支均需 Windows 实机复核（清单见 `credentials` 模块文档）。

## 重新评估条件

- 需要多设备同步或在团队内共享连接（引用必须可迁移）；
- 需要区分「读密码」「写密码」权限（只读共享连接）；
- Secret Service 实现无法按属性可靠检索（届时退回 target name 拼接口径）。
