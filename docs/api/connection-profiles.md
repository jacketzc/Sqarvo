# 共享连接档案

`sqarvo-local-store::ProfileStore` 维护唯一的共享档案文档。GTK 通过本地 gRPC 服务管理档案，
`sqarvo-mcp` 和它的终端管理命令直接复用该存储。各进程从系统凭据库按引用获取密码。

## 存储格式

共享路径为 `AppPaths::config_dir()/connections.json`。Linux 使用
`${XDG_CONFIG_HOME:-$HOME/.config}/sqarvo/connections.json`。按当前目录解析实现，Windows
目标为 `%APPDATA%/sqarvo/Sqarvo/config/connections.json`，macOS 目标为
`~/Library/Application Support/io.sqarvo.Sqarvo/connections.json`。两端路径与行为均未实机验证。

```json
{
  "schema_version": 2,
  "revision": 1,
  "mcp": { "enabled": false, "read_only": true },
  "profiles": [
    {
      "profile_id": "58acd9da-8a78-4d19-8d5e-2a33dd0c7c06",
      "revision": 1,
      "name": "inventory",
      "driver_id": "mariadb",
      "host": "db.example.test",
      "port": 3306,
      "database": "inventory",
      "username": "demo",
      "credential_ref": "sqarvo:conn/0b7f4b6e-1c2d-4e5f-8a9b-0c1d2e3f4a5b",
      "tls": "verify-full",
      "tls_server_name": null,
      "properties": { "connect_timeout_ms": "2000" },
      "file_path": "",
      "display_name": "库存",
      "last_used": null
    }
  ],
  "migration_state": {}
}
```

- 空文档从 revision 0 开始，成功写入推进文档 revision。新档案的 revision 为 1，成功更新时递增。
  `profile_id` 由存储生成，更新保持 ID 稳定。
- `driver_id` 至 `properties` 是 Driver SPI 的连接参数。密码保存在系统凭据库，
  文档只含 `credential_ref`。`name` 用于显示和 MCP 别名解析。`display_name` 可省略。
  SQLite 使用 `host` 作为驱动路径，`file_path` 是兼容字段。保存和读取时只有一处填写路径则补齐
  另一处。两处均填写时必须完全相同，否则返回 `profile.invalid`，原文件保留。Linux 按实际路径
  拼写比较，保留大小写、符号链接及 `..` 的路径语义。请使用绝对路径，确保独立进程打开同一文件。
  服务器型连接的 `file_path` 留空。`last_used` 无值时为 `null`，有值时用 RFC 3339。
- `mcp` 整段缺失或部分缺失时，按字段采用 `enabled: false`、`read_only: true`。
  关闭访问原子恢复这组默认值。关闭只读要求先开启访问。V1 无数据库写入工具。
- `migration_state` 的键为规范化的旧文件路径，值含 SHA-256 十六进制 `sha256` 与 RFC 3339
  `migrated_at`。未知 schema 或无法验证的文档会导致操作失败，原文档保留。

请通过 GUI、gRPC 或管理 CLI 更新文档。存储使用常驻的 `connections.json.lock` 加锁。
并发写者在锁内读取最新文档，校验后通过同目录临时文件同步与原子替换提交。锁等待上限为 2 秒，
文档大小上限为 16 MiB。启动迁移会在独占锁下
清理遗留的 `connections.json.tmp-*`。

## 迁移

| 来源 | 原位置 | 成功导入后的处理 |
|---|---|---|
| GTK | `${XDG_CONFIG_HOME:-$HOME/.config}/sqarvo/Sqarvo/connections.json` | 改名追加 `.bak` |
| Windows v1 | `%LOCALAPPDATA%/Sqarvo/connections.json` | 改名追加 `.bak` |
| MCP | `AppPaths::config_dir()/mcp.json`，或 `--config` 指定位置 | 保留 `.bak`，原位置重写为服务级配置 |

后端启动、MCP 启动和 MCP 管理命令都会尝试迁移。共享文档里既有档案和策略保留。文件路径与内容
指纹都相同的来源重复运行时只补做未完成的备份或替换。已有不同内容的 `.bak` 时，额外备份以
`.bak-<sha256>` 保留。迁移保留凭据引用，同一引用可供多个档案使用。
旧 `connections[].enabled` 不参与全局授权，迁移对新文档使用关闭访问、开启只读的安全默认值。
Linux GTK 的旧 SQLite 档案保留大小写不同的路径，也保留同一路径下名称或只读设置不同的档案。
如果早期版本漏导入了大小写不同的路径，可从 `Sqarvo/connections.json.bak` 查看原档案，
然后在 GTK 里只重新保存缺失的连接。保留备份和现有共享文档。

Windows v1 使用 `{"version":1,"connections":[...]}`，条目含 `driver`、`name`、
`host`、`port`、`database`、`username`、`file_path`、`read_only`、`tls_mode` 和
`credential_ref`。迁移将 `driver` 写成 `driver_id`，将 `tls_mode` 写入 `tls`。共享 TLS 模式使用
连字符，例如 `verify-ca` 和 `verify-full`。SQLite 使用文件路径和驱动属性 `properties.read_only`。
Windows 前端接入冻结。合成示例见 [Windows v1 fixture](../../tests/fixtures/windows-connections-v1.json)。

## 本地 gRPC

六个 RPC 位于 `sqarvo.services.v1.ConnectionProfileService`，每个请求都经过现有的
`x-sqarvo-token` 鉴权。连接参数复用 `ConnectionParams`，响应仅含非敏感参数和凭据引用。
契约以 [profile.proto](../../proto/services/v1/profile.proto) 为准。

| RPC | 语义 |
|---|---|
| `ListProfiles` | 返回同一快照的档案数组和文档 revision |
| `GetProfile` | 按 UUID 读取一条档案，包含档案 revision |
| `SaveProfile` | 新建时省略 ID 和 `expected_revision`。更新时提供 ID 和读到的档案 revision |
| `DeleteProfile` | 提供 UUID 和 `expected_revision`。删除档案保留系统凭据 |
| `GetMcpAccessPolicy` | 返回两个开关和对应的文档 revision |
| `SetMcpAccessPolicy` | 指定一个或两个字段，在锁内应用到当前策略，返回提交值和文档 revision。显式关闭访问时恢复只读 |

重复新建遇到同一连接目标时，服务端返回冲突或歧义。客户端应重新读取档案并用当前 ID、revision
处理更新。连接目标以驱动及服务器连接身份，或驱动及文件路径判断。Linux SQLite 文件路径按原始拼写比较。

| 子码 | gRPC 状态 | 语义 |
|---|---|---|
| `profile.not_found` | `NOT_FOUND` | 档案不存在 |
| `profile.revision_conflict` | `FAILED_PRECONDITION` | 预期 revision 与当前档案不匹配 |
| `profile.ambiguous_name` | `FAILED_PRECONDITION` | 同一目标匹配多个档案，须指定 UUID |
| `profile.locked` | `DEADLINE_EXCEEDED` | 等待共享档案锁超时 |
| `profile.invalid` | 依错误分类 | 例如非法请求、损坏文档或当前状态不允许的策略更新 |

稳定子码通过 `sqarvo-error-code` 元数据返回。MCP 请求在等待或建连期间遇到档案版本变化时，
同样使用 `profile.revision_conflict`。后续请求可直接使用最新档案。连接标识和资源错误见
[MCP 接口参考](mcp-tools.md)。
