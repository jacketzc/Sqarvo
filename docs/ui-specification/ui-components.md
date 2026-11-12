# UI 组件总览

> 本文件由 `scripts/check-ui-sync.sh gen` 生成，源是
> [`components/*.toml`](components/)，**不要手改**。
> 字段与规则见 [`components/README.md`](components/README.md)、
> [ADR 0027](../decisions/0027-ui-component-registry.md)。

共 25 个组件 / 1 个功能区。同步列：`done` 已实现并按本规范核对过，
`partial` 部分实现（差异见组件明细的「平台」），`—` 未实现，`na` 该平台不做，
`blocked` 平台能力做不到。

## 对象树（`object_tree`）

左侧栏：对象树本体、搜索区、右键菜单与临时提示行。对象树是唯一的数据库对象入口，Objects 页与信息面板都跟着它的选中变化。

| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |
|---|---|---|---|---|---|---|
| `object_tree` | 对象树侧栏 | area | active | done | partial | — |
| `object_tree.tree` | 　树本体 | container | active | done | partial | — |
| `object_tree.tree.row` | 　　行模板（箭头 · 图标 · 名称 · 行尾文字） | row | active | done | partial | — |
| `object_tree.tree.row.expander` | 　　　行首箭头 | control | active | done | partial | — |
| `object_tree.tree.row.conn` | 　　　连接行 | row | active | done | partial | — |
| `object_tree.tree.row.catalog` | 　　　库 / schema 行 | row | active | done | partial | — |
| `object_tree.tree.row.group` | 　　　分组行（Tables / Views / Functions / Queries；表下 Fields / Indexes / Foreign Keys / Triggers） | row | active | done | partial | — |
| `object_tree.tree.row.object` | 　　　表 / 视图行 | row | active | done | partial | — |
| `object_tree.tree.row.field` | 　　　列 / 索引 / 触发器行 | row | active | done | — | — |
| `object_tree.tree.row.placeholder` | 　　　占位行（读取中 / 加载更多 / 读取失败 / 空说明 / 未实现） | row | active | done | partial | — |
| `object_tree.search` | 　对象树搜索区（状态栏左边沟） | container | active | done | — | — |
| `object_tree.search.box` | 　　搜索输入框 | control | active | done | — | — |
| `object_tree.search.status` | 　　搜索状态行 | control | active | done | — | — |
| `object_tree.search.results` | 　　搜索结果列表 | container | active | done | — | — |
| `object_tree.ctx_menu` | 　右键菜单 | container | active | done | — | — |
| `object_tree.ctx_menu.connect_saved` | 　　「连接（使用已保存的凭据）」 | item | active | done | — | — |
| `object_tree.ctx_menu.disconnect` | 　　「断开连接」 | item | active | done | — | — |
| `object_tree.ctx_menu.refresh_connection` | 　　「刷新连接」 | item | active | done | — | — |
| `object_tree.ctx_menu.refresh_catalog` | 　　「刷新库」 | item | active | done | — | — |
| `object_tree.ctx_menu.refresh_group` | 　　「刷新这一组」 | item | active | done | — | — |
| `object_tree.ctx_menu.edit_connection` | 　　「编辑连接…」 | item | active | done | — | — |
| `object_tree.ctx_menu.delete_connection` | 　　「删除连接」 | item | active | done | — | — |
| `object_tree.ctx_menu.open` | 　　「打开」 | item | active | done | — | — |
| `object_tree.ctx_menu.dump_table` | 　　「转储表…」 | item | active | done | — | — |
| `object_tree.hint` | 　侧栏底部临时提示行 | control | active | done | — | — |

### `object_tree` 对象树侧栏

kind `area` · status `active`

- 样式：槽位 `sidebar`；尺寸 `sidebar_width`、`sidebar_min`、`sidebar_max`、`row_height_tree`
- 同步：Linux done（v0.2.5 Linux 客户端；§2.1 逐条对照 tree.rs）；Windows partial（侧栏是 TreeView + SplitView；无搜索沟、无侧栏提示行。）；macOS none
- 实现备注：侧栏只有对象树：没有页脚按钮、没有刷新按钮、没有平铺列表、没有分页统计。搜索框与搜索状态行不画在侧栏里，放在状态栏左边沟（见 object_tree.search），宽度跟随分栏线。

### `object_tree.tree` 树本体

kind `container` · status `active` · parent `object_tree`

- 样式：槽位 `sidebar`；尺寸 `row_height_tree`、`badge_min_height`
- 无障碍：可访问名「对象树」；Tab 可聚焦；Tab 进入后 ↑ / ↓ 移动选中
- 同步：Linux done（§2.5 键盘表 + tree.rs install_handlers）；Windows partial（TreeView 承载，↑ / ↓ / Enter 同语义；无自定义按键控制器差异待核。）；macOS none
- 实现备注：GtkListView + 嵌套 ListStore，每层一个 Level，层级按 source 复用（收起不作废层）。折叠后重新展开复用已读内容、不发请求。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key ↑ / ↓ | 树有焦点 | 移动选中；选中变化与鼠标单击同效（信息面板与 Objects 页跟随） | `src/tree.rs:1970` |
| key Enter | 行有焦点 | 等同于双击（同一回调，走 activate 信号） | `src/tree.rs:2009` |

### `object_tree.tree.row` 行模板（箭头 · 图标 · 名称 · 行尾文字）

kind `row` · status `active` · parent `object_tree.tree`

- 样式：槽位 `sidebar`；尺寸 `row_height_tree`、`tree_arrow_box`、`gap_xs`、`font_status`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_mid`、`sqarvo_brand_on_mid`；状态 hover=`sqarvo_brand_soft`、selected=`sqarvo_brand_mid`
- 无障碍：可访问名「对象树行」
- 同步：Linux done（§2.2 单击行 + theme.rs 行状态规则）；Windows partial（行模板与徽标文案一致；缩进量未对齐（Linux 为 4 + 14/层）。）；macOS none
- 实现备注：hover 实际是 alpha(@sqarvo_brand_soft, 0.55)；选中底 @sqarvo_brand_mid + 前景 @sqarvo_brand_on_mid。缩进 4 + 14×层数 目前是代码常量，未进 metrics.toml（P1 补齐）。搜索结果行复用本模板，不另立组件（§2.4：结果就是那棵树）。行尾文字用 dim-label caption（font_status），连接行的行尾是连接态徽标。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 行存在 | 只选中：进入选中态，驱动信息面板与 Objects 页换内容；不展开、不打开、不连接 | `src/tree.rs:1970` |
| hover | 行存在 | 底色换成品牌浅档（alpha 55%），不动前景色 | `src/theme.rs:312` |

### `object_tree.tree.row.expander` 行首箭头

kind `control` · status `active` · parent `object_tree.tree.row`

- 样式：尺寸 `tree_arrow_box`
- 无障碍：可访问名「展开 / 收起」；Tab 不可聚焦（不占 Tab 位）
- 平台差异：Windows：TreeView 自带展开箭头，命中区由控件决定，不接受单独定制。
- 同步：Linux done（§2.2 行首箭头；arrow_button 接线与 set_focusable(false)）；Windows partial（命中区与外观由 TreeView 决定，无法逐像素对齐（接受）。）；macOS none
- 实现备注：扁平按钮 + 箭头图标；只在行可展开时出现，不可展开的行留同宽空白。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 行可展开 | 单击即展开 / 收起，不需要双击 | `src/tree.rs:1245` |

### `object_tree.tree.row.conn` 连接行

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：连接名 + 连接态徽标）」
- 同步：Linux done（§2.2 / §2.1 连接行；object_activation 与 connect_saved）；Windows partial（已保存连接 + 徽标、双击连接与展开同语义；目录页大小 200（Linux 500）。）；macOS none
- 实现备注：行尾是连接态徽标（已连接 / 未连接 / 连接中… / 断开中…），与搜索结果同一份文案。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 连接行 | 已连接 → 展开 / 收起；未连接 → 按保存的凭据连接；凭据缺失 → 明确提示并打开预填对话框；连接中 / 断开中 → 忽略并写日志 | `src/tree.rs:769、src/tree.rs:1688、src/ui.rs:845` |
| load | 已连接 | 首次展开发一次目录层 ListNodes 列出库，每页 500（NODE_PAGE_SIZE），带自己的分页 token | `src/tree.rs:2858` |

### `object_tree.tree.row.catalog` 库 / schema 行

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：库 / schema 名）」
- 同步：Linux done（§2.1 展开库 / schema）；Windows partial（展开库 0 次 RPC、四个分组行一致；分层与徽标文案待逐条核对。）；macOS none
- 实现备注：库与 schema 不合并成一层：驱动给 schema 层就多一层，重名库靠「会话 + 库名」精确匹配。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 行可展开 | 展开 / 收起（展开库只出现分组行，分组仍保持收起） | `src/tree.rs:769` |
| load | 展开库 / schema | 0 次 RPC：只建 Tables / Views / Functions / Queries 四个静态分组行，且不自动展开 | `src/tree.rs:2858` |

### `object_tree.tree.row.group` 分组行（Tables / Views / Functions / Queries；表下 Fields / Indexes / Foreign Keys / Triggers）

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：分组名）」
- 同步：Linux done（§2.1 分组与空态文案）；Windows partial（四个分组行一致；Functions / Queries 只有入口；表下四组未实现（表节点是叶子）；分组页大小 200。）；macOS none
- 实现备注：未实现的分组照实写说明，不发请求：Functions / Queries 无 kinds，Foreign Keys 是 capability.unsupported。空分组的文案按组不同（这一组没有表 / 视图 / 函数 / 已保存的查询；这张表没有字段 / 索引 / 触发器）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 分组行 | 展开 / 收起 | `src/tree.rs:769` |
| load | 展开某个分组 | 发一次 ListNodes(filter.kinds=…)，该层有自己的分页 token；Functions / Queries 不请求；Foreign Keys 写 capability.unsupported 说明 | `src/tree.rs:2858、src/tree.rs:376` |
| load | 服务端返回空 | 说明行按组给文案：「（这一组没有表）」「（这一组没有视图）」「（这一组没有函数）」「（还没有保存的查询）」「（这张表没有字段 / 索引 / 触发器）」 | `src/tree.rs:365、src/tree.rs:446、src/tree.rs:507` |

### `object_tree.tree.row.object` 表 / 视图行

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：对象名 + 种类）」
- 同步：Linux done（§2.2 双击表 / 视图）；Windows partial（双击 / Enter 打开数据页、同表复用标签一致；表节点无 Fields / Indexes / FK / Triggers 子组。）；macOS none
- 实现备注：可打开优先于可展开：表虽然有四组子节点，双击仍是打开数据标签。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 行可打开 | 打开表数据标签（同一张表在对象名 + catalog + 连接键三者一致时复用已有标签，不新建会话、不重载数据） | `src/tree.rs:769、src/tabs.rs:1608` |

### `object_tree.tree.row.field` 列 / 索引 / 触发器行

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：列 / 索引 / 触发器名）」
- 同步：Linux done（§2.2 双击列 / 索引 / 触发器）；Windows none（表节点是叶子，四组尚未实现（§9 已知偏差）。）；macOS none
- 实现备注：叶子对象：没有子层、没有菜单、不发请求。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 叶子对象行 | 无动作（不发请求） | `src/tree.rs:769` |

### `object_tree.tree.row.placeholder` 占位行（读取中 / 加载更多 / 读取失败 / 空说明 / 未实现）

kind `row` · status `active` · parent `object_tree.tree.row`

- 样式：尺寸 `row_height_tree`、`font_status`
- 无障碍：可访问名「占位行」
- 同步：Linux done（§2.1 读取中 / 加载更多 / 读取失败；§2.3 占位行不给菜单）；Windows partial（有占位行「正在加载对象列表…」与「加载更多…」；读取失败行点按即重试（Linux 没有这条路径）；空态文案待逐条核对。）；macOS none
- 实现备注：占位行不可展开、不携带层；右键直接返回，不给菜单。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 「加载更多…」行且还有下一页 | 选中即继续拉下一页（分页动作不要求双击） | `src/tree.rs:2962、src/tree.rs:1970` |
| click | 任意占位行 | 信息面板收到 None（回到空态 / 留白）；不打开任何东西 | `src/tree.rs:1970` |
| load | 层在飞 / 请求失败 | 占位行写「读取中…」；失败写「读取失败…」+ 状态行给可读文案与稳定错误码，与「空」严格区分 | `src/tree.rs:2977、src/tree.rs:3148` |
| right_click | 占位行 | 直接返回，不弹菜单 | `src/tree.rs:3191` |

### `object_tree.search` 对象树搜索区（状态栏左边沟）

kind `container` · status `active` · parent `object_tree`

- 样式：槽位 `status_bar.left_gutter`；尺寸 `status_height`、`sidebar_width`
- 同步：Linux done（§7 状态栏左边沟 + §2.4 搜索）；Windows none（对象树没有搜索入口（§7.1 未记录）。）；macOS none
- 实现备注：v0.2.4 从树顶部搬到状态栏左边沟：宽度跟随分栏线，搜索框不把它撑宽，竖线延伸到底。

### `object_tree.search.box` 搜索输入框

kind `control` · status `active` · parent `object_tree.search`

- 样式：槽位 `status_bar.left_gutter`
- 无障碍：可访问名「搜索连接、库、表」；Tab Ctrl+F 聚焦；Esc 清空并回到树页
- 同步：Linux done（§2.4 搜索；Ctrl+F / Enter / Esc 三键）；Windows none（无搜索框。）；macOS none
- 实现备注：placeholder「搜索连接、库、表...」；匹配语义是 LIKE %关键词%，不做分词。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key 任意输入 | 搜索框有输入 | 防抖 250 ms 后搜索；空白串不搜索并回到树页 | `src/tree.rs:2185、src/tree.rs:2220` |
| key Enter | 搜索框内有文本 | 无条件重新派发一次搜索：取消待触发的定时器、代次 +1，重新清结果并发请求（同一关键词再按也真的再查一遍）；空文本按「停止搜索」处理 | `src/tree.rs:2070、src/tree.rs:2168` |
| key Ctrl+F | 工作区存在 | 把焦点交给搜索框（状态栏左边沟） | `src/ui.rs:3884、src/tree.rs:1441` |
| key Esc | 搜索框有焦点 | 清空输入、回到树页；结果行随列表销毁，结果页里收起过的层级一并忘掉 | `src/tree.rs:2467` |

### `object_tree.search.status` 搜索状态行

kind `control` · status `active` · parent `object_tree.search`

- 样式：槽位 `status_bar.left_gutter`；尺寸 `font_status`
- 无障碍：可访问名「搜索状态」
- 同步：Linux done（§2.4 状态行与三种失败文案）；Windows none（无搜索。）；macOS none
- 实现备注：单行省略，全文进 tooltip。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 搜索完成 | 写「N 个连接 · M 个对象」（M = 命中的库 + 表 / 视图）；有失败追加「；K 条连接失败（…）」；没有已打开连接追加「；没有已打开的连接，只匹配了连接名」；无匹配写「没有名称包含「<关键词>」的连接、库、表或视图。换个关键词试试。」；无权限写「没有权限读取对象列表：<错误>」；其它失败写「搜索失败：<错误>」 | `src/tree.rs:2555、src/tree.rs:2220` |

### `object_tree.search.results` 搜索结果列表

kind `container` · status `active` · parent `object_tree.search`

- 样式：槽位 `status_bar.left_gutter`
- 同步：Linux done（§2.4 结果行点击语义）；Windows none（无搜索。）；macOS none
- 实现备注：结果行复用 object_tree.tree.row：行结构、行高、行尾文字与树逐控件一致。v0.2.3.4 起不再有命中高亮 / 加粗：命中行与树里同一行像素一致。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表 / 视图行 | 打开该对象的数据标签（与树双击同一条 open）；侧栏仍停在结果页，搜索框与命中数原样保留 | `src/tree.rs:2094、src/tree.rs:4501` |
| click | 库行 | 在树里逐层展开祖先并选中（切到树页） | `src/tree.rs:3469、src/tree.rs:2094` |
| click | 连接行 | 按该连接「点连接」同一条路径（未连接则连接、已连接则展开），并清空搜索框退出筛选 | `src/tree.rs:2094` |
| click | 分组行 | 只有行内箭头能收起 / 展开；点行本身没有可点目标、不触发打开 | `src/tree.rs:2094` |

### `object_tree.ctx_menu` 右键菜单

kind `container` · status `active` · parent `object_tree`

- 样式：槽位 `overlay`
- 同步：Linux done（§2.3 右键菜单）；Windows none（对象树没有右键菜单（§7.1 未记录）。）；macOS none
- 实现备注：贴着点击位置的 GtkPopover；点任一条目先收起菜单再执行动作；占位行不给菜单。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| right_click | 非占位行 | 按行种类给条目；占位行与列 / 索引 / 触发器行直接返回（menu_items 为空） | `src/tree.rs:3191、src/tree.rs:3313` |

### `object_tree.ctx_menu.connect_saved` 「连接（使用已保存的凭据）」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 无障碍：可访问名「连接（使用已保存的凭据）」
- 同步：Linux done（§2.3 连接（使用已保存的凭据））；Windows none（无树右键菜单。）；macOS none
- 实现备注：只读 credential_ref，不读也不写明文密码。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接未连接 / 失败 | 与左键双击同一条 connect_saved；失败给出可读原因 | `src/tree.rs:3230、src/ui.rs:845` |

### `object_tree.ctx_menu.disconnect` 「断开连接」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 无障碍：可访问名「断开连接」
- 同步：Linux done（§2.3 断开连接）；Windows none（无树右键菜单。）；macOS none
- 实现备注：只关闭该连接的浏览会话（树 / 搜索 / DDL）；数据页用另一条会话，已加载数据仍可查看。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该连接已连接 | 先确认「断开连接？」（说明影响范围；按钮 取消 / 断开连接，破坏性默认取消），确认后才断开 | `src/tree.rs:3230、src/ui.rs:2954` |

### `object_tree.ctx_menu.refresh_connection` 「刷新连接」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 无障碍：可访问名「刷新连接」
- 同步：Linux done（§2.3 刷新连接）；Windows none（无树右键菜单。）；macOS none
- 实现备注：就地清空该层 store 再发第一页，不收起子树；一层都没建立过时不发请求。只在右键菜单里，没有快捷键。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该连接已连接 | 重读这条连接已经建立的层；一层都没有时侧栏底部提示行写「这条连接还没有展开过：展开时会重新读取」 | `src/tree.rs:3407、src/tree.rs:3376、src/tree.rs:1936` |

### `object_tree.ctx_menu.refresh_catalog` 「刷新库」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 无障碍：可访问名「刷新库」
- 同步：Linux done（§2.3 刷新库）；Windows none（无树右键菜单。）；macOS none

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 库 / schema 节点 | 重读该库下已展开的分组与直接子对象层；分组全收起时只清缓存，提示行写「这个库的分组都收起着：展开时重新读取」 | `src/tree.rs:3434、src/tree.rs:1936` |

### `object_tree.ctx_menu.refresh_group` 「刷新这一组」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 无障碍：可访问名「刷新这一组」
- 同步：Linux done（§2.3 刷新这一组）；Windows none（无树右键菜单。）；macOS none
- 实现备注：静态分组行（Groups / TableGroups）没有服务端内容，刷新直接返回。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 分组节点 | 已展开（层已建立）→ 重读该组第一页；未展开过 → 不发请求，提示行写「这个分组还没展开：展开时会重新读取」（4 秒后自行收起）并记日志 | `src/tree.rs:3347、src/tree.rs:1936` |

### `object_tree.ctx_menu.edit_connection` 「编辑连接…」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 无障碍：可访问名「编辑连接…」
- 同步：Linux done（§2.3 编辑连接）；Windows none（无树右键菜单。）；macOS none
- 实现备注：密码永不回显：已有 credential_ref 时密码行留空、复用引用；不会自动连接。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接存在于配置里 | 打开连接对话框并预填非敏感字段 | `src/ui.rs:785、src/ui.rs:989` |

### `object_tree.ctx_menu.delete_connection` 「删除连接」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 无障碍：可访问名「删除连接」
- 同步：Linux done（§2.3 删除连接）；Windows none（无树右键菜单。）；macOS none
- 实现备注：配置删除与密钥删除分开报告：配置已删而密钥没删时明确说明。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接存在于配置里 | 二次确认「删除连接？」（写明从 connections.json 删除、树节点与浏览会话一并消失、其它连接不受影响）；确认后关会话 → 删配置 → 尽量删密钥条目 | `src/ui.rs:1310、src/ui.rs:454` |

### `object_tree.ctx_menu.open` 「打开」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 无障碍：可访问名「打开」
- 同步：Linux done（§2.3 表 / 视图节点）；Windows none（无树右键菜单。）；macOS none

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表 / 视图节点 | 与双击同一条路径打开数据标签 | `src/tree.rs:3191、src/tree.rs:3230` |

### `object_tree.ctx_menu.dump_table` 「转储表…」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 无障碍：可访问名「转储表…」
- 同步：Linux done（§2.3 转储表…（v0.2.5））；Windows none（无树右键菜单（数据页导出亦未实现）。）；macOS none
- 实现备注：只有表节点有；视图没有（往视图 INSERT 不可重放）。按浏览会话 offset 逐页读全表，建表语句与列名在同一后台线程读，不跨连接。取消或失败不留半截文件；SQL 引号按该连接驱动（MariaDB 反引号 / SQLite 双引号）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表节点且该连接有浏览会话 | 弹确认「转储表 <表名>？」→ 选文件（默认 <表名>.sql，过滤 *.sql）→ 进度 / 可取消；不需要先打开标签 | `src/tree.rs:3230、src/tree.rs:731、src/export_ui.rs` |

### `object_tree.hint` 侧栏底部临时提示行

kind `control` · status `active` · parent `object_tree`

- 样式：槽位 `sidebar`；尺寸 `font_status`、`gap_xs`
- 无障碍：可访问名「侧栏提示」
- 同步：Linux done（§2.3 刷新这一组 / 刷新库的跳过后提示）；Windows none（无侧栏提示行。）；macOS none
- 实现备注：只用于「刷新这一组 / 刷新库 / 刷新连接」的跳过后说明；4 秒后自行收起。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 刷新被跳过 | 写一行说明（例如「这个分组还没展开：展开时会重新读取」），4 秒后自行收起 | `src/tree.rs:1936` |
