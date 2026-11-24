# MCP Server：工具、资源与配置参考

面向**接入方**（AI Agent 宿主、运维、写客户端配置的人）。架构与理由见
[MCP Server 架构](../architecture/mcp.md)；决策见
[ADR 0032](../decisions/0032-mcp-standalone-process.md) /
[ADR 0033](../decisions/0033-sql-parser-readonly-policy.md)。

连接档案、迁移与本地管理 RPC 见 [共享连接档案](connection-profiles.md)。
共享策略的决策记录见 [ADR 0035](../decisions/0035-shared-connection-profiles.md)。

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

# 2. 在 GTK 保存需要的连接档案，或使用已有档案
./target/release/sqarvo-mcp access status

# 3. 确认当前及未来所有连接档案均可授权后开启访问
./target/release/sqarvo-mcp access on

# 4. 启动 MCP 服务。stdio 由客户端使用，日志走 stderr
./target/release/sqarvo-mcp
```

## 2. 能力面总览

| 项 | 值 |
|---|---|
| 传输 | stdio（JSON-RPC 行；stdout 只归协议，日志钉 stderr） |
| 实现版本 | `0.1.0`（workspace 版本）；`rmcp 3.10.0`、`sqlparser 0.64.0`、`rpassword 7.5` |
| capabilities | `tools` + `resources`（无 prompts、无 completion、无 logging 通道） |
| `serverInfo.name` | `sqarvo-mcp` |
| 协议版本 | 支持 `2024-11-05` / `2025-03-26` / `2025-11-25` 旧握手，以及 `2026-07-28` 的无握手 `_meta` 通道（详见架构文档 §9） |
| 工具 | 13 个只读工具、4 个 V2 工具、4 个 V2.1 事务工具、6 个 V2.2 工具。`tools/list` 按 `enabled_tools` 筛选 |
| 资源 | 1 个静态 + 3 个模板（`resources/list` / `resources/templates/list`） |
| 写操作 | 通过独立的 V2、V2.1、V2.2 结构化入口，受全局访问策略和数据库权限约束 |
| 审计 | 每次 `tools/call` 落 `audit.jsonl`，只记 SQL 指纹。资源的全局策略拒绝、名称歧义和需要重启也会记录 |

`tools/list` 与资源发现提供静态描述。工具调用和资源读取会检查共享档案的全局访问策略。
SQL 查询按档案串行执行，跨档案的并发受进程级上限约束。

## 3. V1 只读工具

每个工具的小节格式：**参数表 → 返回（outputSchema 摘要）→ 限额与截断 → 错误子码 → 描述要点**。
`description` 由代码里的文档注释逐字生成，客户端看到的就是它。

所有工具共同的前置顺序：**① `enabled_tools` 检查 → ② 请求级授权（全局开关、档案解析和 revision、操作类型） → ③（SQL 类）策略检查 →
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
- `alias` 用于传入后续工具和资源调用。服务按精确档案 UUID 或唯一名称定位档案。
  名称有歧义时使用 UUID。结果省略用户名、host/port、连接串、凭据引用和密码。
- 全局关闭或共享文档不可读时返回 `mcp.connection_denied`。新增或修改的档案可列出，
  重启前对它建连会返回 `profile.restart_required`。
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

- V2.3 可选字段为 `identity`、`comment`、`generated`、`generation_expression`、`primary_key_position`。省略表示
  方言不适用或无法可靠读取。SQLite 区分 `rowid_alias` 和显式 `autoincrement`，MariaDB
  使用 `auto_increment`。生成表达式作为元数据文本返回。
- 列数或序列化结果超过预算时返回 `mcp.result_too_large`。审计记录返回列数。
- 错误：`mcp.invalid_argument`（三个必填任一为空）、`mcp.connection_not_found` / `_denied`、
  `capability.unsupported`（非空 `schema`）、`metadata.object_not_found`（库/表不存在）、
  `metadata.permission_denied`。

### 3.5 `list_indexes`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `connection_alias` / `database` / `table` | string | 是 | — | |
| `schema` | string | 否 | `""` | 非空 → `capability.unsupported` |

返回保留 `connection_alias`、`database`、`table`、`indexes[].name`、`count`、`truncated`。
索引条目增加 `unique`、`primary`、`index_type`、`origin` 和有序 `columns`。
键项提供 `name`、`kind`、可空的 `sort_order` 和 `prefix_length`。SQLite 表达式键项为
`kind: "expression"`，列名为 `null`。SQLite 来源 `c`、`u`、`pk` 分别对应显式创建、
UNIQUE 约束和主键。隐式索引单列在可选的 `implicit_indexes`，`count` 仍统计原 `indexes`
数组。MariaDB 主键索引名仍为 `PRIMARY`。输出受服务端行数和字节预算约束。

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

- MariaDB 使用普通 `EXPLAIN`。SQLite 使用 `EXPLAIN QUERY PLAN`，返回原生
  `id`、`parent`、`notused`、`detail`。SQLite 计划接受省略 `database` 或显式 `main`。
  `detail` 是数据库版本相关的文本。
- 服务端先验证单条 SELECT 或只读 WITH。限额和取消使用只读查询执行路径，指纹基于原 SQL。

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
  `enabled_tools: []` 时工具全禁，资源读取仍单独检查全局策略（`tools/list` 空，资源发现照常）。

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

### 5.2 资源读取的错误语义

| 情形 | 子码 | JSON-RPC |
|---|---|---|
| URI 自身不合法：scheme 不是 `database://`、段数不对、空段、非法 `%`、非法 UTF-8、双重编码 | `mcp.invalid_argument` | `-32602` |
| 资源不存在或未授权：alias 未知、库/表不存在、驱动报错 | `mcp.connection_not_found` / `metadata.object_not_found` 等 | `-32002` |

全局关闭或无法读取策略时，资源读取返回 `-32002`，详情含 `mcp.connection_denied`。
资源读取因全局策略、名称歧义或档案需要重启而拒绝时，稳定子码以 `resources/read` 写入现有
JSONL 审计。普通资源读取沿用 V1 行为。

整体 JSON 超过生效的 `max_result_bytes`（默认 1 MiB）→ `mcp.result_too_large`（`-32603`），
**不交付半截 JSON**；文案里会指路改用粒度更细的工具。

## 6. `mcp.json` 逐字段

默认路径为 `AppPaths::config_dir()/mcp.json`。Linux 对应
`${XDG_CONFIG_HOME:-$HOME/.config}/sqarvo/mcp.json`。可用 `--config` 指定另一个文件。
该参数选择 MCP 服务设置，档案与策略始终使用共享配置路径。默认文件缺失时采用默认服务设置。
文件存在但 JSON 非法、字段未知或超过 1 MiB 时拒绝启动。

### 6.1 服务级字段

| 字段 | 类型 | 默认 | 处理 |
|---|---|---|---|
| `enabled_tools` | array&lt;string&gt; | 27 个工具均可发现 | 未知名拒绝启动，`[]` 关闭全部工具 |
| `query_limits` | object | 见 §6.3 | 未知字段拒绝启动，数值越界按上限钳制 |

### 6.2 共享档案与旧配置

旧 `connections[]` 由启动迁移读入，备份为 `.bak`，原位置只保留服务级字段。
旧 `alias` 进入档案名称，旧 `enabled` 不继承。全局访问需通过 GTK 或本地 CLI 启用。
[共享档案格式与迁移](connection-profiles.md)。

### 6.3 `query_limits`（钳制后的生效值才起作用）

| 字段 | 类型 | 默认 | 钳制区间 | 越界行为 |
|---|---|---:|---|---|
| `max_rows` | u32 | `200` | `[1, 1000]` | 钳制 + 告警 |
| `timeout_ms` | u64 | `10000` | `[1, 10000]` | 同上（**只能调小**，10 s 是硬上限） |
| `max_result_bytes` | u64 | `1048576` | `[1024, 1048576]` | 同上 |
| `max_concurrency` | usize | `2` | `[1, 4]` | 同上 |

`hard_max_rows=1000`、`1 MiB`、`10 s`、并发 `4` 都是代码常量，**配置无法放宽**——这是「不信任
Agent 输入」与服务端可保证上界的唯一来源。每个被钳制的字段都会在启动日志里点名，例如
`query_limits.max_rows = 50000 exceeds the hard limit and was clamped to 1000`。

### 6.4 服务配置示例

```json
{
  "enabled_tools": ["list_databases", "list_tables", "describe_table", "execute_readonly_query"],
  "query_limits": { "max_rows": 100, "timeout_ms": 5000 }
}
```

该示例只启用四个工具并收紧行数与超时。数据库侧仍应使用只读账号，见 §8。

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
4. **记下打印出来的引用**（`sqarvo:conn/<uuid v4>`），由管理客户端关联到共享档案的 `credential_ref`。
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

### 7.2 全局访问管理

```bash
sqarvo-mcp access status
sqarvo-mcp access on
sqarvo-mcp access off
sqarvo-mcp access read-only status
sqarvo-mcp access read-only on
sqarvo-mcp access read-only off
```

`access on` 和 `access read-only off` 显示当前档案数量及影响范围并要求确认。
非交互终端必须显式传入 `--yes`。只有全局访问开启时才可关闭只读，关闭只读后，授权 Agent 可以使用本页的 V2 写入工具。V1 查询工具继续执行只读规则。
`access off` 撤销后续访问并恢复只读。已获授权且开始执行的操作可以完成，随后释放相应会话。状态显示两个开关、档案数量和文档 revision。
这些命令由本地用户运行。GUI 和 CLI 的开关变更不进入 MCP 工具审计。

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
- 使用 `verify-full` 和 `properties.ssl_ca` 校验证书链与主机名。
  [档案字段](connection-profiles.md) · [MariaDB TLS 验证记录](../database/mariadb.md)。
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
2. 默认发现 `27` 个工具，显式列表配置下等于 `enabled_tools` 里列出的个数（若配了 `enabled_tools: []` 则是 0）；
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
| `mcp.connection_not_found` | `not_found` | 否 | 共享档案中找不到指定 UUID 或名称 | 用 `list_connections` 取真名，别猜 |
| `mcp.connection_denied` | `permission_denied` | 否 | 全局访问关闭或读策略失败。资源面保留协议级拒绝口径 | 这是授权决定，不要重试 |
| `profile.ambiguous_name` | `failed_precondition` | 否 | 名称匹配多个档案 | 改用档案 UUID |
| `profile.restart_required` | `failed_precondition` | 否 | 档案在 MCP 启动后新增或修改 | 由本地用户重启 MCP |
| `mcp.tool_disabled` | `permission_denied` | 否 | 工具不在 `enabled_tools` | 换已启用的工具；资源面不受该开关管辖 |
| `mcp.readonly_violation` | `permission_denied` | 否 | 白名单外语句、多语句、INTO、锁读、hint、危险函数、`:=` | 改成单条 `SELECT`；文案里逐条列出了全部违规原因 |
| `mcp.sql_parse` | `invalid_argument` | 否 | sqlparser(MySqlDialect) 解析失败（含递归深度触顶）；**不代表原语句非法** | 换写法（去 MariaDB 特有语法、降嵌套深度） |
| `mcp.database_not_found` / `mcp.table_not_found` | `not_found` | 否 | 预留（当前由驱动的 `metadata.object_not_found` 承担） | 回查 `list_databases` / `list_tables` |
| `mcp.unsupported` | `unsupported` | 否 | MCP 层能力缺失（例如驱动未声明查询计划能力） | 按文案给的替代方案走 |
| `capability.unsupported` | `unsupported` | 否 | 驱动层不支持：非空 `schema`、SQLite 切库等 | 省略该参数 |
| `mcp.query_timeout` | `timeout` | **是** | 超过 `query_timeout`（≤10 s），已请求取消数据库查询 | 缩范围、加过滤、减列 |
| `mcp.result_too_large` | `resource_exhausted` | **是** | 资源 JSON 超 `max_result_bytes` | 改用粒度更细的工具分批读 |
| `query.cancelled` | `cancelled` | 是 | 客户端断开或 `notifications/cancelled`，已取消驱动查询 | 需要就重发 |
| `mcp.internal` | `internal` | 否 | 内部缺陷；**也包括审计写入失败**（成功路径的审计写不进 → 结果不交付） | 报障，附 `request_id` |
| `config.invalid` | `invalid_argument` | 否 | `mcp.json` 非法 → **拒绝启动**（不是运行时错误） | 看 stderr 中的字段提示或错误位置。诊断省略无效的用户输入值 |
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
| `查询超过 10000 毫秒上限，已请求取消` | 超时（`retryable=true`） | 缩查询 |
| `URI 路径段疑似双重编码（解码后仍含 %XX 序列）；每个路径段只允许编码一次` | 资源 URI 编了两次 | 只编一次 |
| `资源不存在或该连接未授权；请用 resources/list 与 list_connections 核对别名` | 档案未知或全局访问关闭（资源面归一化） | 核对别名与授权 |
| `资源内容 1234567 字节，超过单次上限 1048576 字节；请改用粒度更细的工具…` | 资源超 1 MiB | 分批读 |
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
| 同一档案在途语句 | 1 | 1 | 否 | 会话执行锁（ADR 0008） |
| SQL 解析递归深度 | 50（sqlparser 默认） | 50 | 否（刻意不调高） | 触顶 → `mcp.sql_parse` |
| `mcp.json` 大小 | — | 1 MiB | 否 | 超限拒绝启动 |
| 审计单文件 / 份数 | 10 MiB / 5 | 同左 | 否（代码常量） | 只在完整记录边界轮转 |

## 12. 已知限制与未验证清单

完整理由见 [架构文档 §10](../architecture/mcp.md)。这里给接入方视角的条目：

**已知限制（行为口径，不是 bug）**

1. **普通资源读取沿用 V1 审计范围**。审计记录工具调用及 §5.2 所列的资源授权拒绝。资源与工具都能读取结构，
   审计覆盖不对称。
2. **`server_version` 恒为 `null`**（overview 资源）：不为它多发起一次握手。
3. **驱动级单次执行超时未实现**：超时由外层计时 + 取消实现，语义等价但不是服务端 `timeout`；
   `ExecuteRequest.timeout` 非 `None` 时 MariaDB 直接 `capability.unsupported`。
4. **计数超过 500 就放弃精确值**：`table_count`/`view_count` 给 `null` + `counts_truncated=true`，
   不返回「≥500」这种半截信息。
5. **同一档案串行**。不同档案可在进程级并发上限内并行。
6. **审计多实例 best-effort**：文件锁只串行化「追加一行」，两个实例轮转同一目录仍可能互相
   覆盖；争用时第二个实例退化写 `audit.<pid>.jsonl`。V1 **不宣称**多实例安全。
7. **轮转参数（10 MiB × 5）不可配**。
8. **`PROCEDURE ANALYSE()` 是唯一真正的只读误杀**（合法、只读、极罕见），接受，不做例外放行。
9. **SQLite 计划文本随数据库版本变化**，请按原生列读取。
10. **索引细节随方言可用性变化**，未知的来源、表达式原文或前缀长度可以为空。
11. **没有 `prompts` / `completion` 能力**；`tools/list` 只在 ≥`2026-07-28` 的会话里带缓存字段。
12. **握手前 `SIGTERM` 走系统默认处置**（rmcp 限制，见架构文档 §10.2）。此时无会话无在途查询，
    不需要清理，但不是优雅关停。
13. **`rows` 的 `outputSchema` 单元格是空对象 schema**（`{}`）：单元格是「任意 JSON 值」的契约
    本义，Inspector `--strict` 的 3 条 advisory warning 即来源于此（§13）。

**未验证清单（本机环境限制，不是「已确认没问题」）**

14. **Windows / macOS 未实机验证**：Windows Rust CI 作业由 #218 跟踪；macOS 未接入。
    Windows 共享档案与审计使用 `LockFileEx`，未实机验证。
15. **Cherry Studio GUI 协商 = 人工验收项**：步骤与预期见 §9.1（6 条断言）。
16. **真实 OS 密钥环的失败路径 E2E 本机不可用**：没有可用的 Secret Service 会话，改用双断言
    替代（§13）。
17. **默认配置路径在 macOS/Windows 上的具体拼法未确认**。Linux 路径见 §6。
18. 新增或编辑档案后需要重启 MCP 才可建连。全局开关和档案删除在后续访问请求生效。
19. GUI 与 CLI 的开关变更不写入 MCP 工具审计。

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

## 本地 Streamable HTTP（Linux）

在 GTK 中打开主菜单「MCP」。服务区可以保存监听端口、启动或停止服务，并显示后端确认的状态。首版仅支持 `127.0.0.1`，默认端口 `8765`，路径 `/mcp`。默认不随应用启动。停止或退出 GTK 时回收其伴生后端所启动的 MCP 进程。

独立运行示例：

```bash
sqarvo-mcp serve --transport http --host 127.0.0.1 --port 8765
```

无参数 `sqarvo-mcp` 继续提供 stdio。HTTP 所有请求需要 Bearer，必须使用实际地址和端口，Origin 缺省或与该端点同源。首启在 Secret Service 中生成凭据，解锁失败会报告凭据库不可用。可在管理页或通过 `sqarvo-mcp http-auth rotate` / `revoke` 替换或撤销。旧凭据不能继续认证。关闭数据库数据访问会拒绝下一次数据库操作，HTTP 服务本身仍运行。

### 配置 Codex

在 HTTP 服务区复制正在监听的 MCP 地址，或在 Agent 接入区复制 HTTP 提示词。按实际本地端口添加 `[mcp_servers.sqarvo]`，配置 `url` 和 `bearer_token_env_var = "SQARVO_MCP_HTTP_TOKEN"`。启动端点后，从可信终端运行：

```bash
sqarvo-mcp http-auth launch codex --port 8765
```

替换示例端口为正在运行的端口。启动器从系统凭据库读取令牌，将其注入这一 Codex 进程的环境。启动器为本次运行固定该端点的配置。请仅在受信任的本机模型客户端和工作环境内使用。轮换后重新启动客户端。客户端若只能把明文 Header 保存到文件，选择管理页的 stdio 配置提示词。

### 本地管理 gRPC

`SystemService` 增加 `GetMcpServerStatus`、`GetMcpServerConfig`、`UpdateMcpServerConfig`、`StartMcpServer`、`StopMcpServer`、`RotateMcpServerToken` 和 `RevokeMcpServerToken`。启停返回当前状态，终态通过状态 RPC 回读。失败状态携带稳定错误码，例如 `mcp.http.port_in_use`。配置更新仅在停止或失败状态接受，范围为 1–65535。现有 `ConnectionProfileService` 的权限 RPC 继续独立使用，所有管理 RPC 沿用本地 gRPC 认证。


## V2 结构化写入

在 GTK 的 **MCP → 数据权限**关闭 **只读**，或者由本机用户执行
`sqarvo-mcp access read-only off` 并确认。保留 **MCP 数据访问**开启。
建议日常查询使用专用只读数据库账号，写入使用独立的最小权限档案。
重新开启只读或关闭数据访问后，新写入请求立即拒绝。Agent 无权改这些开关。
使用显式 `enabled_tools` 列表时，需要添加要启用的 V2 工具名。

| 工具 | 公共字段以外的参数 | 直接行数上限 |
|---|---|---:|
| `insert_rows` | `rows`，同批各行使用相同列集合 | 100 |
| `update_rows` | `changes` 列值对象、`conditions` 数组 | 50 个匹配行 |
| `delete_rows` | `conditions` 数组 | 20 |
| `preview_write` | `operation` 为 `insert`、`update` 或 `delete`，并带对应参数 | 采用对应操作上限 |

四个工具都需要 `connection_alias`、`database`、`table`。SQLite 的
`database` 为 `main`。条件以 AND 组合，每项为 `column`、`operator` 和
`value`。`eq`、`lt`、`lte`、`gt`、`gte` 需要非空值。`is_null` 与
`is_not_null` 省略值。例如：

```json
{
  "connection_alias": "test",
  "database": "main",
  "table": "orders",
  "changes": {"status": "closed"},
  "conditions": [{"column": "id", "operator": "eq", "value": 17}]
}
```

支持 null、布尔值、64 位整数、有限浮点数和文本。绑定失败会返回错误。拒绝整表
UPDATE / DELETE、DDL、TRUNCATE、任意 SQL、原始 WHERE、函数表达式及不能验证
副作用范围的表。参数上限 128 KiB，HTTP 的整个请求也受同样大小的外层限制。
一次性连接初始化有独立的最长 10 秒等待预算。BEGIN、预检、DML 和提交前审计
共用另一个最长 10 秒的执行预算。初始化及 BEGIN 均响应请求取消。BEGIN 未获确认
时停止后续 DML，并清理一次性连接。事务终结和连接清理各有 5 秒的等待预算。单次调用使用
独立事务。MariaDB 终结应答超时会终止一次性物理连接。SQLite 锁等待受驱动的
5 秒 busy timeout 约束，底层文件 I/O 的阻塞可能延迟物理关闭。

收到 `mcp.commit_unknown` 或 `transaction_state=rollback_unconfirmed` 时，先核查
数据库和审计，再决定后续操作。两种结果均返回 `retryable=false`。

预览只读取范围，返回估计、超限下界、整表命中或未知标记，以及风险、当前授权与执行资格。
范围无法确认时可返回 `scope=unknown`、`can_execute=false` 和 `scope_reason`。
断连、超时和数据库权限不足返回保留稳定错误码的脱敏工具错误。
预览仅用于辅助判断，无需先执行预览。执行时总是重新检查。

写入审计记录阶段、事务结果和直接影响行数，省略原始 SQL 和绑定值。提交后的审计失败通过 `audit_warning` 返回，数据库结果仍按实际状态反馈。

### MariaDB DBA 部署资格

仅为 DBA 管理了 DDL 权限隔离的实例启用写入。设置期间，普通业务账号及其角色
不能更改目标或在其他数据库创建相关外键。管理员先关闭 MCP 写入并排空会话，再运行
迁移。为写入账号仅授予目标表所需 DML 和资格视图的 SELECT。

DBA 创建 `sqarvo_guard.write_eligibility`，为每个已审查目标只返回一行
`schema_name`、`table_name`、`eligible`。只有 InnoDB 基础表、且不存在触发器、
入站或出站外键及其他不可回滚副作用时返回 `eligible=1`。视图应使用锁定的
`SQL SECURITY DEFINER` 账号，由 DBA 审查目标的列默认值和生成表达式。该账号需要能
看到全部相关触发器及 `information_schema.INNODB_SYS_FOREIGN`，包括跨库引用。
普通写入账号无需全局 `PROCESS` 或 `RELOAD`。目标库表使用小写 ASCII 字母、数字和下划线。

将档案的 UUID 和**当前档案 revision**加入服务级 `mcp.json` 后重启 MCP。键缺省时，
MariaDB 写入关闭。档案修订变化后需要 DBA 重新确认。部署责任见
[ADR 0037](../decisions/0037-mcp-bounded-writes.md)。

```json
{
  "mariadb_write_profiles": {
    "22222222-2222-4222-8222-222222222222": 7
  }
}
```

DBA 可从以下视图模板开始。先在 `sqarvo_guard.approved_target` 建立
`(schema_name, table_name)` 联合主键。该表只保存 DBA 已审查且受 DDL 隔离的目标。
给已锁定、不能登录的 `sqarvo_guard_definer` 授予白名单表的 SELECT、批准目标的
触发器元数据访问权限，以及读取 InnoDB 入站外键字典所需的 PROCESS。不给写入账号
授予白名单表、维护视图或视图定义者的权限。

```sql
CREATE DEFINER='sqarvo_guard_definer'@'localhost' SQL SECURITY DEFINER
VIEW sqarvo_guard.write_eligibility AS
SELECT a.schema_name, a.table_name,
       NOT EXISTS (
          SELECT 1 FROM information_schema.TRIGGERS AS t
          WHERE BINARY t.EVENT_OBJECT_SCHEMA = BINARY a.schema_name
            AND BINARY t.EVENT_OBJECT_TABLE = BINARY a.table_name
       ) AND NOT EXISTS (
          SELECT 1 FROM information_schema.INNODB_SYS_FOREIGN AS f
          WHERE BINARY f.REF_NAME = BINARY CONCAT(a.schema_name, '/', a.table_name)
             OR BINARY f.FOR_NAME = BINARY CONCAT(a.schema_name, '/', a.table_name)
       ) AS eligible
FROM sqarvo_guard.approved_target AS a;
```

配置完成后应从专用写入账号验证批准对象返回一行。视图负责补充该账号看不见的
触发器和外键，MCP 另查表引擎和引用字段。审查 `approved_target` 前，应确认
定义者确实能看到目标触发器和跨库外键。数据库迁移后，重新审查目标并检查视图结果。

隔离 MariaDB 写入测试使用 `SQARVO_MCP_MARIADB_WRITE_FIXTURE`，要求连接到
回环实例的 `sqarvo_mcp_write_fixture` 测试库。目标 `orders` 使用 InnoDB，
包含 `id INT PRIMARY KEY`、`note VARCHAR(100)` 和初始记录 ID 1 与 2。
配置同名资格视图且授权测试账号后，执行
`cargo test --locked -p sqarvo-mcp --test tools_mariadb bounded_write_on_dba_provisioned_fixture`。

事务终态故障注入复用同一隔离库。仅对关闭 TLS 的合成夹具，在真实 MariaDB
监听回环 53387 时运行 `python3 tests/integration/mcp_mariadb_terminal_proxy.py`。
将 `SQARVO_MCP_MARIADB_TERMINAL_FIXTURE` 指向代理回环 53388，夹具初始应无 ID
9901、9902 和 9903，然后运行
`cargo test --locked -p sqarvo-mcp --test tools_mariadb terminal_response_loss_evicts_disposable_write_session`。
代理应显示 COMMIT 和两次 ROLLBACK 收到 OK。前两次扣留应答直到客户端关闭连接，
最后一次在转发应答前断开连接。直接查夹具，
应仅看到已提交的 9901。重复测试前清除该合成记录。

初始化与 BEGIN 的故障注入使用同一代理的 `--startup` 模式。将代理 stdout 重定向
到隔离日志，并把路径设为 `SQARVO_MCP_MARIADB_STARTUP_LOG`。设置
`SQARVO_MCP_MARIADB_STARTUP_FIXTURE` 指向代理后，执行
`cargo test --locked -p sqarvo-mcp --test tools_mariadb startup_stall_or_cancellation_releases_disposable_write_session`。
该用例验证初始化应答停滞、BEGIN 应答停滞及 BEGIN 等待中取消。

## V2.1 Explicit transactions

在同一 stdio 进程或同一已认证的 HTTP MCP Session 内，可以将多次结构化 DML 放入一条物理数据库事务。保持全局 MCP 数据访问开启、只读关闭。MariaDB 沿用 V2 DBA 部署资格。如果配置了显式 `enabled_tools`，加入需要使用的四个事务工具名。

| 工具 | 参数 | 成功结果 |
|---|---|---|
| `begin_transaction` | `connection_alias`、`database`。可选 `isolation_level`，当前仅支持 `serializable`。可选 `timeout_seconds`，整数 1–120，默认 120 | `transaction_id`、固定目标、隔离级别、`expires_at`、`idle_expires_at`、`state=open` |
| `execute_in_transaction` | `transaction_id`、`operation_id`、`operation`、`table`。INSERT 使用 `rows`，UPDATE 使用 `changes` 和 `conditions`，DELETE 使用 `conditions` | 单次行数及 `count_kind`、累计行数和操作数、`idle_expires_at`、`state=open` |
| `commit_transaction` | `transaction_id` | `state=committed`、累计行数和操作数、`audit_warning`、`cleanup_warning` |
| `rollback_transaction` | `transaction_id` | `state=rolled_back`、`audit_warning`、`cleanup_warning` |

例如，先用 `{"connection_alias":"test","database":"main"}` 开启 SQLite 事务。把返回的 ID 代入一次写入，再用新的操作 ID 执行后续写入。

```json
{
  "transaction_id": "<BEGIN 返回的 ID>",
  "operation_id": "close-order-17",
  "operation": "update",
  "table": "orders",
  "changes": {"status": "closed"},
  "conditions": [{"column": "id", "operator": "eq", "value": 17}]
}
```

传入 `{"transaction_id":"<BEGIN 返回的 ID>"}` 提交或回滚。提交前，单独的连接看不到该事务的未提交变更。普通只读工具、预览和 V2 单次写入各自使用现有会话路径。显式事务只接收结构化 INSERT、UPDATE、DELETE，不支持任意 SQL、DDL、嵌套或跨数据库事务。

- 每个所有者最多 1 个事务，每个 MCP 进程最多 4 个。同一事务内操作串行。服务端限制空闲 30 秒、总时长最多 120 秒、最多 20 次 DML、累计 200 行。UPDATE 使用匹配行计数。累计超限自动回滚。
- `operation_id` 是 1–64 字节的客户端操作标识。重复提交同一个逻辑 DML 时沿用该 ID，服务端拒绝重复派发。实际 DML 出错会使事务进入 `failed`，调用 `rollback_transaction` 清理。收到 `retryable=false` 时停止自动重试。
- HTTP Session DELETE、会话失效或服务停止会清理事务。单次 SSE/HTTP 连接断开后若会话仍有效，可以在同一会话继续。stdio EOF 清理此进程的事务。其他 Session 不能使用原事务 ID。
- 关闭访问、开启只读、修改或删除档案会终止已打开的事务。共享档案文档的修订变化会使现存显式事务保守回滚。撤权和提交通过共享档案锁排序。并发撤权返回 `profile.locked` 时，设置尚未写入，可在提交终结后重试更改设置。
- 收到 `commit_unknown` 或 `rollback_unconfirmed` 时，先核对数据库状态和审计。重复终结请求返回错误及仍保留的终态摘要，从不重新发送终结命令。终态摘要在内存中有限保留，不跨进程恢复。终态审计落盘失败时，`audit_warning` 表示需核查审计，数据库应答语义保持原样。
- SQLite 可写事务从 BEGIN 起可能占写锁。5 秒 busy timeout 约束锁等待，底层文件 I/O 可能延迟物理关闭，关闭中的会话继续占用配额。MariaDB 使用专用连接，终结应答超时后隔离并尝试终止该连接。

事务工具错误的 `error.retryable` 固定为 `false`。常见状态码包括 `transaction.not_found`、
`transaction.failed`、`mcp.transaction_expired`、`mcp.transaction_ended`、
`mcp.transaction_busy`、`mcp.operation_already_used`、`mcp.commit_unknown`
和 `mcp.rollback_unconfirmed`。

定向验收使用 `cargo test -p sqarvo-mcp --test shared_access explicit_transactions_over_stdio` 和 `cargo test -p sqarvo-mcp --lib authenticated_http_transaction_isolation_and_delete`。MariaDB 终态用例复用前文的回环 53387 和 53388 代理，只用于隔离测试实例。创建专用合成账号 `mcp_v21_fixture`，仅允许回环访问且仅授予 `orders` DML/SELECT 及资格视图 SELECT。该夹具使用空凭据，库表初始无 9891、9892 和 9901–9903。设置 `SQARVO_MCP_V21_MARIADB_PROXY_LOG` 指向代理输出。以 `SQARVO_MCP_V21_MARIADB_FIXTURE=1 cargo test -p sqarvo-mcp --test shared_access explicit_mariadb_transactions_and_lost_acknowledgements` 启用真实库用例。

## V2.2 Controlled DDL

SQLite `main` 的已有普通文件及按下文受管的 MariaDB 11.8.6 支持结构化表、列和索引变更。
stdio 与认证 Streamable HTTP 使用相同输入。启用全局 MCP 数据访问并关闭只读后，数据库
账号拥有对应权限时可以调用。现有 `enabled_tools` 显式列表配置也适用于新工具。
DDL 从不加入 `begin_transaction` 返回的事务。执行需要系统凭据库可用，以保护用于本地去重的密钥。

### 参数

五个执行工具均必填 `connection_alias`、`database`、`table`、`operation_id`。
MariaDB 目标库须匹配档案默认库和会话所在库。DDL 名称使用
`[a-z_][a-z0-9_]{0,63}`，保留系统名称由 Policy 拒绝。标识符按驱动引用。

| 工具 | 专属参数 |
|---|---|
| `create_table` | 必填 `columns`。可选 `primary_key`（有序列名数组）、`unique_constraints`（列名数组的数组）、`if_not_exists`（默认 false）。仅接受空或省略的 `foreign_keys`。 |
| `alter_table` | 必填 `action`。 `add_column` 带 `column`，只允许可空、无默认和约束的普通列。 `drop_column` 带 `column_name`。 `rename_column` 带 `column_name`、`new_name`。 `rename_table` 带 `new_table`，只能同库。 |
| `drop_table` | 可选 `if_exists`（默认 false）。对象必须为普通表。删表及删列可能永久丢失数据。 |
| `create_index` | 必填 `index`、有序 `columns`，可选 `unique`（默认 false）。 |
| `drop_index` | 必填 `index`。校验与指定表关联且为独立普通非唯一索引。 |
| `preview_schema_change` | 必填 `operation`（任一上列执行工具名）和其 `arguments` 对象，省略 `operation_id`。全局只读时仍可读取预览。 |

列定义必填 `name`、`type`。可以使用 `integer`、`bigint`、`varchar`、`text`、
`decimal`、`boolean`、`date`、`datetime`、`timestamp` 和 `blob`。
`varchar` 要求 `length`（1–4096）。`decimal` 要求 `precision`（1–38）与
`scale`（0–precision）。`nullable` 默认 true，主键列必须不可空。
可选 `primary_key`、`unique`、`auto_increment`（也可写 `identity`）默认 false。
主键可用一个列级布尔或一个表级有序数组。自动编号只用于单列整数主键，SQLite 要求
`integer` 并沿用 rowid 的原生编号语义，MariaDB 使用 `AUTO_INCREMENT`。

`default` 仅接收与类型匹配的 JSON null、布尔、安全整数、文本、
`{"integer":"9223372036854775807"}` 或 `{"decimal":"12.34"}`。
SQLite 遵守声明类型与列亲和性规则。MariaDB 在专用连接上固定 SQL 模式；
日期、时间戳和 blob 的显式默认值仅支持允许的 NULL。SQL 表达式、用户 DDL 原文、
批量操作、CASCADE、外键、虚拟表、生成列、部分或表达式索引及任意修改列定义均不支持。

### 预览与结果

例如只读检查一项变更：

```json
{
  "operation": "alter_table",
  "arguments": {
    "connection_alias": "dev",
    "database": "main",
    "table": "orders",
    "action": "add_column",
    "column": {"name": "note", "type": "text"}
  }
}
```

预览返回 `current_schema`、`proposed_schema`、展示用 `generated_sql`、
`generated_sql_redacted`、`risk_level`、`transactional_ddl`、`implicit_commit`、
`reversible`、`warnings`、`can_execute` 和 `blockers`。默认值从摘要或 SQL 展示文本中
省略或替代。预览不创建 SQLite 文件；只读开关开启时，`read_only` 会成为 blocker。
预览结果基于检查时的快照，执行前再次读取。风险提示涵盖数据丢失、锁等待及可能的
数据重写。耗时、影响行数、锁范围和数据恢复无保证。

实际执行调用工具名 `alter_table`，直接传递上述 `arguments` 内容及新的 UUIDv7
`operation_id`。操作返回 ID、动作、目标、风险、告警、`state`、`effect`、
`audit_warning`、`cleanup_warning`、`state_storage_warning` 和 `replayed`。
业务错误在 MCP 工具结果中带稳定 `error.code`，`retryable=false`。
`not_started` 表示当前调用未派发。 `completed` 携带 `applied`、`already_exists`
或 `already_absent`。 `if_not_exists` 不核对已有结构等价。
`failed` 表示引擎明确拒绝，或 SQLite 写事务已确认回滚。
`outcome_unknown` 意味着结果待核查，数据库可能已经更改。
并发占用返回 `mcp.ddl_busy`，数据库结果另由持有执行槽的调用报告。
任何最新授权变化都会阻止尚未通过最终派发闸口的执行。已经交给驱动的命令可能继续。

复用操作 ID 仅用于取得原结果。重复 ID 和不同规范化请求返回冲突。操作 ID 提交窗口
24 小时，时钟超前容差 60 秒。未确认的记录不会过期；本机同一用户数据目录的执行槽
持续阻止新 DDL。记录位于 `AppPaths::data_dir()/mcp/ddl-state.sqlite3`，
审计使用原 JSONL 日志，可按 ID 核对。先核查引擎已结束、数据库结构及审计，再由本机
操作者运行下面的命令记录核查并解除屏障。记录保留原结果，不会重新发送 SQL。

```sh
sqarvo-mcp ddl-reconcile OPERATION_ID --verified-stopped
```

### MariaDB 受管实例

需要专用实例和 DBA 安装的 `sqarvo_guard.ddl_eligibility`、
`sqarvo_guard.ddl_objects` 完整元数据视图。视图由可信、锁定的独立定义者维护，
该身份能看到所有可能的入站及出站外键、视图、触发器、例程和事件。调用身份仅有
视图 SELECT 及目标库所需数据库权限，不能修改 guard、创建临时表或通过角色和代理
绕过边界。视图定义见[隔离测试夹具](../../tests/integration/mcp-ddl-mariadb-fixture.sql)。
夹具账户与授权只用于合成测试。

受管实例不得同时承载依赖 ADR 0037 的 MariaDB MCP DML 部署。管理员禁止会改写目标
或新增依赖的并发外部身份和入口；正常业务 DML 可在数据库权限下继续。先停用 DDL
并排空在途操作、核查未知终态，再运行 DBA 维护。一个受管实例的 MCP DDL 需使用一个
共享持久状态目录。实例命名锁串行化符合协议的 DDL，数据库自身控制元数据锁。
现有 ProfileStore 无 DDL 档案授权字段。详见 [ADR 0039](../decisions/0039-mcp-controlled-ddl.md)。

### V2.2 限额

执行每个应用数据目录最多 1 个，预览每进程最多 2 个。32 KiB 参数、16 KiB 生成 SQL、
64 列、16 个唯一约束，主键或索引每个 16 列，文本默认值 1 KiB。
连接 5 秒，预检/预览 8 秒，派发闸口 250 毫秒，数据库锁等待约 5 秒，执行应答 30 秒，
终结与清理各 5 秒。达到应答预算时数据库可能仍在处理。运行超限或会话不可回收时保守
拒绝继续执行。持久记录最多 4096 项，窗口内满额拒绝新操作。


## V2.3 Schema Intelligence

全局 MCP 数据访问开启后，可以在 stdio 和认证 Streamable HTTP 使用以下只读工具。
显式配置 `enabled_tools` 时加入需要的工具名。单对象工具都需要 `connection_alias`、
`database` 和 `table`，可选 `schema` 按现有对象层级校验。

| 新工具 | 响应及含义 |
|---|---|
| `list_foreign_keys` | `foreign_keys` 按真实列顺序返回 `columns[{column, referenced_column}]`、`target{database,schema,table}`、`name`、`on_update` 和 `on_delete`。SQLite 的约束名或无法解析的隐式引用列为 `null`。 |
| `list_constraints` | `constraints` 含主键、唯一、外键、CHECK 和引擎报告的其他种类。外键与 `list_foreign_keys` 共用数据。SQLite 以隐式索引来源识别 UNIQUE 约束，仅在能验证原生声明时返回 CHECK 原文。可用的 CHECK 文本只作为元数据。未知约束名、列列表及执行状态为 `null`，覆盖缺口写入 `warnings`。 |
| `get_table_ddl` | `object_type` 区分 `table`、`view` 和 SQLite `virtual_table`。`source: "database_native"` 表示数据库返回的定义。MariaDB 定义可能受 SQL mode 裁剪，SQLite 内部对象或缺少原文的对象返回能力错误。超过响应预算时 `ddl: null`、`complete: false`。 |
| `get_table_statistics` | `row_count_kind` 为 `exact`、`estimate` 或 `unknown`。MariaDB 来自 `information_schema.TABLES`，InnoDB 行数是估算，占用字节按引擎口径作为估算交付（`size_kind`）。SQLite 读取已有 `sqlite_stat1` 的可靠记录，行数为估算，缺失时为 `null`。SQLite 的单表大小和两端的 `last_analyzed_at` 目前为 `null`。 |
| `get_database_schema` | 按二进制对象名顺序返回 `objects`、`count`、`next_page_token`、`truncated`、`truncated_reason`。省略详情开关时返回对象目录。 |

建议先以 `get_database_schema` 发现对象，再按需要请求三个可独立设置的详情开关
`include_columns`、`include_indexes`、`include_foreign_keys`，或通过 `tables`
限定至最多 64 个精确名称。页大小默认 16，目录页上限 64，详情页上限 8，服务端行数
配置可进一步降低上限。续页保持目录、过滤和详情选项并传回 `next_page_token`。
分页期间外部 DDL 可能造成目录漂移。若对象详情超预算，响应可保留对象摘要并标明
缺失部分。旧 Schema Resource 和单表 Resource 仍可按原 URI 读取。

MariaDB 的原生 DDL 读取受数据库权限和元数据锁约束。可能含远程连接配置的存储引擎
当前不开放定义。SQL mode 隐藏引擎时也返回能力错误。元数据正文可能含业务敏感信息，审计记录只保留调用状态、必要计数和
SQL 指纹。统计可能陈旧，读取过程不会执行 `COUNT(*)` 或 `ANALYZE`。无数据、无法读取
及能力不支持分别用空集合、工具错误及能力错误表达。
