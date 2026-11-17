# MCP Server：工具、资源与配置参考

面向**接入方**（AI Agent 宿主、运维、写客户端配置的人）。架构与理由见
[MCP Server 架构](../architecture/mcp.md)；决策见
[ADR 0032](../decisions/0032-mcp-standalone-process.md) /
[ADR 0033](../decisions/0033-sql-parser-readonly-policy.md)。

本文的每一条 schema、默认值、错误子码都取自 `backend/apps/sqarvo-mcp/src/` 的实际实现与
`tools/list` 的真实返回（Linux 实测），不是设计意图。文中出现的密码一律是占位符。

目录：

- [1. 快速开始](#1-快速开始)
- [2. 能力面总览](#2-能力面总览)
- [3. 八个工具](#3-八个工具)
- [4. 值的 JSON 序列化契约](#4-值的-json-序列化契约)
- [5. 四类资源与 URI 编码契约](#5-四类资源与-uri-编码契约)
- [6. `mcp.json` 逐字段](#6-mcpjson-逐字段)
- [7. 凭据 CLI](#7-凭据-cli)
- [8. MariaDB 只读账号模板](#8-mariadb-只读账号模板)
- [9. 客户端配置示例](#9-客户端配置示例)
- [10. 错误模型与子码对照](#10-错误模型与子码对照)
- [11. 限额一览](#11-限额一览)
- [12. 已知限制与未验证清单](#12-已知限制与未验证清单)
- [13. 构建、运行与验收命令](#13-构建运行与验收命令)

## 1. 快速开始

```bash
# 1. 构建（Linux 实测；Windows/macOS 未实机验证）
cd backend && cargo build -p sqarvo-mcp --release
# 产物：backend/target/release/sqarvo-mcp

# 2. 写入数据库密码到系统密钥环（必须交互式终端；不接受参数/环境变量传密）
./target/release/sqarvo-mcp credentials set
#   数据库密码（不回显）：
#   再次输入确认：
#   已写入凭据：sqarvo:conn/0b7f4b6e-1c2d-4e5f-8a9b-0c1d2e3f4a5b
#   把该引用填进 mcp.json 的 credential_ref 字段。凭据实现：<后端标识>

# 3. 建配置（默认路径 = AppPaths::config_dir()/mcp.json，Linux 实测 ~/.config/sqarvo/mcp.json；
#    也可以不建默认文件，改用 --config 指定，见 §6）

# 4. 手工试跑（stdio：stdin 送 JSON-RPC 行，stdout 只出协议消息）
printf '' | ./target/release/sqarvo-mcp --config /path/to/mcp.json; echo "exit=$?"   # exit=0

# 5. 日志（stderr，不污染协议流）
SQARVO_MCP_LOG=debug ./target/release/sqarvo-mcp --config /path/to/mcp.json 2>/tmp/mcp.log
```

## 2. 能力面总览

| 项 | 值 |
|---|---|
| 传输 | stdio（JSON-RPC 行；stdout 只归协议，日志钉 stderr） |
| 实现版本 | `0.1.0`（workspace 版本）；`rmcp 3.10.0`、`sqlparser 0.64.0`、`rpassword 7.5` |
| capabilities | `tools` + `resources`（无 prompts、无 completion、无 logging 通道） |
| `serverInfo.name` | `sqarvo-mcp` |
| 协议版本 | 支持 `2024-11-05` / `2025-03-26` / `2025-11-25` 旧握手，以及 `2026-07-28` 的无握手 `_meta` 通道（详见架构文档 §9） |
| 工具 | 8 个，全部只读；`tools/list` 只播 `enabled_tools` 允许的（下表按注册顺序） |
| 资源 | 1 个静态 + 3 个模板（`resources/list` / `resources/templates/list`） |
| 写操作 | **没有**。`INSERT/UPDATE/DELETE/DDL/GRANT/USE/SET/SHOW/CALL`、事务控制、多语句一律拒 |
| 审计 | 每次 `tools/call` 落 `audit.jsonl`，只记 SQL 指纹不记原文（V1 资源读取不写审计） |

`initialize` 返回的 `instructions` 原文（Agent 会读到，据此建立预期）：

> Sqarvo MCP Server 通过 mcp.json 中已授权的连接别名访问数据库，V1 只提供只读的结构与查询能力。
> 同一个数据库连接上的查询会串行执行（单会话单在途语句），不同连接可在进程级并发上限内并行；
> 排队不是故障。未在 enabled_tools 中启用的工具会被拒绝。所有调用记入本地审计日志（只记录 SQL
> 指纹，不记录原始 SQL 与结果）。

## 3. 八个工具

每个工具的小节格式：**参数表 → 返回（outputSchema 摘要）→ 限额与截断 → 错误子码 → 描述要点**。
`description` 由代码里的文档注释逐字生成，客户端看到的就是它。

所有工具共同的前置顺序：**① `enabled_tools` 检查 → ② alias 授权 → ③（SQL 类）策略检查 →
④ 取并发许可 + 懒连接 → ⑤ 执行 → ⑥ 写审计**。未授权的 alias 不会消耗解析与策略检查，也不会
建立任何数据库连接。

### 3.1 `list_connections`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| — | — | — | — | 无参数（`inputSchema` 是 `{"type":"object"}`，不是缺省） |

返回：

```jsonc
{
  "connections": [
    { "alias": "demo", "driver_id": "sqlite", "database": "",          // SQLite 没有库名 → 空串
      "connection_status": "not_connected", "last_error_code": "" }    // 正常时为空串
  ],
  "count": 1
}
```

- `connection_status`：`not_connected` / `connected` / `failed`。**调用它不会触发连接**。
- `last_error_code` 只在 `failed` 时非空，是稳定子码（例：`credential.not_found`），不是驱动
  原始文案。
- 不出现的字段：用户名、host/port、连接串、`credential_ref`、密码。`enabled: false` 的连接
  根本不进列表（不是「列出来再标记禁用」）。
- 错误：`mcp.tool_disabled`（被禁用时）、`mcp.internal`（审计写失败 → 结果不交付）。
- 审计：记 `tool_name` 与耗时，`returned_rows` 留空（状态读取没有行）。

### 3.2 `list_databases`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `connection_alias` | string | 是 | — | 去空白后不得为空 |

返回：`connection_alias`、`databases: [{name}]`、`count`、`truncated`。

- MariaDB 走 `information_schema.SCHEMATA`，可见性受账号权限约束（没权限的库不出现，不是报错）。
- SQLite 返回 `main` / `temp` / `ATTACH` 出来的库。
- 不切换会话当前库（只读枚举）。
- 限额：单页最多 **500** 项，超出 `truncated=true`。
- 错误：`mcp.invalid_argument`（空 alias）、`mcp.connection_not_found`、`mcp.connection_denied`、
  `credential.not_found` / `credential.unavailable` / `mariadb.*` 连接期错误（状态转 `failed`）、
  `mcp.tool_disabled`、`mcp.internal`。
- 审计：记 `returned_rows=count`、`truncated`。

### 3.3 `list_tables`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `connection_alias` | string | 是 | — | |
| `database` | string | 是 | — | SQLite 用 `list_databases` 的 `main` / `temp` / 附加库 |
| `schema` | string | 否 | `""` | **传非空值 → `capability.unsupported`**（两个内置驱动都没有独立 schema 层，绝不静默忽略） |

返回：`connection_alias`、`database`、`tables: [{name, kind}]`、`count`、`truncated`。

- `kind` 是驱动给的对象种类原文（`table` / `view` / `sequence` …），不映射、不美化。
- 限额：单页最多 **500**。
- 错误：同 3.2，另有 `capability.unsupported`（非空 `schema`）、库不存在时的驱动侧 not-found 子码。
- 审计：`returned_rows=count`、`truncated`。

### 3.4 `describe_table`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `connection_alias` / `database` / `table` | string | 是 | — | `table` 可以是视图名 |
| `schema` | string | 否 | `""` | 非空 → `capability.unsupported` |

返回：`connection_alias`、`database`、`table`、`columns: [ColumnDetail]`、
`primary_key: [{name, position}]`、`has_primary_key`。

`ColumnDetail` 逐字段：

| 字段 | 类型 | 语义与空值口径 |
|---|---|---|
| `name` | string | 列名 |
| `ordinal` | int64 | 列序号，与结果集顺序一致 |
| `type` | string | 逻辑类型 kind（`integer` / `text` / `decimal` / `binary` / `timestamp` / `unsupported` …） |
| `native_type` | string | 数据库原始类型名（如 `varchar(64)`）；**驱动给不出时是空串** |
| `nullable` | bool | 是否允许 NULL |
| `default_kind` | string | `unspecified`（未知，**不等于**没有默认值）/ `no_default` / `null` / `expression` |
| `default_value` | string | 仅 `expression` 时非空，是表达式**原文**，不代求值 |
| `character_set` / `collation` | string | 不适用或未知时是空串 |

- **不输出的字段**：自增属性、列注释——本版本 Driver SPI 没有这两项，宁可不给也不伪造。
- 限额：列数没有单独上限（元数据规模由驱动决定），`truncated` 恒为 `false`；审计里 `returned_rows`
  记的是**列数**。
- 错误：`mcp.invalid_argument`（三个必填任一为空）、`mcp.connection_not_found` / `_denied`、
  `capability.unsupported`（非空 `schema`）、`metadata.object_not_found`（库/表不存在）、
  `metadata.permission_denied`。

### 3.5 `list_indexes`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `connection_alias` / `database` / `table` | string | 是 | — | |
| `schema` | string | 否 | `""` | 非空 → `capability.unsupported` |

返回：`connection_alias`、`database`、`table`、`indexes: [{name}]`、`count`、`truncated`。

- **只有名字**。索引列组成、唯一性、索引类型、排序方式都不在 SPI 能力范围内，因此不返回；
  MariaDB 上主键索引名叫 `PRIMARY`。
- 工具描述里直接给了取明细的正确姿势（这段 SQL 由单测保证能通过自家只读策略，且实测在真库跑通）：

```sql
SELECT INDEX_NAME, NON_UNIQUE, SEQ_IN_INDEX, COLUMN_NAME, INDEX_TYPE
FROM information_schema.STATISTICS
WHERE TABLE_SCHEMA = 'app_db' AND TABLE_NAME = 'users'
ORDER BY INDEX_NAME, SEQ_IN_INDEX
```

  用 `execute_readonly_query` 提交；结果受该连接的账号权限约束。
- 限额：单页最多 **500**。

### 3.6 `get_table_sample`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `connection_alias` / `database` / `table` | string | 是 | — | 表名按标识符交给驱动，**不拼 SQL** |
| `limit` | int64 | 否 | `0` | `0`=用服务端默认（`query_limits.max_rows`，出厂 200）；负数 → `mcp.invalid_argument`；超过硬上限（1000）按上限钳制 |

返回：`connection_alias`、`database`、`table`、`columns`、`rows`（见 §4）、`row_count`、
`truncated`、`truncated_reason`。

- 顺序：有主键按主键升序；**无主键是不稳定顺序**（驱动声明，本版不提供 rowid 近似定位）。
- 取数走驱动的 browse/分页通道并逐页累加，同时受行数与字节两条预算；「恰好取满且驱动说还有
  更多」才算截断，取满但没有更多时 `truncated=false`。
- 限额：行 200 默认 / 1000 硬上限、单次 1 MiB。**该路径不套用 10 s 查询超时**：它走驱动的分页
  浏览通道，没有外层计时器；边界由行数、字节、页大小（≤500）与进程级并发许可共同约束。
- 错误：`mcp.invalid_argument`、`mcp.connection_not_found` / `_denied`、
  `metadata.object_not_found`，以及驱动原样透出的结构类子码（如库/表不存在）。
- 审计：`returned_rows=row_count`、`truncated` 记结果级截断。

### 3.7 `execute_readonly_query`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `connection_alias` | string | 是 | — | |
| `database` | string | 否 | `""` | 非空 → 执行前先切库。事务打开时驱动会拒；SQLite 不支持切库 → `capability.unsupported`（此时省略该参数或用限定名） |
| `sql` | string | 是 | — | **单条** `SELECT` / 只读 `WITH`；DML/DDL/多语句/INTO/锁读/危险函数全拒 |

返回：`columns`、`rows`（§4）、`row_count`、`truncated`、`truncated_reason`、`execution_time_ms`。

- 限额：200 行默认 / 1000 硬上限 / 1 MiB / 10 s 超时（超时后**已请求取消数据库查询**）。
  这三条与 `limit` 无关——SQL 里自己写 `LIMIT 999999` 也只会拿到服务端上限，并且
  `truncated=true` + `truncated_reason="rows"`。
- 策略只验证、**不改写**：下发的 SQL 与提交的一致（保留你的格式与注释）。
- 审计只记 SHA-256 指纹（字面量已脱敏），所以反复跑同形查询在审计里聚合成同一条形态。
- 错误（完整对照见 §10）：`mcp.sql_parse`（解析失败/fail-closed，含递归深度触顶）、
  `mcp.readonly_violation`（白名单外语句、多语句、INTO、锁读、hint、危险函数、`:=`）、
  `mcp.query_timeout`（`retryable=true`）、`query.cancelled`、`mcp.invalid_argument`、
  `mcp.connection_not_found` / `_denied`、`mcp.result_too_large`、`capability.unsupported`。
- 多结果集：不支持（策略层就限定单条语句），一个响应只有一个 `columns`/`rows`。

### 3.8 `explain_query`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `connection_alias` | string | 是 | — | |
| `database` | string | 否 | `""` | 同 3.7 |
| `sql` | string | 是 | — | **不要**自带 `EXPLAIN` / `ANALYZE` 前缀：前缀由服务端拼，自带前缀会被策略拒 |

返回结构与 `execute_readonly_query` 完全相同（计划行就是一张结果表）。

- **驱动范围：仅 MariaDB**。SQLite 返回 `mcp.unsupported`，文案直接给出替代方案：
  「SQLite 的等价物是 EXPLAIN QUERY PLAN，语义不同，本版本不代执行」。这个门控发生在**建连之前**
  （不碰数据库）。
- 只生成普通 `EXPLAIN`，**绝不**生成 `EXPLAIN ANALYZE` / `ANALYZE FORMAT=JSON SELECT`
  （后者会真实执行查询）。
- 限额与取消链与 3.7 同一条代码路径。
- 审计指纹按**原 SQL** 计算（不把 `EXPLAIN ` 前缀归因进去）。

## 4. 值的 JSON 序列化契约

`rows` 是「行的数组，每行是单元格的数组」。单元格编码由 `src/tools/value.rs` 单点决定，
所有工具共用（避免同一个值在不同工具里长两种样子）。

| 驱动值 | JSON 形态 | 说明 |
|---|---|---|
| NULL | `null` | 与空字符串严格区分 |
| bool | `true` / `false` | |
| 整数 `Int` / `UInt` | number，或**十进制字符串** | 绝对值 ≤ 2^53 用 number；超出用字符串，避免客户端按 IEEE-754 解析丢精度 |
| 浮点 | number，或字符串 `"NaN"` / `"inf"` / `"-inf"` | JSON 没有这三个字面量 |
| Decimal | 字符串 | 定点原文，不经过浮点（`123.45` / `0.005` / `1.200`——尾随零是标度的一部分，不化简） |
| 文本 / JSON / UUID | 字符串 | `JSON` 列保留原文，不在这里求值或重排 |
| 二进制 | 字符串 | 标准 base64（带 `=` 填充），列类型 kind 为 `binary` 可辨认 |
| DATE / TIME / DATETIME | 字符串 | ISO 8601；`TIME` 固定 9 位小数（`03:04:05.123456789`）避免精度歧义 |
| TIMESTAMP | 字符串 | RFC 3339，**带时区偏移**（`+08:00` / `Z`） |
| 数组 | array | 递归套用本契约 |
| 不支持的类型 | `{"unsupported_type": "...", "text": "..."}` | 保留原始类型名与文本表示，不伪装成普通值 |

**两层截断必须可区分**（[ADR 0007](../decisions/0007-cell-truncation.md)）：

```jsonc
// 短值：裸标量
"rows": [["alice", 42]]

// 单元格级截断（驱动单值上限截的）：变成对象，带类型与前缀
{"truncated": true, "type": "text", "value": "<截断后的前缀>"}

// 结果级截断（行数/字节上限截的）：在行之外，同一响应的顶层字段
{"truncated": true, "truncated_reason": "rows" | "bytes" | "rows_and_bytes" | "none", "row_count": 200}
```

字节预算按**编码后 JSON 逐行计费**，放不下整行就整体拒收（不返回半行）。

> **`outputSchema` 的一个坑**：`rows` 的单元格 schema 是空对象 `{}`（=「任意 JSON 值」），
> 不是布尔 `true`。布尔 schema 在 JSON Schema 2020-12 里合法，但 MCP 客户端的严格校验会把
> derive 出来的 `{"items": true}` 判成 invalid outputSchema（Inspector `--strict` 实测）。
> 这只改 schema 播发，**运行时序列化不变**——上面的截断对象形态就是证据。

## 5. 四类资源与 URI 编码契约

`resources/list` 只播 1 个静态资源；3 个参数化模板由 `resources/templates/list` 播发。两者都
**不发起数据库连接**。所有资源 MIME 类型都是 `application/json`。

| URI | 内容（字段互不重复） | 触发连接？ |
|---|---|---|
| `database://connections` | `connections: [{alias, driver_id, connection_status}]`、`count` | 否 |
| `database://{connection_alias}/{database}/overview` | `connection_alias`、`driver_id`、`database_name`、`server_version`、`schema_support{has_schema_level, capabilities[]}`、`table_count`、`view_count`、`counts_truncated` | 是 |
| `database://{connection_alias}/{database}/schema` | `database_name`、`object_count`、`by_kind{}`、`objects: [{name, kind, comment}]`、`truncated`、`guidance?` | 是 |
| `database://{connection_alias}/{database}/tables/{table}` | `database`、`table`、`columns: [ColumnDetail]`、`column_count`、`primary_key[]`、`has_primary_key` | 是 |

字段口径：

- **`server_version` 恒为 `null`**：SPI 只在 `test_connection` 报告里给版本，需要额外握手；
  V1 不为此多发起连接，因此固定 `null`（不虚构）。
- `counts_truncated=true` 的含义是「**因为超过单页上限 500，所以给不出精确计数**」，此时
  `table_count` / `view_count` 为 `null`——不是「计数为 0」也不是「数到 500」。驱动不支持过滤
  下推时同样给 `null`，但 `counts_truncated=false`（拿不到 ≠ 被截断，两种情况不合并）。
- `schema` 的对象目录受驱动单页上限 **500** 约束，超出时 `truncated=true` 且带 `guidance`
  指向 `list_tables` 工具；`by_kind` 只统计**当前页**，不是全库分布。
- `tables/{table}` 的列定义与 `describe_table` **逐字段一致**（同一份取数、同一套输出类型，
  等价性由测试钉死）。
- **资源不受 `enabled_tools` 管辖**：那是工具面的开关，资源是另一条协议面。所以
  `enabled_tools: []` 时工具全禁、资源仍可访问（`tools/list` 空、`resources/list` 照旧）。

### 5.1 URI 编码契约（安全契约，不是风格建议）

1. 先按**原始** `/` 拆路径段，**再**对每段独立 percent-decode。
2. 每段只解码一次；非法 `%` 序列、解码后非法 UTF-8 → 直接拒。
3. 解码结果里若还有合法的 `%XX` 三元组 → 按**双重编码**拒。否则「解一次」和「解两次」会给出
   两个不同标识符，编码歧义就变成权限绕过的通道。
4. **解码完成后**才做 alias / database / table 的授权与存在性判断；之后不再解码。
5. 编码侧只保留 RFC 3986 unreserved（`A-Z a-z 0-9 - . _ ~`），其余字节一律 `%XX`（大写十六进制）。
   `/`、`%`、空格、中文都会被编码，因此表名里有 `/` 不会变成两个段。未编码的原始 UTF-8（客户端
   直接塞中文）按字面接受——它已经是标识符本身。

```text
表名  orders/2026  →  database://demo/main/tables/orders%2F2026      ✅ 一个段
表名  用户 表      →  database://demo/main/tables/%E7%94%A8%E6%88%B7%20%E8%A1%A8
表名  100%         →  database://demo/main/tables/100%25             ✅ 编码后的 %
                     database://demo/main/tables/100%25325            ❌ 双重编码 → -32602
                     database://demo/main/tables/bad%zz               ❌ 非法 % 序列 → -32602
```

### 5.2 资源读取的错误语义（二选一，已钉死）

| 情形 | 子码 | JSON-RPC |
|---|---|---|
| URI 自身不合法：scheme 不是 `database://`、段数不对、空段、非法 `%`、非法 UTF-8、双重编码 | `mcp.invalid_argument` | `-32602` |
| 资源不存在或未授权：alias 未知、库/表不存在、驱动报错 | `mcp.connection_not_found` / `metadata.object_not_found` 等 | `-32002` |

`enabled: false` 的连接在资源面被**归一化成 `-32002`「资源不存在或未授权」**（工具面才是
`mcp.connection_denied`）——不让 Agent 用错误码差异探测「某个 alias 是否存在于配置里」；原始
子码只进 stderr 日志。

整体 JSON 超过生效的 `max_result_bytes`（默认 1 MiB）→ `mcp.result_too_large`（`-32603`），
**不交付半截 JSON**；文案里会指路改用粒度更细的工具。

## 6. `mcp.json` 逐字段

默认路径：`AppPaths::config_dir()/mcp.json`。Linux 实测
`$XDG_CONFIG_HOME/sqarvo/mcp.json`（本机 `~/.config/sqarvo/mcp.json`）；macOS
`~/Library/Application Support/io.sqarvo.Sqarvo/mcp.json`（未在实机确认）；Windows
`%APPDATA%\sqarvo\Sqarvo\config\mcp.json`（未确认）。**以 `--config` 与进程日志里的实际路径为准**
——`AppPaths` 的模块注释与 `directories` 6.x 的实际拼法有出入，`crates/local-store` 的注释漂移
已记在 [ADR 0032](../decisions/0032-mcp-standalone-process.md) 已知限制 2。

加载策略（默认拒绝）：

| 情况 | 行为 |
|---|---|
| 文件不存在 | 空连接列表启动 + stderr 警告；任何工具都连不上库（不是错误退出） |
| 文件存在但 JSON 非法 / 字段非法 | **拒绝启动**，stderr 一行原因，exit 1，stdout 保持空 |
| 未知顶层字段 / 未知连接字段（含 `password`） | 拒绝启动（`deny_unknown_fields`：拼错与塞密码都是显式错误） |
| 未知工具名、未知 `driver_id`、非法 alias / 引用格式、非法驱动参数组合 | 拒绝启动 |
| 超过 1 MiB（`MAX_CONFIG_BYTES`） | 拒绝启动 |
| 限额越界、`enabled_tools: []`、MariaDB 缺 `credential_ref` | **告警但照常启动**（可继续运行的偏差不该让人起不来） |

### 6.1 顶层

| 字段 | 类型 | 必填 | 默认 | 安全含义 | 非法处理 |
|---|---|---|---|---|---|
| `connections` | array | 否 | `[]` | 授权白名单：不在这里的 alias 一律不可访问 | 元素非法 → 拒启 |
| `enabled_tools` | array&lt;string&gt; \| 缺失 | 否 | 缺失 = 8 个全启用 | 收窄 Agent 能力面 | 含未知名 → **拒启**；`[]` → 全禁用 + 告警（服务仍起，所有工具调用被拒） |
| `query_limits` | object | 否 | 见 6.3 | 服务端强制限额，不信任 Agent 输入 | 未知字段 → 拒启；越界 → 钳制 + 告警 |

### 6.2 `connections[]`（每条连接）

| 字段 | 类型 | 必填 | 默认 | 适用驱动 | 安全含义 | 非法处理 |
|---|---|---|---|---|---|---|
| `alias` | string | 是 | — | 全部 | Agent 侧唯一可见标识，进工具参数、资源 URI 与审计 | `[a-z0-9_-]{1,64}` 之外 → 拒启；重复 → 拒启 |
| `driver_id` | string | 是 | — | 全部 | 决定参数白名单与只读门控 | 非 `sqlite`/`mariadb` → 拒启（fail-closed） |
| `host` | string | 是 | — | 全部 | MariaDB=主机；**SQLite=数据库文件路径**（进程有权打开该文件） | 空 → 拒启 |
| `port` | u16 | MariaDB 必填 | `0` | 全部 | SQLite 必须 `0`（本地文件没有端口） | MariaDB `0` → 拒启；SQLite `≠0` → 拒启 |
| `database` | string \| null | 否 | `null` | MariaDB | 默认库；SQLite 不允许该字段 | SQLite 给了 → 拒启 |
| `username` | string \| null | MariaDB 必填 | `null` | MariaDB | 建议用只读专用账号（§8）；SQLite 出现即可疑 | SQLite 给了 / MariaDB 空白 → 拒启 |
| `credential_ref` | string \| null | 否 | `null` | MariaDB | **只有引用**，密码在系统密钥环（ADR 0012）；SQLite 不允许 | 格式非 `sqarvo:conn/<uuid v4>` → 拒启（文案不回显原文）；SQLite 给了 → 拒启；MariaDB 缺 → 告警并以空密码连接 |
| `tls` | string | 否 | `"unspecified"` | 全部 | 六档：`unspecified` / `disabled` / `preferred` / `required` / `verify_ca` / `verify_full`（snake_case 精确匹配，大小写变体算写错） | SQLite 给非 `unspecified`/`disabled` → 拒启；`disabled` + 证书参数 → 拒启；未知值 → 拒启 |
| `tls_server_name` | string \| null | 否 | `null` | MariaDB | 显式 SNI/主机名校验目标 | SQLite 给了 → 拒启；`tls=disabled` 时给了 → 拒启 |
| `properties` | map&lt;string,string&gt; | 否 | `{}` | 按驱动 | 只放非敏感驱动参数；**键名含 `password`/`passwd`/`secret`/`token` 一律拒**（跨驱动兜底，将来新驱动也适用） | 白名单外键 → 拒启（错误文案点名该键） |
| `enabled` | bool | **是** | 无默认 | 全部 | 授权总开关：`false` = Agent 看不见也调不动 | **缺字段即非法** → 拒启（默认拒绝，不给「漏写字段=默认放开」的机会） |

`properties` 白名单（启动期预检，镜像各自驱动的 `from_params`）：

| 驱动 | 允许的键 | 取值约束 |
|---|---|---|
| `sqlite` | `read_only` | `true`/`false`/`1`/`0`；`read_only` + `:memory:` 组合被拒（内存库无此语义） |
| `mariadb` | `ssl_ca`、`ssl_cert`、`ssl_key`、`connect_timeout_ms` | 证书与私钥必须**同时**给；`connect_timeout_ms` ∈ `[100, 300000]` 整数；`tls=disabled` 时不得带证书参数 |

预检的边界要说清：它只做**提前拒绝**，不做提前接受——驱动仍是连接期的唯一权威，握手逻辑不在
这里复制一份。它的价值是把拼错的键暴露在启动时，而不是等到第一次工具调用才在懒连接里失败。

### 6.3 `query_limits`（钳制后的生效值才起作用）

| 字段 | 类型 | 默认 | 钳制区间 | 越界行为 |
|---|---|---:|---|---|
| `max_rows` | u32 | `200` | `[1, 1000]` | 钳制 + 告警 |
| `timeout_ms` | u64 | `10000` | `[1, 10000]` | 同上（**只能调小**，10 s 是硬上限） |
| `max_result_bytes` | u64 | `1048576` | `[1024, 1048576]` | 同上 |
| `max_concurrency` | usize | `2` | `[1, 4]` | 同上 |

`hard_max_rows=1000`、`1 MiB`、`10 s`、并发 `4` 都是代码常量，**配置无法放宽**——这是「不信任
Agent 输入」与服务端可保证上界的唯一来源。每个被钳制的字段都会在启动日志里点名（旧文案是中文，
形如「`query_limits.max_rows = 50000` 超出硬上限，已钳制为 1000」）。

### 6.4 完整示例（全部占位符）

```jsonc
{
  "connections": [
    {
      "alias": "prod_mariadb_readonly",
      "driver_id": "mariadb",
      "host": "db.internal.example",
      "port": 3306,
      "database": "app_db",
      "username": "sqarvo_mcp_ro",
      "credential_ref": "sqarvo:conn/0b7f4b6e-1c2d-4e5f-8a9b-0c1d2e3f4a5b",
      "tls": "verify_full",
      "properties": { "ssl_ca": "/etc/ssl/certs/internal-ca.pem", "connect_timeout_ms": "3000" },
      "enabled": true
    },
    {
      "alias": "local_sqlite",
      "driver_id": "sqlite",
      "host": "/srv/data/analytics.sqlite",
      "port": 0,
      "properties": { "read_only": "true" },
      "enabled": true
    },
    {
      "alias": "staging_off",              // 关掉授权：Agent 完全看不见这条
      "driver_id": "mariadb",
      "host": "staging.internal.example",
      "port": 3306,
      "username": "sqarvo_mcp_ro",
      "credential_ref": "sqarvo:conn/11111111-2222-4333-8444-555555555555",
      "enabled": false
    }
  ],
  "enabled_tools": ["list_databases", "list_tables", "describe_table", "execute_readonly_query"],
  "query_limits": { "max_rows": 100, "timeout_ms": 5000 }
}
```

这个例子同时是「最小权限」的形状：只读账号 + `verify_full` + 只放 4 个工具 + 把行数和超时收紧。
想验证「写不进去」，把 `sql` 参数换成 `UPDATE …` 看它报 `mcp.readonly_violation` 即可，不必真
给账号写权限。

## 7. 凭据 CLI

```bash
sqarvo-mcp credentials set [REFERENCE]     # 写入；省略 REFERENCE 则自动生成并打印
sqarvo-mcp credentials delete REFERENCE    # 删除；不存在视为成功（幂等）
```

### 7.1 首次录入（五步）

1. **构建二进制**：`cd backend && cargo build -p sqarvo-mcp --release`（下面的路径都用这个产物）。
2. **在交互式终端里运行** `./target/release/sqarvo-mcp credentials set`。stdin 不是 TTY 时会
   **在打开密钥环之前**就失败退出（`mcp.invalid_argument`，文案点名「需要交互式终端」）——管道、
   重定向、CI 里跑不通是有意的：那里的「读密码」要么回显进日志，要么在无人值守时永远阻塞。
3. **按提示输入两遍密码**（不回显）。两次不一致 → 不写入任何东西；空密码 → 拒绝（不需要密码的
   账号就不要配 `credential_ref`）。
4. **记下打印出来的引用**（`sqarvo:conn/<uuid v4>`），填进 `mcp.json` 的 `credential_ref`。
   引用也可以自己指定：`credentials set sqarvo:conn/<你自己的 uuid v4>`，格式非法会**先**报错、
   不会让你先输两遍密码。
5. **验证**：起进程后调 `list_connections`——`connection_status` 应为 `connected`（第一次真正
   调用工具后），不再是 `failed` + `last_error_code: credential.not_found`。

约束：

- **没有任何非交互入口**：不接受 `--password`、位置参数传密或环境变量。多一个入口就多一份进
  shell history / 进程表 / 崩溃报告的机会。
- 密码只存在于操作系统凭据库（Linux Secret Service、Windows Credential Manager）。密钥环不可用
  → `credential.unavailable`，**永远不回退明文存储**（ADR 0012）。macOS 未实机验证。
- 管理模式不启动 server、不输出协议消息；提示走 stderr，stdout 只留「已写入 + 引用」，方便脚本
  取引用。
- 删除引用后配置仍指向它 → 该连接下次建连失败并转 `failed`，`last_error_code` =
  `credential.not_found`；重新 `set` 同一个引用即恢复（不需要改配置）。

## 8. MariaDB 只读账号模板

AST 策略不是最终防线，数据库侧必须有第二道（纵深防御的第五层，见架构文档 §7）。**给 MCP 单独
建账号，不要复用 GUI 或应用账号。**

```sql
-- 1) 专用账号：不落盘密码由 credential_ref 引用；这里的临时密码用完即弃
CREATE USER 'sqarvo_mcp_ro'@'%' IDENTIFIED BY '<REPLACE-ME-STRONG-PASSWORD>';

-- 2) 只授予 SELECT：结构发现 + 只读查询够用，写入与 DDL 从权限层面就不成立
GRANT SELECT ON `app_db`.* TO 'sqarvo_mcp_ro'@'%';

-- 3) 需要跨库时逐库授权，不要用 *.*（*.* 会连带 information_schema 之外的库全部可见）
--    GRANT SELECT ON `reporting`.* TO 'sqarvo_mcp_ro'@'%';

-- 4) 资源 overview 需要统计计数，元数据可见性由 SELECT 隐含（information_schema 可读即可）；
--    不需要、也**不要**给下列任何一项：
--      FILE            → LOAD_FILE()/SELECT ... INTO OUTFILE 的服务端能力
--      INSERT/UPDATE/DELETE/DROP/ALTER/CREATE/INDEX/CREATE VIEW/...  → 任何 DML/DDL
--      SUPER/CONNECTION ADMIN/SESSION_VARIABLES_ADMIN → 会话与实例级控制权
--      PROCESS         → 能看别的会话的 SQL 文本
--      CREATE USER     → 能改授权，等于绕过本模板

-- 5) 可选但推荐：服务端资源护栏（与 MCP 的 10 s/200 行是叠加关系，不是替代）
ALTER USER 'sqarvo_mcp_ro'@'%' WITH
  MAX_QUERIES_PER_HOUR 10000
  MAX_CONNECTIONS_PER_HOUR 200
  FAILED_LOGIN_ATTEMPTS 5 PASSWORD_LOCK_TIME 1;

FLUSH PRIVILEGES;

-- 6) 自检：应该只有 Usage + Select on app_db
SHOW GRANTS FOR 'sqarvo_mcp_ro'@'%';
```

要点与陷阱：

- **`FILE` 权限必须没有**：危险函数表里 `LOAD_FILE` 是第一项，但那是静态判定；账号没有 `FILE`
  才是真的兜底。
- MariaDB **没有连接级只读开关**（`docs/database/mariadb.md`）：要限写只能靠授权。只读事务是
  会话级能力，MCP 不显式开事务，所以别指望它替你兜住。
- 只给 `SELECT` 时，「库不存在」与「库不可读」都可能只拿到 `1044`，MCP 会如实报
  `metadata.permission_denied` 并在文案里说明这个歧义——这是受限账号的固有现象，不是 bug。
- 用 `verify_full` + `ssl_ca`（§6.2）而不是 `disabled`；`tls` 六档的语义与逐档验证记录见
  [`docs/database/mariadb.md`](../database/mariadb.md)。
- SQLite 没有账号概念，「只读」靠 `properties.read_only = "true"` + 文件系统权限（给进程一个
  只读挂载点或只读文件）。

## 9. 客户端配置示例

配置文件的配置文件——大多数 Agent 宿主只需要 `command` + `args`。

### 9.1 Cherry Studio

设置 → MCP → 添加 → 选择 **STDIO** 类型，填：

| 字段 | 值 |
|---|---|
| 名称 | `Sqarvo`（自定） |
| 命令 | `/absolute/path/to/backend/target/release/sqarvo-mcp` |
| 参数（args） | `--config`、`/absolute/path/to/mcp.json` |
| 环境变量 | `SQARVO_MCP_LOG=info`（可选；日志在客户端的 stderr 面板里看） |

界面等价于：

```json
{
  "mcpServers": {
    "sqarvo": {
      "name": "Sqarvo",
      "type": "stdio",
      "command": "/absolute/path/to/backend/target/release/sqarvo-mcp",
      "args": ["--config", "/absolute/path/to/mcp.json"],
      "env": { "SQARVO_MCP_LOG": "info" }
    }
  }
}
```

预期结果与验收步骤（**人工验收项，本机未确认**）：

1. 服务器条目变绿/「已连接」，展开后能看到工具列表；
2. 工具数量 = `8`，或等于 `enabled_tools` 里列出的个数（若配了 `enabled_tools: []` 则是 0）；
3. 资源标签页出现 `database://connections`（静态资源）——**不展开不发起连接**；
4. 调 `list_connections`：返回 `alias` / `driver_id` / `database` / `connection_status` /
   `last_error_code`，**没有任何用户名字段**；
5. 调一次 `execute_readonly_query`，`SELECT 1` 返回 `structuredContent.columns/rows`；
6. 关掉该 MCP 服务器/退出客户端 → `ps` 里 `sqarvo-mcp` 进程消失（关窗即退出，不残留）。

任何一步不符合预期 → 先拿 stderr 日志（`SQARVO_MCP_LOG=debug`）比对架构文档 §9 的协议矩阵，
再决定是客户端侧展示差异还是服务端行为回归。

### 9.2 通用 stdio 客户端（Claude Desktop / Inspector / 自研宿主）

```json
{
  "mcpServers": {
    "sqarvo": {
      "command": "/absolute/path/to/backend/target/release/sqarvo-mcp",
      "args": ["--config", "/absolute/path/to/mcp.json"]
    }
  }
}
```

MCP Inspector 手工验证：

```bash
npx @modelcontextprotocol/inspector --cli /absolute/path/to/backend/target/release/sqarvo-mcp \
  --method tools/list --args --config --args /absolute/path/to/mcp.json
# 实测记录：Inspector @2.13 的 tools/list + resources/list 成功；
# --strict = 0 errors / 3 advisory warnings（见 §13「已确认的告警」）
```

## 10. 错误模型与子码对照

### 10.1 两条错误通道

| 通道 | 何时用 | Agent 看到什么 |
|---|---|---|
| **工具级** `result.isError = true` | 一切业务失败：参数非法、未授权、只读违规、超时、限额…… | `content[0].text` = `<安全文案>（<稳定子码>）`，外加 `structuredContent.error = {code, category, message, retryable}` |
| **协议级** JSON-RPC `error` | 只有三类：工具名不存在、请求形状不合法（资源 URI 非法）、服务不可用 | `-32602` / `-32002` / `-32603` + `data.error` 结构化体 |

业务失败**不走**协议级错误：客户端对协议错误通常只显示一句笼统提示，Agent 就读不到「换条
SELECT」这种可执行信息了。协议级错误的分类映射沿用通用语义：

| `ErrorCategory` | JSON-RPC | 典型场景 |
|---|---|---|
| `invalid_argument` / `failed_precondition` | `-32602` | 资源 URI 非法（scheme/段数/空段/非法 `%`/双重编码）、未知工具名 |
| `not_found` | `-32002` | 资源不存在或未授权（含 `enabled: false` 的连接被归一化成「不存在」） |
| 其余（含 `internal` / `unavailable` / `permission_denied` / `timeout`） | `-32603` | 资源 JSON 超 `max_result_bytes` 等 |

`retryable` 由分类推出：`unavailable` / `timeout` / `resource_exhausted` 三类为 `true`。

### 10.2 `mcp.*` 子码全表（文本即契约，允许新增、不改既有含义）

| 子码 | 分类 | retryable | 触发点 | Agent 该怎么办 |
|---|---|---|---|---|
| `mcp.invalid_argument` | `invalid_argument` | 否 | 必填参数为空、`limit` 为负、资源 URI 形状/编码非法 | 修参数；URI 检查是否双重编码 |
| `mcp.connection_not_found` | `not_found` | 否 | alias 不在 `mcp.json` 里 | 用 `list_connections` 取真名，别猜 |
| `mcp.connection_denied` | `permission_denied` | 否 | alias 存在但 `enabled: false`（**仅工具面**；资源面归一化成 not_found） | 这是授权决定，不要重试 |
| `mcp.tool_disabled` | `permission_denied` | 否 | 工具不在 `enabled_tools` | 换已启用的工具；资源面不受该开关管辖 |
| `mcp.readonly_violation` | `permission_denied` | 否 | 白名单外语句、多语句、INTO、锁读、hint、危险函数、`:=` | 改成单条 `SELECT`；文案里逐条列出了全部违规原因 |
| `mcp.sql_parse` | `invalid_argument` | 否 | sqlparser(MySqlDialect) 解析失败（含递归深度触顶）；**不代表原语句非法** | 换写法（去 MariaDB 特有语法、降嵌套深度） |
| `mcp.database_not_found` / `mcp.table_not_found` | `not_found` | 否 | 预留（当前由驱动的 `metadata.object_not_found` 承担） | 回查 `list_databases` / `list_tables` |
| `mcp.unsupported` | `unsupported` | 否 | MCP 层能力缺失（目前只有 `explain_query` 打非 MariaDB 驱动） | 按文案给的替代方案走 |
| `capability.unsupported` | `unsupported` | 否 | 驱动层不支持：非空 `schema`、SQLite 切库等 | 省略该参数 |
| `mcp.query_timeout` | `timeout` | **是** | 超过 `query_timeout`（≤10 s），已请求取消数据库查询 | 缩范围、加过滤、减列 |
| `mcp.result_too_large` | `resource_exhausted` | **是** | 资源 JSON 超 `max_result_bytes` | 改用粒度更细的工具分批读 |
| `query.cancelled` | `cancelled` | 是 | 客户端断开或 `notifications/cancelled`，已取消驱动查询 | 需要就重发 |
| `mcp.internal` | `internal` | 否 | 内部缺陷；**也包括审计写入失败**（成功路径的审计写不进 → 结果不交付） | 报障，附 `request_id` |
| `config.invalid` | `invalid_argument` | 否 | `mcp.json` 非法 → **拒绝启动**（不是运行时错误） | 看 stderr 那一行原因 |
| `credential.not_found` / `credential.unavailable` | `not_found` / `unavailable` | 否 / 是 | 密钥环里没有该引用 / 密钥环不可用 | `credentials set` 补写 / 修 Secret Service |
| `session.not_found` / `transport.failed` | — | — | 会话消失 → 触发一次「重连 + 重试」 | 无需处理，重试对用户透明 |

### 10.3 常见现象对照（含真实文案）

| 现象 / 文案 | 根因 | 处理 |
|---|---|---|
| `只读白名单仅允许单条 SELECT / 只读 WITH 查询，本次输入是语句类型 Delete。` | DML 被顶层白名单拒 | 换查询 |
| `禁止 INTO 子句：V1 一律拒绝任何形式的 SELECT ... INTO（会话变量 / 列清单 / 表 / OUTFILE / DUMPFILE），目标：@x` | `INTO` 命中 | 去掉 `INTO` |
| `禁止锁定读：FOR UPDATE ...` | 锁读 | 去掉锁子句 |
| `禁止危险函数：SLEEP——人为延长查询，占用连接与超时预算。` | 危险函数表 | 换普通表达式 |
| `解析失败：... 已按 fail-closed 原则拒绝（...MariaDB 特有语法请改写后重试）` | MySqlDialect 解析不了 | 常见于 `LOCK IN SHARE MODE` / `INTO OUTFILE` / `ANALYZE SELECT` / `PROCEDURE ANALYSE()` |
| `explain_query 当前仅支持 MariaDB（普通 EXPLAIN）；驱动 sqlite 未声明 Explain 能力。SQLite 的等价物是 EXPLAIN QUERY PLAN...` | 驱动门控 | SQLite 侧别用该工具 |
| `查询超过 10000 毫秒上限，已请求取消` | 超时（`retryable=true`） | 缩查询 |
| `URI 路径段疑似双重编码（解码后仍含 %XX 序列）；每个路径段只允许编码一次` | 资源 URI 编了两次 | 只编一次 |
| `资源不存在或该连接未授权；请用 resources/list 与 list_connections 核对别名` | alias 未知或 `enabled:false`（资源面归一化） | 核对别名与授权 |
| `资源内容 1234567 字节，超过单次上限 1048576 字节；请改用粒度更细的工具…` | 资源超 1 MiB | 分批读 |
| stderr：`未找到 mcp.json（...）：默认路径不存在，请按文档创建；以空连接列表启动，所有连接请求都会被拒绝` | 默认路径没配置且没传 `--config` | 建配置或传 `--config` |
| stderr：`连接 prod 的 properties 不允许携带凭据类参数：db_password；密码只能写入系统凭据库，由 credential_ref 引用` | 想从配置塞密码 | 用 `credentials set`（§7） |
| 启动即 exit 1，stdout 空 | 配置非法（未知字段/未知名/非法组合/超 1 MiB） | 按 stderr 那行改配置 |

## 11. 限额一览

| 限额 | 默认 | 硬上限 | 可配 | 落点 |
|---|---:|---:|---|---|
| 工具未指定 `limit` 时的行数 | 200 | 1000 | `query_limits.max_rows` | `ExecuteRequest.max_rows` + 取批上限 |
| Agent 显式 `limit` | — | 1000 | 否 | `effective_row_limit` 钳制，负数拒 |
| 单次结果字节数 | 1 MiB | 1 MiB | 只能调小（≥1 KiB） | 序列化前逐行计费，放不下整行不返回半行 |
| 查询超时 | 10 s | 10 s | 只能调小 | 外层 `timeout` + `SessionHandle::cancel`（驱动级超时未实现，见 §12 限制 3） |
| 元数据列表单页 | 500 | 500 | 否 | `MAX_PAGE_SIZE`；4 个列表工具与 `schema` 资源 |
| 进程级并发 | 2 | 4 | `query_limits.max_concurrency` | 信号量与 `QueryRegistry` 同源 |
| 同 alias 在途语句 | 1 | 1 | 否 | 会话执行锁（ADR 0008） |
| SQL 解析递归深度 | 50（sqlparser 默认） | 50 | 否（刻意不调高） | 触顶 → `mcp.sql_parse` |
| `mcp.json` 大小 | — | 1 MiB | 否 | 超限拒绝启动 |
| 审计单文件 / 份数 | 10 MiB / 5 | 同左 | 否（代码常量） | 只在完整记录边界轮转 |

## 12. 已知限制与未验证清单

完整理由见 [架构文档 §10](../architecture/mcp.md)。这里给接入方视角的条目：

**已知限制（行为口径，不是 bug）**

1. **V1 资源读取不写审计**：只有 `tools/call` 进 `audit.jsonl`。资源与工具能读到同一份结构，
   审计覆盖不对称。
2. **`server_version` 恒为 `null`**（overview 资源）：不为它多发起一次握手。
3. **驱动级单次执行超时未实现**：超时由外层计时 + 取消实现，语义等价但不是服务端 `timeout`；
   `ExecuteRequest.timeout` 非 `None` 时 MariaDB 直接 `capability.unsupported`。
4. **计数超过 500 就放弃精确值**：`table_count`/`view_count` 给 `null` + `counts_truncated=true`，
   不返回「≥500」这种半截信息。
5. **同 alias 串行**：并发只在不同 alias 之间生效；要并发就配多条 alias。
6. **审计多实例 best-effort**：`flock` 只串行化「追加一行」，两个实例轮转同一目录仍可能互相
   覆盖；争用时第二个实例退化写 `audit.<pid>.jsonl`。V1 **不宣称**多实例安全。
7. **轮转参数（10 MiB × 5）不可配**。
8. **`PROCEDURE ANALYSE()` 是唯一真正的只读误杀**（合法、只读、极罕见），接受，不做例外放行。
9. **`explain_query` 仅 MariaDB**；SQLite 的 `EXPLAIN QUERY PLAN` 语义不同，不代执行。
10. **索引明细不在 `list_indexes` 里**：只有名字，列组成/唯一性/类型要查 `information_schema`。
11. **没有 `prompts` / `completion` 能力**；`tools/list` 只在 ≥`2026-07-28` 的会话里带缓存字段。
12. **握手前 `SIGTERM` 走系统默认处置**（rmcp 限制，见架构文档 §10.2）。此时无会话无在途查询，
    不需要清理，但不是优雅关停。
13. **`rows` 的 `outputSchema` 单元格是空对象 schema**（`{}`）：单元格是「任意 JSON 值」的契约
    本义，Inspector `--strict` 的 3 条 advisory warning 即来源于此（§13）。

**未验证清单（本机环境限制，不是「已确认没问题」）**

14. **Windows / macOS 未实机验证**：Windows Rust CI 作业由 #218 跟踪；macOS 未接入。
    Windows 审计跨进程锁是刻意跳过（best-effort）。
15. **Cherry Studio GUI 协商 = 人工验收项**：步骤与预期见 §9.1（6 条断言）。
16. **真实 OS 密钥环的失败路径 E2E 本机不可用**：没有可用的 Secret Service 会话，改用双断言
    替代（§13）。
17. **默认配置路径在 macOS/Windows 上的具体拼法未确认**：只确认了 Linux 实测值（§6）。

## 13. 构建、运行与验收命令

```bash
# 构建（工作目录 backend/）
cd backend
cargo build -p sqarvo-mcp --release           # 产物 target/release/sqarvo-mcp
cargo build -p sqarvo-mcp                     # debug 产物同样可跑

# 单元测试 + 离线集成测试（无需任何数据库）
cargo test -p sqarvo-mcp                      # 默认门控：MariaDB 专属用例打印跳过原因
cargo clippy -p sqarvo-mcp --all-targets -- -D warnings
cargo fmt --all -- --check

# 严格档：真实 MariaDB（缺 env 直接 panic，避免「绿色但零验证」）
SQARVO_MCP_REQUIRE_MARIADB=1 \
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-mcp

# workspace 回归（证明 MCP 没打破既有后端）
cargo test

# 手工冒烟：stdio 上一次握手 + 一次 tools/list
printf '%s\n%s\n%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
 | ./target/release/sqarvo-mcp --config /path/to/mcp.json 2>/tmp/mcp.log
```

已记录的验收证据（写需求状态与本文时引用的同一批）：

| 命令 | 实测结果 |
|---|---|
| `cargo test -p sqarvo-mcp` | **171 passed**（默认门控一遍；`SQARVO_MCP_REQUIRE_MARIADB=1` + 真实 MariaDB `53306` 另一遍也是 171） |
| `cargo test`（workspace 回归） | **895 passed / 0 failed**（2026-11-17 在本工作树 `be13213` 实测） |
| MCP Inspector `@2.13` `tools/list` + `resources/list` | 成功；`--strict`：**0 errors / 3 advisory warnings** |
| `tests/protocol_matrix.rs` | 14 个用例（旧握手 3 + 2026-07-28 双通道 5 + 版本回落 1 + 错误形状与 `enabled_tools` 2 + 生命周期 3），全绿 |
| `tests/policy_snapshots.rs` | 正向放行 61 例、策略拒绝 66 例、fail-closed 13 例、EXPLAIN 旁路负面 79 例同码拒绝 |
| `tests/audit_concurrency.rs` | 双进程写同一目录逐行完整（`two_server_processes_merge_into_whole_jsonl_records`）、争用退化 pid sidecar、轮转后行数与整行性、单进程并发不交错、脱敏不变量 |
| 密钥环失败路径 | **本机不可用**，双断言替代：装配层返回 `credential.unavailable`；非交互 stdin 在打开密钥环**之前**就失败（`credentials_set_requires_an_interactive_terminal`） |

> **已确认的 3 条 advisory warning**：`execute_readonly_query` / `explain_query` /
> `get_table_sample` 的 `outputSchema.rows.items.items` 是**空对象** schema。Inspector `--strict`
> 提示「空 schema 无关键字」，但这正是单元格契约的本义（任意 JSON 值，见 §4）；0 errors 是硬门槛，
> 3 条 warning 是已知且接受的形态。

## 14. 相关文档

- [MCP Server 架构](../architecture/mcp.md)——进程模型、并发、安全分层、协议矩阵、已知限制全集。
- [ADR 0032 MCP 独立进程与连接模型](../decisions/0032-mcp-standalone-process.md)
- [ADR 0033 SQL Parser 只读安全策略](../decisions/0033-sql-parser-readonly-policy.md)
- [第三方依赖许可声明](../licenses.md)
- [产品需求 §9 MCP 对外接口（V1）](../product/requirements.md)
- [MariaDB 驱动行为](../database/mariadb.md)、[SQLite 驱动行为](../database/sqlite.md)
- [gRPC 约定](grpc-conventions.md)——GUI 通道（与本文的 MCP 通道并列，互不依赖）
