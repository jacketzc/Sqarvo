# 契约测试

## 当前内容

`backend/crates/rpc-server/tests/grpc_smoke.rs` 是真正的端到端契约测试：

1. 绑定真实 Unix 域套接字；
2. 生成并校验 token；
3. 通过自定义连接器建立 tonic 客户端；
4. 调用 `SystemService.GetVersion/GetHealth/ListDrivers`；
5. 验证无 token 调用被拒绝（`UNAUTHENTICATED`）；
6. 验证 `ConnectionService.TestConnection` 在无驱动时返回 `NOT_FOUND` +
   metadata `sqarvo-error-code = driver.not_found`；
7. 验证查询取消对未知 ID 返回 `NOT_FOUND`、非法 ID 返回 `INVALID_ARGUMENT`。

`backend/crates/rpc-server/tests/grpc_credential.rs` 覆盖 `CredentialService`（#27）：
真实 UDS + token + 注入的 `MemoryCredentialStore`，验证 `StoreCredential` 生成/复用引用、
覆盖写、`DeleteCredential` 幂等、无 token 返回 `UNAUTHENTICATED`、非法引用返回
`INVALID_ARGUMENT` + `config.invalid`、`UnavailableCredentialStore` 返回
`credential.unavailable`，以及错误文案不含密码。需要真实 MariaDB 的用例
（写入的密码能连接、错误密码得到 `mariadb.auth_failed`、未知引用得到
`credential.not_found`）读取 `SQARVO_MARIADB_URL`，未设置时打印跳过原因。

## 运行

```bash
cd backend
cargo test -p sqarvo-rpc-server --test grpc_smoke
SQARVO_MARIADB_URL='mysql://user:***@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-rpc-server --test grpc_credential
```

## 待补充

- Protobuf 兼容性检查（字段编号复用、破坏性变更）——计划引入 `buf breaking`；
- 三平台客户端代码生成验证（C# / Swift / Rust）；
- 消息大小上限与超时行为的契约测试；
- 断连与半开连接的恢复行为。

## 约定

- 契约变更必须先改 `proto/`，再重新生成，再更新本测试；
- 不允许手工编辑生成代码；
- 测试中不得出现真实凭据或生产连接串。
