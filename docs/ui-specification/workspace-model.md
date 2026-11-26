# 工作区模型与区域规范

本文件只剩**版式与区域**：区域划分与布局算术（§2）、平台映射（§8）、不出现与禁用（§9）、
键盘指针（§10）、密度与可访问性（§11）、与原型的有意偏差（附录 A）。

> **行为与组件不在本文件。** 逐组件的行为、样式归属、生命周期与Active 平台同步状态在组件注册表
> [`components/`](components/README.md)（可读产物 [ui-components.md](ui-components.md)）；全局规则
> （快捷键、危险操作、失败文案）在 [ui-behavior-contract.md](ui-behavior-contract.md) §8。
> 冲突时行为按注册表、版式按本文件。

- 结构来源：Figma `v0.2 / Low-Fi / 1200×800`（node `12:1493`，1200×800）；同族帧
  `v0.2`（`7:171`）、`v0.2-backup`（`12:732`）、`oranginal`（`7:452`）不是本规范的依据。
- 配套决策：[ADR 0021](../decisions/0021-workspace-tab-session.md)（标签 ↔ 会话）、
  [ADR 0022](../decisions/0022-table-browse-pagination.md)（分页/排序/筛选/总数）、
  [ADR 0023](../decisions/0023-cross-platform-shell.md)（区域一致与原生控件）。
- 设计原则与六种状态的要求见 [design-principles.md](design-principles.md)；本文件只定义结构，
  不重复原则。

> 原型是低保真结构模型：`LO-FI` 徽章、`CN`/`NQ`/`TB`/`VW`/`ƒx`/`QR` 代号、右下角
> `appendinfo` 注释组都是图例，不是产品元素。附录 A 列出全部有意偏差。

## 1. 基准

| 项 | 值 |
|---|---|
| 参考窗口 | 1200 × 800 |
| 默认窗口 | **按显示器逻辑工作区分四档**，规则与取值见 [scaling-and-window-sizing.md](scaling-and-window-sizing.md) §5.2；1200 × 800 只是参考窗口，不再是默认值（原 1100 × 720） |
| 最小窗口 | 840 × 600 / 1050 × 750 / 1260 × 900 为基准，按逻辑工作区收敛，实际下限受原生控件约束。见 [窗口尺寸 §5.4](scaling-and-window-sizing.md#54-最小窗口尺寸) |
| 界面密度档 | 三档 `compact` 100% / `comfortable` 125% / `large` 150%，数字唯一真值是 [metrics.toml](metrics.toml)；本表以下各行的数字按「compact / comfortable / large」三档给出（100% 档就是参考窗口那一套，原型与截图基线都用它）。切档的入口、可用性守卫与切换后的窗口变化见 [ui-behavior-contract.md](ui-behavior-contract.md) §1.5 |
| 侧栏宽度 | 首选、最小与最大取 `sidebar_width` / `sidebar_min` / `sidebar_max` |
| 信息面板宽度 | 首选、最小与最大取 `inspector_width` / `inspector_min` / `inspector_max` |
| 行高 | 树 24 / 30 / 36；数据行 32 / 40 / 48；表头内容高 `column_header_min_height` 26 / 33 / 39（100% 档实测总高 **30** = 26 内容 + 上下各 2px） |
| 字号 | 标题 13 / 16 / 20 semibold；界面 12 / 15 / 18；数据单元格 13 / 16 / 20；信息面板 10 / 13 / 15；状态栏 11 / 14 / 17 |

## 2. 区域

六个区域自外向内、自上而下：

| 区域 | 高度 | 内容 | GTK 载体 |
|---|---|---|---|
| 01 窗口外壳 | `header_height` | 全宽标题与菜单 | `AdwHeaderBar` |
| 02 主工具条 | `toolbar_height` | 左组：连接 / 新建查询；分隔线；**种类入口**（`Tables` / `Views` / `Functions` / `Queries`，纯文字无图标，与「新建查询」同一套 flat 样式、离分隔线同样 14px）；高亮 = 当前列的种类。右侧为三个布局按钮；工作区外壳建立后可用 | `GtkBox` + flat 按钮（窗口级，挂 headerbar 之下） |
| 03 对象树 | 自适应 | 连接标题、固定搜索、反馈、独立滚动对象树 | 外层 `GtkPaned` 起始侧 |
| 04 工作区 | 自适应 | 标签、业务内容、行操作和分页 | 内层 `GtkPaned` 起始侧 |
| 05 状态栏 | `status_height` | 全宽 SQL 摘要、最后刷新、启动态连接摘要及 MCP 状态 | 窗口底部 `GtkBox` |
| 06 信息面板 | 自适应 | Inspector 内容 | 内层 `GtkPaned` 末端侧 |


注：主工具条的种类入口是纯拉丁文字（`Tables` / `Views` / `Functions` / `Queries`），和中文
「新建查询」同一行混排时，字面会比中文高 2px（Pango 的中文 em 盒更靠下）。按实测用
`.sqarvo-kind label { margin-top: 2px }` 把字下移，**按钮盒子与命中区不变**（仍同高同 y），
这条补偿有单测锁着。

### 2.1 布局算术

分栏以窗口内容实际可用宽度为输入。中央目标宽高、分隔把手及恢复余量见 [metrics.toml](metrics.toml)。
中央宽度等于可用宽度减去可见侧栏与其把手。紧凑档默认首选下，1440 宽分配左 260、中央 888、右 280。
宽 1092 时右栏恰可见，降至 1091 折叠，恢复宽度 1116。右栏折叠时左栏在 786 恰可见，降至
785 折叠，恢复宽度 810。原生控件的自然最小尺寸可限制实际可拖到的窗口大小。

分栏交互与状态见组件注册表 `shell.layout`，持久化见 [窗口尺寸 §7](scaling-and-window-sizing.md#7-持久化)。

## 3. 标签模型

组件与行为见 [`components/tabs.toml`](components/tabs.toml)（标签条、标签项、关闭确认框、查询标签；
#189 新增查询分隔把手 `tabs.query.splitter` 与关闭结果区按钮 `tabs.query.results.close`：新标签结果区
隐藏、编辑器占满，Execute 响应后显示，可拖动分栏，数值见 metrics.toml 的 `query_*` 键）。
查询结果区的行结构（#231 定稿版式，自上而下）：状态行 → 说明行（左＝`tabs.query.results.views` 三页签
「消息 / 摘要 / 结果」，右＝状态圆点 / 导出 ▾ / 关闭 ✕）→ 三页内容（`tabs.query.results.message` /
`tabs.query.results.summary` /「结果」页＝原表格区，多个结果集时页内顶部多一枚选择行
`tabs.query.results.result_picker`）。页签插在既有说明行内、不新增行，页签下方**没有**独立控件行
（原语句切换行已随 `tabs.query.results.statements` 退役）；「结果」页在没有结果集时不出现（页签条只剩
消息 / 摘要）。结果区纵向算术不变：编辑器与结果区仍按 `query_editor_ratio` 分栏、最小高度取
`query_editor_min_height` / `query_results_min_height`（口径见 `tabs.query.splitter`）。
标签 ↔ 会话的模型见 [ADR 0021](../decisions/0021-workspace-tab-session.md)，`Objects` 固定页宽度在
[metrics.toml](metrics.toml)（`objects_min_width`）。本节不再维护第二份行为描述。

## 4. 对象树

见 [`components/object_tree.toml`](components/object_tree.toml)。侧栏宽度的算术在本文
§2.1，行为口径的指针见行为契约 §2。

## 5. 信息面板（Inspector）

见 [`components/inspector.toml`](components/inspector.toml)。表信息承接导航叶节点的索引与触发器分页、
刷新和 SQLite 触发器独立 DDL 操作。宽度与钳制规则在 §2.1 与 metrics.toml。

## 6. 表格浏览

见 [`components/data_page.toml`](components/data_page.toml)（36 个组件）。列宽 / 行高 / 分页模型在
[metrics.toml](metrics.toml) 与 [ADR 0022](../decisions/0022-table-browse-pagination.md)；原 §6.1 的
导出细节已并入 `data_page.export_menu.*`。

## 7. 状态栏

见 [`components/status_bar.toml`](components/status_bar.toml)。`status_height` 为全局状态行目标高度。
中央操作行继续使用原生业务控件。

## 8. 平台映射

区域与语义由公共规范维护，控件与外观遵循平台原生惯例。当前映射覆盖 [Active 前端](../../frontend/platforms.toml)。

| 区域 | Linux（GNOME / GTK4） |
|---|---|
|---|---|
| 窗口外壳 | `AdwHeaderBar` + `AdwWindowTitle` |
| 菜单行 | 不设；用 headerbar 主菜单 + 快捷键 |
| 窗口控制按钮 | 不实现，交给系统 |
| 主工具条 | `GtkBox` 分组按钮 |
| 标签条 | `AdwTabView` + `AdwTabBar`（chrome 行） |
| 状态栏 | 全宽 `GtkBox` |
| 信息面板 | `GtkPaned` 子控件 + `GtkStack` |

主菜单（Linux headerbar）只保留有实现支撑的集合：文件（新建连接 / 新建查询标签 /
退出）、表（导出 CSV）、帮助（关于 Squarvo，含后端版本 / 协议 / PID / 端点文件 / 驱动清单；
「查看日志」在 v0.2.3.2 由后端失效对话框提供）。视图与查询两个子菜单**整项不挂**（不留空菜单）；
「关闭标签」与表的事务 / 刷新 / 新增行 / 删除选中行也不在菜单里——这些动作仍由 `win.*` 注册，
有快捷键的照旧（`Ctrl+W` / `F5`），其余入口在工具条与状态栏。菜单与工具条的可用性走
同一个后端就绪状态：后端未握手时「新建连接 / 新建查询 / 执行 / 取消 / 表动作」全部禁用，
布局按钮在工作区外壳存在时可操作，重置布局位于「视图」菜单。
`Favorites`、`Tools`、`Window` 在 v0.2.3 不出现（无收藏、无工具集、无多窗口）。

## 9. 不出现与禁用

| 元素 | v0.2.3 处理 |
|---|---|
| 导入（`IN`） | 按钮禁用 + tooltip「导入在 v0.2.5 提供」，不弹假对话框 |
| 导出（`EX`） | CSV 已可用：查询结果导出**已加载的行**（不重跑 SQL）；表数据弹出范围选择：**当前页**（默认，只写本页已加载行）或**整张表**（按 offset 逐页取回、写进同一个文件，带进度与取消；取消 / 失败不留半截文件）。JSON / SQL 格式禁用并说明 |
| `More` 菜单 | 只放已实现项；空则隐藏 |
| 结构对比、ER 图、数据同步 | 不出现 |
| 视图切换（`▦ / ▥`） | 面板视图不实现，按钮隐藏 |
| 「断开后端」 | v0.2.3.2 移除（三处：表格工具条 / 查询页 / 启动页）。用户看不到前后端之分；后端失效时只提供「重启 Sqarvo」与「查看日志」 |
| 平铺列表、页脚 `DDL` 按钮 | v0.2.3.2 移除：对象树是唯一入口 |

## 10. 键盘

快捷键的完整语义以 [ui-behavior-contract.md](ui-behavior-contract.md) §8.1 为准（以 Linux
代码为依据）；本文件不再维护第二份列表，避免与实现漂移。

快捷键必须对应可用动作。

## 11. 密度与可访问性

- 紧凑密度通过全局 CSS 实现（行高 24/32、字号 12/13）；Linux 当前启用。
- 紧凑化不得破坏：焦点可达（键盘可走完树 → 标签 → 表格 → 面板）、可见的焦点环、
  最小点击区域 24×24、对比度 ≥ 4.5:1（正文）。
- 表格与树必须能用键盘完成选择与滚动。信息面板在 v0.2.3.2 起常驻，不存在「折叠后焦点
  留在不可见控件上」这一情形。

## 附录 A：与原型的有意偏差

1. 不实现自绘窗口控制按钮，使用系统控件。
2. 不实现窗口内菜单行于 Linux。
3. 表格宽度按布局算术计算（§2.1），不沿用 772。
4. 搜索框与反馈位于导航上部，输入、清除及结果沿用对象树的处理。
5. ~~状态栏行 2 右侧增加「连接 · 事务 · 耗时」~~（v0.2.3.3 删除）：原型只有 SQL 摘要/记录数/
   刷新时间；会话级信息改由标签名 `对象 @ 库` 与表格工具条承担，状态栏行 2 只留 SQL 摘要与
   `Last Refresh`。
6. `LO-FI` 徽章、代号字形、`appendinfo` 注释组不实现。
7. 表头合并成**单行**「列名 · 原始类型」（12px semibold，中点分隔，底色取原类型提示条
   的窗口底色）：原来第二行的类型条带（11px 右对齐）删除，同一信息不再出现两处。
8. 表格工具条为**一行**（v0.2.4 起，状态行并入动作行）：`开始事务`
   （开事务后原位换成 `提交事务 / 回滚事务`）在左，弹性空白，然后**状态簇**
   （spinner / 本次操作的临时消息 / **状态圆点**）紧贴漏斗左侧，最后是 `筛选 / 列显隐 / 导出`
   三个图标。v0.2.3.4 起工具条上的「提交更改」也已移除：行变更提交统一由状态栏行操作组
   「应用（✓）」（`status_bar.row1.ops.apply`，与 `Ctrl+S` 同一路径）承担，在途取消由状态栏
   「取消」承担；「撤销」「删除」更早前也已从这一行移到状态栏 / 右键菜单。三个动作按钮
   共用 `padding: 4px 6px`（按钮盒 36 宽 × 32 高；控件行 `set_height_request(32)` 守住
   上 8 / 下 6 的留白），所以 46 仍等于「内容 32 + 上 8 + 下 6」。「开始事务」有中性底色
   （不是 `flat`，也不用强调色）。**工具条上没有「新增」**：插入行只从状态栏行操作组 `+`
   （`RowOp::Add`）进入。状态圆点 7px
   （≈漏斗墨迹 12 × 11 的一半）、命中区 24 × 24，三态互斥：灰 = 没有未提交的更改、也没有
   打开的事务；`@warning_color` = 事务已打开（与状态行 `.warning` 文字同一个 token）；
   `@accent_bg_color` = 有脏行 / 新行（压过事务态）。原来常驻的「自动提交」事务徽标与
   「脏行 N · 待提交 M 条」两段文字收进它的 tooltip（事务在不在不再另写一句「事务状态：…」），
   所以 960 宽下只有过程消息按省略号收缩，整行需求进一步低于分配（旧版含「提交」按钮时实测
   407 / 分配 417）。
   v0.2.3.3 删除了原「表名 · 排序（按主键升序）」提示行，也删除了按钮行右侧的「重新读取」图标
   （刷新只在状态栏 `⟳`，同一动作只留一个入口）；`--qa-layout-audit` 复核「需求 ≤ 分配」，
   `gridHeight` 因此比两行版多 18px。
9. 原型里的「对象树 / 平铺列表」切换在 v0.2.3.2 **整体删除**（平铺列表没有不可替代的价值，
   且与 `Objects` 页的列表重复）。
10. S2 保留紧凑档 `AdwTabBar` 已观察到的约 47 逻辑像素高度。40 仍为 S6 的设计目标。
    中央区高度由 GTK 按原生标签条与操作行的实际分配给出。固定 `Objects` 页用
    `set_page_pinned(true)` 钉在最左，显示图标且无法关闭。
