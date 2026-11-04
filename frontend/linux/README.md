# Linux 前端（GTK4 + libadwaita）

## 状态

**已接入后端 gRPC**：启动后按下面「启动顺序」完成握手，窗口在
Loading / Connected / Error / Disconnected 四态之间切换，Connected 时显示
后端版本、协议版本与传输方式。

**连接对话框已接入**（#27）：标题栏「新建连接」打开 MariaDB 连接对话框，密码经
`CredentialService.StoreCredential` 写入系统凭据库，连接配置只保存 `credential_ref`；
「测试连接」显示延迟与服务端版本，「保存并连接」建立会话并用 `ListSessions` 刷新会话列表。
错误按 `sqarvo-error-code` 分类展示（认证失败 / 凭据不存在 / 凭据库不可用 / 驱动缺失 /
超时），不合并成一句「连接失败」。

**SQL 执行已接入**（#28）：连接数据库后主区域是 SQL 工作区——多行等宽纯文本编辑器、
`Ctrl+Enter` 执行、`Escape` 取消、`GtkColumnView` 虚拟化结果表格、`FetchBatch` 分批与
接近末尾自动预取、取消/断连状态与游标释放。单元格按 `ColumnValue.kind` 分别显示
（NULL、空串、二进制、Decimal、DATE/TIME/DATETIME/TIMESTAMP、截断标记、不支持类型）。

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
| `src/dialog.rs` | GNOME HIG 连接对话框（`AdwDialog` + `AdwPreferencesPage`） |
| `src/backend/client.rs` | tonic Channel（UDS / TCP 自定义连接器）、token 拦截器、`SystemService` 调用 |
| `src/backend/supervisor.rs` | 后端可执行文件查找、启动、等待就绪、退出清理 |
| `src/backend/worker.rs` | 后台 tokio 线程、命令/事件通道、握手与健康探测循环 |
| `src/backend/error.rs` | 可直接展示的错误文案（绝不含 token） |
| `src/backend/query.rs` | 查询域：视图模型、单元格格式化（NULL/空串/二进制/Decimal/日期时间/截断）、失败分类；不依赖 GTK |
| `src/results.rs` | 结果区：自定义 `GListModel`（`RowStore`）+ `GtkColumnView` 虚拟化、预取触发、状态栏 |
| `src/sql_page.rs` | SQL 工作区：编辑器、执行/取消按钮与快捷键、页面状态机、QA 滚动与取消钩子 |
| `src/proto.rs` | 由 `proto/` 生成、位于 `OUT_DIR` 的客户端代码（勿手改） |

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

1. 对话框收集驱动（固定 MariaDB）、主机、端口、数据库、用户名、TLS 模式与密码；
2. 「保存并连接」先调 `CredentialService.StoreCredential`，后端把密码写进系统凭据库
   （Linux Secret Service）并返回 `credential_ref`；
3. 随后 `ConnectionService.TestConnection`（拿服务端版本与延迟）→ `Connect` →
   `ListSessions` 刷新会话页；
4. 非敏感配置写到 `$XDG_CONFIG_HOME/sqarvo/Sqarvo/connections.json`（目录 0700、
   文件 0600），字段固定为
   `name / driver / host / port / database / username / tls_mode / credential_ref`；
   **没有** password、secret 或 token 字段；
5. 系统凭据库不可用（无会话总线、已锁定、用户拒绝）时如实报 `credential.unavailable`，
   绝不把密码退回配置文件或环境变量。

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

- 每批 500 行（服务端单批上限 5000）；**Execute 之后不会一次性拉全量**：
  10 万行查询在不滚动时只加载首批 500 行，状态行显示「总数未知（服务端分批返回）」；
- 滚动位置距末尾不足 **2 个视口**时触发一次预取（`should_prefetch`，纯函数，有单元测试），
  同一时刻最多一条 `FetchBatch` 在途；
- 用户停在末尾时会像无限滚动一样继续按批拉取（每批 500 行）——这是为了「看到末尾就必须
  加载到末尾」；不存在「Execute 即拉全量」的路径。

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

`SELECT seq AS n, CONCAT('row-', seq) AS label FROM seq_1_to_100000`，每批 500 行：

| 时点 | `VmRSS` |
|---|---|
| 首批加载后（t≈1.0s，500 行） | 156.0 MB |
| 滚动到第 50000 行后（t≈2.35s，已加载 50500 行） | 171.6 MB |
| 滚动到末尾后（t≈3.35s，100000 行） | 189.9 MB |
| 加载完成后静置 10s（4s→13s） | 192.3 MB（稳定） |

- 进程刚启动约 4 MB；GTK + libadwaita + 连接对话框稳定后约 150 MB（debug 构建）；
- 行数据本身约 **0.37 KB/行**（两个字符串列 + 每格 tooltip），这是「为已加载页付内存」，
  与滚动次数无关；
- 另一轮测试在数据加载完成后反复滚动 `末尾 → 顶部 → 5 万 → 顶部 → 5 万 → 末尾`
  （6 次，10 秒内），RSS 恒定在 **191.7 MB**（4s→13s 无一次变化），最大波动来自截图渲染（约 208 MB）：
  **滑动本身不分配内存**，虚拟化只维持可见行的 widget；
- 局限：协议没有随机定位（`page_token` 只能顺序推进），要看到第 10 万行必须把前 10 万行
  都加载进来，所以「加载到末尾」时 RSS 必然随已加载行数增长；这不是滚动泄漏，而是数据本身。
  若未来需要「跳到末尾不占内存」，需要服务端提供可寻址分页或视窗查询。

## 安全约定

- 端点文件里的 token 只保存在 `BackendClient` 内部；`Debug` 实现与所有日志/错误文案
  只输出 `<redacted>` 或 token 长度；
- 数据库密码只出现在 `ConnectionDraft`（`Debug` 为 `<redacted>`）到
  `CredentialService.StoreCredential` 的这一次调用里；UI 不保存、不回显已保存密码，
  日志与错误文案不含密码；
- 不引入 WebKitGTK 或任何 Web 视图。

## 已知限制

- **语法高亮、多标签工作区、查询历史尚未实现**（编辑器是纯文本 `GtkTextView`）；
- 事务未接线：`auto_commit=true` 固定提交，界面上没有 BEGIN/COMMIT/ROLLBACK；
- v0.1 只有一条语句的结果集：`statements` 全部如实列出，但表格只展示 `statement_index=0`；
  多语句/单语句限制由服务端决定，前端不伪造；
- 取消一个**尚未返回查询 ID** 的 `Execute` 无法通知服务端（见「取消语义」）；
- 大字段只显示服务端返回的前缀（`truncated=true`），「查看完整值」的延迟加载未实现；
- 连接对话框只支持内置 MariaDB 驱动，驱动行不可切换；
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
- 所有 `[query]` / `[qa]` 日志带 `t=+Xms`（进程启动起算），可与 `/proc/<pid>/status`
  的 `VmRSS` 采样按时间对齐；
- 不存在绕过 UI 直接调 gRPC 的捷径。

### 连接对话框的无人值守验证（`--qa-profile` / `--qa-password-env`）

```bash
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
```

- 后端握手完成后自动打开连接对话框，把档案里的值填进**与用户输入相同的控件**
  （密码从环境变量取，命令行里不出现明文），再触发与「保存并连接」按钮**相同的回调**；
- 不存在绕过对话框直接调 gRPC 的捷径：自动化走的就是用户会走的那条路径；
- `--screenshot` 拍的是**当前最上层**界面：对话框还开着（例如认证失败）就拍对话框，
  否则拍主窗口；拍完会强制关闭对话框再走正常退出路径；
- 档案文件缺少 `host` / `database` / `username`、`driver` 不是 `mariadb`、或密码环境变量
  为空时会直接报错/保持密码行为空，不会静默用错参数冒充通过。
