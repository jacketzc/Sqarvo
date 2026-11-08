# ADR 0025：UI 行为契约的单一来源（跨平台移植）

- 状态：**讨论稿（未评审，不作为实施依据）**
- 日期：2026-11-08
- 关联：Issue 待建；REQ-PLAT-001、REQ-UI-001、REQ-A11Y-001；AGENTS.md §8（原生前端）、
  §13（测试）、§14（文档单一来源）；ADR 0021（标签 ↔ 会话）、ADR 0022（表浏览分页）、
  ADR 0023（区域一致与原生控件）
- 上游：[workspace-model.md](../ui-specification/workspace-model.md)、
  [design-principles.md](../ui-specification/design-principles.md)

## 背景

Windows 与 macOS 前端即将接后端（M6 剩余条件），两端目前只有窗口骨架
（`frontend/windows/Sqarvo.Windows/{App.xaml,MainWindow.xaml}`、
`frontend/macos/Sources/SqarvoApp/ContentView.swift`，自 `3b6a811` 初始化后未改动）。
照抄 Linux 的实现细节会把 GTK 的取舍固化成「规范」，而这个风险不在于外观，在于行为。

### 已有的（这些不需要重做）

| 层 | 位置 | 覆盖 |
|---|---|---|
| 结构规范 | `docs/ui-specification/workspace-model.md`（279 行） | 六区域、尺寸算术、标签模型、对象树语义、信息面板、状态栏字段、§8 平台映射、§9 禁用项、§10 快捷键、附录 A 有意偏差 |
| 设计原则 | `docs/ui-specification/design-principles.md` | 六种状态（Loading/Empty/Success/Error/Cancelled/Disconnected）、键盘可达、危险操作分级 |
| 平台口径 | ADR 0023 | 「区域与语义一致、控件原生」；**验收按语义不按像素** |
| 行为语义 | ADR 0005、0008、0017、0021、0022 | 结果游标、会话并发与取消、行编辑与取消、标签 ↔ 会话、分页/排序/筛选/总数 |
| 自动化 | `frontend/linux/src/qa.rs`、`layout_audit.rs`、`scripts/v023-acceptance.sh` | 走真实用户路径的动作脚本、量化「不裁切」自检、139 项断言 |

结构层已经足够细，能直接支撑另一端的布局实现。真正缺的是**动作级行为**。

### 缺口

1. **动作级逻辑只在 GTK 代码里**：每个操作的前置条件、禁用理由、调用的 RPC 序列、
   成功/失败后的状态跃迁、用户可见文案，没有任何一份成表的记录。窗口动作只注册了 18 条
   （`frontend/linux/src/ui.rs::install_window_actions` 的 16 条 + `app.close-window` +
   `app.quit`；v0.2.4 删掉 `win.toggle-inspector` 后从 19 条减为 18 条）；而右键菜单条目、单元格双击编辑、列头排序 / 拖宽 / 双击自适应、分页器、
   脏行三选确认、导出取消、只读态禁用等路径**没有**进 GAction。恰恰是这些没进 GAction 的
   路径，新平台最容易漏。总量粗估 45–60 条，需在动作目录阶段实测清点。
2. **文案散落**：用户可见中文以内联字面量与 `format!` 分布在源码各处，
   `frontend/linux/src/*.rs` 里只有 10 个 `pub const …: &str`。而文档要求三端文案一致，
   目前没有共用来源，也没有比对手段。
3. **错误码 → 呈现映射缺失**：`docs/api/grpc-conventions.md` 只有协议层错误码与 gRPC 状态码
   的对应；「哪个错误出横幅、哪个进确认框、哪些可重试、retry 按钮文案」没有成表。
4. **现有验收证据不可移植**：`layout_audit.rs` 依赖 GTK 的 `Widget::measure`，截图依赖
   GTK 自渲染（Wayland 下系统截屏不可靠），`--qa-tabs-file` 的步骤名绑定 GTK 控件。
   新平台要么重写整套验收链，要么事实上没有验收。

### 为什么现在写

ADR 0023 已经把「哪些必须一致」定在语义层，但语义层没有可验收的载体。两端接入时，
「一致」的实际判据只能是「照 Linux 抄」，这会连 GTK 的实现取舍一起固化。
在两端动手之前把动作层写成可对照、可比对的事实源，成本最低；接入之后补，等于给既成事实
追写规范。

## 备选方案

1. **只扩充 prose 文档**：在 `workspace-model.md` 里补动作描述。成本最低，但散文无法被
   自动化校验，也说不清「失败后去哪一步」，三端仍会分歧。
2. **三层事实源 + 防漂移校验**（建议）：动作目录（规范化表格）+ 状态机转移表 +
   可回放流程 fixture；由脚本双向比对代码与文档，CI 阻断漂移。
3. **共享 UI 核心**：把状态机与动作分派抽成平台无关的 Rust 核心，经 FFI 给 C#（P/Invoke）
   与 Swift。逻辑确实只有一份，但代价是三端构建链与绑定维护、错误与异步语义跨 FFI 的
   重新定义，以及与本项目「各端原生、不强制共享 UI 代码」取向的张力（§2.1）。
   当前没有三端漂移的实测证据，属于为未发生的问题预付复杂度。
4. **截图 / 录屏当规范**：能证明「长什么样」，证明不了「什么条件下可用、失败后去哪」，
   也无法断言 RPC 序列；Wayland 下还依赖 GTK 自渲染才能取到窗口内容。

## 建议决策（待评审）

采用方案 2：三份平台无关的交付物 + 一条 CI 守护。全部是**数据与文字**，不含任何一端的代码。

### 1. `docs/ui-specification/actions.md` —— 动作目录

一条动作一行，列固定：

| 动作 ID | 入口 | 前置条件与禁用原文 | RPC 序列 | 状态跃迁 | 用户可见文案 | 平台差异 |
|---|---|---|---|---|---|---|

- 动作 ID 点号分层、稳定、全小写，例如 `connection.connect_saved`、`tab.open_table`、
  `grid.sort`、`rows.commit`、`export.csv`；一旦被 fixture 引用即进入兼容面。
- 「禁用原文」指控件置灰时 tooltip / 说明里的原文，例如 §9 的「导入在 v0.2.5 提供」。
  这一列是移植时最容易丢、也最容易各写一套的内容。
- 示例（仅示意格式，取值以评审后的正文为准）：

```text
connection.connect_saved | 侧栏行左键（启动态与已连接树同一入口） | 未连接且 credential_ref 非空；
为空 → 打开预填对话框，提示「这条连接缺少密码」 | Connect → ListSessions → ListNodes(catalog)
| Disconnected → Connecting → Browsing | 「这条连接缺少密码」 | 无

tab.open_table | 树双击 / 右键「打开」 | 节点 kind ∈ {table, view}；连接中禁用
| ListNodes(table) → BrowseTable(首页) | — → Opening → Ready（Loading → Success/Empty/Error）
| 「打开失败：<错误码>」 | 无

rows.commit | 工具条第 2 行「提交」 | dirty > 0 且非只读；超过阈值走确认框
| CommitRows(stream) | Dirty → Committing → Ready | 「已提交 N 行」/「M 行提交失败」
| macOS 单行状态栏
```

### 2. `docs/ui-specification/state-machines.md` —— 状态机

五个有生命周期的对象各一张转移表，每张表带一列「六态如何呈现」：

- **连接节点**：Disconnected / Connecting / Browsing / Disconnecting / Error（保留错误码与 detail）。
- **标签**：Opening / Ready / Dirty / Committing / Closing（脏行三选、未提交事务提示）。
- **表格**：Loading / Empty（0 行结果集）/ Success / Error / Cancelled / ReadOnly。
- **查询**：Idle / Running / Cancelling / Done / Cancelled / Failed。
- **导出**：Idle / Running / Done / Cancelled / Failed（目标不留半截文件）。

本文件只做**汇总与呈现口径**，不重复 ADR 0005 / 0008 / 0017 / 0021 / 0022 已定的语义；
冲突时以 ADR 为准并在本文件标注引用。规则类内容（「连接中重复点击被忽略」「取消幂等」
「切换标签不重建会话」）按对象归位到对应表下。

### 3. `tests/contract/ui/flows/*.json` —— 可回放流程

从现有 `--qa-tabs-file` 脚本导出，去掉一切控件名词，只留动作 ID 与期望轨迹：

```json
{"flow": "open-table-sort-page",
 "steps": [
   {"do": "connection.connect_saved", "target": "演示库 · MariaDB sqarvo_demo",
    "expect": {"state": "Browsing", "rpc": ["Connect", "ListSessions", "ListNodes"]}},
   {"do": "tab.open_table", "target": "orders",
    "expect": {"state": "Loading", "rpc": ["ListNodes", "BrowseTable"]}},
   {"do": "grid.sort", "target": "amount:desc", "expect": {"rpc": ["BrowseTable"]}},
   {"do": "grid.page", "target": "next", "expect": {"state": "Success", "rows": 500}}
 ]}
```

断言对象是 RPC 序列、状态、行数、文案子串，**不含坐标与控件类型**——与 ADR 0023
「按语义不按像素」一致。新平台写一个 runner 读同一份 fixture，比对它自己的 RPC 轨迹。

### 4. `scripts/check-ui-contract.sh` + CI job —— 防漂移

没有这条，前面三份就是死文档：

- 扫 `gio::SimpleAction::new("…")`、`set_accels_for_action("…")`、`qa.rs` 的脚本步骤关键字，
  与 `actions.md` 的动作 ID 集合、fixture 的 `do` 取值做**双向**比对（文档有而代码无、
  代码有而文档无，都失败）；
- `actions.md` 每行带代码锚点（符号名，例如 `ui.rs::connect_saved`，不用行号），
  锚点在源码中不存在即失败；
- fixture 的 golden 由 Linux 实现重放生成，用 `SQARVO_UI_CONTRACT_UPDATE=1` 显式更新，
  CI 只校验不更新。

### 5. 文档更新触发条件

在 AGENTS.md §14 的触发列表里加一条：新增或修改用户可见动作、状态、文案时，
必须同步 `actions.md` / `state-machines.md` / 对应 fixture；PR 模板加一行自查。
这与 §14.1「一份计划一个地方」的口径一致：动作逻辑只有这一处事实源。

## 取舍

- 三份文档 + 一个脚本是额外维护成本。用 CI 双向比对把它变成「改代码时被迫改文档」，
  而不是「定期人工核对」。
- fixture 只断言 RPC 与状态，不断言外观。这意味着按钮位置不对、间距不对**永远不会**被 CI
  发现，仍需人看；外观问题按 ADR 0023 属于平台自由，这是有意的边界。
- 动作 ID 成为兼容面：改名要同时过 fixture 与三端实现。这是可接受的代价（比三端各叫一套
  更便宜），但要求评审时把 ID 命名口径定死。
- 契约不承诺三端共享代码：同一份契约由三份实现分别满足，仍需三套 runner。
  runner 逻辑应保持极简（发动作 + 比对轨迹），否则本身就是第二份产品代码。
- 已知纸面风险：Windows / macOS 上「程序化触发一个动作」的通道尚未验证
  （WinUI 的 AutomationPeer / InvokePattern、AppKit 的 `NSMenuItem.performAction`）。
  本 ADR 通过前应先在一端做最小验证，否则第 3 项的可执行性没有依据。

## 落地切片（建议顺序）

1. **连接 + 表浏览两条流程**：`actions.md` 对应段落 + 状态机两章 + 1 个 fixture +
   `check-ui-contract.sh`，接 CI。约 1–2 天。
2. **写路径**：begin / edit / apply / rollback、删除行、脏行三选、提交阈值确认、只读态禁用。
3. **导出与跨库**：CSV 公式前缀开关、截断标记、取消路径、`ObjectRef.catalog` 跨库打开、
   无默认库连接。
4. **错误与取消态**：错误码 → 呈现映射落地（见未决问题 4）、取消幂等与「已取消」文案。
5. **全量清点**：把「45–60 条」换成实测数字，并核对是否有动作只存在于某端。

全量动作目录 + 状态机粗估 2–3 天；此后随 Issue 增量维护，不再集中返工。

## 附：v0.2.4 动作目录增量（讨论稿先记下来，评审后并入正文）

本轮的 UI 逻辑调整直接改变了动作面，按下表登记（ID 命名口径沿用上文）：

| 变化 | 动作 ID | 入口与语义 |
|---|---|---|
| 新增 | `tree.select` | 树行单击：只选中，信息面板与 `Objects` 页跟着换内容 |
| 新增 | `tree.activate` | 树行双击 / 回车：表视图=打开；连接=连接；库/分组=展开收起 |
| 新增 | `tree.group.expand` | 表节点下的 `Fields` / `Indexes` / `Foreign Keys` / `Triggers`，每组一次 `ListNodes(filter.kinds)` |
| 新增 | `objects.open_table` | `Objects` 页行双击 → 数据标签（与 `tab.open_table` 同一条 RPC 序列） |
| 变更 | `tab.open_table` | 入口从「单击」改为「双击 / 右键打开」；右键菜单条目只剩「打开」 |
| 删除 | `tree.view_ddl` | 右键「查看 DDL」删除；DDL 只在信息面板 `[DDL]` 页看 |
| 删除 | `backend.disconnect` | 「断开后端」三处入口删除（用户可见面里没有前后端之分） |
| 新增 | `backend.restart_app` | 后端失效对话框：「重启 Sqarvo」= 释放工作区与会话后重新握手 |
| 新增 | `backend.open_logs` | 同一对话框：「查看日志」= `$XDG_STATE_HOME/sqarvo/desktop.log` 的末尾 200 KB |
| 变更 | `export.csv` | 表数据新增 `scope = page | table`：`page` 写当前页已加载行；`table` 由 worker 后台线程按当前排序/筛选 offset 逐页读取、流式写同一文件，有进度、可取消（取消/失败不留半截文件） |
| 变更 | `rows.cancel` | 语义不变（ADR 0018），入口从文字改为 `process-stop` 图标，tooltip 写明「整批回滚」 |
| 变更 | `txn.begin` | 「自动提交」徽标删除；开始事务与提交/回滚互斥显示 |

## 未决问题（评审要回答的）

1. **放哪里**：动作目录属于「技术事实」（`docs/ui-specification/`），还是与 fixture 同址
   （`tests/contract/ui/`）？AGENTS §14 的分层表把两者分开，需要明确一处主副本。
2. **runner 用什么写**：各端用各端语言（C# / Swift / Rust），还是用一个独立的驱动器
   （不经任何前端、直接对后端 gRPC 重放）？后者更省，但只能证明后端语义，不能证明 UI 接线。
3. **文案是否抽成可加载资源**（例如 `ui-contract/strings.json`，三端共用键与原文）：
   一致性强，但引入一层加载与打包；不抽则零成本、靠 CI 文本比对。
4. **错误码 → 呈现映射**放本 ADR，还是单独一条 ADR？它影响所有服务域，不只 UI。
5. **动作 ID ↔ 可访问性名称**的对应表是否需要：ADR 0023 要求按语义验收，自动化必须能读到
   控件名，三端命名不统一就等于没有可读的语义。

## 重新评估条件

- Windows 或 macOS 接入时发现动作无法被程序化触发（无 AutomationPeer / 菜单 perform 通道），
  fixture 方案退化为手工验收清单，本 ADR 需改写第 3 项；
- 三端出现动作语义分歧（同一动作在不同端的 RPC 序列不同），说明契约粒度不对，
  或共享核心（方案 3）的必要性上升；
- 动作总量远超预估（例如 > 120 条），需要分组、自动生成与按域拆分文档；
- 文案需要多语言，第 3 项的资源化从「可选」变为「必须」。

## 参考（现状证据）

- `frontend/linux/src/ui.rs::install_window_actions` —— 18 条 GAction 与 `set_accels_for_action`
- `frontend/linux/src/qa.rs` 模块文档 —— `--qa-tabs-file` 脚本 DSL（`open-table` / `sort` /
  `filter` / `page` / `export` / `expect-trace` / `expect-rows` …）及 #139 / #144 增补
- `frontend/linux/src/layout_audit.rs` —— GTK 专用量化自检（失败以 5 退出）
- `frontend/linux/scripts/v023-acceptance.sh` —— 139 项断言（54 应用内 + 85 脚本）
- `frontend/windows/Sqarvo.Windows/{App.xaml,MainWindow.xaml}`、
  `frontend/macos/Sources/SqarvoApp/ContentView.swift` —— 两端骨架
