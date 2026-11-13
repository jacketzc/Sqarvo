# UI 行为契约与全局规则

本文件管两件事：**全局规则**（§8：快捷键、危险操作确认、五态、失败文案硬约束）与**已知偏差**
（§9）。逐组件的行为——动作、可用性条件、结果文案、源码依据、三端同步状态——在**组件注册表**
[`components/`](components/README.md)：注册表共 **9 个功能区 / 187 个组件**，本文件 §1–§7 是其中
7 个主要区的指针（24+25+10+20+36+20+22 = 157 个组件），另两个区 `dialogs`（25）/ `startup`（5）
的指针见 §7 之后；分工与边界见 [`components/README.md`](components/README.md)。

## 定位、分工与优先级

| 文件 | 管什么 | 不管什么 |
|---|---|---|
| [`components/*.toml`](components/) | 组件存在性、层级、样式引用、交互、生命周期、三端同步 —— **UI 行为的唯一真值** | 全局规则、版式算术 |
| **本文件** | 全局规则（§8）、已知偏差（§9）、各区指针 | 单个组件的动作细节 |
| [`ui-components.md`](ui-components.md) | 注册表的生成物（人读），禁止手改 | — |
| [`workspace-model.md`](workspace-model.md) | 区域划分、布局算术、平台映射、密度 | 具体动作语义 |

冲突时：单组件行为按注册表；全局规则按本文件；版式按 `workspace-model.md`。三者都不许与代码矛盾。

**维护规则**：

- 改某个组件的行为 → 改注册表对应组件并 `mark`（AGENTS.md §14.3）。
- 改全局规则（快捷键 / 危险操作 / 五态 / 失败文案）→ 改本文件 §8，并同步到受影响的组件。
- 发现代码与注册表不一致 → 记 §9（代码如此 / 文档如此 / 建议），能当场改的就改掉再删条目。
- 依据是代码，不是旧文档：注册表与本文都以已完成的 Linux 客户端为依据（`frontend/linux`，
  源码路径相对它）；Windows 现状在各组件的 `sync.windows` 与 `platform_notes.windows` 里。
- QA 专用路径（`--qa-*`）不算用户可达行为。
- 发版前跑 `scripts/check-ui-sync.sh release-check`（门禁见 AGENTS.md §14.4）。

---

## 1. 窗口壳与主工具条

已迁入组件注册表 → [`components/shell.toml`](components/shell.toml)（24 个组件：窗口与标题、主工具条、
四个种类入口、菜单栏、密度档、错误页、后端失效恢复）。原 §1.1–§1.6 各小节都归到上面这个文件，
其中 §1.6 的 Windows 窗口层并入对应组件的 `platform_notes.windows`；未收口项见 §9。

## 2. 对象树

已迁入组件注册表 → [`components/object_tree.toml`](components/object_tree.toml)（25 个组件：树本体、
行模板与七种行、搜索区、右键菜单九项、底部提示行）。原 §2.1–§2.5 各小节（结构懒加载、单击 / 双击 /
回车、右键菜单、搜索、键盘）都归到上面这个文件。

## 3. Objects 固定页

已迁入组件注册表 → [`components/objects_page.toml`](components/objects_page.toml)（10 个组件：
固定页签、三列表格、状态行、加载更多、空态）。

## 4. 标签

已迁入组件注册表 → [`components/tabs.toml`](components/tabs.toml)（20 个组件：标签条、标签项、
关闭确认框、查询标签的动作行 / 编辑器 / 结果区）。原 §5.11（查询标签的版面收敛与不可用态）
也归 `tabs`。

## 5. 表数据页

已迁入组件注册表 → [`components/data_page.toml`](components/data_page.toml)（36 个组件：工具条、
事务、筛选 / 排序 / 列显隐、网格与编辑、分页、导出、行右键、展示形式切换、查询结果单表编辑）。
原 §5.1–§5.10 各小节都归到上面这个文件。

## 6. 信息面板（Inspector）

已迁入组件注册表 → [`components/inspector.toml`](components/inspector.toml)（20 个组件：标题块、
四种视图、DDL 与表信息两页、状态与提示行）。

## 7. 状态栏

已迁入组件注册表 → [`components/status_bar.toml`](components/status_bar.toml)（22 个组件：左边沟、
行 1 行操作与分页器、行 2 摘要）。原 §7.1 的 Windows 现状已并入各组件 `platform_notes.windows`。

**另两个功能区的指针**（不在 §1–§7 编号序列，补在这里以对齐 9 区 / 187 组件；与 §1–§7 同为索引，
行为细节仍在各自 toml）：

- 对话框 → [`components/dialogs.toml`](components/dialogs.toml)（25 个组件：新建 / 编辑连接对话框、
  关于对话框等独立于某个动作、有自己生命周期的表面）。
- 启动态 → [`components/startup.toml`](components/startup.toml)（5 个组件：启动 / 握手外壳、
  Loading 阶段文案）。

7 区 157 个 + `dialogs` 25 个 + `startup` 5 个 = **9 区 / 187 个组件**，与严格校验口径一致。

## 8. 全局规则

### 8.1 快捷键（以代码为准）

| 动作 | 快捷键 | 语义 | 源码依据 |
|---|---|---|---|
| 新建连接 | `Ctrl+N` | 打开连接对话框（后端未握手时忽略） | `src/ui.rs:320`、`src/ui.rs:4775` |
| 新建查询标签 | `Ctrl+T` | 与 `＋`、工具条「新建查询」同一路径 | `src/ui.rs:327`、`src/ui.rs:4781` |
| 关闭当前标签 | `Ctrl+W` | 非 `Objects`；有脏行 / 事务时进入三选。**菜单里已不列「关闭标签」**，只保留这条快捷键 | `src/ui.rs:328`、`src/ui.rs:4792`（`win.close-tab`） |
| 关闭窗口 | `Ctrl+Shift+W` | 与标题栏关闭按钮同一清理路径 | `src/ui.rs:204-208` |
| 退出应用 | `Ctrl+Q` | 走关窗守卫：先 `window.close()` 主窗口（与标题栏关闭 / `Ctrl+Shift+W` 同一条 `close-request`），有未提交更改时先弹「有未提交的更改」（取消 / 放弃更改并退出），窗口关完后 GApplication 自然退出；只有没有窗口时才直接 `app.quit()` | `src/main.rs:76`、`src/main.rs:340-365` |
| 下一个标签 | `Ctrl+Tab` | 选中 `AdwTabView` 的下一个标签；最后一个再往后**回绕到第一个**（`Objects` 也在循环里）。`AdwTabView` 自带的同名快捷键已摘掉，只剩这一条路径 | `src/ui.rs:332`、`src/ui.rs:4805`、`src/tabs.rs:3814` |
| 上一个标签 | `Ctrl+Shift+Tab` | 与上一行反向：第一个再往前回绕到最后一个 | `src/ui.rs:333`、`src/ui.rs:4814`、`src/tabs.rs:3816` |
| 直选第 N 个标签 | `Alt+1`…`Alt+9` | 选中第 N 个标签（1 起，`Objects` 算第 1 个）；超出当前标签数时**什么都不做**（不弹框、不崩），只记一行 `[tabs] Alt+3：只有 2 个标签，忽略` | `src/ui.rs:334-335`、`src/ui.rs:4826`、`src/tabs.rs:3818` |
| 刷新当前表 / 对象树 | `F5` | 当前标签是表数据页 → 刷当前表（保持排序 / 筛选、回到第 1 页；脏行守卫照旧）；否则刷整棵对象树。**菜单里已不列「视图 → 刷新」**，只保留这条快捷键 | `src/ui.rs:323`、`src/ui.rs:2519-2528`、`src/ui.rs:4852` |
| 聚焦对象搜索 | `Ctrl+F` | 焦点交给窗口左下角（状态栏左边沟）的对象树搜索框 | `src/ui.rs:325`、`src/ui.rs:4863` |
| 导出当前标签 | `Ctrl+Shift+E` | CSV（表标签先选范围） | `src/ui.rs:356`、`src/ui.rs:4972` |
| 执行 SQL | `Ctrl+Enter` | 编辑器内生效：执行整段（不是「当前语句」）。**菜单「查询」整项不挂**，只保留快捷键与查询页按钮 | `src/sql_page.rs:283-287`、`src/ui.rs:4874`（`win.execute`） |
| 取消查询 | `Esc` | 编辑器内生效；幂等（重复点不报错、不发 RPC）。**菜单「查询」整项不挂**，只保留快捷键与查询页按钮；执行中 `Esc` 取消查询，空闲态分流见下一行 | `src/sql_page.rs:288-297`、`src/ui.rs:4886`（`win.cancel`） |
| 取消行选中（空闲态） | `Esc` | 编辑器 / 结果区 / 数据网格空闲（无查询执行、无行内编辑、无筛选面板）时，`Esc` 清空行选中，不改数据、不弹框；与「取消查询」「关闭筛选面板」按当前上下文分流 | `src/sql_page.rs:288-297`、`src/results.rs:376-388`、`src/data_page.rs:1243-1255` |
| 关闭筛选面板 / 收起条件编辑 | `Esc` | 只在筛选面板内生效：先收起行内编辑，没有编辑态时关面板 | `src/data_page.rs:1803-1816` |
| 列表移动 / 激活 | `↑` `↓` / `Enter` | GTK 默认；`Enter` 等同双击（树与 `Objects` 页三列表格） | `src/tree.rs:2041`、`src/tabs.rs:938` |
| 界面密度升一档 | `Ctrl+Plus`（`Ctrl+KP_Add`、`Ctrl+=`） | 见组件 `shell.density`：目标档在当前显示器放不下时忽略并记日志，已在最高档时同样忽略 | `src/ui.rs:357-366`、`src/ui.rs:5005` |
| 界面密度降一档 | `Ctrl+minus`（`Ctrl+KP_Subtract`） | 见组件 `shell.density`；已在 `compact` 时忽略 | `src/ui.rs:365`、`src/ui.rs:5014` |
| 界面密度重置 | `Ctrl+0` | 直接回到 `compact`（不是「回退一个可用档」） | `src/ui.rs:366`、`src/ui.rs:5023` |

`Ctrl+Tab` / `Ctrl+Shift+Tab` / `Alt+1..9` 目前只在 §8.1 登记，组件注册表暂无对应的 `keys`
条目；F5 在表数据标签刷表在 `data_page` 区也暂无条目。注册表的补登记由对应改动处理（见 §9），
本文件不改注册表。

### 8.2 危险操作确认规则

总原则：**可能改动数据或不可逆的操作，按风险给确认；普通查询不加确认**。满足以下任一条件
就必须确认：变更行数 **> 100**，或**包含任何删除**（删除单独警告）。

| 操作 | 触发条件 | 对话框与按钮 | 源码依据 |
|---|---|---|---|
| 提交行变更 | 变更 > 100 行，或含删除（本地判定）；服务端要求 `acknowledge_risk` 时**再次**走同一个对话框 | 标题「确认提交变更？」；正文给更新 / 删除行数与删除警告，>100 行时追加「变更数超过 100 行的风险阈值…」；按钮 取消 / 确认提交（破坏性，默认取消）；取消 →「已取消提交（未发送任何变更）」 | `src/table_data.rs:30`、`src/table_data.rs:489-515`、`src/data_page.rs:5218-5219` |
| 回滚事务 | 有活动事务 | 标题「回滚事务？」；有脏行时正文写明会丢弃数据库侧变更与 N 行界面更改；按钮 取消 / 回滚（破坏性，默认取消） | `src/data_page.rs:5447` |
| 断开连接 | 已连接 | 标题「断开连接？」；说明只关浏览会话、数据页另一条会话仍可看已加载数据；按钮 取消 / 断开连接（破坏性，默认取消） | `src/ui.rs:3742` |
| 删除已保存连接 | 连接在配置里 | 标题「删除连接？」；按钮 取消 / 删除连接（破坏性，默认取消） | `src/ui.rs:1655` |
| 关闭含脏行或事务的标签 | 有脏行 / 有未提交事务 | 标题「关闭标签？」；按钮见组件 `tabs.close_dialog`（默认取消） | `src/tabs.rs:227-253`、`src/tabs.rs:2718-2724` |
| 关闭窗口（未提交的更改） | 任一表数据标签有未提交的更改：脏行（含标记删除）或「新增行」草稿 | 标题「有未提交的更改」；正文逐表列出（`users：3 行未提交`、`orders：1 行标记删除`，同一表同时有两类时写 `2 行未提交 · 1 行标记删除`），并给出「选『取消』回到工作区提交或放弃」的出口；按钮 取消 / 放弃更改并退出（破坏性，默认取消，关闭 = 取消）；取消 → 窗口保持原样，日志「取消关窗：保持在工作区」；放弃 → 日志「已放弃未提交更改并退出」，随后走 §1.1 的清理流程。确认过一次后不再二次弹窗；查询标签的未提交事务不在本守卫范围内（关标签时仍按上一行确认） | `src/ui.rs:989`（`confirm_close_with_dirty`）、`src/tabs.rs:283`（`close_guard_body`）、`src/data_page.rs:2287`（`dirty_tab_summary`） |
| 覆盖已存在的导出文件 | 目标路径存在 | 再确认一次；默认**不覆盖**（默认取消） | `src/export_ui.rs:852` |
| 重启 Sqarvo | 后端失效 | 标题「Sqarvo 需要重启」；正文写明会关闭所有标签页、未提交更改会丢失；重启按钮为破坏性 | `src/ui.rs:1291` |

### 8.3 加载 / 空 / 错误 / 取消 / 断开 五态

| 状态 | 对象树 | `Objects` 页 | 表数据页 | 查询标签 | 外壳 / 状态栏 |
|---|---|---|---|---|---|
| **加载** | 占位行「读取中…」/「加载中…」 | 状态行「正在读取…」 | 占位页「正在读取表数据…」+ 工具条 spinner 与「正在读取表数据…」 | 结果区「正在执行…」、取消按钮可点 | 启动/握手：居中 spinner + 阶段文案；分页摘要「读取中…」 |
| **空** | 空分组说明行；注册表为空 → 状态页「还没有保存的连接」 | 空态页（「没有选中对象」+ 说明）或「（这个库里没有这类对象）」 | 「空表」/「本页没有数据」+ 原因 | 空 SQL 提示「SQL 不能为空：请输入后按 Ctrl+Enter」 | 行操作全禁用 + 原因 tooltip |
| **错误** | 占位行「读取失败…」+ 状态行错误码 | 「读取失败：<headline>（<code>）」 | 错误横幅 + 状态行 error + 逐行错误列表；行保留 | 失败态（保留已加载行），区分「客户端上限」与「查询失败」 | 后端四态错误页 + 「重试」 |
| **取消** | 不适用 | 不适用 | 取消提交 = 整批回滚，脏行与编辑文本保留；分页取消由新一轮读取覆盖 | 取消后状态 `Cancelled`，保留已加载行 | 导出取消 → 无半截文件 |
| **断开** | 该连接的层全部作废；其它连接不受影响 | 页面保留，下一轮请求报「这条连接没有浏览会话」 | 丢弃未提交更改，横幅「已断开：…」；已加载行保留为只读 | 明确标注失连，执行 / 取消禁用 | 标题 `Sqarvo`、副标题「已断开」；状态栏清连接名 |

### 8.4 失败文案的硬约束

| 约束 | 具体要求 | 源码依据 |
|---|---|---|
| 不泄露凭据 | 界面与日志都不得出现密码、`credential_ref` 指向的密钥值、会话 token；`ConnectionDraft` 的 `Debug` 把密码打成 `<redacted>`；错误类型模块明确「不得包含会话 token 或任何凭据」 | `src/backend/connection.rs:7`、`src/backend/connection.rs:248`、`src/backend/error.rs:1-7` |
| 不回显已保存密码 | 编辑已保存连接时密码行永远留空；有 `credential_ref` 就复用引用，提示「密码将从系统凭据库读取」 | `src/dialog.rs:593-628` |
| 不展示堆栈 | 界面只写可操作文案（下一步做什么），不出现 panic / backtrace / 内部结构体；日志可以更详细，但也不含凭据 | `src/backend/error.rs:1-9`、`src/backend/table.rs:790-793` |
| 稳定错误码 | 失败文案形如「<headline>（<code>）」或「<headline>：<detail> [<code>]」；分类不合并成一句「失败」 | `src/backend/table.rs:790-793`、`src/tabs.rs:1127-1131` |
| 不编造字段 | 拿不到的值不出现；不写 `—` / `N/A` / 空占位冒充；估计值必须写「约」 | `src/inspector.rs:349-359`、`src/browse_view.rs:635-646` |
| 不假装成功 | 未实现能力如实写「尚未实现 / 驱动未提供」，0 次 RPC、不摆假数据；删除配置成功但密钥未删时如实提示 | `src/tree.rs:302-310`、`src/ui.rs:1355` |

---

## 9. 已知偏差与存疑

**本节只列仍未收口项与已知边界两类，二者分开列**：§9.1 是仍未收口的偏差（代码与产品口径
尚未一致、或实现与注册表 / 文档尚未收口的地方），按「代码如此 / 文档如此 / 建议」写明，
绝不静默删除；只有已被代码收口的条目才从这里删除，其口径直接写在本文件正文相应小节。
§9.2 是已知边界，不是偏差，单列方便检索。历史核验快照见 git 历史（`32b049c` 的
`ui-behavior-contract.review.md`，已删除，只作为那次核验的记录存在）。

2026-11-10 复核（v0.2.3.4）时曾记「产品口径层面没有未收口项」，那只是当时的历史快照；
2026-11-12 把功能章节迁入组件注册表时，又暴露出若干「实现与文档 / 与注册表口径不一致」
的条目，列在下面 §9.1，逐条带代码依据；它们需要产品或实现裁决，不由文档单方面拍定。

### 9.1 注册表与 Windows 实现不一致（2026-11-12 记录）

先收口三条**文档侧**（以代码为准，已改）：Windows 状态栏「已连接 · 驱动 N · 浏览会话 M」在行 1
左格而非旧 §7.1 写的行 2 右格；`objects_page.tab` 的启动态副本由 `startup.rs` 自建；Linux 无表标签
时分页器置灰不隐藏（保持行 1 右组宽度不跳）。

下面三条已裁决、待 Windows 端实现（另一任务执行，锚点与验收见表格）：

| 项 | 决定 | 实现锚点 | 完成后 |
|---|---|---|---|
| 断连原因可复制 | 补 | `Views/ShellView.xaml` 的 `SqlSummaryText`（`a84560f` 上在 :826；旧稿写「约 :676」，那时该行是 `DragCompleted`，锚点已漂移）加 `IsTextSelectionEnabled="True"` | `status_bar.row2.sql` 的 windows note 去掉该差异 |
| 「新增 / 删除」行按钮 | **不渲染**（不是置灰） | `Views/ShellView.xaml` 删 `ButtonRowAdd` / `ButtonRowDelete`（`a84560f` 上在 :770 / :773；旧稿写 :622 / :625，已漂移）；`Views/ShellView.Data.cs:2069-2072` 目前只是 `IsEnabled=false`，接线与 tooltip 一并处理；`Ctrl+S` / `Ctrl+Z` 仍是提交 / 撤销入口 | `status_bar.row1.ops.add` / `.delete` 的 `sync.windows` 改 `na` + note |
| 行 2 会话格 | 与 Linux 一致：当前标签是表数据标签时整格不出现 | `Views/ShellView.Chrome.cs` 的 `ApplyStatusChrome`（:120-141）按当前标签切可见性，切标签即时刷新；当前无条件写 `SessionText` | `status_bar.row2.session` 去掉「恒写入」差异 |

**已收口（不再列为偏差）**：Windows 侧栏与信息面板拖动已由 #168 实现
（`Views/ShellView.xaml:407-409,474-483,669-678` 两条 Thumb 分隔线；
`Views/ShellView.xaml.cs:932-997` 拖动 / 双击复位 / 键盘微调的全套处理），`object_tree` /
`inspector` 的 `sync.windows` 随之收口，这里不再保留旧缺口。

**Objects 页双击 / Enter 打开表数据标签（仍未收口）**：
- **代码如此**：契约 `components/objects_page.toml:224-228` 的 `double_click` 记为「打开表数据
  标签（与树双击同一条 `open_table`）」、`:145-148` 的 `Enter` 等同双击、`:236-239` Linux 记
  `done`；Windows 实作 `Views/ShellView.Metadata.cs:772-776`（双击 → `SelectObjectRow`）、
  `:879-880`（注释「这一层只同步信息面板」）、`:905-908`（Enter → `SelectObjectRow`）都只选中行、
  同步信息面板，不打开表数据标签。
- **文档如此**：组件注册表把双击 / Enter 记为「打开表数据标签」，`sync.windows` 仍是 `partial`，
  note 只登记了 ↑/↓ 移动与 Enter 同步信息面板，未登记双击打开。
- **建议**：要么让 Windows 双击 / Enter 走 `open_table` 打开表数据标签并更新 `sync.windows`，
  要么按 AGENTS.md §14.3 走 `deprecated/removed` 把该交互降级；在代码收口前本条保留在 §9，
  **不写成已实现**。

**本轮已收口的文档侧差距（改动确认单 #5，基线 `a84560f`）**：`check --strict` 原报 39 条
过期同步标记、81 条缺 `frontend/windows/…:行` 锚点，另有部分 `interaction.source` 行号随
`ui.rs`（已从约 5082 行增长到 5584 行）等源文件增长而漂移、`AGENTS.md:522` 把组件数写成 186
（应为 187）。本轮由各组分工重填真实锚点、把 11 处误嵌进 `[[component.interaction]]` 的
`platform_notes` 移回组件顶层、刷新三端 `spec_hash` 并按 `gen` 重建生成物；收口后
`scripts/check-ui-sync.sh check --strict` 为 **0 过期 / 0 警告 / exit 0**。

**按 §14.2 保留、不得静默删除的真实差距**（明细在各注册表 `sync.windows.note`）：
- 上面「Objects 页双击 / Enter 打开表数据标签」与 §9.1 三条待 Windows 端实现的裁决；
- 10 条违反 `components/README.md` 粒度规则的纯布局容器（`status_bar.row1/row2/gutter`、
  `shell.header`、`data_page.toolbar/export_menu`、`inspector.header`、`tabs.strip/query/query.results`）：
  物理删会破坏 `parent` 层级校验（脚本要求 `id` 以 `parent + "."` 开头）且 §14.3 禁止改已发布 id，
  故**不删、不标退役**，仅在各自 `impl_notes` 记「待下沉」；
- `icons.toml` 的 `object.sequence` / `object.function` 有真实代码语义但注册表未引用，属覆盖缺口；
  本轮只删了无引用且无代码语义的 `sort.ascending` / `sort.descending` 两条重复登记；
- `shell.toolbar.tables/views/functions/queries` 与 `tabs.strip.new` 在 Windows 端仍未接线，
  其 `sync.windows` 保持 `partial` 并补「反证锚点」（指向该能力在 main 上确实不存在的代码行），
  不改成 `na`——§9.1 的「不渲染」裁决尚未落实，不能把未落实写成已落实。

**未合并 PR 的「合并后需同步」清单**（#176–#179 尚未合入基线 `a84560f`，其行为不写入本契约）：
- 删除行已在 PR #177 实现，合并后需同步 §9.2「Windows 端表数据面（#173）」里的「未实现：
  新增 / 删除行」一句；
- 其余 PR 合并后，按各自改动核对 §9.2 对应历史行与组件注册表的 `sync.windows` / `interaction.source`。

### 9.2 已知边界（不是偏差，单列方便检索）

「正文」列的 § 是已退役章节的编号，对应组件见 §1–§7 指针。

| 边界 | 现状 | 正文 |
|---|---|---|
| Wayland 下的最小窗口尺寸 | 用 `set_size_request` 声明；X11 实测被抬到 960×640，Wayland 依赖合成器遵守 `xdg_toplevel.set_min_size`，未逐一手测 | §1.1 |
| 快捷键验收方式 | 通过 GApplication 的 accel 表 + 同一 action 的 QA 驱动取证；测试环境没有物理按键注入 | §8.1 |
| 事务失败态 | 与其它三态共用 `TransactionBadge::label()`，未单独截图 | §5.2 |
| 960×640 下状态簇 | 动作行分配约 417px；两段常驻文字收成 24×24 的状态圆点后，整行需求 409 ≤ 分配 417、状态簇自身需求 38（`--qa-layout-audit` 实测，17 项 0 超限）；过程消息仍按省略号收缩，完整文案留在圆点 tooltip 与状态栏；删除 / 放弃的操作结果也只写在 tooltip 末段「最近一次操作：…」，可见过程消息为空（`--qa-expect-status-dot` + 「状态簇」需求 42px 实测，1400 宽） | §5.1 |
| `Alt+N` 与拖拽重排 | 序号按 `AdwTabView::pages()` 实际顺序取，拖拽重排后的映射未实测 | §4 |
| 搜索的取数上限 | 目录层与每个库的搜索都只取第一页（`NODE_PAGE_SIZE = 500`），不翻页；库数量或单个库的命中数超过一页时结果不完整（日志写明「目录层还有下一页」），后续需要按库翻页或服务端子树搜索 | §2.4 |
| Windows 端连接面（#169，2026-11-12 历史快照） | 当时只有连接配置、测试连接、建立会话与对象树连接层；元数据树、查询面、表数据面在那一版尚未实现，相关入口保持禁用并在 tooltip 里逐项说明原因。此后各面已陆续实现，见下面 #171 / #172 / #173 三行 | §7.1 |
| Windows 端元数据面（#171） | 已实现对象树的库 / 分组 / 对象三层与 200 条分页、`Objects` 页三列表格、信息面板三态；仍未实现：表节点下的 `Fields` / `Indexes` / `Foreign Keys` / `Triggers` 四组、工具条四个种类入口的联动、对象树搜索、表数据标签（`Enter` 只同步信息面板）、`PageInfo.total_size` 不可用因而不显示总数 | §7.1 |
| Windows 端查询面（#172） | 已实现查询标签（新建 / 关闭 / `*` 未执行标记 / 上限 8）、等宽多行编辑器 + 行号、选中优先执行、执行 / 取消（同一入口切换）、结果网格（列头 + 行号 + 视口虚拟化 + 横向同步）、200 行一批的 `Execute` + `FetchBatch` 续拉、五态与状态栏行 2 摘要。**已在 Windows CI 构建**：`.github/workflows/ci.yml:317`（windows-latest）、`:335`（`dotnet build`）、`:349`（`xaml_lint` 门禁）。早期「XAML 未变、只在替身类型下过类型检查」是当时的历史验收方式，`main` 上已有真机提交；未合并 PR 的验收结果不写成 main 现状 | §7.1 |
| Windows 端表数据面（#173） | 已实现数据页（打开 / 复用 / 关页、200 行 offset 分页 + 加载更多、NULL 与空串区分、主键列标记、单元格暂存与 `Ctrl+S` / `Ctrl+Z`、事务状态与关页 / 关窗守卫）。**未实现**：排序 / 筛选下推（Windows 前端未接线；服务端能力位 `table_browse_sort_filter_total` 在两个内置驱动里已为 `true`，见 `sqlite/src/driver.rs:65`、`mariadb/src/driver.rs:70`，旧稿「服务端能力位未开」的理由不成立）、新增 / 删除行、导出 CSV、跨标签同库事务提示（在 main 上仍成立；删除行已在 PR #177 实现，合并后需同步本行）；**已知边界**：主键列不可编辑（改主键会改变行身份）、就地编辑只覆盖文本 / 整数 / 小数 / 布尔 / JSON / UUID、数据页复用该连接已有的数据库会话（不另建标签会话）；切密度档会重建外壳，数据页不随快照恢复（查询页按 SQL 文本恢复，表数据页需要重新打开）。**已在 Windows CI 构建**：见 `.github/workflows/ci.yml:317,335,349`；早期「XAML 未变、本机只过真类型探针与纯逻辑探针」是当时的历史验收方式，`main` 上已有真机提交 | §5、§7.1 |
| Windows 端 `Objects` 列头用「名称」 | 与 Linux 的「表名」是同**一列、同一含义**（该列也列视图，「表名」在 Windows 片按 issue 要求写成「名称」）；语料待统一 | §3、§7.1 |
