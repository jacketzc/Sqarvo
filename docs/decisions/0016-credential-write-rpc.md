# ADR 0016：写凭据的 gRPC 接口（`CredentialService.StoreCredential`）

- 状态：已接受
- 日期：2026-11-04
- 关联：S3 / M3、#27；ADR 0002（本地 IPC）、ADR 0012（引用格式与生命周期）、
  ADR 0013（凭据传给驱动）、ADR 0014（Linux 凭据存储）、ADR 0015（凭据解析）

## 背景

ADR 0012 定下了「密码只进系统凭据库，配置里只留 `credential_ref`」，ADR 0014/0015 让后端
能**读**凭据并把 `credential_ref` 解析成 `Secret` 交给驱动。但契约里一直缺少**写**的路径：

- `ConnectionParams` 没有 `password` 字段（刻意如此），`ConnectionService` 也没有任何
  写入语义；
- 于是前端拿到用户在连接对话框里输入的密码之后，没有任何合法手段把它存进密钥环：
  写配置文件违反 AGENTS.md §9，直连 Secret Service 又等于在 GTK 客户端里实现一遍
  平台凭据映射。

结果是 #27 的连接对话框无法落地：用户输入的密码无处可去。本 ADR 决定补哪一个接口、
以及这条路径上的明文如何被限制。

## 备选

| 方案 | 问题 |
| --- | --- |
| 前端直接写系统凭据库（Linux 用 `secret-service`/`zbus`，Windows 用 `CredWrite`，macOS 用 `SecItem*`） | 三份平台实现；条目属性（`application`/`profile`/`reference`）必须与后端一致，否则后端读不到；错误分类（锁定/权限/不存在）要在前端再实现一遍并与后端文案分叉 |
| 在 `ConnectionService.Connect` 上加一次性 `password` 字段 | 把「建立会话」和「保存凭据」耦合：测试连接、临时密码、只改密码等场景都得连一次库；而且密码字段会混进 `ConnectionParams`，让本可安全 `Debug` 的结构重新变成敏感结构 |
| 让前端写一个受后端监管的临时文件，后端读走后删除 | 明文落盘（哪怕只有毫秒级）且依赖文件权限与清理逻辑；崩溃即泄漏，明显比一次本地 RPC 差 |
| **新增 `CredentialService`（`StoreCredential` / `DeleteCredential`）** | 需要新 proto 服务与一个新的 `Arc<dyn CredentialStore>` 注入点；明文密码经过一次本地 gRPC 请求 |

## 决策

1. 新增 proto 服务 `sqarvo.services.v1.CredentialService`（`proto/services/v1/credential.proto`）：
   - `StoreCredential(StoreCredentialRequest) returns (StoreCredentialResponse)`：
     请求 `{ credential_ref, secret }`，`credential_ref` 为空表示服务端生成新的
     `sqarvo:conn/<uuid-v4>`（ADR 0012 的格式），响应回传最终引用；
   - `DeleteCredential(DeleteCredentialRequest) returns (sqarvo.common.v1.Empty)`：
     幂等，引用不存在视为成功。
2. 字段一旦发布不复用编号；`secret` 字段的注释明确写出「只在本 RPC 内存在，不得写入
   日志、错误文案、端点文件或配置」。
3. 后端实现 `CredentialServiceImpl` 复用既有 `CredentialStore` trait：
   - 引用校验用 `sqarvo_local_store::validate_reference`（`config.invalid` /
     `INVALID_ARGUMENT`）；
   - 同步的 `set` / `delete` 一律包在 `tokio::task::spawn_blocking` 里（ADR 0014/0015）；
     阻塞任务异常结束归为 `internal`，不伪装成认证失败；
   - 错误分类原样透传存储层：`credential.unavailable`（UNAVAILABLE，含锁定/无会话总线）、
     `credential.not_found`（NOT_FOUND）。本服务不引入新的错误码。
4. `Services::new` 增加 `Arc<dyn CredentialStore>` 参数，与 `SessionManager` 共用同一个
   实例：**只保留一个构造入口**，避免「写入用一个存储、读取用另一个」的分叉。
5. 空密码显式拒绝（`config.invalid`）：proto3 无法区分「没传 secret」与「传了空字符串」，
   允许空值会让客户端漏填字段时静默覆盖掉已保存的密码。需要沿用旧密码时复用原引用，
   而不是提交空密码。
6. 密码在服务端只走一条路径：`Request` → `Secret::new` → 移进 `spawn_blocking` 闭包 →
   `CredentialStore::set` → 闭包释放。不缓存、不进 `SessionSummary`、不进错误上下文；
   日志只记引用。

## 取舍

- **明文密码经过本地 IPC**：这是本决策最直接的代价。请求体里有一份明文（HTTP/2 帧，
  Unix 域套接字）。缓解措施：
  - 传输是本地 UDS，socket 位于 0700 用户运行时目录、权限 0600，内核保证对端是同 uid；
  - 每个请求都要带端点描述文件（0600）里的随机 token，`UNAUTHENTICATED` 在进入服务前拒绝；
  - 不监听 `0.0.0.0`、不走网络、不落盘。回环 TCP 仅在「平台无 UDS」或开发调试时使用，
    同样带 token 且只绑定 `127.0.0.1`；
  - 前端不接受「把密码写进配置文件」的任何回退路径。
  反过来说：同一 uid 的进程本来就能读该用户的密钥环，也能读端点文件里的 token，
  因此这条路径没有把攻击面扩大到「同 uid 进程」之外。
- 空密码的连接（MariaDB 允许匿名/空密码账户）在本版本不受支持：`StoreCredential`
  会拒绝。若将来需要，应引入显式的「无凭据」语义（例如 `password_required=false`），
  而不是把空字符串当成「没传」。
- `DeleteCredential` 不做「引用是否仍被配置使用」的检查：配置在前端，后端没有全局视图。
  悬空引用在连接时表现为 `credential.not_found`，这是 ADR 0012 已定的语义。
- 写入不做事务语义：`StoreCredential` 成功但随后的 `Connect` 失败时，密钥环里会留下一个
  未被任何配置引用的条目；前端在保存配置失败时会尽力调用 `DeleteCredential` 清理，
  但无法保证（进程崩溃即残留）。残留条目不构成泄漏，用户可在密钥环管理界面看到
  `Sqarvo` label 的条目并删除。

## 重新评估条件

- 需要「一次性密码 / 不保存密码」的连接流程（届时评估不落密钥环的一次性凭据通道）；
- 需要把凭据写入限定到某个会话或短时效 token（云数据库临时凭据）；
- 出现「同 uid 进程嗅探 D-Bus/本地 IPC」的实际威胁模型，需要升级到 `DH` 会话或
  在应用层做加密信封；
- 前端需要在没有后端（后端未启动）时也能保存密码——那意味着前端必须拥有平台凭据实现，
  届时重新评估第 1 条的备选。
