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

`backend/crates/rpc-server/tests/table_data_contract.rs` 覆盖表数据浏览与行编辑契约（ADR 0017，
#41）。它不依赖服务实现（`TableDataService` 尚未接线），三类断言：

1. **Protobuf 字段号兼容**：读取 `build.rs` 生成的 `FileDescriptorSet`
   （`OUT_DIR/sqarvo-descriptor.bin`），逐字段锁定 `BrowseTableRequest` /
   `BrowseTableResponse` / `ApplyRowChanges*` / `GetPrimaryKey*` 的字段编号与名字，
   锁定 `TableDataService` 与 `MetadataService`（含新增 `GetPrimaryKey`）的方法表，
   锁定三个枚举的取值编号；同时断言值字段仍是 `ColumnValue`（不退化成字符串）、
   游标仍是 `string`（不允许换成客户端可解析的结构）、行变更不带 SQL 字段；
2. **错误语义**：`table_data.*` 与 `transaction.*` 新错误码 → gRPC 状态码 →
   trailing metadata `sqarvo-error-code` 的映射固定；未实现能力映射为 `UNIMPLEMENTED`
   而不是执行失败；文案不含 SQL 与凭据字样；
3. **游标不透明性**：`DriverSession` 的默认实现（`table_data()` 为 `None`、
   `supports_parameterized_statements()` 为 `false`、`execute_parameterized` 返回
   `capability.unsupported`）；假 `TableDataProvider` 证明游标原样透传、篡改后返回
   `table_data.cursor_invalid`、无主键表标注 `Unstable` 且只读、行编辑默认被拒绝。

## 运行

```bash
cd backend
cargo test -p sqarvo-rpc-server --test grpc_smoke
cargo test -p sqarvo-rpc-server --test table_data_contract
SQARVO_MARIADB_URL='mysql://user:***@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-rpc-server --test grpc_credential
```

## 待补充

- Protobuf 兼容性检查（跨版本对比字段编号复用、破坏性变更）——当前只锁定「本版编号」，
  跨版本 diff 仍计划引入 `buf breaking`；
- `TableDataService` 的服务实现落地后，补真实 UDS + SQLite 的浏览/编辑端到端用例
  （游标续页、无主键只读、影响行数冲突、确认阈值）；
- 三平台客户端代码生成验证（C# / Swift / Rust）；
- 消息大小上限与超时行为的契约测试；
- 断连与半开连接的恢复行为。

## 约定

- 契约变更必须先改 `proto/`，再重新生成，再更新本测试；
- 不允许手工编辑生成代码；
- 测试中不得出现真实凭据或生产连接串。
