# 工作区模型与区域规范

本文件只剩**版式与区域**：区域划分与布局算术（§2）、平台映射（§8）、不出现与禁用（§9）、
键盘指针（§10）、密度与可访问性（§11）、与原型的有意偏差（附录 A）。

> **行为与组件不在本文件。** 逐组件的行为、样式归属、生命周期与三端同步状态在组件注册表
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
| 最小窗口 | `min(960·T, 工作区宽-32) × min(640·T, 工作区高-48)`（T = 密度档；见 [scaling-and-window-sizing.md](scaling-and-window-sizing.md) §5.4，行为见 [ui-behavior-contract.md](ui-behavior-contract.md) §1.1）。100% 档且工作区足够时等于 960 × 640 |
| 界面密度档 | 三档 `compact` 100% / `comfortable` 125% / `large` 150%，数字唯一真值是 [metrics.toml](metrics.toml)；本表以下各行的数字按「compact / comfortable / large」三档给出（100% 档就是参考窗口那一套，原型与截图基线都用它）。切档的入口、可用性守卫与切换后的窗口变化见 [ui-behavior-contract.md](ui-behavior-contract.md) §1.5 |
| 侧栏宽度 | 237 / 296 / 356（可拖拽；可调区间 180–420 / 225–525 / 270–630）。来源 `metrics.toml` 的 `sidebar_width` / `sidebar_min` / `sidebar_max` |
| 信息面板宽度 | 280 / 350 / 420（可拖拽 240–520 / 300–650 / 360–780；v0.2.3.2 起常驻，不可折叠）。来源 `inspector_width` / `inspector_min` / `inspector_max` |
| 行高 | 树 24 / 30 / 36；数据行 32 / 40 / 48；表头内容高 `column_header_min_height` 26 / 33 / 39（100% 档实测总高 **30** = 26 内容 + 上下各 2px） |
| 字号 | 标题 13 / 16 / 20 semibold；界面 12 / 15 / 18；数据单元格 13 / 16 / 20；信息面板 10 / 13 / 15；状态栏 11 / 14 / 17 |

## 2. 区域

六个区域自外向内、自上而下：

| 区域 | 高度 | 内容 | GTK 载体 |
|---|---|---|---|
| 01 窗口外壳 | 64 | 标题栏 40（居中标题 + 右侧控件）+ 菜单行 24 | `AdwHeaderBar`（Linux 无菜单行，见 §8） |
| 02 主工具条 | 44 | 左组：连接 / 新建查询；分隔线；**种类入口**（`Tables` / `Views` / `Functions` / `Queries`，纯文字无图标，与「新建查询」同一套 flat 样式、离分隔线同样 14px）；高亮 = 当前列的种类。**后端未握手时六个按钮全部禁用**（窗口构建即存在，握手完成才启用） | `GtkBox` + flat 按钮（窗口级，挂 headerbar 之下） |

注：主工具条的种类入口是纯拉丁文字（`Tables` / `Views` / `Functions` / `Queries`），和中文
「新建查询」同一行混排时，字面会比中文高 2px（Pango 的中文 em 盒更靠下）。按实测用
`.sqarvo-kind label { margin-top: 2px }` 把字下移，**按钮盒子与命中区不变**（仍同高同 y），
这条补偿有单测锁着。

| 03 对象树 | 644（自适应） | `My Connections` 头 + 多连接树 + 种类分组；树从工具条下方**直接开始**（上面没有搜索框、没有分割线） | `GtkPaned` 左栏，`GtkTreeListModel` |
| 04 工作区 | 644 | 标签条 47（`AdwTabBar` 实测）+ 表格工具条 46（一行：动作按钮 / 状态簇 / 次要动作）+ 表格 + 信息面板 | `AdwTabView` + `GtkStack` + `GtkColumnView` |
| 05 状态栏 | 48 | 分栏线延伸到底，左侧整块属对象树功能区（**对象树搜索框 + 搜索状态**，窗口左下角）；右侧两行（各 24）：行操作/分页器；行 2 只有 SQL 摘要（紧贴行操作正下方）与 `Last Refresh` | `GtkBox`：左边沟（挂搜索框）+ 1px 竖线 + 右列两行 |
| （06 信息面板） | 与表格同高 | 连接 / 库 / 表三种内容 | `GtkBox` + `GtkStack` + `GtkRevealer` |

原型里 05 叫 `06 Status Bar`、信息面板没有独立区域名（占的是注释位）。本规范统一称
**05 状态栏** 与 **信息面板（Inspector）**，避免沿用图例编号。

### 2.1 布局算术

```text
contentHeight = windowHeight - 108 - 48      # 108 = 外壳 64 + 工具条 44（v0.2.3.3：76 → 44）；48 = 状态栏两行各 24（v0.2.4：56 → 48）
contentWidth  = windowWidth  - sidebarWidth
tabStrip      顶高 47                        # AdwTabBar 实测；原型画的是 24
tableActions  顶高 32 + 上 8 / 下 6 = 46      # 一行：动作按钮 / 状态簇（spinner·消息·状态圆点）/ 次要动作
gridHeight   = contentHeight - 47 - 46
gridWidth    = contentWidth - 18（左内边距）- 3（右内边距）- [10（间隙）+ inspectorWidth]
```

1200 × 800：`contentHeight = 644`、`gridHeight = 551`（`= 644 − 47 − 46`）。表格工具条在 960 宽下
最紧：动作按钮保持最小宽度、过程消息按省略号收缩（事务状态与脏行计数的全文在**状态圆点**的
tooltip 里，见 §附录 A 第 8 条），整行需求 409 ≤ 分配 417（`--qa-layout-audit` 实测）；标签条高度
取 `AdwTabBar` 实测值而非原型的 24，见附录 A 第 10 条。

1200 × 800 下的实际宽度：

| inspector | gridWidth | 说明 |
|---|---:|---|
| 240 | 692 | 下限 |
| 280 | 652 | 默认 |
| 320 | 612 | 上限附近 |

信息面板在 v0.2.3.2 起**常驻**，因此没有「折叠 = 942」这一档：表格永远少 280（默认）左右。

原型里的 772 是**为注释图例让位算出来的**，不是需求。7 列自然宽度合计就是 772
（32 + 60 + 80 + 160 + 72 + 152 + 112 + 104），所以在默认 280 面板下表格必然横向滚动或
列宽压缩——这是接受的取舍，验收时必须检查横向滚动条的可用性，不得让列被静默裁掉。

## 3. 标签模型

组件与行为见 [`components/tabs.toml`](components/tabs.toml)（标签条、标签项、关闭确认框、查询标签）；
标签 ↔ 会话的模型见 [ADR 0021](../decisions/0021-workspace-tab-session.md)，`Objects` 固定页宽度在
[metrics.toml](metrics.toml)（`objects_min_width`）。本节不再维护第二份行为描述。

## 4. 对象树

见 [`components/object_tree.toml`](components/object_tree.toml)（25 个组件）。侧栏宽度的算术在本文
§2.1，行为口径的指针见行为契约 §2。

## 5. 信息面板（Inspector）

见 [`components/inspector.toml`](components/inspector.toml)（20 个组件：标题块、四种视图、两页切换、
空态）。宽度与钳制规则在 §2.1 与 metrics.toml。

## 6. 表格浏览

见 [`components/data_page.toml`](components/data_page.toml)（36 个组件）。列宽 / 行高 / 分页模型在
[metrics.toml](metrics.toml) 与 [ADR 0022](../decisions/0022-table-browse-pagination.md)；原 §6.1 的
导出细节已并入 `data_page.export_menu.*`。

## 7. 状态栏

见 [`components/status_bar.toml`](components/status_bar.toml)（22 个组件）。总高 `status_height`、
左边沟与侧栏分栏的宽度联动在 §2.1 与 metrics.toml。

## 8. 平台映射

区域与语义三端一致，控件与外观各自原生。详见 ADR 0023。

| 区域 | Linux（GNOME / GTK4） | Windows（WinUI 3） | macOS（SwiftUI） |
|---|---|---|---|
| 窗口外壳 | `AdwHeaderBar` + `AdwWindowTitle` | 系统标题栏（AppWindow）+ 窗口内菜单行 | 系统标题栏 + 系统主菜单栏 |
| 菜单行 | 不设；用 headerbar 主菜单 + 快捷键 | 窗口内菜单行（原型形态） | 系统菜单栏，窗口内不出现 |
| 窗口控制按钮 | 不实现，交给系统 | 系统 | 系统 |
| 主工具条 | `GtkBox` 分组按钮 | `CommandBar` | `NSToolbar` |
| 标签条 | `AdwTabView` + `AdwTabBar`（chrome 行） | `TabView` | 原生 tab |
| 状态栏 | 自绘两行 `GtkBox` | 自绘 `StatusBar` | 单行细条 + 工具条状态 |
| 信息面板 | `GtkRevealer` + `GtkStack` | `SplitView` 右侧面板 | 右侧 inspector |

主菜单（Linux headerbar）只保留有实现支撑的集合：文件（新建连接 / 新建查询标签 /
退出）、表（导出 CSV）、帮助（关于 Squarvo，含后端版本 / 协议 / PID / 端点文件 / 驱动清单；
「查看日志」在 v0.2.3.2 由后端失效对话框提供）。视图与查询两个子菜单**整项不挂**（不留空菜单）；
「关闭标签」与表的事务 / 刷新 / 新增行 / 删除选中行也不在菜单里——这些动作仍由 `win.*` 注册，
有快捷键的照旧（`Ctrl+W` / `F5`），其余入口在工具条与状态栏。菜单与工具条的可用性走
同一个后端就绪状态：后端未握手时「新建连接 / 新建查询 / 执行 / 取消 / 表动作」全部禁用，
不存在「菜单禁用、按钮可点」的两套口径。信息面板常驻，因此「视图 → 信息面板显隐」
与 `Ctrl+I` 在 v0.2.3.2 移除。
`Favorites`、`Tools`、`Window` 在 v0.2.3 不出现（无收藏、无工具集、无多窗口）；macOS 的
「窗口」菜单由系统提供。

## 9. 不出现与禁用

| 元素 | v0.2.3 处理 |
|---|---|
| 导入（`IN`） | 按钮禁用 + tooltip「导入在 v0.2.5 提供」，不弹假对话框 |
| 导出（`EX`） | CSV 已可用：查询结果导出**已加载的行**（不重跑 SQL）；表数据弹出范围选择：**当前页**（默认，只写本页已加载行）或**整张表**（按 offset 逐页取回、写进同一个文件，带进度与取消；取消 / 失败不留半截文件）。JSON / SQL 格式禁用并说明 |
| `Functions` / `Queries` 分组 | v0.2.3.3 起**只画入口**：分组行在树里可见（带图标、英文名），展开写一行「尚未实现（点开看说明）」，0 次 RPC、不摆假数据 |
| `More` 菜单 | 只放已实现项；空则隐藏 |
| 结构对比、ER 图、数据同步 | 不出现 |
| 视图切换（`▦ / ▥`） | 面板视图不实现，按钮隐藏 |
| 「断开后端」 | v0.2.3.2 移除（三处：表格工具条 / 查询页 / 启动页）。用户看不到前后端之分；后端失效时只提供「重启 Sqarvo」与「查看日志」 |
| 平铺列表、页脚 `DDL` 按钮 | v0.2.3.2 移除：对象树是唯一入口 |

## 10. 键盘

快捷键的完整语义以 [ui-behavior-contract.md](ui-behavior-contract.md) §8.1 为准（以 Linux
代码为依据）；本文件不再维护第二份列表，避免与实现漂移。

macOS 把 `Ctrl` 换成 `⌘`，其余语义不变。快捷键不得引入无对应动作的条目。

## 11. 密度与可访问性

- 紧凑密度通过全局 CSS 实现（行高 24/32、字号 12/13）；只在 Linux 启用，Windows/macOS
  保留系统默认密度，只对齐信息层级。
- 紧凑化不得破坏：焦点可达（键盘可走完树 → 标签 → 表格 → 面板）、可见的焦点环、
  最小点击区域 24×24、对比度 ≥ 4.5:1（正文）。
- 表格与树必须能用键盘完成选择与滚动。信息面板在 v0.2.3.2 起常驻，不存在「折叠后焦点
  留在不可见控件上」这一情形。

## 附录 A：与原型的有意偏差

1. 不实现自绘窗口控制按钮（三端都用系统控件）。
2. 不实现窗口内菜单行于 Linux；macOS 同样不实现。
3. 表格宽度按布局算术计算（§2.1），不沿用 772。
4. 侧栏搜索框 v0.2.4 移到**窗口左下角**（状态栏左边沟），树从工具条下方直接开始；沟里
   不再放「连接过滤」（v0.2.3.3 删除，与搜索语义重复）。
5. ~~状态栏行 2 右侧增加「连接 · 事务 · 耗时」~~（v0.2.3.3 删除）：原型只有 SQL 摘要/记录数/
   刷新时间；会话级信息改由标签名 `对象 @ 库` 与表格工具条承担，状态栏行 2 只留 SQL 摘要与
   `Last Refresh`。
6. `LO-FI` 徽章、代号字形、`appendinfo` 注释组不实现。
7. 表头合并成**单行**「列名 · 原始类型」（12px semibold，中点分隔，底色取原类型提示条
   的窗口底色）：原来第二行的类型条带（11px 右对齐）删除，同一信息不再出现两处。
8. 表格工具条为**一行**（v0.2.4 起，状态行并入动作行）：`提交 / 撤销 / 删除 / 开始事务`
   （开事务后 `开始事务` 原位换成 `提交事务 / 回滚事务`）在左，弹性空白，然后**状态簇**
   （spinner / 本次操作的临时
   消息 / **状态圆点**）紧贴漏斗左侧，最后是 `筛选 / 列显隐 / 导出` 三个图标。四个动作按钮
   共用 `padding: 4px 6px`（按钮盒 36 宽 × 32 高；控件行 `set_height_request(32)` 守住
   上 8 / 下 6 的留白），所以 46 仍等于「内容 32 + 上 8 + 下 6」。「开始事务」有中性底色
   （不是 `flat`，也不用强调色）。**工具条上没有「新增」**：插入行只从状态栏行操作组 `+`
   （`RowOp::Add`）进入。状态圆点 7px
   （≈漏斗墨迹 12 × 11 的一半）、命中区 24 × 24，三态互斥：灰 = 没有未提交的更改、也没有
   打开的事务；`@warning_color` = 事务已打开（与状态行 `.warning` 文字同一个 token）；
   `@accent_bg_color` = 有脏行 / 新行（压过事务态）。原来常驻的「自动提交」事务徽标与
   「脏行 N · 待提交 M 条」两段文字收进它的 tooltip（事务在不在不再另写一句「事务状态：…」），
   所以 960 宽下只有过程消息按省略号收缩，整行需求 407（分配 417）。
   v0.2.3.3 删除了原「表名 · 排序（按主键升序）」提示行，也删除了按钮行右侧的「重新读取」图标
   （刷新只在状态栏 `⟳`，同一动作只留一个入口）；`--qa-layout-audit` 复核「需求 ≤ 分配」，
   `gridHeight` 因此比两行版多 18px。
9. 原型里的「对象树 / 平铺列表」切换在 v0.2.3.2 **整体删除**（平铺列表没有不可替代的价值，
   且与 `Objects` 页的列表重复）。
10. 标签条高 **47px**（`AdwTabBar` 实测）而非原型的 24：保留 GNOME 原生标签组件优先于像素一致
    （ADR 0023），代价是表格比原型再少 23px。勘误（v0.2.3.2）：`AdwTabPage` **确有** `pinned`
    API（libadwaita 1.2 起），`Objects` 页因此用 `set_page_pinned(true)` 钉在最左：紧凑
    （只显示图标）、没有关闭按钮、也无法关闭；宽度用 CSS 固定为 84px。旧版「没有 pinned
    API、只能点击关闭时拒绝」的结论作废。
