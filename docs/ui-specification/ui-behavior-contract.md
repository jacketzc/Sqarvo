# UI 行为契约（Linux 客户端）

本文件是 Sqarvo **UI 行为**的唯一真相：能做什么、什么时候可用 / 禁用、动作的结果（含文案、
错误、取消）、快捷键、以及状态之间的转换。它回答「点下去会发生什么」，不回答「东西放在哪、
多宽多高」。

## 定位、分工与优先级

| 文件 | 管什么 | 不管什么 |
|---|---|---|
| **本文件**（`ui-behavior-contract.md`） | 行为：动作、可用性条件、结果文案、快捷键、状态转换 | 版式、尺寸、视觉原则 |
| [`workspace-model.md`](workspace-model.md) | 版式：区域划分、尺寸、布局算术、标签 / 会话模型的结构 | 具体动作的语义与可用性 |
| [`design-principles.md`](design-principles.md) | 设计原则：视觉层级、自解释、状态可见性等取向 | 单个控件的行为细节 |

**冲突时的优先级**：涉及「行为」的冲突以本文件为准；涉及「版式 / 尺寸」的冲突以
`workspace-model.md` 为准；涉及「设计取向」的冲突以 `design-principles.md` 为准。三份文件
都不得与代码事实矛盾——本文件尤其如此，因为它是行为口径的落点。

**本文件的依据是代码，不是旧文档。** 每条行为都以已完成的 Linux 客户端
（`frontend/linux`，Rust + GTK4/libadwaita）为准。文中源码路径均相对 `frontend/linux/`；
`file:line` 是写作时的位置，重构后会漂移，**函数名是一级定位、行号是辅助**。

**维护规则**：改行为必须**同时**改本文件与代码，在同一个 PR 里成对出现。

- 算行为变更：新增 / 删除动作、改可用性条件、改界面文案、改错误分类、改快捷键、改状态转换、
  改默认值（页大小、上限、阈值）。
- 只改视觉（颜色、间距、字号、图标形状）不需要改本文件，但若改动了「图标表达什么动作」
  就需要。
- 行为变了而本文件没变 = 文档缺陷；本文件写了代码没有的行为 = 契约缺陷，必须先落到
  §9「已知偏差与存疑」并给出产品决策，不得由实现者自行拍定。

**不覆盖**：Windows / macOS 客户端（尚未完成）。三端语义一致性要求见 `AGENTS.md` §2.1
与 ADR 0023；本文件是 Linux 端的现状基线，也是将来另外两端对齐行为的参照。唯一例外是
[§1.6](#16-windows-窗口层168)，那一节记 Windows 窗口层（#168）已经落地的行为，源码路径相对
`frontend/windows/`。

**QA 专用路径不算用户可达行为。** 代码里 `--qa-*` 开关（`qa.rs`）驱动的动作、自动确认、
脚本步骤与断言，只在无人值守验收里存在；本文件只在「用户能走到」的意义上描述行为，
个别地方会注明「QA 走同一条路径」以说明证据来源。

---

## 1. 窗口壳与主工具条

### 1.1 窗口、标题与退出

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 应用启动 | 无 | 先 Loading（居中 spinner + 握手阶段文案），握手成功后**直接落地工作区**（未连接时的启动态外壳），不再停在诊断页。主工具条从窗口构建起就存在：后端未握手时六个按钮全部禁用（见 §1.2） | `src/ui.rs:121`（`activate`）、`src/ui.rs:218-236`（工具条构建与 `set_backend_ready(false)`）、`src/ui.rs:1447`（`show_startup_workspace`） |
| 窗口最小尺寸 | 窗口创建时 | 最小窗口按显示器逻辑工作区**收敛**：`min(960·T, 工作区宽-32) × min(640·T, 工作区高-48)`（T = 密度档，本轮 1.0），由 `window_size::relayout` 算出后交给 `GtkWidget::set_size_request`，GTK 把它转成 toplevel 的最小尺寸（Wayland 走 `xdg_toplevel.set_min_size`、X11 走 WM normal hints）。1080p@200%（逻辑 960×540）与 1366×768@125% 这类屏上窗口不再比屏幕高；`--window-size` 给更小的值时被抬到当次下限并记一行 `[monitor] 目标窗口 … 低于最小窗口 …`，实测尺寸看 `--qa-layout-audit` 的「窗口 WxH」或 `--qa-report-monitor` 的 `[monitor]` 行 | `src/window_size.rs:139`（`relayout`）、`src/window_size.rs:208`（`min_window`）、`src/ui.rs:148`（`activate` 首次 `set_size_request`） |
| 后端握手失败 | 无 | 主区域换成错误页：标题「无法连接后端」、副标题「连接失败」、可操作文案 + 「重试」；标题栏回到 `Sqarvo`。工具条保留在错误页上方并**保持全部禁用**（日志「`[toolbar] 后端未握手：0/6 个按钮可用`」） | `src/ui.rs:3295`（`show_error`）、`src/ui.rs:473-481`（事件泵回退）、`src/shell.rs:705`（`set_backend_ready`） |
| 连接数据库成功 | 有连接结果 | 标题 = `<连接名> - <驱动显示名>`；副标题 = `已连接 · <显示名>（<driver_id>）· <服务端版本>`（版本缺失时写「未知（后端未返回）」） | `src/ui.rs:1414-1448` |
| 切换标签 | 工作区存在 | 标题跟着当前标签变：`Objects` → `<连接名> - <驱动>`；表 / 查询标签 → `<标签标题> (<连接名>)`；副标题不变（连接级信息） | `src/tabs.rs:2069`（`refresh_current`）、`src/tabs.rs:128`（`window_title_for`） |
| 后端断开 | 无 | 标题 `Sqarvo`、副标题「已断开」；**保留已建立的工作区**（已加载的行仍可查看）；所有标签标记失连；状态栏连接名清空、SQL 摘要写「连接已断开」 | `src/ui.rs:413-443`、`src/tabs.rs:2227`（`mark_disconnected`） |
| 后端失效（进程没了） | 任一标签的横幅「重启 Sqarvo」按钮，或数据页错误横幅 | `AdwAlertDialog`「Sqarvo 需要重启」+ 说明「重启会关闭所有标签页，未提交的更改会丢失」；按钮：取消 / 查看日志 / 重启 Sqarvo（破坏性，默认取消） | `src/ui.rs:956`（`show_backend_failure_dialog`） |
| 重启 Sqarvo | 同上 | 不新开进程：释放工作区与全部标签会话 → 清后端会话登记 → 回到 Loading「正在重启 Sqarvo…」→ 重新握手 | `src/ui.rs:982`（`restart_software`） |
| 关闭窗口 | 标题栏按钮 / `Ctrl+Shift+W` | **先过脏行守卫**：任一表数据标签有未提交的更改（脏行 / 标记删除 / 未提交的新增行）时**不关窗**，弹「有未提交的更改」确认框（逐表列出，见 §8.2）；选「取消」什么都不做，选「放弃更改并退出」才继续。确认过一次后（标志位 `close_confirmed`）再关不再二次弹窗；已进入关闭流程（`closing == true`）时直接放行。继续后显示「正在停止后端并清理端点文件…」，等 worker 回报 `ShutdownComplete` 才销毁窗口 | `src/ui.rs:352`（`connect_close_request`）、`src/ui.rs:886`（`dirty_tab_summaries`）、`src/ui.rs:899`（`confirm_close_with_dirty`）、`src/tabs.rs:2555`（`close_current`）、`src/data_page.rs:1562` |
| 退出应用 | `Ctrl+Q` / 菜单「文件 → 退出」 | 与关闭窗口同一条守卫：有窗口时先 `window.close()`（走上面那行 `close_request`，脏行未提交会先弹确认框），窗口关完后 GApplication 自然退出；没有窗口时才直接 `app.quit()`。两种情况 `app.connect_shutdown` 都会 `worker.shutdown()` + `join()`，子进程不会变成孤儿 | `src/main.rs:285-305`（`install_app_actions` / `quit_app`）、`src/ui.rs:702`（`connect_shutdown`） |

### 1.2 主工具条

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 「连接」 | 后端已握手；未握手时 `sensitive=false`（tooltip「后端尚未就绪：正在连接本地后端，握手完成后才能新建连接」） | 打开连接对话框；对话框已打开时只把它前置（不叠第二个）。禁用时不发动作，只在处理器里留一条日志兜底 | `src/shell.rs:705-736`（`set_backend_ready`）、`src/ui.rs:989-994`（`open_connection_dialog_for` 的兜底判定） |
| 「新建查询」 | 后端已握手**且**已连接（工作区存在）；任一条件不满足时 `sensitive=false` | 新建查询标签（§4）。未连接时 tooltip「尚未连接数据库：先连接再新建查询标签」；后端未就绪时「后端尚未就绪：握手完成且连接数据库后才能新建查询标签」；启用时为「新建查询标签（Ctrl+T）」 | `src/shell.rs:738-770`（`apply_new_query` / `set_new_query_enabled`）、`src/ui.rs:2272`（工作区建立时打开） |
| 种类入口 `Tables` / `Views` / `Functions` / `Queries` | 后端已握手；未握手时四个按钮 `sensitive=false`（tooltip「后端尚未就绪：握手完成后可用」） | 见 §1.3；tooltip 分别说明「在 Objects 页列出当前库的…」/「函数列表：驱动尚未提供（点开看说明）」/「已保存的查询：尚未实现（点开看说明）」 | `src/shell.rs:604-638`（构建）、`src/shell.rs:720-735`（就绪开关）、`src/ui.rs:964`（`request_kind_entry`） |
| 工具条高亮 | 随当前标签刷新 | 当前标签是表数据标签 → 该表的种类；`Objects` 页 → 页面正在列的种类；查询标签 → 无高亮。高亮实现为给对应按钮加 `sqarvo-kind-active`，同一时刻最多一个 | `src/ui.rs:1719-1763`（`apply_tab_status`）、`src/shell.rs:801`（`set_active`） |
| 可用性日志 | 每次就绪状态变化 | `[toolbar] 后端未握手：0/6 个按钮可用` / `[toolbar] 后端已就绪：6/6 个按钮可用`（数字由六个按钮的真实 `sensitive` 统计；启动态「新建查询」禁用时为 5/6） | `src/shell.rs:683-702`（`log_availability`） |

工具条是**窗口级**控件（`ui.rs` 在 headerbar 之下创建），不随工作区重建：窗口构建后、拿到
worker 之前先 `set_backend_ready(false)`；握手完成置 `true`；后端失效 / 退出 / 重启回退
`false`。菜单侧走同一个状态（§1.4），两边不再各写一套判定。

### 1.4 菜单栏动作与可用性

菜单栏与工具条是**同一批动作的两个入口**，可用性走同一个后端就绪状态
（`Toolbar::set_backend_ready` 与 `WindowActions::set_backend_ready` 同时调用，
`src/ui.rs:4058-4125`），不再出现「菜单禁用、按钮可点」。

| 菜单项 | 前置状态与可用性 | 结果 | 源码依据 |
|---|---|---|---|
| 文件 → 新建连接 | 后端已握手才可点（未就绪时禁用，与工具条「连接」同一条判定） | 打开连接对话框；已打开时只前置，不重复创建 | `src/ui.rs:4375`（菜单项）、`src/ui.rs:4423-4447`（`set_backend_ready`）、`src/ui.rs:4505`（`win.new-connection` 接线）、`src/ui.rs:989-994` |
| 文件 → 新建查询标签 | 后端已握手**且**工作区存在（`set_workspace_ready` 统一开关）；否则禁用 | 与工具条「新建查询」、标签条 `＋`、`Ctrl+T` 同一条 `TabSet::new_query_tab` | `src/ui.rs:4376`（菜单项）、`src/ui.rs:4472-4497`（`set_workspace_ready`）、`src/ui.rs:4511`（`win.new-tab` 接线） |
| 文件 → 退出 | 任意 | 与 `Ctrl+Q` / 关窗同一条守卫：先 `window.close()`（脏行确认照旧），再走清理流程 | `src/ui.rs:4377`（菜单项）、`src/main.rs:285-305`（`install_app_actions` / `quit_app`） |
| 表 → 导出 CSV | 后端已握手**且**工作区存在；表数据页 / 查询结果页是否有可导出内容由动作自己判定 | 与表工具条 `EX`、`Ctrl+Shift+E` 同一条导出入口 | `src/ui.rs:4382`（菜单项）、`src/ui.rs:4702`（`win.export-csv` 接线）、`src/ui.rs:4472-4497`（工作区开关） |
| 帮助 → 关于 Squarvo | 任意（不依赖后端） | 自绘对话框：版本、协议、后端诊断字段 | `src/ui.rs:4388`（菜单项）、`src/ui.rs:4714`（`win.about` 接线）、`src/ui.rs:4814`（`show_about`） |
| 视图 → 界面密度 | 任意（不依赖后端）；当前显示器放不下的档禁用 | 三个单选项（行为见 §1.5），当前档打勾、选中即切档。**这一节是自绘控件**：`GMenu` 没有逐项 tooltip 与逐项禁用，所以菜单里只放一个占位项（`id=tier-section`），弹出时用 `PopoverMenu::add_child` 换成 `GtkCheckButton` 组；视图子菜单只为它而存在 | `src/ui.rs:4485-4499`（`build_main_menu` 的占位项与 `append_submenu`）、`src/ui.rs:5048`（`build_tier_menu`）、`src/ui.rs:5097`（`ensure_tier_menu`）、`src/ui.rs:2989`（QA 截图走同一路径） |
| 查询 / 「关闭标签」/ 表的事务与行操作 | — | **不在菜单里**（菜单裁剪）：查询子菜单整项不挂（不留空菜单）；「关闭标签」与表的事务 / 刷新 / 新增行 / 删除选中行只保留 `win.*` 动作——有快捷键的照旧（`Ctrl+W` / `F5`），其余由工具条、状态栏入口触发，可用性与工具条同一条口径。见 §8.1 的「只保留快捷键」行 | `src/ui.rs:4471-4499`（`build_main_menu`）、`src/ui.rs:4625`（`install_window_actions`）、`src/ui.rs:293-329`（快捷键绑定） |

### 1.3 四个种类入口的联动

目标库的解析规则（四个入口共用）：**树里当前选中项所在的库**——选中的表 / 视图 / 分组也算；
只有什么都没选中时才退回活动连接的默认库。多连接下按「会话 ID + 库名」精确匹配，避免同名
`main` 跳到别的连接。

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 点 `Tables` | 树里能定位到库 | 在树里**一次选中**该库的 `Tables` 分组行（库未展开先展开），切到 `Objects` 页并按 `filter.kinds=[TABLE]` 拉第一页；信息面板随树选中切换；工具条高亮 `Tables` | `src/ui.rs:1771-1835`（`show_objects_for_kind`）、`src/tree.rs:3120`（`select_kind`） |
| 点 `Views` | 同上 | 同上，分组行为 `Views`，`filter.kinds=[VIEW, MATERIALIZED_VIEW]` | 同上、`src/tree.rs:265` |
| 点 `Functions` | 同上 | **树里照样选中 `Functions` 分组行**（联动两向都要通）；`Objects` 页写「函数列表尚未实现」+ 说明「驱动没有提供 Functions 列表（契约里没有该种类的拉取能力），所以这里不摆假数据。」；**0 次 RPC** | `src/ui.rs:1806-1824`、`src/tabs.rs:478-490`（`unavailable_copy`） |
| 点 `Queries` | 同上 | 同上，文案「已保存的查询尚未实现」+「本版没有「已保存的查询」这一层：查询只存在于打开着的查询标签里。」 | 同上 |
| 定位不到库（例如连接还没展开、树里没有库） | 后端已握手，入口可点 | `Functions` / `Queries`：切到 `Objects` 页并显示未实现说明（不静默）；`Tables` / `Views`：**只写日志，界面不变化**（见 §9） | `src/ui.rs:1826-1840` |
| 反方向联动（树 → 页） | 树里选中分组 / 表 / 视图 / 库 | `Objects` 页换作用域（§3），工具条高亮在同一次刷新里跟上 | `src/ui.rs:1752-1764`、`src/ui.rs:1027-1060`（`objects_target`） |
| 启动态（未连接）点任一种类入口 | 后端已握手、没有数据库连接 | 不编造内容：把固定 `Objects` 页摆到前面并**保持空态**，日志说明「尚未连接数据库」 | `src/startup.rs:275-284`（`StartupWorkspace::show_kind`） |

### 1.5 界面密度档（100% / 125% / 150%）

三档的**数字**唯一真值是 [metrics.toml](metrics.toml)（启动时解析一次，之后每个尺寸 / 字号 /
行高 / 图标盒都直接取当前档的写死整数，不做「基准 × 倍率」），规则见
[scaling-and-window-sizing.md](scaling-and-window-sizing.md) §2–§4。本节只写行为。

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 启动档位 | 无 | 优先级：`--tier`（QA，取证用）→ `window.json` 的 `tier` → `compact`。日志 `[tier] 启动档位 compact（100%；来源=默认/记忆/QA）`；随后按当前工作区跑一次可用性守卫（下一行） | `src/density.rs:33`（`ensure_startup_tier`）、`src/layout.rs:446`（`Metrics::for_tier`）、`src/ui.rs`（读 `--tier`） |
| 已保存的档位当前放不下 | 该档的 `min_window`（1200×800 / 1440×960）大于当前逻辑工作区 | **回退到下一个可用档**并记一行 `[tier] large 在当前工作区 1092x582 不可用（需要 1440×960），回退 comfortable`；不静默钳到 100%。连 `compact` 都放不下时保留 `compact`，窗口按 §5.4 收敛到工作区 | `src/density.rs:53`、`src/window_size.rs:225`（`available`）、`src/window_size.rs:232`（`first_available_from`） |
| 升一档 | `Ctrl+Plus`（含 `Ctrl+KP_Add`、`Ctrl+=` 别名） | 走 `win.tier-up`：目标档可用就切（下一行）；已经在 `large` 时**什么都不做**并记 `[tier] 已经是最高档（large），Ctrl+Plus 忽略`；目标档在当前显示器放不下时不切换、不写盘，记 `[tier] … 不可用（需要 W×H），未切换` | `src/ui.rs:357-366`（accel 表）、`src/ui.rs:4859`（`tier-up`）、`src/density.rs:78`（`switch`） |
| 降一档 | `Ctrl+minus`（含 `Ctrl+KP_Subtract`） | 同上，走 `win.tier-down`；已在 `compact` 时记 `[tier] 已经是最低档（compact），Ctrl+minus 忽略` | `src/ui.rs:365`、`src/ui.rs:4868` |
| 重置档位 | `Ctrl+0` | 直接切到 `compact`（不是「回退一个可用档」） | `src/ui.rs:366`、`src/ui.rs:4877`（`tier-reset`） |
| 菜单入口 | 主菜单 → 视图 → 界面密度 | 三个单选项，当前档打勾（程序化同步时不触发 action）。当前显示器放不下的档**禁用**，tooltip 写明缺多少：`当前显示器放不下 150%：需要 1440×960 逻辑工作区（当前 1280×698）`；可用项 tooltip：`切换到 舒适（125%）；窗口会按档位一起放大 / 收小` | `src/ui.rs:4977`（`sync_tier_checks`）、`src/ui.rs:5048`（`build_tier_menu`）、`src/ui.rs:5097`（`ensure_tier_menu`） |
| 切档后的界面 | 目标档可用 | **同一次动作里**：① 四套样式表（主壳 / 数据页 / 对象树 / DDL）用同一个 provider 重载；② 窗口按 §5.3 重算并钳到工作区（`[monitor] 档位导致窗口放大：1400x838 → 1888x1000（档位 large）`）；③ 侧栏 / 信息面板按 **compact 归一值 × 新档**换算（`[tier] 面板宽度随档位换算：侧栏 237 → 356，信息面板 280 → 420（compact → large）`），新窗口放不下 600px 表格区时收到各自下限并记日志；④ 已建列的固定宽度、行高、字号与图标按钮命中区按新档重设（只改样式不会动列宽）；⑤ 档位写入 `window.json` | `src/density.rs:78`（`switch`）、`src/theme.rs:123`（`reload`）、`src/window_size.rs:488`（`apply_for_tier`）、`src/shell.rs:591`（`reapply_tier`）、`src/data_page.rs:5432`（`reapply_metrics`）、`src/tabs.rs:2868`（`reapply_tier`） |
| 切档后放不下面板 | 新档的侧栏 + 信息面板把表格挤到 600px 以下 | 侧栏 / 信息面板收到各自下限（`sidebar_min` / `inspector_min`），并记 `[tier] 工作区放不下 large 档的面板宽度：侧栏 / 信息面板收到下限 270 / 360（表格优先保留 600px）` | `src/shell.rs:591`（`reapply_tier`） |
| 面板宽度跨档往返 | 任一档 | 宽度类字段一律按 **compact 归一值**（实际值 ÷ 档位倍率）存，读时乘当前档，所以 `100% → 150% → 100%` 往返不漂移（实测信息面板 280 → 420 → 280、侧栏 237 → 356 → 237） | `src/shell.rs:591`、`src/window_state.rs:199`（`saved_tier`）、`src/window_state.rs:309`（`remember_tier`） |
| 跨档恢复窗口几何 | `window.json` 记的几何属于旧档 | 按新档重算并记 `[monitor] 档位导致窗口放大：1200x800 → 1888x1000（记忆尺寸属于 compact，当前 large）`；几何越界的校验规则不变（§1.1） | `src/window_size.rs:488`、`src/window_state.rs:309` |
| QA 取证 | `--tier compact\|comfortable\|large`（与 `--window-size` 同一次解析） | 只决定起始档；非法值报 `--tier 只能是 compact\|comfortable\|large，实际：…` 并以失败退出 | `src/qa.rs:1786`（`tier()`）、`src/main.rs`（GApplication 选项注册） |

**不做的事**：不用 `Ctrl+滚轮`（滚轮在数据网格与 SQL 编辑器里已有语义）；不提供小于 100%
的档位；切档不重建标签（未提交编辑与滚动位置保留）；系统 DPI 与系统字号不乘进档位
（三个轴分开，见 [scaling-and-window-sizing.md](scaling-and-window-sizing.md) §1）。

### 1.6 Windows 窗口层（#168）

本节只记 Windows 端的窗口层行为（DPI、位置 / 尺寸、最大化恢复、面板拖动、菜单行）。
其余各节仍以 Linux 客户端为基线；源码路径相对 `frontend/windows/`。

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 启动恢复窗口几何 | `window.json` 里有 `window_width/height/x/y` | 记忆尺寸先按当前档位钳到「最小窗口 ≤ 尺寸 ≤ 工作区−32 / −48」，再还原矩形；`maximized=true` 时**先还原这个矩形、再最大化**（还原边界 = 非最大化矩形，所以「还原」按钮回到该矩形而不是最大化尺寸）。记忆点不在任何显示器工作区内（容差 32px）时丢弃几何、改用 §5.2 分档推荐并记 `[layout] 保存的几何 … 已不在任何显示器工作区内`。`[monitor]` 的 `来源=记忆` / `记忆+最大化` | `Sqarvo.Windows/MainWindow.xaml.cs:251`（`ApplyWindowGeometry`）、`:378`（`TryRestoreSavedGeometry`）、`:855`（`SaveGeometry`） |
| 拖动窗口（位置变化） | 任意 | **只记账、不重算**：记下还原边界，并按新显示器刷新最小尺寸。窗口留在用户放下的位置——不会被拉回屏幕中央，也不会被改回推荐尺寸（原来位置变化走推荐尺寸路径，是最刺眼的一条缺陷） | `MainWindow.xaml.cs:640`（`OnAppWindowChanged`）、`:658`（`RememberWindowGeometry`） |
| 用户改窗口尺寸 | 任意（拖边 / 最大化按钮 / 双击标题栏） | 同上，只更新几何记忆，不抢回推荐尺寸；下限由 `OverlappedPresenter.PreferredMinimumWidth/Height`（物理像素）守住，不会出现「设了最小值还能拖更小」 | `MainWindow.xaml.cs:640`、`:355`（`ApplyMinimumSize`） |
| 最大化后关窗 / 退出 | 窗口处于最大化 | `window.json` 写**还原边界**（非最大化矩形）与 `maximized=true`，不写最大化后的屏幕尺寸；下次启动先还原矩形再最大化 | `MainWindow.xaml.cs:855` |
| DPI / 系统缩放变化 | 运行中 | 从 `XamlRoot.Changed` 取 `RasterizationScale`（`DisplayInformation` 在 WinUI 3 抛异常），与窗口重算合并成**同一个入口**：保留当前窗口的 DIP 矩形（只钳到工作区与最小窗口），不居中、不改推荐尺寸；`[monitor] 来源=保留` | `MainWindow.xaml.cs:612`（`OnXamlRootChanged`）、`:139`（`Relayout`）、`:185`（`RelayoutCore`） |
| 显示器 / 工作区变化 | 运行中 | 与上一行同一条路径；最小尺寸按新的逻辑工作区重算 `min(档位最小窗口, W−32) × min(…, H−48)`，窗口仍留在当前 DIP 位置 | `MainWindow.xaml.cs:612`、`:355` |
| 菜单行高度 | 任意档 | 菜单行 = 外壳高 − 系统标题栏基准 × 档位 → 三档 **24 / 30 / 36**（基准 40 DIP 由度量相减推出：compact 外壳 64 − 菜单行 24，代码里没有 40 这个字面量）。系统标题栏是平台事实：实测值写进 `[monitor] titleBar=`，与口径偏差 > 2 DIP 时记 `[chrome] 系统标题栏实测 … DIP` 并按实测报外壳总高——不为了让总高等于度量而裁剪系统区域（裁了会切到窗口按钮） | `Views/ShellView.xaml.cs:544`（`ApplyMetrics`）、`Layout/WindowSizing.cs:89`（`MenuRowHeight`）、`:108`（`MeasuredNonClientDips`）、`MainWindow.xaml.cs:706`（`UpdateChromeMeasurements`） |
| 拖动侧栏 | 任意档 | 侧栏分隔线本身是拖动条（`Thumb`，不引入第三方 GridSplitter）：命中区取 `grid_padding_end`，视觉线取不缩放的 1px 描边；范围 `sidebar_min..sidebar_max`（180–420 / 225–525 / 270–630），越界只钳制不报错；键盘 ←/→ 每次微调 `inspector_gap`；双击回到该档默认宽度 `sidebar_width` | `Views/ShellView.xaml.cs:641`（`OnSidebarSplitterDragDelta`）、`:526`（`ApplyPanelWidths`） |
| 拖动信息面板 | 任意档 | 同上：范围 `inspector_min..inspector_max`（240–520 / 300–650 / 360–780），默认 `inspector_width`；面板常驻，没有折叠入口 | `Views/ShellView.xaml.cs:684`（`OnInspectorSplitterDragDelta`）、`:526`（`ApplyPanelWidths`） |
| 面板宽度写回 `window.json` | 拖动结束 / 键盘微调 / 双击复位 | **松手才写盘**：实际宽度 ÷ 档位系数 → compact 归一值写回，再用 `SidebarWidthFor` / `InspectorWidthFor` 换算回当前档校验并落回界面（因此界面显示的就是下次启动会恢复的值，不会出现「存 630 恢复 520」）；拖动过程中不写盘。诊断行 `[panel] 侧栏 … → 归一 …` | `MainWindow.xaml.cs:520`（`OnPanelWidthsChanged`）、`Storage/WindowState.cs:93`（`Normalize`）、`:97` / `:104`（`InspectorWidthFor` / `SidebarWidthFor`） |
| 面板宽度跨档往返 | 切档 | 归一值 × 新档系数：侧栏 237 → 296 → 356、信息面板 280 → 350 → 420；`100% → 150% → 100%` 不漂移（钳制只发生在拖动越界时，不改写归一值） | `Storage/WindowState.cs:147`（`SelfTest`） |
| 诊断开关 | — | `--report-monitor` 与 `--qa-report-monitor` 等价（别名表归一，规范与 README 两套写法都生效）；`[monitor]` 行给出 `scale` / 工作区（物理与 DIP）/ 窗口（DIP 与物理）/ `来源` / `最大化` / `titleBar`（实测）/ `menuRow` / `侧栏` / `面板`；`--self-test-window` 断言「最小窗口 = 度量真值」「菜单行 24/30/36」「面板归一往返」「1920×1032 三档推荐尺寸」，失败以非 0 退出 | `Diagnostics/AppOptions.cs:25`（`Aliases`）、`Diagnostics/DiagnosticReporter.cs:73`（`MonitorLine`）、`:92`（`SelfTestWindow`） |

已知边界（#168，都是「知道但本轮不扩范围」的）：

- **表格区 600px 优先**：Linux 在面板挤压表格时把侧栏 / 面板收到下限（§1.5 那两行）；Windows
  本轮只按 `sidebar_min..max` / `inspector_min..max` 钳制。600 不在 `metrics.toml` 里，
  要做得先把它变成三端共用的度量，不在本 Issue 范围内。
- **分隔条命中区**取 `grid_padding_end`（3 / 4 / 5 DIP）：`metrics.toml` 没有「分隔条宽度」这个键，
  加键要三端同改并动生成器，本轮先用现有度量。鼠标命中偏窄，键盘 ←/→ 是补偿。
- **菜单行 24 / 30 / 36** 的前提是系统标题栏 = 40 DIP（规范外壳算术 64 = 40 + 24）。
  标题栏高度是系统给的，实测不同时按实测报外壳总高并记诊断，不裁剪系统区域。
- **未在 Windows 上编译 / 运行**：WinUI XAML 无法在 Linux 上编译，本节的「已落地」指代码与
  纯函数断言（`--self-test-window` 在 Linux 上以替身类型编译并执行通过），编译与运行验证命令
  见 `frontend/windows/README.md`。

---

## 2. 对象树

### 2.1 结构、懒加载与分组

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 树结构 | 连接注册表可用 | `My Connections` → 连接（来自 `connections.json`）→ 库 / schema → `Tables` / `Views` / `Functions` / `Queries` → 对象；表 → `Fields` / `Indexes` / `Foreign Keys` / `Triggers`；视图下直接列出其列 | `src/tree.rs:77`、`src/tree.rs:244`、`src/tree.rs:332`、`src/tree.rs:2262-2297` |
| 展开连接 | 已连接 | 发一次目录层请求（列出库），每页 500（`NODE_PAGE_SIZE`），带自己的分页 token；未连接时双击等于「按保存凭据连接」 | `src/tree.rs:2194-2196`、`src/tree.rs:2405-2416`、`src/backend/metadata.rs:39` |
| 展开库 / schema | 已展开的连接 | **0 次 RPC**：只建 4 个静态分组行（`Tables / Views / Functions / Queries`），且**不自动展开**任何分组 | `src/tree.rs:2262-2279` |
| 展开某个分组 | 分组行存在 | 发**一次** `ListNodes(filter.kinds=…)`；该层有自己的分页 token；`Functions` / `Queries` 没有 kinds，不请求 | `src/tree.rs:2428-2464` |
| 展开一张表 | 表节点 | 先出 4 个静态二级分组行（0 次 RPC），展开某组才拉那一组 | `src/tree.rs:2281-2297` |
| 折叠后重新展开 | 任意层 | **复用已读内容，不发请求**（`level_for` 按 `source` 命中已有 `Level` 就复用同一 `ListStore`；收起只换箭头，不作废层）。要看服务端最新数据走右键「刷新连接 / 刷新库 / 刷新这一组」（`F5` 在表标签下刷的是当前表，回到 `Objects` 页时才是整棵树） | `src/tree.rs:2231-2238`（复用）、`src/tree.rs:1144-1155`（收起不作废） |
| 读取中 / 加载更多 | 层在飞 / 还有下一页 | 占位行「读取中…」；「加载更多…」行只在还有下一页时出现 | `src/tree.rs:65-69`、`src/tree.rs:2378-2385`、`src/tree.rs:2516-2522` |
| 空分组 | 服务端返回空 | 说明行按组给文案：「（这一组没有表）」「（这一组没有视图）」「（这一组没有函数）」「（还没有保存的查询）」「（这张表没有字段 / 索引 / 触发器）」 | `src/tree.rs:293-300`、`src/tree.rs:374-381` |
| 未实现的分组 | `Functions` / `Queries` / `Foreign Keys` 展开 | 不发请求，写一行说明：「函数列表尚未实现（点开看说明，不发请求）」「已保存的查询尚未实现（点开看说明）」「驱动未提供外键列表（capability.unsupported）：需要后端增加外键能力后再显示，这里不猜测、不拼装」 | `src/tree.rs:304-310`、`src/tree.rs:385-393`、`src/tree.rs:2428-2445` |
| 读取失败 | 该层请求失败 | 占位行「读取失败…」+ 状态行给出错误可读文案与稳定错误码；与「空」严格区分 | `src/tree.rs:2567-2580`（错误占位行）、`src/tree.rs:2659-2667`（`fail_level`） |
| 侧栏本体 | — | 侧栏**只有对象树**：没有页脚按钮、没有刷新按钮、没有平铺列表、没有分页统计 | `src/ui.rs:1856-1860`、`src/startup.rs:144-148` |

### 2.2 单击 / 双击 / 回车

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 单击任意行 | 行存在 | **只选中**：行进入选中态（选中样式来自 GTK 默认的 `row:selected`，代码只给行加 `sqarvo-tree-row`，没有自定义左侧色条）；驱动信息面板与 `Objects` 页换内容；不展开、不打开、不连接 | `src/tree.rs:1774-1812`、`src/tree.rs:964-966`、`src/tree.rs:1174-1181` |
| 单击「加载更多…」 | 还有下一页 | 选中即继续拉下一页（分页动作不该要求双击） | `src/tree.rs:1787-1793` |
| 双击 / 回车：表或视图 | 行可打开 | 打开表数据标签（走 `open_table`，同一张表复用已有标签）。**可打开优先于可展开**：表虽然有四组子节点，双击仍是打开 | `src/tree.rs:697-705`（`object_activation`）、`src/tree.rs:1856-1860`、`src/tabs.rs:1608` |
| 双击 / 回车：连接 | 连接行 | 已连接 → 展开 / 收起；未连接 → 按保存的凭据连接（`connect_saved`）；凭据缺失 → 明确提示并打开预填对话框；连接中 / 断开中 → 忽略（只写日志） | `src/tree.rs:1845-1854`、`src/ui.rs:845-884` |
| 双击 / 回车：库 / 分组 | 行可展开 | 展开 / 收起（展开库只出现分组行，分组仍收起） | `src/tree.rs:1855`、`src/tree.rs:1858` |
| 双击 / 回车：列 / 索引 / 触发器 | 叶子对象 | 无动作（不发请求） | `src/tree.rs:1859`、`src/tree.rs:703` |
| 行首箭头 | 行可展开 | 单击即展开 / 收起（不需要双击）；箭头按钮不可聚焦 | `src/tree.rs:987`（`set_focusable(false)`）、`src/tree.rs:1130-1142`（点击接线） |
| 选中占位行 | 占位行 | 信息面板收到 `None`（回到空态 / 留白）；不打开任何东西 | `src/tree.rs:1795-1810` |

### 2.3 右键菜单

菜单按**行种类**给条目，不给占位行（占位行右键直接返回）。菜单是贴着点击位置的 `GtkPopover`，
点任一条目先收起菜单再执行动作。

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 连接节点（已连接） | `state.is_connected()` | 条目依次为：`断开连接`、`刷新连接`、`编辑连接…`、`删除连接` | `src/tree.rs:2702-2716` |
| 连接节点（未连接） | 未连接 / 失败 | 条目：`连接（使用已保存的凭据）`、`编辑连接…`、`删除连接` | 同上 |
| `连接（使用已保存的凭据）` | 未连接 | 与左键双击同一条 `connect_saved`：只读 `credential_ref`、不读也不写明文密码；失败会给出可读原因 | `src/tree.rs:2742-2750`、`src/ui.rs:845` |
| `断开连接` | 已连接 | 先弹确认「断开连接？」：说明只关闭该连接的**浏览会话**（对象树 / 搜索 / DDL），数据页用的是另一条会话、已加载数据仍可查看，其它连接不受影响；按钮 取消 / 断开连接（破坏性，默认取消） | `src/tree.rs:2737-2741`、`src/ui.rs:2954-3009` |
| `刷新连接` | 已连接 | 重读**这条连接已经建立的层**（就地清空该层 store 再发第一页，不收起子树）；一层都没建立时不发请求，侧栏底部临时提示行写「这条连接还没有展开过：展开时会重新读取」 | `src/tree.rs:3010`（`reload_connection`）、`src/tree.rs:1829`（`show_hint`）、`src/tree.rs:2917-2942`（`reload_level`） |
| `编辑连接…` | 连接在配置里 | 打开连接对话框并预填**非敏感字段**；已有 `credential_ref` 时密码行留空、复用引用（密码永不回显）；不会自动连接 | `src/ui.rs:785-810`、`src/dialog.rs:579-613` |
| `删除连接` | 连接在配置里 | 二次确认「删除连接？」（正文写明从 `connections.json` 删除、树节点与浏览会话一并消失、其它连接不受影响）；确认后：关会话 → 删配置 → 尽量删密钥条目。配置删除失败 → 提示「删除连接失败」；配置已删但密钥没删 → 提示「配置已删除，密钥条目未删除（<code>）：<detail>」 | `src/ui.rs:1310-1389`、`src/ui.rs:454-472` |
| 分组节点（`Tables` / `Views` / 表下的四组） | 分组行 | `刷新这一组`：已展开（层已建立）→ 重读该组第一页；未展开过（层还没建立）→ **不发请求**，侧栏底部临时提示行写「这个分组还没展开：展开时会重新读取」（4 秒后自行收起），并记日志 `[tree] 分组 <name> 未展开，刷新跳过`。静态分组行层（`Groups` / `TableGroups`）没有服务端内容，直接返回 | `src/tree.rs:2944`（`reload_group`）、`src/tree.rs:1829`（`show_hint`） |
| 库 / schema 节点 | 对象行 | `刷新库`：重读该库下**已展开**的分组与直接子对象层；分组全收起时只清缓存，侧栏底部临时提示行写「这个库的分组都收起着：展开时重新读取」 | `src/tree.rs:3029-3063`（`reload_container`） |
| 表 / 视图节点 | 可浏览对象 | 只有 `打开`（与双击同一条路径）。没有「查看 DDL」——DDL 在信息面板里 | `src/tree.rs:2718-2729`、`src/tree.rs:2790-2794` |
| 其它对象（列 / 索引 / 触发器） | 非可浏览 | 没有菜单（`menu_items` 返回空） | `src/tree.rs:2720-2727` |
| `F5`（菜单里已不列「视图 → 刷新」） | 工作区存在 | 当前标签是表数据页 → 调 `RowOp::Refresh` 刷该表（与状态栏 `⟳` 同一入口：保持排序 / 筛选、回到第 1 页，有未提交更改时被脏行守卫拒绝并说明原因）；查询标签 / `Objects` 页 / 启动态 → 整棵树（全部连接）重建：清已连接连接的层、重读连接层；状态行「正在重新读取对象树…」 | `src/ui.rs:2422-2457`（`refresh_target` / `refresh`）、`src/ui.rs:4582`（`win.refresh` 接线）、`src/tree.rs:1468` |

### 2.4 搜索

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 输入关键词 | 搜索框在**窗口左下角**（状态栏左边沟，与对象树同一列宽；`Ctrl+F` 聚焦），placeholder「搜索连接、库、表...」 | 防抖 **250 ms** 后搜索；空白串不搜索并回到树页。匹配语义是 `LIKE %关键词%`（不做分词）：**连接名**对注册表里的全部连接在客户端做大小写不敏感 contains；**库 / 表 / 视图**只查已打开（已连接、有浏览会话）的连接，且逐库发 `ListNodes` + `filter.name.contains`（服务端下推为 `LIKE '%kw%'`，前端不拼通配符） | `src/tree.rs:2216`（`run_search`）、`src/tree.rs:2304`（目录层）、`src/tree.rs:2406`（逐库搜索）、`src/backend/metadata.rs`（`contains_filter` / `search_request_in`） |
| 回车（`activate` 信号） | 搜索框内有文本 | **无条件重新派发一次搜索**：不要求文本变化、不等 250 ms 防抖——取消待触发的定时器、代次 +1，重新清结果并发请求（同一个关键词再按一次回车也会真的再查一遍）。文本为空 / 全空白时按「停止搜索」处理（回到树页）。输入防抖、清空恢复树与 `Esc` 语义都不变 | `src/tree.rs:2066`（`search_entry.connect_activate`）、`src/tree.rs:2164`（`search_now`）、`src/tree.rs:2216`（`run_search`）、`src/tree.rs:4256`（`qa_press_search_enter`） |
| 停止搜索 / 清空（`Esc` 在搜索框内） | 搜索页 | 清空输入、回到树页；结果行随列表销毁（不残留），结果页里收起过的层级也一并忘掉 | `src/tree.rs:2463`（`clear_search`）、`src/tree.rs:4238`（`qa_clear_search`） |
| 有结果 | 至少一行 | 结果**就是那棵树，只是无关项不出现**（v0.2.3.4）：行结构与树逐控件一致——`[箭头] 图标 名字 行尾文字`，缩进按层 4 + 14×层数（连接 / 库 / 分组 / 表 · 视图），行高 24、行尾文字同一套 `dim-label caption`；连接行的行尾是连接态徽标（已连接 / 未连接…），与树里同一份文案。只有「连接名命中」或「其中有命中」的连接出现，命中的库 / 分组作为祖先一起出现并默认展开；同一库只一行，多个表命中归到同一个 `Tables` 下。**不再有命中高亮**：命中行看起来与树里那一行完全相同。祖先行带箭头，可以像树里一样把这一支收起 / 再展开（收起状态按路径记，重新搜索仍生效）。搜索框下方状态行（单行省略、全文进 tooltip）写「N 个连接 · M 个对象」（M = 命中的库 + 表 / 视图），有连接失败时追加「；K 条连接失败（…）」，没有已打开连接时追加「；没有已打开的连接，只匹配了连接名」 | `src/tree.rs:4492`（`build_search_rows`）、`src/tree.rs:2551`（`render_search_rows`）、`src/tree.rs:4606`（`apply_search_visibility`）、`src/theme.rs:277`（`list.sqarvo-search-list > row`） |
| 命中行（数据态） | 命中行 | 只用于状态行的对象计数与 QA 断言（`--qa-expect-search highlight=`）：界面**不再**画高亮、加粗或强调条，命中行与树里的同一行像素一致（v0.2.3.4 移除了命中样式） | `src/tree.rs:1893`（`search_snapshot`）、`src/tree.rs:4492`（`build_search_rows`） |
| 没有匹配 | 请求成功但为空 | 状态「没有名称包含「<关键词>」的连接、库、表或视图。换个关键词试试。」，与「没有权限」区分 | `src/tree.rs:2430` |
| 没有权限 | 错误码 `metadata.permission_denied` | 「没有权限读取对象列表：<错误>」；搜索状态记为 `Denied` | `src/tree.rs:2430` |
| 其它失败 | 请求失败 | 「搜索失败：<错误>」 | `src/tree.rs:2430` |
| 点一条结果 | 结果页 | **表 / 视图行**：打开该对象的数据标签，与树里双击同一条 `TreeCallbacks::open`（v0.2.4）；侧栏**仍停在结果页**——搜索框、结果行与命中数原样保留，可以接着点下一条，**不**调树内定位（`reveal`）。**库行**：保持原行为，在树里逐层展开祖先并选中（切到树页）。**分组行**（`Tables` / `Views`）：只有行内箭头能收起 / 展开，点行本身没有可点目标、不触发打开。**连接行**：按该连接「点连接」同一条路径（未连接则连接；已连接则展开），并清空搜索框退出筛选 | `src/tree.rs:2082`（`search_list.connect_row_activated`）、`src/tree.rs:2110`（表 / 视图 → `open`）、`src/tree.rs:3432`（`reveal`，只剩「不可打开的对象行」与 QA 的 `--qa-tree-click-result` 用） |

### 2.5 键盘

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| `↑` / `↓` | 树有焦点 | 移动选中（`GtkListView` + `SingleSelection` 的默认行为）；选中变化与鼠标单击同效（面板 / `Objects` 页跟随） | `src/tree.rs:964-966`、`src/tree.rs:1774` |
| `Enter` | 行有焦点 | 等同于双击（`GtkListView::activate` 信号，同一条回调） | `src/tree.rs:1813` |
| `Ctrl+F` | 工作区存在 | 把焦点交给窗口左下角的搜索框（状态栏左边沟；placeholder「搜索连接、库、表...」） | `src/ui.rs:3884-3892`、`src/tree.rs:916` |
| 其余按键 | — | 沿用 GTK 默认（树没有自定义按键控制器） | `src/tree.rs`（无 `EventControllerKey`） |

---

## 3. Objects 固定页

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 固定页存在 | 工作区 / 启动态建立时 | 页签标题 `Objects`、图标 `view-grid-symbolic`、**pinned**（最左、紧凑、无关闭按钮）、不占标签名额 | `src/tabs.rs:1733-1739`、`src/startup.rs:180-190` |
| 关闭 `Objects` | 任何关闭请求 | 拒绝（`Propagation::Stop`）+ 日志「Objects 页固定存在，不可关闭」；页面上没有可点的 × | `src/tabs.rs:2521-2526` |
| 页面结构 | — | 标题「Objects」+ **三列表格**（`GtkColumnView` + 单选；列固定为 `行号` / `表名` / `类型`，表头文本就是这三个词）+ 状态行 + 「加载更多…」按钮；**没有**作用域说明行。渲染与数据网格同一套：行高 32 + 1px 行分隔线、列分隔线、悬停 / 选中用品牌三档色（浅档 `#D8B7D3` hover / 中档 `#A66FA0` 选中，与数据网格同一条规则）、外框同一份 `.sqarvo-table-frame`（2px / 8px 圆角，容器 `overflow: hidden` 裁切首末行底色） | `src/tabs.rs:660-848` |
| 行内容 | 有数据 | 三列各有取值：`行号` = 模型位置 + 1（右对齐、11px monospace、色弱一档，tooltip「第 N 行」）；`表名` = `[种类图标] [名称]`（图标与树共用 `kind_icon`，名称过长省略号）；`类型` = `表` / `视图` / `物化视图`（`TableEntry::kind_label`，次要信息色弱一档）。tooltip「<名称>（<种类>）· 双击打开」挂在表名格上 | `src/tabs.rs:660-770` |
| 列宽 / 列序 | — | `行号` 固定 32（复用 `data_page::ROW_NUMBER_WIDTH`，不可拖）；`表名` 为扩展列（初值 `data_page::COLUMN_WIDTH` = 200，可拖）；`类型` 120（可拖，够放「物化视图」）；列序固定 `行号 → 表名 → 类型`，`set_reorderable(false)`，拖不乱 | `src/tabs.rs:772-800` |
| 单击行 | 行存在 | **只选中**，并同步右侧信息面板；**不反写树的选中**（否则会形成回环并跳走浏览位置） | `src/tabs.rs:891-908`、`src/tabs.rs:982` |
| 双击 / 回车行 | 行存在 | 打开表数据标签（`GtkColumnView::activate` → 与树双击同一条 `open_table` 路径，同一张表复用已有标签） | `src/tabs.rs:909-925` |
| 作用域：选库 / `Tables` / 表内分组 / 表 | 树里选中 | 列出该库的**表**（`filter.kinds=[TABLE]`） | `src/ui.rs:1037-1045`、`src/ui.rs:1056` |
| 作用域：选 `Views` / 视图 | 树里选中 | 列出该库的**视图**（`[VIEW, MATERIALIZED_VIEW]`） | `src/ui.rs:1041-1042`、`src/ui.rs:1047` |
| 作用域：选 `Functions` / `Queries` 分组 | 树里选中 | 页面写未实现说明（§1.3 同一份文案），清空列表与分页 token | `src/ui.rs:1050-1055`、`src/tabs.rs:958-971` |
| 作用域：选连接 / 占位行 | — | 空态页（不是空白，也不是残留上一批行） | `src/ui.rs:1058`、`src/tabs.rs:987-999` |
| 作用域没变 | 同一个库、同一类 | **不重发 `ListNodes`**，行保留、列表不闪；但树选中变化会先清掉行高亮（高亮属于上一次浏览） | `src/tabs.rs:1006-1041` |
| 重新进入本页（从别的标签切回来） | — | 清掉行选中；列表内容与信息面板都不动 | `src/tabs.rs:1176-1180`、`src/tabs.rs:1043-1054` |
| 读取中 | 请求在飞 | 状态行「正在读取…」 | `src/tabs.rs:1039`、`src/tabs.rs:1052` |
| 读取成功 | 有结果 | 状态行「共 N 个对象」；还有下一页时追加「（还有更多）」并显示「加载更多…」 | `src/tabs.rs:1088-1094` |
| 空结果 | 成功但 0 条 | 「（这个库里没有这类对象）」 | `src/tabs.rs:1095-1097` |
| 读取失败 | — | 「读取失败：<headline>（<code>）」；「加载更多…」隐藏 | `src/tabs.rs:1099-1106` |
| 没有浏览会话 | 连接未建立 | 「这条连接没有浏览会话：展开连接后重试」 | `src/tabs.rs:1034-1038` |
| 「加载更多…」 | 还有下一页 | 按上次作用域 + token 继续拉下一页；按钮在有下一页时才显示 | `src/tabs.rs:879-889`、`src/tabs.rs:1089` |
| 空态文案（连接态） | 没有作用域 | 标题「没有选中对象」，说明「在左侧对象树里选中一个库（或它下面的 Tables / Views），这里会列出该库的表或视图。」 | `src/tabs.rs:828-839` |
| 空态文案（启动态） | 未连接 | 标题「没有选中对象」，说明「从左侧选择一个连接开始：双击一条已保存的连接即可连接。」 | `src/startup.rs:172-175` |

---

## 4. 标签

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 表数据标签标题 | 打开表 | `<对象名> @ <catalog>`；catalog 为空时用「默认」（不编造库名、不把连接名塞进标签）；tooltip `<标题> · 标签会话`（会话建立后阶段换成 `会话 <id 前 8 位>`） | `src/tabs.rs:73-80`、`src/tabs.rs:1686-1688`、`src/tabs.rs:2042-2067` |
| 查询标签标题 | 新建查询 | `查询 <N>`（**不带 `@ 库`**；序号自 1 起递增，不复用已关闭的序号）。库与连接改放 tooltip：`查询 1 · 标签会话 · 库 <catalog> · 连接 <连接名>`，会话建立后阶段换成 `会话 <id 前 8 位>`；多连接同时开着各自「查询 1」时靠 tooltip 区分。窗口标题仍是 `<标签标题> (<连接名>)` | `src/tabs.rs:88-90`、`src/tabs.rs:1712-1758`、`src/tabs.rs:2042-2067` |
| 新建查询标签 | 未达上限 | 四个入口同一条路径：工具条「新建查询」、标签条 `＋`、`Ctrl+T`、菜单「文件 → 新建查询标签」；成功后聚焦编辑器 | `src/tabs.rs:1712`、`src/tabs.rs:1765`、`src/ui.rs:979`、`src/ui.rs:4180` |
| 打开表（新建标签） | 未达上限 | 新建标签 + 请求一条**该标签自己的**数据库会话（`Connect`） | `src/tabs.rs:1651`、`src/tabs.rs:1792` |
| 打开同一张表（重复） | 已有同表标签 | 对象名 + catalog + 连接键三者完全一致即复用：只切到已有标签，**不新建标签、不新建会话、不重载数据**；复用检查在标签上限之前，已开满 8 个仍可切过去 | `src/tabs.rs:97-106`、`src/tabs.rs:1617-1624` |
| 标签上限 | 同时打开的「有会话标签」达到 8（`Objects` 不占名额） | 拒绝新建：`AdwAlertDialog`「无法新建标签」+「标签上限 8 个（当前已开 N 个）：请先关闭一个标签再新建」，按钮「知道了」；同一段文案不重复弹 | `src/tabs.rs:58`、`src/tabs.rs:65-67`、`src/tabs.rs:2432-2462` |
| 关闭标签（× / `Ctrl+W`；菜单里已不列「关闭标签」，只保留快捷键） | 非 `Objects` | 同一条 `close-page`：没有脏行、没有未提交事务 → 直接关；否则先弹三选 | `src/tabs.rs:1188-1196`、`src/tabs.rs:2243-2265`、`src/ui.rs:301` |
| 关闭对话框 | 有脏行或有事务 | 标题「关闭标签？」；正文至少一行「关闭「<标题>」会同时断开它自己的数据库会话。」；有脏行追加「该标签有 N 行未提交的更改。关闭会丢失这些更改：可以选择先保存，或直接放弃。」；有事务追加「该标签有未提交事务（<标签>）：关闭将**回滚该标签的事务**，其它标签不受影响。」；默认响应与关闭响应都是「取消」 | `src/tabs.rs:227-253`、`src/tabs.rs:2276-2294` |
| 关闭对话框按钮 | 有脏行 | 「放弃更改并关闭」（破坏性）/「保存并关闭」（建议，有事务时写「保存、提交事务并关闭」）/「取消」；只有事务没有脏行时：「回滚事务并关闭」（破坏性）/「取消」 | `src/tabs.rs:2277-2292` |
| 保存并关闭 | 有脏行 | 先提交脏行 → 若标签有未提交事务再提交事务 → 最后才摘页面；任一步失败则标签保留，并在页内提示原因 | `src/tabs.rs:2343-2363`、`src/tabs.rs:1974-2015` |
| 放弃并关闭 | — | 丢弃本地草稿与新增行，再关页面。被 `close-page` 信号拦下的那次关闭必须用 `close_page_finish(page, true)` 收尾——重发 `close_page()` 会被 libadwaita 丢掉，标签停在半关状态、之后再点关闭毫无反应（v0.2.3.4 修正） | `src/tabs.rs:2542-2578`、`src/tabs.rs:2465-2478` |
| 取消 | — | 标签与会话都保留；这次被拦下的关闭请求同样要收尾（`close_page_finish(page, false)`），否则页面停在「正在关闭」，后续关闭请求全部失效 | `src/tabs.rs:2543-2548`、`src/tabs.rs:2465-2478` |
| 重复关闭请求 | 该标签的确认框已在显示 | 忽略这次请求并记一行「关闭标签 `<id>` 的确认框已在显示」，不叠第二个框；用户答完（或关掉）那个框后再点关闭才会重新弹 | `src/tabs.rs:2408-2445` |
| 标签被摘掉 | 页面已 detach | 释放该标签会话（worker 侧：Cancel 在途查询 → Close 游标 → Disconnect）；同时收起该标签的筛选面板；若没有当前标签则选中第一个 | `src/tabs.rs:2368-2400` |
| 切换标签 | 点击标签 | **不重建会话**、不发请求；标题 / 状态栏 / 工具条高亮刷新；信息面板按新标签恢复「DDL / 表信息」记忆；其它标签的筛选面板被收起 | `src/tabs.rs:1175-1186`、`src/tabs.rs:1920-1932`、`src/inspector.rs:761-775` |

---

## 5. 表数据页

### 5.1 单行工具条与按钮

工具条**只有一行**（v0.2.4：状态行并入动作行）：动作按钮 → 弹性空白 → **状态簇** → 次要动作。
按钮顺序固定：
`提交` `撤销` `删除` `开始事务`（开事务后原位换成 `提交事务` `回滚事务`）──弹性空白──
[spinner · 过程消息 · ●] `筛选` `列显隐` `导出`。
工具条上**没有「新增」**（v0.2.4 删除）：插入行的入口是状态栏行操作组 `+`
（`RowOp::Add`）——菜单里不再列「新增行」，`win.add-row` 仍注册但没有快捷键。
四个动作按钮共用一套内距（`padding: 4px 6px`，按钮盒 36 宽 × 32 高；v0.2.4 前是
32 宽 × 28 高）；「开始事务」不挂 `flat`、也不加 `suggested-action` / `destructive-action`，
因此与其它三个一样有中性底色，但不用强调色。
状态簇紧贴漏斗**左侧**、与图标按钮同一垂直中线；全行 32 + 上 8 / 下 6 留白（版式归
`workspace-model.md` §2.1；控件行 `set_height_request(32)` 把上下留白守住）。
`●` 是**状态圆点**：7px 实心圆（≈漏斗墨迹 12×11 的一半），
命中区仍是 24×24；三态互斥、优先级自上而下：灰 = 没有未提交的更改、也没有打开的事务；
`@warning_color`（与状态行 `.warning` 文字同一个 token，Yaru 浅色下 `#9f4700` 的棕）=
事务已打开（含只读 / 失败态）；`@accent_bg_color`（系统强调色，Yaru 下橙红——刻意保留的语义红，与主操作按钮的
品牌深档 `#5B3256` **不是**同一色系）= 有脏行 / 新行（脏行就压在事务态之上）。原来常驻的两段文字
（`自动提交` 事务徽标、`脏行 N · 待提交 M 条`）**不再占可见文案**，收进圆点的 tooltip：
`<事务状态> · 脏行 N · 待提交 M 条`换行接一行状态说明，随状态实时刷新。
下文各行的「状态行」指的是这一处消息位（不再是独立一行）。

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| `提交` | 可编辑、有待提交变更（脏行或新行）、没有 RPC 在途 | 走风险判定（§8.2）后提交；按钮文案随待提交条数变化（`提交（N）`）。**在途时同一按钮变成「取消提交」且可点**，请求取消后显示「正在取消…」；取消中/已结束不会误触发新提交（幂等） | `src/data_page.rs:717-720`、`src/data_page.rs:4174-4204`、`src/submit_batch.rs:434-444`、`src/data_page.rs:5618-5637` |
| `撤销` | 可编辑且（有脏行或有新行） | 丢弃全部未提交编辑与删除标记，新行占位行一并移除；结果是「已放弃未提交的更改」/「已放弃未提交的更改（含新增行）」——**不进可见过程消息**，只写状态圆点 tooltip 末段「最近一次操作：…」（口径见本节「操作结果收进圆点」一行），动作行保持为空；换表 / 重新读取后该段清空 | `src/data_page.rs:722-724`、`src/data_page.rs:4098`（`discard_changes`）、`src/data_page.rs:5885`（`note_action`） |
| `删除` | 可编辑且有选中行 | 只把选中行**标记为待删除**（不立刻写库），结果是「第 N 行已标记删除（提交前可撤销）」或「已标记删除 N 行（批量提交前可撤销）」——同样**不进可见过程消息**，只写状态圆点 tooltip 末段「最近一次操作：…」（QA 的 `--qa-data-delete-row` 与用户点「删除」同一句）；新行占位行不在删除范围，提示用「放弃更改」 | `src/data_page.rs:4637`（`delete_selected_row`）、`src/data_page.rs:4702`（`delete_note`）、`src/data_page.rs:5136`（`qa_delete_row`） |
| `新增行`（**不在工具条、也不在菜单里**：唯一入口是状态栏行操作组 `+`） | 可编辑且没有待插入行 | 在末尾插入一行草稿并进入可编辑态：有默认值的列显示数据库默认值原文，自增 / 无默认列保持「省略」（交给数据库生成）；已有待插入行时提示「已经有一行待插入的新行：先提交或放弃它」。入口走 `perform_row_op(RowOp::Add)` → `begin_insert`（`win.add-row` 仍注册，但**没有**绑快捷键、菜单里也不列）；工具条上的同名按钮已在 v0.2.4 删除，`+` 的可用性由 `add_row_available` 判定，不再读某个按钮的 `sensitive` | `src/data_page.rs:2488`（`begin_insert`）、`src/data_page.rs:4261`（`add_row_available`）、`src/data_page.rs:4293`（`perform_row_op`）、`src/shell.rs:914`（状态栏 `+`）、`src/ui.rs:4677`（`win.add-row` 动作） |
| `开始事务` | 没有打开的事务、未断开，**且没有未提交的更改**（脏行 / 标记删除 / 新行草稿） | 打开隔离级别卡片（§5.2）；已有事务时按钮隐藏（不是置灰）。**有未提交的更改时按钮置灰**，tooltip 给原因「还有 N 行未提交的更改：先提交或放弃它们再开事务」；`begin_transaction()` 自己再核一遍同一条件（页内按钮、`win.begin-txn` 动作（菜单已不列）、QA 都走这一条命令），被挡住时状态行写同一句话、不打开卡片。中性动作：有底色但不用 `suggested-action` / `destructive-action`（v0.2.4 起不再是 `flat`）。QA：`--qa-expect-begin enabled\|disabled`（读按钮 `sensitive` 与 `tooltip_text()`） | `src/data_page.rs:4694`（`begin_transaction` 自检）、`src/data_page.rs:5857`（`begin_tooltip`）、`src/data_page.rs:5869`（`update_buttons`）、`src/data_page.rs:4311`（`row_ops.begin_txn` → 菜单）、`src/ui.rs:4519`（`win.begin-txn`） |
| `提交事务` | 有活动事务且**没有脏行** | 提交；有脏行时拒绝并提示「还有 N 行更改未提交：先点「提交更改」或「放弃更改」再提交事务」 | `src/data_page.rs:4458-4482` |
| `回滚事务` | 有活动事务（含失败态） | 先确认「回滚事务？」（正文在脏行时写「回滚事务将丢弃数据库侧的变更，并放弃 N 行未提交的界面更改。」）；确认后回滚 | `src/data_page.rs:4485-4530` |
| 事务按钮互斥 | — | 无事务：只显示「开始事务」；有事务（含失败）：只显示「提交事务 / 回滚事务」 | `src/data_page.rs:5688-5701` |
| `筛选`（漏斗） | 驱动声明支持筛选且已打开表 | 打开 / 收起筛选与排序面板（§5.3）；能力位为 false 时按钮**隐藏**（不是点了报错） | `src/data_page.rs:3470-3478`、`src/data_page.rs:1591-1600` |
| `列显隐`（眼睛） | 有列 | 打开页内「列显隐」卡片；只影响渲染，不改变查询与导出口径 | `src/data_page.rs:699-701`、`src/data_page.rs:1524-1590` |
| `导出` | 有已加载行且没有 RPC 在途 | 打开导出流程（§5.7）；tooltip 说明「导出当前页已加载的 N 行为 CSV（只含本页；JSON/SQL 格式在 v0.2.5；Ctrl+Shift+E）」 | `src/data_page.rs:5662-5666`、`src/export_ui.rs:323` |
| 工具条状态簇（漏斗左侧） | — | spinner（有 RPC 在途时可见）+ 本次操作的临时消息（错误 / 进度 / 结果）+ **状态圆点**（7px 实心圆、命中区 24×24）；与动作按钮**同一行**、在漏斗左侧，全部 `valign=Center` 对齐图标按钮。圆点三态互斥：灰 = 没有未提交的更改、也没有打开的事务；`@warning_color` = 事务已打开（含只读 / 失败态，与状态行 `.warning` 文字同一 token）；`@accent_bg_color` = 有脏行 / 新行（压过事务态）；tooltip 是「`<事务状态> · 脏行 N · 待提交 M 条（含 K 个删除）· 新行 1 行`」换行接一行事务状态说明，有已发生的操作结果时再接第三行「`最近一次操作：…`」，随状态实时刷新（QA：`--qa-expect-status-dot`，读控件 tooltip）。读取完成后**擦掉**「正在读取…」，不在这一处重复行数 / 页码 / 总数；960 宽下只有过程消息按省略号收缩，行本身不溢出（`--qa-layout-audit` 复核「表格工具条」与「状态簇」两项：需求 407 ≤ 分配 417） | `src/data_page.rs:854-869`（圆点构造）、`src/data_page.rs:5877`（`sync_status_dot`）、`src/data_page.rs:5722`（`update_dirty_label`）、`src/layout.rs:144`（直径）、`src/theme.rs:252-275`（三态颜色）、`src/ui.rs:4015`（QA 断言） |
| 过程消息的三类归属：操作结果收进圆点 | 任意 | 过程消息位只留两类：① **「正在…」进行中**（`正在提交…` / `正在取消提交（批次 …）…` / `正在读取表数据…`，读取完成后擦掉）——价值在时效；② **warning / error**（权限不足、类型不可编辑、行已标记删除、提交逐行失败…）——用户必须立刻看到原因。**已经发生的操作结果**（`第 N 行已标记删除（提交前可撤销）`、`已标记删除 N 行（批量提交前可撤销）`、`已放弃未提交的更改` / `（含新增行）`）**不占可见文案**：写进状态圆点 tooltip 末段「`最近一次操作：…`」，可见过程消息同时清空（`note_action` 的统一出口）。注记在换表 / 重新读取（`begin_open`）、提交落定（`show_applied`，未取消时）与事务结束丢弃草稿（`show_transaction`）后清空，只描述当前这一屏还成立的结果。**圆点不存在时**（没有打开的表）不凭空造可见文案，只记一行 `[data]` 日志。QA：`--qa-expect-status-dot 标记删除` 读控件 tooltip（实测 `自动提交 · 脏行 1 · 待提交 1 条（含 1 个删除）⏎自动提交：每条语句自成一个事务⏎最近一次操作：第 2 行已标记删除（提交前可撤销）`），同一次运行 `--qa-layout-audit` 的「状态簇」需求 42px（只剩圆点，没有文字） | `src/data_page.rs:5885`（`note_action`）、`src/data_page.rs:6147`（tooltip 第三行）、`src/data_page.rs:4098`（`discard_changes`）、`src/data_page.rs:4637` / `src/data_page.rs:5136`（删除，含 QA 入口）、`src/data_page.rs:3628` / `3903` / `4038`（清空注记） |
| 表头上方徽标行 | 视情况常驻 | 只读 / 无主键 / 视图（种类名）/ 无权限 / 未统计 / 驱动不支持排序筛选；每枚带解释性 tooltip；有任意一枚时整行可见 | `src/data_page.rs:3573-3636` |

### 5.2 事务

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 打开「开始事务」卡片 | 点「开始事务」，且**没有未提交的更改**（脏行 / 标记删除 / 新行草稿：那时按钮置灰、只能放弃更改，见 §5.1） | 卡片标题「开始事务」；列出隔离级别（不支持的项**在开始前**就禁用并给出原因）；「只读」勾选（只读事务不能写入）；按钮「开始事务」/「取消」，取消不开始 | `src/data_page.rs:1055-1100`、`src/data_page.rs:3046`（`open_txn_chooser`）、`src/data_page.rs:2899-3040` |
| 只读事务生效 | 已开始的只读事务 | 横幅常驻说明只读原因；编辑类按钮禁用；状态行 tooltip 给同一原因 | `src/data_page.rs:3400-3406`、`src/data_page.rs:5702-5715` |
| 事务失败 | 服务端返回 `transaction.failed` | 徽标状态转为失败：**只能回滚**；提交按钮禁用；圆点仍为事务棕（失败态可回滚），tooltip 首段「事务失败（只能回滚）」、次段「事务失败：只能回滚」，状态行 tooltip 给同一原因——**不再**另写一句可见文字 | `src/data_page.rs:3903`（`show_transaction`）、`src/data_page.rs:3532`（`read_only_reason`）、`src/data_page.rs:5893-5915`（圆点 tooltip） |
| 事务结束（提交 / 回滚完成） | — | 丢弃尚未提交的界面更改，并重新读取当前表以显示数据库真实状态；圆点从棕 / 红回到灰 | `src/data_page.rs:3915-3933` |
| 事务状态的可见表达 | 任意 | 三处同时成立：① 按钮互斥（开始 vs 提交/回滚）；② 只读 / 失败原因的 tooltip 与横幅；③ 工具条状态簇里**状态圆点**（漏斗左侧）——颜色（事务打开 = 棕，见 §5.1）+ tooltip：第一段与 `TransactionBadge::label()` 同一口径四态：`自动提交` / `事务中` / `事务中（只读）` / `事务失败（只能回滚）`（断开时 `已断开`），第二段是状态说明（`自动提交：每条语句自成一个事务` / `事务中：提交事务之前，改动只在当前会话可见` / `事务失败：只能回滚` …）。圆点不随状态变化消失，只在没有表时隐藏；状态变化（开始 / 提交 / 回滚 / 失败）**不再**在过程消息位写任何可见文字——开事务不再出现「事务状态：事务中」，结束事务也不再出现「事务状态：自动提交」，与圆点重复的说明只留在 tooltip 里；开始事务那句在途提示（`开始事务：{隔离级别} · 读写`）以及「正在提交事务…/正在回滚事务…」在事务状态落定后清除，不停留在动作行上。状态栏不写事务 | `src/data_page.rs:5877`（`sync_status_dot`）、`src/data_page.rs:5849`（`sync_txn_label`）、`src/data_page.rs:3915-3933`（按钮互斥）、`src/data_page.rs:3903`（`show_transaction` 不写可见过程消息）、`src/table_data.rs:1063`（`label`） |

### 5.3 筛选 / 排序 / 列显隐

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 打开筛选与排序面板 | 点漏斗 | 挂在漏斗按钮上的 `GtkPopover`（`autohide=false`），锚点落在面板上沿偏右（0.618）；打开时把**已应用**的筛选与排序复制成草稿 | `src/data_page.rs:1037-1048`、`src/data_page.rs:1591-1620` |
| 关闭面板 | 点漏斗 / `✕` / `Esc` | 未应用的改动**丢弃**（草稿模型）；`Esc` 先收起行内编辑，没有编辑态时才关面板 | `src/data_page.rs:1320-1339`、`src/data_page.rs:1622-1633` |
| 面板标题右侧状态 | — | 草稿 == 已应用 →「已应用」（dim）；否则「有未应用的更改」（warning）+ tooltip 说明「点应用才生效；关闭会丢弃」 | `src/data_page.rs:2130-2148` |
| 添加筛选条件 | 有列清单 | 列 + 算子 + 值 → 条件行；多条件按 **AND** 组合；算子只有符号（`=` `≠` `≤` `LIKE` `IS NULL` 等），`IS NULL` / `IS NOT NULL` 不需要值（输入框禁用并清空） | `src/data_page.rs:1698-1727`、`src/data_page.rs:1353-1366` |
| 编辑条件 | 点条件行 | 就地展开为编辑器；回车或 `✓` 提交，`✗` 或点面板空白处收起；编辑态与只读态同一套外观 | `src/data_page.rs:1812-1935` |
| 排序键 | 有列清单 | 列 + 方向（`asc` / `desc`），支持多列，列表顺序即优先级；**点击列名切换方向**；行尾三个图标按钮分别是 ↑ 上移优先级、↓ 下移优先级、✕ 删除该键 | `src/data_page.rs:2046-2048`（点列名切方向）、`src/data_page.rs:2050-2082`（行尾 ↑/↓/✕） |
| 应用 | 点「应用」 | 有未提交更改 → **拒绝**（状态行「有未提交的更改：先提交或「放弃更改」再翻页/排序/筛选（翻页会替换整页数据）」）；未知列或算子缺值 → 面板内错误 + 状态行 error，不发请求；成功 → 写回唯一状态、回到第 1 页、重发浏览请求、状态行「筛选与排序已应用（N 条条件 · M 个排序键）· 回到第 1 页」。**面板不关闭** | `src/data_page.rs:2152-2204`、`src/data_page.rs:2306-2321` |
| 清空 | 点「清空」 | 只清**草稿**，仍需点「应用」才生效（状态行「条件与排序已清空（点「应用筛选与排序」恢复全表）」） | `src/data_page.rs:2206-2218` |
| 表头排序标记 | 已有排序 | 列标题（单行表头的「列名 · 原始类型」之后）显示升 / 降标记（`▲` / `▼` / `▲2`），与筛选面板共用同一份排序状态。**只作状态展示**，表头本身不参与排序 | `src/data_page.rs:5553`（`refresh_header_titles`）、`src/data_page.rs:546`（`header_title_text`）、`src/browse_view.rs:240-263` |
| 点表头 | — | 表头只有**一行**（「列名 · 原始类型」）：点列头 = **查看列属性**（信息面板切到该列的列页），不改排序；列右缘 6px 内是 GTK 的拖宽把手，落在那里的松开不算点击。排序只在筛选面板里改（v0.2.3.3 产品决定），也不存在 Shift 多列排序的表头路径 | `src/data_page.rs:3387`（`refresh_header_tooltips` 挂手势）、`src/data_page.rs:3430`（`publish_column_info`）、`src/data_page.rs:546`（表头文案） |
| 列显隐卡片 | 点眼睛 | 页内右对齐卡片（不是窗口）：标题「列显隐」+ 复选框清单 + `✕`；说明「隐藏列只影响这张表格的渲染；查询、复制与即将提供的导出口径不变。」；导出**始终包含全部列** | `src/data_page.rs:880-917`、`src/data_page.rs:1524-1590` |
| 驱动能力位 | 打开表时 | `filtering=false` → 漏斗隐藏、列显隐禁用；`sorting=false` → 排序入口不生效；`totals=false` → 不显示总数；不支持排序 / 筛选 / 总数时出现「驱动不支持排序/筛选」徽标 | `src/data_page.rs:3468-3478`、`src/data_page.rs:3619-3626` |

### 5.4 数据网格、编辑与脏行

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 网格结构 | 已打开表 | 行号列固定 32（行号 = offset + 行下标，不是主键）+ **单行表头**（「列名 · 原始类型」，如 `id · INTEGER`；条带 30px = 26 内容 + 上下 2px 内距，底色取原类型提示条的窗口底色 `@theme_bg_color` `#fafafa`，文字与数据行同档）+ 数据行。数据列宽按本页内容自适应：表头文案与已加载值里最长的那个 + 单元格内距，下限 64、**上限仍是原来的固定宽度 200**（超出照旧省略号截断）；拖过列边界 / 双击自适应过的列记为用户宽度，翻页与刷新不再覆写。例：`id` 收成 85、`created_at` 因时间戳 121px 收成 137、超长 `note` 封顶 200 | `src/data_page.rs:5196`（`build_columns`）、`src/data_page.rs:5382`（`auto_size_columns`）、`src/data_page.rs:5354`（`mark_user_sized`）、`src/data_page.rs:81-85`（`COLUMN_WIDTH` / `COLUMN_MIN_WIDTH` / `CELL_PADDING_X`） |
| 表格外框 | 已打开表 / `Objects` 三列表格 | 两块表格**同一份** `.sqarvo-table-frame`：2px `currentColor` 30% 边框 + 8px 圆角（比分隔线 1px / 10% 实一档，仍弱于中档 `#A66FA0` 的选中底色）。容器设 `overflow: hidden`，首行 / 末行的 hover 与选中底色按圆角裁切，四角不会填成方角；线画在容器上，行与单元格不加整圈边框，表头、最后一行 / 一列的边界线与滚动条都不被遮住 | `src/theme.rs:106-109`、`src/data_page.rs:947-951`、`src/tabs.rs:722-723` |
| 网格结构 | 已打开表 | 行号列固定 32（行号 = offset + 行下标，不是主键）+ 表头两行（列名 / 原始类型）；数据列可拖宽，双击列边界按内容自适应，类型条带跟随列宽 | `src/data_page.rs:4942-5045`、`src/data_page.rs:79-81` |
| 表格外框 | 已打开表 / `Objects` 三列表格 | 两块表格**同一份** `.sqarvo-table-frame`：2px `currentColor` 30% 边框 + 8px 圆角（比分隔线 1px / 10% 实一档，仍弱于中档 `#A66FA0` 的选中底色）。容器设 `overflow: hidden`，首行 / 末行的 hover 与选中底色按圆角裁切，四角不会填成方角；线画在容器上，行与单元格不加整圈边框，表头、最后一行 / 一列的边界线与滚动条都不被遮住 | `src/theme.rs:138-141`、`src/data_page.rs:947-951`、`src/tabs.rs:814-825` |
| 行号标记 | 有脏行 / 删除 / 新行 | 普通 = 行号；脏行 = `●N`（「第 N 行 · 有未提交的更改」）；标删 = `✕N`（「第 N 行 · 已标记删除（提交前可撤销）」）；新行 = `＋`（「新行 · 尚未提交（双击单元格填值）」） | `src/data_page.rs:4979-5024` |
| 脏行底色 | 有未提交的更改 | 已修改单元格的底色 = 品牌浅档 `#D8B7D3` 实底 + `@theme_fg_color` 深色文字（成对写 `background` + `color`：选中行里也不继承白字，避免「白字压浅紫」）；行号列的 `●N` 仍是系统强调红（语义色，本轮不动）。同一个浅档也用于事务「自动提交」徽标、View 徽标、列信息高亮与筛选面板条目 | `src/theme.rs:525-543`（浅档四条规则）、`src/data_page.rs:5609-5637`（脏单元格加类） |
| 单击单元格 | — | 只选中行（多选用 `Ctrl` / `Shift`）；不改数据 | `src/data_page.rs:5380-5396` |
| 双击单元格 | 单元格可编辑 | 打开编辑 popover：标题「编辑 <列> · <类型>」、输入框、「设为 NULL（SQL 空值）」勾选、提示行、「应用」/「取消」；**进入编辑时值不被全选，光标停在值的末尾**（`grab_focus_without_selecting` 只聚焦不选中，`set_position(-1)` 把光标放到末尾：双击的目的是改值，全选会让选中底色压住原值、误按一个字符就整段替换；`Ctrl+A` 仍可全选）；应用失败时错误就地显示、popover 不关 | `src/data_page.rs:4825`（`open_editor`）、`src/data_page.rs:4969-4973` |
| 编辑提示 | 列可空 / 不可空 | 可空：「空文本在文本列表示空字符串 ''；要写 NULL 请勾选上方选项」；不可空：「该列不允许 NULL：请输入合法值」且 NULL 勾选禁用 | `src/data_page.rs:4601-4612` |
| 编辑被拒绝 | 只读 / 类型不可编辑 / 行已删 / 已标删 / 截断 / 越界 | 状态行给具体原因，例如「该行已标记删除：撤销删除后才能编辑」「列 X 的值被服务端截断（只收到前缀），禁止编辑：提交截断值会写坏数据」 | `src/data_page.rs:4677-4731` |
| 点列头 | — | 信息面板切到该列的列页（与树上列节点同一个页面，不重发 RPC）；列右缘 6px 内留给拖宽把手，不算点击 | `src/data_page.rs:3387`（手势）、`src/data_page.rs:3430`（`publish_column_info`）、`src/inspector.rs:715-733` |
| 新行编辑 | 有待插入行 | 用另一套编辑器（多一个「省略（交给数据库默认值）」态）；提交时才发 `InsertRows` | `src/data_page.rs:4538-4541`、`src/data_page.rs:2533-2685` |
| 提交顺序 | 有新行 + 脏行 | 先 `InsertRows`（整批原子），成功后才提交脏行；插入失败则一条脏行都不提交（避免半套状态） | `src/data_page.rs:4231-4284` |
| 单批上限 | 变更超过 500 行 | 只执行前 500 条，其余留待下次；横幅「单批上限 500 行：本次只执行了前 N 条，还有 M 条未提交——请再次点「提交更改」续传。」 | `src/data_page.rs:3671-3676` |
| 提交结果 | — | 成功行变干净；逐行失败在工具条下方错误列表里列出（错误码 + 原因）；状态行给汇总 | `src/data_page.rs:3639-3691`、`src/data_page.rs:5743-5780` |
| 取消提交 | 提交在途 | 整批回滚：界面**保留**脏行与编辑文本（不采信任何部分结果），用户可再次「提交」重试 | `src/data_page.rs:4200-4229`、`src/data_page.rs:3646-3656` |
| 刷新（状态栏 `⟳` / 表菜单） | 已打开表、无 RPC 在途、**无未提交更改** | 重读当前表第一页（清空草稿）；有脏行时被 `dirty_guard` 拒绝并说明原因；tooltip 在禁用时附原因 | `src/data_page.rs:4101-4114`、`src/data_page.rs:4139-4150` |
| 滚动到底 | — | **不自动取下一页**（offset 分页互斥）；翻页由分页器驱动 | `src/data_page.rs:1383-1385` |

### 5.5 offset 分页与三态总数

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 页大小 | — | 每页 **500 行**（`TABLE_PAGE_SIZE`） | `src/backend/table.rs:27` |
| 分页器 | 有表标签 | 只有 首页 / 上页 / 下页 / 末页；页码**只读**显示；没有「跳转到第 N 页」。尺寸对齐状态栏行操作里的 `+`（v0.2.3.4）：四个翻页图标都由本仓库提供（`resources/icons/sqarvo-first-page-symbolic.svg`、`…-page-prev-…`、`…-page-next-…`、`…-last-page-…`），统一画在 16×16 盒里、填充路径 + 内孔（描边会被 GTK 的符号重着色按 alpha 阈值放大，两笔在 11px 下糊成实心三角），渲染墨迹约 10–11px 高、与 `+` 同档；按钮命中区 24 高、水平内边距 4px；页码文字 12px（沿用状态栏的 UI 字族，中文走系统 CJK 回退）。首页 / 末页是 `|<` / `>|`：主题的 `«` / `»` 与相邻的 `‹` / `›` 只差一条线，读不出是两档不同的动作 | `src/shell.rs:949-996`、`src/icons.rs`（`FIRST_PAGE` / `PAGE_PREV` / `PAGE_NEXT` / `LAST_PAGE`）、`src/theme.rs`（`.sqarvo-pager button` / `.sqarvo-pager-label`） |
| 状态栏行 1 文案 | 有表 | `第 N 页`；知道总页数时 `第 N / M 页`；有总数时追加 ` · 共 X 行` / ` · 约 X 行`（估计值必须带「约」） | `src/shell.rs:1047-1066`、`src/browse_view.rs:639-646` |
| 总数三态 | 服务端返回 | 精确 `共 N 行`；估计 `约 N 行`；未知则不显示；计数超预算时 `未统计（超出计数预算）` 并出现「未统计」徽标 | `src/browse_view.rs:600-647`、`src/data_page.rs:3612-3618` |
| 末页 | 总数未知 | 「末页」禁用（`can_last` 需要总页数）；点了也不会发请求，状态行「<动作>不可用（没有对应页）」 | `src/browse_view.rs:728-736`、`src/data_page.rs:2285-2291` |
| 读取中再翻页 | 上一页在飞 | 状态行「上一页仍在读取：稍候再翻页」，不发第二次请求 | `src/data_page.rs:2269-2273` |
| 翻页 | 无未提交更改 | 读取目标 offset（状态行「正在读取 offset N …（<动作>）」）；成功后**回到顶部** | `src/data_page.rs:2292-2302`、`src/data_page.rs:3539-3540` |
| 翻页 / 排序 / 筛选的守卫 | 有脏行或新行 | 一律拒绝，状态行「有未提交的更改：先提交或「放弃更改」再翻页/排序/筛选（翻页会替换整页数据）」 | `src/data_page.rs:2306-2321` |

### 5.6 只读、空、错误、断连

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 打开表（加载中） | 点表 / 翻页 | 占位页「正在读取表数据」+「等待服务端返回第一页…」，工具条状态「正在读取表数据…」 | `src/data_page.rs:3410-3446` |
| 空表 | 第一页 0 行 | 占位页「空表」+「该表存在，但当前条件下没有任何行」/「…但目前没有任何行」；offset 越界时「本页没有数据」+「offset 超出总数（服务端如实返回空行）；回首页或刷新再看」 | `src/data_page.rs:3516-3533` |
| 读取 / 提交失败 | 任意失败 | 错误横幅 + 状态行 error + 逐行错误列表；已加载的行保留；进入 `Error` 态 | `src/data_page.rs:3694-3772` |
| 权限不足 | 错误码含 permission | 额外置「无权限」徽标（常驻），不只靠一句状态行 | `src/data_page.rs:3697-3730` |
| 只读（表级 / 视图 / 无主键 / 只读事务） | 服务端标记 | 横幅说明原因 + 「只读」「无主键」「视图」徽标；编辑类按钮禁用，状态行 tooltip 给同一原因（不能只靠禁用暗示） | `src/data_page.rs:3480-3515`、`src/data_page.rs:3578-3604`、`src/data_page.rs:5670-5682` |
| 断连 | 会话丢失或后端断开 | 丢弃未提交更改；横幅「已断开：<原因> 未提交的 N 行变更已丢弃；数据库事务已由后端强制回滚。」（无脏行时省略中间一句）；状态行「已断开 · 已加载 N 行（只读）」；**已加载数据保留** | `src/data_page.rs:3825-3856` |

### 5.7 导出 CSV（`Ctrl+Shift+E`）

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 入口 | 当前标签是表数据页或查询页 | 三个入口同一条路径：工具条导出图标、菜单「表 → 导出 CSV」、`Ctrl+Shift+E`；没有可导出的结果页时只写日志 | `src/ui.rs:3992-4002`、`src/tabs.rs:2537`、`src/data_page.rs:1260-1268` |
| 查询标签导出 | 有已加载行 | 只导**已加载的行**，不重跑 SQL；没有行 → 提示「没有可导出的行」+「当前标签没有已加载的行：先执行查询，再导出。」 | `src/export_ui.rs:323-337` |
| 表标签：选范围 | 已打开表 | 对话框「导出范围」，正文分别说明「当前页」与「整张表」；按钮 取消 / 当前页 / **整张表**；默认「当前页」，整张表必须显式选择 | `src/export_ui.rs:343-416` |
| 表标签：未打开表 | 标签还没有表 | 提示「没有可导出的行」+「这个标签还没有打开表：先在对象树里打开一张表，再导出。」 | `src/export_ui.rs:344-352` |
| 明确知道 0 行 | 计划为空或总数为 0 | 提示「没有可导出的行」+「这张表当前没有可导出的行（或这一页还没加载到数据）。」，不进入文件选择器 | `src/export_ui.rs:398-411` |
| 选择目标文件 | — | `GtkFileDialog`（标题「导出为 CSV」、过滤 `*.csv`、默认文件名）；在文件对话框取消 → 不写文件、不报错 | `src/export_ui.rs:419-451` |
| 确认导出 | 已选路径 | 标题「导出已加载的 N 行为 CSV？」/「导出整张表为 CSV？」；附「公式注入防护」开关（**默认开**，关闭后写原始值）；按钮 取消 / 导出（建议） | `src/export_ui.rs:497-604` |
| 目标已存在 | — | 再确认一次，默认**不覆盖**（默认响应「取消」），按钮 取消 / 覆盖 | `src/export_ui.rs:607-668` |
| 整表导出进行中 | 已确认 | 进度对话框（已写行数 / 截断单元格数）+「取消」；取消或失败不留半截文件与临时文件 | `src/export_ui.rs:938-1100` |
| 当前页导出的数据口径 | — | 写页面**当前的值，含未提交编辑**（对话框里写明）；整表导出写数据库已提交值，本页未提交编辑不在文件中 | `src/export_ui.rs:355-362` |
| 列口径 | — | 导出**全部列**；列显隐只影响渲染 | `src/data_page.rs:1458-1466` |

---

## 6. 信息面板（Inspector）

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 面板存在 | 启动态与连接态 | 常驻显示，宽度可拖、**窗口级记忆**（读 / 写独立的 `window.json`）；没有折叠按钮，也没有 `Ctrl+I` | `src/shell.rs:129-133`、`src/ui.rs:2019-2035`、`src/window_state.rs:1-15` |
| 没有选中对象 | — | 保留面板外壳（标题「信息面板」+ 分隔线），内容区**留白**，不写「没有选中对象」这类文字 | `src/inspector.rs:575-582`、`src/inspector.rs:492-505` |
| 选中连接 | 连接在 / 不在配置 | 在配置：标题 = 连接名、副标题 = 驱动；字段行给驱动、地址（`host:port` 或文件路径）、默认库、用户、TLS、凭据引用（**不含密码**）、服务端版本、延迟、会话数。不在配置：状态页「连接已不在配置里」+ 说明 + 错误码 `config.invalid` | `src/inspector.rs:585-605`、`src/inspector.rs:131-165` |
| 选中库 / catalog | — | 标题 = 库名、副标题「库」；字段给表 / 视图计数，计数只来自**已展开**的分组层（没展开过就不出现，不主动全量统计） | `src/inspector.rs:608-623`、`src/inspector.rs:166-190` |
| 选中表 / 视图 | — | 顶部出现 `[DDL]` / `[表信息]` 两个独立 toggle（默认 DDL，按**标签**记忆选择）；DDL 取自 `GetDdl`；表信息给列、主键、索引、触发器、已加载行数 | `src/inspector.rs:656-670`、`src/inspector.rs:778-805`、`src/inspector.rs:1014-1117` |
| DDL 页 | — | 只显示正文（不折行、横向滚动、滚动条常驻）；`exact=false` 时正文上方有琥珀警示；复制走选中 + `Ctrl+C`；不显示来源徽标 / 复制按钮 | `src/inspector.rs:449-478`、`src/ddl.rs`（`DdlPanel::show_for`） |
| 表信息页 | — | 拿不到的字段**不出现**（不写 `—` / `N/A`）；索引 / 触发器读取失败时保留错误码 | `src/inspector.rs:349-359`、`src/inspector.rs:1188-1216` |
| 选中列（树） | 列节点有父表 | 列页：列名、类型、可空、默认值（四态）、字符集、排序规则；没有父表时状态页「列的详细属性需要知道所属的表」+ 错误码 | `src/inspector.rs:671-695`、`src/inspector.rs:958-1006` |
| 选中列（数据网格列头） | 已打开表 | 同一个列页，用 `GetColumns` 已有模型，不重发 RPC | `src/inspector.rs:715-733` |
| 选中索引 / 触发器 | — | 状态页「不支持」文案 + 稳定错误码（MariaDB 无独立 DDL），不是空面板 | `src/inspector.rs:703-708`、`src/inspector.rs:249-262` |
| 选中序列 / 函数 / 过程 / 类型 | 有 DDL | 只有 DDL 页（无 `[DDL] / [表信息]` 切换） | `src/inspector.rs:696-702` |
| 切换表页 | 点 toggle | 只切面板内的页，不改变标签与树选中；选择记到当前标签 | `src/inspector.rs:778-805` |
| 切标签 | — | 只换「表页选择」的记忆键，**不重发请求**（看的还是同一个对象） | `src/inspector.rs:757-775` |
| 跨标签事务提示 | 同一 catalog 的其它标签有未提交事务 | 面板顶部常驻一行「同一 catalog 的其它标签还有未提交事务：<标签名…>」；没有则整行隐藏 | `src/inspector.rs:807-819`、`src/ui.rs:1489-1514` |
| 面板内容由谁驱动 | — | **只由「选中」事件驱动**（树、`Objects` 页列表、数据网格列头）；鼠标经过不改面板 | `src/ui.rs:1746-1827`、`src/ui.rs:1989-2011` |

---

## 7. 状态栏

| 行为/操作 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| 左边沟 | — | 分栏线左侧整块属于对象树功能区，里面放**对象树搜索框**（v0.2.4 从树顶部搬来）与它下方一行搜索状态；宽度跟随侧栏分栏位置（拖分栏线同步）且搜索框不会把它撑宽，竖线一直延伸到底。状态栏总高 **48**（两行各 24，v0.2.4：56 → 48），搜索框垂直居中、不撑高状态栏 | `src/shell.rs:156-175`、`src/shell.rs:1117`（`mount_search`）、`src/tree.rs:1327`（`search_widget`） |
| 行 1：行操作 | 见下 | 六个图标按钮，tooltip 同时登记为可访问名：新增 / 删除 / 应用 / 撤销 / 刷新 / 取消；尺寸与命中区统一 | `src/shell.rs:798-841` |
| 行 1：各按钮可用性 | — | 与页内按钮**完全一致**（直接读数据页按钮的 `sensitive`，不重复判定）；「刷新」例外，单独按状态判定（已打开表、无 RPC 在途、无脏行）；「取消」只在提交批次在途时可点。全部禁用时每个 tooltip 追加「（当前不可用：<原因>）」 | `src/shell.rs:1119-1135`、`src/data_page.rs:4069-4114` |
| 行 1：无表标签 | `Objects` / 查询标签 / 启动态 | 行操作全部禁用并给原因；分页摘要与翻页按钮整格隐藏；选中数隐藏；`Last Refresh` 隐藏。原因按场景不同：「Objects 页：在对象树里打开表或新建查询标签」/「当前标签是查询标签：没有可编辑的表格」/「未连接：从左侧选择一个连接开始」 | `src/shell.rs:1139-1161`、`src/ui.rs:1537-1560`、`src/startup.rs:246-248` |
| 行 1：选中数 | 有选中行 | 显示 `N selected`（>0 即出现，不只多选）；tooltip「已选中 N 行：Ctrl/Shift 可多选，删除按批量语义提交」 | `src/shell.rs:1078-1085` |
| 行 1：分页器 | 有表标签 | 四个翻页按钮 + 只读摘要（§5.5）；无表时摘要为空且按钮全禁用；整组 tooltip 说明「offset 分页（#118）：首页 / 上页 / 下页 / 末页；页码只读，跳转到远处页码不在本版提供」 | `src/shell.rs:54-56`、`src/shell.rs:848-880`、`src/shell.rs:1046-1067` |
| 行 2：左段 | — | `SQL: <摘要>`（ASCII 冒号）：表标签给当前筛选 / 排序拼出的只读 `SELECT … LIMIT {offset}, {page_size}`（不执行）；`Objects` 页重置为「尚未执行」；查询标签显示最近一次查询摘要；断连时「连接已断开」 | `src/shell.rs:912-917`、`src/shell.rs:1163-1167`、`src/ui.rs:1554`、`src/browse_view.rs:860-883` |
| 行 2：右段 | — | `Last Refresh: <相对时间>`（`<1m` / `Nm` 形式，每 30 秒重算；tooltip 给墙上时间 `HH:MM:SS` 与「offset 分页不保证跨页稳定」提示）；没有刷新过时整格隐藏 | `src/shell.rs:919-922`、`src/shell.rs:1088-1116` |
| 行 2：空态摘要 | **只在没有表标签时** | 才让 `session` 一格出现：连接态显示连接名；启动态显示「未连接」+「驱动 N · 浏览会话 M」（合成一行 `未连接 · 驱动 N · 浏览会话 M`）。有表标签时一律不出现（表名 / 库在标签名里，事务在表格工具条里） | `src/shell.rs:1181-1207`、`src/startup.rs:240-245` |
| 行 2 的排版约束 | — | 文案用**ASCII 冒号**（`SQL:` / `Last Refresh:`）：全角冒号会把中文行盒高度带进来，两段不在同一基线 | `src/shell.rs:909-917` |

### 7.1 Windows 端（#169）实现状态

Windows 客户端在 #169 只落地「连接面」；本文件其余小节的口径仍然适用，下面只记录**这一片已经
落地、以及仍被禁用**的入口，避免把计划当成已实现。

| 入口 | Windows 现状 | 依据 |
|---|---|---|
| 文件 → 新建连接…、工具条「连接」 | 可用性读**同一个**后端状态位（`BackendState.IsReady`）：握手未完成时禁用，tooltip 逐条说明原因（不是统一一句「尚未实现」） | `frontend/windows/Sqarvo.Windows/Views/ShellView.xaml.cs`（`ApplyBackendState` / `WireMenu`） |
| 新建连接对话框 | MariaDB 字段（主机 / 端口 / 用户名 / 密码 / 库 / TLS 五档）；「测试连接」就地展示延迟与服务端版本；失败给「标题 + 后端文案 + 稳定错误码」；密码只进 Windows 凭据管理器，落盘只有 `credential_ref` | `Views/ConnectionDialog.xaml(.cs)`、`Connections/ConnectionForm.cs` |
| 文件 → 新建查询标签、表 → 导出 CSV、工具条「新建查询」与四个种类入口、树搜索框、状态栏行操作 | 禁用，tooltip 逐条说明具体缺口（元数据树 #170 / SQL 编辑器与查询面 #171 / 表数据编辑更后） | 同上（`WireMenu`） |
| 对象树 | `My Connections` → 已保存连接（`connections.json`；徽标 未连接 / 连接中… / 已连接）；连接成功后挂一行「正在加载对象列表…」，库 / 表留待 #170 | `Views/ShellView.xaml.cs`（`RebuildTree`） |
| 状态栏行 2 右格 | `已连接 · 驱动 N · 浏览会话 M`（`GetHealth` 的真实计数，5 秒一轮）；未就绪时 `未连接 · 驱动 0 · 浏览会话 0` | `Workspace/BackendState.cs`（`StatusLine`） |
| 断开 | 健康探测失败即回「未连接」，原因与稳定错误码写在行 2 左格（可选中复制）；不自动重启后端 | `Workspace/BackendSession.cs`（`RefreshHealthAsync`） |

---

## 8. 全局规则

### 8.1 快捷键（以代码为准）

| 动作 | 快捷键 | 语义 | 源码依据 |
|---|---|---|---|
| 新建连接 | `Ctrl+N` | 打开连接对话框（后端未握手时忽略） | `src/ui.rs:293`、`src/ui.rs:4505` |
| 新建查询标签 | `Ctrl+T` | 与 `＋`、工具条「新建查询」同一路径 | `src/ui.rs:300`、`src/ui.rs:4511` |
| 关闭当前标签 | `Ctrl+W` | 非 `Objects`；有脏行 / 事务时进入三选。**菜单里已不列「关闭标签」**，只保留这条快捷键 | `src/ui.rs:301`、`src/ui.rs:4522`（`win.close-tab`） |
| 关闭窗口 | `Ctrl+Shift+W` | 与标题栏关闭按钮同一清理路径 | `src/ui.rs:190-195` |
| 退出应用 | `Ctrl+Q` | 走关窗守卫：先 `window.close()` 主窗口（与标题栏关闭 / `Ctrl+Shift+W` 同一条 `close-request`），有未提交更改时先弹「有未提交的更改」（取消 / 放弃更改并退出），窗口关完后 GApplication 自然退出；只有没有窗口时才直接 `app.quit()` | `src/main.rs:297-320`、`src/ui.rs:377` |
| 下一个标签 | `Ctrl+Tab` | 选中 `AdwTabView` 的下一个标签；最后一个再往后**回绕到第一个**（`Objects` 也在循环里）。`AdwTabView` 自带的同名快捷键已摘掉，只剩这一条路径 | `src/ui.rs:305`、`src/ui.rs:4535`、`src/tabs.rs:2004` |
| 上一个标签 | `Ctrl+Shift+Tab` | 与上一行反向：第一个再往前回绕到最后一个 | `src/ui.rs:306`、`src/ui.rs:4544`、`src/tabs.rs:2004` |
| 直选第 N 个标签 | `Alt+1`…`Alt+9` | 选中第 N 个标签（1 起，`Objects` 算第 1 个）；超出当前标签数时**什么都不做**（不弹框、不崩），只记一行 `[tabs] Alt+3：只有 2 个标签，忽略` | `src/ui.rs:307-309`、`src/ui.rs:4554-4566`、`src/tabs.rs:2024` |
| 刷新当前表 / 对象树 | `F5` | 当前标签是表数据页 → 刷当前表（保持排序 / 筛选、回到第 1 页；脏行守卫照旧）；否则刷整棵对象树。**菜单里已不列「视图 → 刷新」**，只保留这条快捷键 | `src/ui.rs:296`、`src/ui.rs:2422-2457`、`src/ui.rs:4582` |
| 聚焦对象搜索 | `Ctrl+F` | 焦点交给窗口左下角（状态栏左边沟）的对象树搜索框 | `src/ui.rs:298` |
| 导出当前标签 | `Ctrl+Shift+E` | CSV（表标签先选范围） | `src/ui.rs:329`、`src/ui.rs:4702` |
| 执行 SQL | `Ctrl+Enter` | 编辑器内生效：执行整段（不是「当前语句」）。**菜单「查询」整项不挂**，只保留快捷键与查询页按钮 | `src/sql_page.rs:229-245`、`src/ui.rs:4604`（`win.execute`） |
| 取消查询 | `Esc` | 编辑器内生效；幂等（重复点不报错、不发 RPC）。**菜单「查询」整项不挂**，只保留快捷键与查询页按钮 | `src/sql_page.rs:239-245`、`src/sql_page.rs:308-325`、`src/ui.rs:4616`（`win.cancel`） |
| 关闭筛选面板 / 收起条件编辑 | `Esc` | 只在筛选面板内生效：先收起行内编辑，没有编辑态时关面板 | `src/data_page.rs:1320-1339` |
| 列表移动 / 激活 | `↑` `↓` / `Enter` | GTK 默认；`Enter` 等同双击（树与 `Objects` 页三列表格） | `src/tree.rs:1813`、`src/tabs.rs:909-925` |
| 界面密度升一档 | `Ctrl+Plus`（`Ctrl+KP_Add`、`Ctrl+=`） | 见 §1.5：目标档在当前显示器放不下时忽略并记日志，已在最高档时同样忽略 | `src/ui.rs:357-366`、`src/ui.rs:4859` |
| 界面密度降一档 | `Ctrl+minus`（`Ctrl+KP_Subtract`） | 见 §1.5；已在 `compact` 时忽略 | `src/ui.rs:365`、`src/ui.rs:4868` |
| 界面密度重置 | `Ctrl+0` | 直接回到 `compact`（不是「回退一个可用档」） | `src/ui.rs:366`、`src/ui.rs:4877` |

### 8.2 危险操作确认规则

总原则：**可能改动数据或不可逆的操作，按风险给确认；普通查询不加确认**。满足以下任一条件
就必须确认：变更行数 **> 100**，或**包含任何删除**（删除单独警告）。

| 操作 | 触发条件 | 对话框与按钮 | 源码依据 |
|---|---|---|---|
| 提交行变更 | 变更 > 100 行，或含删除（本地判定）；服务端要求 `acknowledge_risk` 时**再次**走同一个对话框 | 标题「确认提交变更？」；正文给更新 / 删除行数与删除警告，>100 行时追加「变更数超过 100 行的风险阈值…」；按钮 取消 / 确认提交（破坏性，默认取消）；取消 →「已取消提交（未发送任何变更）」 | `src/table_data.rs:30`、`src/table_data.rs:481-517`、`src/data_page.rs:4286-4331` |
| 回滚事务 | 有活动事务 | 标题「回滚事务？」；有脏行时正文写明会丢弃数据库侧变更与 N 行界面更改；按钮 取消 / 回滚（破坏性，默认取消） | `src/data_page.rs:4485-4530` |
| 断开连接 | 已连接 | 标题「断开连接？」；说明只关浏览会话、数据页另一条会话仍可看已加载数据；按钮 取消 / 断开连接（破坏性，默认取消） | `src/ui.rs:2954-3009` |
| 删除已保存连接 | 连接在配置里 | 标题「删除连接？」；按钮 取消 / 删除连接（破坏性，默认取消） | `src/ui.rs:1310-1389` |
| 关闭含脏行或事务的标签 | 有脏行 / 有未提交事务 | 标题「关闭标签？」；按钮见 §4（默认取消） | `src/tabs.rs:227-253`、`src/tabs.rs:2270-2294` |
| 关闭窗口（未提交的更改） | 任一表数据标签有未提交的更改：脏行（含标记删除）或「新增行」草稿 | 标题「有未提交的更改」；正文逐表列出（`users：3 行未提交`、`orders：1 行标记删除`，同一表同时有两类时写 `2 行未提交 · 1 行标记删除`），并给出「选『取消』回到工作区提交或放弃」的出口；按钮 取消 / 放弃更改并退出（破坏性，默认取消，关闭 = 取消）；取消 → 窗口保持原样，日志「取消关窗：保持在工作区」；放弃 → 日志「已放弃未提交更改并退出」，随后走 §1.1 的清理流程。确认过一次后不再二次弹窗；查询标签的未提交事务不在本守卫范围内（关标签时仍按上一行确认） | `src/ui.rs:899`（`confirm_close_with_dirty`）、`src/tabs.rs:276`（`close_guard_body`）、`src/data_page.rs:1562`（`dirty_tab_summary`） |
| 覆盖已存在的导出文件 | 目标路径存在 | 再确认一次；默认**不覆盖**（默认取消） | `src/export_ui.rs:607-668` |
| 重启 Sqarvo | 后端失效 | 标题「Sqarvo 需要重启」；正文写明会关闭所有标签页、未提交更改会丢失；重启按钮为破坏性 | `src/ui.rs:956-976` |

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
| 不泄露凭据 | 界面与日志都不得出现密码、`credential_ref` 指向的密钥值、会话 token；`ConnectionDraft` 的 `Debug` 把密码打成 `<redacted>`；错误类型模块明确「不得包含会话 token 或任何凭据」 | `src/backend/connection.rs:7`、`src/backend/connection.rs:243`、`src/backend/error.rs:1-7` |
| 不回显已保存密码 | 编辑已保存连接时密码行永远留空；有 `credential_ref` 就复用引用，提示「密码将从系统凭据库读取」 | `src/dialog.rs:579-613` |
| 不展示堆栈 | 界面只写可操作文案（下一步做什么），不出现 panic / backtrace / 内部结构体；日志可以更详细，但也不含凭据 | `src/backend/error.rs:1-9`、`src/backend/table.rs:792-794` |
| 稳定错误码 | 失败文案形如「<headline>（<code>）」或「<headline>：<detail> [<code>]」；分类不合并成一句「失败」 | `src/backend/table.rs:792-794`、`src/tabs.rs:962-966` |
| 不编造字段 | 拿不到的值不出现；不写 `—` / `N/A` / 空占位冒充；估计值必须写「约」 | `src/inspector.rs:349-359`、`src/browse_view.rs:635-646` |
| 不假装成功 | 未实现能力如实写「尚未实现 / 驱动未提供」，0 次 RPC、不摆假数据；删除配置成功但密钥未删时如实提示 | `src/tree.rs:302-310`、`src/ui.rs:454-472` |

---

## 9. 已知偏差与存疑

**本节只列仍未收口的项**：代码与产品口径尚未一致、或实现留有已知边界的地方。已收口的条目
已从本节删除，其口径直接写在本文件正文相应小节；历史核验快照见
[`ui-behavior-contract.review.md`](ui-behavior-contract.review.md)。

当前**没有**未收口项（v0.2.3.4，2026-11-10 复核；最近一次收口是界面密度档 #160，口径见 [scaling-and-window-sizing.md](scaling-and-window-sizing.md) 与
[ADR 0026](../decisions/0026-ui-scaling-window-sizing.md)，行为写在 §1.5）。发现新偏差时按同样格式追加：
「代码如此 / 文档如此 / 建议」，能当场改的就改掉再删条目，不要把已经改好的留在这一节。

已知边界（不是偏差，正文已写明，列在这里方便检索）：

| 边界 | 现状 | 正文 |
|---|---|---|
| Wayland 下的最小窗口尺寸 | 用 `set_size_request` 声明；X11 实测被抬到 960×640，Wayland 依赖合成器遵守 `xdg_toplevel.set_min_size`，未逐一手测 | §1.1 |
| 快捷键验收方式 | 通过 GApplication 的 accel 表 + 同一 action 的 QA 驱动取证；测试环境没有物理按键注入 | §8.1 |
| 事务失败态 | 与其它三态共用 `TransactionBadge::label()`，未单独截图 | §5.2 |
| 960×640 下状态簇 | 动作行分配约 417px；两段常驻文字收成 24×24 的状态圆点后，整行需求 409 ≤ 分配 417、状态簇自身需求 38（`--qa-layout-audit` 实测，17 项 0 超限）；过程消息仍按省略号收缩，完整文案留在圆点 tooltip 与状态栏；删除 / 放弃的操作结果也只写在 tooltip 末段「最近一次操作：…」，可见过程消息为空（`--qa-expect-status-dot` + 「状态簇」需求 42px 实测，1400 宽） | §5.1 |
| `Alt+N` 与拖拽重排 | 序号按 `AdwTabView::pages()` 实际顺序取，拖拽重排后的映射未实测 | §4 |
| 搜索的取数上限 | 目录层与每个库的搜索都只取第一页（`NODE_PAGE_SIZE = 500`），不翻页；库数量或单个库的命中数超过一页时结果不完整（日志写明「目录层还有下一页」），后续需要按库翻页或服务端子树搜索 | §2.4 |
| Windows 端连接面（#169） | 只有连接配置、测试连接、建立会话与对象树连接层；元数据树、查询面、表数据面尚未实现，入口保持禁用并在 tooltip 里逐项说明原因 | §7.1 |
