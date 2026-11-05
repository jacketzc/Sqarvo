# Linux 前端（GTK4 + libadwaita）

## 状态

**已接入后端 gRPC**：启动后按下面「启动顺序」完成握手，窗口在
Loading / Connected / Error / Disconnected 四态之间切换，Connected 时显示
后端版本、协议版本与传输方式。

**连接对话框已接入**（#27 / #67）：标题栏「新建连接」打开连接对话框，**驱动下拉由
`SystemService.ListDrivers` 填充**（不在前端硬编码清单），切换驱动即切换字段集——
MariaDB 是主机/端口/库/用户/TLS/密码，SQLite 是数据库文件路径 + 只读开关。
密码经 `CredentialService.StoreCredential` 写入系统凭据库，连接配置只保存
`credential_ref`；SQLite 这类本地文件驱动不写凭据库。
「测试连接」显示延迟与服务端版本，「保存并连接」建立会话并用 `ListSessions` 刷新会话列表。
错误按 `sqarvo-error-code` 分类展示（认证失败 / 凭据不存在 / 凭据库不可用 / 驱动缺失 /
超时），不合并成一句「连接失败」。

**SQL 执行已接入**（#28）：连接数据库后主区域是 SQL 工作区——多行等宽纯文本编辑器、
`Ctrl+Enter` 执行、`Escape` 取消、`GtkColumnView` 虚拟化结果表格、`FetchBatch` 分批与
接近末尾自动预取、取消/断连状态与游标释放。单元格按 `ColumnValue.kind` 分别显示
（NULL、空串、二进制、Decimal、DATE/TIME/DATETIME/TIMESTAMP、截断标记、不支持类型）。

**可度量与有上限**（#29）：`scripts/bench.sh` 一条命令产出冷启动 / 空闲内存 / 10 万行 /
200 列宽表的实测记录；结果集有客户端行数与估算字节上限（超限停止拉取并显示
`result.limit_reached`，已加载数据保留）；预取加了消费门与单批背压（用户没有滚动到新区域
就不会继续拉取）。数值见 [`docs/releases/v0.2.md`](../../docs/releases/v0.2.md) 与下方
「性能实测」。

**表数据页已接入**（#48）：连接后左侧是 `MetadataService.ListNodes` 的表/视图列表，
点击进入数据页——`TableDataService.BrowseTable` 按主键升序 + 不透明 cursor 翻页（每页
500 行、滚动到底继续取）、`GtkColumnView` 虚拟化网格、单元格编辑（文本 → 强类型值，
NULL 与空串严格区分）、脏行标记与批量 `ApplyRowChanges` 提交、逐行错误展示、
> 100 行/含删除的风险确认、事务徽标（自动提交 / 事务中 / 失败态 / 已断开）与
开始/提交/回滚按钮。无主键表只读并在横幅里说明「无稳定排序 / 无法稳定定位行」；
断连时丢弃未提交变更并说明后端已强制回滚事务。

**语法高亮尚未实现**：编辑器是纯文本，编辑器上方如实标注「当前为纯文本编辑，语法高亮
将在后续版本提供」，不用配色暗示已有高亮。

## 依赖

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev build-essential pkg-config
```

GTK 4.16 及以上（`gtk4` crate 的 `v4_16` feature），libadwaita 1.6 及以上。
本机已验证版本（2026-11-04，Ubuntu resolute）：GTK 4.22.4、libadwaita 1.9.1。

构建时**不需要**系统安装 `protoc`：`build.rs` 使用 `protoc-bin-vendored` 提供
的 protoc 生成描述符，再交给 `tonic-prost-build` 生成客户端代码到 `OUT_DIR`。
生成文件不得手工提交或修改，契约以 `proto/` 为准。

## 构建与运行

```bash
cd frontend/linux
cargo build
cargo run
```

## 启动顺序

1. 读取端点描述文件（默认 `$XDG_RUNTIME_DIR/sqarvo/backend-endpoint.json`，
   由 `directories::ProjectDirs::from("io", "sqarvo", "Sqarvo")` 推导，与后端一致）；
2. 校验 `protocol_version` 主版本（客户端支持 `v1`）；**不兼容直接进入 Error 并提示
   升级/降级，不新建后端进程**——多一个进程无法修复版本错配，还会与正在运行的后端
   争抢 socket；
3. 校验端点里的 `pid` 是否存活（Linux 读 `/proc/<pid>`）；存活则复用该后端，
   已退出则视为陈旧端点、启动自己的后端；
4. 端点文件不存在时启动 `sqarvo-backend`，轮询等待端点文件（10 秒超时，100 ms 一次），
   且必须等到 `pid` 等于自己启动的子进程，避免读到上一轮遗留文件；
5. 建立 gRPC Channel（Unix socket 自定义连接器 / 回环 TCP），每个请求携带
   `x-sqarvo-token`；
6. 调用 `SystemService.GetVersion`，用返回值再次校验协议主版本，并与端点里的
   `service_version` 对照；不一致时在界面上明确标注（不静默）；
7. 进入 Connected 后每 5 秒调用一次 `GetHealth`，失败即转为 Disconnected。

## 后端可执行文件查找顺序

1. 环境变量 `SQARVO_BACKEND_BIN` 指向的路径；
2. 与前端可执行文件同目录的 `sqarvo-backend`；
3. 仓库相对路径 `backend/target/debug/sqarvo-backend`：依次尝试可执行文件目录与
   当前工作目录的上若干层（覆盖 `cargo run` 在 `frontend/linux` 下的场景）。

显式设置了 `SQARVO_BACKEND_BIN` 但文件不可用时**不会**回退到其他候选，而是直接报错。
全部候选都找不到时，界面会列出已尝试的路径并提示设置 `SQARVO_BACKEND_BIN` 或先在
`backend/` 下执行 `cargo build -p sqarvo-backend-service`；不会 panic。

## 环境变量

| 变量 | 作用 |
|---|---|
| `SQARVO_BACKEND_BIN` | 指定后端可执行文件，优先于所有自动查找 |
| `SQARVO_ENDPOINT_FILE` | 覆盖端点描述文件路径（测试/自定义部署） |
| `XDG_CONFIG_HOME` | 连接配置目录的基准：配置写在 `$XDG_CONFIG_HOME/sqarvo/Sqarvo/connections.json` |
| `SQARVO_RESULT_ROW_LIMIT` | 结果集行数上限（默认 500000；正整数，非法值回默认并打印警告） |
| `SQARVO_RESULT_BYTES_LIMIT` | 结果集估算字节上限（默认 512 MiB；同上） |

`SQARVO_ENDPOINT_FILE` 的约束：后端自身不支持覆盖端点路径，它总是写到
`$XDG_RUNTIME_DIR/sqarvo/backend-endpoint.json`。因此当覆盖路径指向的文件不存在、
需要前端自己拉起后端时，覆盖路径必须保持 `<runtime_root>/sqarvo/backend-endpoint.json`
布局；前端会把**子进程**的 `XDG_RUNTIME_DIR` 设为 `<runtime_root>`，让后端把 socket
与端点写到同一目录。不符合该布局时前端直接报错说明原因，而不是空等超时。

## 模块结构

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 应用入口与动作（Ctrl+Q 退出、Ctrl+W 关闭窗口） |
| `src/ui.rs` | 窗口、四态状态页、事件泵与关闭流程 |
| `src/backend/endpoint.rs` | 端点定位、解析、协议主版本校验、pid 存活探测 |
| `src/backend/connection.rs` | 连接草稿、TLS 模式、`CredentialService`/`ConnectionService` 调用、错误分类 |
| `src/config.rs` | `connections.json` 读写（目录 0700、文件 0600，只含非敏感字段） |
| `src/dialog.rs` | GNOME HIG 连接对话框（`AdwDialog` + `AdwPreferencesPage`）：驱动下拉、按驱动切换字段行、文件选择器、只读开关 |
| `src/driver_form.rs` | 驱动 → 字段集的**描述表**与草稿校验/组装（纯逻辑：SQLite 空路径、文件不存在、内存库只读等，可单测） |
| `src/backend/client.rs` | tonic Channel（UDS / TCP 自定义连接器）、token 拦截器、`SystemService` 调用 |
| `src/backend/supervisor.rs` | 后端可执行文件查找、启动、等待就绪、退出清理 |
| `src/backend/worker.rs` | 后台 tokio 线程、命令/事件通道、握手与健康探测循环 |
| `src/backend/error.rs` | 可直接展示的错误文案（绝不含 token） |
| `src/backend/query.rs` | 查询域：视图模型、单元格格式化（NULL/空串/二进制/Decimal/日期时间/截断）、失败分类、结果集上限计量与预取决策（纯函数）；不依赖 GTK |
| `src/results.rs` | 结果区：自定义 `GListModel`（`RowStore`）+ `GtkColumnView` 虚拟化、预取触发（消费门/单批背压）、上限状态、状态栏 |
| `src/sql_page.rs` | SQL 工作区：编辑器、执行/取消按钮与快捷键、页面状态机、QA 滚动与取消钩子 |
| `src/backend/table.rs` | 表数据域：浏览结果/列元数据/只读原因/逐行结果/错误分类与文案（纯逻辑，不依赖 GTK） |
| `src/table_data.rs` | 表编辑纯逻辑：文本 → 强类型值、NULL/空串、全文本草稿与批次构造、风险判定、只读文案、事务徽标状态机（可单测） |
| `src/data_page.rs` | 数据页：表列表侧栏、虚拟化网格（`DataRowStore`）、单元格编辑弹窗、提交/回滚与风险确认、事务徽标、逐行错误列表 |
| `src/proto.rs` | 由 `proto/` 生成、位于 `OUT_DIR` 的客户端代码（勿手改） |
| `scripts/bench.sh` | 可复现基准：冷启动、空闲内存、10 万行、200 列宽表、不滚动背压证据 |

UI 不直接接触 socket、进程或端点文件，只与 `backend::worker` 收发消息；后续的连接
对话框（#27）与 SQL 执行（#28）应复用 `BackendClient`（`channel()` 返回可克隆的
Channel）和同一套命令/事件通道。

## 线程模型

- tokio `current_thread` 运行时独占一个后台线程（`sqarvo-backend-worker`）；
- UI → worker：`tokio::sync::mpsc`（发送不阻塞主线程）；
- worker → UI：`async_channel`，主线程用 `glib::spawn_future_local` 消费后更新界面；
- 每个 `Execute` / `FetchBatch` / `Cancel` / `Close` 都以独立 tokio 任务执行，worker 循环
  只负责状态机与事件派发：长语句在跑的时候取消命令、健康探测与新批次都不会被堵住；
- **单元格格式化（含 Decimal 拼接、时间分量格式化）在 worker 线程完成**，主线程只做
  `label.set_text`；10 万行场景不会把字符串处理压到 GTK 主循环上；
- 所有 GTK 对象只在主线程操作。

## 退出清理

- 关闭窗口（标题栏或 Ctrl+W）与应用退出（Ctrl+Q、会话结束）走同一条清理路径：
  先让 worker 停止，收到 `ShutdownComplete` 后才销毁窗口；
- 只终止**自己启动**的后端：先 `SIGTERM`，最多等 5 秒，超时再 `SIGKILL`，并如实
  报告「清理未完成」；不是自己启动的后端只断开连接，绝不删除其端点文件或 socket；
- 后端收到 `SIGTERM` 后自行删除 socket 与端点文件。

## 连接与凭据

1. 握手时调用 `SystemService.ListDrivers`，把返回的驱动填进对话框的驱动下拉
   （`AdwComboRow`）；前端**不维护自己的驱动清单**，后端多一个驱动下拉就多一项。
   前端还不认识的驱动也会列出来，但标签写着「前端暂无表单」且提交按钮禁用——
   不给一份填了也会被后端拒绝的表单；
2. 字段集由 `src/driver_form.rs` 的一张描述表决定，切换驱动只改控件可见性：

   | 驱动族 | 可见字段 | 不可见（也不会进 `ConnectionParams`） |
   |---|---|---|
   | 服务器型（`mariadb`） | 主机 / 端口 / 数据库 / 用户名 / TLS 模式 / 密码 | 数据库文件、只读 |
   | 文件型（`sqlite`） | 数据库文件（可手输或用 `GtkFileDialog` 选）/ 只读打开 | 主机 / 端口 / 库 / 用户名 / TLS / 密码 |

3. 「保存并连接」先调 `CredentialService.StoreCredential`（**仅服务器型驱动**），
   后端把密码写进系统凭据库（Linux Secret Service）并返回 `credential_ref`；
   SQLite 是本地文件数据库，后端明确拒绝用户名/密码，因此这条链路上不调用凭据服务；
4. 随后 `ConnectionService.TestConnection`（拿服务端版本与延迟）→ `Connect` →
   `ListSessions` 刷新会话页；
5. 非敏感配置写到 `$XDG_CONFIG_HOME/sqarvo/Sqarvo/connections.json`（目录 0700、
   文件 0600），字段固定为
   `name / driver / host / port / database / username / file_path / read_only / tls_mode / credential_ref`；
   **没有** password、secret 或 token 字段；
6. 系统凭据库不可用（无会话总线、已锁定、用户拒绝）时如实报 `credential.unavailable`，
   绝不把密码退回配置文件或环境变量。

### SQLite 连接的参数映射

`ConnectionParams` 没有「文件路径」字段，后端 SQLite 驱动的约定是
**用 `host` 承载路径**（`backend/crates/drivers/sqlite/src/driver.rs` 的
`SqliteConfig::from_params`），并且拒绝多余字段。前端按同一契约组装：

| `ConnectionParams` 字段 | SQLite 取值 |
|---|---|
| `driver_id` | `sqlite` |
| `host` | 数据库文件路径 |
| `port` | `0`（SQLite 不支持端口） |
| `database` / `username` / `credential_ref` | 空（非空会被后端以 `config.invalid` 拒绝） |
| `tls_mode` | `TLS_MODE_DISABLED` |
| `properties["read_only"]` | `"true"` / `"false"` |

界面层的校验（`driver_form::build_draft`）：路径为空拦住、文件不存在给出
「数据库文件不存在：<路径>」而不是让后端报晦、`:memory:` 不允许只读
（与后端同一条规则）。会话显示名按驱动派生：`用户@主机/库` 或 `文件名（SQLite）`，
标题栏与会话行都能看出连的是哪种数据库。

失败提示按稳定错误码分开：`mariadb.auth_failed`（认证失败：用户名或密码错误）、
`credential.not_found`（凭据不存在）、`credential.unavailable`（系统凭据库不可用）、
`driver.not_found` / `capability.unsupported`（驱动侧不支持）、超时（`DEADLINE_EXCEEDED`
或客户端 30 秒上限），其余情况显示状态码与后端文案。

## SQL 工作区（#28）

连接数据库后，主区域从「已连接」状态页切换为 SQL 工作区：顶部是连接信息与按钮，
中间是编辑器（上方有纯文本提示），下方是状态行 + 虚拟化结果表格。

### 交互

| 操作 | 方式 |
|---|---|
| 执行 | 「执行」按钮或 `Ctrl+Enter`（捕获阶段处理，不会插入换行） |
| 取消 | 「取消」按钮或 `Escape`（幂等：重复点击不发第二次 RPC、不弹错误） |
| 浏览结果 | 滚轮/滚动条；接近末尾时自动预取下一批 |
| 断开后端 | 工作区右上「断开后端」（与状态页同一个断开路径） |

### 执行链路

1. `ExecuteQueryRequest { session_id, sql, max_rows_per_batch=500, auto_commit=true,
   timeout_millis=0 }`；`timeout_millis=0` 表示由服务端策略决定，客户端不凭空加期限。
   **Execute 没有客户端超时**：本地提前放弃会让界面显示「超时」而后端仍在执行；
2. 响应里的 `query_id`、`statements`（列元数据、`rows_affected`、`has_result_set`）、
   首批 `rows`、`page_token`、`has_more` 全部接收；
3. 有结果集 → 建立列并显示首批行；无结果集（UPDATE/INSERT/DDL）→ 显示「影响 N 行」，
   不显示空表格；`statements` 的每一条都写进第二行状态；
4. 后续批次用 `FetchBatch(query_id, statement_index=0, page_token, max_rows=500)`；
5. 读完（`has_more=false` / `complete=true`）或取消/关闭时调用 `Close(query_id)`
   释放驱动游标（ADR 0005）。`Close` 幂等，服务端已结束时调用同样成功。

### 分批与预取

- 每批 500 行（服务端单批上限 5000）；**Execute 之后不会一次性拉全量**：10 万行查询在
  不滚动时只加载首批 500 行，状态行显示「总数未知（服务端分批返回）」；
- 预取决策是纯函数 `prefetch_decision`（`backend/query.rs`），规则按顺序：
  1. 没有更多批次 / 已达客户端上限 / 已有一批在途 → 不拉；
  2. 视口尚未布局 → 不拉；
  3. **消费门**：内容超过一屏时，只有用户在**上次预取之后继续向下滚动**才拉——
     停在末尾不动不会自动继续拉取（旧实现会因为「末尾触发」+ 内容增长反复命中，
     最终把整个结果集读进内存）；
  4. **填充门**：内容不足一屏时允许继续拉，但要求上一批确实带来了新行，避免空批空转；
  5. 几何条件：滚动位置距末尾不足 2 页（`should_prefetch`）；
- **同一时刻最多一批在途**：界面 `fetch_requested` + worker `loading` 双重保证，重复的
  `FetchNext` 只记一行「忽略 FetchNext：<原因>」；
- 每次暂停的原因只在上次原因变化时打印一行（`[query] 暂停预取：未滚动到新区域所以不预取`），
  滚动不会把日志刷爆，但「为什么没有继续拉」始终可复核；
- 用户继续滚动（或 QA 的 `--qa-scroll-to-row` 目标尚未加载）时，才会发出下一批。

### 结果集上限（客户端）

| 项 | 值 |
|---|---|
| 行数上限 | `SQARVO_RESULT_ROW_LIMIT`，默认 500000 |
| 估算字节上限 | `SQARVO_RESULT_BYTES_LIMIT`，默认 512 MiB |
| 触发后的行为 | 丢弃本批放不下的行、保留已加载行、释放游标、停止拉取 |
| 界面 | 「已达客户端上限 · 已保留 N 行」+ 明细行，错误码 `result.limit_reached` |
| 之后 | 执行按钮仍可用：调大上限或改写 SQL 后可以直接跑下一条查询 |

- **这是客户端上限，不是服务端错误**：服务端没有拒绝任何请求；与
  `query.limit_reached`（服务端并发查询上限）是两回事，两个错误码不会混用；
- 估算口径是**近似值**：`行开销 48 B + 每格 64 B + 显示文本 + tooltip` 的长度，
  只覆盖前端持有的显示数据，**不含 GTK 渲染开销**（见下方 200 列实测）；
- 上限只在 worker 线程判定（`ResultMeter::admit_prefix`），首批与后续批次都会走同一条路径；
- 触顶时本批里放不下的行会被丢弃，不会为了凑数再发一次 `FetchBatch`（有单测覆盖）。

### 虚拟化

- `RowStore` 是自定义 `gio::ListModel`：按位置返回一个 `BoxedAnyObject`，内部是
  `Rc<RowView>`；`GtkColumnView` 只对**可见行**调用工厂的 `setup`/`bind`；
- 行数据（已格式化的字符串）在 worker 线程生成，界面不重复解析 proto；
- **不会为每一行创建 widget**：10 万行时同时存在的单元格 `GtkLabel` 只有可见的几十个，
  这也是「重复滚动不涨内存」的原因（见下方实测）。

### 类型显示

| `ColumnValue.kind` | 显示 |
|---|---|
| `VALUE_KIND_NULL` | 灰色斜体 `NULL`（tooltip 注明与空串不同） |
| `TEXT` 空串 | `''`（暗色斜体；与 NULL 明确区分） |
| `BYTES` | `0x89 50 4E 47 …（共 N 字节）`；超大值标注「前缀 N 字节」 |
| `INT64` / `UINT64` / `FLOAT64` | 等宽右对齐；浮点用最短可往返表示 |
| `DECIMAL` | `unscaled_value` + `scale` 精确拼字面量，**绝不经过 f64** |
| `DATE` / `TIME` / `DATETIME` | 按分量格式化：`YYYY-MM-DD`、`HH:MM:SS[.fff]` |
| `TIMESTAMP` | `unix_nanos` + `offset_seconds` 转本地时间并带 `±HH:MM` 偏移 |
| `truncated=true` | 值前加「…（已截断）」并在 tooltip 说明只收到前缀（ADR 0007） |
| `VALUE_KIND_UNSUPPORTED` | `原始类型名：文本`；类型名缺失时写「类型名未提供」 |
| `JSON` / `UUID` / `ARRAY` | 等宽显示；数组逐元素按同一规则格式化 |

> 说明：MariaDB 驱动的 `TIMESTAMP` 不做隐式时区转换，按服务端返回的墙上时间以
> DATETIME 分量传输（`with_time_zone=false`），因此这一列显示为无偏移的分量；
> 带偏移的 `TimestampValue` 路径由 `format_timestamp` 处理并有单元测试。
>
> 另：当前后端 `mapping.rs` 不填 `ColumnValue.original_type_name`，`UNSUPPORTED` 单元格的
> 「原始类型名」实际来自列元数据缺失时的兜底文案「类型名未提供」——属于后端 TODO，
> 前端按契约优先使用 `original_type_name`，字段补齐后即生效。

### 状态

| 状态 | 表现 |
|---|---|
| Loading | 状态行 spinner +「执行中…」，占位页「正在执行查询」 |
| Streaming | 「加载中 · 已加载 N 行 · 总数未知（服务端分批返回）」 |
| Complete | 「已完成 · 已加载 N 行 · 共 N 行」（绿色标题） |
| Empty | 「空结果集 · 共 0 行」——成功后没有行，与失败严格区分 |
| NoResultSet | 「已执行 · 无结果集」+「语句 1 影响 N 行」（UPDATE/INSERT/DDL） |
| Error | 红色「错误 · 已保留 N 行」+ 结论/后端文案/错误码 |
| Cancelled | 橙色「已取消 · 已保留 N 行」+ 说明（例如查询 ID 未返回） |
| Disconnected | 橙色「已断开 · 已保留 N 行」+ 顶部横幅，已加载数据仍可查看 |

取消与断连都**保留已加载行**，不会清空用户已经看到的数据。

### 取消语义（含已知契约缺口）

- `Cancel` 幂等：重复点击不再发 RPC，也不产生错误提示；`Escape` 与按钮同一条路径；
- 已有 `query_id` 时：`Cancel(query_id)` → 立即进入 Cancelled → 随后 `Close(query_id)`；
- **缺口**：`Execute` 在服务端返回之前不携带查询 ID（`ExecuteQueryRequest` 没有
  客户端指定的 ID，`ExecuteQueryResponse` 才带 `query_id`），因此对
  `SELECT SLEEP(30)` 这类**尚未返回的 Execute**，前端无法发送 `Cancel`/`Close`。
  此时界面仍在毫秒级进入 Cancelled，并如实说明「服务端尚未返回查询 ID：界面已停止等待，
  但该语句可能仍在数据库侧执行」；该 `Execute` 返回后 worker 会补发 `Close`。
  可行的修复方向（需要改 `proto/`，本 Issue 范围外）：允许客户端在请求里携带 `query_id`，
  或把 `Execute` 改成服务端流式先回 ID 再回结果；
- 另一个后端行为：取消后立即退出应用时，后端可能因为语句仍在执行而 5 秒内不响应
  `SIGTERM`，前端会如实报告「清理未完成，已发送 SIGKILL」。

### 断连

- 每 5 秒 `GetHealth` 探测失败、或查询返回 `UNAVAILABLE` / `session.not_found` 时：
  界面进入 Disconnected，顶部横幅说明原因并提供「重新连接后端」；
- worker 对活动查询尽力 `Cancel` + `Close`（失败只记日志），随后终止自己启动的后端；
- 已加载的行保留、标明「已断开」，执行/取消按钮禁用。

### 性能实测（2026-11-04，本机，debug 构建）

用 `scripts/bench.sh` 一条命令复现（脚本自己清理残留进程与 runtime 目录，失败时非 0 退出
并指明步骤）：

```bash
cd frontend/linux
cargo build
cd ../.. && cargo build -p sqarvo-backend-service   # 后端二进制
cd frontend/linux && ./scripts/bench.sh --out /tmp/bench.md
```

本机（AMD Ryzen 7 8745H / 16 核 / 24.8 GiB，GTK 4.22.4，libadwaita 1.9.1，debug 构建）：

| 场景 | 指标 | 实测 |
|---|---|---|
| 冷启动（3 次） | 进程入口→Connected 中位数 | **240 ms**（样本 243 / 240 / 234） |
| 空闲内存 | 连接后静置 5s VmRSS / VmHWM | **149.3 MB** / 149.3 MB |
| 10 万行 | 首批到达（500 行） | **953 ms** |
| 10 万行 | 全量加载（100000 行，199 个后续批次） | **2808 ms** |
| 10 万行 | 全量加载后 VmRSS | **191.6 MB**（约 0.42 MB / 千行） |
| 10 万行（不滚动 3s） | 已加载行数 / 后续批次 | **500 行 / 0 批**（背压生效） |
| 200 列 × 2000 行 | 首批 / 全量加载 | 1137 ms / **3554 ms** |
| 200 列 × 2000 行 | 加载后 VmRSS | **603.4 MB** |

结论与未达标项（如实记录）：

- 冷启动与 10 万行场景达到 v0.2 的「可用」标准：首屏 < 1 s，10 万行 3 s 内读完，
  RSS 191.6 MB；**这不是 release 构建的数值**，release 未在本轮测量；
- **200 列宽表内存明显偏高**：2000 行 × 200 列占 603.4 MB，而同样 10 万单元格的
  20 列 × 5000 行只有 208.6 MB、100 列 × 1000 行为 376.5 MB（对照实验，同一轮测量）。
  内存随**列数**上升（约 2 MB/列量级），与行数关系很小（200 列 500 行 563.5 MB →
  2000 行 603.4 MB，仅 +40 MB）。已排除「按行增长」的解释，但**没有定位到具体分配点**
  （怀疑是 `GtkColumnView` 每列的单元格/管理器开销），因此不声称已优化；
- 估算字节与实际 RSS 有系统偏差：10 万行估算 25.3 MiB / 实际增量约 42 MB（约 1.7 倍），
  200 列 2000 行估算 40.0 MiB / 实际 603 MB。上限口径只覆盖前端显示数据，**不含 GTK
  渲染开销**，不能当作 RSS 的精确预测；
- 冷启动数值的计时起点是进程内 `main` 入口（`qa::init`），exec 之后动态链接的几毫秒
  未计入；
- 本轮未重复 #28 的「反复滚动不涨内存」实验；#28 记录为：加载完成后 10 秒内 6 次
  `末尾 → 顶部 → 5 万 → 顶部 → 5 万 → 末尾`，RSS 恒定 191.7 MB。

局限：协议没有随机定位（`page_token` 只能顺序推进），要看到第 10 万行必须把前 10 万行
都加载进来，所以「加载到末尾」时 RSS 必然随已加载行数增长；这不是滚动泄漏，而是数据本身。
若未来需要「跳到末尾不占内存」，需要服务端提供可寻址分页或视窗查询。

## 数据页（#48）

连接数据库后，主区域左侧出现「表与视图」列表（`MetadataService.ListNodes` 根节点，
失败时用分类文案展示，不是空列表）；点击条目进入右侧数据页。右上「SQL 工作区」
切回编辑器，两个页面共用同一个数据库会话。

### 浏览与翻页

- `TableDataService.BrowseTable(object, page_size=500, page_token)`：固定按主键升序，
  cursor 不透明且只由 worker 持有，界面只在滚动接近末尾（或内容不足一屏）时请求下一页；
  同一时刻最多一页在途（单批背压），停在末尾不动不会继续拉取；
- 列头显示**原始类型名**（`varchar(64)`、`decimal(12,3)`…），来自 `ColumnMeta`；
- 行号跨页连续；`has_more=false` 后不再请求；
- 空表显示「空表 · 共 0 行」，与错误/只读严格区分；
- 游标非法（`table_data.cursor_invalid`）时停止翻页并展示原因，**不回退到第一页**。

### 编辑

| 操作 | 方式 |
|---|---|
| 编辑单元格 | 双击单元格打开编辑弹窗（文本框 + 「设为 NULL」开关） |
| 标记删除 | 选中一行后点「删除选中行」（提交前可放弃更改撤销） |
| 提交 | 「提交更改（N）」；`N` 为待提交行数 |
| 放弃 | 「放弃更改」把当前值恢复为加载时的值 |

- 值按列类型转换：整数（区分无符号）、浮点、Decimal（`unscaled_value + scale`，不经 f64）、
  布尔（1/0、true/false、是/否）、DATE/TIME/DATETIME/TIMESTAMP（时间戳可写 `+08:00`）、
  TEXT/JSON/UUID；转换失败就地显示原因，不静默丢弃；
- **NULL 与空串**：文本列的空文本是空字符串 `''`；要写 SQL NULL 必须勾选「设为 NULL」。
  数值/时间列的空文本按 NULL 处理（不可空列直接报错）。字符串 `NULL` 仍是文本；
- 脏行在行首显示 `●` 并高亮修改过的单元格，状态行右侧显示「脏行 N · 待提交 M 条」；
- 截断列（`truncated=true`）、二进制/数组/未映射类型**不进入编辑态**并给出原因
  （提交截断值会被服务端以 `table_data.truncated_value` 拒绝）；
- 提交只发送「主键值 + 列变更」（`ApplyRowChanges`），前端不拼接任何 SQL；
  `expected_rows_affected=1` 由服务端核对。

### 风险确认

- 预计影响 **> 100 行**，或**包含任何删除**时，提交前弹 `AdwAlertDialog` 确认；
  删除会单独警告「提交后无法通过本界面撤销」；
- 未确认不发送任何变更；服务端仍会以 `table_data.confirmation_required` 兜底
  （客户端不自行放宽阈值），此时界面会重新弹出同一个确认对话框；
- 单批上限 500 行：服务端返回 `clamped` / `deferred_changes` 时，界面明确提示
  「还有 N 条未提交，请再次提交」，不静默丢弃。

### 事务徽标与按钮

| 徽标 | 含义 | 允许的操作 |
|---|---|---|
| 自动提交 | 未打开事务，行变更立即生效 | 开始事务 |
| 事务中 / 事务中（只读） | 显式事务；只读事务禁止编辑 | 提交事务 / 回滚事务（只读态可编辑性另受限制） |
| 事务失败（只能回滚） | 服务端 `transaction.failed` | 仅回滚 |
| 已断开 | 后端/会话失效，后端已强制回滚 | 无（横幅提供重新连接） |

- 「开始事务」带「只读」开关；只读事务下编辑与删除会被拒绝并说明原因；
- 有未提交更改时「提交事务」不可用（先提交或放弃更改），避免误以为更改已随事务落库；
- 事务结束（提交/回滚）后自动重新浏览当前表，显示数据库真实状态。

### 只读态

| 原因 | 界面行为 |
|---|---|
| 无主键表（`table_data.no_primary_key`） | 浏览可用；横幅说明「无稳定排序 / 无法稳定定位行」，编辑/删除/提交禁用 |
| 驱动未声明行编辑（`capability.unsupported`） | 浏览可用；横幅说明驱动不支持行编辑 |
| 只读事务 | 横幅说明只读事务；回滚后可重新开始读写事务 |

只读原因**始终以文字说明**，不是只把按钮置灰；`no_primary_key` 的文案不会暗示存在
rowid / ctid 之类的近似定位方式。

### 断连

`GetHealth` 失败、`UNAVAILABLE` 或 `session.not_found` 时：数据页进入「已断开」，
**丢弃未提交的界面更改**并在横幅里写明「未提交的 N 行变更已丢弃；数据库事务已由后端
强制回滚」。已加载行保留为只读视图。

### 无人值守验证参数

| 参数 | 作用 |
|---|---|
| `--qa-data-table <name>` | 通过侧栏按名打开表（与点击条目同一条路径） |
| `--qa-data-begin-txn` | 打开表后点「开始事务」 |
| `--qa-data-read-only` | 开始只读事务（配合上一个参数） |
| `--qa-data-edit <row>:<column>:<value>` | 用编辑态的值转换修改单元格（可重复；行号 0 起） |
| `--qa-data-delete-row <row>` | 标记删除一行（可重复） |
| `--qa-data-scroll-to-row <n>` | 滚动到第 n 行，未加载时继续取页 |
| `--qa-data-commit` | 点「提交更改」（必要时弹风险确认） |
| `--qa-data-rollback` | 点「回滚事务」 |
| `--qa-data-reload` | 提交/回滚后重新浏览并打印行值 |
| `--qa-auto-confirm-risk` | 无人值守时自动确认风险对话框（仍走同一个对话框与提交路径） |
| `--qa-data-fail-txn` | 重放「服务端报 `transaction.failed`」的失败路径（走同一个 `show_failure`） |
| `--qa-expect-data-state <loading\|ready\|empty\|error\|disconnected>` | 截图时刻断言数据页状态，不符则退出码 3 |
| `--qa-expect-txn <auto\|active\|readonly\|failed\|disconnected>` | 截图时刻断言事务徽标（文案 + CSS 类 + 按钮可用性），不符则退出码 3 |

`--qa-expect-*` 必须与 `--screenshot` 一起用（断言发生在截图那一刻，画面先落盘再断言，
断言失败也不会丢证据）。退出码：`0` 通过、`2` 截图失败、`3` 断言失败。

#### 数据页三态与事务失败态的取证（#62）

```bash
# Loading：外部会话 LOCK TABLES WRITE 锁住表，browse 阻塞，界面停在 Loading
podman exec <mariadb> mariadb -u… -p… sqarvo_test_62 \
  -e "LOCK TABLES locked_items WRITE; SELECT SLEEP(14);" &
./target/debug/sqarvo-desktop --qa-profile /tmp/profile.json --qa-password-env SQARVO_QA_PASSWORD \
  --qa-data-table locked_items --qa-expect-data-state loading \
  --screenshot ~/sqarvo-v021-shots/data-page-loading.png --screenshot-delay-ms 3000

# Empty：空表
… --qa-data-table empty_items --qa-expect-data-state empty --screenshot …/data-page-empty.png

# Error：唯一键冲突，提交被服务端拒绝
… --qa-data-table uniq_items --qa-data-edit 0:code:B --qa-data-commit \
  --qa-expect-data-state error --screenshot …/data-page-error.png --screenshot-delay-ms 5200

# 失败态徽标：先产生一行脏数据，再注入 transaction.failed
… --qa-data-table demo_items --qa-data-begin-txn --qa-data-edit 0:note:alpha-edit \
  --qa-data-fail-txn --qa-expect-txn failed --qa-expect-data-state error \
  --screenshot …/data-page-txn-failed.png --screenshot-delay-ms 3500
```

失败态只能注入：一个 GUI 进程只有一条数据库会话，而服务端进入失败态的确定性来源是
死锁（1213），必须由第二条会话制造，单会话无法复现；注入点调用的仍是真实 RPC 错误
回来时的同一个 `show_failure`，徽标、按钮、横幅都走真实代码。

不带这些参数的正常启动路径完全不受影响。

## 安全约定

- 端点文件里的 token 只保存在 `BackendClient` 内部；`Debug` 实现与所有日志/错误文案
  只输出 `<redacted>` 或 token 长度；
- 数据库密码只出现在 `ConnectionDraft`（`Debug` 为 `<redacted>`）到
  `CredentialService.StoreCredential` 的这一次调用里；UI 不保存、不回显已保存密码，
  日志与错误文案不含密码；
- 不引入 WebKitGTK 或任何 Web 视图。

## 已知限制

- **语法高亮、多标签工作区、查询历史尚未实现**（编辑器是纯文本 `GtkTextView`）；
- **结果集上限是近似估算**：按前端显示文本 + 固定开销计算，不含 GTK 渲染开销；
  200 列宽表 2000 行实测 RSS 603.4 MB，远高于估算的 40.0 MiB（见「性能实测」）；
- **顺序分页**：`page_token` 只能向前推进，要看第 N 行必须加载前 N-1 行，「跳到末尾」必然
  占用与行数成正比的内存；服务端可寻址分页尚未实现；
- 触顶时**当前批次里放不下的行会被丢弃**，不是「从下一批继续」：调大上限后重新执行才能
  看到完整结果；
- 默认上限（500000 行 / 512 MiB 估算）按本机 24 GiB 内存选择，较小内存机器应显式调低；
- 冷启动与内存数值都来自**debug 构建**，release 构建的基准尚未测量；
- SQL 工作区的查询仍是 `auto_commit=true`（查询侧不使用显式事务）；数据页的事务按钮
  只作用于表数据的行变更，两者共用同一个数据库会话；
- 数据页**不支持插入新行**（v0.2.2）、不提供按非主键列排序（cursor 只按主键升序）；
- 截断列（`truncated=true`）禁止编辑，避免把前缀写回数据库；「查看完整值」未实现；
- v0.1 只有一条语句的结果集：`statements` 全部如实列出，但表格只展示 `statement_index=0`；
  多语句/单语句限制由服务端决定，前端不伪造；
- 取消一个**尚未返回查询 ID** 的 `Execute` 无法通知服务端（见「取消语义」）；
- 大字段只显示服务端返回的前缀（`truncated=true`），「查看完整值」的延迟加载未实现；
- **SQLite 只能打开已存在的数据库文件**：连接对话框会拦住不存在的路径（避免后端报晦），
  因此本版本还不能用它新建空库；先用 `sqlite3` 建库再连接。`:memory:` 可以用，但不支持只读；
- 前端只认识 `mariadb` 与 `sqlite` 两个驱动的字段集：后端注册的其他驱动会出现在下拉里，
  但标签标注「前端暂无表单」且提交禁用（不猜字段集）；
- 会话页只展示 `ListSessions` 结果，暂未提供「断开单个数据库会话」按钮；
- 空密码账户不受支持（`StoreCredential` 拒绝空 secret，避免客户端漏填字段时覆盖已保存密码）；
- 只支持本地传输（Unix socket、回环 TCP），无 TLS / 远程后端；
- 端点覆盖（`SQARVO_ENDPOINT_FILE`）仅在标准目录布局下可用于自动拉起后端；
- 平台判定 pid 存活依赖 Linux `/proc`，其他平台保守视为存活，由连接失败暴露问题。

## 手工验证

```bash
# 1. 清理残留
pkill -x sqarvo-desktop; pkill -x sqarvo-backend; rm -rf /run/user/1000/sqarvo

# 2. 启动（终端会同时显示前端与后端日志）
cd frontend/linux && cargo run

# 3. 期望：窗口显示「已连接」与后端版本；runtime 目录出现 0600 端点文件与 socket
ls -l /run/user/1000/sqarvo

# 4. 关闭窗口后：无 sqarvo-backend 进程，端点文件与 socket 均已消失
pgrep -x sqarvo-backend; ls -A /run/user/1000/sqarvo

# 5. 连库后验证查询链路：Ctrl+Enter 执行、Escape 取消、滚到末尾自动预取
#    （无人值守版本见下面的 --qa-sql-file / --qa-execute）
```

### 取消耗时的复查步骤

1. 用上面的命令跑 `SELECT SLEEP(30);`，加 `--qa-cancel-after-ms 1000`；
2. 终端日志里找两行：
   `[query] t=+Ams 点击取消` 与 `[query] t=+Bms 状态=Cancelled（点击取消后 N ms）`；
3. `N = B - A`；本机实测 **0 ms**（界面立即进入 Cancelled）。
   注意：这条断言只覆盖**界面状态**；服务端能否立刻停下取决于是否已拿到 `query_id`
   （见「取消语义」的契约缺口）。

## 无人值守取证（`--screenshot`）

GNOME Wayland 会话里 `gnome-screenshot` / `org.gnome.Shell.Screenshot` 需要
xdg-desktop-portal 的交互授权，无人值守时会被拒绝；XWayland 根窗口不做合成，
`x11grab` 只能抓到全黑。为了在没有桌面截图权限时也能拿到**真实画面**，应用支持把窗口
自己的渲染结果写成 PNG：

```bash
pkill -x sqarvo-desktop; pkill -x sqarvo-backend; rm -rf /run/user/1000/sqarvo
DISPLAY=:0 WAYLAND_DISPLAY=wayland-0 ./target/debug/sqarvo-desktop \
  --screenshot /tmp/ui.png --screenshot-delay-ms 3500
```

延时不指定时默认 4000 ms；写盘后窗口关闭并走同一套清理路径。截图失败时进程退出码为 2，
不会静默成功。该参数只用于开发/自动化验证，不属于产品功能。

### SQL 执行的无人值守验证（`--qa-sql-file` / `--qa-execute` / `--qa-scroll-to-row` / `--qa-cancel-after-ms`）

```bash
cat > /tmp/q.sql <<'EOF'
SELECT seq AS n, CONCAT('row-', seq) AS label FROM seq_1_to_100000;
EOF

pkill -x sqarvo-desktop; pkill -x sqarvo-backend; rm -rf /run/user/1000/sqarvo
DISPLAY=:0 WAYLAND_DISPLAY=wayland-0 XDG_CONFIG_HOME=/tmp/sqarvo-cfg SQARVO_QA_PASSWORD='***' \
  ./target/debug/sqarvo-desktop \
  --qa-profile /tmp/profile.json --qa-password-env SQARVO_QA_PASSWORD \
  --qa-sql-file /tmp/q.sql --qa-execute \
  --qa-scroll-to-row 50000 --qa-scroll-to-row 99999 \
  --screenshot /tmp/result.png --screenshot-delay-ms 14000
```

- `--qa-sql-file <path>`：把文件内容填进**与用户输入相同的 `GtkTextView` 缓冲区**；
- `--qa-execute`：连接成功后触发与「执行」按钮**相同的方法**（含空 SQL 校验与状态机）；
- `--qa-scroll-to-row <n>`：滚动到第 n 行（0 起，可重复，按顺序每隔 1 秒执行一次）。
  未加载到该行时按批预取——与用户快速拖到末尾走的是同一条滚动/预取路径，没有绕过 UI；
- `--qa-cancel-after-ms <n>`：执行后 n 毫秒点两次「取消」（间隔 150 ms），用于验证
  幂等与「点击 → Cancelled」耗时；日志里同时打印点击与状态时刻；
- `--qa-exit-after-ms <n>`：启动后 n 毫秒自动关窗（走正常清理路径），基准脚本据此
  无人值守退出；
- `--qa-second-sql-file <path>` + `--qa-execute-again-after-ms <n>`：首次执行后 n 毫秒
  用第二段 SQL 再执行一次（与用户按 Ctrl+Enter 同一条路径），用于验证「达到上限 / 取消
  之后仍能继续查询」；
- 所有 `[query]` / `[qa]` 日志带 `t=+Xms`（进程启动起算），可与 `/proc/<pid>/status`
  的 `VmRSS` 采样按时间对齐；
- 不存在绕过 UI 直接调 gRPC 的捷径。

### 数据页的无人值守验证（#48）

```bash
pkill -x sqarvo-desktop; pkill -x sqarvo-backend; rm -rf /run/user/1000/sqarvo
DISPLAY=:0 WAYLAND_DISPLAY=wayland-0 XDG_RUNTIME_DIR=/run/user/1000 \
XDG_CONFIG_HOME=/tmp/sqarvo-cfg SQARVO_QA_PASSWORD='***' \
  ./target/debug/sqarvo-desktop \
  --qa-profile /tmp/profile.json --qa-password-env SQARVO_QA_PASSWORD \
  --qa-data-table demo_items --qa-data-begin-txn \
  --qa-data-edit 0:name:alpha-preview \
  --screenshot /tmp/data-page-dirty-txn.png --screenshot-delay-ms 3200
```

- 打开表、开始事务、编辑单元格都走用户路径（侧栏条目 / 事务按钮 / 编辑态的值转换）；
- 提交路径加 `--qa-data-commit --qa-data-reload`（可选 `--qa-auto-confirm-risk`），
  日志里能看到「提交 1 条行变更」「影响 1 行」以及重新浏览后的行值；
- 回滚路径把 `--qa-data-commit` 换成 `--qa-data-rollback`，重新浏览后值应保持不变；
- 翻页用 `--qa-data-scroll-to-row 1100`（表需足够大），日志给出第 N 页行数与累计行数；
- `--qa-data-table demo_notes`（无主键表）会进入只读态并打印只读原因。

### 连接对话框的无人值守验证（`--qa-profile` / `--qa-password-env`）

```bash
# MariaDB
cat > /tmp/profile.json <<'EOF'
{"driver":"mariadb","host":"127.0.0.1","port":53306,
 "database":"sqarvo_test_27","username":"sqarvo_test",
 "tls_mode":"preferred","name":"sqarvo_test_27@127.0.0.1"}
EOF

pkill -x sqarvo-desktop; pkill -x sqarvo-backend; rm -rf /run/user/1000/sqarvo
XDG_CONFIG_HOME=/tmp/sqarvo-cfg SQARVO_QA_PASSWORD='***' \
  ./target/debug/sqarvo-desktop \
  --qa-profile /tmp/profile.json --qa-password-env SQARVO_QA_PASSWORD \
  --screenshot /tmp/connect.png --screenshot-delay-ms 7000

# SQLite（只读）
cat > /tmp/sqlite-ro.json <<'EOF'
{"driver":"sqlite","path":"/tmp/sqarvo67/demo.db","read_only":true}
EOF
pkill -x sqarvo-desktop; pkill -x sqarvo-backend; rm -rf /run/user/1000/sqarvo
XDG_CONFIG_HOME=/tmp/sqarvo-cfg-sqlite ./target/debug/sqarvo-desktop \
  --qa-profile /tmp/sqlite-ro.json --qa-data-table demo_items \
  --qa-expect-data-state ready --qa-expect-txn auto \
  --screenshot /tmp/sqlite-readonly.png --screenshot-delay-ms 5000
```

- 后端握手完成后自动打开连接对话框，把档案里的值填进**与用户输入相同的控件**
  （密码从环境变量取，命令行里不出现明文），再触发与「保存并连接」按钮**相同的回调**；
- `driver: "mariadb"` 需要 `host` / `database` / `username`；
  `driver: "sqlite"` 需要 `path`（文件必须已存在；`":memory:"` 不接受 `read_only`），
  不需要主机/端口/用户/密码；
- 档案里的驱动必须是后端 `ListDrivers` 返回的驱动，否则进程以**退出码 4** 结束
  （档案没生效就不该被当成通过）；
- 不存在绕过对话框直接调 gRPC 的捷径：自动化走的就是用户会走的那条路径；
- `--screenshot` 拍的是**当前最上层**界面：对话框还开着（例如认证失败、或配合
  `--qa-action test`）就拍对话框，否则拍主窗口；拍完会强制关闭对话框再走正常退出路径；
- 档案文件缺少该驱动需要的字段、`driver` 是前端不认识的驱动、或密码环境变量
  为空时会直接报错/保持密码行为空，不会静默用错参数冒充通过。

#### 多驱动与只读态取证（#67）

```bash
# 驱动选择器 + SQLite 字段集（--qa-action test 让对话框留在屏幕上）
./target/debug/sqarvo-desktop --qa-profile /tmp/sqlite.json --qa-action test \
  --screenshot ~/sqarvo-v021-shots/driver-picker.png --screenshot-delay-ms 3500

# SQLite：打开表 → 改一行 → 提交 → 重新浏览（外部 sqlite3 复核落库结果）
./target/debug/sqarvo-desktop --qa-profile /tmp/sqlite.json \
  --qa-data-table demo_items --qa-data-edit 0:note:edited-by-qa \
  --qa-data-commit --qa-data-reload --qa-expect-data-state ready \
  --screenshot ~/sqarvo-v021-shots/sqlite-data-edit.png --screenshot-delay-ms 7000
sqlite3 /tmp/sqarvo67/demo.db "SELECT note FROM demo_items WHERE id = 1;"
```

只读连接的日志会出现 `[data] 只读原因：只读：连接以只读方式打开：…`，
截图上方横幅是同一段文案——「只读态在界面上可见」有日志与画面两份证据。
