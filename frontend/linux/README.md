# Linux 前端（GTK4 + libadwaita）

## 状态

> 行为口径（动作、可用性、结果文案、快捷键、状态转换）以
> [`docs/ui-specification/ui-behavior-contract.md`](../../docs/ui-specification/ui-behavior-contract.md)
> 为准；本节只记实现要点，不重复维护第二份行为说明。

**已接入后端 gRPC**：启动后按下面「启动顺序」完成握手，窗口在
Loading / Connected / Error / Disconnected 四态之间切换。握手成功后**直接落地工作区**：
侧栏是与连接态**同一个对象树**（`My Connections` + 已保存连接，全部处于未连接态；
对象树搜索框在窗口左下角的**状态栏左边沟**，与树同一列宽），主区域是固定 `Objects` 标签的空态
（「从左侧选择一个连接开始」），状态栏显示「未连接」、驱动数与浏览会话数；
后端版本 / 协议 / PID / 端点文件 / 驱动清单在「帮助 → 关于 Squarvo」里
（不再常驻主区域，见 #139）。

**连接对话框已接入**（#27 / #67）：主工具条左组「连接」（或「文件 → 新建连接」/ `Ctrl+N`）
打开连接对话框，**驱动下拉由
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

**对象树已接入**（#86）：连接后左侧是可展开的对象树（库 → 表 / 视图 → 表下的
`Fields` / `Indexes` / `Foreign Keys` / `Triggers` 四组，视图仍直接列出列；
`GtkTreeListModel` 逐层懒加载、每页 500、「加载更多」翻页），左下角搜索框（状态栏左边沟，
v0.2.4 从树顶部搬来）按名称包含搜索，并在树里逐层展开定位。v0.2.3.2 起单击只选中（信息面板与 `Objects` 页跟着换内容），
双击 / 回车才激活（表 / 视图 = 打开数据标签，连接 = 按保存的凭据连接，库 / 分组 = 展开收起）；
表展开后先出四组，展开某组才发那一次 `ListNodes(filter.kinds)`，`Foreign Keys` 没有驱动
实现、展开只显示「驱动未提供外键列表（capability.unsupported）」。DDL 在信息面板的
`[DDL]` 页查看：正文与「表信息」页同一口径（10px、不加粗），长语句横向滚动；只有
`exact = false`（客户端拼装）时正文上方才出现一行琥珀警示。v0.2.3.3 起不再显示来源
徽标、来源说明行与「复制」按钮，复制用选中 + `Ctrl+C`；
侧栏平铺表列表、页脚「DDL」按钮与右键「查看 DDL」在 v0.2.3.2 一并删除。
刷新入口在**右键菜单**：连接节点「刷新连接」（重读该连接已经建立的目录层与已展开的层），
库 / schema 节点「刷新库」（重读已展开的 `Tables` / `Views` 等分组；分组都收起时只清缓存，
展开时自然重拉）；侧栏底部不再有刷新按钮，也没有分割线——侧栏就是对象树本体。
菜单「视图 → 刷新」/ `F5` 的目标随当前标签走：当前标签是表数据页时刷新该表
（与「表 → 刷新当前表」「状态栏 `⟳`」同一条入口：保持排序 / 筛选、回到第 1 页，
有未提交更改时被脏行守卫拒绝），查询标签 / `Objects` 页 / 启动态才重建整棵对象树。

**表数据页已接入**（#48）：双击树里的表（或回车）进入数据标签——`TableDataService.BrowseTable` 按主键升序 + offset 分页（每页
500 行，翻页由分页器驱动；滚动到底不再自动取下一页，见 ADR 0022）、`GtkColumnView` 虚拟化网格、单元格编辑（文本 → 强类型值，
NULL 与空串严格区分）、脏行标记与批量 `ApplyRowChanges` 提交、逐行错误展示、
> 100 行/含删除的风险确认、事务按钮互斥（开始 ⇄ 提交/回滚，状态变化在工具条状态行写一次）与
开始/提交/回滚按钮。无主键表只读并在横幅里说明「无稳定排序 / 无法稳定定位行」；
断连时丢弃未提交变更并说明后端已强制回滚事务。**提交中可取消**（#89）：「提交更改」在
批次执行期间变成「取消提交」，前端自己生成批次 `query_id` 并调 `CancelRowChanges`，
取消后整批回滚、脏行保留可重试（状态行写明「已执行 N 条随后整批回滚」）；在途登记在
成功/失败/取消/断连后都会清理。

**六区域外壳已就位**（#113）：`AdwHeaderBar`（标题 / 副标题 / 主菜单）→ 44 高主工具条
（连接 / 新建查询 │ 表 / 视图 / 函数 / 查询：点一下切到 `Objects` 页；`Tables` / `Views` 列出该种类，
`Functions` / `Queries` 只显示「尚未实现」说明）→ `GtkPaned`
（侧栏 237，可拖 180–420 | 工作区 | 信息面板 280（v0.2.3.2 起常驻：可拖 240–520，没有折叠按钮与
`Ctrl+I`）→ 48 高两行状态栏（左边沟放对象树搜索框与搜索状态）。
布局尺寸在一个纯函数模块里（`src/layout.rs`，有 1200×800 与 960×640 两档单测），紧凑密度
（行高 24/32、字号 12/13/11）由 `src/theme.rs` 的全局 CSS 提供。主界面不再常驻
「端点文件：…」，该信息移到「帮助 → 关于 Squarvo」（#139）。标签工作区（#116）、
信息面板内容（#119）、多连接（#114）与页码分页（#118）已在 v0.2.3 落地，见下文各节。

**语法高亮尚未实现**：编辑器是纯文本，编辑器上方如实标注「当前为纯文本编辑，语法高亮
将在后续版本提供」，不用配色暗示已有高亮。

## 依赖

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev build-essential pkg-config
```

`build.rs` 还会调用 `glib-compile-resources`（`libglib2.0-dev`，装 GTK 开发包时已一并装上）
把 `resources/icons/*.svg` 编成 `.gresource` 嵌进二进制；缺这个工具时构建会直接报出可读原因。

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

CI 也跑这三项：`cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、
`cargo test`（`.github/workflows/ci.yml` 的 `前端（Linux）` 作业）。该作业在 ubuntu-26.04 上
运行——24.04 的 `libgtk-4-dev` 只有 4.14.2，低于 crate 要求的 GTK 4.16（下面「依赖」一节），
构建期就会失败。测试不需要显示：本地 `env -u DISPLAY -u WAYLAND_DISPLAY cargo test` 同样 315 项全过
（#139 之后；此前基线 311）。

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

## 文档索引

| 文档 | 管什么 |
|---|---|
| [`docs/ui-specification/ui-behavior-contract.md`](../../docs/ui-specification/ui-behavior-contract.md) | **行为口径以 `ui-behavior-contract.md` 为准**：动作、可用性、文案、快捷键、状态转换（依据本目录的代码，改行为须与代码同一个 PR 成对更新） |
| [`docs/ui-specification/workspace-model.md`](../../docs/ui-specification/workspace-model.md) | 版式：区域、尺寸、布局算术（行为冲突不看这份，看契约） |
| [`docs/ui-specification/design-principles.md`](../../docs/ui-specification/design-principles.md) | 设计原则与视觉取向 |

## 模块结构

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 应用入口与动作（`Ctrl+Q` 退出：有窗口时先 `window.close()`，与关闭按钮同一条脏行守卫） |
| `src/ui.rs` | 窗口外壳（区域 01：`AdwHeaderBar` + 标题规则 + 主菜单与窗口动作）、四态状态页、启动态落地与「关于 Squarvo」诊断、事件泵与关闭流程 |
| `src/shell.rs` | 区域 02–06 的外壳：主工具条、`GtkPaned` 侧栏 / 工作区、信息面板、两行状态栏；模块文档给出六个区域的槽位表 |
| `src/startup.rs` | 启动落地工作区（#139）：未连接时复用连接态的对象树（搜索 + `My Connections`）、固定 `Objects` 空态、状态栏「未连接 · 驱动 N · 浏览会话 M」 |
| `src/layout.rs` | 布局算术（§1 / §2.1）的常量与纯函数（`content_height` / `grid_width` / `grid_height` / `status_y` / 拖拽钳制），含 1200×800 与 960×640 单测 |
| `src/theme.rs` | 紧凑密度 CSS（全局 `CssProvider`，行高 24/32、字号 12/13/11，最小点击区 24×24）；表格右边界与**下边界**（`row:last-child`）、图标按钮命中区 |
| `src/icons.rs` + `resources/icons/` | 图标标准与自有符号图标：唯一尺寸 16×16（以状态栏 `+` 为基准）+ 固定命中区 28×24，`--qa-layout-audit` 断言；`resources/icons/*.svg` 由 `build.rs` 编成 GResource（漏斗等 Adwaita 没有的形状） |
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
| `src/table_data.rs` | 表编辑纯逻辑：文本 → 强类型值、NULL/空串、全文本草稿与批次构造、风险判定、只读文案、事务状态机（可单测） |
| `src/data_page.rs` | 数据页：虚拟化网格（`DataRowStore`）、单元格编辑弹窗、提交/回滚与风险确认、事务控制行（开始 / 提交 / 回滚互斥显示）、逐行错误列表 |
| `src/backend/metadata.rs` | 元数据域（#86）：节点/种类映射、请求构造、DDL 来源与错误分类（纯逻辑，可单测）+ 独立元数据线程与主线程请求路由（`MetadataHub`） |
| `src/tree.rs` | 对象树：`GtkTreeListModel` 懒加载、自定义展开箭头、分页「加载更多」、搜索防抖（250 ms）与跳转、树的健康状态 |
| `src/ddl.rs` | DDL 面板：`GetDdl` 正文（与表信息同字号、不折行、横向滚动）、拼装警示、四态文案 |
| `src/proto.rs` | 由 `proto/` 生成、位于 `OUT_DIR` 的客户端代码（勿手改） |
| `scripts/bench.sh` | 可复现基准：冷启动、空闲内存、10 万行、200 列宽表、不滚动背压证据 |
| `scripts/v023-acceptance.sh` | v0.2.3 验收链（#121）：SQLite + MariaDB 两库跑完整流程，配置隔离、逐项断言、仓库外取证 |
| `scripts/qa-139-startup.sh` | #139 启动落地工作区验收：有/无已保存连接、`Objects` 空态、「关于 Squarvo」诊断，自渲染截图落仓库外 |

UI 不直接接触 socket、进程或端点文件，只与 `backend::worker` 收发消息；后续的连接
对话框（#27）与 SQL 执行（#28）应复用 `BackendClient`（`channel()` 返回可克隆的
Channel）和同一套命令/事件通道。

## 键盘快捷键

口径细节（前置条件、取消语义）以
[`ui-behavior-contract.md`](../../docs/ui-specification/ui-behavior-contract.md) §8.1 为准。
默认修饰键按平台走 `<primary>`（Linux 是 `Ctrl`）。

| 快捷键 | 动作 | 备注 |
|---|---|---|
| `Ctrl+N` | 新建连接 | 后端未握手时忽略 |
| `Ctrl+T` | 新建查询标签 | 与标签条 `＋`、工具条「新建查询」同一路径 |
| `Ctrl+W` | 关闭当前标签 | 有脏行 / 未提交事务时进入三选（非 `Objects`） |
| `Ctrl+Shift+W` | 关闭窗口 | 与标题栏关闭同一条清理路径 |
| `Ctrl+Q` | 退出应用 | **先关主窗口**：与 `Ctrl+Shift+W` 同一条 `close_request`，未提交更改会先弹「有未提交的更改」；窗口关完由 GApplication 自然退出，只有没有窗口时才直接 `app.quit` |
| `Ctrl+Tab` | 下一个标签 | 最后一个再往后回绕到第一个（`Objects` 也在循环里） |
| `Ctrl+Shift+Tab` | 上一个标签 | 第一个再往前回绕到最后一个 |
| `Alt+1`…`Alt+9` | 直选第 N 个标签 | 1 起（`Objects` 算第 1 个）；超出标签数什么都不做，只记 `[tabs] Alt+3：只有 2 个标签，忽略` |
| `Ctrl+F` | 聚焦对象搜索 | 焦点交给窗口左下角的对象树搜索框（状态栏左边沟） |
| `F5` | 刷新当前表 / 对象树 | 表标签刷当前表，否则刷整棵树 |
| `Ctrl+Shift+E` | 导出当前标签 | CSV |
| `Ctrl+Enter` / `Esc` | 执行 / 取消查询 | 只在 SQL 编辑器内生效 |

`Ctrl+Tab` / `Ctrl+Shift+Tab` / `Alt+1..9` 是应用的窗口动作（`win.tab-next` / `win.tab-prev` /
`win.tab-1..9`），`AdwTabView` 自带的同名内置快捷键已在 `TabSet::new` 里摘掉——否则谁先响应
取决于焦点，回绕与「超出忽略」的语义都不再由应用决定。

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

- 关闭窗口（标题栏、`Ctrl+Shift+W`）与退出应用（`Ctrl+Q`、会话结束）都先过**脏行守卫**：
  `Ctrl+Q` 不是直接 `app.quit`，而是先 `window.close()` 主窗口，因此与点关闭按钮落到同一条
  `close_request`；任一表数据标签有未提交的更改（脏行 / 标记删除 / 未提交的新增行）时不关窗，
  弹「有未提交的更改」确认框（逐表列出，按钮 取消 / 放弃更改并退出）；选「取消」什么都不做，
  选「放弃更改并退出」才继续清理。只有没有窗口时 `app.quit` 才直接退出（没有守卫可走）；
- 关窗与退出走同一条清理路径：先让 worker 停止，收到 `ShutdownComplete` 后才销毁窗口，
  最后一个窗口关掉后 GApplication 自然退出；
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
（与后端同一条规则）。会话显示名按驱动派生：`用户@主机/库`、无默认库时 `用户@主机`，
或 `文件名（SQLite）`；标题栏与会话行都能看出连的是哪种数据库。

「数据库」在服务器型驱动里是**可选**字段（ADR 0024 §3）：占位文案说明「留空则连接后浏览
所有库」，留空不会补成 `information_schema` 或任何默认库；连上后树根层直接是库列表，
从哪个库打开表由节点的 `ObjectRef.catalog` 决定，没有「当前库」这一层。

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
| 后端失效 | 弹出「重启 Sqarvo」对话框（附「查看日志」，日志在 `$XDG_STATE_HOME/sqarvo/desktop.log`）；v0.2.3.2 删除了三处「断开后端」按钮 |

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

### 性能实测（debug 与 release）

本机：AMD Ryzen 7 8745H w/ Radeon 780M Graphics / 16 核 / 24.8 GiB，GTK 4.22.4，
libadwaita 1.9.1，内核 `Linux 7.0.0-38-generic x86_64`；MariaDB 测试库，500 行/批，
结果集上限保持默认（行数 500000 / 估算 512.0 MiB）。前端是单实例 `GtkApplication`，
同一会话内只能同时跑一条基准命令（脚本自己 `pkill`、清 runtime 目录）。

**debug 构建（2026-11-04）**——用 `scripts/bench.sh` 一条命令复现（脚本失败时非 0 退出
并指明步骤）：

```bash
cd frontend/linux
cargo build
cd ../.. && cargo build -p sqarvo-backend-service   # 后端二进制
cd frontend/linux && ./scripts/bench.sh --out /tmp/bench.md
```

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

**release 构建（2026-11-06，同机同脚本，连续 3 轮取第 3 轮）**：

```bash
cd backend && cargo build --release -p sqarvo-backend-service
cd frontend/linux && cargo build --release
cd frontend/linux
SQARVO_BENCH_BIN=$PWD/target/release/sqarvo-desktop \
SQARVO_BENCH_BACKEND=$PWD/../../backend/target/release/sqarvo-backend \
  ./scripts/bench.sh --out /tmp/bench105-release.md
```

`SQARVO_BENCH_BACKEND` 会被 `bench.sh` 作为 `SQARVO_BACKEND_BIN` 传给前端（不传时前端只按候选
路径找 debug 后端，release 前端会因「找不到后端可执行文件」握手失败）。

| 场景 | 指标 | release | debug（对照） |
|---|---|---|---|
| 冷启动（3 次） | 进程入口→Connected 中位数 | **235 ms**（235 / 232 / 236） | 240 ms（243 / 240 / 234） |
| 空闲内存 | 连接后静置 5s VmRSS / VmHWM | **141.6 MB** / 141.6 MB | 149.3 MB / 149.3 MB |
| 10 万行 | 首批到达（500 行） | **945 ms** | 953 ms |
| 10 万行 | 全量加载（100000 行，199 批） | **2095 ms** | 2808 ms |
| 10 万行 | 全量加载后 VmRSS | **184.6 MB** | 191.6 MB |
| 10 万行（不滚动 3s） | 已加载行数 / 后续批次 | **500 行 / 0 批** | 500 行 / 0 批 |
| 200 列 × 2000 行 | 首批 / 全量加载 | 987 ms / **2927 ms** | 1137 ms / 3554 ms |
| 200 列 × 2000 行 | 加载后 VmRSS | **595.6 MB** | 603.4 MB |

release 下「不滚动」的预取暂停日志（背压证据）：

```text
[query] t=+982ms 暂停预取：未滚动到新区域所以不预取（已加载 500 行，距末尾 17241px，视口 259px）
```

debug 与 release 的差异来自构建类型：差别集中在**值转换与批次处理**这一段（10 万行全量
−25%，200 列全量 −18%、首批 −13%），而冷启动（235 vs 240 ms）与首批到达（945 vs 953 ms）
在噪声内，内存只降 5–8 MB——说明首屏耗时与这批内存不在 debug 未优化代码上。完整表格、
三轮复跑与逐条结论见 [`docs/releases/v0.2.2.md`](../../docs/releases/v0.2.2.md#性能实测release-构建)。

结论与未达标项（如实记录）：

- 冷启动与 10 万行场景达到 v0.2 的「可用」标准：首屏 < 1 s，10 万行 3 s 内读完，
  RSS 191.6 MB（debug）；release 下同样达标且更快（235 ms / 2095 ms / 184.6 MB），见上表；
- **200 列宽表内存明显偏高**：2000 行 × 200 列占 603.4 MB，而同样 10 万单元格的
  20 列 × 5000 行只有 208.6 MB、100 列 × 1000 行为 376.5 MB（对照实验，同一轮测量）。
  内存随**列数**上升（约 2 MB/列量级），与行数关系很小（200 列 500 行 563.5 MB →
  2000 行 603.4 MB，仅 +40 MB）。已排除「按行增长」的解释，但**没有定位到具体分配点**
  （怀疑是 `GtkColumnView` 每列的单元格/管理器开销），因此不声称已优化；release 构建下
  仍是 595.6 MB（−1.3%，在轮次波动内），说明这笔开销与构建类型无关，问题继续按未达标记录；
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

## 对象树、搜索与 DDL 面板（#86）

连接数据库后，左侧是对象树：第一层是**目录层**（`ListNodes(parent.kind = CATALOG)`，
列出当前账号可见的库），展开库得到 `Tables` / `Views` 分组，再展开表先得到
`Fields` / `Indexes` / `Foreign Keys` / `Triggers` 四组（视图直接列列）。
驱动没有目录层（返回 `capability.unsupported`）时自动回退到「当前库对象」根层，
两者都不可用时只在侧栏状态行说明原因（v0.2.3.2 删掉了平铺列表回退）。

- **逐层懒加载**：只有展开才发 `ListNodes`。`GtkTreeExpander` 在 bind 时会向
  `GtkTreeListRow` 要子模型（用来决定有没有箭头），等于为每个可见行发一次请求，
  因此这里自己画箭头（同时手动按 `depth` 缩进）；
- **顺序由服务端决定**（先种类、再名字），客户端不重排——重排会让界面顺序与分页
  token 的语义不一致；
- **分页**：每页 500，`next_page_token` 非空时在末尾显示「加载更多…（已加载 N 个）」，
  点它继续；`truncated` 只在本页被上限截断**且没有下一页**时才提示「有数据拿不到」；
- **搜索**：输入防抖 250 ms，带 `filter.name.contains` 请求根层；关键词原样下推，
  客户端不拼通配符、不做二次过滤（大小写敏感性跟随数据库）。空结果与
  `metadata.permission_denied` 是两句不同的话（后者给出授权建议）；点结果在树里
  逐层展开并选中，失败时在状态行说明「不在当前库的目录层下」；
- **DDL 面板**：只有正文与状态文案。`exact = false`（客户端拼装）时正文上方多一行
  琥珀警示，并给出 `source_detail` 与 `generator_version`（拼装缺了什么、哪版拼的）；
  原文不写来源徽标与说明行（v0.2.3.3 删掉了「原文 / 复制」那一条，它夹在页切换与正文
  之间只是噪音）。Loading / Empty / Error / 不支持各有文案，
  `metadata.object_kind_unsupported` 与 `capability.unsupported` 分开显示；
  **不折行**：DDL 是代码，窄面板里折行会把一条语句断成两条，改为横向滚动并让滚动条
  常驻（`overlay-scrolling = false`）；字号 / 字重与「表信息」页同一个类
  （`.sqarvo-panel-text`，`layout::FONT_PANEL` = 10px / 400）：面板是次要信息，
  视觉重心在数据网格，正文比网格小、也不加粗；分节靠间距与颜色分层，复制用
  选中 + `Ctrl+C`；
- **线程**：树的每个展开、搜索、`GetDdl` 都发到独立的元数据线程（`MetadataHub` 按请求
  ID 路由回主线程），主线程只更新控件。

## 数据页（#48）

连接数据库后，主区域左侧是对象树（`ListNodes` 根节点，失败时用分类文案展示，不是空列表；
v0.2.3.2 删掉了「表与视图」平铺列表切换）；双击树里的表 / 视图（或回车）进入右侧数据标签。
右上「SQL 工作区」切回编辑器，两个页面共用同一个数据库会话。

### 浏览与翻页

- `TableDataService.BrowseTable(object, page_size=500, offset, sort, filter)`：**offset 分页**，
  默认按主键升序；排序 / 筛选只从筛选与排序面板下推，翻页由分页器（首页 / 上页 / 下页 /
  末页，页码只读）驱动，同一时刻最多一页在途；
- 列头显示**原始类型名**（`varchar(64)`、`decimal(12,3)`…），来自 `ColumnMeta`；
- 行号跨页连续（行号 = offset + 行下标，不是主键）；`has_more=false` 后不再请求；
- 空表显示「空表」/「本页没有数据」，与错误/只读严格区分；
- UI 不再为浏览签发 `page_token`：不透明 cursor 只保留 v0.2.2 的兼容路径（ADR 0022）。

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

### 事务控制行

事务按钮在行变更动作之后（工具条上不再有「新增」按钮：插入行走状态栏行操作组的 `+` 与
「表」菜单）：没有事务时只显示「开始事务」，有事务（含失败态）时同一位置只显示「提交事务 /
回滚事务」。事务状态**不常驻**：只有按钮互斥，每次状态变化在工具条状态行写一次「事务状态：…」，
状态栏不写事务。原先单独占据一行的「表名 · 排序」提示已删除。

| 状态 | 含义 | 允许的操作 |
|---|---|---|
| 无事务 | 行变更按批次立即生效 | 开始事务 |
| 事务中 / 事务中（只读） | 显式事务；只读事务禁止编辑 | 提交事务 / 回滚事务（只读态可编辑性另受限制） |
| 事务失败（只能回滚） | 服务端 `transaction.failed` | 仅回滚 |
| 已断开 | 后端/会话失效，后端已强制回滚 | 无（后端失效给「重启 Sqarvo」对话框） |

- 「只读」开关在「开始事务」的隔离级别卡片里（v0.2.3.2 起不再常驻工具条）；只读事务下
  编辑与删除会被拒绝并说明原因；
- 有未提交更改时「提交事务」不可用（先提交或放弃更改），避免误以为更改已随事务落库；
- 事务结束（提交/回滚）后自动重新浏览当前表，显示数据库真实状态。

### 列信息

数据网格表头的**第二行（原始类型）**整格可点（行号列不可点），点击把右侧信息面板切到该列
的列页（与树上列节点同一个页面，不重发 RPC）。页内 Overlay 浮层与「顶栏列信息按钮」在列信息
迁入信息面板时删除。字段口径：列名 / 原始类型 / 可空 / 默认值 / 字符集 / 排序规则，与
`docs/api/grpc-conventions.md`「列元数据的存在性语义」一致：

| 元数据 | 面板显示 |
|---|---|
| `default_value` 未设置 | 「未知（服务端未提供）」——不是「无默认值」 |
| `COLUMN_DEFAULT_NONE` | 「无默认值」 |
| `COLUMN_DEFAULT_NULL` | 「默认 NULL」（与空串默认值 `''` 区分） |
| `COLUMN_DEFAULT_EXPRESSION` | 服务端原文（如 `'untitled'`、`1.50`、`current_timestamp()`），不求值、不去引号 |
| `character_set` / `collation` 未设置 | 「不适用」（SQLite 整张表都是这一态），不是空字符集 |

协议只有「表达式」这一种带文本的默认值状态：常量默认值（`'untitled'`、`0`）与
`current_timestamp()` 都按原文展示，前端不额外推断「这是常量还是函数」，也不把
原文当成已求值的字面量。文案映射是纯函数（`src/column_info.rs`），不依赖 GTK。

浮层用 `GtkOverlay` 而不是 `GtkPopover`：Popover 是独立 `GdkSurface`，无人值守
截图（`WidgetPaintable` + `GskRenderer`）拍不到，取证要求画面里必须有这块内容。

列头点击的实现口径（这条容易踩错，写清楚）：

- **`GtkColumnViewColumn` 没有自定义表头控件的 API**，只有 `title` 与 `header_menu`；
- **`GtkColumnView:header-factory`（GTK ≥ 4.12）不是列头**：GTK 文档原文是
  「populating the `GtkListHeader` objects used in **section headers**」——它给的是
  按行分组的小节表头（`GtkListHeader` 的 `item` 是**那一行的数据**），设了它会在
  表头下面多出一条空的分组条，跟列头无关；
- 真正的列头控件是 `GtkColumnView` 内部的 `GtkColumnViewRowWidget css=header`，每列一个
  `GtkColumnViewTitle`。界面在**列头行**上挂捕获阶段的 `GtkGestureClick`（不挂在
  `GtkColumnView` 上：那样会把数据行的按下一起认领，抢掉单元格双击编辑），
  按下时用 `pick()` 命中到列头那一格，读该格显示的标题，按建列时记录的
  「标题 → 数据列下标」表反查（`column_info::column_index_for_title`，整串比对，
  不解析标题、不按第几个列头猜）。落点在列头右缘 8px 内视为拉伸手柄，手势判
  `Denied` 让给 GTK 自己的列宽拖拽，不抢 resize；
- 刷新/换表时 `clear_columns` 一并清掉标题表与列对象，旧表的列头点不到。

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
| `--qa-data-replay-cancelled` | 重放「批次被取消（`cancelled=true`）」的结果（走同一个 `show_applied`；注入的是响应，不是真的取消，原因见下） |
| `--qa-data-edit-range <start>:<count>:<column>:<value>` | 批量编辑连续行（每行都走与单元格编辑同一条路径），500 行批次用 |
| `--qa-data-cancel-after-ms <n>` | 提交**真正发出**后 n 毫秒点「取消提交」（真的发 `CancelRowChanges`） |
| `--qa-close-dirty answer=discard\|cancel` | 脏行建好后触发 `app.close-window`（与 `Ctrl+Shift+W` / 标题栏关闭同一条 `close_request`）：`discard` = 点「放弃更改并退出」（退出码 0，日志 `[close] 已放弃未提交更改并退出`）、`cancel` = 点「取消」（窗口保持打开，由 `--qa-exit-after-ms` 收尾，日志 `[close] 取消关窗：保持在工作区`）。答案走确认框自己的 `response` 信号；需配合 `--qa-data-table` 与编辑 / 标记删除步骤（否则建不出脏行，参数会直接报错） |
| `--qa-quit` | 把 `--qa-close-dirty` 那一步的触发动作从 `app.close-window` 换成 `app.quit`（`Ctrl+Q` 同一个动作）：日志先出现 `[app] 退出请求：先关主窗口（脏行守卫照常生效）`，随后仍是同一张确认框。必须与 `--qa-close-dirty` 同用，单独出现直接报错 |
| `--qa-expect-submit <phase\|outcome\|button\|status>=<value>` | 截图时刻断言在途批次（阶段/结束方式逐字相等，按钮与状态行按包含比对） |
| `--qa-expect-data-state <loading\|ready\|empty\|error\|disconnected>` | 截图时刻断言数据页状态，不符则退出码 3 |
| `--qa-expect-txn <auto\|active\|readonly\|failed\|disconnected>` | 截图时刻断言事务状态（状态机文案 + CSS 类 + 按钮可用性；v0.2.3.2 起工具条徽标已删除），不符则退出码 3 |
| `--qa-column-info <column>` | 打开表后**点该列的列头**展开浮层，并高亮该列（点击坐标取列头控件的真实几何中心，走与用户点击同一个列头点击处理） |
| `--qa-expect-column <column>:<field>=<text>` | 截图时刻断言浮层里该列某字段的**控件文本**（可重复；field 取 `name\|type\|nullable\|default\|charset\|collation`），不符则退出码 3 |
| `--qa-data-insert` | 打开表后点「新增行」（走状态栏行操作组 `+` 的那条路径 `RowOp::Add`；工具条上没有这个按钮了），空行进入编辑态 |
| `--qa-data-insert-set <column:value>` | 给新行某列填值（可重复，按列名） |
| `--qa-data-insert-null <column>` | 把新行某列显式设为 NULL（可重复；与「省略」不同） |
| `--qa-expect-row <column=value>` | 重新浏览后必须存在该值的行（不成立时退出码 3） |
| `--qa-isolation-dialog` | 打开「开始事务」的隔离级别选择卡片（截图用，不真的开事务） |
| `--qa-isolation-level <default\|read-uncommitted\|read-committed\|repeatable-read\|serializable>` | 预先选中隔离级别（`--qa-data-begin-txn` 也用它） |
| `--qa-isolation-confirm` | 在卡片里点「开始事务」（真的开事务） |
| `--qa-export <path>` | 数据就绪后导出到该路径（仍走确认对话框，QA 自动点「导出」；#120） |
| `--qa-export-hold` | 停在导出确认对话框（截图用，不自动确认、不写文件） |
| `--qa-export-cancel-after-rows <n>` | 写满 n 行后走取消路径（验证临时文件清理与「不留半截文件」） |
| `--qa-export-raw-values` | 关掉导出确认对话框里的「公式注入防护」开关，写原始值（验证 #137 的开关；默认开启防护） |
| `--qa-startup-workspace` | 截图时刻断言未连接的启动工作区：主区域是 `Objects` 页、空态文案存在、**不含**「后端版本 / 端点文件」（#139） |
| `--qa-open-about` | 握手完成后打开「关于 Squarvo」，截图目标是该对话框（#139） |
| `--qa-expect-about <substring>` | 「关于 Squarvo」里必须出现的文案（可重复，例如 `后端版本`、`SQLite`；#139） |
| `--qa-refresh` | 触发一次与 `F5` / 菜单「视图 → 刷新」相同的 `win.refresh` 动作（不另开刷新路径）：有表标签刷当前表、否则刷对象树，日志写明走了哪条 |

`--qa-expect-*` 必须与 `--screenshot` 一起用（断言发生在截图那一刻，画面先落盘再断言，
断言失败也不会丢证据）。退出码：`0` 通过、`2` 截图失败、`3` 断言失败、`4` 档案里的驱动或字段不被当前后端支持。

截图偶发失败（退出码 2，日志「控件快照为空」）是**本机 Wayland 会话**的问题，不是产品状态：
窗口 `mapped=true` 却拿不到渲染节点，换 `GDK_BACKEND=x11` 复跑即可正常出图（同一份二进制、
同一组步骤）。`qa::save_widget` 已经做两层兜底：先按 40ms 间隔重试 12 次（每次都继续跑主循环，
光 sleep 不会触发重绘），仍拿不到就退到窗口的内容子控件；两次都失败才报错，报错信息里会打印
前几层子控件的尺寸 / `node` / `visible` / `mapped`，便于判断是「还没渲染」还是「窗口没映射」。

#### 导出 CSV（#120）

`EX`（表工具条 / SQL 结果区）、「表 → 导出 CSV」与 `Ctrl+Shift+E` 走同一个入口：
`GtkFileDialog` 选目标 → 确认口径 → 目标已存在时二次确认 → 逐行写临时文件 → 成功后改名。

写出口径（确认对话框里逐条列出）：

- **范围**：查询结果只含**已加载的行**（不重跑 SQL、不拉全量）；表只含**当前页**。
  客户端上限 / 仍在加载 / 查询取消时，对话框与完成提示都写明「不是全部结果」。
- **NULL vs 空串**：空字段 = SQL NULL；空字符串写为 `""`（字节层面可区分）。
- **二进制**：占位 `(BLOB n bytes)`，绝不导出原始字节；截断值附加 `已截断`。
- **Decimal**：原文（`unscaled` + `scale` 拼字面量，不经 f64）；日期时间用 ISO 8601。
- **截断**：确认对话框统计「有 N 个单元格被服务端截断」，文件写已返回的前缀。
- **列**：表数据页导出全部列；`CC` 隐藏列只影响渲染。
- **路径**：拒绝相对路径与 `..`；父目录必须存在；默认不覆盖（已存在需二次确认）。
  临时文件与目标文件权限 0600。

```bash
# 查询结果（SQLite）：目标不存在时直接确认并写出
./target/debug/sqarvo-desktop --qa-profile /tmp/sqlite.json \
  --qa-sql-file /tmp/q.sql --qa-execute --qa-export /tmp/query.csv \
  --screenshot /tmp/export-done.png --screenshot-delay-ms 5000 --qa-exit-after-ms 6500

# 表当前页（MariaDB）
./target/debug/sqarvo-desktop --qa-profile /tmp/mariadb.json --qa-password-env SQARVO_QA_PASSWORD \
  --qa-data-table export_values --qa-export /tmp/page.csv --qa-exit-after-ms 6000

# 取消：写 2 行后走取消路径；外部复核目标不存在、目录里没有 .sqarvo-tmp
./target/debug/sqarvo-desktop … --qa-export /tmp/cancel.csv --qa-export-cancel-after-rows 2

# 10 万行基准（默认 #[ignore]；行按需生成，RSS 增量只反映导出缓冲）
cargo test export_100k_benchmark -- --ignored --nocapture
# [bench] export rows=100000 bytes=7409540 lines=100001 elapsed=0.418s peak_rss=5440KiB delta=824KiB
```

`--qa-export` 在查询结果读完（`has_more=false`）或表当前页打开后触发；分批结果仍在加载时
不会自动导出（用户可以点 EX 导出此刻已加载的前缀）。

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

#### 提交中取消：真实取消（#89）与结果渲染（#77）

前端在提交时**自己生成** `query_id`（`query_<uuid>`，与 `QueryService` 的 ID 同形）随
`ApplyRowChanges` 发出，并在 worker 里登记为在途批次；提交期间「提交更改」按钮变成
「取消提交」，点击调 `CancelRowChanges`。下面这条命令造 500 行 update，提交后 300 ms 点取消：

```bash
# 让一批 500 行真的跑不完（约 1 s）：每次改 note 时在行边界停 2 ms。
# 生产里这种耗时来自慢库；这里用触发器造出同一个现象，前端一行代码都不为它改。
podman exec sqarvo-mariadb-test mariadb -usqarvo_test -psqarvo_test_only sqarvo_test_89 -e '
CREATE TRIGGER items89_slow_before BEFORE UPDATE ON items89 FOR EACH ROW
BEGIN IF OLD.note <> NEW.note THEN SET @sqarvo89_pause = SLEEP(0.002); END IF; END'

pkill -x sqarvo-desktop; pkill -x sqarvo-backend; rm -rf /tmp/sqarvo-89-run
XDG_CONFIG_HOME=/tmp/sqarvo-cfg-89 SQARVO_QA_PASSWORD='***' \
SQARVO_BACKEND_BIN=…/backend/target/debug/sqarvo-backend \
  ./target/debug/sqarvo-desktop \
  --qa-profile /tmp/profile89.json --qa-password-env SQARVO_QA_PASSWORD \
  --qa-data-table items89 --qa-data-edit-range 0:500:note:bulk-edit \
  --qa-auto-confirm-risk --qa-data-commit --qa-data-cancel-after-ms 300 \
  --qa-expect-submit outcome=cancelled --qa-expect-submit status=已取消 \
  --screenshot ~/sqarvo-v022-shots/89-submit-cancel.png --screenshot-delay-ms 7000

# 外部复核：库里 0 行变化（已执行的行随批次回滚）
podman exec sqarvo-mariadb-test mariadb -usqarvo_test -psqarvo_test_only sqarvo_test_89 \
  -e "SELECT COUNT(*) total, SUM(note='orig') orig FROM items89;"
```

- `--qa-data-cancel-after-ms` 从「提交真正发出」的那一刻开始计时（提交前还有风险确认对话框，
  按启动时间算会让取消落在批次结束之后）；
- 取消在**行边界**生效：日志里 `executed_changes` > 0 证明真的跑到一半，而外部 SQL 复核
  仍是 0 行变化（已执行的那些随批次整批回滚）；
- 提交中的界面同样可断言：`--qa-expect-submit phase=submitting --qa-expect-submit button=取消提交`；
- 批次先结束、取消点击才到（`--qa-data-cancel-after-ms 1500`）：按钮已经回到「提交更改」，
  这一下不发 `CancelRowChanges`、不报错，也不覆盖「已提交 N 行」的结果文案
  （被取消且还有脏行时才当作「重试提交」）；
- 真实往返的证据是这两行日志（不是重放）：

```text
[data] t=+4360ms 提交 500 条行变更（事务：自动提交，query_id=query_4bc08170-…）
[data] t=+4660ms 发送 CancelRowChanges（query_id=query_4bc08170-…）
[data] t=+4662ms CancelRowChanges 返回成功（query_id=query_4bc08170-…，往返 1ms）
```

`--qa-data-replay-cancelled`（#77）仍然保留，但用途只剩一个：回归「取消**结果**怎么渲染」
（`cancelled = true`、`results` 为空、`total_rows_affected = 0` 时不能显示成「已提交 0 行」）。
它注入的是响应，不证明取消 RPC 能用；要证明取消到达服务端，看上面那两行 `CancelRowChanges` 日志。

#### 列信息（信息面板列页）的取证（#87）

`--qa-column-info <列>` 点的是数据网格表头的**第二行（原始类型）**，断言的是右侧信息面板的
列页文本；页内 Overlay 浮层已在 v0.2.3.2 迁入信息面板。

```bash
# (a) 四种默认值态同屏：无默认值 / 默认 NULL / 常量原文 / 表达式原文
… --qa-data-table column_defaults --qa-column-info note \
  --qa-expect-column "amount:default=无默认值" \
  --qa-expect-column "note:default=默认 NULL" \
  --qa-expect-column "title:default='untitled'" \
  --qa-expect-column "created:default=current_timestamp()" \
  --screenshot ~/sqarvo-v022-shots/column-info-defaults.png --screenshot-delay-ms 7000

# (b) MariaDB 的列级字符集/排序规则（数值列显示「不适用」）
… --qa-data-table column_defaults --qa-column-info title \
  --qa-expect-column "title:charset=utf8mb4" \
  --qa-expect-column "title:collation=utf8mb4_general_ci" \
  --qa-expect-column "id:collation=不适用" \
  --screenshot ~/sqarvo-v022-shots/column-info-charset.png --screenshot-delay-ms 7000

# (c) SQLite：字符集/排序规则整列「不适用」
… --qa-profile /tmp/sqlite-profile.json --qa-data-table notes --qa-column-info body \
  --qa-expect-column "title:charset=不适用" \
  --qa-expect-column "body:default=默认 NULL" \
  --qa-expect-column "created_at:default=datetime('now')" \
  --screenshot ~/sqarvo-v022-shots/column-info-sqlite.png --screenshot-delay-ms 7000
```

断言读的是浮层里那六个 `GtkLabel` 的文本（不是重新调用一遍文案函数），所以
「文案函数对、界面没接线」也会失败。`--qa-column-info` 依次执行
「点该列列头 → 浮层出现且高亮的是这一列 → 点关闭 → 再点同一列列头 → 重新出现」，
任何一步不成立都会在日志里写明原因（高亮错列会直接报「列头 → 列映射错位」）。
Wayland 会话里无法注入指针事件，所以这里不是 xdotool 式的鼠标点击：取列头控件的
`compute_bounds` 中心坐标，交给用户点击同一个 `header_click_at`（`pick` → 列头格 →
标题 → 数据列下标）。截图里能直接看到列头行、被高亮的那一行和副标题的「已选 `<列名>`」。

不带这些参数的正常启动路径完全不受影响。

#### 插入新行与隔离级别的取证（#88）

```bash
# (a) 空行进入编辑态：默认值列显示数据库默认原文，自增列只说「省略 → 数据库决定」
… --qa-data-table insert_items_88 --qa-data-insert \
  --screenshot ~/sqarvo-v022-shots/insert-draft-mariadb.png --screenshot-delay-ms 3500

# (b) 插入一行（省略 id / created_at，显式写 code、note，显式 NULL 写 amount）→ 提交 → 重新浏览
… --qa-data-table insert_items_88 --qa-data-insert \
  --qa-data-insert-set 'code:qa-new-1' --qa-data-insert-set 'note:新行备注' \
  --qa-data-insert-null amount --qa-data-commit --qa-data-reload \
  --qa-expect-row 'code=qa-new-1' \
  --screenshot ~/sqarvo-v022-shots/insert-row-mariadb.png --screenshot-delay-ms 9000

# (c) 唯一冲突：整批回滚，界面必须说「库里 0 行变化」，草稿行保留供修正
… --qa-data-table insert_items_88 --qa-data-insert --qa-data-insert-set 'code:seed-a' \
  --qa-data-commit --screenshot ~/sqarvo-v022-shots/insert-rollback-mariadb.png --screenshot-delay-ms 5200

# (d) 隔离级别：SQLite 只剩「默认 + 可序列化」，其余三项禁用并写明原因
… --qa-profile /tmp/sqlite-profile.json --qa-data-table items88 --qa-isolation-dialog \
  --screenshot ~/sqarvo-v022-shots/isolation-sqlite.png --screenshot-delay-ms 4000
```

`(b)` 的日志里 `插入结果：语句成功 1 行，净影响 1 行，整批回滚=false` 与 `新行主键：id=3`
是同一次响应的两个字段；`(c)` 里 `整批回滚=true` 时 `inserted_rows` 可能仍大于 0（语句层
成功过），所以界面**只认 `rolled_back`**，`total_rows_affected` 恒为 0 才是「库里没变化」。

隔离级别是否真的到达数据库有两层证据（跑过一次，记录在此）：

```bash
# 外部会话开一个不提交的事务改 id=2，保持 22 秒（容器内 mariadb 客户端，库名按你的测试库替换）
podman exec -i sqarvo-mariadb-test mariadb -usqarvo_test -psqarvo_test_only sqarvo_test_88 -e \
  "START TRANSACTION; UPDATE items88 SET note='dirty-ru' WHERE id=2; DO SLEEP(22);" &
… --qa-data-begin-txn --qa-isolation-level read-uncommitted --qa-data-reload …
# → 重新浏览读到 note=dirty-ru（脏读成立 = 窗口真的在 READ UNCOMMITTED 里）
… --qa-data-begin-txn --qa-isolation-level repeatable-read --qa-data-reload …
# → 同一时刻读到 note=NULL（可重复读看不到未提交值）
```

两次唯一的差别是所选隔离级别，读到的值不同，说明 `BeginTransactionRequest.isolation`
真的被服务端接受并生效（不是前端自说自话）。

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
- 冷启动与内存数值在 debug（2026-11-04）与 release（2026-11-06）两种构建上都有实测，
  见「性能实测」；宽表内存偏高在两种构建下都存在，仍未定位到分配点；
- SQL 工作区的查询仍是 `auto_commit=true`（查询侧不使用显式事务）；数据页的事务按钮
  只作用于表数据的行变更，两者共用同一个数据库会话；
- 数据页已支持插入新行（`InsertRows`，单行一批），但**协议没有「自增」标志位**：
  `ColumnMeta` 不带 `auto_increment`，MariaDB 的自增列读出来与「不可空 + 无默认值」完全一样，
  所以界面只能把这类列默认「省略」并提示原因，无法在 UI 上禁用输入；显式填写自增值仍由
  数据库接受（MariaDB 允许）。要真正禁止需要契约追加列级标志（待定，见 #88）；
- 插入成功后按返回的主键把新行定位到网格末尾，**不回读**省略列的实际值（渲染为「数据库生成」）；
  协议没有「按主键回读单行」的 RPC，要看真实值请点「刷新」；
- 不提供「按任意列连续游标翻页」：排序走筛选与排序面板下推的 `sort` 键（offset 分页），
  cursor 兼容路径仍只按主键升序；
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

`--window-size WIDTHxHEIGHT` 指定初始窗口尺寸（不低于验收下限 960×640；窗口最小尺寸未强制），用于
在 1200×800 与 960×640 两档取外壳截图；**产品默认仍是 1200×800**，不给这个参数就用默认值：

```bash
# v0.2.3 六区域外壳：两档尺寸各一张（连一个临时 SQLite 库，打开一张表）
./target/debug/sqarvo-desktop --qa-profile /tmp/sqlite.json --qa-data-table items \
  --window-size 1200x800 --screenshot ~/sqarvo-v023-shots/shell-1200x800.png
./target/debug/sqarvo-desktop --qa-profile /tmp/sqlite.json --qa-data-table items \
  --window-size 960x640 --screenshot ~/sqarvo-v023-shots/shell-960x640.png
```

启动日志里会打印一行布局自检，便于把公式与实际分配对上：

```text
[shell] 布局 1200x800：工具条 44，状态栏 48（y=752），内容 963x644，表格 652x574，侧栏 237，信息面板 280
```

### README 首图怎么重现

仓库根 `README.md` 的首图是 `docs/assets/screenshots/linux-main-window.png`（1200×800），由本前端
自渲染（不是桌面截图）。自包含：`sqlite3` 造一个 5 张表 + 2 个视图的演示库，不需要 MariaDB 与密码；
`--qa-tree-activate main` 让库行展开出四个分组（产品行为：分组保持收起），
`--qa-tree-select main` 把 `Objects` 页切到该库的表，`--qa-objects-select orders` 选中一行并让
信息面板同步到该表，延迟 9 s 等目录层、列表与 DDL 都就绪后写盘：

```bash
W=$(mktemp -d); mkdir -p "$W/cfg" "$W/run/sqarvo"
sqlite3 "$W/demo.db" <<'SQL'
CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT, created_at TEXT);
CREATE TABLE orders(id INTEGER PRIMARY KEY, user_id INTEGER, total INTEGER, note TEXT);
CREATE TABLE order_items(id INTEGER PRIMARY KEY, order_id INTEGER, sku TEXT);
CREATE TABLE products(id INTEGER PRIMARY KEY, title TEXT, price INTEGER);
CREATE TABLE audit_log(id INTEGER PRIMARY KEY, at TEXT, what TEXT);
CREATE VIEW v_active_users AS SELECT id, name FROM users;
CREATE VIEW v_order_totals AS SELECT user_id, count(*) FROM orders;
SQL
cat > "$W/profile.json" <<JSON
{"driver":"sqlite","path":"$W/demo.db","read_only":false,"name":"sqarvo-demo"}
JSON
env GDK_BACKEND=x11 XDG_CONFIG_HOME="$W/cfg" \
    SQARVO_ENDPOINT_FILE="$W/run/sqarvo/backend-endpoint.json" \
    SQARVO_QA_APP_ID=io.sqarvo.Sqarvo.ReadmeShot \
  ./target/debug/sqarvo-desktop --window-size 1200x800 \
    --qa-profile "$W/profile.json" \
    --qa-tree-activate main --qa-tree-select main --qa-objects-select orders \
    --screenshot ../docs/assets/screenshots/linux-main-window.png \
    --screenshot-delay-ms 9000 --qa-exit-after-ms 11000
```

`GDK_BACKEND=x11` 是必需的：Wayland 会话下窗口 `mapped` 却拿不到渲染节点，截图会报「控件快照为空」。
换成 MariaDB 演示库时改 `profile.json`（`driver` / `host` / `port` / `database` / `username` /
`name`）并加 `--qa-password-env SQARVO_QA_PASSWORD`——密码只走环境变量，命令行里不出现明文。

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

### 对象树、搜索与 DDL 面板的无人值守验证（#86）

```bash
# 展开两层 → 选中表 → 看 DDL，并在截图时刻断言正文与状态
./target/debug/sqarvo-desktop \
  --qa-profile /tmp/profile.json --qa-password-env SQARVO_QA_PASSWORD \
  --qa-tree-expand sqarvo_test_86 --qa-tree-open orders \
  --screenshot ~/sqarvo-v022-shots/tree-open.png --screenshot-delay-ms 6500 \
  --qa-expect-tree health=Ready --qa-expect-tree selected=orders \
  --qa-expect-ddl state=ready '--qa-expect-ddl=contains=CREATE TABLE'

# 搜索 → 点第一条结果 → 在树里逐层展开并选中
./target/debug/sqarvo-desktop \
  --qa-profile /tmp/profile.json --qa-password-env SQARVO_QA_PASSWORD \
  --qa-tree-search ord --qa-tree-click-result \
  --screenshot ~/sqarvo-v022-shots/search-jump.png --screenshot-delay-ms 5500 \
  --qa-expect-tree search=results --qa-expect-tree first=order_items \
  --qa-expect-tree selected=order_items --qa-expect-tree page=tree

# 树形搜索结果（v0.2.4）：连接名 + 表 / 视图在同一次搜索里；结果就是那棵树（祖先自动展开、
# 无关项不出现），v0.2.3.4 起不再有命中高亮
./target/debug/sqarvo-desktop \
  --qa-profile /tmp/sqlite.json \
  --qa-search user --qa-expect-search hits=3 \
  --qa-expect-search row=*/main/Tables/users --qa-expect-search row=*/main/Views/v_users \
  --screenshot /tmp/search-tree.png --screenshot-delay-ms 6000 --qa-exit-after-ms 8000

# 退出筛选：清空搜索框后回到树页，命中高亮不残留
./target/debug/sqarvo-desktop --qa-profile /tmp/sqlite.json \
  --qa-search user --qa-search-clear \
  --qa-expect-search state=idle --qa-expect-search hits=0 \
  --screenshot /tmp/search-cleared.png --screenshot-delay-ms 7000 --qa-exit-after-ms 9000

# v0.2.4：点结果里的表 = 打开数据标签，结果页与搜索框原地保留（不再切回对象树去定位）
./target/debug/sqarvo-desktop --qa-profile /tmp/sqlite.json \
  --qa-search user --qa-search-click-object \
  --qa-expect-search state=results --qa-expect-search hits=3 \
  --screenshot /tmp/search-click-object.png --screenshot-delay-ms 10000 --qa-exit-after-ms 12000

# 大库：展开 5000 对象的库（每页 500），再点一次「加载更多」，日志给出耗时
./target/debug/sqarvo-desktop \
  --qa-profile /tmp/profile.json --qa-password-env SQARVO_QA_PASSWORD \
  --qa-tree-expand sqarvo_bench_86 --qa-tree-load-more \
  --screenshot ~/sqarvo-v022-shots/bench-5000.png --screenshot-delay-ms 6000
```

- `--qa-tree-expand <name>`：按名字展开节点（可重复，按参数顺序；每步都会重试到节点
  出现为止）。展开走 `GtkTreeListModel` 的真实展开路径，**只有展开才发 `ListNodes`**。
  对**库**节点它会把 `Tables` / `Views` / `Functions` / `Queries` 四个分组行一起展开
  （把「用户再点几下」自动化；`Functions` / `Queries` 只出说明行，不发 RPC）；
  产品行为相反：双击库只展开库本身，分组保持收起（v0.2.3.3），要证明这条用
  `--qa-tree-activate <库名>`（走双击那条 `activate` 信号）；
- `--qa-tree-open <name>`：选中并打开表（与双击行同一条回调），进入数据页；
- `--qa-tree-activate <name>`：**走双击 / 回车那条 `GtkListView::activate` 信号**激活节点
  （v0.2.3.2）：表 / 视图应打开数据标签，库 / schema 才展开。与 `--qa-tree-open` 的区别是
  它不直接调打开回调，而是回到「激活」的判定里，用来回归「双击表只展开、不打开」；
- `--qa-tree-ddl` 在 v0.2.3.2 **已移除**（右键「查看 DDL」与侧栏 DDL 按钮一起删掉）：
  旧脚本传这个参数会直接报错，不会安静地什么都不做。DDL 改在信息面板的 `[DDL]` 页看；
- `--qa-search <query>`（v0.2.4）：把关键词填进搜索框，走用户输入同一条
  `changed` → 250 ms 防抖 → 请求路径（**不**跳过防抖）；`--qa-search-clear` 随后清空
  输入框，走用户清空 / `Esc` 的同一条清理路径（用于证明结果行与高亮不残留）；
  `--qa-expect-search <字段=值>`（可重复）在截图时刻断言搜索结果：
  `hits=N`（高亮行数）、`row=连接/库/Tables/表`（必须出现，`*` 匹配单段，可逗号分隔多条）、
  `rows=…`（完整行序，逐项相等）、`highlight=…`（**真正命中的行**按显示顺序逐项相等；
  界面自 v0.2.3.4 起不再画命中高亮，结果行与树里的同一行像素一致，这个字段只剩数据态）、
  `status=…`（状态行包含，如 `2 个连接 · 3 个对象`）、`state=results|empty|denied|failed|idle`；
  `--qa-search-click-object`（v0.2.4）点结果里**第一个可打开的对象行**（表 / 视图），走结果行
  真实的 `row-activated` 信号——与用户单击结果行同一条路径，不是直接调打开回调。日志里会出现
  `[qa] … 点击搜索结果 <路径>` 与 `[tree] 打开表 <路径>（搜索结果点击…）` + `[tabs] 新建表标签`，
  而**没有**「跳转成功」（说明没有切回对象树去逐层定位）；配合 `--qa-expect-search state=results`
  可以断言点完之后结果页与搜索框仍原样保留。必须与 `--qa-search` / `--qa-tree-search` 同用；
- `--qa-tree-search <query>`：把关键词填进搜索框并搜索（与用户输入同一条路径，
  只是跳过键盘；注意 `--qa-expect-tree results=N` 现在数的是**结果页行数**，含连接 /
  库 / 分组祖先行）；`--qa-tree-click-result` 点第一条结果，`--qa-tree-load-more`
  点第一个「加载更多…」占位行；`--qa-tree-disconnect <name>` 断开指定连接的浏览会话
  （与右键菜单「断开连接」同一条路径，包含确认框；排在搜索步骤之前）；
- `--qa-objects-select <name>` / `--qa-objects-activate <name>`（v0.2.3.3）：在固定
  `Objects` 页里按名字选中 / 激活一行。选中走单选模型（与单击同一条路径，会同步信息
  面板），激活发 `GtkListView::activate`（与双击 / 回车同一条路径，会开表数据标签）；
  `--qa-expect-objects selected=<name>` 断言当前高亮的是哪一行。这两步排在
  `--qa-tree-select` 之后，需要先让树选中一个库 / 分组把列表拉出来。
  v0.2.3.3 删除了页内作用域说明行，`--qa-expect-objects scope=` 随之删除：旧脚本传它
  会直接报错，而不是拿一个界面上已经没有的值「通过」；
- `--qa-kind <tables|views|functions|queries>`（v0.2.3.3）：点主工具条的种类入口
  （`emit_clicked`，与用户点击同一条信号）。`tables` / `views` 会选中树里对应的库 /
  `Tables`/`Views` 分组并把 `Objects` 页切到该种类；`functions` / `queries` 打开「尚未实现」的
  如实说明页；
- `--qa-tree-select-group <tables|views>`（v0.2.3.3）：选中树里的种类分组行（与用户点分组
  行同一条路径），用来验证反方向联动——树 → 工具条的种类高亮（日志
  `[toolbar] 高亮种类：…`，只在变化时打）；
- `--qa-expect-tree <field>=<value>`：`health|selected|search|results|first|page|loaded|
  status_contains`；`--qa-expect-ddl <field>=<value>`：`contains|state|visible`。断言读的是
  **控件上真实渲染的文本**（搜索状态行、DDL 缓冲区正文），不是「状态机里应该是」。
  v0.2.3.3 起 DDL 面板不再显示来源徽标 / 来源说明 / 「复制」按钮，`provenance` /
  `detail_contains` / `object_contains` / `copy` 四个字段随之删除——旧脚本传它们会直接
  报错，而不是拿一条没有控件承载的断言「通过」；
- `--opt=value` 与 `--opt value` 两种写法等价（GLib 的选项解析两种都接受，QA 的自解析
  必须一致，否则「参数没生效」会看起来像功能坏了）；
- 并行开发时可以给每个 worktree 一个独立端点，避免互相杀掉对方的后端：
  `SQARVO_ENDPOINT_FILE=/tmp/sqarvo-86-run/sqarvo/backend-endpoint.json`
  （前端会把子进程的 `XDG_RUNTIME_DIR` 指到同一目录，后端就写到那里）。

### 多连接注册表与每连接浏览会话（#114）

树根是 `My Connections`：`connections.json` 里的每条连接一个节点，与「当前是否连上」无关；
未连接的节点点一下就用已保存的凭据建立一条**浏览会话**（对象树 / 搜索 / DDL 都走它，
不执行用户 SQL）。数据页仍绑定「保存并连接」建立的那条会话（一标签一会话在 #116）。

```bash
# 先种一条 SQLite 连接（免凭据），再让 MariaDB 通过对话框成为数据页连接，
# 最后从树里点开 SQLite：两条连接并列，各自一条浏览会话
./target/debug/sqarvo-desktop \
  --qa-profile /tmp/profile.json --qa-password-env SQARVO_QA_PASSWORD \
  --qa-tree-expand "本地 SQLite" --qa-tree-expand main \
  --screenshot ~/sqarvo-v023-shots/two-connections.png --screenshot-delay-ms 9500

# 断开其中一条，另一条仍能搜索/展开（日志里 ListSessions 前后各一次）
… --qa-tree-expand "本地 SQLite" --qa-tree-disconnect "本地 SQLite" \
  --qa-tree-search demo --qa-tree-click-result --screenshot …/disconnect-one.png
```

- 展开一个 catalog 只建立 `Tables` / `Views` 两个**分组行**（0 次 RPC），分组保持收起
  （v0.2.3.3：双击库时注意力在 `Objects` 页，那里已经列出表；树不再替用户展开分组）。
  点开某一组才发那一次的 `ListNodes`（`filter.kinds`），日志里是
  `分组 Tables 拉取第 1 页（filter.kinds=[Table]）`；
- 断开连接只关它自己的浏览会话：`ListSessions` 从 3 条变 2 条（另一条连接的数据会话 +
  浏览会话不动），数据页不受影响；若断开的是当前数据页绑定的连接，确认框与日志都会写明；
- 数据页只浏览当前连接的表：树里另一条连接的表被点开时，数据页横幅给出明确说明
  （每标签会话在 #116 提供），不会用错会话去读同名表。

### 启动落地工作区验收（#139）

```bash
cd frontend/linux
./scripts/qa-139-startup.sh
# 可选：SQARVO_QA139_DIR=/tmp/shots（默认 /home/jack/sqarvo-v023-shots/139-startup-workspace）
#       SQARVO_QA139_NO_BUILD=1（跳过 cargo build）
```

脚本在隔离的 `XDG_CONFIG_HOME` / `SQARVO_ENDPOINT_FILE` 下跑三次，产物全部落在仓库外：

1. 有 2 条已保存连接：截图主窗口，应用内断言「启动态=Objects 空态」且主界面没有诊断字段；
2. `--qa-open-about`：截图目标是「关于 Squarvo」，断言包含后端版本与 `MariaDB` / `SQLite`；
3. 删除 `connections.json`：仍然落到同一个空态（0 条连接），不崩溃、不退回诊断页。

断言读的是控件树里真实渲染的文本（`--qa-expect-about` 也是），不是状态机自述。

启动态的形状与连接态一致（#155 之前启动态外壳的 `Rc<Shell>` 被丢弃，面板从不落位、
`Objects` 也不是固定页；之后启动态侧栏也改为复用同一个对象树）：

- **侧栏是同一个 `tree::TreePanel`**：`My Connections` 与已保存连接节点都与连接态一致，
  搜索框同样挂在状态栏左边沟（位置与连接态相同），只是所有连接显示为「未连接」；不再是
  另一套大行连接列表；
- **信息面板在启动态即常驻**（默认 280，240–520 可拖；**窗口级记忆**，启动态与连接态
  读同一个值，点击连接不再从 280 跳成某条连接的 240），不再折叠、也没有「无面板」那一档；
  空态与选中态共用同一套外壳（标题「信息面板」+ 分隔线），内容区留白、不写说明文字；
- **`Objects` 页与连接态是同一实现**（`tabs::ObjectsPage`）：钉住（`AdwTabPage:pinned`）
  在最左、只显示图标、没有关闭按钮，只是空态文案换成「未连接 / 从左侧选择一个连接开始」；
- 截图的形状差异（侧栏 237、工作区到 x≈919、919–1200 是面板）可以这样重现，两次运行
  都用隔离的 `XDG_CONFIG_HOME` 与端点文件，产物落在仓库外：

```bash
W=/tmp/sqarvo-layout-check
rm -rf $W; mkdir -p $W/config/sqarvo/Sqarvo $W/runA/sqarvo $W/runB/sqarvo
cp ~/.config/sqarvo/Sqarvo/connections.json $W/config/sqarvo/Sqarvo/

# A 启动态（未连接）：断言 Objects 固定页 + 信息面板已落位
env XDG_CONFIG_HOME=$W/config XDG_STATE_HOME=$W/state \
    SQARVO_ENDPOINT_FILE=$W/runA/sqarvo/backend-endpoint.json \
    SQARVO_BACKEND_BIN=…/backend/target/debug/sqarvo-backend \
    SQARVO_QA_APP_ID=io.sqarvo.Sqarvo.QALayoutA \
  ./target/release/sqarvo-desktop --window-size 1200x800 --qa-layout-audit \
    --qa-startup-workspace --screenshot $W/01-startup.png --qa-exit-after-ms 8000

# B 双击一条已保存连接之后：形状必须与 A 相同（竖线仍在 237 / 919）
# 环境变量与 A 完全相同，只把 SQARVO_QA_APP_ID 换成 QALayoutB、端点文件换成 runB 下的同名文件
env XDG_CONFIG_HOME=$W/config XDG_STATE_HOME=$W/state \
    SQARVO_ENDPOINT_FILE=$W/runB/sqarvo/backend-endpoint.json \
    SQARVO_BACKEND_BIN=…/backend/target/debug/sqarvo-backend \
    SQARVO_QA_APP_ID=io.sqarvo.Sqarvo.QALayoutB \
  ./target/release/sqarvo-desktop --window-size 1200x800 --qa-layout-audit \
    --qa-startup-click "演示库 · demo.db（SQLite）" --qa-expect-startup-connect connects=1 \
    --screenshot $W/02-after-connect.png --screenshot-delay-ms 6000 --qa-exit-after-ms 15000
```

`--qa-layout-audit` 不能省：`qa::enabled()` 只认它这一组参数，没有它 `SQARVO_QA_APP_ID`
不生效，新进程会把激活转发给正在运行的同名实例（表现为退出码 0、日志只有一行
「日志文件：…」）。端点文件必须正好是 `<runtime_root>/sqarvo/backend-endpoint.json`。
A 的日志里应当有 `[shell] 布局 1200x800：… 侧栏 237，信息面板 280` 与
「断言通过：…信息面板 280px 可见，Objects pinned」。

### v0.2.3 验收链（#121）

```bash
cd frontend/linux
SQARVO_QA_PASSWORD=… ./scripts/v023-acceptance.sh
# 可选：SQARVO_ACCEPTANCE_DIR=/tmp/shots（默认 /home/jack/sqarvo-v023-shots）
#       SQARVO_ACCEPTANCE_NO_BUILD=1（跳过 cargo build）
#       PYTHON=/path/to/python3（需要能 import pymysql）
```

一条命令跑完 v0.2.3 的完整流程，每一步都断言**控件文本或服务端计数**，失败以非零退出：

```text
展开 catalog（Tables/Views 两组 → 2 次 ListNodes，按分组数计，不预取全部）
→ Ctrl+F 的焦点目标（对象树搜索框）→ 选中表（信息面板 DDL）
→ 开 3 个表标签（ListSessions = 1 条浏览 + 3 条标签）
→ 排序（name asc）→ 筛选（category = keep，1100 行）
→ 第 2 / 3 页（500 / 100 行）→ 信息面板切「表信息」（4 列）
→ 导出当前页 CSV（101 行 = 表头 + 100 行，首行是第 3 页的 1001）
→ 关一个标签（ListSessions = 3）
```

SQLite 与 MariaDB 各跑一遍，最后追加一段**无默认库跨库**用例（#141）：

| 用例 | 连接 | 说明 |
|---|---|---|
| `sqlite` | 临时库 `accept.db` | 3 张表 + 1 个视图，1200 行（1100 `keep` / 100 `drop`） |
| `mariadb` | `sqarvo_test_121` | 同样的表与数据；同一隔离配置里已有 SQLite 连接，树里因此是 **2 条连接**，但只有展开的那条产生浏览会话 |
| `mariadb-nodb` | 不填库名 | 档案里没有 `database`：树根层直接是库列表；在 `sqarvo_test_140a` / `sqarvo_test_140a_b` 各开一张同名表 `probe_141_items`（1200 行，a-/b- 前缀），断言标签名、ListSessions 计数、总数/排序/翻页/导出与会话名退化 |

两个库必须已存在且账号有 ALL 权限（`SQARVO_CROSS_DB_A` / `SQARVO_CROSS_DB_B` 可覆盖）；
脚本在各库建/重建 `probe_141_items`。

**配置隔离**：脚本把 `XDG_CONFIG_HOME` 指向 `mktemp -d` 的目录，所有 `connections.json`
与 `window.json` 都写在里面；结束时断言真实 `~/.config/sqarvo/Sqarvo/connections.json`
条目数与开始时一致（本轮基线 13 条，跑完仍是 13 条）。

**证据**：截图（GTK 自渲染）与日志写在仓库外
`/home/jack/sqarvo-v023-shots/acceptance-<时间戳>/<用例>/`，脚本把每张图的路径与字节数、
每条断言的真假打到 stdout：

| 文件 | 内容 |
|---|---|
| `01-tree.png` | 树展开到 Tables/Views（视图可见）+ 搜索框获得焦点 |
| `02-tabs.png` | 3 个表标签 + 服务端 4 条会话 |
| `03-page3.png` | 第 3 页、1100 行、name 升序、category=keep |
| `04-info.png` | 信息面板「表信息」：4 列、主键、索引/触发器 |
| `05-export.png` | 导出完成提示（第 3 页 100 行、2.4 KiB、CSV 路径） |
| `page3.csv` | 导出的当前页（101 行） |

`mariadb-nodb` 用例另有 `01-tree-catalogs.png`（连接节点 + 库列表）、`02-b-table.png`
（B 库表的标签名 / 第 2 页）、`03-two-catalogs-tabs.png`（两个同名表标签互不影响）、
`04-connection-unspecified.png`（信息面板「默认库：未指定」）与 `b-page2.csv`。

脚本用到的开关（既有语义一个都没改）：

| 开关 | 作用 | 前置条件 |
|---|---|---|
| `--qa-profile <json>` | 把连接档案填进对话框并点「保存并连接」 | 驱动必须是后端 `ListDrivers` 返回的；SQLite 路径必须存在 |
| `--qa-password-env <ENV>` | 从环境变量读密码（命令行无明文） | 服务器型驱动；变量非空 |
| `--qa-tabs-file <path>` | 按行执行标签工作区脚本（见下表） | 只在工作区建立后运行 |
| `SQARVO_QA_APP_ID`（环境变量） | 给验收进程一个独立的 GApplication id，避免把激活转发给用户正在运行的实例（后者会让新进程以 0 退出、日志与截图全空） | 仅启用 QA 参数时生效 |
| `--window-size 1200x800` | 取证窗口尺寸（产品默认也是 1200×800） | `--screenshot`/验收脚本使用 |
| `--qa-exit-after-ms <n>` | 定时走正常清理路径关窗（基准脚本） | 不与 `quit` 脚本步骤同用 |

`--qa-tabs-file` 的步骤（#121 增补的步骤不引入新命令行开关）：

| 步骤 | 作用 | 前置条件 |
|---|---|---|
| `expand <名字>` | 展开连接 / catalog / 分组（走树节点展开） | 节点已加载 |
| `select-node <名字>` | 选中对象节点，信息面板随之切换 | 节点已加载 |
| `select-connection <名字>` | 选中连接节点（信息面板切到「连接」态） | 连接节点已加载 |
| `open-node <库> <表>` | 打开**指定库**里的表（与行激活同一条回调；同名表分属两库时用） | 该库已展开到表 |
| `inspector <ddl\|info>` | 信息面板换页（`show` 仍被接受但无动作；`hide` 在 v0.2.3.2 报错：面板常驻） | 选中的是表/视图才能换页 |
| `open-table <表名>` | 打开表标签（终点与树里双击表相同；按名字解析已加载的树节点） | 该表已在树里加载 |
| `select <n>` / `close <n> <save\|discard\|cancel>` | 切标签 / 关标签（0 是固定 `Objects` 页） | 标签存在 |
| `close-signal <n> <save\|discard\|cancel>` | **走 `close-page` 信号**关第 n 个标签：与点标签中键 / `Ctrl+W` 完全同一条路径（信号 → 确认框 → 回答）。`close` 直接弹确认框、不经过 `AdwTabView` 的关闭请求，所以「脏标签关不掉 / 关完之后再点没反应」这类问题只有 `close-signal` 能复现（v0.2.3.4 回归用例） | 标签存在且非 `Objects` |
| `tab-next` / `tab-prev` | 与 `Ctrl+Tab` / `Ctrl+Shift+Tab` 同一条路径（同一个 `TabSet::step_selection`）：选下一个 / 上一个，到头回绕；日志写「当前标签 N/M：<标题>」 | 有工作区 |
| `select-n <N>` | 与 `Alt+N` 同一条路径（`TabSet::select_position`）：直选第 N 个标签（1 起，含 `Objects`）；N 超过标签数时**不失败**，只记 `[tabs] Alt+3：只有 2 个标签，忽略` | 有工作区 |
| `sort-panel <标签> <列> <asc\|desc>` | 在筛选与排序面板里加一个排序键并应用（表头不可点，排序只有这条路径） | 表标签；驱动声明排序能力 |
| `filter <标签> <列> <算子> <值>` | 应用一条筛选（`eq/ne/lt/le/gt/ge/like/null/notnull`） | 表标签；驱动声明筛选能力 |
| `page <标签> <next\|prev\|first\|last>` | 分页器动作 | 表标签；offset 分页 |
| `export <标签> <路径>` | 导出该标签当前页（确认框自动确认） | 标签有已加载行；父目录存在 |
| `expect-file <路径> <最少字节>` | 等导出写完后断言字节数 | 同上 |
| `screenshot <路径>` | GTK 自渲染截图（仓库外取证） | 窗口已渲染 |
| `focus-search` / `expect-focus search` | 把焦点放到对象树搜索框并断言焦点链 | 树已创建 |
| `mark-list-nodes` / `expect-list-nodes <n>` | 断言自基线以来的 `ListNodes` 次数（= 分组数） | catalog 展开期间 |
| `expect-sessions <n>` | 重新读 `ListSessions` 并断言会话总数 | 后端有活动会话 |
| `expect-browse <标签> <字段>=<值>` | 断言页码 / 页数 / 行数 / 总数 / 位置 / 排序 / 筛选数 / 摘要 | 表标签已就绪 |
| `expect-inspector <字段>=<值>` | 断言面板页 / 子页 / 列数 / 可见性 / 标题 / 字段行（`facts`）/ DDL 状态 | 面板已显示 |
| `expect-tab-title <标签> <子串>` | 断言标签页真实标题（`对象 @ 库`） | 标签存在 |
| `sessions <说明>` | 写一次 `ListSessions` 日志（前后对比证据） | 有注册表 |
| `quit` | 收起模态提示后关窗，走 `connect_shutdown` 清理 | 工作区存在 |

`expect-*` 步骤在重试 40 次（约 8 秒）后仍不成立就以退出码 3 结束；退出码 4 表示脚本无法
解析，2 表示截图失败（与既有 `--qa-*` 同一口径）。

**`Ctrl+F` 的路径**：`focus-search` 不是测试代码自己抓焦点，它激活产品动作
`win.focus-search`（`ui.rs` 里绑到 `<primary>f`），再只读断言焦点链落在对象树搜索框上；
`expect-focus search` 只做那次只读断言。因此这一条证明的是「快捷键那条路径」。

### v0.2.3.2 Objects 联动 / 导出范围验收

```bash
cd frontend/linux
./scripts/v0232-objects-export.sh            # 全量（发布门禁：Objects + 导出 + 树激活 + 960 布局）
SQARVO_ACCEPTANCE_ONLY=objects ./scripts/v0232-objects-export.sh   # 只跑 Objects + 搜索段（A0–A12）
# 可选：SQARVO_ACCEPTANCE_DIR=/tmp/shots（默认 /home/jack/sqarvo-v0232-shots，仍追加时间戳）
#       SQARVO_ACCEPTANCE_NO_BUILD=1（跳过 cargo build）
```

自包含脚本：只用 `sqlite3` 在 mktemp 目录建一个临时库（1 张 30 行的表 + 索引 + 触发器 + 视图），
**不需要 MariaDB、不需要密码**；串行跑 19 个应用实例（只跑 Objects 段时 14 个；各自独立
`SQARVO_QA_APP_ID` 与 `XDG_CONFIG_HOME`），覆盖 `Objects` 固定页随树选中切换（选中库 → 该库的
表、选中视图 → 该库的视图，含行数 / 首行 / 选中行 / 空态 / 状态行断言与截图）、单击选中同步
信息面板、双击 / 回车激活开标签、作用域未变不重发、切回 `Objects` 页清掉行选中、
树形搜索结果（A10：`--qa-search probe` 命中表 + 视图共 2 个对象，`rows=` 断言
连接 > 库 > `Tables` > 表 > `Views` > 视图的行序、`highlight=` 断言只有命中行高亮）、
清空搜索退出筛选（A11：`state=idle` + `hits=0`）、点搜索结果里的表直接开数据标签且结果页原样
保留（A12：`--qa-search-click-object` 走真实 `row-activated` 信号，日志有「打开表 …」与
「新建表标签」、**没有**「跳转成功」/「跳转超时」，`state=results` + `hits=3` + 状态行不变）、
`EX` 整表导出（行数 = 数据行 + 1、
权限 0600、目录无 `.sqarvo-tmp` 残留、日志命中完成与进度）、取消路径（目标不创建、无残留、
日志写明「已写入 N 行后取消」）与默认的当前页导出对照。断言读控件文本、日志与文件事实，
失败以非零退出，末尾打印「N 项断言全过」；证据全部落在仓库外。

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
- `driver: "mariadb"` 需要 `host` / `username`；`database` 可选，省略（或空串）= 无默认库，
  连接后树根层直接列出所有库，会话名退化为 `用户@主机`；
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
