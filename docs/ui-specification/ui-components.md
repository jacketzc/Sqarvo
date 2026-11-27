# UI 组件总览

> 本文件由 `scripts/check-ui-sync.sh gen` 生成，源是
> [`components/*.toml`](components/)，**不要手改**。
> 字段与规则见 [`components/README.md`](components/README.md)、
> [ADR 0027](../decisions/0027-ui-component-registry.md)。

前端状态（源：[frontend/platforms.toml](../../frontend/platforms.toml)）：Linux Active、Windows Rewriting、macOS Planned。同步列仅显示 Active 平台。

共 240 个组件 / 11 个功能区。同步列：`done` 已实现并按本规范核对过，
`partial` 部分实现（差异见组件明细的「平台」），`—` 未实现，`na` 该平台不做，
`blocked` 平台能力做不到；`已清` / `待清` 只用于退役组件，是该端「代码删了 / 界面隐藏了」
与「还留着」的清理进度（见文末「待清理」）。旧平台的实现描述仅供历史参考。

## 表数据页（`data_page`）

表数据页（含查询标签复用同一套编辑态）：单行工具条与事务、筛选与排序面板、列显隐、数据网格与就地编辑、脏行标记与批量提交、offset 分页、导出 CSV / INSERT / 转储表、数据行右键菜单、按行浏览与单行「字段 · 值」两种展示形式，以及只读 / 空 / 错误 / 断连四态。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `data_page` | 表数据页（整页） | area | active | done |
| `data_page.toolbar` | 　单行工具条 | container | active | done |
| `data_page.toolbar.commit` | 　　「提交」按钮（已移除；提交由状态栏「应用」承担） | control | removed | 已清 |
| `data_page.toolbar.begin_txn` | 　　「开始事务」按钮 | control | active | done |
| `data_page.toolbar.commit_txn` | 　　「提交事务」按钮 | control | active | done |
| `data_page.toolbar.rollback_txn` | 　　「回滚事务」按钮 | control | active | done |
| `data_page.toolbar.status_cluster` | 　　工具条状态簇（spinner · 过程消息 · 状态圆点） | container | active | done |
| `data_page.toolbar.status_dot` | 　　状态圆点（灰 / 事务棕 / 脏行强调色） | control | active | done |
| `data_page.toolbar.filter` | 　　「筛选」（漏斗）按钮 | control | active | done |
| `data_page.toolbar.columns` | 　　「列显隐」（眼睛）按钮 | control | active | done |
| `data_page.toolbar.columns.card` | 　　　列显隐弹层（复选清单 + 说明） | container | active | done |
| `data_page.toolbar.export` | 　　「导出 ▾」菜单按钮 | control | active | done |
| `data_page.export_menu` | 　导出菜单（三项出口） | container | active | done |
| `data_page.export_menu.csv` | 　　「导出 CSV…」 | item | active | done |
| `data_page.export_menu.insert` | 　　「导出为 INSERT 语句…」 | item | active | done |
| `data_page.export_menu.dump` | 　　「转储表…」 | item | active | done |
| `data_page.badges` | 　表头上方徽标行 | container | active | done |
| `data_page.grid` | 　数据网格（表格外框 · 行号列 · 表头 · 数据行） | container | active | done |
| `data_page.grid.header` | 　　列头（列名 · 原始类型 · 排序标记） | row | active | done |
| `data_page.grid.row_number` | 　　行号列（行号 / 脏行 / 标删 / 新行） | cell | active | done |
| `data_page.grid.cell` | 　　数据单元格（值 · NULL / 空串 · 脏色） | cell | active | done |
| `data_page.editor` | 　单元格编辑器（标题 · 输入框 / 日期时间选择器 · 设为 NULL · 应用 / 取消） | container | active | done |
| `data_page.row_menu` | 　数据行右键菜单 | container | active | done |
| `data_page.row_menu.delete` | 　　「删除行」 | item | active | done |
| `data_page.row_menu.copy_insert` | 　　「复制为 ▸ 复制为 INSERT」 | item | active | done |
| `data_page.detail` | 　单行「字段 · 值」展示形式 | container | active | done |
| `data_page.filter_panel` | 　筛选与排序面板（草稿 · 应用 / 清空） | container | active | done |
| `data_page.filter_panel.condition_composer` | 　　条件编辑组（列 · 算子 · 值 · 添加条件） | container | active | done |
| `data_page.filter_panel.condition_row` | 　　条件行（列 · 算子 · 值，可就地编辑） | row | active | done |
| `data_page.filter_panel.sort_composer` | 　　排序编辑组（列 · 方向 · 添加排序） | container | active | done |
| `data_page.filter_panel.sort_row` | 　　排序键行（列名 · asc / desc · 上移 / 下移 / 删除） | row | active | done |
| `data_page.txn_chooser` | 　「开始事务」卡片（隔离级别 · 只读） | container | active | done |
| `data_page.placeholder` | 　占位页（尚未打开 / 正在读取） | container | active | done |
| `data_page.banner` | 　页首横幅（只读 / 断连 / 错误） | control | active | done |
| `data_page.errors` | 　逐行提交失败列表 | container | active | done |
| `data_page.query_edit` | 　查询结果的单表编辑（复用同一份草稿与提交路径） | container | active | done |

### `data_page` 表数据页（整页）

kind `area` · status `active`

- 样式：槽位 `workspace.content`；尺寸 `table_actions_height`、`row_height_data`、`grid_padding_start`
- 同步：Linux done（v0.2.5 Linux 客户端；§5.1–§5.10 逐条对照 data_page.rs（begin_open / show_open / 四态））
- 实现备注：自上而下：横幅 → 单行工具条 → 表头上方徽标行 → 逐行错误列表 → 表格外框（网格 / 字段 · 值）→ 占位页。页内浮层只有两块：列显隐卡片与「开始事务」卡片；筛选与排序面板是挂在漏斗按钮上的 popover（不进主窗口排版）。表格外框与 Objects 列表共用同一份 .sqarvo-table-frame（外框线宽 frame_border_width、圆角 radius_lg），圆角靠 overflow=hidden 真正裁到内容。行号列 32、表头 26、数据行 32 都只来自 metrics；页面本身不另存一份数字。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 打开表 / 换表 / 重新读取 | 清空上一屏的草稿、选中、逐行错误与「最近一次操作」注记，退回按行浏览，占位页写「正在读取表数据」+「等待服务端返回第一页…」，工具条消息同步「正在读取表数据…」 | `src/data_page.rs:4304、src/data_page.rs:4339` |

### `data_page.toolbar` 单行工具条

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `table_actions_height`、`row_height_data`
- 同步：Linux done（§5.1 单行工具条；data_page.rs new() 的 controls 行 + --qa-layout-audit「表格工具条 / 状态簇」两项）
- 实现备注：只有一行：开始事务（开事务后原位换成 提交事务 回滚事务）──弹性空白── [spinner · 过程消息 · 状态圆点] 筛选 列显隐 导出 ▾。这一行**没有**「撤销」「删除」「新增」「刷新」「提交更改」：撤销与删除在状态栏行操作组（↶ / −），删除另有行右键菜单，新增只有状态栏 +，刷新只有状态栏 ⟳，提交只有状态栏 ✓（status_bar.row1.ops.apply，与 Ctrl+S 同一路径）；在途取消提交也只有状态栏「取消」。三个动作按钮共用一套内距（按钮盒 36 宽 × 32 高）；「开始事务」是中性动作：不挂 flat，也不用 suggested-action / destructive-action。行内容高度钉在 row_height_data，上 8 / 下 6 留白；spinner、消息与圆点全部 valign=Center，与图标按钮同一垂直中线。状态簇紧贴漏斗左侧；960 宽下只有过程消息按省略号收缩，行本身不溢出。

### `data_page.toolbar.commit` 「提交」按钮（已移除；提交由状态栏「应用」承担）

kind `control` · status `removed` · parent `data_page.toolbar` · → `status_bar.row1.ops.apply`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「提交」；Tab （已移除）原为工具条内第一枚动作按钮
- 同步：Linux removed（工具条提交按钮已从 data_page.rs 删除；提交走 status_bar.row1.ops.apply（RowOp::Apply → on_commit_clicked），在途取消走 status_bar.row1.ops.cancel。）
- 实现备注：本条目已退役，下面记录的是移除前的行为真值：文案随待提交条数变化（提交（N），N = 脏行 + 新行草稿 + 标记删除）；在途时同一按钮变「取消提交」，取消中变「正在取消…」——文案由批次状态决定，不在视图层拼。现在这套文案只作为 QA 观测量由纯函数 `submit_batch::commit_button_label` 计算，不再对应可见控件。可用性现在由数据页显式谓词 `data_page::commit_available()` 给出，状态栏「应用（✓）」读同一谓词（不再读页内按钮的 sensitive）；在途可点即取消、取消中不可点，只读 / 失败 / 断连原因最后覆盖。只读原因一旦成立，提交入口恒禁用，并把原因写进过程消息的 tooltip（不能只靠置灰暗示）。提交顺序：有新行时 `InsertRows` **先行**（整批原子，任一行失败则整批回滚），成功后才提交脏行；插入失败则一条脏行都不提交——避免库里留下半套状态。单批上限 500 行：超出时只执行前 N 条，其余留待下次，横幅「单批上限 500 行：本次只执行了前 N 条，还有 M 条未提交——请点状态栏「应用」续传。」；逐行失败的行留在草稿里，可改完再提交。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 可编辑、有待提交变更、没有 RPC 在途 | 走风险判定后提交：逐行更新与删除按主键定位；按钮文案「提交（N）」，状态行「正在提交 M 条变更（批次 …）：点「取消提交」可在行边界中止」 | `src/data_page.rs:5193、src/data_page.rs:5254、src/submit_batch.rs:436` |
| click | 变更超过 100 行或含删除（本地判定） | 弹「确认提交变更？」：正文给更新 / 删除行数，超过 100 行时追加「变更数超过 100 行的风险阈值…」；按钮 取消 / 确认提交（破坏性，默认取消）；取消则「已取消提交（未发送任何变更）」。服务端要求 acknowledge_risk 时再走同一个对话框 | `src/table_data.rs:535、src/table_data.rs:566、src/data_page.rs:5309` |
| click | 提交在途 | 同一按钮变成「取消提交」且可点：请求取消后显示「正在取消…」，整批回滚、界面保留脏行与编辑文本（不采信任何部分结果），可再次点「提交」重试；取消中 / 已结束的重复点击幂等 | `src/data_page.rs:5222、src/submit_batch.rs:414、src/submit_batch.rs:454` |
| load | 只读 / 已断开 / RPC 在途 / 没有待提交变更 | 按钮禁用（不弹错误）；只读与断连时过程消息的 tooltip 给同一句原因 | `src/data_page.rs:6743、src/data_page.rs:4289` |

### `data_page.toolbar.begin_txn` 「开始事务」按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「开始事务」；Tab 工具条内第一枚动作按钮（「提交」移除后）
- 同步：Linux done（§5.1 开始事务 / §5.2；begin_transaction + begin_tooltip；驱动不支持时落只读原因 capability.unsupported）
- 实现备注：打开事务后**隐藏**（不是置灰）：同一位置换成「提交事务 / 回滚事务」。有未提交的更改时置灰，tooltip 给原因「还有 N 行未提交的更改：先提交或放弃它们再开事务」——最容易踩到的那一条优先。页内按钮、win.begin-txn 动作与 QA 都落到 begin_transaction 这同一条命令，被挡住时状态行写同一句话、不打开卡片。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 没有打开的事务、未断开、没有未提交的更改（脏行 / 标记删除 / 新行草稿） | 打开「开始事务」卡片（隔离级别 + 只读勾选），不直接发 RPC | `src/data_page.rs:5477、src/data_page.rs:3710` |
| load | 有待提交的更改 | 按钮置灰，tooltip「还有 N 行未提交的更改：先提交或放弃它们再开事务」；点了不打开卡片，状态行同一句 | `src/data_page.rs:6725、src/data_page.rs:6743` |
| load | 驱动未声明显式事务 / 行编辑能力 | 如实退回自动提交、不假装事务存在：状态圆点与 tooltip 首段仍是「自动提交」，日志与 tooltip 写明 capability.unsupported；Linux 侧同一能力位先落到只读原因上（横幅「只读：驱动未声明行编辑能力」+ 编辑类按钮禁用），事务棕只在事务真的打开时出现 | `src/table_data.rs:1027、src/data_page.rs:6883` |

### `data_page.toolbar.commit_txn` 「提交事务」按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「提交事务」；Tab 有事务时出现在工具条动作区
- 同步：Linux done（§5.1 / §5.2 事务按钮互斥与失败态；commit_transaction + sync_badge）
- 实现备注：只在有活动事务时可见（与「开始事务」原位互斥）；失败态**不可提交**，只剩回滚。有脏行时拒绝：先点状态栏「应用」提交或「放弃更改」，与提交入口同一套谓词（`commit_available()`）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有活动事务且没有脏行 | 提交事务；成功后丢弃尚未提交的界面更改并重新读取当前表，圆点从棕回到灰 | `src/data_page.rs:5495、src/data_page.rs:4694` |
| click | 有活动事务但有脏行 | 拒绝并写「还有 N 行更改未提交：先点状态栏「应用」或「放弃更改」再提交事务」 | `src/data_page.rs:5495、src/data_page.rs:6743` |

### `data_page.toolbar.rollback_txn` 「回滚事务」按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「回滚事务」；Tab 有事务时出现在「提交事务」右侧
- 同步：Linux done（§5.1 回滚事务 / §8.2 回滚确认；rollback_transaction 的 AlertDialog）
- 实现备注：破坏性动作（destructive-action）；失败态仍可用——「事务失败（只能回滚）」时这是唯一出口。回滚完成 = 事务结束：丢弃界面草稿并重新读取当前表，让屏幕回到数据库真实状态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有活动事务（含失败态） | 先确认「回滚事务？」（有脏行时正文写「回滚事务将丢弃数据库侧的变更，并放弃 N 行未提交的界面更改。」），按钮 取消 / 回滚（破坏性，默认取消）；确认后回滚，圆点回到灰 | `src/data_page.rs:5526、src/data_page.rs:4694` |

### `data_page.toolbar.status_cluster` 工具条状态簇（spinner · 过程消息 · 状态圆点）

kind `container` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`status_dot_diameter`
- 无障碍：可访问名「操作状态」
- 同步：Linux done（§5.1 工具条状态簇与「操作结果收进圆点」；set_state / set_status / note_action）
- 实现备注：自左向右：spinner（有 RPC 在途时可见）→ 本次操作的临时消息 → 状态圆点；整体在漏斗左侧，与图标按钮同一垂直中线。过程消息只留两类：①「正在…」进行中（正在提交… / 正在取消提交（批次 …）… / 正在读取表数据…，读取完成后擦掉）；② warning / error（权限不足、类型不可编辑、行已标记删除、提交逐行失败…）。已经发生的操作结果不占可见文案，只写进圆点 tooltip 末段「最近一次操作：…」：第 N 行已标记删除（提交前可撤销）、已放弃未提交的更改（含新增行），以及普通提示——「筛选与排序已应用（N 条条件 · M 个排序键）· 回到第 1 页」「条件与排序已清空」「筛选条件已清空」、新增行「新行：… · 双击单元格填值，填完点状态栏「应用」」与新行单元格改动摘要、「开始事务：<级别> · <模式>」。上述普通提示走 note_action（写圆点 tooltip 并清空可见过程消息位）；只有「正在…」进行中与 warning / error 才走 set_status 占可见文案。error 级提示的全文（含点名字段、整批回滚、草稿保留、错误码）都写进过程消息，不另造元素：这一行最多 24 个 CJK 字，窄时由 GtkLabel 省略号缩略，悬停同一处文本给全文（tooltip 取 status 完整文本，只读原因优先）。新增行因 NOT NULL 留空失败时不再弹页首横幅或逐行错误列表——工具条与编辑器之间不新增元素。这一处**不重复**行数 / 页码 / 总数：那些只在状态栏分页器显示一次。撤销（放弃未提交的更改，状态栏行操作组 ↶ / RowOp::Discard）与删除（−）的结果同样不占可见文案：前者把 ●N / ✕N / ＋ 清回普通行号，后者把行标删；两句都只写进圆点 tooltip 的「最近一次操作：…」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 有 RPC 在途 | spinner 可见并转动，消息写「正在读取表数据…」/「正在提交 M 条变更（批次 …）：点「取消提交」可在行边界中止」/「正在取消提交（批次 …）：等驱动在行边界停下，已执行的行会整批回滚…」 | `src/data_page.rs:6570、src/submit_batch.rs:454` |
| load | 读取完成 | 擦掉「正在读取…」，消息位留空（不写行数 / 页码）；最近一次读取摘要只进状态栏那格的 tooltip | `src/data_page.rs:6585、src/data_page.rs:6637` |
| load | 普通操作完成（筛选应用 / 清空、新增行开始、新行单元格改动、开始事务、标记删除、放弃更改） | 提示写进圆点 tooltip 末段「最近一次操作：…」，可见过程消息位保持为空（note_action 先清空）；只有「正在…」进行中与 warning / error 才占可见文案 | `src/data_page.rs:6663、src/data_page.rs:2844、src/data_page.rs:3241` |
| load | 新增行提交失败（NOT NULL 字段留空，整批回滚） | 过程消息写全文「插入失败：列 X 不能为空；整批已回滚，库里 0 行变化（错误码）· 草稿保留可重试」（error 色调），窄时省略号缩略、悬停该文本给全文；不在工具条与编辑器之间新增横幅 / 逐行错误列表，不给「重启 Sqarvo」 | `src/data_page.rs:3558、src/backend/table.rs:568` |

### `data_page.toolbar.status_dot` 状态圆点（灰 / 事务棕 / 脏行强调色）

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `status_dot_diameter`；颜色 `warning_color`、`accent_bg_color`
- 无障碍：可访问名「事务与未提交更改的状态」；Tab 不可聚焦（不占 Tab 位），仍在无障碍树里
- 自绘：没有原生等价物：GTK / Adw 没有「状态圆点」控件；用 GtkBox + CSS 画 7px 圆（尺寸走 metrics.status_dot_diameter，颜色走 token），命中区仍是 24×24 的 GtkButton。
- 同步：Linux done（§5.1 状态圆点三态 / §5.2 事务状态的可见表达；sync_status_dot + update_dirty_label）
- 实现备注：视觉是一枚 status_dot_diameter 的实心圆（约漏斗墨迹的一半），命中区仍是 24×24；focusable=false，但 tooltip 同时登记为可访问名。三态互斥、优先级自上而下：灰 = 没有未提交的更改、也没有打开的事务；@warning_color = 事务已打开（含只读 / 失败态）；@accent_bg_color = 有脏行 / 新行（压在事务态之上）。没有打开的表时整枚隐藏——圆点说的是「这张表格此刻怎样」。原来常驻的两段文字（自动提交徽标、脏行 N · 待提交 M 条）不再占可见文案，只作 tooltip 与 QA 断言的文案来源。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| hover | 已打开表 | tooltip 三段：第一段写事务状态 + 脏行 N · 待提交 M 条（含 K 个删除）· 新行 1 行，第二段是事务说明（「自动提交：每条语句自成一个事务」/「事务中：提交事务之前，改动只在当前会话可见」/「事务失败：只能回滚」/「已断开：事务已由后端回滚」），有已发生结果时第三段「最近一次操作：…」（如「第 2 行已标记删除（提交前可撤销）」「已放弃未提交的更改（含新增行）」） | `src/data_page.rs:6883、src/data_page.rs:6671` |
| load | 状态变化（开始 / 提交 / 回滚 / 失败 / 标记删除 / 放弃） | 颜色随状态实时切换（有脏行即强调色，否则事务打开即棕，否则灰）；状态变化本身**不**在过程消息位写任何可见文字 | `src/data_page.rs:6883、src/data_page.rs:4694` |

### `data_page.toolbar.filter` 「筛选」（漏斗）按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「筛选与排序」；Tab 工具条次要动作组内第一枚
- 图标：`action.filter`
- 同步：Linux done（§5.1 / §5.3 能力位与面板开合；toggle_filter_panel + show_open）
- 实现备注：图标是项目自有的 sqarvo-filter-symbolic：放大镜会被读成「搜索」，Adwaita / Yaru 都没有漏斗。驱动能力位 filtering=false 时按钮**隐藏**（不是点了报错）；没有列时一并禁用。面板是挂在它上面的 popover（autohide=false），锚点落在面板上沿偏右（0.618）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 驱动声明支持筛选且已打开表 | 打开 / 收起筛选与排序面板；打开时把**已应用**的筛选与排序复制成草稿 | `src/data_page.rs:2416、src/data_page.rs:2425` |
| load | 打开表时读到能力位 filtering=false | 漏斗隐藏；排序入口随之不生效；出现「驱动不支持排序/筛选」徽标 | `src/data_page.rs:4376、src/data_page.rs:4476` |

### `data_page.toolbar.columns` 「列显隐」（眼睛）按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「列显隐」；Tab 工具条次要动作组内第二枚
- 图标：`action.columns`
- 同步：Linux done（§5.1 列显隐 / §5.3 列显隐卡片；open_column_visibility）
- 实现备注：只影响渲染：不改变查询、复制与导出口径（导出始终包含全部列）。没有列时隐藏；有列时不依赖驱动能力位（与筛选取舍相反）。卡片是挂在它上面的 popover（autohide=false），锚点落在面板上沿偏右（0.618），与「筛选」同一形态。与漏斗互斥：打开一个就收起另一个（同一块区域不让两个弹层同时开）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前表有列 | 打开 / 收起列显隐弹层（挂在眼睛按钮上的 popover，带指示箭头，锚点在上沿 0.618 处）；再点一次收起 | `src/data_page.rs:2373、src/data_page.rs:1788` |

### `data_page.toolbar.columns.card` 列显隐弹层（复选清单 + 说明）

kind `container` · status `active` · parent `data_page.toolbar.columns`

- 样式：槽位 `overlay`；尺寸 `column_panel_width`、`column_list_max_height`、`radius_lg`、`row_height_data`；颜色 `sqarvo_brand_mid`、`sqarvo_brand_on_mid`
- 无障碍：可访问名「列显隐」
- 原生控件：gtk:GtkPopover
- 同步：Linux done（§5.3 列显隐卡片；cc_card 构建 + on_column_choice_toggled）
- 实现备注：挂在眼睛按钮上的 GtkPopover（与筛选面板同一形态）：带指示箭头，内容宽取 metrics 的 column_panel_width。标题「列显隐」+ 复选框清单 + ✕；清单最大高度取 metrics 的 column_list_max_height，超出滚动。勾选态是复选控件自己的 :checked 圆点：底色用中档品牌紫（theme.rs 的 .sqarvo-columns 规则），高对比模式退回系统 accent。说明行原文：「隐藏列只影响这张表格的渲染；查询、复制与即将提供的导出口径不变。」；复选标签是「列名 · 原始类型」，tooltip 追加「取消勾选只隐藏这一列，不影响数据与导出口径」。换表 / 刷新后按新列清单重建，隐藏状态复位（旧下标对新表没有意义）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 卡片已打开、该列在清单里 | 勾选 = 显示该列、取消勾选 = 隐藏该列；只重排表头与单元格，数据与导出口径不变；可以隐藏到只剩行号 | `src/data_page.rs:2397、src/data_page.rs:2352` |
| click | 弹层已打开 | 点 ✕ 收起弹层，隐藏选择保持不变 | `src/data_page.rs:1790` |

### `data_page.toolbar.export` 「导出 ▾」菜单按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「导出」；Tab 工具条次要动作组内第三枚（最右）
- 图标：`action.export`
- 同步：Linux done（§5.1 导出 ▾ / §5.7 入口；MenuButton + 三个菜单项走 export_ui::begin）
- 实现备注：图标 + 下拉箭头（always_show_arrow）：**一个入口三种出口**，不是单击即导出的图标；图标仍是 document-save-symbolic。tooltip：「导出：CSV / INSERT 语句（当前页或整张表）/ 转储表；本页已加载 N 行（Ctrl+Shift+E 直接导出 CSV）」。有已加载行且没有 RPC 在途时可用；在途时禁用（避免写出半套状态）。Ctrl+Shift+E 仍是**直接导出 CSV**（不弹格式菜单），与这一格不是同一条路径；主菜单「表 → 导出 CSV」已暂时隐藏（改动确认单 #1 / #198，注册表 shell.menu.export_csv 标 removed）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有已加载行、没有 RPC 在途 | 展开三项菜单：「导出 CSV…」「导出为 INSERT 语句…」「转储表…」；单击图标先展开菜单，范围（当前页 / 整张表）仍由紧接着的对话框显式选择 | `src/data_page.rs:1087、src/data_page.rs:1715` |
| load | 没有已加载行（或正在提交） | 按钮禁用；没有可导出内容时不弹菜单、只写一行日志 | `src/data_page.rs:6743` |

### `data_page.export_menu` 导出菜单（三项出口）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`
- 原生控件：gtk:GtkPopoverMenu
- 同步：Linux done（§5.7 三种出口共用一条流程；data_page new() 的 export popover + export_ui::begin）
- 实现备注：挂在「导出 ▾」按钮上的 GtkPopover（popup surface，窗口自渲染截图拍不到）；点任一项先收起菜单再执行。三项都进入 export_ui::begin 这同一条流程：范围 → 文件 → 确认 → 覆盖 → 进度 / 取消 → 完成，差别只在写出的格式与文件过滤器。菜单里没有第三条之外的路径（没有「复制到剪贴板」「导出全部结果」这类重复出口）。查询标签的结果区复用同一份「导出 ▾」（入口在 tabs 的查询结果区、由本区记录）：只有 CSV / INSERT 两项，**没有**「转储表」；范围固定为已加载行、没有「整张表」，不重跑 SQL；没有已加载行时提示「没有可导出的行」+「当前标签没有已加载的行：先执行查询，再导出。」。

### `data_page.export_menu.csv` 「导出 CSV…」

kind `item` · status `active` · parent `data_page.export_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「导出 CSV…」
- 同步：Linux done（§5.7 CSV 出口 / §6.1 导出口径与公式注入防护；export_ui::begin(ExportKind::Csv)）
- 实现备注：范围对话框问当前页与整张表，默认当前页，整张表必须显式选择。确认标题「导出已加载的 N 行为 CSV？」/「导出整张表为 CSV？」；正文给写出口径（空字段 = SQL NULL、空字符串写成空引号串、二进制写占位 (BLOB n bytes)、Decimal 保留原文、日期时间 ISO 8601）与截断警告。公式注入防护开关**只给 CSV**（文本值以等号 / 加号 / 减号 / at 号 / 制表符 / 回车开头时前置单引号），默认开启。文件对话框标题「导出为CSV」、过滤 *.csv、默认 sqarvo-export.csv；目标已存在时再确认一次，默认不覆盖。当前页导出的数据口径：写页面当前的值，含未提交编辑（对话框里写明）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有已加载行 | 选范围 → 选文件 → 确认「导出已加载的 N 行为 CSV？」→ 写文件；完成提示写已写行数、截断数与防护开关状态；在文件对话框取消则不写文件、不报错 | `src/export_ui.rs:453、src/export_ui.rs:522、src/export_ui.rs:711` |
| click | 计划为空或总数为 0（明确知道 0 行） | 提示「没有可导出的行」，不进入文件选择器（转储表例外：空表也允许，只写建表语句） | `src/export_ui.rs:453` |

### `data_page.export_menu.insert` 「导出为 INSERT 语句…」

kind `item` · status `active` · parent `data_page.export_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「导出为 INSERT 语句…」
- 同步：Linux done（§5.7 INSERT 写出口径；export_ui::begin(ExportKind::Insert) + export 的 sql_literal / write_insert）
- 实现备注：范围同样问当前页 / 整张表；确认标题「导出已加载的 N 行为 INSERT 语句？」/「导出整张表为 INSERT 语句？」。写出口径：一行一条 INSERT INTO 目标表 (列清单) VALUES (值清单);（LF 行尾）；NULL 写 NULL、空串写空引号串；数字 / Decimal 原文；日期时间加引号 ISO 8601；BLOB 写 NULL（客户端拿不到原始字节），文件末尾注释写明数量。标识符与字符串按驱动：SQLite 双引号，MariaDB 反引号 + 反斜杠转义。文件对话框标题「导出为INSERT 语句」、过滤 *.sql、默认 sqarvo-insert.sql。目标表名（查询结果导出时才需要推导）：只接受**唯一一个**顶层 FROM（跳过字符串 / 注释 / 括号）并取 `[库.]表` 的最后一段；JOIN 外的第二个 FROM、`FROM (`、没有 FROM 时推断不出，写占位名 `result` 并在文件头加注释「结果集不携带来源表名：目标表写成占位名 result，导入前请替换成真实表名。」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有已加载行 | 选范围 → 选文件 → 确认 → 写 SQL 脚本；不做公式注入防护（SQL 不经过表格软件）；截断单元格在文件末尾追加一行注释 | `src/export_ui.rs:453、src/export_ui.rs:711、src/export.rs:348` |

### `data_page.export_menu.dump` 「转储表…」

kind `item` · status `active` · parent `data_page.export_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「转储表…」
- 同步：Linux done（§5.7 转储表内容；export_ui::begin_object_dump + run_whole_table_export）
- 实现备注：**不问范围**（天然整表）：确认标题「转储表 <表名>？」。文件内容：文件头注释（Sqarvo 导出：表名与列数）→ 可选注释 → 建表语句（现场 GetDdl；exact=false 时注明「建表语句由客户端拼装」）→ 每行一条 INSERT；列名与建表语句在同一后台线程、同一会话里读。空表也允许导出，只写建表语句；默认文件名 <表名>.sql，文件对话框标题「导出为SQL 脚本」。同一个动作另有对象树表节点右键菜单入口（登记在 object_tree 区），两条入口共用 export_ui::begin_object_dump。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 已打开表（有浏览会话） | 确认「转储表 <表名>？」→ 选文件 → 进度对话框（已写行数 / 截断单元格数）+「取消」；取消或失败不留半截文件与临时文件 | `src/export_ui.rs:506、src/export_ui.rs:1242、src/export_ui.rs:1471` |

### `data_page.badges` 表头上方徽标行

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`gap_sm`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_on_soft`
- 无障碍：可访问名「表状态徽标」
- 同步：Linux done（§5.1 表头上方徽标行 / §5.6 权限不足；update_badges + show_failure）
- 实现备注：六类徽标：只读 / 无主键 / 视图（写种类名）/ 无权限 / 未统计 / 驱动不支持排序筛选；每枚带解释性 tooltip，有任意一枚时整行可见。不随状态变化消失：用户不用去看禁用的按钮猜为什么不能编辑。「无权限」在读取或提交返回 permission 类错误码时置上，不只靠一句状态行。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 打开表（可叠加权限 / 统计 / 能力位结果） | 按条件逐枚追加：只读（tooltip 给具体原因）、无主键（「无主键表：不能稳定定位行，因此只读；浏览、排序、筛选与计数仍可用」）、视图（「视图：数据来自查询定义，编辑受限…」）、无权限（「当前账号对该对象没有权限…」）、未统计（「精确计数超过预算，已跳过…」）、驱动不支持排序/筛选 | `src/data_page.rs:4476` |
| load | 读取 / 提交返回 permission 类错误 | 置上「无权限」徽标并常驻，直到重新打开表 | `src/data_page.rs:4609` |

### `data_page.grid` 数据网格（表格外框 · 行号列 · 表头 · 数据行）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`column_header_min_height`、`row_number_width`、`column_width`、`frame_border_width`、`radius_lg`、`grid_padding_start`；颜色 `theme_bg_color`、`theme_fg_color`
- 无障碍：可访问名「数据网格」
- 原生控件：gtk:GtkColumnView
- 自绘：WinUI 3 没有 GtkColumnView 的等价控件：GridView / ListView 的虚拟化不与列宽拖动、行号列、单行表头这三者组合，因此这一层用自绘虚拟化行区自己管理行号、列宽与表头（§7.1）。
- 同步：Linux done（§5.4 网格结构 / §5.5 offset 分页与总数三态；build_columns + navigate + scroll_to_top）
- 实现备注：结构：行号列（固定 row_number_width，行号 = offset + 行下标 + 1，**不是**主键）+ 单行表头 + 数据行。列宽默认 column_width；列右缘 column_resize_hit 命中热区（指针 SizeWestEast）可拖动改列宽，双击把手按**本页内容**自适应（表头文案与已加载值里最长文字 + 单元格内距 column_cell_padding），下限 column_min_width、上限 column_width 都取 metrics；拖过 / 自适应过的列记为用户宽度，表头与所有行共用同一份列宽状态，翻页 / 刷新 / 重读不覆写、换表重置。表头条带 26 内容 + 上下 2 内距（column_header_min_height）；底色取 theme_bg_color，文字与数据行同档。横向滚动条常驻可见（关掉 overlay 自动淡出），宽表只横向滚动、不挤压右侧信息面板；窗口最大化后横向滚动位置回到第一列。滚动到底**不**自动取下一页（与 offset 分页互斥），翻页只由状态栏分页器驱动。页码与总数只在状态栏分页器出现一次（status_bar.row1.pager / .summary）：三态是「共 N 行」/「约 N 行」（估计值必须带「约」）/「共 ? 行」（总数未知或计数超时写问号，不编造数字；Linux 未知时只写页码，Windows 写「共 ? 行」）；计数超预算时写「未统计（超出计数预算）」并出现「未统计」徽标。每页 500 行（TABLE_PAGE_SIZE，实际 page_size 随第一页返回）；翻页 = 整页替换 + 回到顶部。刷新（状态栏 ⟳ / 表菜单）走**同一条** dirty_guard：有脏行 / 标删 / 新行时被拒绝并说明原因，按钮禁用时 tooltip 附同一句——数据页只给谓词与文案，按钮本身在 status_bar。末页需要总页数：总数未知时 can_last = false（按钮禁用），点了也不发请求，状态行写「<动作>不可用（没有对应页）」；驱动不声明 totals 与非精确计数同一写法（不显示总数）。新行占位行是**网格里的最后一行**（行号列写 ＋，行号 0 = 尚未落库）：提交成功后按返回的主键定位再赋真实行号；整批全部省略时提交即 INSERT DEFAULT VALUES（自增 / 默认值列由数据库填）。空表 / 筛选无结果 / offset 越界这类 0 行结果照常绘制表头与网格、只是没有数据行；不进占位页（否则新增行的草稿行被挡住）。0 行时「回到顶部」只归零滚动位置，不调用 GtkColumnView::scroll_to(0)，避免 `pos < n_items` 断言。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 第一页 0 行（空表 / 筛选无结果 / offset 越界） | 照常绘制列名表头与网格、只是没有数据行；新增行仍可用，点状态栏「新增行」出现 ＋ 草稿行 | `src/data_page.rs:4443` |
| click | 可编辑且没有待插入行（入口：状态栏行操作组 +，工具条与菜单里都没有） | 在末尾追加一行草稿并进入可编辑态：有默认值的列显示数据库默认值原文、自增 / 无默认列写「省略（交给数据库默认值）」；已有待插入行时先选中它并写「已经有一行待插入的新行：先提交或放弃它」，不再追加；每格双击填值，点「提交」才发 InsertRows | `src/data_page.rs:3155、src/data_page.rs:5095、src/data_page.rs:5156` |
| load | 打开表 / 翻页 / 刷新（无未提交更改） | 整页替换并写「正在读取 offset N …（<动作>）」；成功后回到第一行；翻页中再翻页写「上一页仍在读取：稍候再翻页」，不发第二次请求 | `src/data_page.rs:3057、src/data_page.rs:4350、src/data_page.rs:4467、src/backend/table.rs:28` |
| load | 有脏行、标记删除或新行草稿 | 翻页 / 排序 / 筛选 / 刷新一律拒绝，写「有未提交的更改：先提交或「放弃更改」再翻页/排序/筛选（翻页会替换整页数据）」 | `src/data_page.rs:3098、src/data_page.rs:3057` |
| scroll | 滚到本页最后一行 | 不自动取下一页（offset 分页互斥），页脚不出现「加载更多」 | `src/data_page.rs:4467` |

### `data_page.grid.header` 列头（列名 · 原始类型 · 排序标记）

kind `row` · status `active` · parent `data_page.grid`

- 样式：尺寸 `column_header_min_height`、`column_width`、`font_ui`、`font_weight_title`；状态 hover=`sqarvo_brand_soft`
- 无障碍：可访问名「（行内容：列名 · 原始类型）」
- 同步：Linux done（§5.3 表头排序标记 / §5.4 点列头；refresh_header_tooltips + header_title_text；Linux 表头无 PK 标记（见 impl_notes））
- 实现备注：表头只有**一行**：标题文本是「列名 · 原始类型」（类型为空时只写列名）；整个标题可点，不是只有文字。主键标记只出现在声明了它的实现里（Windows 写「· PK」）；Linux 的表头不带主键标记，主键进信息面板的列页与徽标。拖宽把手在列右缘 6px 内，落在那里的松开不算「点击列头」。排序标记跟在标题末尾（▲ / ▼ / ▲2），与筛选面板共用同一份排序状态；表头本身不参与排序。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该列存在（松开点不在列右缘 6px 内） | 信息面板切到该列的列页（与树上列节点同一个页面，不重发 RPC）；**不改排序** | `src/data_page.rs:4086、src/data_page.rs:4129` |
| load | 打开表 / 排序变化 / 列显隐变化 | 标题写「列名 · 原始类型」，有排序键时追加 ▲ / ▼ / ▲2（数字是优先级）；tooltip「点击查看列属性：<列名>」 | `src/data_page.rs:775、src/data_page.rs:6335` |
| drag | 指针落在列右缘的拖宽把手 | 改列宽并记为**用户宽度**（翻页 / 刷新 / 自动列宽都不再覆写）；双击把手按内容自适应同样记为用户宽度 | `src/data_page.rs:6051、src/data_page.rs:6072` |

### `data_page.grid.row_number` 行号列（行号 / 脏行 / 标删 / 新行）

kind `cell` · status `active` · parent `data_page.grid`

- 样式：尺寸 `row_number_width`、`font_status`、`font_weight_ui`；颜色 `accent_bg_color`
- 无障碍：可访问名「（行内容：行号或未提交标记）」
- 同步：Linux done（§5.4 行号标记；build_columns 的 number_factory（●N / ✕N / ＋ 与四条 tooltip））
- 实现备注：宽度固定 row_number_width、右对齐、等宽字体、色弱一档；行号不是主键，也不可拖宽。四种写法互斥，按优先级：已删除（服务端报告行已不在）写 ✕N → 新行占位写 ＋ → 已标记删除写 ✕N → 有未提交更改写 ●N → 普通行号。放弃（状态栏行操作组 ↶ / 放弃未提交的更改）后 ●N / ✕N / ＋ 都回到普通行号；一次成功提交后被删行整行消失、脏行变普通行号。●N 的颜色是系统强调色（语义色），脏单元格的浅底是品牌浅档——两者刻意不同：一个是「有未提交的更改」，一个是「改过的格子」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 行已加载 | 普通行写行号（tooltip「第 N 行」）；脏行写「●N」（「第 N 行 · 有未提交的更改」）；标删写「✕N」（「第 N 行 · 已标记删除（提交前可撤销）」）；新行写「＋」（「新行 · 尚未提交（双击单元格填值）」）；被其它会话删除的行同样写「✕N」并给删除线与压暗 | `src/data_page.rs:5972、src/data_page.rs:5978、src/data_page.rs:5984、src/data_page.rs:5966` |
| hover | 行号格有标记 | tooltip 说明这一格为什么不是普通数字（有未提交的更改 / 已标记删除（提交前可撤销）/ 已删除 / 新行尚未提交） | `src/data_page.rs:5995` |

### `data_page.grid.cell` 数据单元格（值 · NULL / 空串 · 脏色）

kind `cell` · status `active` · parent `data_page.grid`

- 样式：尺寸 `row_height_data`、`column_width`、`font_data_cell`、`font_weight_cell`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_on_soft`、`sqarvo_brand_mid`；状态 selected=`sqarvo_brand_mid`
- 无障碍：可访问名「（行内容：单元格值）」
- 同步：Linux done（§5.4 单击 / 双击 / 脏行底色 / NULL 与空串；cell_factory bind + edit_block_reason）
- 实现备注：NULL 写「NULL」（弱色斜体，tooltip「SQL NULL（与空字符串不同）」）；空串**不能**与 NULL 长得一样：Linux 写空引号串，Windows 留空（§7.1 明确「空串保持空白」）。数值与二进制右对齐（便于比较），文本左对齐；超长文本省略号截断，全文进 tooltip。脏单元格是品牌浅档实底 + 对应深色前景（成对写，选中行里也不继承白字）；标记删除的行整行删除线 + 压暗。服务端截断的值 tooltip 追加「该值被截断：不能编辑（提交截断值会写坏数据）」，且不允许进入编辑。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 行存在 | 只选中行（Ctrl / Shift 多选），不改数据；状态栏选中数出现 N selected | `src/data_page.rs:1255、src/data_page.rs:6347` |
| double_click | 单元格可编辑 | 打开编辑（见 data_page.editor）；不可编辑时状态行给具体原因：「该行已标记删除：撤销删除后才能编辑」「列 X 的值被服务端截断…」等，不弹空编辑器 | `src/data_page.rs:6372、src/data_page.rs:5577、src/data_page.rs:5647` |
| key Esc | 网格有焦点、有选中行 | 清空行选中（不影响 SQL 编辑器与对话框） | `src/data_page.rs:1266` |

### `data_page.editor` 单元格编辑器（标题 · 输入框 / 日期时间选择器 · 设为 NULL · 应用 / 取消）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`
- 无障碍：可访问名「编辑单元格」
- 原生控件：gtk:GtkPopover（编辑器）+ GtkEntry + GtkMenuButton + GtkPopover（选择面板）+ GtkCalendar + 3×GtkSpinButton
- 自绘：GTK4 0.11 / libadwaita 0.9 没有时间选择器，也没有 AdwDateRow / AdwTimeRow / 日期选择对话框；日期用原生 GtkCalendar、时间用三个原生 GtkSpinButton 拼成复合件（L2，登记即可无需 ADR）。
- 同步：Linux done（§5.4 双击单元格 / 编辑提示 / 应用失败就地显示；open_cell_editor + open_editor。tests/ui/linux/test_10_data_page_insert_editor.py::test_insert_editor_omit_unchecked_by_default：隔离 X11 双击合成表的草稿单元格，AT-SPI 核对省略默认未勾选及可输入，取消保留原草稿且零写入。）
- 实现备注：同一个编辑器被三处复用：数据网格、单行「字段 · 值」视图的值列、查询结果区的单表编辑（open_cell_editor 是唯一实现）。标题「编辑 <列> · <类型>」；输入框 +「设为 NULL（SQL 空值）」勾选 + 提示行 +「应用」/「取消」。编辑器内容宽 320 是代码常量，未进 metrics。进入编辑时值**不被全选**、光标停在末尾（双击的目的是改值；全选会让选中底色压住原值，误按一个字符就整段替换），Ctrl+A 仍可全选。可空列提示「空文本在文本列表示空字符串；要写 NULL 请勾选上方选项」；不可空列提示「该列不允许 NULL：请输入合法值」且 NULL 勾选禁用。值转换失败时错误就地显示、编辑框不关（可以在原处改）。写库时机：应用只改**界面草稿**，不落库；真正写库只有「提交」（ApplyRowChanges）。新行用**另一套**编辑器（open_insert_editor）：多一个「省略该列（交给数据库默认值 / 自增）」态，与「设为 NULL」/ 文本三态互斥，省略的列不出现在 InsertRows 请求里；有默认值的列在省略态显示默认值原文（dim 样式 + 提示），不猜实际值。新行编辑器打开时「省略该列」**默认不勾**（双击意在填值），输入框可用；从省略态进入但没输入任何值就点应用时仍维持省略，不悄悄写成空串 / NULL。提示行固定用可空 / 不可空那一条，不取省略态的长说明——后者换行会把弹层撑高，勾选前后大小不一致；长说明仍在网格单元格 tooltip 里。日期时间字段（强类型 Date/Time/Datetime/Timestamp，以及 SQLite 声明为这些类型、实际以 Text 返回的列）用 `GtkEntry` + `GtkMenuButton`（`linked` 组合）：点日历按钮弹出独立 `GtkPopover`，面板里 `GtkCalendar` 选年月日、分隔线下三个带 Hour / Minute / Second 标题的 `GtkSpinButton` 选时分秒、底部「现在 / 应用」。面板打开时按输入框文本同步控件；旋钮显示两位、范围 0–23/0–59；手动输入格式不合法时输入框就地标红（空文本例外，保留新行省略语义）。它只是文本框的另一入口，写回文本按字段投影 Date=YYYY-MM-DD / Time=HH:MM:SS / Datetime·Timestamp=YYYY-MM-DD HH:MM:SS，NULL / 省略 / 提交路径完全不变。面板没有额外说明文字；日历保持自然宽度并居中，时间行旋钮等宽，两者左右对齐。选择器精度到秒：原值带小数秒或时区时仍可直接编辑文本框保留；无法解析为日期时间的原值（SQLite 列里存了任意文本）会以当前时间初始化，并显示点名原值的警告说明应用会覆盖；新行空初值只初始化选择器、不写文本框，「省略该列」的默认语义不变。勾选「设为 NULL」/「省略该列」时选择器与文本框同步禁用。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 输入合法（或勾选了设为 NULL） | 值写进界面草稿：单元格加脏色、行号变「●N」、状态圆点转强调色；编辑框收起。写库仍要等「提交」 | `src/data_page.rs:582、src/data_page.rs:5705、src/data_page.rs:5742` |
| click | 编辑框已打开 | 「取消」直接收起、不改草稿（与从未打开等价） | `src/data_page.rs:582` |
| key Esc | 编辑框已打开 | 收起编辑框并放弃本次输入（Linux：popover 的 Esc 收起等同取消）；不写草稿、不写库 | `src/data_page.rs:582、src/data_page.rs:658` |
| key Enter | 就地编辑态（Windows 口径） | 把输入提交到**本地暂存**（不落库）、单元格转脏；Linux 的编辑框是 应用 / 取消 两个按钮的形态，Enter 不提交（见 platform_notes.linux），最终草稿与提交口径一致 | `src/data_page.rs:582、src/data_page.rs:5742` |
| key F2 | 网格有选中单元格 | 进入编辑（与双击同一入口）；Linux 未绑 F2 —— 双击是唯一编辑入口（见 platform_notes.linux） | `src/data_page.rs:5577` |
| click | 编辑框已打开、点了编辑框以外的任何位置 | Linux：popover 自动收起，等同取消（没点「应用」就什么都没改）；Windows：按「暂存」处理，输入进本地草稿 | `src/data_page.rs:590、src/data_page.rs:582` |
| load | 日期时间字段的编辑器打开（含新行） | 点 GtkMenuButton 日历图标弹出选择面板：GtkCalendar 选日期、三个 GtkSpinButton 选时分秒（Hour / Minute / Second），底部「现在」填当前时间、「应用」写回文本框并收起；面板打开时按当前输入同步控件，手动输入格式错误就地标红；勾「设为 NULL」/「省略该列」时同步禁用 | `src/data_page.rs:590、src/data_page.rs:3587` |
| load | 原值无法解析为日期时间 | 选择器填当前时间，文本框同步写规范文本；另显示点名原值的警告「原值「X」无法解析为日期时间，已填入当前时间；点「应用」将覆盖原值」，不静默覆盖 | `src/data_page.rs:627、l10n/data_page.toml:editor_picker_unparseable` |

### `data_page.row_menu` 数据行右键菜单

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`
- 无障碍：可访问名「数据行菜单」
- 原生控件：gtk:GtkPopoverMenu
- 同步：Linux done（§5.8 数据行右键菜单；show_row_menu + context_menu 模块）
- 实现备注：原生 GtkPopoverMenu（与系统右键菜单同一外观与键盘操作）；每个条目一个 gio::SimpleAction，动作组挂在**菜单自己**身上，名字一定解析得到。手势挂在**行控件**上（label → cell → row 爬两层，一行只挂一次，用 sqarvo-row-menu-host 类标记）：单元格文字、列分隔线、行分隔线、末列右侧空白都算右键位置；表格下方空白不属于任何行、不响应。未选中的行先被选中，已多选时保留多选，菜单动作永远作用于当前选中集。「复制为」的二级菜单展开在条目**右侧**（默认向下会压住下方条目）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| right_click | 已打开表 | 按点击位置弹出菜单：「删除行」、「复制为 ▸ 复制为 INSERT」；不可编辑时「删除行」灰掉（delete_available = false），「复制为 INSERT」仍可用 | `src/data_page.rs:2157、src/data_page.rs:2178、src/data_page.rs:2182、src/context_menu.rs:71` |
| click | 菜单打开中 | 点菜单以外的任何地方（其它行、工具条、外面板）一律收起 | `src/context_menu.rs:77` |

### `data_page.row_menu.delete` 「删除行」

kind `item` · status `active` · parent `data_page.row_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「删除行」
- 同步：Linux done（§5.8 删除行；delete_selected_row + delete_note）
- 实现备注：与状态栏 − / RowOp::Delete 完全同一方法（delete_selected_row），可用性同一谓词（delete_available）。只标记待删除，不立刻写库；新行占位行不在删除范围。结果不占可见文案，只写状态圆点 tooltip 末段「最近一次操作：…」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 可编辑且有选中行 | 把选中行标记为待删除：行号变「✕N」、整行删除线 + 压暗；圆点注记「第 N 行已标记删除（提交前可撤销）」或「已标记删除 N 行（批量提交前可撤销）」，一次「提交」才真正写库 | `src/data_page.rs:5380、src/data_page.rs:5124、src/data_page.rs:5442` |

### `data_page.row_menu.copy_insert` 「复制为 ▸ 复制为 INSERT」

kind `item` · status `active` · parent `data_page.row_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「复制为 INSERT」
- 同步：Linux done（§5.8 复制为 INSERT；copy_selected_as_insert + export 的 multi_row_insert）
- 实现备注：把选中行合成**一条多值** INSERT（目标表 + 列清单 + 多组 VALUES，行间换行）写进系统剪贴板，用选中行的**当前值**（含未提交编辑）。BLOB / 非有限浮点写 NULL，并在末尾附一行 sqarvo 注释；标识符按该连接驱动引号。只读也要能导出数据：本条目在只读连接下仍可用。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有选中行 | 合成多值 INSERT 写入剪贴板；成功后圆点注记「已复制 N 行为 INSERT 语句」，失败写 warning 说明原因 | `src/data_page.rs:2031、src/data_page.rs:2182` |

### `data_page.detail` 单行「字段 · 值」展示形式

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`frame_border_width`、`radius_lg`
- 无障碍：可访问名「字段 / 值视图」
- 同步：Linux done（§5.9 展示形式切换；toggle_detail_view / set_detail_mode / fill_detail / detail_cell_factory）
- 实现备注：与网格共用同一块表格外框（sqarvo-table-frame），由视图栈切换；进场是 120ms 交叉淡入。两列：字段（220，可拖）与值（自适应）；行 = 该行的每个字段，值取**当前值**（含未提交编辑），显示文本与网格**同一份**（NULL 为 NULL；空串在 Linux 写空引号串、Windows 留空）。220 是代码常量，未进 metrics。只讲**第一个**选中的行（多选时）；日志「[data] 展示形式：字段 / 值（栈当前页 detail）」。入口是状态栏分页器**前方**的 toggle（登记在 status_bar.row1.detail_toggle，属状态栏区）；本条目只登记这块内容与它的编辑 / 复位语义。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有选中行、当前是按行浏览（入口：状态栏的展示形式 toggle） | 表格外框内换成「字段 | 值」两列，只讲第一个选中行；按钮呈按下态（选中态由数据页快照推回，不在按钮自己记） | `src/data_page.rs:2074、src/data_page.rs:2114、src/data_page.rs:1309` |
| click | 已在「字段 / 值」视图 | 再按一次同一按钮切回按行浏览（同一动作幂等切换），网格原样回来，选中与脏行不变 | `src/data_page.rs:2074、src/data_page.rs:2094` |
| double_click | 「值」列、该值可改（不是只读连接 / 只读事务 / 截断列 / 已标记删除的行） | 打开与网格同一个编辑器（行号 = 详情当前行、列号 = 字段下标）；改完立即按当前值重填整页（日志「详情第 N 行：id=2 · name=…」），提交仍走「提交」与脏行守卫 | `src/data_page.rs:700、src/data_page.rs:765` |
| load | 没有选中行 / 换表 / 重新读取 / 选中集清空 | 没有选中行时不切视图，状态行「先选中一行，再切换到「字段 / 值」视图」；换表与选中集清空**强制**回到按行浏览并清空详情数据 | `src/data_page.rs:2074、src/data_page.rs:4304` |

### `data_page.filter_panel` 筛选与排序面板（草稿 · 应用 / 清空）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`；尺寸 `filter_panel_width`、`filter_column_width`、`radius_lg`、`row_height_data`
- 无障碍：可访问名「筛选与排序」
- 原生控件：gtk:GtkPopover
- 同步：Linux done（§5.3 打开 / 关闭 / 应用 / 清空 / 草稿模型；open_filter_panel + apply_browse_options + clear_filter）
- 实现备注：挂在漏斗按钮上的 GtkPopover（autohide=false），锚点落在上沿的黄金分割点（0.618，偏右）；面板宽 filter_panel_width。面板里的改动是**草稿**：点「应用」才同时生效（筛选 + 排序一起提交，并回到第 1 页）；关闭面板丢弃草稿。标题右侧写「已应用」/「有未应用的更改」（warning + tooltip「点应用才生效；关闭会丢弃」）。面板里只有一种字号（12px，与对象树一致）：正文、按钮、下拉、输入框、说明字统一，层级只靠字重与颜色；列表行 24 高。底部只有一个调整区：分隔线 + 右对齐的「清空」「应用」；文案不带作用域，也不写说明段落。两个区：筛选条件（列 + 算子 + 值 → 条件行，多条按 AND）与排序（列 + 方向 → 排序键，列表顺序即优先级）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 打开面板 | 把**已应用**的筛选与排序复制成草稿并重建列下拉；状态文案回到「已应用」；错误提示清除 | `src/data_page.rs:2425、src/data_page.rs:2960` |
| click | 草稿与已应用状态不同 | 「应用」：未知列或算子缺值则面板内报错 + 状态行 error，不发请求；成功则写回唯一状态、回到第 1 页、重发浏览请求、状态行「筛选与排序已应用（N 条条件 · M 个排序键）· 回到第 1 页」；**面板不关闭** | `src/data_page.rs:2982` |
| click | 有未提交更改 | 「应用」被拒绝，状态行「有未提交的更改：先提交或「放弃更改」再翻页/排序/筛选（翻页会替换整页数据）」，草稿保留 | `src/data_page.rs:2982、src/data_page.rs:3098` |
| click | 面板已打开 | 「清空」只清**草稿**（条件与排序键），仍需点「应用」才生效；状态行「条件与排序已清空（点「应用筛选与排序」恢复全表）」 | `src/data_page.rs:3040` |
| click | 面板已打开 | 点 ✕（或再点漏斗）关闭面板，未应用的改动丢弃 | `src/data_page.rs:2450、src/data_page.rs:1790` |
| key Esc | 面板内有行内编辑态 | 先收起行内编辑；没有编辑态时才关面板（一个 Esc 不该把整张面板关掉） | `src/data_page.rs:1809` |

### `data_page.filter_panel.condition_composer` 条件编辑组（列 · 算子 · 值 · 添加条件）

kind `container` · status `active` · parent `data_page.filter_panel`

- 样式：尺寸 `filter_column_width`、`row_height_data`
- 无障碍：可访问名「添加筛选条件」
- 同步：Linux done（§5.3 添加筛选条件；add_filter_condition + read_condition_editor）
- 实现备注：列下拉固定 filter_column_width（不随面板拉伸）；算子下拉只写符号（= ≠ ≤ LIKE IS NULL 等）且**不显示下拉箭头**；值是单行输入框。算子选 IS NULL / IS NOT NULL 时不需要值：输入框禁用并清空。点「添加条件」把这一组追加到**草稿**（不改正在显示的数据），值输入框清空，条件出现在下方清单。校验先在本地说清楚：缺值报「<算子> 需要一个值（列 X · 类型）」，未知列同样在面板内报错，不发请求。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有列清单、算子需要值且值非空（或算子为 IS NULL 类） | 条件追加到草稿并渲染成条件行（多条按 AND 组合）；日志「筛选条件已加入草稿：<列> <算子> <值>（尚未应用）」 | `src/data_page.rs:2526、src/data_page.rs:2585` |
| click | 算子下拉可见 | 只列出符号算子；选 IS NULL / IS NOT NULL 后值输入框禁用并清空（这两条不需要值） | `src/data_page.rs:1417、src/data_page.rs:2557` |
| click | 算子需要值但值被清空 | 不加入条件，面板内红字「<算子> 需要一个值（列 X · 类型）」 | `src/data_page.rs:2557、src/data_page.rs:2569` |

### `data_page.filter_panel.condition_row` 条件行（列 · 算子 · 值，可就地编辑）

kind `row` · status `active` · parent `data_page.filter_panel`

- 样式：尺寸 `row_height_data`；颜色 `sqarvo_brand_soft`；状态 hover=`sqarvo_brand_soft`
- 无障碍：可访问名「（行内容：列 算子 值）」
- 同步：Linux done（§5.3 编辑条件；condition_label_row / condition_editor_row / commit_condition_edit）
- 实现备注：只读态是一行文字（列 + 符号算子 + 值）+ 行尾一个 ✗；点这一行**就地展开**为三件套编辑器，不弹二级对话框。编辑态与只读态同一套外观：同底色（不加强调色）、同行高、同字号，只把文字换成控件。已加的条件是淡底色 + 圆角的实体条（品牌浅档的 55%，悬停变成浅档实色）；行尾始终只有一个 ✗。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该条件行不是编辑态 | 就地展开为列 / 算子 / 值编辑器；回车或 ✓ 提交，✗ 或点面板空白处收起 | `src/data_page.rs:2722、src/data_page.rs:2642` |
| key Enter | 条件行处于编辑态 | 提交这一行修改（只改草稿），收起编辑器；校验失败时不收起并在面板内报错 | `src/data_page.rs:2766` |
| click | 条件行存在 | 点行尾 ✗ 从草稿里删掉这条条件（仍需点「应用」才影响数据） | `src/data_page.rs:2808、src/data_page.rs:2609` |

### `data_page.filter_panel.sort_composer` 排序编辑组（列 · 方向 · 添加排序）

kind `container` · status `active` · parent `data_page.filter_panel`

- 样式：尺寸 `filter_column_width`、`row_height_data`
- 无障碍：可访问名「添加排序键」
- 同步：Linux done（§5.3 排序键；add_sort_key）
- 实现备注：列下拉与筛选同宽（filter_column_width）；方向下拉只写 asc / desc（SQL 词，不翻译、不加方向图标、不显示箭头）。点「添加排序」把键加入**草稿**；同一列再加一次只改方向，不产生重复键。驱动不支持排序时面板内报「当前驱动不支持排序」，不加入。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有列清单且驱动支持排序 | 列 + 方向加入草稿的排序键列表（列表顺序即优先级）；日志「排序键已加入草稿：<列> <asc|desc>（尚未应用）」 | `src/data_page.rs:2821、src/data_page.rs:2851` |
| click | 驱动排序能力位为 false | 不加入，面板内报「当前驱动不支持排序」 | `src/data_page.rs:2829` |

### `data_page.filter_panel.sort_row` 排序键行（列名 · asc / desc · 上移 / 下移 / 删除）

kind `row` · status `active` · parent `data_page.filter_panel`

- 样式：尺寸 `row_height_data`；颜色 `sqarvo_brand_soft`；状态 hover=`sqarvo_brand_soft`
- 无障碍：可访问名「（行内容：列名 asc 或 desc）」
- 同步：Linux done（§5.3 排序键行；render_sort_keys + toggle_sort_key_direction / move_sort_key / remove_sort_key）
- 实现备注：行文本是「列名  asc|desc」，**不写序号**：从上到下就是优先级，序号与上移 / 下移表达的是同一件事。行尾三个线条图标按钮：上移（第一条禁用）、下移（最后一条禁用）、删除（✕）；与面板其它 ✗ 同一风格。改动只进草稿，仍需点「应用」；与表头的 ▲ / ▼ 标记共用同一份排序状态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 排序键行存在 | 点列名切换 asc ↔ desc（只改草稿），列表重排并刷新标题状态 | `src/data_page.rs:2916、src/data_page.rs:2878` |
| click | 该键不是第一条 / 不是最后一条 | 上移 / 下移把它在列表里挪一位，列表顺序即 SQL 的排序优先级 | `src/data_page.rs:2929` |
| click | 排序键行存在 | 点 ✕ 删除这个排序键（仍需「应用」才生效） | `src/data_page.rs:2947` |

### `data_page.txn_chooser` 「开始事务」卡片（隔离级别 · 只读）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`；尺寸 `radius_lg`
- 无障碍：可访问名「开始事务」
- 同步：Linux done（§5.2 打开「开始事务」卡片；open_txn_chooser + render_isolation_options + confirm_begin_transaction）
- 实现备注：页内模态卡片（不是独立窗口，也不是 popover）：标题「开始事务」+ 隔离级别选项 +「只读」勾选 + 底部 取消 / 开始事务。不支持的隔离级别**在开始前**就禁用并标「· 不可用」，说明行给原因——不让用户点完才知道不支持。每次打开都按当前驱动重建选项（换库 / 换驱动后不能沿用旧能力表）；卡片宽 460 是代码常量，未进 metrics。打开卡片前已经过 begin_transaction 的自检（无事务、未断开、无未提交更改），被挡住时根本不打开。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 工具条「开始事务」可用 | 卡片出现：隔离级别（默认选中第一个可用项）、「只读」勾选、按钮「开始事务」/「取消」 | `src/data_page.rs:3710、src/data_page.rs:3719` |
| click | 已选可用隔离级别 | 「开始事务」按所选级别（及只读勾选）发 RPC；在途提示「开始事务：<隔离级别> · 读写」，事务状态落定后清除，圆点转棕 | `src/data_page.rs:3821、src/data_page.rs:1762` |
| click | 卡片已打开 | 「取消」或 ✕ 收起卡片，不开始事务、不发 RPC | `src/data_page.rs:1766` |

### `data_page.placeholder` 占位页（尚未打开 / 正在读取）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`
- 无障碍：可访问名「数据页占位」
- 同步：Linux done（§5.6 打开表（加载中）；begin_open 的占位页分支）
- 实现备注：两段文案互斥：尚未打开表 / 正在读取表数据；表一旦打开（含 0 行）就走 data_page.grid，空表不再占这一页。「空」与「错」严格区分：0 行照常画表头、只是没有数据行；错误走横幅 + 逐行错误列表，不占这一页。图标与说明只用 dim 文字，不放大色块。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 打开表 / 翻页请求已发出 | 「正在读取表数据」+「等待服务端返回第一页…」 | `src/data_page.rs:4339、src/data_page.rs:1331` |

### `data_page.banner` 页首横幅（只读 / 断连 / 错误）

kind `control` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；颜色 `warning_color`、`sqarvo_brand_soft`、`sqarvo_brand_on_soft`
- 无障碍：可访问名「数据页横幅」
- 同步：Linux done（§5.6 只读 / 断连；show_read_only_banner + mark_disconnected）
- 实现备注：只读横幅常驻（直到刷新事务状态）：正文是「<标题>：<解释>」，例如「只读：该表没有主键：无主键表只有无稳定排序的浏览顺序，无法稳定定位行，因此不能编辑…」。断连横幅写明「未提交的 N 行变更已丢弃；数据库事务已由后端强制回滚」（无脏行时省略中间一句），已加载数据**保留**为只读。仅后端失效 / 断连横幅带一枚「重启 Sqarvo」按钮（重启确认属 shell 区）；插入回滚、单批钳制、只读、行已删除等普通失败只给一次性说明，不提供重启按钮——重启后端并不能解决这些失败。横幅不替代徽标行与状态行：三者同时在，各自说明不同的层级（原因 / 能力 / 这一刻的动作结果）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 服务端标记只读（表级 / 视图 / 无主键 / 只读事务） | 横幅常驻显示只读原因；编辑类按钮禁用；过程消息 tooltip 给同一原因（不能只靠禁用暗示） | `src/data_page.rs:6983、src/data_page.rs:4413` |
| load | 会话丢失或后端断开 | 丢弃未提交更改并写「已断开：<原因> 未提交的 N 行变更已丢弃；数据库事务已由后端强制回滚。」；状态行「已断开 · 已加载 N 行（只读）」；已加载行保留 | `src/data_page.rs:4756` |
| load | 读取 / 提交 / 事务操作失败 | 横幅写服务端给的失败文案（错误码进日志与状态行），状态行转 error，逐行失败另见 data_page.errors；**已加载的行保留**、不清屏，可重试；普通失败不提供「重启 Sqarvo」按钮，仅会话丢失 / 后端不可用才提供 | `src/data_page.rs:4609、src/data_page.rs:4703` |
| load | 新增行提交失败（如 NOT NULL 字段留空导致整批回滚，或净影响为 0） | 不再弹页首横幅：全文提示进 data_page.toolbar.status_cluster 的可见过程消息（点名字段 + 整批回滚 + 草稿保留 + 错误码，窄时省略、悬停给全文），不另弹逐行错误列表；草稿行保留，修正后可再次提交；不提供「重启 Sqarvo」按钮 | `src/data_page.rs:3558、src/data_page.rs:3607` |

### `data_page.errors` 逐行提交失败列表

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`gap_xs`
- 无障碍：可访问名「提交失败的行」
- 同步：Linux done（§5.4 提交结果；show_applied → show_failures）
- 实现备注：位置在工具条下方、表格上方；一行一条「第 N 行：[错误码] 原因」，可选中复制。只列**这一次提交里失败的行**；成功的行变干净，失败的行保留在原处可重试。没有失败时整块隐藏（不占高度）；取消提交后整块清空（不采信任何部分结果）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 提交批次里有失败行 | 逐行列出「第 N 行：[code] 原因」（如 table_data.row_deleted / table_data.rows_affected_mismatch）；块可见、已加载行保留；状态行与横幅同时给汇总 | `src/data_page.rs:6991、src/data_page.rs:4573` |

### `data_page.query_edit` 查询结果的单表编辑（复用同一份草稿与提交路径）

kind `container` · status `active` · parent `data_page`

- 无障碍：可访问名「查询结果编辑态」
- 同步：Linux done（§5.10 查询结果的单表编辑（v0.2.5）；sql_page try_enable_edit / on_applied，result_edit，results 重绘）
- 实现备注：查询标签的结果区默认只读；当结果**等价于一张表的当前页**时，状态栏的 + − ✓ ↶ 对它生效，口径与表数据页一致（同一份 TableDraft / InsertDraft / ApplyRowChanges）。三个条件同时成立才装上编辑态：结果终态、SQL 是唯一顶层 FROM 的简单 SELECT（无 JOIN / 集合运算 / GROUP BY / 多表逗号）、结果列覆盖目标表全部主键列。编辑用与网格同一个 open_cell_editor；提交目标表由 SQL 页显式给出，会话是本标签自己的会话。本版边界（仍成立）：新增行一次只有一行；限流（Limited）结果不启用编辑；查询标签没有提交确认对话框（风险只写进状态行与日志）。行级脏色与删除线在 v0.2.5 **已落地**：改过的单元格 `.sqarvo-cell-dirty`、标删的行 `.sqarvo-row-deleted`（删除线 + 0.55 透明度）、新行占位整行按「待提交」着色，与数据页同一套 class；`.sqarvo-row-deleted` 原先只定义在数据页自己的 provider 里，查询标签没打开过数据页时根本没加载，现搬到全局样式表（theme::stylesheet）两处共用。重绘不是重跑：编辑 / 标删 / 新增 / 提交 / 放弃之后按草稿重绘（重跑会丢掉未提交的草稿）；只有**提交成功**才自动重跑同一条语句。重绘范围：小结果集（≤ 5000 行，RELOAD_ROW_LIMIT）整表重绘、只发一次 items_changed；更大的结果集只重绘变过的行，但仍回到首行。提交落定后只重绘本批变过的行（dirty_indices），不再把已加载的每一行都 items_changed 一遍。回到的是**首行首列**（scroll_home）：只把行滚到 0 不够——GtkColumnView 会为露出「焦点所在的那一列」而横向滚动，在最后一列改完值再提交就会停在那里（用户报「刷新后跳到最后一列」）；做法是点名第一列 + 下一帧再兜一次横向 adjustment（焦点恢复发生在那之后），有变化时写日志「[query] 水平滚动复位：X → 0」。每次重跑重建列也走这条。状态圆点是**常驻**的（不是「有脏行才出现」）：没有未提交更改时是灰点、tooltip「没有未提交的更改」；有脏行 / 标删 / 新行时强调色，tooltip「自动提交 · 脏行 N · 待提交 M 条 ⏎ 自动提交：每条语句自成一个事务」口径同表工具条同一枚圆点（同一套 CSS 与文案）。表事件的归属：路由层先判「该标签没有浏览状态」再投给 SQL 页（tabs::route_table），**不能**让它撞上「通道不符」被整批丢弃——否则提交后既没有回填也不会自动重跑（日志「丢弃事件：标签 tab-q1 是查询标签，不接收表数据事件」就是这条路径走错了）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 结果终态、唯一顶层 FROM 的简单 SELECT、覆盖全部主键列 | 装上编辑态：行操作组可用，日志「[query] 结果可编辑：表 users，3 行，主键 [id]」；状态圆点常驻（灰 = 没有未提交的更改，有脏行转强调色） | `src/sql_page.rs:716、src/result_edit.rs:46、src/results.rs:854` |
| load | 上述任一不成立 | 行操作组整体置灰，tooltip 写明具体原因：「结果不是单表查询（JOIN / 聚合 / 子查询）：不能新增、编辑或删除」/「结果里没有主键列 X：定位不到要改的行，不能编辑」/「这张表没有主键：不能新增、编辑或删除」/「读取主键失败：…」——不静默灰掉 | `src/sql_page.rs:723、src/sql_page.rs:748、src/result_edit.rs:79` |
| double_click | 结果可编辑、单元格可改 | 打开与网格同一个编辑器；确定后按当前值重绘该行（未提交前不写库），并立刻重算行操作可用性（第一次编辑后 ✓ 即可点） | `src/results.rs:975、src/sql_page.rs:739` |
| click | 可编辑、没有未提交改动、没有待插入行（入口：状态栏 +） | 结果末尾追加一行占位，各列写「（默认）」；双击各列填值，提交时按「省略未填列、交给数据库默认值」生成单行 INSERT（日志「[query] 新行草稿：第 N 行（占位）」）；已有未提交改动时提示「已经有未提交的更改：先提交或放弃，再新增行」，有占位行时提示「已经有一行待插入的新行：先提交或放弃它」 | `src/results.rs:905、src/sql_page.rs:967、src/result_edit.rs:199` |
| click | 可编辑且有选中行（入口：状态栏 −） | 把选中行标记待删除（不立刻写库），可 ↶ 放弃；重绘后整行删除线 + 压暗，状态行「已标记删除 N 行（提交前可以放弃）」 | `src/sql_page.rs:979、src/result_edit.rs:177` |
| click | 有脏行 / 待插入行（入口：状态栏 ✓ / Ctrl+S） | 走 ApplyRowChanges；全部成功后**自动重跑同一条语句**（results.statement()，不是编辑器里的当前文本），结果区写「已提交 N 行 · 已自动重新执行查询」/「已插入 N 行 · 已自动重新执行查询」；有失败行时**不重跑**，失败行留在草稿里 | `src/sql_page.rs:809、src/sql_page.rs:852、src/sql_page.rs:383` |
| click | 有脏行（入口：状态栏 ↶） | 全部编辑、删除标记与新行草稿一起清掉，按草稿重绘并回到首行首列，状态行「已放弃未提交的更改」 | `src/sql_page.rs:1034、src/results.rs:768` |
| load | 有未提交的更改且点了刷新（⟳） | 拒绝并写「有未提交的更改：先提交或放弃，再刷新」（重跑会换掉整个结果集，草稿没有落点） | `src/sql_page.rs:965` |

## MCP 策略确认（`dialog`）

两个风险确认框，由主菜单中的策略开关触发。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `dialog` | MCP 策略确认 | area | active | done |
| `dialog.mcp_enable_confirm` | 　启用 MCP 数据访问确认 | control | active | done |
| `dialog.mcp_readonly_disable_confirm` | 　关闭 MCP 只读确认 | control | active | done |

### `dialog` MCP 策略确认

kind `area` · status `active`

- 同步：Linux done（tests/ui/linux/test_06_mcp_access.py：两个真实确认框的标题、档案数、取消及确认。）

### `dialog.mcp_enable_confirm` 启用 MCP 数据访问确认

kind `control` · status `active` · parent `dialog`

- 样式：槽位 `dialogs`
- 无障碍：可访问名「启用 MCP 数据访问？」；Tab 默认落在取消，确认按钮紧随其后
- 原生控件：gtk:AdwAlertDialog
- 同步：Linux done（tests/ui/linux/test_06_mcp_access.py::test_mcp_access_menu_and_live_status 验证确认和取消。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 在主菜单启用 MCP 数据访问 | 显示当前档案数和授权范围（现有及将来新增的档案），确认后写入策略并回读，取消不写入。 | `src/mcp_access.rs:226` |

### `dialog.mcp_readonly_disable_confirm` 关闭 MCP 只读确认

kind `control` · status `active` · parent `dialog`

- 样式：槽位 `dialogs`
- 无障碍：可访问名「关闭 MCP 只读模式？」；Tab 默认落在取消，确认按钮紧随其后
- 原生控件：gtk:AdwAlertDialog
- 同步：Linux done（tests/ui/linux/test_06_mcp_access.py::test_mcp_read_only_confirmation 验证风险提示和取消。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 数据访问已启用，并在主菜单关闭 MCP 只读 | 显示当前档案数，说明授权 Agent 可执行受控插入、更新和删除，仍受数据库权限和写入限额约束。确认后写入策略并回读，取消不写入。 | `src/mcp_access.rs:226` |

## 对话框（`dialogs`）

连接对话框（新建 / 编辑两种模式共用一张表面）与关于对话框。连接对话框是唯一配置连接参数、测试连接、把密码交给系统凭据库的入口；关于对话框只陈述版本、协议与后端诊断，不依赖后端可用。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `dialogs` | 对话框区 | area | active | done |
| `dialogs.connection` | 　连接对话框（新建 / 编辑） | container | active | done |
| `dialogs.connection.header` | 　　标题与副标题 | control | active | done |
| `dialogs.connection.driver` | 　　驱动下拉 | control | active | done |
| `dialogs.connection.fields` | 　　连接参数表单（按驱动族显隐） | container | active | done |
| `dialogs.connection.fields.host` | 　　　主机 | control | active | done |
| `dialogs.connection.fields.alias` | 　　　别名（可选） | control | active | done |
| `dialogs.connection.fields.port` | 　　　端口 | control | active | done |
| `dialogs.connection.fields.username` | 　　　用户名 | control | active | done |
| `dialogs.connection.fields.database` | 　　　数据库（可选） | control | active | done |
| `dialogs.connection.fields.password` | 　　　密码 | control | active | done |
| `dialogs.connection.fields.tls` | 　　　TLS 模式（五档） | control | active | done |
| `dialogs.connection.fields.file_path` | 　　　数据库文件（含「浏览」） | control | active | done |
| `dialogs.connection.fields.read_only` | 　　　「只读打开」 | control | active | done |
| `dialogs.connection.credential_note` | 　　凭据说明行（「密码只写入系统凭据库」） | control | active | done |
| `dialogs.connection.status` | 　　状态行（校验 / 测试结果 / 失败） | control | active | done |
| `dialogs.connection.actions` | 　　底部操作条（状态行 + 两个按钮） | container | active | done |
| `dialogs.connection.actions.test` | 　　　「测试连接」 | control | active | done |
| `dialogs.connection.actions.save` | 　　　保存动作（新建：「保存并连接」/ 编辑：「保存」） | control | active | done |
| `dialogs.connection.actions.cancel` | 　　　「取消」 | control | active | done |
| `dialogs.about` | 　关于对话框 | container | active | done |
| `dialogs.about.app_info` | 　　名称 / 版本 / 一句说明 | control | active | done |
| `dialogs.about.protocol` | 　　后端版本与协议 | control | active | done |
| `dialogs.about.diagnostics` | 　　后端诊断字段 | control | active | done |
| `dialogs.about.privacy_note` | 　　保密说明行 | control | active | done |

### `dialogs` 对话框区

kind `area` · status `active`

- 样式：槽位 `dialogs`；尺寸 `gap_page`
- 同步：Linux done（dialog.rs（AdwDialog + AdwPreferencesPage）与 ui.rs show_about 逐条对照）
- 实现备注：只收独立于某个动作、有自己生命周期的表面：新建 / 编辑连接对话框、关于对话框。确认框跟随触发它的区：关闭标签确认在 tabs，删除 / 断开连接确认在 object_tree，后端重启确认在 shell —— 它们不是本区的组件。两个对话框都是 Attached Modal 表面（AdwDialog 附着主窗口呈现），不新开顶层窗口；同一时刻各只有一个实例。

### `dialogs.connection` 连接对话框（新建 / 编辑）

kind `container` · status `active` · parent `dialogs`

- 样式：槽位 `dialogs`；尺寸 `gap_md`、`gap_page`
- 无障碍：可访问名「新建连接」；Tab 默认动作是「保存并连接」（Enter 即保存并连接）
- 原生控件：gtk:AdwDialog + AdwPreferencesPage
- 同步：Linux done（§2.3 编辑连接 + dialog.rs build / apply_saved 复核）
- 实现备注：宽度 560 是代码常量，未进 metrics（P1 补齐）。标题始终是「新建连接」，编辑模式不改标题：两种模式的差异只在预填的字段与凭据引用上，不在措辞上。标题栏左侧只有「取消」；主体是「连接参数」表单 + 凭据说明两块；底部是状态行与两个提交按钮。字段显隐由 driver_form 的一张描述表决定，界面代码里没有按驱动 ID 分支：加驱动不改界面代码。系统凭据库不可用或密码写入失败时如实报错，不回退明文保存；对话框只收集输入与展示结果，RPC 在 worker 线程执行。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 后端已握手（未握手时工具条「连接」与菜单项禁用；动作本身也直接返回，只记日志「后端尚未连接，忽略「新建连接」」，不发任何请求） | 打开对话框：标题「新建连接」、副标题「未选择驱动」，驱动下拉默认选中 MariaDB（存在时）；已打开时只把它前置，不叠第二个 | `src/ui.rs:1109、src/ui.rs:1111、src/ui.rs:1114、src/dialog.rs:133` |
| load | 从对象树右键「编辑连接…」进入，或按保存凭据连接时发现缺密码 | 用已保存连接预填非敏感字段（驱动、主机、端口、库、用户名、文件路径、只读、TLS 档位）；密码行留空，已有 credential_ref 时复用它并在状态行提示「已填入保存的连接：密码将从系统凭据库读取」，引用为空时提示「已填入保存的连接；请输入密码后连接」；不自动连接，用户仍需点「测试连接」或「保存并连接」 | `src/dialog.rs:614、src/dialog.rs:643、src/dialog.rs:646、src/ui.rs:1128` |
| load | 从对象树 / 启动态按保存的凭据连接时凭据缺失（配置里没有引用，或后端回 credential.not_found） | 打开预填对话框，并在状态行写「这条连接缺少密码，请编辑后重试（<原因>）」：原因分别是「配置里没有凭据引用」或「<stage>: <message>（credential.not_found）」；不发连接 RPC，真正的失败分类由用户重新提交后的结果给出 | `src/ui.rs:490、src/ui.rs:1199、src/ui.rs:1226、src/dialog.rs:574` |

### `dialogs.connection.header` 标题与副标题

kind `control` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `font_title`、`font_panel`
- 无障碍：可访问名「连接对话框标题」
- 同步：Linux done（dialog.rs apply_selection / set_pending 复核）
- 实现备注：副标题只说「当前驱动是谁」，不写主机 / 用户：那两样在表单里本身可见。标题字号由 AdwWindowTitle 决定，未进 metrics。标题按入口变：工具条「连接」是「新建连接」，对象树「编辑连接…」是「编辑连接」（同时改窗口标题与标题栏）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 对话框构建或驱动切换 | 副标题 = 当前驱动的显示名（后端 ListDrivers 返回的 display_name）；没有可选驱动时写「未选择驱动」 | `src/dialog.rs:450、src/dialog.rs:138` |
| load | 从「编辑连接…」进入 | 标题改「编辑连接」；对话框标题属性与标题栏同步改 | `src/dialog.rs:408、src/ui.rs:3838` |
| load | 「测试连接」或「保存并连接」的请求在飞 | 副标题改为「正在与数据库握手…」；回包后恢复为驱动显示名 | `src/dialog.rs:510、src/dialog.rs:529` |

### `dialogs.connection.driver` 驱动下拉

kind `control` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「驱动」；Tab 可聚焦；↑ / ↓ 换驱动
- 同步：Linux done（dialog.rs apply_selection 与 DriverChoice::from_summary 复核）
- 实现备注：清单只来自后端 SystemService.ListDrivers，前端不硬编码；默认值偏好 MariaDB 只是肌肉记忆，不是清单。下拉标签形如「显示名（driver_id）」；前端没有表单的驱动照样列出，标签追加「前端暂无表单」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 对话框构建 | 列出后端全部驱动并默认选中 MariaDB（不存在时选第一个）；后端没返回驱动时只显示「（后端未返回任何驱动）」，下拉与两个提交按钮一并禁用，状态行写「后端没有注册任何驱动（ListDrivers 为空）：无法新建连接」 | `src/dialog.rs:147、src/dialog.rs:292、src/dialog.rs:469` |
| load | 切换驱动 | 按驱动族重排字段可见性、按能力重建 TLS 档位标签、补回被清空的主机 / 端口默认值；前端没有该驱动表单时清空并禁用提交，状态行写「此版本前端没有驱动「<driver_id>」的连接表单：请升级前端，或换一个驱动」 | `src/dialog.rs:408、src/dialog.rs:312、src/dialog.rs:461` |

### `dialogs.connection.fields` 连接参数表单（按驱动族显隐）

kind `container` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `gap_xs`、`gap_md`
- 同步：Linux done（driver_form.rs fields_for 描述表 + dialog.rs 显隐接线）
- 实现备注：服务器型驱动显示主机 / 端口 / 库 / 用户名 / TLS / 密码；文件型驱动只显示数据库文件与只读打开。隐藏的字段不进入 ConnectionParams：不是「显示了但不填」。分组标题「连接参数」；顺序固定，不随驱动重排。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 驱动族确定（服务器型 / 文件型 / 前端不认识） | 按描述表显示或隐藏各行；切回服务器型驱动时空着的主机与端口补回默认值（用户没改过就不该看到空主机） | `src/dialog.rs:214、src/driver_form.rs:91、src/dialog.rs:436` |

### `dialogs.connection.fields.host` 主机

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「主机」
- 同步：Linux done（dialog.rs 默认值 + driver_form.rs 校验文案）
- 实现备注：新建时的初值与切回服务器型驱动时的补值是 127.0.0.1；文件型驱动下不显示。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 切到服务器型驱动且主机为空 | 主机补回 127.0.0.1（用户改过就不动） | `src/dialog.rs:166、src/dialog.rs:436` |
| click | 提交时主机为空 | 状态行写「请填写主机」，不发任何 RPC | `src/driver_form.rs:169、src/dialog.rs:738` |

### `dialogs.connection.fields.alias` 别名（可选）

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「别名（可选）」；Tab 驱动下拉之后、主机之前
- 同步：Linux done（tests/ui/linux/test_06_mcp_access.py::test_edit_profile_preserves_id_and_increments_revision 验证编辑别名与档案修订。）
- 实现备注：用作对象树连接行显示名及共享档案的名称；留空时对象树显示连接目标。编辑保留档案 UUID，修改名称不会创建新档案。迁移时旧档案有别名则沿用，缺失则使用原有名称。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 打开新建 / 编辑连接对话框 | 编辑时填入保存的别名；新建时留空 | `src/dialog.rs:180、src/dialog.rs:632` |
| click | 点「保存并连接」且校验通过 | trim 后作为 SaveProfile 的档案名称和可选显示名。 | `src/config.rs:222` |

### `dialogs.connection.fields.port` 端口

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「端口」
- 同步：Linux done（driver_form.rs 端口解析与两条文案）
- 实现备注：新建时的初值是 3306；输入框是数字用途（不是数字键盘强制的自绘控件）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 切到服务器型驱动且端口为空 | 端口补回 3306 | `src/dialog.rs:170、src/dialog.rs:439` |
| click | 提交时端口不是数字，或不在 1-65535 之间 | 状态行写「端口必须是 1-65535 之间的数字（当前：<输入>）」（空值或 0 写不带当前值的同一句），不发任何 RPC | `src/driver_form.rs:176、src/driver_form.rs:181` |

### `dialogs.connection.fields.username` 用户名

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「用户名」
- 同步：Linux done（driver_form.rs 用户名校验）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 提交时用户名为空（服务器型驱动） | 状态行写「请填写用户名」，不发任何 RPC | `src/driver_form.rs:193、src/dialog.rs:738` |

### `dialogs.connection.fields.database` 数据库（可选）

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「数据库」
- 同步：Linux done（dialog.rs 行标题 + driver_form.rs default_name）
- 实现备注：行标题本身就是说明：「数据库（可选：留空则连接后浏览所有库）」，不靠 tooltip 才说清可选。空库名是合法值：语义是「没有默认库」，不是缺必填项。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 服务器型驱动且库名留空 | 按合法值处理：会话显示名不拼斜杠（用户@主机），连上后浏览所有库 | `src/dialog.rs:175、src/driver_form.rs:189、src/driver_form.rs:250` |

### `dialogs.connection.fields.password` 密码

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「密码」
- 同步：Linux done（dialog.rs apply_saved（留空 + 复用引用）+ driver_form.rs 密码校验）
- 实现备注：输入框是密码专用的输入行（自带显示 / 隐藏切换），内容不进日志、不进配置。密码永不回显：没有「读出旧密码」的路径，编辑模式下这一行一定是空的。密码随草稿交给 worker 后即被对话框丢弃，本模块不保存它。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 编辑模式（有已保存连接） | 密码行留空，并复用已有的 credential_ref：后端从操作系统凭据库取密码，界面不展示任何已存密码；引用为空时状态行提示「已填入保存的连接；请输入密码后连接」 | `src/dialog.rs:638、src/dialog.rs:643` |
| click | 提交时密码为空且没有 credential_ref | 状态行写「请输入密码」，不发任何 RPC | `src/driver_form.rs:203、src/dialog.rs:738` |

### `dialogs.connection.fields.tls` TLS 模式（五档）

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「TLS 模式」
- 同步：Linux done（dialog.rs rebuild_tls_model / tls_subtitle_for + TlsMode::label_for）
- 实现备注：五档固定顺序：禁用（disabled）/ 优先（preferred）/ 必需（required）/ 校验 CA（verify-ca）/ 完整校验（verify-full）；默认「优先」。标签随驱动能力重建：驱动做不到只校链时，第四档写「校验 CA（verify-ca，本驱动等同 verify-full）」并给副标题「本驱动无法只校验证书链：仍会校验主机名」。文件型驱动没有 TLS：不显示该行，落盘的 tls_mode 是 disabled，不写一个假的安全策略。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 服务器型驱动 | 按当前驱动能力重建五档标签，并把选中档位映射回新列表（换 model 会让选中失效，重建后显式选回同一档） | `src/dialog.rs:490、src/dialog.rs:775、src/backend/connection.rs:109` |
| load | 选中「校验 CA」且驱动不支持只校链 | 该行副标题写「本驱动无法只校验证书链：仍会校验主机名」；切到别的档位时副标题立刻消失 | `src/dialog.rs:502、src/dialog.rs:788` |

### `dialogs.connection.fields.file_path` 数据库文件（含「浏览」）

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「数据库文件」
- 同步：Linux done（dialog.rs open_file_chooser + driver_form.rs 路径校验）
- 实现备注：路径可手输，也可点行尾「浏览」用系统文件选择器（标题「选择 SQLite 数据库文件」，过滤 *.db、*.sqlite、*.sqlite3）。文件选择器取消不算错误：不写行、不记错误日志。:memory: 是合法路径：不要求文件存在。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 文件型驱动 | 打开系统文件选择器；选中后只把路径写进输入行，其余流程与手输完全相同（取消则什么都不改） | `src/dialog.rs:197、src/dialog.rs:801` |
| click | 提交时路径为空或文件不存在 | 状态行写「请填写数据库文件路径（例如 /home/me/app.db）」或「数据库文件不存在：<路径>（请选择已有文件，或先用 sqlite3 创建）」，不发任何 RPC | `src/driver_form.rs:210、src/driver_form.rs:217` |

### `dialogs.connection.fields.read_only` 「只读打开」

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`min_target`
- 无障碍：可访问名「只读打开」
- 同步：Linux done（driver_form.rs 只读 + 内存库互斥规则）
- 实现备注：副标题「以只读方式连接：数据库会拒绝任何写入（数据页会显示只读原因）」；只对文件型驱动显示。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 已勾选只读且路径是 :memory: | 状态行写「内存数据库（:memory:）不支持只读模式」，不发任何 RPC（与后端同一条规则，提前在界面说清） | `src/driver_form.rs:225、src/dialog.rs:205` |

### `dialogs.connection.credential_note` 凭据说明行（「密码只写入系统凭据库」）

kind `control` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `font_status`
- 无障碍：可访问名「密码只写入系统凭据库」
- 同步：Linux done（dialog.rs credential_note 构建与显隐）
- 实现备注：只说事实，不说安全承诺以外的内容：配置里只有一个随机引用。不需要密码的驱动隐藏这一行（说了也没有含义）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 当前驱动需要密码 | 显示两行：「密码只写入系统凭据库」与「连接配置只保存一个随机引用；系统凭据库不可用时如实报错，不会回退明文保存」 | `src/dialog.rs:228、src/dialog.rs:430` |

### `dialogs.connection.status` 状态行（校验 / 测试结果 / 失败）

kind `control` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `font_status`
- 无障碍：可访问名「连接状态」
- 同步：Linux done（§7.1 失败三段文案口径 + dialog.rs show_status / show_failure / show_test_report）
- 实现备注：单行区，失败时按两行显示三段：分类主标题、后端文案、稳定错误码（形如「认证失败：用户名或密码错误」/「<detail> [<code>]」），可换行、左对齐、占满可用宽度。成功 / 失败 / 说明三种语义只靠样式类区分，不换控件。状态行只在对话框里，不落到状态栏、不弹第二个提示框。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 保存前本地校验不通过 | 写校验原因（例如「请填写主机」「端口必须是 1-65535 之间的数字（当前：abc）」「请输入密码」），不发任何 RPC，对话框保持打开 | `src/driver_form.rs:145、src/dialog.rs:738` |
| load | 「测试连接」成功 | 写「连接成功：延迟 <N> ms · 服务端版本 <version>」；后端未返回版本时写「未知（后端未返回版本）」，并把返回的凭据引用留在对话框里复用 | `src/dialog.rs:537、src/ui.rs:612` |
| load | 测试或保存失败 | 两行：分类主标题（如「认证失败：用户名或密码错误」「系统凭据库不可用（不会回退明文保存）」「连接超时」），下一行是后端文案加「[稳定错误码]」（如 mariadb.auth_failed / credential.not_found / timeout） | `src/dialog.rs:558、src/backend/connection.rs:479、src/backend/connection.rs:513` |
| load | 切换驱动，或上一次的错误已不适用 | 状态行收起，旧驱动的错误不留在屏幕上 | `src/dialog.rs:457、src/dialog.rs:599` |

### `dialogs.connection.actions` 底部操作条（状态行 + 两个按钮）

kind `container` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `gap_md`、`gap_xl`、`fs_button_min_height`、`min_target`
- 同步：Linux done（dialog.rs actions 布局 + set_pending 禁用清单）
- 实现备注：状态行左对齐占满，两个按钮右对齐；底栏上下留白 gap_md、左右留白 gap_xl。对话框本身没有「断开中」这一态：连接 / 断开中的重复点击由连接注册表忽略（见 object_tree 连接行），本对话框只在请求在飞期间整体禁用。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 「测试连接」或「保存并连接」的请求在飞 | 两个按钮、驱动下拉与全部输入行一并禁用（sensitive=false），避免重复提交；回包后按驱动与能力恢复可用性 | `src/dialog.rs:510、src/dialog.rs:755` |

### `dialogs.connection.actions.test` 「测试连接」

kind `control` · status `active` · parent `dialogs.connection.actions`

- 样式：槽位 `dialogs`；尺寸 `fs_button_min_height`、`min_target`
- 无障碍：可访问名「测试连接」
- 同步：Linux done（dialog.rs 测试按钮接线 + Event::Tested 回填）
- 实现备注：不落盘：测试成功只把凭据引用留在对话框里，配置与凭据都不写（写盘只发生在「保存并连接」）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有可用驱动（ListDrivers 非空且前端认识该驱动），请求不在飞 | 先做本地校验，通过后整体置为禁用、状态行写「正在测试连接…」，调 TestConnection；成功回「连接成功：延迟 … ms · 服务端版本 …」，失败回分类错误两行 | `src/dialog.rs:246、src/dialog.rs:738、src/ui.rs:612` |
| click | 没有可用驱动（后端未返回驱动，或前端没有该驱动的表单） | 按钮禁用，点不动：不发测试请求，也不退回一份写死的 MariaDB 表单 | `src/dialog.rs:445、src/dialog.rs:159` |

### `dialogs.connection.actions.save` 保存动作（新建：「保存并连接」/ 编辑：「保存」）

kind `control` · status `active` · parent `dialogs.connection.actions`

- 样式：槽位 `dialogs`；尺寸 `fs_action_min_height`、`min_target`；颜色 `sqarvo_brand_deep`、`sqarvo_brand_on_deep`
- 无障碍：可访问名「保存并连接（新建）/ 保存（编辑）」
- 同步：Linux done（GTK --qa-profile --qa-action save 验证新建；tests/ui/linux/test_06_mcp_access.py 验证编辑、UUID 与 revision。）
- 实现备注：建议动作：实底用品牌深档 + 深档前景（与工具条「连接」同一条 token），hover / active 只叠半透明，不退回系统强调色。两种入口两种语义：新建 = 保存并立即连接；编辑 = **只保存**（写配置与凭据，不建会话、不重连），下次连接时生效。档案由 SaveProfile 写入，密码保存在系统凭据库；档案仅保存凭据引用。新建保存成功即建立会话并关闭对话框，落到工作区；编辑保存成功只关闭对话框并刷新树上那一行；保存失败则对话框留着、状态行给原因。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 新建态（工具条「连接」进入）、本地校验通过、驱动可用 | 整体置为禁用、状态行写「正在保存密码并建立会话…」→ 密码写入系统凭据库 → 配置只落 credential_ref → 连接成功后关闭对话框并落入工作区 | `src/dialog.rs:738、src/ui.rs:1391、src/ui.rs:641` |
| click | 编辑态（「编辑连接…」进入）、本地校验通过 | 按钮文案是「保存」：按 UUID 与 revision 写共享档案。成功后关闭对话框，连接行按新名称或目标重绘。编辑保存不建立会话。 | `src/config.rs:204、src/backend/worker.rs:4284` |
| click | 本地校验不通过 | 不提交：状态行给原因，对话框保持打开，用户改正后可再点 | `src/dialog.rs:739` |
| click | 后端 worker 不可用（已退出或未握手） | 状态行写「本地后端未连接」（错误码 backend.unavailable），按钮恢复可点，不假装已保存 | `src/ui.rs:1408、src/ui.rs:1410、src/backend/connection.rs:489` |

### `dialogs.connection.actions.cancel` 「取消」

kind `control` · status `active` · parent `dialogs.connection.actions`

- 样式：槽位 `dialogs`；尺寸 `fs_button_min_height`、`min_target`
- 无障碍：可访问名「取消」
- 同步：Linux done（dialog.rs cancel 接线与注释）
- 实现备注：在标题栏左侧，不在底部操作条里：破坏性为零的出口不需要挤在提交按钮旁边。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 对话框已打开 | 直接关闭：不写配置、不写凭据、不建会话；再次打开从默认值重新开始 | `src/dialog.rs:141、src/dialog.rs:305` |

### `dialogs.about` 关于对话框

kind `container` · status `active` · parent `dialogs`

- 样式：槽位 `dialogs`；尺寸 `gap_page`、`gap_md`、`gap_lg`
- 无障碍：可访问名「关于 Squarvo」
- 原生控件：gtk:AdwDialog（自绘内容）
- 同步：Linux done（§1.4 帮助 → 关于 + ui.rs show_about 逐行对照）
- 实现备注：自绘对话框：宽度 620、高度 520 是代码常量，未进 metrics（P1 补齐）。不依赖后端：后端未握手时照样能打开，诊断区块改写端点文件与「后端：尚未握手完成」。内容顺序固定：应用名与版本 → 「后端诊断」标题 → 诊断逐行 → 保密说明。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 帮助 → 关于 Squarvo（任意状态，不依赖后端） | 打开对话框；已打开时只把它前置，不叠第二个 | `src/ui.rs:5301、src/ui.rs:5302、src/ui.rs:4635` |
| load | 内容高于可视区 | 纵向滚动，横向永不滚动；长路径按字符折行，不让一个没有空格的长字符串把对话框顶宽 | `src/ui.rs:5377、src/ui.rs:5363` |

### `dialogs.about.app_info` 名称 / 版本 / 一句说明

kind `control` · status `active` · parent `dialogs.about`

- 样式：槽位 `dialogs`；尺寸 `font_panel`
- 无障碍：可访问名「Sqarvo 版本」
- 同步：Linux done（ui.rs show_about 应用信息三行）
- 实现备注：版本号取编译期包版本，不写死文案；字号来自 Adw 的 title-1 与 dim-label，未进 metrics。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 对话框打开 | 三行：「Sqarvo」、「版本 <包版本>」、「轻量级原生数据库客户端（Linux 前端：GTK4 + libadwaita）」 | `src/ui.rs:5313、src/ui.rs:5316、src/ui.rs:5319` |

### `dialogs.about.protocol` 后端版本与协议

kind `control` · status `active` · parent `dialogs.about`

- 样式：槽位 `dialogs`；尺寸 `font_status`
- 无障碍：可访问名「后端版本与协议」
- 同步：Linux done（ui.rs backend_diagnostic_lines 首行与未握手分支）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 后端已握手 | 写「后端版本：<service_version>（协议 <protocol_version>，<build_profile> 构建）」 | `src/ui.rs:5255` |
| load | 后端未握手 | 不写版本与协议行，诊断区块改写「端点文件：<路径>」与「后端：尚未握手完成」——不编造版本号 | `src/ui.rs:5338` |

### `dialogs.about.diagnostics` 后端诊断字段

kind `control` · status `active` · parent `dialogs.about`

- 样式：槽位 `dialogs`；尺寸 `font_status`、`gap_lg`
- 无障碍：可访问名「后端诊断」
- 同步：Linux done（ui.rs backend_diagnostic_lines + 会话行拼装）
- 实现备注：逐行一个标签，全部可选中复制（对报障有用），逐字折行。版本不一致必须显式写出来，不能静默：可能连到了旧版本后端或读到了过期端点文件。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 后端已握手 | 逐行给：传输方式、后端进程 PID（由前端启动：是 / 否）、端点文件、版本不一致时「注意：端点记录的后端版本 … 与 GetVersion 返回的 … 不一致。」、已注册驱动「显示名（driver_id）」清单（无驱动时写「已注册驱动：（后端未返回任何驱动）」） | `src/ui.rs:5255、src/ui.rs:5273、src/ui.rs:5280` |
| load | 有工作区 | 追加连接名、初始会话短 ID（「连接对话框建立的初始会话：…（未领养时随退出释放）」）与逐条标签会话行 | `src/ui.rs:5343、src/ui.rs:5346` |

### `dialogs.about.privacy_note` 保密说明行

kind `control` · status `active` · parent `dialogs.about`

- 样式：槽位 `dialogs`；尺寸 `font_status`
- 无障碍：可访问名「保密说明」
- 同步：Linux done（ui.rs show_about 结尾说明行）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 对话框打开 | 写「本对话框只在本机显示，不含密码、凭据引用与 token。」（诊断字段里也确实没有这三样） | `src/ui.rs:5369` |

## 信息面板（`inspector`）

右侧信息面板：面板外壳与固定宽度、空态留白、连接 / 库 / 表 / 列四种视图、[DDL] 与 [表信息] 两页，以及状态页与跨标签事务提示。内容按当前标签钉住：表数据标签钉这张表、查询标签钉它选择的库（没有选择库 → 空态）、Objects 固定页跟随对象树选中；树选中、Objects 行单击与数据网格列头点击仍即时驱动预览。鼠标经过不改面板。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `inspector` | 信息面板 | area | active | done |
| `inspector.header` | 　标题块（标题 · 副标题）与分隔线 | container | active | done |
| `inspector.empty` | 　空态（内容区留白） | container | active | done |
| `inspector.connection` | 　连接视图（标题 · 副标题 · 字段行） | container | active | done |
| `inspector.field` | 　字段行（标签 · 值） | row | active | done |
| `inspector.catalog` | 　库 / 分组视图（标题 · 副标题 · 计数） | container | active | done |
| `inspector.object` | 　表 / 视图视图（标题 · 切换条 · 两页） | container | active | done |
| `inspector.pane_switch` | 　[DDL] / [表信息] 切换条 | container | active | done |
| `inspector.pane_switch.ddl` | 　　「DDL」按钮 | control | active | done |
| `inspector.pane_switch.info` | 　　「表信息」按钮 | control | active | done |
| `inspector.object.info` | 　　表信息页（列 · 主键 · 索引 · 触发器 · 已加载行数） | container | active | done |
| `inspector.object.info.children` | 　　　索引与触发器 | container | active | done |
| `inspector.object.info.children.trigger_picker` | 　　　　触发器选择 | control | active | done |
| `inspector.object.info.children.load_more` | 　　　　加载更多索引和触发器 | control | active | done |
| `inspector.object.info.children.retry` | 　　　　重试读取索引和触发器 | control | active | done |
| `inspector.object.info.children.view_ddl` | 　　　　查看触发器 DDL | control | active | done |
| `inspector.object.info.refresh` | 　　　刷新表信息 | control | active | done |
| `inspector.object.info.trigger_ddl` | 　　　触发器 DDL 正文 | container | active | done |
| `inspector.object.info.trigger_ddl.copy` | 　　　　复制触发器 DDL | control | active | done |
| `inspector.object.info.trigger_ddl.back` | 　　　　返回表信息 | control | active | done |
| `inspector.object.info.trigger_ddl.retry` | 　　　　重试读取触发器 DDL | control | active | done |
| `inspector.object.info.columns` | 　　　列清单（列名 · 类型 · 可空 · 主键） | container | active | done |
| `inspector.object.info.columns.row` | 　　　　列行（列名 · 类型 · 可空 · 主键标记） | row | active | done |
| `inspector.object.info.pk` | 　　　主键节（主键列 · 读不到时的说明行） | control | active | done |
| `inspector.object.ddl` | 　　DDL 正文 | container | active | done |
| `inspector.object.ddl.warning` | 　　　DDL 琥珀警示（exact=false） | control | active | done |
| `inspector.column` | 　列视图（六项属性） | container | active | done |
| `inspector.state` | 　状态页（不支持 · 无从判断） | control | active | done |
| `inspector.txn_hint` | 　跨标签事务提示行 | control | active | done |
| `inspector.object.info.status` | 　　　表信息提示行（读取中 · 读取失败） | control | active | done |

### `inspector` 信息面板

kind `area` · status `active`

- 样式：槽位 `inspector`；尺寸 `inspector_width`、`inspector_min`、`inspector_max`、`inspector_gap`
- 无障碍：可访问名「信息面板」
- 同步：Linux done（test_10_layout.py 信息面板自动折叠、手动展开、相反侧首选宽度保留、把手拖动及双击。 tests/ui/linux/results/run.9ME2GefI/report.json）
- 实现备注：区域 06：工作区与面板共用一条 GtkPaned，面板是 end child，显隐由 shell.layout 控制，保留原控件与已显示内容。分隔只由 Paned 自己的分栏线提供，面板不再画 border-left；面板只留内边距（inspector_gap）。宽度默认 inspector_width，钳在 inspector_min 与 inspector_max 之间；启动态、连接态、切换连接读同一个值。宽度落盘是窗口级记忆（独立窗口状态文件），不写进 connections.json。面板内容按当前标签钉住（#196）：切到表数据标签钉该标签的表、切到查询标签钉它当前选择的库（没有选择库 → 空态）、切到 Objects 固定页跟随树当前选中；旧的「切换标签只换表页记忆（Inspector::retarget）」已删除。钉住渲染链路：tabs.rs::TabSet::pin_target 产出 TabPin → ui.rs::apply_pin 按身份键（ui.rs::pin_identity = 标签|目标）去重 → inspector.rs::show_table_pinned / render_table 渲染；Objects 页的钉住走 ui.rs::render_selection。临时预览语义保留：树选中、Objects 行单击、数据网格列头点击仍立即驱动面板，预览不改钉住键；切走再回到某标签恢复该标签钉住的内容。鼠标经过不改面板。Inspector 不缓存跨访问的元数据：每次主动选中、切换子页、重新进入或 F5 刷新都按目标重新查库；已展示数据只服务本次渲染，面板不保证与数据库实时同步。按当前展示目标的会话处理断连。所属会话断开时清理展示及在途请求，其它连接断开时保留当前展示。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| drag | 面板可见 | 拖动调整该侧首选宽度并在松手时持久化；双击还原该侧默认首选宽度 | `src/shell.rs:249、src/shell.rs:230、src/window_state.rs:235` |
| load | 对象树的选中发生变化 | 按选中种类换页：连接 → 连接视图；库 / schema → 库视图；表 / 视图 / 物化视图 → 表视图；列 → 列视图；索引 / 触发器 → 状态页；序列 / 函数 / 过程 / 类型 → 只有 DDL；分组行或没有选中对象 → 空态 | `src/ui.rs:2224、src/inspector.rs:723` |
| load | 切换到表数据标签 | 面板钉住该标签的表：进表视图；首进默认子页「表信息」，手动切过 [DDL] / [表信息] 之后按标签记住（树预览默认仍是 DDL）；每次进入按目标子页重新查库 | `src/inspector.rs:857、src/inspector.rs:878` |
| load | 切换到查询标签且它已选择库 | 面板钉住它当前选择的库（服务端真值优先、其次选择器请求值）：进库视图；计数口径与树选中一致，按库名取自已展开分组（tree.group_counts_by_catalog），没展开过的不显示行 | `src/tabs.rs:1863、src/ui.rs:2335、src/tree.rs:1905` |
| load | 切换到查询标签且没有选择库（真值与请求值都为空） | 面板落空态页，不编造库信息 | `src/ui.rs:2343、src/inspector.rs:669` |
| load | 切换到 Objects 固定页 | 面板跟随对象树当前选中：连接 → 连接视图、库 / schema → 库视图、表 / 视图 / 物化视图 → 表视图（与既有「按选中种类换页」同一语义） | `src/tabs.rs:1834、src/ui.rs:2314` |
| load | 标签状态事件再次到达但钉住身份键没变（切回同一标签、重复事件、翻页） | 同键（pin_identity = 标签|目标）只拦截同一次操作的重复状态事件，不重复渲染、不重复发请求；用户重新进入同一对象或按 F5 主动刷新不受去重拦截，仍重新查库；表数据标签的表尚未建立时暂保留当前内容，快照到达后按新键再钉一次 | `src/ui.rs:1549、src/ui.rs:2366` |
| load | 刷新对象树（F5 / 右键刷新） | 不沿用历史读数，下一次展示重新查库；F5 主动刷新即使对象身份没变也强制重查当前 Inspector | `src/ui.rs:2805、src/inspector.rs:924` |
| load | 数据库连接断开（浏览会话失效） | 只清理属于该连接的当前展示：标题 / 副标题 / 提示 / 子页按钮与旧错误一并复位，使该连接在途响应失效；其它连接仍正常展示，钉住键同步清掉；后端整体失效时清全部 | `src/inspector.rs:958、src/ui.rs:4180` |

### `inspector.header` 标题块（标题 · 副标题）与分隔线

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_sm`、`gap_lg`、`gap_none`
- 同步：Linux done（§6 空态保留面板外壳；inspector.rs::new 的 header 与 Separator）
- 实现备注：标题用 heading 类、单行省略；副标题 dim-label + caption，跟着选中换成驱动 / 种类 / 「库」。标题与副标题之间不留间距（gap_none）：两块读成一组；标题块下沿与分隔线之间留一段（gap_lg），副标题不贴线。分隔线是面板根容器的直接子控件，从左端铺满内容区；切换条在分隔线下方。空态与选中态共用这一套外壳：只有标题文字回到「信息面板」、副标题清空。

### `inspector.empty` 空态（内容区留白）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_xl`
- 同步：Linux done（§6 没有选中对象；§5 空态与选中态共用外壳（inspector.rs::show_empty））
- 实现备注：空页是一个不带子控件的盒子：标题仍写「信息面板」、分隔线仍在，内容区留白；说明文字由标题承担（§5）。启动态还没有连上任何数据库时也走这一页，与连接态共用同一份实现，不存在「启动态无面板」这一档。分组行被选中时同样落到这一页：分组没有对象信息，面板不另立一套空白文案。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中分组行，或树里没有任何选中对象 | 保留面板外壳（标题「信息面板」+ 分隔线），副标题清空，内容区留白 | `src/inspector.rs:611、src/ui.rs:2152` |

### `inspector.connection` 连接视图（标题 · 副标题 · 字段行）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_sm`、`gap_md`、`gap_lg`
- 同步：Linux done（test_16_workspace_context.py::test_navigation_target_and_clear_selection：活动 A 时预览 B，B 改别名后 Inspector 同步名称、A 的标题保持、元数据请求数不变；test_14_inspector_no_cache.py 验证 Preview 请求隔离。）
- 实现备注：字段行顺序固定：驱动、地址、默认库、用户、TLS、凭据引用、服务端版本、连接延迟、会话数。地址是 host:port 或文件路径，不拼用户名与库名（它们各有独立字段）。服务器型驱动没有默认库时写「未指定」（吞掉整行会像漏了字段）；文件型驱动没有 TLS 一层，这一行整行不出现。凭据引用只写 credential_ref，面板里没有明文密码。连接别名或显示名消歧义变化时，当前预览按档案 ID 更新名称，不新增元数据请求。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中连接行且连接在配置里 | 标题 = 树与标签共享的连接显示名，副标题 = 驱动。会话数按该档案的浏览会话及本窗口的独立会话统计；字段行给驱动、地址（host:port 或文件路径）、默认库、用户、TLS、凭据引用（不含密码）、服务端版本、连接延迟、会话数；拿不到的字段不出现 | `src/ui.rs:2122、src/inspector.rs:621、src/inspector.rs:133` |
| load | 选中连接行但连接已不在配置里 | 状态页「连接已不在配置里」+「connections.json 里已经没有这条连接（config.invalid）。刷新对象树后重试。」 | `src/inspector.rs:625` |

### `inspector.field` 字段行（标签 · 值）

kind `row` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_sm`、`gap_none`
- 无障碍：可访问名「（行内容：字段标签 + 值）」
- 同步：Linux done（§5 拿不到的字段不出现；inspector.rs::fill_facts）
- 实现备注：两行垂直布局：标签 dim-label、值同字号同字重（font_panel，不加粗）；值逐字折行、可选中复制。值是长 token（如凭据引用）时按字符折行：按词折行会把面板的最小宽度顶大，面板就拖不回 inspector_width。值为空白时整行不渲染，所以「拿不到」表现为这一行不出现（§5），不用占位符号冒充。连接、库、列三种视图共用这一份行模板，不各自拼一套。

### `inspector.catalog` 库 / 分组视图（标题 · 副标题 · 计数）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_sm`、`gap_md`、`gap_lg`
- 同步：Linux done（test_15_refresh_regressions.py::test_query_catalog_tracks_rebind_and_disconnect 两个断开场景：同名库计数不串连接，目标失败后仍是原库。隔离 GTK qa_query_target_tracks_the_original_tab_session 覆盖迟到回答。）
- 实现备注：字段：名称、表计数、视图计数；计数只来自树里已经展开过的分组层，打开一个库不顺带扫整库。统计按连接的浏览会话与库或 Schema 身份匹配。无法唯一定位或未成功读取的分组省略该字段。该层还有下一页时补一行「计数说明：按已展开的结果统计（该层还有下一页）」，不把当前页当总数。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中库 / schema 行 | 标题 = 库名、副标题「库」；字段行给名称与表 / 视图计数，计数只来自已展开的分组层，没展开过的那一行不出现 | `src/ui.rs:2132、src/inspector.rs:644、src/inspector.rs:192` |
| load | 当前 SQL 标签已确定库上下文，或用户在 SQL 标签里切换连接 | 库视图跟随该标签实际绑定的连接。重绑等待中与失败时保留原归属及此前确认的库，成功后按新会话的库上下文更新，同名库也重新匹配统计 | `src/tabs.rs::TabSet::pin_target、src/ui.rs::pin_identity、src/tree.rs::TreePanel::group_counts_by_catalog` |
| load | 选中 Tables 或 Views 分组行 | 分组行没有对象信息：面板回到空态，标题回到「信息面板」 | `src/ui.rs:2152` |

### `inspector.object` 表 / 视图视图（标题 · 切换条 · 两页）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_sm`、`gap_none`
- 同步：Linux done（§6 选中表 / 视图；inspector.rs::show_object 的 Table 分支）
- 实现备注：标题 = 对象名、副标题 = 对象种类；标题 tooltip 给完整路径。顶部固定出现切换条，默认落在 DDL 页；两页共用一个 stack，同一时刻只有一页可见。只切面板内的页，不改标签与树的选中。对象名与种类来自选中节点，不等 GetDdl / GetColumns 返回：切换期间面板不会先空一下。序列 / 函数 / 过程 / 类型没有表信息页：只显示 DDL 正文一页，切换条整条隐藏（不是出现切换条却点不动）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中表 / 视图 / 物化视图 | 标题 = 对象名、副标题 = 种类；显示 [DDL] / [表信息] 切换条，按当前标签记住的页恢复（默认 DDL） | `src/inspector.rs:665、src/inspector.rs:691` |
| load | 切换标签（看的还是同一个对象） | 只换「表页选择」的记忆键，按新标签记住的页恢复 DDL / 表信息并按该页重新查库；不重建标签 | `src/inspector.rs:878、src/inspector.rs:924` |
| load | 选中序列 / 函数 / 过程 / 类型（有 DDL 的种类） | 标题 = 对象名、副标题 = 种类（标题 tooltip 给完整路径）；只显示 DDL 正文一页，不出现 [DDL] / [表信息] 切换条，也没有表信息页 | `src/inspector.rs:675、src/inspector.rs:732、src/inspector.rs:735` |

### `inspector.pane_switch` [DDL] / [表信息] 切换条

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `min_target`、`radius_sm`、`gap_xxs`、`gap_sm`、`gap_xs`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_mid`；状态 hover=`sqarvo_brand_soft`、selected=`sqarvo_brand_mid`
- 原生选型说明：原生 GtkStackSwitcher 的样式与命中区不符合「两个按钮各自贴边、各自 tooltip」，改用两个 set_group 的 GtkToggleButton 拼装（控件原生）。
- 同步：Linux done（§5 两个独立 toggle；theme.rs 的 .sqarvo-pane-switch 规则）
- 实现备注：两个独立 toggle，不用 .linked 分段容器：选中块是贴着文字的小圆角块，四角都圆。命中区仍不小于 min_target，缩的是内边距；hover 是 brand_soft 的半透明档，选中是 brand_mid 的半透明档（实底会把小号正文压成一块砖）。拉丁标签「DDL」比「表信息」多一小段上边距做光学补偿：行盒居中会让前者看着偏高。切换条与标题块、表信息行、DDL 正文共用一条左基准线（分隔线的左顶点）。切换条只在表 / 视图视图里出现；其它视图与状态页隐藏。

### `inspector.pane_switch.ddl` 「DDL」按钮

kind `control` · status `active` · parent `inspector.pane_switch`

- 样式：槽位 `inspector`；尺寸 `min_target`、`radius_sm`
- 无障碍：可访问名「显示该对象的 DDL」；Tab 可聚焦；面板内 Tab 次序为 DDL → 表信息
- 同步：Linux done（§6 切换表页；inspector.rs::install_toggles / set_pane）
- 实现备注：文字标签「DDL」，tooltip 同时登记为可访问名。与「表信息」互斥：按下本按钮即取消另一个的按下态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前是表 / 视图视图 | 切到 DDL 页；选择记到当前标签（标签生命周期内有效），重发 GetDdl 查询目标子页；不改变标签与树的选中 | `src/inspector.rs:972、src/inspector.rs:1064` |

### `inspector.pane_switch.info` 「表信息」按钮

kind `control` · status `active` · parent `inspector.pane_switch`

- 样式：槽位 `inspector`；尺寸 `min_target`、`radius_sm`
- 无障碍：可访问名「列、主键、索引与触发器、已加载行数」；Tab 可聚焦；面板内 Tab 次序为 DDL → 表信息
- 同步：Linux done（§6 切换表页；inspector.rs::install_toggles / apply_pane）
- 实现备注：文字标签「表信息」，tooltip 同时登记为可访问名。与「DDL」互斥：按下本按钮即取消另一个的按下态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前是表 / 视图视图 | 切到表信息页；选择记到当前标签（标签生命周期内有效），只重查 GetColumns / GetPrimaryKey / ListNodes；不改变标签与树的选中 | `src/inspector.rs:972、src/inspector.rs:1070` |

### `inspector.object.info` 表信息页（列 · 主键 · 索引 · 触发器 · 已加载行数）

kind `container` · status `active` · parent `inspector.object`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_lg`、`gap_sm`
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py 覆盖显式分页、重试、刷新、触发器查看和对象切换；test_15_refresh_regressions.py 验证行数更新不提交元数据请求。）
- 实现备注：分节提供列、主键、索引、触发器和行数。索引与触发器保留分页状态和显式刷新入口。按对象身份从有效的数据标签计算已加载行数。标签刷新、分页、关闭或失效时重算，最后一个有效来源消失后隐藏该节。这些行数事件不发送 Inspector 元数据请求。正文与 DDL 正文同字号同字重（font_panel、不加粗），层级只靠 dim 标签与分节间距。索引 / 触发器读取失败时显示错误及重试入口，已读回的本表资料仍然显示。内容多了纵向滚动，宽度不够时横向滚动，不把字段挤成两三字一行。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 切到表信息页（每次都重新查询列 / 主键 / 索引与触发器） | 查询当前表的列、主键和首批索引与触发器。后续页仅由显式操作读取，SQLite 触发器可在表信息中读取独立 DDL。过期响应不写入其他对象的展示。 | `src/inspector.rs::Inspector::load_info` |

### `inspector.object.info.children` 索引与触发器

kind `container` · status `active` · parent `inspector.object.info`

- 样式：槽位 `inspector`
- 无障碍：可访问名「索引与触发器」
- 原生控件：gtk:GtkBox
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_table_info_pages_retry_and_refresh）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 读取当前表的索引和触发器 | 显示已读取的索引和触发器。分页或重试失败时保留本次已加载的数据，错误带稳定错误码。 | `src/inspector.rs::Inspector::finish_info_children` |

### `inspector.object.info.children.trigger_picker` 触发器选择

kind `control` · status `active` · parent `inspector.object.info.children`

- 样式：槽位 `inspector`
- 无障碍：可访问名「选择触发器」
- 原生控件：gtk:GtkDropDown
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_trigger_ddl_view_copy_and_object_isolation）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 已读取一个或多个触发器 | 选中带完整对象引用的触发器。仅改变选择，不发元数据请求。 | `src/inspector.rs::ChildControls::new` |

### `inspector.object.info.children.load_more` 加载更多索引和触发器

kind `control` · status `active` · parent `inspector.object.info.children`

- 样式：槽位 `inspector`
- 无障碍：可访问名「加载更多索引和触发器」
- 原生控件：gtk:GtkButton
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_table_info_pages_retry_and_refresh）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前表还有有效分页令牌 | 显式读取一页索引与触发器，追加到当前表的结果。正在读取时禁止重复派发。 | `src/inspector.rs::Inspector::continue_info_children` |

### `inspector.object.info.children.retry` 重试读取索引和触发器

kind `control` · status `active` · parent `inspector.object.info.children`

- 样式：槽位 `inspector`
- 无障碍：可访问名「重试读取索引和触发器」
- 原生控件：gtk:GtkButton
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_table_info_pages_retry_and_refresh）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前表的索引或触发器读取失败 | 重试失败页。分页状态失效时从第一页重读，成功前保留已显示数据。 | `src/inspector.rs::Inspector::continue_info_children` |

### `inspector.object.info.children.view_ddl` 查看触发器 DDL

kind `control` · status `active` · parent `inspector.object.info.children`

- 样式：槽位 `inspector`
- 无障碍：可访问名「查看触发器 DDL」
- 原生控件：gtk:GtkButton
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_trigger_ddl_view_copy_and_object_isolation）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前 SQLite 浏览会话中选中了触发器 | 按选中对象的完整引用请求一次 GetDdl，在表信息中打开触发器正文。 | `src/inspector.rs::Inspector::open_trigger_ddl` |

### `inspector.object.info.refresh` 刷新表信息

kind `control` · status `active` · parent `inspector.object.info`

- 样式：槽位 `inspector`
- 无障碍：可访问名「刷新表信息」
- 原生控件：gtk:GtkButton
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_table_info_pages_retry_and_refresh）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 显示当前表的表信息 | 显式重新读取当前表的列、主键及首批索引和触发器。 | `src/inspector.rs::Inspector::force_reload` |

### `inspector.object.info.trigger_ddl` 触发器 DDL 正文

kind `container` · status `active` · parent `inspector.object.info`

- 样式：槽位 `inspector`
- 无障碍：可访问名「触发器 DDL」
- 原生控件：gtk:GtkStack + GtkTextView
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_trigger_ddl_view_copy_and_object_isolation）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 已明确请求当前 SQLite 触发器的 DDL | 复用 DDL 面板显示数据库原文和所属触发器。切换对象或离开正文使在途结果失效。 | `src/ddl.rs::DdlPanel::show_trigger` |

### `inspector.object.info.trigger_ddl.copy` 复制触发器 DDL

kind `control` · status `active` · parent `inspector.object.info.trigger_ddl`

- 样式：槽位 `inspector`
- 无障碍：可访问名「复制触发器 DDL」
- 原生控件：gtk:GtkButton
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_trigger_ddl_view_copy_and_object_isolation）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前触发器已取得 DDL | 复制当前触发器的完整正文到系统剪贴板，不发请求。 | `src/ddl.rs::DdlPanel::build` |

### `inspector.object.info.trigger_ddl.back` 返回表信息

kind `control` · status `active` · parent `inspector.object.info.trigger_ddl`

- 样式：槽位 `inspector`
- 无障碍：可访问名「返回表信息」
- 原生控件：gtk:GtkButton
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_trigger_ddl_view_copy_and_object_isolation）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 显示触发器 DDL | 返回当前表信息和已读页面，不重复读取元数据。 | `src/inspector.rs::Inspector::close_trigger_ddl` |

### `inspector.object.info.trigger_ddl.retry` 重试读取触发器 DDL

kind `control` · status `active` · parent `inspector.object.info.trigger_ddl`

- 样式：槽位 `inspector`
- 无障碍：可访问名「重试读取触发器 DDL」
- 原生控件：gtk:GtkButton
- 同步：Linux done（tests/ui/linux/test_14_inspector_no_cache.py::test_trigger_ddl_view_copy_and_object_isolation）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前触发器 DDL 读取失败 | 按原触发器引用重试一次 GetDdl。 | `src/ddl.rs::DdlPanel::show_trigger` |

### `inspector.object.info.columns` 列清单（列名 · 类型 · 可空 · 主键）

kind `container` · status `active` · parent `inspector.object.info`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_xxs`、`gap_sm`
- 平台差异：Linux：Linux 渲染为每列一张卡片：标题「列名 · 类型」+ 可空 / 默认值 / 字符集 / 排序规则四行；主键另起一节，不画四列表头。
- 同步：Linux done（§6 表信息给列与主键；inspector.rs 列节的渲染）
- 实现备注：列的四个口径：列名、类型、可空（可空 / 不可空（NOT NULL）两态）、主键标记。主键读不到时整列不打任何主键标记，也不把「读不到」画成「不是主键」。列清单为空时写「服务端返回了空列清单」，不画空表头。列名过长单行省略，全文进 tooltip。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 列已读回（GetColumns 成功） | 按列顺序给出列名、类型、可空、主键标记；主键读不到时整列不打标记 | `src/inspector.rs:1074、src/inspector.rs:1081、src/column_info.rs:105` |

### `inspector.object.info.columns.row` 列行（列名 · 类型 · 可空 · 主键标记）

kind `row` · status `active` · parent `inspector.object.info.columns`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_xxs`、`gap_sm`
- 无障碍：可访问名「（行内容：列名 + 类型 + 可空 + 主键标记）」
- 同步：Linux done（§6 表信息页的列；inspector.rs 列卡片的 compact_row 四行）
- 实现备注：不是可点行：列属性只在面板里读，点它不打开标签、不发请求。默认值三态与两档缺失分开写：「未知（服务端未提供）」不等于「无默认值」，字符集 / 排序规则缺失写「不适用」。行高随内容（可空与默认值可能折行），不套用网格的 row_height_data。

### `inspector.object.info.pk` 主键节（主键列 · 读不到时的说明行）

kind `control` · status `active` · parent `inspector.object.info`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_xs`、`gap_sm`
- 无障碍：可访问名「主键」
- 平台差异：Linux：说明走在表信息页顶部的提示行（info_status），不在主键节里；主键节只在 GetPrimaryKey 读回后渲染。
- 同步：Linux done（§6 表信息给主键；inspector.rs::primary_key_label 与失败路径 info_note）
- 实现备注：复合主键按主键内位置排序后用逗号连起来，不按名字排序。确实没有主键时写「无主键」；读不到主键是另一回事，给一行说明，不写成「无主键」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | GetPrimaryKey 读回 | 「主键列」一行按主键内顺序给列名（复合主键按位置排、逗号分隔）；确实没有主键时写「无主键」 | `src/inspector.rs:1114、src/inspector.rs:223` |
| load | GetPrimaryKey 失败 | 给一行说明（错误 headline + 「<文案>（<稳定错误码>）」），列清单里不画任何主键标记 | `src/inspector.rs:953、src/inspector.rs:1045` |

### `inspector.object.ddl` DDL 正文

kind `container` · status `active` · parent `inspector.object`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_md`、`gap_lg`、`gap_none`
- 原生控件：gtk:GtkTextView
- 平台差异：Linux：Linux 侧不单独设等宽字族：§5 要求 DDL 正文与表信息同字体同字号，正文走 .sqarvo-panel-text。
- 同步：Linux done（§6 DDL 页；ddl.rs::DdlPanel::new 的 TextView 与 ScrolledWindow）
- 实现备注：正文与表信息同字号同字重（font_panel）、不折行；长语句横向滚动，滚动条常驻（关掉 overlay 自动淡出，能滚到的地方要看得见）。正文可选中，复制走选中 + Ctrl+C；页内不画来源徽标、来源说明行与「复制」按钮。服务端返回空 DDL 时正文区写「服务端返回了空 DDL」+ 说明，不当成对象没有结构。没有浏览会话时正文区换状态页「这条连接没有浏览会话」+「对象所属连接的会话已断开：在对象树里点该连接重新连接后再试。」

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 切到 DDL 页（选中表 / 视图时默认落在这一页） | 每次进入 / 切回都发一次 GetDdl 并只显示正文：不折行、横向滚动且滚动条常驻、可选中复制；读取中写「正在读取 DDL…」+「等待 GetDdl 返回」；迟到响应在碰控件前按加载令牌丢弃 | `src/ddl.rs:281、src/ddl.rs:337` |
| load | GetDdl 失败 | 正文区换状态页：标题给错误 headline、说明行给「<文案>（<稳定错误码>）」；§7.1 的文案口径为「DDL 读取失败：<文案>（<错误码>）」，不是空面板 | `src/ddl.rs:307、src/ddl.rs:317` |

### `inspector.object.ddl.warning` DDL 琥珀警示（exact=false）

kind `control` · status `active` · parent `inspector.object.ddl`

- 样式：槽位 `inspector`；尺寸 `font_status`、`gap_sm`、`gap_xxs`；颜色 `warning_color`
- 无障碍：可访问名「DDL 来源警示」
- 同步：Linux done（§6 exact=false 时正文上方有琥珀警示；ddl.rs 的 warning 标签与样式表）
- 实现备注：警示行在正文上方、可折行，颜色只走 @warning_color（与状态行的警告文字同源）。exact=true（数据库原文）时整行不出现：原文不再另写来源说明。文案里带来源说明（拼装器版本与缺失项），用户能看出这份 DDL 缺了什么。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | GetDdl 返回 exact=false（客户端拼装） | 正文上方出现一行琥珀警示「⚠ 这份 DDL 由客户端拼装，不是数据库原文；信息有损失 · <来源说明>」 | `src/ddl.rs:289、src/ddl.rs:291、src/ddl.rs:92` |

### `inspector.column` 列视图（六项属性）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_sm`、`gap_md`、`gap_lg`
- 同步：Linux done（test_15_refresh_regressions.py::test_column_info_f5_reloads_that_column_not_another_object；GTK column_reload_updates_model_and_discards_stale_responses）
- 实现备注：字段顺序固定：列名、原始类型、可空、默认值、字符集、排序规则。默认值四态分开写：「未知（服务端未提供）」/「无默认值」/「默认 NULL」/ 表达式原文（原文为空写「表达式（服务端未给出文本）」）。字符集 / 排序规则缺失写「不适用」；这两种缺失是数据库没有这一层，不是面板没读到。从树上的列节点进来时副标题补所属表名（「列 · 表名 · 类型」），从网格列头进来只有类型。重读时清除原列字段与错误状态。只接受当前加载周期的响应，成功时同步更新列模型和字段，列已删除或读取失败时显示对应错误。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中列节点且父节点是表 / 视图 | 读父表的 GetColumns 再挑出这一列：标题 = 列名、副标题「列 · <表名> · <类型>」，字段行给 列名、原始类型、可空、默认值、字符集、排序规则；能确定父表与列身份时 F5 会重查这一列 | `src/inspector.rs:706、src/inspector.rs:997、src/column_info.rs:166` |
| click | 点数据网格的列头（该表已打开） | 进同一个列页：用 GetColumns 已有模型，不重发 RPC；同时记下父表与列身份供 F5 精确重查；结果集表达式 / 别名列没有父表，不猜测来源 | `src/inspector.rs:751、src/ui.rs:2204` |

### `inspector.state` 状态页（不支持 · 无从判断）

kind `control` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_lg`、`gap_md`
- 无障碍：可访问名「面板状态」
- 同步：Linux done（§6 选中索引 / 触发器、选中列（无父表）；inspector.rs::StateText）
- 实现备注：权限不足、对象不存在、不支持种类都给可读文案 + 稳定错误码。状态页会挤掉 DDL 与表信息两页的可见性，切换条同时隐藏：面板不会出现「空页 + 切换条」这种半截状态。换对象时上一次的状态页文案要清掉，防止别的对象读到旧错误。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中索引 / 触发器（MariaDB 没有独立 DDL） | 状态页「该对象种类不支持查看 DDL」+「<种类>没有独立的 DDL：定义在所属表的 DDL 里（metadata.object_kind_unsupported）。换一个表 / 视图再试。」；不是空面板 | `src/inspector.rs:739、src/inspector.rs:286` |
| load | 选中的列节点不在表 / 视图下面（拿不到父表） | 状态页「列的详细属性需要知道所属的表」+「从对象树里表的子节点进入列，才能读到 GetColumns 的完整属性。（metadata.object_not_found）」 | `src/inspector.rs:723` |

### `inspector.txn_hint` 跨标签事务提示行

kind `control` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `font_status`、`gap_md`
- 无障碍：可访问名「未提交事务提示」
- 同步：Linux done（§6 跨标签事务提示；inspector.rs::set_txn_hint）
- 实现备注：一行可折行的 caption，没有未提交事务时整行隐藏（不占位）。位置在标题块与分隔线之间：读面板内容之前先看到「别处还有没提交的事务」。当前挂的 sqarvo-inspector-warning 类在 theme.rs 里还没有对应规则，颜色随正文色——警示色待补（未进 theme.rs 的 token）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 同一 catalog 的其它标签有未提交事务 | 面板顶部常驻一行「同一 catalog 的其它标签还有未提交事务：<标签名…>」；没有则整行隐藏 | `src/inspector.rs:847、src/ui.rs:1832` |

### `inspector.object.info.status` 表信息提示行（读取中 · 读取失败）

kind `control` · status `active` · parent `inspector.object.info`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_sm`
- 无障碍：可访问名「表信息状态」
- 同步：Linux done（§6 表信息页的失败路径；inspector.rs::info_note 与 load_table 的提示行）
- 实现备注：提示行在表信息页最上面、可折行；有分节渲染出来后它退场。读取失败时提示行留在最上面，已读回的分节继续显示（不清空、不盖成空面板）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 表信息页开始读取 | 写「正在读取列、主键与索引…」；没有浏览会话时写「这条连接没有浏览会话：列属性读不到（metadata.object_not_found）。在对象树里重新连接后再试。」 | `src/inspector.rs:910、src/inspector.rs:911` |
| load | 列 / 主键读取失败 | 提示行给错误 headline + 「<文案>（<稳定错误码>）」；§7.1 的文案口径为「列读取失败：<文案>（<错误码>）」，不是空面板 | `src/inspector.rs:1045、src/inspector.rs:931` |

## 对象树（`object_tree`）

左侧导航包含连接标题、本地筛选及显式远程搜索、资源树、右键菜单与反馈。只有主动浏览会改变固定 Objects 的范围。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `object_tree` | 对象树侧栏 | area | active | done |
| `object_tree.tree` | 　树本体 | container | active | done |
| `object_tree.tree.row` | 　　行模板（箭头 · 图标 · 名称 · 状态） | row | active | done |
| `object_tree.tree.row.expander` | 　　　行首箭头 | control | active | done |
| `object_tree.tree.row.conn` | 　　　连接行 | row | active | done |
| `object_tree.tree.row.catalog` | 　　　库 / Schema 行 | row | active | done |
| `object_tree.tree.row.group` | 　　　资源分类行（Tables / Views） | row | active | done |
| `object_tree.tree.row.object` | 　　　表 / 视图行 | row | active | done |
| `object_tree.tree.row.field` | 　　　列 / 索引 / 触发器行 | row | removed | 已清 |
| `object_tree.tree.row.placeholder` | 　　　辅助行（读取中 / 加载更多 / 重试 / 空说明） | row | active | done |
| `object_tree.search` | 　对象树搜索区 | container | active | done |
| `object_tree.search.box` | 　　搜索输入框 | control | active | done |
| `object_tree.search.status` | 　　搜索状态行 | control | active | done |
| `object_tree.search.results` | 　　远程搜索结果列表 | container | active | done |
| `object_tree.ctx_menu` | 　右键菜单 | container | active | done |
| `object_tree.ctx_menu.connect_saved` | 　　「连接（使用已保存的凭据）」 | item | active | done |
| `object_tree.ctx_menu.disconnect` | 　　「断开连接」 | item | active | done |
| `object_tree.ctx_menu.refresh_connection` | 　　「刷新连接」 | item | active | done |
| `object_tree.ctx_menu.refresh_catalog` | 　　「刷新库」 | item | active | done |
| `object_tree.ctx_menu.refresh_group` | 　　「刷新这一组」 | item | active | done |
| `object_tree.ctx_menu.edit_connection` | 　　「编辑连接…」 | item | active | done |
| `object_tree.ctx_menu.edit_connection.confirm` | 　　　编辑连接前的关闭确认框 | container | active | done |
| `object_tree.ctx_menu.delete_connection` | 　　「删除连接」 | item | active | done |
| `object_tree.ctx_menu.open` | 　　「打开」 | item | active | done |
| `object_tree.ctx_menu.dump_table` | 　　「转储表…」 | item | active | done |
| `object_tree.hint` | 　侧栏底部临时提示行 | control | active | done |
| `object_tree.header` | 　连接标题 | container | active | done |
| `object_tree.header.collapse_all` | 　　全部折叠 | control | active | done |
| `object_tree.header.more` | 　　导航更多菜单 | control | active | done |
| `object_tree.header.more.clear_selection` | 　　　清除选择 | item | active | done |
| `object_tree.search.remote` | 　　搜索已连接的数据库 | control | active | done |

### `object_tree` 对象树侧栏

kind `area` · status `active`

- 样式：槽位 `sidebar`；尺寸 `sidebar_width`、`sidebar_min`、`sidebar_max`
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：固定导航容器、真实动作和筛选；tests/ui/linux/test_16_workspace_context.py::test_navigation_target_and_clear_selection：F5 列表变化拒绝旧菜单。）
- 实现备注：顶部标题和搜索区固定，资源树独立纵向滚动。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key F5 | 工作区外壳存在，当前页为 Objects 或查询 | 重读已加载的导航层。对象列表沿用原范围；表数据页的 F5 继续刷新该表。 | `src/tree.rs::TreePanel::reload、src/ui.rs` |

### `object_tree.tree` 树本体

kind `container` · status `active` · parent `object_tree`

- 样式：槽位 `sidebar`；尺寸 `navigation_row_height`
- 无障碍：可访问名「对象树」；Tab 可聚焦，方向键移动或展开，Enter 激活
- 原生控件：gtk:GtkListView + GtkTreeListModel
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：真实方向键、Enter、筛选与清词恢复当前选择；tests/ui/linux/test_16_workspace_context.py::test_navigation_target_and_clear_selection：隐藏目标、失效资源与固定页；隔离 GTK reveal_failure_preserves_search_and_newer_explicit_selection、reveal_distinguishes_page_limit_complete_absence_and_incomplete_data。）
- 实现备注：层级为连接 → 库 → 可选 Schema → 分类 → 对象。GTK 虚拟化列表复用会话内缓存。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key ↑ / ↓ / ← / → | 树有焦点 | 上下键移动逻辑选择；左右键展开、收起或移向父子资源。方向键不改变固定 Objects 范围。 | `src/tree.rs::TreePanel::install_handlers` |
| key Enter | 资源或可操作的辅助行有焦点 | 库、Schema、分类主动浏览固定 Objects；其他行激活其对应动作。 | `src/tree.rs::TreePanel::activate_row` |
| load | 显示的模型折叠、过滤或重建 | 保留逻辑选择。失效目标持续拦截动作；明确重新选择或清除后更新目标。 | `src/tree.rs::TreePanel::restore_visible_selection` |
| load | 显式定位命令给出完整资源身份 | 在捕获档案的现有浏览会话内沿目标路径每层最多追加两页。成功后定位并在必要时清词；失败保留选择、检索词和搜索模式。达到上限、完整路径找不到资源与读取未完成分别提示。后续定位、主动选择或更改搜索使旧定位失效。 | `src/tree.rs::TreePanel::reveal_target` |

### `object_tree.tree.row` 行模板（箭头 · 图标 · 名称 · 状态）

kind `row` · status `active` · parent `object_tree.tree`

- 样式：槽位 `sidebar`；尺寸 `navigation_row_height`、`gap_xs`、`radius_sm`、`navigation_indent`、`gap_sm`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_mid`、`sqarvo_brand_on_mid`、`sqarvo_brand_on_soft`；状态 hover=`sqarvo_brand_soft`、selected=`sqarvo_brand_mid`
- 无障碍：可访问名「对象树行」
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：数据库和分类与连接选择事件各自分派；tests/ui/linux/test_10_layout.py::test_layout_preference_survives_first_connection、test_bottom_state_follows_active_workspace：真实单击后 Enter 激活连接及表；tests/ui/linux/test_05_disconnect.py::test_menu_driven_active_disconnect：连接状态可访问描述。）
- 实现备注：名称单行尾截断，完整名称和连接状态保留在 Tooltip 与可访问属性。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 主动单击资源正文 | 选择资源并将键盘焦点交给该行。库、Schema 与分类同时主动浏览；连接和对象保留当前工作区，后续 Enter 激活所选资源。 | `src/tree.rs::TreePanel::install_handlers` |

### `object_tree.tree.row.expander` 行首箭头

kind `control` · status `active` · parent `object_tree.tree.row`

- 样式：尺寸 `tree_arrow_box`、`navigation_arrow_icon`
- 无障碍：可访问名「展开或折叠」；Tab 不可聚焦（不占 Tab 位）
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_tree_navigation：箭头开合层级；tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：箭头保持活动查询。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 行有可展开的子层 | 单独控制展开和收起；无业务选择变化。未连接行与对象叶子不提供箭头。 | `src/tree.rs::TreePanel::toggle_branch` |

### `object_tree.tree.row.conn` 连接行

kind `row` · status `active` · parent `object_tree.tree.row`

- 样式：尺寸 `status_dot_diameter`；颜色 `sqarvo_status_fg`
- 无障碍：可访问名「（行内容：连接显示名；可访问描述：状态）」
- 图标：`tree.connection`、`tree.connection.local`、`tree.connection.generic`
- 同步：Linux done（tests/ui/linux/test_05_disconnect.py::test_menu_driven_active_disconnect：主动断开后双击重新连接；tests/ui/linux/test_16_workspace_context.py::test_navigation_target_and_clear_selection：Peer 别名及档案所有权。）
- 实现备注：状态圆点取浏览会话状态。驱动使用系统符号图标，未知驱动使用通用图标。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接行 | 选择连接并更新原有预览。 | `src/tree.rs::TreePanel::install_handlers` |
| double_click | 连接行 | 已连接时展开或收起；未连接时建立指定档案的浏览会话。缺凭据走原对话框，过渡态沿用连接入口守卫。 | `src/tree.rs::TreePanel::activate_row` |
| load | 首次展开已连接的档案 | 读该连接的目录页，后续分页由用户显式触发。 | `src/tree.rs::TreePanel::level_for` |

### `object_tree.tree.row.catalog` 库 / Schema 行

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：库或 Schema 名）」
- 图标：`object.catalog`、`object.schema`
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：库单击与 Enter 浏览固定页；tests/ui/linux/test_04_tree.py::test_tree_navigation：独立展开；objects_target_follows_the_selected_node_kind 单测核对 Schema 作用域。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 主动单击正文 | 选择完整库或 Schema 范围并浏览固定 Objects。 | `src/ui.rs::objects_target` |
| double_click | 主动双击正文 | 第一击浏览一次，第二击展开或收起。 | `src/tree.rs::TreePanel::activate_row` |
| load | 展开库或 Schema | 无 Schema 层的库与 Schema 节点创建支持的分组；驱动声明 Schema 层时按需读取该层。 | `src/tree.rs::TreePanel::child_source` |

### `object_tree.tree.row.group` 资源分类行（Tables / Views）

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：分组名）」
- 图标：`tree.group.tables`、`tree.group.views`
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：分类双击一次 Objects 请求；tests/ui/linux/test_04_tree.py::test_local_refresh_error_can_retry_while_filtered：未实现分类隐藏、可用空组、读取失败与重试。）
- 实现备注：在树中隐藏未实现的 Functions、Queries 和驱动明确返回能力不支持的分类。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 主动单击分类正文 | 选择该分类并按精确范围浏览固定 Objects。 | `src/tree.rs::TreePanel::update_navigation` |
| double_click | 主动双击分类正文 | 一次浏览，同一手势的第二击无额外请求。 | `src/tree.rs::TreePanel::activate_row` |
| load | 通过独立箭头展开分类 | 逐组使用已有 ListNodes 请求和分页；空列表保留分组，权限或读取失败保留重试入口。 | `src/tree.rs::TreePanel::finish_page` |

### `object_tree.tree.row.object` 表 / 视图行

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：对象名；可访问描述：种类）」
- 图标：`object.table`、`object.view`
- 同步：Linux done（tests/ui/linux/test_16_workspace_context.py::test_navigation_target_and_clear_selection：不同档案的同名表、复用、刷新后新身份；table_and_view_remain_leaf_nodes_even_with_metadata_children 单测确认叶节点。）
- 实现备注：表和视图是叶节点。表信息仍在 Inspector 中显示字段、索引和触发器；数据网格列菜单提供字段详情。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 具体对象 | 更新选择和原有 Inspector 预览，保留当前工作区与 Objects 范围。 | `src/tree.rs::TreePanel::update_navigation` |
| double_click | 可打开的对象 | 按完整资源身份打开或复用数据工作区。 | `src/tree.rs::TreePanel::activate_row` |

### `object_tree.tree.row.field` 列 / 索引 / 触发器行

kind `row` · status `removed` · parent `object_tree.tree.row` · → `inspector.object.info`

- 无障碍：可访问名「（行内容：列 / 索引 / 触发器名）」
- 图标：`object.column`、`object.index`、`object.trigger`、`object.type`
- 同步：Linux removed
- 实现备注：叶子对象：没有子层、没有菜单、不发请求。

### `object_tree.tree.row.placeholder` 辅助行（读取中 / 加载更多 / 重试 / 空说明）

kind `row` · status `active` · parent `object_tree.tree.row`

- 样式：尺寸 `navigation_row_height`、`font_status`
- 无障碍：可访问名「（行内容：加载状态或可用操作）」
- 图标：`tree.note.loading`、`tree.note.warning`、`tree.note.info`
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_filter_can_page_without_matches：无匹配手动分页一次；tests/ui/linux/test_04_tree.py::test_local_refresh_error_can_retry_while_filtered：真实读取失败保留旧数据、一次手动重试；隔离 GTK page_failure_keeps_loaded_rows_and_only_unsupported_hides_a_group。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前层有加载更多或重试 | 显式读取相应页。辅助动作保留原逻辑选择；刷新失败保留已读取数据。 | `src/tree.rs::TreePanel::activate_row` |
| load | 读取中、空数据、读取失败或权限不足 | 按真实结果显示不同状态。只有分页和重试行可以主动激活。 | `src/tree.rs::TreePanel::finish_page` |
| right_click | 辅助行 | 保持当前逻辑选择和工作区。 | `src/tree.rs::TreePanel::install_handlers` |

### `object_tree.search` 对象树搜索区

kind `container` · status `active` · parent `object_tree`

- 样式：槽位 `sidebar`；尺寸 `sidebar_width`、`navigation_search_height`
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：在导航语义容器中找到搜索、远程入口和状态。）
- 实现备注：挂载于导航标题与资源树之间。

### `object_tree.search.box` 搜索输入框

kind `control` · status `active` · parent `object_tree.search`

- 样式：槽位 `sidebar`；尺寸 `font_ui`、`navigation_search_height`
- 无障碍：可访问名「搜索资源…」；Tab Ctrl+F 请求显示导航并聚焦；Esc 清词
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：大小写不敏感、改词和清词、收起快照恢复，qa.rpc 业务计数不变。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key 输入 / Esc | 输入、修改或清除关键词 | 按名称或别名匹配已加载资源，大小写不敏感。保留祖先与匹配父节点的已加载子树、可用分页和重试。输入和清词零数据库请求；退出恢复普通树展开与当前逻辑选择。 | `src/tree.rs::TreePanel::refresh_local_filter` |
| key Enter | 搜索框有焦点 | Enter 保留当前搜索模式。远程搜索有独立按钮。 | `src/tree.rs::TreePanel::install_handlers` |
| key Ctrl+F | 工作区存在 | 请求显示导航并聚焦输入框。 | `src/ui.rs` |

### `object_tree.search.status` 搜索状态行

kind `control` · status `active` · parent `object_tree.search`

- 样式：槽位 `sidebar`；尺寸 `font_status`
- 无障碍：可访问名「搜索状态」
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：本地无匹配及远程第一页范围；tests/ui/linux/test_04_tree.py::test_filter_can_page_without_matches：无匹配可继续分页。）
- 实现备注：可见文本单行省略，完整提示保留在 Tooltip。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 存在筛选关键词或远程结果 | 本地提示仅筛选已加载资源或没有匹配。远程显示加载、结果、空、权限或失败及既有取数限制。 | `src/tree.rs::TreePanel::set_remote_status` |

### `object_tree.search.results` 远程搜索结果列表

kind `container` · status `active` · parent `object_tree.search`

- 样式：槽位 `sidebar`
- 无障碍：可访问名「远程搜索结果」
- 原生控件：gtk:GtkListView
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：具名远程结果列表、库行定位成功后清词；隔离 GTK late_remote_responses_leave_new_local_projection_untouched、retired_session_pages_cannot_publish_results_or_replace_new_cache；单测 remote_search_keeps_same_named_resources_in_distinct_scopes。）
- 实现备注：按远程代次、每条请求的档案会话及发布时整轮会话快照校验结果。按完整对象引用合并重复结果，保留不同范围的同名资源；列表虚拟化，按捕获身份打开或定位。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表或视图结果 | 打开数据工作区，保留结果和关键词。 | `src/tree.rs::TreePanel::install_handlers` |
| click | 库或 Schema 结果 | 使用捕获资源身份定位；成功后更新逻辑选择和必要的搜索状态。 | `src/tree.rs::TreePanel::reveal_target` |
| click | 连接结果 | 明确激活该连接。 | `src/tree.rs::TreePanel::install_handlers` |
| click | 结果分支箭头 | 只控制当前远程结果分支。 | `src/tree.rs::TreePanel::install_remote_factory` |

### `object_tree.ctx_menu` 右键菜单

kind `container` · status `active` · parent `object_tree`

- 样式：槽位 `overlay`
- 原生控件：gtk:GtkPopoverMenu
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：分类真实右键保留查询；tests/ui/linux/test_16_workspace_context.py::test_navigation_target_and_clear_selection：捕获菜单身份、失效后拒绝。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| right_click | 资源行有可用动作 | 先更新逻辑选择，再用捕获的档案、会话及完整资源路径显示相应菜单，不切换固定 Objects。执行时重新校验目标。 | `src/tree.rs::TreePanel::show_context_menu` |

### `object_tree.ctx_menu.connect_saved` 「连接（使用已保存的凭据）」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「连接（使用已保存的凭据）」
- 同步：Linux done（§2.3 连接（使用已保存的凭据））
- 实现备注：只读 credential_ref，不读也不写明文密码。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接未连接 / 失败 | 与左键双击同一条 connect_saved；失败给出可读原因 | `src/tree.rs:3305、src/ui.rs:845` |

### `object_tree.ctx_menu.disconnect` 「断开连接」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「断开连接」
- 同步：Linux done（tests/ui/linux/test_05_disconnect.py::test_menu_driven_active_disconnect：隔离 X11 真实右击、菜单和确认、未连接后双击复连；tests/ui/linux/test_16_workspace_context.py::test_navigation_target_and_clear_selection：独立查询会话断开后实际执行 SQL。）
- 实现备注：只关闭该连接的浏览会话（树 / 搜索 / DDL）；数据页用另一条会话，已加载数据仍可查看；其它连接的浏览会话不受影响。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该连接已连接 | 先确认「断开连接？」（说明影响范围；按钮 取消 / 断开连接，破坏性默认取消），确认时复核原浏览会话；会话失效时提示 | `src/tree.rs:3305、src/ui.rs:2954` |

### `object_tree.ctx_menu.refresh_connection` 「刷新连接」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「刷新连接」
- 同步：Linux done（§2.3 刷新连接）
- 实现备注：重读已经加载的层，失败时保留有效旧数据并提供重试。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该连接已连接 | 重读这条连接已经建立的层；一层都没有时侧栏底部提示行写「这条连接还没有展开过：展开时会重新读取」 | `src/tree.rs:3482、src/tree.rs:3451、src/tree.rs:1949` |

### `object_tree.ctx_menu.refresh_catalog` 「刷新库」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「刷新库」
- 同步：Linux done（§2.3 刷新库）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 库 / schema 节点 | 重读该库或 Schema 已加载的资源层。没有已加载层时给出提示；失败时保留有效旧数据。 | `src/tree.rs:3509、src/tree.rs:1949` |

### `object_tree.ctx_menu.refresh_group` 「刷新这一组」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「刷新这一组」
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_local_refresh_error_can_retry_while_filtered：隔离 X11 真实分类菜单，SQLite 锁失败后保留已加载结果，解锁后手动重试。）
- 实现备注：读取失败保留旧数据，从相应错误行可重试。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 分组节点 | 已展开（层已建立）→ 重读该组第一页；未展开过 → 不发请求，提示行写「这个分组还没展开：展开时会重新读取」（4 秒后自行收起）并记日志 | `src/tree.rs:3422、src/tree.rs:1949` |

### `object_tree.ctx_menu.edit_connection` 「编辑连接…」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「编辑连接…」
- 同步：Linux done（test_06_mcp_access.py::test_edit_profile_preserves_id_and_increments_revision 验证启动菜单保存原 UUID 并推进 revision；test_16_workspace_context.py::test_navigation_target_and_clear_selection 验证工作区编辑仍使用捕获档案。）
- 实现备注：密码永不回显：已有 credential_ref 时密码行留空、复用引用；不会自动连接。按 saved_key 重建对话框：对已关闭的 AdwDialog 调 present() 是空操作，复用旧实例会让这个条目「点了没反应」（#175）。档案不存在或其驱动不可用时提示目标失效，编辑对话框只接收原档案。编辑期间不留旧会话：配置改动只在下次连接时生效，留着旧标签会让人误以为已经生效。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接存在于配置里 | 该连接已连接时先弹确认（见 edit_connection.confirm）：确认后关闭它名下所有标签、断开连接，再打开已预填的「编辑连接」对话框；未连接时直接打开对话框 | `src/ui.rs:3781、src/ui.rs:3826` |

### `object_tree.ctx_menu.edit_connection.confirm` 编辑连接前的关闭确认框

kind `container` · status `active` · parent `object_tree.ctx_menu.edit_connection`

- 样式：槽位 `overlay`
- 无障碍：可访问名「编辑连接？」
- 原生控件：gtk:AdwAlertDialog
- 同步：Linux done（tests/ui/linux/results/run.3jtK5EPM/report.json：已连接档案编辑确认列出待放弃 SQL；取消保留 SQL，确认后进入原档案编辑。）
- 实现备注：只在连接**已连接**时弹：没有会话、没有标签可关的条目直接进编辑，不多一次点击。正文两句，第二句单独一行：「未提交的更改会一并丢弃。」；有脏行时换成「其中 N 个标签有未提交的更改，确认后一并丢弃。」；确认后才关标签、断连接，然后打开对话框；取消则连接与标签保持原样。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接已连接 | 取消：什么都不做；「关闭并编辑」：复核原浏览会话，会话失效时提示，仍有效时关闭确认时捕获并且仍属于该连接的标签（正文列出将放弃的未保存 SQL 与未提交更改）→ 断开连接 → 打开已预填的「编辑连接」对话框 | `src/ui.rs:3781、src/tabs.rs:2921` |

### `object_tree.ctx_menu.delete_connection` 「删除连接」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「删除连接」
- 同步：Linux done（tests/integration/gtk_mcp_access.py 验证 GTK 删除档案后同一 MCP 子进程拒绝查询并释放 SQLite 文件描述符。）
- 实现备注：档案删除失败提示「删除连接失败」；系统凭据引用可以被其他档案共用，删除档案时保留凭据。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接存在于配置里 | 二次确认「删除连接？」后按 UUID 与 revision 删除共享档案，移除树节点并清理该连接的浏览会话。保留凭据。 | `src/ui.rs:1796、src/ui.rs:701` |

### `object_tree.ctx_menu.open` 「打开」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「打开」
- 同步：Linux done（tests/ui/linux/test_16_workspace_context.py::test_navigation_target_and_clear_selection：真实表资源菜单打开、复用、两条同名表不串及旧目标拒绝。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表 / 视图节点 | 与双击同一条路径打开数据标签 | `src/tree.rs:3266、src/tree.rs:3305` |

### `object_tree.ctx_menu.dump_table` 「转储表…」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「转储表…」
- 同步：Linux done（§2.3 转储表…（v0.2.5））
- 实现备注：只有表节点有；视图没有（往视图 INSERT 不可重放）。按浏览会话 offset 逐页读全表，建表语句与列名在同一后台线程读，不跨连接。取消或失败不留半截文件；SQL 引号按该连接驱动（MariaDB 反引号 / SQLite 双引号）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表节点且该连接有浏览会话 | 弹确认「转储表 <表名>？」→ 选文件（默认 <表名>.sql，过滤 *.sql）→ 进度 / 可取消；不需要先打开标签 | `src/tree.rs:3305、src/tree.rs:738、src/export_ui.rs` |

### `object_tree.hint` 侧栏底部临时提示行

kind `control` · status `active` · parent `object_tree`

- 样式：槽位 `sidebar`；尺寸 `font_status`、`gap_xs`
- 无障碍：可访问名「侧栏提示」
- 同步：Linux done（tests/ui/linux/test_16_workspace_context.py::test_navigation_target_and_clear_selection：AT-SPI 读到选择库及明确目标不可用提示；隔离 GTK reveal_failure_preserves_search_and_newer_explicit_selection、reveal_distinguishes_page_limit_complete_absence_and_incomplete_data。）
- 实现备注：用于刷新、命令目标与定位的临时反馈；4 秒后自行收起。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 刷新被跳过 | 写一行说明（例如「这个分组还没展开：展开时会重新读取」），4 秒后自行收起 | `src/tree.rs:1949` |
| load | 工作区命令缺少必要的目标，或明确目标已不可用 | 提示选择所需资源，或重新选择、清除已失效的选择 | `src/tree.rs::TreePanel::show_hint、src/ui.rs::show_objects_for_kind、src/tabs.rs::TabSet::new_query_tab` |
| load | 有限定位未找到目标 | 区分分页上限、目标路径已读完但无资源、目标列表未读完或不可用；达到分页上限时提示可手动加载更多。 | `src/tree.rs::TreePanel::reveal_object` |

### `object_tree.header` 连接标题

kind `container` · status `active` · parent `object_tree`

- 样式：槽位 `sidebar`；尺寸 `navigation_header_height`
- 无障碍：可访问名「连接」
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：顶部搜索、全部折叠、更多菜单位于导航语义容器。）

### `object_tree.header.collapse_all` 全部折叠

kind `control` · status `active` · parent `object_tree.header`

- 样式：槽位 `sidebar`；尺寸 `navigation_header_button`
- 无障碍：可访问名「全部折叠」
- 图标：`tree.collapse_all`
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：搜索内全部折叠、改词保持折叠，清词恢复普通树。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 导航可用 | 收起当前视图的资源分支，保留逻辑选择与普通树在搜索前的展开状态。 | `src/tree.rs::TreePanel::collapse_all` |

### `object_tree.header.more` 导航更多菜单

kind `control` · status `active` · parent `object_tree.header`

- 样式：槽位 `sidebar`；尺寸 `navigation_header_button`
- 无障碍：可访问名「更多」
- 图标：`tree.more`
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：打开具名原生更多菜单后通过键盘激活。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 导航可用 | 打开包含清除选择的原生菜单。 | `src/tree.rs::TreePanel::new` |

### `object_tree.header.more.clear_selection` 清除选择

kind `item` · status `active` · parent `object_tree.header.more`

- 样式：槽位 `sidebar`
- 无障碍：可访问名「清除选择」
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：从具名原生更多菜单清除隐藏逻辑选择。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 更多菜单打开 | 通过全局清除选择命令清空逻辑选择。固定 Objects 范围保持。 | `src/tree.rs::TreePanel::clear_selection` |

### `object_tree.search.remote` 搜索已连接的数据库

kind `control` · status `active` · parent `object_tree.search`

- 样式：槽位 `sidebar`
- 无障碍：可访问名「搜索已连接的数据库」
- 同步：Linux done（tests/ui/linux/test_04_tree.py::test_navigation_pointer_and_filter：显式按钮才发 MetadataHub 请求，改词返回本地；隔离 GTK 迟到目录与对象回包丢弃。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 已输入关键词 | 执行显式远程搜索。返回本地、修改或清词使旧请求失效。远程仅查询目录及各库的第一页，不自动翻页或建立连接。 | `src/tree.rs::TreePanel::search_now` |

## Objects 固定页（`objects_page`）

工作区最左侧的固定页：标题「Objects」、固定本地检索、三列表格、计数和分页。主动 Browse 决定连接、库、Schema 与分类；列表使用现有浏览会话并独立持有分页 token。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `objects_page` | Objects 固定页 | area | active | done |
| `objects_page.tab` | 　固定页页签（pinned，无关闭按钮） | control | active | done |
| `objects_page.filter` | 　Objects 检索框 | control | active | done |
| `objects_page.table` | 　三列表格（行号 · 表名 · 类型） | container | active | done |
| `objects_page.table.row` | 　　行模板（行号 · 图标 + 名称 · 类型） | row | active | done |
| `objects_page.table.row.menu` | 　　　对象行菜单 | container | active | done |
| `objects_page.table.row.menu.open` | 　　　　打开 | control | active | done |
| `objects_page.table.row.number` | 　　　行号格 | cell | active | done |
| `objects_page.table.row.name` | 　　　表名格（图标 + 名称） | cell | active | done |
| `objects_page.table.row.kind` | 　　　类型格 | cell | active | done |
| `objects_page.status` | 　状态行 | control | active | done |
| `objects_page.more` | 　「加载更多…」与「重试」 | control | active | done |
| `objects_page.empty` | 　空态页 | control | active | done |

### `objects_page` Objects 固定页

kind `area` · status `active`

- 样式：槽位 `workspace.content`；尺寸 `gap_xl`、`gap_md`
- 无障碍：可访问名「Objects」
- 同步：Linux done（test_15_refresh_regressions.py::test_objects_filter_keeps_selection_and_scope_local：Objects 分组、检索和列表独立可达。test_10_layout.py::test_focus_and_navigation：最小窗口下可聚焦、输入、清除。）
- 实现备注：页首为静态标题和右侧检索框，列表独立滚动，列表下方呈现分页和状态。页面没有自己的数据库会话：列表读这条连接的浏览会话（MetadataHub），与标签会话无关；关掉标签也不会动它。启动态（未连接）用同一个 ObjectsPage 实现，只换空态文案（见 objects_page.empty）。页内距和元素间距分别取 gap_xl 与 gap_md。

### `objects_page.tab` 固定页页签（pinned，无关闭按钮）

kind `control` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.tabstrip`；尺寸 `objects_min_width`、`tab_strip_height`
- 无障碍：可访问名「Objects」；Tab 可聚焦；固定在标签条首位
- 图标：`tab.objects`
- 同步：Linux done（test_19_workspace_tabs.py 验证固定 Objects 不可拖拽，支持键盘位次和溢出列表，显式 Browse 参与 MRU。test_15_refresh_regressions.py 验证普通重入保留列表和检索，F5 和已知失效才刷新。test_18_unbound_query.py 覆盖未绑定草稿和会话额度。）
- 实现备注：固定页独立持有已确认的浏览范围和当前请求范围。普通标签激活和树的被动选择不会改动浏览范围。AdwTabPage:pinned = true：固定在最左、紧凑（只显示图标）、没有关闭按钮，界面上不存在可点的 × —— 比「点关闭时拒绝」更贴近固定页语义。页签文本「Objects」，图标 view-grid-symbolic（与主工具条种类入口同一枚）。不占标签名额：8 个上限只数「有会话的标签」，Objects 不在其中，开满 8 个仍能切回本页。握手后的启动态与连接态复用 TabSet 和同一固定页。页签 id 固定 OBJECTS_TAB_ID，由 TabSet::new 在其它标签之前建一次（add_objects_page），工作区里不会出现第二个 Objects 页。重入保留有效列表、检索词与行选择。F5 和已知元数据失效才重读第一页。与当前代次匹配的成功首页响应确认范围。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 工作区建立 / 启动态落地 | 在标签条首位建出本页：标题「Objects」、图标 view-grid-symbolic、pinned（最左、紧凑、无关闭按钮、不占 8 个标签名额）；首屏是空态页，不是空白列表 | `src/tabs.rs::TabSet::add_objects_page` |
| click | 页签可见 | 切回固定页，恢复原浏览范围、已有分页列表和检索词，并登记一次真实激活。范围已待刷新且没有在途请求时重读其第一页。固定页不可关闭或拖动，在标签列表中始终可选。 | `src/tabs.rs::TabSet::enter_objects` |
| key Ctrl+W | 当前页是 Objects | 拒绝关闭（Propagation::Stop）并记一行「Objects 页固定存在，不可关闭」；页面不动、没有确认框 | `src/tabs.rs:2674、src/tabs.rs:2686` |
| click | 中键点页签，或任何程序化 close_page 请求（当前页是 Objects） | 与 Ctrl+W 同一条 on_close_page：拒绝（Propagation::Stop）并记同一句日志；页面不动、不弹确认框——关闭请求的来源不影响结论 | `src/tabs.rs:1393、src/tabs.rs:2684` |

### `objects_page.filter` Objects 检索框

kind `control` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.content`；尺寸 `objects_filter_width`、`objects_filter_min_width`、`objects_filter_max_width`、`objects_filter_height`、`radius_sm`、`icon_box`
- 无障碍：可访问名「筛选对象…」；Tab 页首可聚焦，随后可进入列表
- 原生控件：gtk:GtkSearchEntry
- 同步：Linux done（test_15_refresh_regressions.py::test_objects_filter_keeps_selection_and_scope_local：输入、改词、程序化选中变化、清词的业务 RPC 增量均为零；方向键进入列表并保持当前页，Ctrl+F、Ctrl+Shift+F 分别聚焦导航和检索。test_objects_filter_is_not_restored_at_startup：同一配置重启检索词为空。test_10_layout.py::test_focus_and_navigation：最小窗口检索可用且 Ctrl+F 仍聚焦导航。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | Objects 固定页活动 | 页首右侧显示检索框，列表独立滚动。非空时提供原生清除入口。 | `src/tabs.rs::ObjectsPage::new` |
| key 文本输入 | 输入、改词或删除关键词 | 按对象名称做不区分大小写的普通子串匹配。当前范围的所有已加载页应用同一条件，继承原序、保留源行和分页 token。过滤和由此产生的选中变化发出零业务 RPC，导航选择和其他工作区绑定保持原样。 | `src/tabs.rs::ObjectsPage::apply_filter` |
| click | 检索词非空 | 原生清除入口恢复全部已加载行与原顺序，业务 RPC 增量为零。 | `src/tabs.rs::ObjectsPage::apply_filter` |
| key Ctrl+Shift+F | Objects 固定页活动 | 聚焦检索框。Ctrl+F 继续聚焦导航搜索。 | `src/ui.rs（win.focus-objects-filter）` |
| key Esc / ↓ | 检索框有焦点 | Esc 清词；有匹配行时按 ↓ 进入列表。输入框中的 Enter 不打开对象。 | `src/tabs.rs::ObjectsPage::new` |
| load | 重入、重复 Browse、刷新、分页或布局改变 | 同一连接、库、Schema 和分类保留关键词；作用域变更、失效并清空或重启时清词。纯显示名称变化保留关键词。 | `src/tabs.rs::ObjectsPage::show、TabSet::browse_objects` |

### `objects_page.table` 三列表格（行号 · 表名 · 类型）

kind `container` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`row_number_width`、`column_width`、`column_header_min_height`、`frame_border_width`、`radius_md`、`font_ui`、`font_weight_title`；颜色 `sqarvo_frame`、`sqarvo_separator`
- 无障碍：可访问名「对象列表」；Tab 可聚焦；Tab 进入后 ↑ / ↓ 移动选中
- 原生控件：gtk:GtkColumnView
- 自绘：WinUI 3 没有等价的列式表格控件：GridView 不支持「虚拟化 + 可拖列宽 + 行号列」三者组合，ItemsControl 的虚拟化不带列宽状态；这里按 Linux 同口径自绘虚拟化三列表格（§7.1）。
- 同步：Linux done（test_15_refresh_regressions.py：同范围重入和只选连接保持列表；F5、应用内 DDL、部分失败脚本、事务提交和回滚各触发一次适用的首页请求。test_16_workspace_context.py：A 的 DDL 不刷新 B 的列表，范围失效后不改投其它库。）
- 实现备注：GtkColumnView + GtkSingleSelection：autoselect(false)、can_unselect(true) —— 进页不自动选中首行，选中是用户的动作、也是信息面板的输入。列序固定「行号 → 表名 → 类型」，set_reorderable(false)：拖不乱；表头文本就是这三个词，单行。行号列 fixed_width 取 row_number_width 且 resizable(false)（不可拖）；表名列 expand + 可拖，初值 column_width；「类型」列 fixed_width 120 是代码常量 OBJECTS_KIND_COLUMN_WIDTH，未进 metrics.toml（P1 补齐）。行高 row_height_data，行 / 列分隔线由 GTK 画（颜色同源 @sqarvo_separator）；外框挂在 ScrolledWindow 上（.sqarvo-table-frame：frame_border_width + radius_md，overflow hidden 裁首末行底色），行与单元格不加整线。横竖滚动条常驻可见（overlay_scrolling(false)）：把「表名」拖宽后仍能滚回行号列。换密度档时只重设行号 / 表名两列宽度（reapply_tier），不重建列表、不重发请求。整个表格与数据网格逐像素同口径：行高、表头高度、悬停 / 选中底色、外框都是同一组选择器（theme.stylesheet 里 .sqarvo-grid 与 .sqarvo-objects 并列）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key ↑ / ↓ | 表格有焦点且已有行 | 移动选中（SingleSelection 默认行为）；选中变化与单击同效 —— 同步右侧信息面板 | `src/tabs.rs:680、src/tabs.rs:917` |
| key Enter | 有选中行 | 等同于双击：打开该表的数据标签（同一个 activate 信号，同一张表复用已有标签） | `src/tabs.rs:938、src/tabs.rs:1779` |
| load | 单击库、Schema 或表分类正文，或主动点击 Tables 入口 | 列出该库的表（filter.kinds=[TABLE]）；页面自己请求，树没展开过也能看到完整列表 | `src/ui.rs:1346、src/ui.rs:1357、src/ui.rs:2093` |
| load | 单击 Views 分组正文，或主动点击 Views 入口 | 列出该库的视图（[VIEW, MATERIALIZED_VIEW]） | `src/ui.rs:1354、src/ui.rs:1360、src/ui.rs:1366` |
| load | 只选择连接、具体对象、辅助行或显式清除选择 | 维持已有的 Objects 浏览范围。单纯选择、程序化定位或重建不派发列表请求 | `src/ui.rs:1377、src/tabs.rs:1016` |
| load | 重复主动浏览已加载的同一个库或 Schema 及分类 | 有效范围沿用已加载行、检索词与选中项。在途的相同请求不会叠加。 | `src/tabs.rs:1035、src/tabs.rs:1047、src/tabs.rs:1050` |
| load | 从别的标签切回本页 | 保留有效列表与检索。非结果集 SQL（含提交、回滚）或已执行部分语句的失败脚本可能使同连接的元数据失效。重新进入时请求该范围的首页，在途时复用，成功后清除待刷新标记。右侧信息面板仍按现有工作区和导航契约更新。 | `src/tabs.rs::TabSet::enter_objects、TabSet::route_query` |
| key F5 | 当前页是 Objects | 强制重查当前范围第一页并保留检索词。成功后替换数据；旧分页响应按请求代次丢弃。对象树与 Inspector 沿用 F5 原有刷新路径。 | `src/ui.rs::Workspace::refresh、src/tabs.rs::ObjectsPage::reload` |

### `objects_page.table.row` 行模板（行号 · 图标 + 名称 · 类型）

kind `row` · status `active` · parent `objects_page.table`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`gap_sm`、`gap_md`、`icon_box`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_mid`、`sqarvo_brand_on_mid`；状态 hover=`sqarvo_brand_soft`、selected=`sqarvo_brand_mid`
- 无障碍：可访问名「（行内容：行号 + 对象名 + 种类）」
- 同步：Linux done（test_15_refresh_regressions.py：主动选择触发正确 Inspector DDL；过滤导致选中位移或消失的请求增量为零；双击打开匹配资源。）
- 实现备注：hover 实际是 alpha(@sqarvo_brand_soft, 0.55)；选中 @sqarvo_brand_mid + 前景 @sqarvo_brand_on_mid，与对象树行、数据网格行同一条规则（三个选择器并列，选中在前、hover 在后靠顺序让中档赢）。行只由可见行创建控件（GtkColumnView 虚拟化），几百行也不卡主线程；模型里只放数据。单元格上下内距归零，行高才落在 row_height_data（否则「图标 + 名称」的格子会被 Adwaita 撑到 35）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 行存在 | 用户选中可沿用 Inspector 预览请求；导航选择和当前范围保持。投影变动后的行位置只用于定位可见行，预览使用行的完整资源引用。 | `src/tabs.rs:917、src/tabs.rs:1011` |
| double_click | 行存在 | 使用命中可见行的完整资源身份打开或复用数据标签（GtkColumnView::activate → open_table）。 | `src/tabs.rs:938、src/tabs.rs:1824` |
| hover | 行存在 | 底色换成品牌浅档（alpha 55%），前景不动；已选中的行 hover 仍是中档，不会退回系统强调色 | `src/theme.rs:314` |

### `objects_page.table.row.menu` 对象行菜单

kind `container` · status `active` · parent `objects_page.table.row`

- 无障碍：可访问名「（对象行的操作菜单）」
- 原生控件：gtk:GtkPopoverMenu
- 同步：Linux done（tests/ui/linux/test_15_refresh_regressions.py::test_objects_filter_keeps_selection_and_scope_local：鼠标右键定位已过滤行，打开菜单后改词仍使用此前捕获的资源。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| right_click | 可见对象行 | 捕获点击行的对象引用和作用域请求代次，打开对象菜单。 | `src/tabs.rs::ObjectsPage::open_row_menu` |

### `objects_page.table.row.menu.open` 打开

kind `control` · status `active` · parent `objects_page.table.row.menu`

- 无障碍：可访问名「打开」
- 同步：Linux done（tests/ui/linux/test_16_workspace_context.py::test_navigation_target_and_clear_selection：过滤后右键打开 A 的同名资源、双击打开 B 的同名资源。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 对象菜单打开 | 沿用数据标签打开命令，使用菜单打开时捕获的对象身份。浏览范围或请求代次失效后旧菜单动作失效。 | `src/tabs.rs::ObjectsPage::open_row_menu、TabSet::open_table` |

### `objects_page.table.row.number` 行号格

kind `cell` · status `active` · parent `objects_page.table.row`

- 样式：尺寸 `row_number_width`、`font_status`、`font_weight_ui`
- 无障碍：可访问名「（行内容：第 N 行）」
- 同步：Linux done（§3 行内容（行号列）；number_factory 的 bind 与 tooltip）
- 实现备注：行号 = 模型位置 + 1（不是主键）：右对齐、11px monospace、色弱一档（.sqarvo-row-number）；tooltip「第 N 行」。回收的行控件可能还挂着旧行：item() 为空时清空文本，不显示上一行的行号。

### `objects_page.table.row.name` 表名格（图标 + 名称）

kind `cell` · status `active` · parent `objects_page.table.row`

- 样式：尺寸 `column_width`、`icon_box`、`gap_md`、`gap_sm`、`font_data_cell`、`font_weight_cell`
- 无障碍：可访问名「（行内容：图标 + 对象名）」
- 同步：Linux done（§3 行内容（表名列：图标 + 名称 + tooltip）；name_factory 的 bind）
- 实现备注：GtkBox（图标 + Label）：图标与对象树共用 kind_icon 映射（表 view-grid、视图 view-reveal），尺寸取 icon_box；名称 xalign 0、hexpand、过长省略号。单元格内距与数据网格一致（左右各 gap_sm，图标与名称间距 gap_md）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| hover | 行存在 | 悬停显示 tooltip「<名称>（<种类>）· 双击打开」（tooltip 挂在整格上，不是只挂在文字上） | `src/tabs.rs:759` |

### `objects_page.table.row.kind` 类型格

kind `cell` · status `active` · parent `objects_page.table.row`

- 样式：尺寸 `font_data_cell`、`font_weight_cell`、`gap_sm`、`gap_md`
- 无障碍：可访问名「（行内容：表 / 视图 / 物化视图）」
- 同步：Linux done（§3 行内容（类型列）；kind_factory 的 setup / bind）
- 实现备注：文案来自 TableEntry::kind_label，只有「表」「视图」「物化视图」三种；dim-label 次要信息色弱一档，过长省略号。「物化视图」四个汉字要放得下 —— 这也是该列固定宽度的来由（120，未进 metrics.toml）。

### `objects_page.status` 状态行

kind `control` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.content`；尺寸 `font_status`、`gap_sm`
- 无障碍：可访问名「对象列表状态」
- 同步：Linux done（test_15_refresh_regressions.py::test_objects_filter_manual_pagination_and_retry：首批 0 / 500、锁定失败后保留 500 行、重试后 1 / 514。test_sql_editor_ddl_refreshes_objects_on_reentry：待刷新首页失败后检索零请求；重新进入时重试并保留关键词。）
- 实现备注：分页下方显示已加载计数与列表状态。未匹配提示只说明已加载集合，分页在途或失败时仍保留有效计数。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 作用域已变、请求在飞 | 写「正在读取…」（首次首页与「加载更多…」续页都写这一句） | `src/tabs.rs:1070、src/tabs.rs:1083` |
| load | 首页或续页返回、有行 | 显示「显示 X / Y 个已加载对象」；X 是匹配数，Y 是已加载数。有分页 token 时显示「（还有更多）」。关键词非空且 X 为零时显示「已加载的对象中没有匹配项」。 | `src/tabs.rs:1131` |
| load | 请求成功但 0 条（本页与累计都是空） | 累计无行且没有更多页时显示「（当前范围没有这类对象）」。 | `src/tabs.rs:1135、src/tabs.rs:1136` |
| load | 请求失败 | 显示「读取失败：<headline>（<code>）」和「重试」。保留已有行。分页失败使用原 token 重试；已加载列表的首页刷新失败后保留待刷新状态，可点击「重试」，也可离开后重新进入。检索输入仍只投影已有行。 | `src/tabs.rs::ObjectsPage::finish_page` |
| load | 该连接没有浏览会话（展开连接前 / 已断开） | 写「这条连接没有浏览会话：展开连接后重试」，不发请求 | `src/tabs.rs:1065、src/tabs.rs:1067` |

### `objects_page.more` 「加载更多…」与「重试」

kind `control` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.content`；尺寸 `min_target`、`font_ui`、`gap_sm`
- 无障碍：可访问名「加载更多」
- 同步：Linux done（test_15_refresh_regressions.py::test_objects_filter_manual_pagination_and_retry：无匹配仍可手动分页、失败保留数据和重试、重试只产生一个分页 RPC。隔离 GTK objects_pagination_preserves_retry_and_discards_stale_responses 验证失败后 token 保留且范围和会话的旧响应均丢弃。）
- 实现备注：默认隐藏。有下一页 token 时显示「加载更多…」，请求失败时显示「重试」。分页是「接着取」而不是翻页：新行接在现有行后面，不跳页、不清已加载行、不改变滚动位置以外的状态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 按钮可见（还有下一页） | 按当前范围和分页 token 读取下一页或重试失败页。续页成功后应用当前过滤并追加。在途同一 token 只提交一次。即使没有匹配，仍可使用适用的分页或重试入口。 | `src/tabs.rs::ObjectsPage::load` |
| load | 首页或续页返回 | 成功时按是否还有下一页显示或隐藏；失败时保留重试入口。后到的旧范围或旧代次响应不更新当前列表。 | `src/tabs.rs::ObjectsPage::finish_page` |

### `objects_page.empty` 空态页

kind `control` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.content`；尺寸 `font_title`、`font_panel`
- 无障碍：可访问名「对象列表空态」
- 同步：Linux done（test_01_startup.py：握手后的空态工作区。test_16_workspace_context.py::test_navigation_target_and_clear_selection：浏览会话断开后清词并回到空态，不读取其它连接。）
- 实现备注：adw::StatusPage，与「列表 + 状态行」在同一个 GtkStack 里互斥：要么列表，要么空态页，不存在半截。启动态与连接态共用同一个空态控件。断开当前浏览范围后恢复对应的引导文案。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 没有已浏览范围，或其浏览会话已断开 | 标题「没有选中对象」，引导选择一个库或分类。仅选中连接或辅助行会保留已有效浏览范围；范围失效时清空关键词和旧数据。 | `src/tabs.rs::ObjectsPage::show_empty、TabSet::clear_objects_for_session` |
| load | 启动态（未连接） | 标题「没有选中对象」，说明「从左侧选择一个连接开始：双击一条已保存的连接即可连接。」 | `src/tabs.rs::TabSet::set_initial_objects_hint、src/ui.rs::show_startup_workspace` |
| load | 当前范围已失效，页面回到空态 | 恢复默认文案，并清空状态行与「加载更多…」。 | `src/tabs.rs::ObjectsPage::show_empty` |

## 窗口壳与主工具条（`shell`）

窗口本身（最小尺寸、几何恢复、关闭守卫）、标题与副标题、主菜单、主工具条（连接、新建查询、分类、目标和布局控制）、深色模式三档、界面密度三档，以及握手期的 Loading、握手失败错误页与后端失效后的重启入口。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `shell` | 窗口外壳 | area | active | done |
| `shell.window` | 　主窗口（最小尺寸 · 几何恢复 · 关闭守卫入口） | container | active | done |
| `shell.header` | 　标题栏（窗口外壳） | container | active | done |
| `shell.title` | 　标题与副标题 | control | active | done |
| `shell.menu` | 　主菜单 | control | active | done |
| `shell.menu.new_connection` | 　　文件 → 新建连接 | item | active | done |
| `shell.menu.new_tab` | 　　文件 → 新建查询标签 | item | active | done |
| `shell.menu.clear_selection` | 　　视图 → 清除选择 | item | active | done |
| `shell.menu.quit` | 　　文件 → 退出 | item | active | done |
| `shell.menu.export_csv` | 　　表 → 导出 CSV | item | removed | 已清 |
| `shell.menu.about` | 　　帮助 → 关于 Squarvo | item | active | done |
| `shell.toolbar` | 　主工具条 | container | active | done |
| `shell.toolbar.connect` | 　　「连接」 | control | active | done |
| `shell.toolbar.new_query` | 　　「新建查询」 | control | active | done |
| `shell.toolbar.tables` | 　　种类入口「Tables」 | control | active | done |
| `shell.toolbar.views` | 　　种类入口「Views」 | control | active | done |
| `shell.toolbar.functions` | 　　种类入口「Functions」 | control | removed | 已清 |
| `shell.toolbar.queries` | 　　种类入口「Queries」 | control | removed | 已清 |
| `shell.toolbar.target` | 　　操作目标 | control | active | done |
| `shell.toolbar.more` | 　　更多分类 | control | active | done |
| `shell.theme` | 　深色模式（跟随系统 / 浅色 / 深色） | container | active | done |
| `shell.theme.system` | 　　「跟随系统」单选项 | item | active | done |
| `shell.theme.light` | 　　「浅色」单选项 | item | active | done |
| `shell.theme.dark` | 　　「深色」单选项 | item | active | done |
| `shell.density` | 　界面密度档（紧凑 / 舒适 / 大） | container | active | done |
| `shell.density.compact` | 　　「紧凑（100%）」单选项 | item | active | done |
| `shell.density.comfortable` | 　　「舒适（125%）」单选项 | item | active | done |
| `shell.density.large` | 　　「大（150%）」单选项 | item | active | done |
| `shell.error` | 　错误页（握手失败 / 无工作区断开） | container | active | done |
| `shell.error.retry` | 　　「重试」/「重新连接」 | control | active | done |
| `shell.backend_failure` | 　后端失效恢复（「重启 Sqarvo」对话框与重启流程） | container | active | done |
| `shell.menu.mcp` | 　　MCP | item | active | done |
| `shell.layout` | 　窗口三栏布局 | container | active | done |
| `shell.toolbar.layout` | 　　导航、专注模式、信息面板 | control | active | done |
| `shell.menu.reset_layout` | 　　视图 → 重置布局 | control | active | done |

### `shell` 窗口外壳

kind `area` · status `active`

- 样式：尺寸 `header_height`、`toolbar_height`、`status_height`、`min_window_width`、`min_window_height`
- 同步：Linux done（test_01_startup.py 启动及主菜单，test_10_layout.py 两种外壳布局入口。 tests/ui/linux/results/run.9ME2GefI/report.json）
- 实现备注：自外向内 = 标题栏（区域 01）+ 主工具条（区域 02）+ 页面槽位；工具条挂在标题栏之下、不随工作区重建。Linux 没有独立菜单行：菜单收进标题栏右侧的主菜单按钮（workspace-model 附录 A 第 2 条）；Windows 同样没有独立菜单行——菜单收进自绘标题行右侧的汉堡按钮（v0.2.5 起）。页面槽位轮流承载 Loading / 启动态外壳 / 工作区 / 错误页。两种工作区外壳共用 shell.layout。

### `shell.window` 主窗口（最小尺寸 · 几何恢复 · 关闭守卫入口）

kind `container` · status `active` · parent `shell`

- 样式：尺寸 `min_window_width`、`min_window_height`
- 无障碍：可访问名「Sqarvo 主窗口」
- 同步：Linux done（tests/ui/linux/results/run.DmA8Fxr9/report.json：关窗、退出和语言重启的草稿保护。tests/ui/linux/results/run.ID18uRL0/report.json：关闭取消同时保留 SQL、待提交结果及会话。）
- 实现备注：最小尺寸由 window_size::relayout 算出后交给 set_size_request，GTK 转成 toplevel 的最小尺寸（Wayland 走 xdg_toplevel.set_min_size，X11 走 WM normal hints）。位置不恢复是平台事实：GTK4 没有读写 toplevel 位置的 API，window_x / window_y 照常写入与校验但不拿来挪窗口（blocked 的替代口径见 README「同步」一节）。关闭是一次性的：确认过一次（`close_confirmed`）后再关不二次弹窗，已进入关闭流程时，重复请求继续等待同一次 worker 清理完成。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 窗口创建 / 每次重排 | 最小窗口按显示器逻辑工作区收敛：min(该档最小宽, 工作区宽-32) × min(该档最小高, 工作区高-48)（该档基准取值来自 metrics.toml）；`--window-size` 给更小的值时被抬到当次下限并记 `[monitor] 目标窗口 … 低于最小窗口 …` | `src/window_size.rs:205、src/window_size.rs:136、src/ui.rs:171` |
| load | window.json 里有记忆的尺寸 / 最大化 | 尺寸与最大化恢复、**位置不恢复**；记忆矩形不落在任何当前显示器几何内（±32 容差）时丢弃几何、改用分档推荐并记 `[monitor] 几何越界已丢弃：…` | `src/window_size.rs:365、src/window_state.rs:209` |
| load | window.json 记的几何属于旧档（记忆尺寸的档位 ≠ 当前档） | 按当前档重算窗口并记 `[monitor] 档位导致窗口放大：1200x800 → 1888x1000（记忆尺寸属于 compact，当前 large）`；记忆矩形越界的校验规则不变（±32 容差内才恢复，否则丢弃改推荐尺寸） | `src/window_size.rs:365、src/window_size.rs:429` |
| click | 点标题栏的关闭按钮 | 窗口关闭、应用退出和语言切换重启共用关闭守卫。逐表列出脏行，逐标签列出未保存 SQL 及活动事务、待提交结果或执行中的查询。选「取消」保留工作区和编辑；选「放弃更改并退出」才继续。等 worker 回报 `ShutdownComplete` 后销毁窗口 | `src/ui.rs:414、src/ui.rs:985、src/ui.rs:972` |
| key Ctrl+Shift+W | 窗口存在 | 与标题栏关闭按钮同一条 `window.close()` → 同一个 close_request（脏行守卫照旧） | `src/ui.rs:204、src/ui.rs:414` |

### `shell.header` 标题栏（窗口外壳）

kind `container` · status `active` · parent `shell`

- 样式：槽位 `header`；尺寸 `header_height`、`font_ui`、`gap_md`；颜色 `sqarvo_separator`
- 原生控件：gtk:AdwHeaderBar
- 自绘：WinUI 3 的系统标题栏不能承载自绘内容：标题行用 ExtendsContentIntoTitleBar + SetTitleBar(拖拽区) 把自绘行注册为拖拽区（图标 / 标题 / 副标题 / 汉堡按钮），窗口控制按钮仍用系统按钮；没有等价的原生「可拖拽自绘标题行」控件（§1.1）。
- 同步：Linux done（tests/ui/linux/results/run.cmSUm8oW/report.json：真实拖动、双击最大化、标题栏右键菜单、最小化、还原和关闭。）
- 实现备注：Linux 使用 AdwHeaderBar 居中承载 AdwWindowTitle，主菜单位于结束侧。原生装饰负责拖动、双击、窗口菜单和窗口控制按钮。

### `shell.title` 标题与副标题

kind `control` · status `active` · parent `shell`

- 样式：尺寸 `font_title`、`font_status`、`font_weight_title`、`font_weight_ui`、`gap_xxs`
- 无障碍：可访问名「窗口标题」
- 原生控件：gtk:AdwWindowTitle
- 同步：Linux done（tests/ui/linux/results/run.cmSUm8oW/report.json：SQLite 实际库、长名称无障碍描述、原生 Tooltip 查询及零 RPC；tests/ui/linux/results/run.TJeqNMo1/report.json：双连接归属；隔离 GTK qa_query_target_tracks_the_original_tab_session：失败重绑保留原绑定。 tests/ui/linux/results/run.kEQx6EUw/report.json：原标题、安全描述与无导航失连；system_title_appends_the_brand_once 校验品牌后缀。）
- 实现备注：标题读取活动工作区的原始名称和实际连接显示名，副标题取友好数据库类型和实际数据库名，缺少的字段省略。Objects 分类使用实际分类名称，数据库级使用 Objects。无有效范围或无活动工作区时只显示 Sqarvo；未绑定 SQL 只显示查询名。全名、实际 Schema、安全端点、已有版本和状态按可用性补入原生 tooltip 与可访问描述。断开的存续工作区保留实际身份；在途及失败重绑不改标题。启动、重试与失连的状态继续在业务区域显示。没有业务工作区时，标题为 Sqarvo，启动状态放入 tooltip。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 活动工作区或该工作区实际绑定变化 | 更新居中主副标题。系统标题保留单个 Sqarvo 品牌后缀，空工作区使用单独的 Sqarvo。长名截断，完整名称经原生 tooltip 和无障碍读取。 | `src/tabs.rs TabSet::window_title_and_subtitle；src/ui.rs TabContext::on_title` |

### `shell.menu` 主菜单

kind `control` · status `active` · parent `shell`

- 样式：槽位 `menubar`；尺寸 `min_target`、`icon_box`、`font_weight_menu`
- 无障碍：可访问名「主菜单（文件 / 视图 / MCP / 帮助）」；Tab 可聚焦；Enter / Space 展开菜单，Esc 收起
- 图标：`menu.open`
- 原生控件：gtk:MenuButton(GMenu → PopoverMenu)
- 同步：Linux done（tests/ui/linux/results/run.x16Mog9W/report.json：草稿创建、编辑、关闭与退出取消、语言重启取消、服务恢复。）
- 实现备注：图标是 `open-menu-symbolic` 的扁平按钮，tooltip 原文是「主菜单（文件 / 视图 / MCP / 帮助）」。「查询」子菜单整项不挂（不留空菜单）；「表」也整项不挂——唯一条目「导出 CSV」暂时隐藏（改动确认单 #1 / #198，见 shell.menu.export_csv）。「关闭标签」与表的事务 / 刷新 / 行列操作只保留 `win.*` 动作与快捷键，菜单里不重复一份。「视图」下是三个三级小节（深色模式 / 界面密度 / 语言），都是真子菜单：条目绑 stateful `win.*`，当前值打勾、不可用项由该条动作 `enabled=false` 置灰；不再有自绘占位小节。「语言」小节（#199，ADR 0031 决策 6）：三个 stateful 单选项（跟随系统 / 简体中文 / English，动作 `win.lang-system/lang-zh/lang-en`），当前偏好打勾；与主题不同，**切换重启生效**——点选会改生效值时弹「切换语言需要重启」确认框（「重启」= 先走窗口关闭守卫；获准退出后才保存语言偏好并起新进程；「稍后」只回滚勾选、不落盘）；生效值不变的点选（如中文系统上从「跟随系统」改点「简体中文」）直接更新勾选并写 `window.json`，不打扰用户。外壳区条目文字与 tooltip 自 #199 起经 `l10n/shell.toml` 生成 catalog（`l10n::t`）取值：zh 与登记的中文原文逐字节相同，英文界面显示对应译文；「视图 → 语言」的条目文字在英文界面下仍按自译名显示（简体中文 / English）。弹层条目文字的规则只有一条：`popover label, menuitem label { font-weight: font_weight_menu }`。400 时「色」的细撇在 1x 屏只剩 1px 宽（暗底上读成缺一笔），500 让它变实；字号不动 —— 弹层按系统 11pt 渲染，不跟窗口的 12px（改动确认单 #2）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 窗口存在 | 展开主菜单：文件（新建连接 / 新建查询标签 / 退出）、视图（清除选择 / 深色模式 ▸ 跟随系统 / 浅色 / 深色、界面密度 ▸ 紧凑 / 舒适 / 大、语言 ▸ 跟随系统 / 简体中文 / English）、MCP（管理页）、帮助（关于 Squarvo）；弹出时刷新界面密度的勾选、条目文字（放不下时追加「，放不下」）与逐项可用性；语言的勾选反映当前偏好，点选改生效值时弹重启确认；在重启确认或关闭守卫中取消时保留当前语言及工作区 | `src/ui.rs:4945、src/ui.rs:321` |

### `shell.menu.new_connection` 文件 → 新建连接

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「新建连接」
- 同步：Linux done（§1.4 文件 → 新建连接；open_connection_dialog_for 的未就绪兜底与「只前置」分支）
- 实现备注：与工具条「连接」、`Ctrl+N` 同一条判定与同一个动作（`win.new-connection`）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手 | 打开连接对话框；对话框已打开时只把它前置（不叠第二个）。未就绪时该项禁用，处理器里还有一条兜底日志「后端尚未连接，忽略「新建连接」」 | `src/ui.rs:4751、src/ui.rs:1103` |
| key Ctrl+N | 后端已握手 | 与菜单项同一个动作 `win.new-connection` | `src/ui.rs:320` |

### `shell.menu.new_tab` 文件 → 新建查询标签

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「新建查询标签」
- 同步：Linux done（test_18_unbound_query.py：win.new-tab 共用入口及未绑定可用；tests/ui/linux/results/run.cmSUm8oW/report.json：真实 Ctrl+T 与加号共用目标确认。 tests/ui/linux/results/run.kEQx6EUw/report.json：win.new-tab 统一确认及失效保护。）
- 实现备注：与工具条新建查询、标签栏加号和 Ctrl+T 共用 win.new-tab。目标摘要截断时通过同一确认流程。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且工作区存在（`set_workspace_ready` 统一开关） | 按导航逻辑选择新建独立查询；只有无逻辑选择时使用活动工作区的实际绑定。只选连接时不借用工作区的库。两者均无目标时新建未绑定 SQL 草稿；目标不可用或确认期间变更时提示原因并拒绝执行。成功时聚焦编辑器；握手前禁用入口 | `src/ui.rs::request_toolbar_command、src/tabs.rs::TabSet::new_query_tab_for_target` |
| key Ctrl+T | 后端已握手且工作区存在 | 与菜单项同一个动作 `win.new-tab` | `src/ui.rs:327` |

### `shell.menu.clear_selection` 视图 → 清除选择

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「清除选择」
- 同步：Linux done（test_16_workspace_context.py::test_navigation_target_and_clear_selection：失效导航阻止查询，win.clear-selection 后新查询从活动 A 读到合成数据。GTK 菜单模型断言该条目及动作。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 主菜单的视图子菜单可用 | 清除树的逻辑与可见选择，保留活动标签和 Objects 范围。下一次全局命令可以使用活动工作区。 | `src/ui.rs::install_window_actions、src/tree.rs::TreePanel::clear_selection` |

### `shell.menu.quit` 文件 → 退出

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「退出」
- 同步：Linux done（tests/ui/linux/results/run.x16Mog9W/report.json：草稿创建、编辑、关闭与退出取消、语言重启取消、服务恢复。）
- 实现备注：菜单项走 `app.quit`，与 `Ctrl+Q`、D-Bus 同一个入口；有窗口时先关窗，因此 SQL 草稿、脏行和活动会话的关闭守卫照常生效。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 任意 | 有窗口时先 `window.close()`（存在需要保护的内容或会话操作时弹确认框），最后一个窗口关完后 GApplication 自然退出；没有窗口时才直接 `app.quit()`。两种情况 `connect_shutdown` 都会停后端并 join 子进程，不留孤儿进程 | `src/ui.rs:4596、src/main.rs:353` |
| key Ctrl+Q | 任意 | `app.quit` 加速度器；与菜单项、D-Bus 走同一条 `quit_app` | `src/main.rs:76` |

### `shell.menu.export_csv` 表 → 导出 CSV

kind `item` · status `removed` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「导出 CSV」
- 同步：Linux removed（build_main_menu 不再挂「表」子菜单（src/ui.rs:4945-4953）；菜单按钮 tooltip 已改「主菜单（文件 / 视图 / 帮助）」（src/ui.rs:208）；win.export-csv 与 Ctrl+Shift+E 保留（src/ui.rs:5328、377））
- 实现备注：与表工具条 `导出 ▾` 菜单的「导出 CSV…」、`Ctrl+Shift+E` 同一条导出入口；固定 CSV。INSERT 语句与转储表只在表工具条 / 结果区 / 树右键菜单里，菜单不重复。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且工作区存在；当前标签有没有可导出内容由动作自己判定 | 导出当前结果页 / 当前页为 CSV；当前标签没有可导出的结果页时只写日志「当前标签没有可导出的结果页」，不弹空对话框 | `src/ui.rs:5328（win.export-csv 动作；菜单条目已随改动确认单 #1 不挂）` |
| key Ctrl+Shift+E | 同上 | 直接导出 CSV（与菜单项同一个 `win.export-csv`） | `src/ui.rs:377` |

### `shell.menu.about` 帮助 → 关于 Squarvo

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「关于 Squarvo」
- 同步：Linux done（§1.4 帮助 → 关于 Squarvo；show_about 的「已打开只前置」与诊断行）
- 实现备注：不依赖后端：握手前也能打开（诊断字段会写「未连接」/ 空）。对话框本体（版本、协议、后端诊断字段的排版）归 dialogs 区；这里只登记入口。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 任意 | 打开「关于 Squarvo」对话框：版本、协议、后端诊断字段（不含密码、凭据引用与 token）；已打开时只前置，不叠第二个 | `src/ui.rs:4623、src/ui.rs:5289、src/ui.rs:4957` |

### `shell.toolbar` 主工具条

kind `container` · status `active` · parent `shell`

- 样式：槽位 `toolbar`；尺寸 `toolbar_height`、`toolbar_padding_h`、`toolbar_button_height`、`gap_xxs`、`gap_xs`、`gap_sm`、`gap_lg`、`gap_xl`、`min_target`、`font_ui`、`icon_box`；颜色 `sqarvo_separator`
- 无障碍：可访问名「主工具条」
- 自绘：GTK 提供独立按钮与原生弹出菜单。此工具条组合了这些控件，以同一行呈现动作、响应式分类、目标和右侧布局控制。
- 同步：Linux done（tests/ui/linux/results/run.cmSUm8oW/report.json：Compact 840×600 可达、原生 More、目标摘要与 零额外业务 RPC。 tests/ui/linux/results/run.kEQx6EUw/report.json：中英文三档可达，反复缩放无新增业务 RPC。）
- 实现备注：窗口级单行工具条，位于标题栏下。左侧为连接、新建查询和分隔后的对象分类；弹性空间内显示实际目标，右侧为导航、专注、信息面板三个布局控制。GTK 原生按钮与标签的自然宽度决定分类收纳和动作图标化；始终保留实际可用范围内的关键入口。分类能力取 Action Context 和已实现能力。高亮读取活动工作区类型或 Objects 实际分类；普通 SQL 无分类高亮。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 窗口空间或动作上下文变化 | 以实际文字与原生控件测量重排工具条。分类按固定顺序恢复；布局控制延用工作区外壳的实际布局状态。 | `src/shell.rs Toolbar::reflow；src/ui.rs schedule_toolbar_refresh` |

### `shell.toolbar.connect` 「连接」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`
- 无障碍：可访问名「连接」
- 图标：`tree.connection`
- 同步：Linux done（tests/ui/linux/results/run.TJeqNMo1/report.json：原生连接对话框打开和取消；test_18_unbound_query.py::test_reconnect_from_objects_only_startup：后端丢失时入口禁用。）
- 实现备注：左侧强调动作，使用图标和文字。空间不足时保留图标、可访问名称和 tooltip。后端握手前禁用并说明原因；就绪后打开既有连接对话框。入口含义与数据库在线状态无关。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手 | 打开连接对话框；对话框已打开时只把它前置，不叠第二个。禁用时不发动作，只在处理器里留一条日志兜底 | `src/shell.rs:721、src/ui.rs:1096、src/ui.rs:1103` |

### `shell.toolbar.new_query` 「新建查询」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`
- 无障碍：可访问名「新建查询」
- 图标：`action.new_query`
- 同步：Linux done（tests/ui/linux/results/run.TJeqNMo1/report.json：双连接、仅选连接、失效与清除选择；tests/ui/linux/results/run.cmSUm8oW/report.json：截断目标时加号与 Ctrl+T 共用确认。 tests/ui/linux/results/run.kEQx6EUw/report.json：加号和工具条目标文本一致，确认时目标变化拒绝创建；toolbar_target_rejects_lost_and_replaced_sessions 覆盖失效、重连和档案修订。）
- 实现备注：中性动作，使用图标和文字。空间不足时保留图标与可访问名称。后端握手后可用，无目标时可创建未绑定查询。tooltip 包含实际动作目标，与标签栏加号相同。工具条、加号、菜单与 Ctrl+T 共用 win.new-tab。仅连接不借用其他标签的数据库，失效目标不给回退。目标摘要被截断时先显示可读的目标确认。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且工作区存在 | 按导航逻辑选择新建独立查询；只有无逻辑选择时使用活动工作区的实际绑定。只选连接时不借用工作区的库。两者均无目标时新建未绑定 SQL 草稿；目标不可用或确认期间变更时提示原因并拒绝执行。成功时聚焦编辑器；握手前禁用入口 | `src/ui.rs::request_toolbar_command、src/tabs.rs::TabSet::new_query_tab_for_target` |

### `shell.toolbar.tables` 种类入口「Tables」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`、`gap_xxs`、`gap_lg`、`radius_sm`；颜色 `sqarvo_brand_mid`、`sqarvo_brand_on_mid`；状态 selected=`sqarvo_brand_mid`
- 无障碍：可访问名「Tables」
- 同步：Linux done（tests/ui/linux/results/run.TJeqNMo1/report.json：目标 B 浏览固定 Objects 且保持查询 A；隔离 GTK page_failure_keeps_loaded_rows_and_only_unsupported_hides_a_group：只有明确能力不支持才隐藏。）
- 实现备注：纯文字分类入口。只有 Action Context 支持且软件已有实现时出现；空列表、权限错误或读取失败保留分类。缺少可用数据库范围时禁用，tooltip 说明原因。活动工作区的 Tables 类别使用圆角高亮；当入口收纳时由 More 标记该分类。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手，Action Context 有可用的数据库或 Schema | 切到固定 Objects 页，按解析出的目标数据库或 Schema 列出 Tables，高亮来自 Objects 当前种类。树逻辑选择保留 | `src/ui.rs:1078、src/ui.rs:1909、src/shell.rs:958` |
| load | 工作区没有可用数据库或 Schema，或目标已失效 | 分类保留并禁用。tooltip 提供原因，当前工作区保持原样。 | `src/ui.rs::show_objects_for_kind` |

### `shell.toolbar.views` 种类入口「Views」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`、`gap_xxs`、`gap_lg`、`radius_sm`；颜色 `sqarvo_brand_mid`、`sqarvo_brand_on_mid`；状态 selected=`sqarvo_brand_mid`
- 无障碍：可访问名「Views」
- 同步：Linux done（tests/ui/linux/results/run.cmSUm8oW/report.json：More 中打开唯一的固定 Objects、空 Views、活动菜单项可读；tests/ui/linux/results/run.TJeqNMo1/report.json：目标 B 视图。）
- 实现备注：纯文字分类入口。只有 Action Context 支持且软件已有实现时出现；空列表、权限错误或读取失败保留分类。缺少可用数据库范围时禁用，tooltip 说明原因。活动工作区的 Views 类别使用圆角高亮；当入口收纳时由 More 标记该分类。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手，Action Context 有可用的数据库或 Schema | 切到固定 Objects 页，按解析出的目标数据库或 Schema 列出 Views，高亮来自 Objects 当前种类。树逻辑选择保留 | `src/ui.rs:1078、src/ui.rs:1904、src/ui.rs:1920` |
| load | 工作区没有可用数据库或 Schema，或目标已失效 | 分类保留并禁用。tooltip 提供原因，当前工作区保持原样。 | `src/ui.rs::show_objects_for_kind` |

### `shell.toolbar.functions` 种类入口「Functions」

kind `control` · status `removed` · parent `shell.toolbar`

- 同步：Linux removed（test_01_startup.py 与 test_15_refresh_regressions.py::test_objects_unimplemented_kind_hidden_after_refresh：启动与刷新后均隐藏 Functions。）

### `shell.toolbar.queries` 种类入口「Queries」

kind `control` · status `removed` · parent `shell.toolbar`

- 同步：Linux removed（test_01_startup.py 与 test_15_refresh_regressions.py::test_objects_unimplemented_kind_hidden_after_refresh：启动与刷新后均隐藏 Queries。）

### `shell.toolbar.target` 操作目标

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「目标」
- 原生控件：gtk:GtkLabel
- 同步：Linux done（tests/ui/linux/results/run.cmSUm8oW/report.json：隐藏导航后提示、同范围且可见时隐藏、截断先确认；tests/ui/linux/results/run.TJeqNMo1/report.json：两连接、仅连接、失效和清除。 tests/ui/linux/results/run.kEQx6EUw/report.json：确认期间清除选择，继续时提示原目标失效，查询数和 Connect 数不变。）
- 实现备注：导航目标与活动工作区的连接、库或 Schema 不一致，导航选择不可见，或明确目标失效时显示实际目标。连接级选择省略数据库值，tooltip 注明仅选连接。安全全名、附加标识和失效原因可由 tooltip 和可访问描述读取。省略同一可见操作目标的常驻提示。摘要无法完整显示时，打开业务命令前显示完整目标。继续时复核工作区、捕获目标、档案版本和会话身份。原目标失效或变化时提示并结束本次操作。普通操作直接执行。

### `shell.toolbar.more` 更多分类

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`icon_box`、`gap_xxs`、`radius_sm`；颜色 `sqarvo_brand_mid`、`sqarvo_brand_on_mid`
- 无障碍：可访问名「更多分类」
- 图标：`tree.more`
- 原生控件：gtk:GtkMenuButton + GtkPopoverMenu
- 同步：Linux done（tests/ui/linux/results/run.cmSUm8oW/report.json：溢出及恢复、项目唯一、活动项可读、旧菜单关闭、从菜单浏览固定 Objects；重排与展合零业务 RPC。 tests/ui/linux/results/run.kEQx6EUw/report.json：More 活动态、子项标记、旧目标菜单关闭及反复缩放；tests/ui/linux/results/run.WBo2vd51/runner.log：切到不支持当前分类的目标后清除高亮，空菜单隐藏。）
- 实现备注：以分类的自然宽度和当前可分配空间决定收纳，项目按工具条顺序排列且只出现一次。没有项目时隐藏。活动分类被收纳时同时标记 More 和对应的菜单项。空间恢复后分类回到原顺序。打开时捕获 Action Context 及相应档案版本与会话身份。目标改变、失效或后端离线时关闭旧菜单，触发时再核对捕获目标。

### `shell.theme` 深色模式（跟随系统 / 浅色 / 深色）

kind `container` · status `active` · parent `shell`

- 样式：槽位 `overlay`；尺寸 `font_ui`、`font_weight_menu`
- 无障碍：可访问名「深色模式」；Tab 菜单里三档为一组单选项，Tab 进入、↑ / ↓ 切换、Space 选择
- 同步：Linux done（§1.5 菜单入口；build_main_menu 的「深色模式」子菜单、install_window_actions 的 win.theme-* 与 apply_theme_choice；--qa-open-menu 截出 -menu-view-theme.png）
- 实现备注：位置：「视图 → 深色模式」的三级子菜单（与「界面密度」同级，排在它上面），三项各绑一个 stateful `win.theme-*` 动作，当前偏好打勾。生效方式是 `AdwStyleManager::set_color_scheme`：跟随系统 = `Default`，浅色 / 深色 = `ForceLight` / `ForceDark`（用户明确点了选择，不再被系统偏好改回去）。`dark` 通知重载样式表，四套 CSS 不各自监听主题。偏好写 `window.json` 的 `theme`（`system` / `light` / `dark`，与 Windows 同键同值）；启动优先级 `--theme`（QA）→ `window.json` → 跟随系统，并在 `theme::install()` 之前设定，避免先浅色再闪成深色。不做的事：不提供「浅色 / 深色 跟随系统但覆盖强调色」这类组合；系统强调色仍归平台（Windows 的强调色只用于系统绘制的部件）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 窗口存在 | 展开「视图 → 深色模式」：跟随系统（当前系统值打勾时无覆盖）/ 浅色 / 深色；点任一项即切换主题、写 `window.json` 并打勾，日志 `[theme] 主题偏好：浅色（light）` 与 `[theme] 主题偏好 light 已记入 <path>`；无后端依赖，握手前后都可用 | `src/ui.rs:5164、src/ui.rs:5315、src/theme.rs:239` |

### `shell.theme.system` 「跟随系统」单选项

kind `item` · status `active` · parent `shell.theme`

- 样式：尺寸 `font_ui`、`font_weight_menu`
- 无障碍：可访问名「跟随系统」
- 同步：Linux done（§1.5 三档之一；apply_theme_choice → theme::set_scheme）
- 实现备注：默认档：不写 `color-scheme`（`Default`），深浅色完全由系统 / libadwaita 决定；系统切换时 `dark` 通知重载样式表。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 窗口存在 | 把偏好设回跟随系统（`AdwStyleManager::set_color_scheme(Default)`），`window.json` 的 `theme` 写 `system`，本项打勾 | `src/ui.rs:5247、src/ui.rs:5188` |

### `shell.theme.light` 「浅色」单选项

kind `item` · status `active` · parent `shell.theme`

- 样式：尺寸 `font_ui`、`font_weight_menu`
- 无障碍：可访问名「浅色」
- 同步：Linux done（§1.5 三档之一；--theme light 与菜单同一落点）
- 实现备注：`ForceLight`：系统是深色时也强制浅色。样式表由 `dark` 通知重载，不重建控件。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 窗口存在 | 强制浅色（`ForceLight`），`window.json` 的 `theme` 写 `light`，本项打勾；日志同上 | `src/ui.rs:5188、src/theme.rs:246` |

### `shell.theme.dark` 「深色」单选项

kind `item` · status `active` · parent `shell.theme`

- 样式：尺寸 `font_ui`、`font_weight_menu`
- 无障碍：可访问名「深色」
- 同步：Linux done（§1.5 三档之一；--theme dark 与菜单同一落点）
- 实现备注：`ForceDark`：系统是浅色时也强制深色。深色色板是浅色同一组色相的提亮版（`theme.rs` 的 `DARK_PALETTE`），不是第二套色相。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 窗口存在 | 强制深色（`ForceDark`），`window.json` 的 `theme` 写 `dark`，本项打勾；日志同上 | `src/ui.rs:5188、src/theme.rs:246` |

### `shell.density` 界面密度档（紧凑 / 舒适 / 大）

kind `container` · status `active` · parent `shell`

- 样式：槽位 `overlay`；尺寸 `font_ui`、`font_weight_menu`、`font_status`、`gap_xs`
- 无障碍：可访问名「界面密度」；Tab 菜单里三档为一组单选项，Tab 进入、↑ / ↓ 切换、Space 选择
- 同步：Linux done（§1.5 全部行为；density::switch 的六步、ensure_startup_tier 的守卫、fill_density_section 的「放不下」文字）
- 实现备注：位置：「视图 → 界面密度」的三级子菜单（紧接「深色模式」），三个 stateful `win.tier-*` 动作，当前档打勾。GMenu 没有逐项 tooltip：某档在当前工作区放不下时，条目文字追加「，放不下」，完整原因（需要 W×H、当前工作区多大）进日志 `[tier] … 不可用`；该档动作同时 `enabled=false`。换文字只能重填这一节 —— GMenu 在 append 时就把条目属性复制进模型，之后再改 MenuItem 不生效。三档的数字唯一真值是 metrics.toml（每个尺寸 / 字号 / 行高 / 图标盒都直接取当前档的写死整数，不做「基准 × 倍率」）。不做的事：不用 `Ctrl+滚轮`（滚轮在数据网格与 SQL 编辑器里已有语义）；不提供小于 100% 的档位；切档不重建标签（未提交编辑与滚动位置保留）；系统 DPI 与系统字号不乘进档位。切档后「表格区优先保留 600」是代码常量，未进 metrics。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key Ctrl+Plus / Ctrl+KP_Add / Ctrl+= | 窗口存在 | 升一档（`win.tier-up`）：目标档可用就切；已经在「大（150%）」时什么都不做并记「[tier] 已经是最高档（large），Ctrl+Plus 忽略」；目标档在当前显示器放不下时不切换、不写盘并记「[tier] … 不可用（需要 W×H），未切换」 | `src/ui.rs:361、src/ui.rs:4978` |
| key Ctrl+minus / Ctrl+KP_Subtract | 窗口存在 | 降一档（`win.tier-down`）；已在「紧凑（100%）」时记「[tier] 已经是最低档（compact），Ctrl+minus 忽略」 | `src/ui.rs:365、src/ui.rs:4987` |
| key Ctrl+0 | 窗口存在 | 直接切到「紧凑（100%）」（`win.tier-reset`，不是「回退一个可用档」）；已经是紧凑时该项禁用 | `src/ui.rs:366、src/ui.rs:4996、src/ui.rs:5091` |
| load | 启动 | 启动档位优先级：`--tier`（QA，取证用）→ `window.json` 的 `tier` → 紧凑（100%）；日志 `[tier] 启动档位 …（来源=默认/记忆/QA）`。已保存的档位当前放不下时**回退到下一个可用档**并记 `[tier] … 在当前工作区 … 不可用（需要 W×H），回退 …`，不静默钳到紧凑；连紧凑都放不下时保留紧凑、窗口收敛到工作区 | `src/ui.rs:135、src/density.rs:33、src/window_size.rs:237` |
| load | 切档成功 | 同一次动作里：① 四套样式表（主壳 / 数据页 / 对象树 / DDL）用同一个 provider 重载；② 窗口按新档重算并钳到工作区（日志「[monitor] 档位导致窗口放大：…」）；③ 侧栏 / 信息面板按 **compact 归一值 × 新档**换算（往返不漂移），新窗口放不下表格区下限时收到各自下限并记日志；④ 已建列的固定宽度、行高、字号与图标按钮命中区按新档重设；⑤ 档位写入 `window.json` | `src/density.rs:78、src/shell.rs:591、src/theme.rs:241` |

### `shell.density.compact` 「紧凑（100%）」单选项

kind `item` · status `active` · parent `shell.density`

- 样式：尺寸 `font_ui`、`font_weight_menu`
- 无障碍：可访问名「紧凑（100%）」
- 同步：Linux done（§1.5 菜单入口；build_main_menu 的密度子菜单与 apply_tier_choice）
- 实现备注：当前档打勾：勾选来自 stateful action 的 state，点击后由 `refresh_tier_actions` 写回。是 `Ctrl+0` 的目标档，也是 `Ctrl+minus` 的下界。条目文字就是 `tier.label()`；放不下时由 `fill_density_section` 改成「紧凑（100%），放不下」并禁用。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该档在当前显示器可用 | 切到紧凑档（走 shell.density 记的同一套切档流程）；已经是当前档时不重复重排并记「[tier] 已经是 compact 档（100%），不重复重排」 | `src/ui.rs:5183、src/ui.rs:5303、src/density.rs:104` |

### `shell.density.comfortable` 「舒适（125%）」单选项

kind `item` · status `active` · parent `shell.density`

- 样式：尺寸 `font_ui`、`font_weight_menu`
- 无障碍：可访问名「舒适（125%）」
- 同步：Linux done（§1.5 菜单入口、逐项禁用与「放不下」条目文字；refresh_tier_actions 与 fill_density_section）
- 实现备注：与其它两档同一组单选；本档放不下时动作 `enabled=false`（菜单里灰掉）、条目文字追加「，放不下」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该档在当前显示器可用 | 切到「舒适（125%）」（窗口按档位一起放大 / 收小）；目标档在当前显示器放不下时不切换、不写盘（该项此时也已禁用，条目文字为「舒适（125%），放不下」），完整原因记「[tier] … 不可用（需要 W×H）」 | `src/ui.rs:5183、src/ui.rs:5303、src/window_size.rs:230` |

### `shell.density.large` 「大（150%）」单选项

kind `item` · status `active` · parent `shell.density`

- 样式：尺寸 `font_ui`、`font_weight_menu`
- 无障碍：可访问名「大（150%）」
- 同步：Linux done（§1.5 升一档上界与菜单禁用；refresh_tier_actions 的 available 判定）
- 实现备注：本档的最小窗口最大，因此最容易被禁用：这是「不静默钳到 100%」的直接表现（菜单里灰掉 + 文字标「放不下」）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该档在当前显示器可用 | 切到「大（150%）」（窗口按档位一起放大 / 收小）；放不下时动作禁用、条目文字为「大（150%），放不下」，完整原因进日志；已是本档时点击不重复重排 | `src/ui.rs:5183、src/ui.rs:5303` |

### `shell.error` 错误页（握手失败 / 无工作区断开）

kind `container` · status `active` · parent `shell`

- 样式：槽位 `workspace.content`；尺寸 `font_title`、`font_status`、`gap_page`
- 无障碍：可访问名「无法连接后端」
- 同步：Linux done（test_18_unbound_query.py：Objects 单页失连回到恢复页，存续查询保留实际标题及 SQL。）
- 实现备注：同一张 StatusPage 承载两种状态：握手失败「无法连接后端」与无工作区时的「已断开连接」；有工作区时断开**不**换页（保留已加载的行并标失连）。工具条保留在错误页上方，依赖后端的命令禁用。工作区外壳已建立时，布局命令继续使用其当前状态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 后端握手失败 | 主区域显示「无法连接后端」、安全错误说明与「重试」按钮；标题栏显示 Sqarvo，连接失败状态进入 tooltip。工具条继续呈现其可用状态 | `src/ui.rs::show_error` |
| load | 后端断开且没有普通业务标签可提供页内恢复入口 | 状态页显示断开原因与「重新连接」。标题栏显示 Sqarvo，断开状态进入 tooltip | `src/ui.rs::show_disconnected` |

### `shell.error.retry` 「重试」/「重新连接」

kind `control` · status `active` · parent `shell.error`

- 样式：尺寸 `min_target`、`font_ui`
- 无障碍：可访问名「重试」
- 同步：Linux done（test_18_unbound_query.py::test_reconnect_from_objects_only_startup：恢复入口成功重新握手并保留运行期专注状态。）
- 实现备注：同一枚按钮：握手失败页写「重试」，断开页写「重新连接」，走同一个回调。强调色 + pill 外形；点击后回到 Loading，而不是停在原地重试。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 错误页 / 断开页可见 | 回到 Loading「准备连接本地后端…」；若之前是就绪状态，工具条与菜单先回未就绪（保持后端命令禁用），然后重新握手 | `src/ui.rs:369、src/ui.rs:3786` |

### `shell.backend_failure` 后端失效恢复（「重启 Sqarvo」对话框与重启流程）

kind `container` · status `active` · parent `shell`

- 样式：槽位 `overlay`；尺寸 `min_target`、`font_ui`
- 无障碍：可访问名「Sqarvo 需要重启」
- 同步：Linux done（tests/ui/linux/results/run.DmA8Fxr9/report.json：Objects 单页恢复、草稿编辑器保留、旧服务强制回收及原档案显式重连，SQL 未自动执行。）
- 实现备注：横幅控件本身画在标签页（SQL 编辑器条）与数据页里，按钮文案「重启 Sqarvo」；本组件只登记窗口级的对话框与重启流程，横幅的位置归各自区的组件。对话框文案不写「后端进程已退出」这类内部细节，只说「本地服务已失效」—— 进程死活不是用户能判断的事。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 点任一标签的后端失效横幅「重启 Sqarvo」，或数据页错误横幅 | 弹 AdwAlertDialog「Sqarvo 需要重启」+「本地服务已失效，Sqarvo 无法继续读写数据库。重启本地服务会保留当前标签和 SQL 草稿。」；按钮：取消 / 查看日志 / 重启 Sqarvo（破坏性，默认取消） | `src/ui.rs:1379、src/sql_page.rs:128` |
| click | 对话框里选「重启 Sqarvo」 | 在原窗口重启本地服务并重新握手。保留已有工作区、标签、布局与编辑缓冲区；失效的会话禁用。确认退出的旧进程释放其会话额度；结果未知时保留额度。已绑定查询可在服务恢复后显式重新连接本档案。 | `src/ui.rs:1403` |
| click | 对话框里选「查看日志」 | 打开日志窗口；不重启、不改会话状态 | `src/ui.rs:1393` |

### `shell.menu.mcp` MCP

kind `item` · status `active` · parent `shell.menu`

- 样式：槽位 `menubar`
- 无障碍：可访问名「MCP」；Tab 与视图、帮助同级，位于两者之间
- 原生控件：gtk:GMenu + GSimpleAction
- 同步：Linux done（GTK 单测 mcp_management_has_a_top_level_menu_item 核对一级菜单模型；AT-SPI test_06_mcp_access.py 打开 MCP 管理页。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 主菜单可用 | 打开 MCP 管理页 | `frontend/linux/src/ui.rs、frontend/linux/src/mcp_management.rs` |

### `shell.layout` 窗口三栏布局

kind `container` · status `active` · parent `shell`

- 样式：尺寸 `workspace_target_width`、`workspace_target_height`、`panel_handle`、`panel_recovery`、`sidebar_width`、`sidebar_min`、`sidebar_max`、`inspector_width`、`inspector_min`、`inspector_max`
- 原生控件：gtk:GtkPaned
- 同步：Linux done（test_10_layout.py 自动折叠恢复、两侧拖拽及双击、偏好持久化，活动事务与未保存编辑下 gRPC 派发计数零增加；layout 单测覆盖精确阈值。test_15_refresh_regressions.py 复验同一导航选择、Objects 范围、标题和业务 RPC 计数，证据 tests/ui/linux/results/run.zZWok5oY/report.json。 tests/ui/linux/results/run.9ME2GefI/report.json 收尾用例覆盖隐藏侧栏后的焦点转移、主动隐藏后扩宽、事务与 Objects 的业务 RPC 计数。 tests/ui/linux/results/run.FlRhTuGR/report.json）
- 实现备注：首选宽度和主动显示偏好持久化。实际分配和运行期专注、优先请求及折叠历史不落盘。优先留出中央目标宽度。先缩右再隐藏右，再缩左再隐藏左。仅可见侧栏占用把手，自动恢复须满足恢复余量。主动展开自动折叠栏时给予临时优先，必要时隐藏另一侧；重新容纳两侧时恢复正常优先级。显隐即时生效，保留导航选择、Objects 范围、会话、编辑与事务。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| drag | 该侧栏及其分隔把手可见 | 调整该侧首选宽度；必要时压缩另一栏到下限，保持另一栏可见及其首选宽度。 | `src/layout.rs PanelLayout::drag` |
| double_click | 该侧栏及其分隔把手可见 | 恢复该侧默认首选宽度，按当前可用宽度重新求解。 | `src/shell.rs Shell::install_handle_gestures` |

### `shell.toolbar.layout` 导航、专注模式、信息面板

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`toolbar_layout_icon`、`gap_xxs`、`gap_lg`；颜色 `sqarvo_brand_soft`
- 无障碍：可访问名「导航 / 专注模式 / 信息面板」
- 图标：`layout.navigation`、`layout.focus`、`layout.inspector`
- 原生控件：gtk:GtkToggleButton
- 同步：Linux done（tests/ui/linux/results/run.TJeqNMo1/report.json：三档密度、无数据库连接仍可用、临时优先、专注和布局零 RPC；tests/ui/linux/results/run.cmSUm8oW/report.json：Compact 的三枚布局按钮。 tests/ui/linux/results/run.kEQx6EUw/report.json：两种语言、三档密度均可访问五个关键入口。）
- 实现备注：主工具栏最右侧的三个图标按钮依次为导航、专注模式、信息面板；工作区外壳建立后可用，与数据库连接无关。按下状态来自实际可见侧栏或专注状态。空间不足、专注覆盖与可主动显示状态各有工具提示。专注期间点击任一侧栏按钮退出专注并请求显示该栏。按钮调用 shell.layout 的既有布局命令及临时优先求解。

### `shell.menu.reset_layout` 视图 → 重置布局

kind `control` · status `active` · parent `shell.menu`

- 无障碍：可访问名「重置布局」
- 同步：Linux done（test_10_layout.py 通过菜单绑定的 win.reset-layout 恢复偏好和运行状态，保留窗口、密度、主题和语言。 tests/ui/linux/results/run.9ME2GefI/report.json）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 工作区外壳存在 | 恢复两侧栏默认首选宽度和主动显示，清除专注、临时优先和自动折叠历史，以现有窗口宽度重新求解。 | `src/ui.rs win.reset-layout` |

## 启动态外壳（`startup`）

握手期间的 Loading 页，以及握手成功后、还没有连接数据库时的六区域外壳：侧栏是与连接态同一个对象树（全部「未连接」）、主区域是固定 Objects 页的空态、信息面板常驻留白、状态栏写「未连接」。这里登记的是「启动态」这一档表面，不重复登记表结构。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `startup` | 启动态外壳 | area | active | done |
| `startup.loading` | 　Loading 页（标题 + 握手阶段文案 + 居中 spinner） | container | active | done |
| `startup.loading.spinner` | 　　加载 spinner | control | active | done |
| `startup.loading.stage` | 　　握手阶段文案 | control | active | done |
| `startup.workspace` | 　启动态工作区外壳（侧栏 + Objects + 信息面板 + 状态栏） | container | active | done |

### `startup` 启动态外壳

kind `area` · status `active`

- 样式：槽位 `startup`；尺寸 `sidebar_width`、`inspector_width`、`status_height`
- 同步：Linux done（tests/ui/linux/results/run.TJeqNMo1/report.json：后端握手后的工作区外壳、导航空态、无绑定查询。）
- 实现备注：握手成功后工作区外壳挂载同一套 Shell、对象树、固定 Objects 和信息面板。首个数据库连接更新已有实例。后端诊断信息（版本、协议、PID 和端点文件）在关于对话框。启动阶段先挂标题栏、工具条和页面槽位。在页面槽位显示 Loading、后端握手、再挂工作区。状态文案留在 Loading 页面及标题栏 tooltip。

### `startup.loading` Loading 页（标题 + 握手阶段文案 + 居中 spinner）

kind `container` · status `active` · parent `startup`

- 样式：槽位 `startup`；尺寸 `spinner_px`、`gap_xl`
- 同步：Linux done（test_01_startup.py::test_startup_readiness 与 test_18_unbound_query.py::test_reconnect_from_objects_only_startup：启动和重新握手由同一入口恢复，业务入口在未就绪时禁用。）
- 实现备注：AdwStatusPage：标题「正在连接后端」，说明行 = 当前握手阶段文案，子控件是 spinner（居中、上方留 gap_xl）。页面只占窗口的页面槽位：Loading 期间侧栏、标签条、信息面板、状态栏都还不存在（窗口根是 headerbar + 主工具条 + 页面槽位）。窗口标题和启动状态 tooltip 归 shell 的标题组件，本区不登记；后端握手失败后的错误页同样归 shell。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 应用启动 / 重新握手（worker 尚未回报 Connected） | 页面槽位换成居中 StatusPage：标题「正在连接后端」、说明行写当前阶段文案、spinner 同时开始旋转；主工具条已挂载，依赖后端的入口在握手前禁用 | `src/ui.rs::show_loading` |

### `startup.loading.spinner` 加载 spinner

kind `control` · status `active` · parent `startup.loading`

- 样式：尺寸 `spinner_px`
- 无障碍：可访问名「正在连接后端」
- 同步：Linux done（show_loading 的 GtkSpinner + set_size_request(spinner_px) + start()）
- 实现备注：GtkSpinner：边长取 spinner_px（请求值，由 GTK 决定实际绘制），构造后立即 start()。阶段推进走同一个 show_loading：set_page 先清空页面槽位再挂新的 StatusPage 与 spinner（ui.rs:1532-1554），上一帧的动画状态不保留；两帧之间只差一行说明，视觉上仍是同一处居中 spinner、没有静止帧。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | Loading 页出现 | 立即开始旋转（无静止帧）；阶段推进只换说明行，spinner 一直在转（整页替换时不出现静止帧、不闪） | `src/ui.rs:1558、src/ui.rs:1561` |

### `startup.loading.stage` 握手阶段文案

kind `control` · status `active` · parent `startup.loading`

- 无障碍：可访问名「握手阶段」
- 同步：Linux done（Event::Stage 带枚举、显示边界按 `startup.stage_*` 取文案（zh 六条逐字节同迁移前）；supervisor 夹具断言照旧；cargo test 494 passed；`--lang=en --qa-layout-audit --screenshot` 960×640、1200×800 均 14 项 0 超限）
- 实现备注：阶段文案是六条固定原文，由后端 supervisor 的握手阶段枚举给；worker 以 Event::Stage 只推枚举本身，界面在显示边界按「枚举 → l10n catalog key」映射取文案（#199，ADR 0031）：中文仍是逐字节不变的六条原文，界面不拼接；英文界面显示 startup.stage_* 的对应译文。六条原文（`l10n/startup.toml` key `startup.stage_*`）：「读取端点描述文件…」「校验协议版本与后端进程…」「启动后端进程 sqarvo-backend…」「等待后端就绪（最多 10 秒）…」「建立 gRPC 通道并附带本地会话 token…」「调用 SystemService.GetVersion 核对版本…」。说明行不总是阶段原文：show_loading 的说明还有三条调用方给的固定文案（同样进 catalog）——首帧 / 重新握手「准备连接本地后端…」（startup.desc_preparing）、重启「正在重启 Sqarvo…」（startup.desc_restarting）、关窗前「正在停止后端并清理端点文件…」（startup.desc_stopping）；标题恒为「正在连接后端」（startup.loading_title），界面只换说明行。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 收到 Event::Stage（握手推进） | 说明行改写为当前语言的阶段文案（zh = 六条原文之一，界面不拼接）；只有文本变化，不出现进度百分比、不加按钮、不改标题 | `src/backend/supervisor.rs:56、src/ui.rs:504、src/ui.rs:1556` |

### `startup.workspace` 启动态工作区外壳（侧栏 + Objects + 信息面板 + 状态栏）

kind `container` · status `active` · parent `startup`

- 样式：槽位 `startup`；尺寸 `sidebar_width`、`inspector_width`、`status_height`、`tab_strip_height`
- 同步：Linux done（tests/ui/linux/results/run.TJeqNMo1/report.json：未连接时固定 Objects 及布局入口；test_18_unbound_query.py：未绑定草稿和首个数据库连接。）
- 实现备注：握手成功即整体替换 Loading，不再停在旧版诊断页；窗口标题为 `Sqarvo` 且副标题为空（标题归 shell 区）。侧栏是与连接态同一个对象树（同一份连接注册表）：启动态所有连接都处于「未连接」，点一下即按保存的凭据连接（行级语义见 object_tree 区）。握手完成创建一个 Workspace、TabSet 和 Shell；首次数据库连接更新同一实例。保留布局、标签顺序、活动页和草稿的未绑定状态。握手完成后可从标签行、工具条、菜单和快捷键新建草稿。主区域落地就是固定 Objects 页的空态：标题「没有选中对象」，说明「从左侧选择一个连接开始：双击一条已保存的连接即可连接。」。页面本体（页签、三列表格、状态行）与这一档文案已由 objects_page 区登记（objects_page.tab / objects_page.empty），本区保证无普通标签时显示这一页。由 TabSet 建立单个固定 Objects 页，首次连接保留这一页。信息面板是同一套外壳：未选中对象时内容区留白；选中一条已保存的连接时按 inspector 区显示连接卡片，只是没有服务端版本 / 延迟两行（未连接时省略）。启动态摘要不属于本区：状态栏把连接名「未连接」与「驱动 N · 浏览会话 M」拼成一行「未连接 · 驱动 0 · 浏览会话 0」，登记在 status_bar 区（status_bar.row2.session）；本区只把 N / M 的真实取值（驱动数照抄握手快照、浏览会话数为 0）交给它。启动态没有表标签：中央操作行的行操作全部禁用并给原因「未连接：从左侧选择一个连接开始」，SQL 摘要写「尚未连接」——行 1 的结构与原因文案归 status_bar 区。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 握手成功（Event::Connected） | 外壳落地：侧栏换成对象树（已保存连接全部「未连接」）、主区域放固定 Objects 页、信息面板按布局状态显隐，未选中对象时内容区留白、状态栏写「未连接」；连上数据库前不存在表标签 | `src/ui.rs::show_startup_workspace` |
| load | 主工具条是窗口级控件（不随外壳重建） | 握手后可连接或新建未绑定查询；分类需要可用数据库目标。三个布局按钮随工作区可用 | `src/ui.rs::show_startup_workspace、src/shell.rs::Toolbar::set_backend_ready` |
| load | 连接注册表为空（没有已保存的连接） | 侧栏不列行，显示状态页：标题「还没有保存的连接」，说明「用工具条的「连接」保存一条连接。保存后可在导航中找到它。」；树状态行写「连接注册表为空」。树本体与行模板不在本区重复登记，本条目只记启动态会看到什么 | `src/tree.rs:1520、src/tree.rs:3179` |
| load | 后端已握手但没有可用浏览目标 | 可适用的分类入口保持禁用，tooltip 说明需要选择数据库或 Schema | `src/shell.rs::Toolbar::update_buttons` |

## 状态栏（`status_bar`）

窗口底部为全宽状态栏，中央操作行承载行操作和分页。读数与可用性跟随当前标签。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `status_bar` | 状态栏 | area | active | done |
| `status_bar.gutter` | 　左边沟（分栏线左侧整块） | container | removed | 已清 |
| `status_bar.row1` | 　行 1（行操作 · 选中数 · 展示形式 · 分页器） | container | active | done |
| `status_bar.row1.ops` | 　　行操作按钮组（新增 / 删除 / 应用 / 撤销 / 刷新 / 取消） | container | active | done |
| `status_bar.row1.ops.add` | 　　　「新增」图标按钮 | control | active | done |
| `status_bar.row1.ops.delete` | 　　　「删除」图标按钮 | control | active | done |
| `status_bar.row1.ops.apply` | 　　　「应用」图标按钮 | control | active | done |
| `status_bar.row1.ops.discard` | 　　　「撤销」图标按钮 | control | active | done |
| `status_bar.row1.ops.refresh` | 　　　「刷新」图标按钮 | control | active | done |
| `status_bar.row1.ops.cancel` | 　　　「取消」图标按钮（两用：取消查询 / 中止提交批次） | control | active | done |
| `status_bar.row1.selected` | 　　选中行数（N selected） | control | active | done |
| `status_bar.row1.detail_toggle` | 　　展示形式切换（按行浏览 / 字段 · 值） | control | active | done |
| `status_bar.row1.pager` | 　　分页器（首页 / 上页 / 下页 / 末页 + 只读页码摘要） | container | active | done |
| `status_bar.row1.pager.first` | 　　　「首页」按钮 | control | active | done |
| `status_bar.row1.pager.previous` | 　　　「上一页」按钮 | control | active | done |
| `status_bar.row1.pager.next` | 　　　「下一页」按钮 | control | active | done |
| `status_bar.row1.pager.last` | 　　　「末页」按钮 | control | active | done |
| `status_bar.row1.pager.summary` | 　　　只读页码摘要（第 N 页 / 共 M 页 · 总数） | control | active | done |
| `status_bar.row2` | 　全局状态行（SQL 摘要 · Last Refresh / 空态摘要 / MCP） | container | active | done |
| `status_bar.row2.sql` | 　　SQL 摘要（左段） | control | active | done |
| `status_bar.row2.last_refresh` | 　　Last Refresh（右段） | control | active | done |
| `status_bar.row2.session` | 　　空态摘要（连接名 / 未连接 · 驱动 N · 浏览会话 M） | control | active | done |

### `status_bar` 状态栏

kind `area` · status `active`

- 样式：尺寸 `status_height`、`row_height_tree`；颜色 `sqarvo_separator`、`sqarvo_status_fg`、`window_fg_color`
- 同步：Linux done（test_10_layout.py 验证全局状态和中央工作区语义容器及原有读数；test_01_startup.py 验证启动摘要。 tests/ui/linux/results/run.9ME2GefI/report.json）
- 实现备注：全宽贴窗口底部，展示 SQL 摘要、最后刷新、启动态连接摘要及 MCP 状态。行操作和分页属于中央工作区，保留原有控件与命令。导航搜索及反馈归 object_tree.search。

### `status_bar.gutter` 左边沟（分栏线左侧整块）

kind `container` · status `removed` · parent `status_bar`

- 同步：Linux removed（Shell::mount_search 将原搜索控件挂到导航顶部。）

### `status_bar.row1` 行 1（行操作 · 选中数 · 展示形式 · 分页器）

kind `container` · status `active` · parent `status_bar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_tree`、`gap_md`
- 同步：Linux done（test_10_layout.py 活动事务时按钮及分页器仍归中央工作区。 tests/ui/linux/results/run.9ME2GefI/report.json）
- 实现备注：左起为行操作，操作行位于中央工作区底部。中间一个可扩展空白把右组（选中数 · 展示形式 · 分页器）推到中央工作区右端；间距 8（gap_md）。行 1 自己没有交互与读数：可用性来自 status_bar.row1.ops 的六个按钮、分页器来自 status_bar.row1.pager，本组件只记布局。

### `status_bar.row1.ops` 行操作按钮组（新增 / 删除 / 应用 / 撤销 / 刷新 / 取消）

kind `container` · status `active` · parent `status_bar.row1`

- 样式：尺寸 `icon_button_width`、`icon_button_height`；状态 disabled=`sqarvo_status_fg`
- 同步：Linux done（tests/ui/linux/results/run.3jtK5EPM/report.json：已绑定 SQL 与行操作；tests/ui/linux/results/run.x16Mog9W/report.json：零连接和未绑定执行禁用；tests/ui/linux/results/run.d98WgmUQ/report.json 状态栏与断连用例通过。）
- 实现备注：六个图标按钮连成一组（linked，组内间距 0），图标盒与命中区统一走 icon_button_width / icon_button_height，不按单个字形调大小。禁用态：前景换 @sqarvo_status_fg 并叠 opacity 0.45（v0.2.5 加，为「激活 / 非激活一眼可辨」）；同时用 opacity: 1 / filter: none 压掉 Adwaita `.flat:disabled` 与 `dim-label` 自带的透明度，颜色只由这两条规则决定。组尾一条竖分隔线（1px、按行高居中）把行操作与右侧读数隔开；分隔线没有交互，不单独登记。按钮只显示图标，tooltip 同时登记为可访问名；tooltip 文案是动作本身（不带「新增行：」这类前缀），可访问名仍是构建时那句带前缀的话。这里是同一个动作的第二个入口：点击直接走数据页 / 查询页的同一套方法，状态栏不复制任何业务逻辑。§7 把「查询标签没有可编辑表格」概括为一句话；实现按结果可编辑性给更具体的原因（只读结果、新行还空着、没有选中行各一句），见 sql_page 的行操作可用性。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 当前标签是表标签 | 六个按钮的可用性与页内按钮同一条判定（直接读页内按钮的 sensitive，或用数据页的显式谓词，不重复判定）；「刷新」单独判定（已打开表、无 RPC 在途、无脏行、无未提交新行）；「取消」只在提交批次在途时可点（查询页是另一档，见下一条）；全部禁用时每个 tooltip 追加「（当前不可用：<原因>）」，原因取数据页当前状态（如「正在读取数据」「连接已断开」「没有可提交的更改」） | `src/shell.rs:1123、src/shell.rs:1133、src/data_page.rs:5043、src/data_page.rs:5076、src/data_page.rs:5134` |
| load | 当前标签不是表标签（Objects 页 / 查询标签 / 启动态未连接） | 六个按钮全部禁用并给原因：Objects 页「Objects 页：在对象树里打开表或新建查询标签」；未连接「未连接：从左侧选择一个连接开始」；查询标签按结果可编辑性给原因（例如「结果只读：执行单表查询后才能编辑」）。唯一例外是「取消」（改动确认单 #7，两用）：查询标签上按**查询是否在途**给值——在途可点（点它取消查询），空闲置灰并写「当前没有执行中的查询」。同时分页摘要、选中数、Last Refresh 整格隐藏 | `src/ui.rs::show_startup_workspace、src/ui.rs::apply_tab_status、src/sql_page.rs::SqlPage::row_ops` |

### `status_bar.row1.ops.add` 「新增」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「新增行：在当前表末尾新增一行」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.add`
- 同步：Linux done（§7 行 1 六个按钮；RowOp::Add → begin_insert。tests/ui/linux/test_10_data_page_insert_editor.py 核对新增产生一行可编辑草稿，取消单元格输入后草稿仍可提交。）
- 实现备注：工具条上已没有这个按钮（v0.2.4 删除）：它是状态栏 `+` 与 `win.add-row` 的唯一入口，可用性由数据页显式谓词给出。禁用时 tooltip 变成「在当前表末尾新增一行（当前不可用：<原因>）」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 可编辑的表标签，且没有在途请求、没有未提交的新行草稿 | 在当前表末尾插入一行草稿（与页内新增同一条路径，不落库）：新行可逐格填值，填完要按「应用」才提交 | `src/shell.rs:789、src/data_page.rs:5156` |

### `status_bar.row1.ops.delete` 「删除」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「删除选中行：标记为待删除（提交前可撤销）」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.delete`
- 同步：Linux done（§7 行 1 六个按钮；RowOp::Delete → delete_selected_row）
- 实现备注：v0.2.5 起工具条上的「删除」已删，入口在这里与数据行右键菜单；两处共用同一条可用性谓词。只标记、不发 RPC：多条选中按批量语义一次标记。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 可编辑的表标签且有选中行，无在途请求 | 把选中行标记为待删除（提交前可「撤销」回来），不立即发请求 | `src/shell.rs:789、src/data_page.rs:5156` |

### `status_bar.row1.ops.apply` 「应用」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「应用：提交所有脏行的变更」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.apply`
- 同步：Linux done（test_08_data_page_commit_entry.py 初始禁用与入口唯一；test_10_layout.py 布局变更后保留草稿与应用入口。 tests/ui/linux/results/run.9ME2GefI/report.json）
- 实现备注：这是表数据页 / 单表查询结果**唯一**的提交入口：工具条「提交更改」已移除（data_page.toolbar.commit，removed_in v0.2.3.4），在途取消由同组「取消」承担。可用性由数据页显式谓词 `commit_available()` 给出（在途提交时点它即取消、取消中禁用，其余按可编辑 + 有待提交变更 + 不忙，只读 / 失败 / 断连最后覆盖），状态栏不重复判定，也不再读页内按钮的 sensitive。危险操作判定不在这里另写一套：本地判定（>100 行或含任何删除）与服务端 acknowledge_risk 都走数据页的同一条路径。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有脏行且没有在途提交（提交在途时点它就取消本批，取消中禁用） | 提交本批变更（全部成功后再 Commit）；变更 >100 行或含任何删除时先弹「确认提交变更？」（正文给更新 / 删除行数与删除警告，破坏性默认取消；取消 →「已取消提交（未发送任何变更）」），服务端要求 acknowledge_risk 时再走同一个对话框 | `src/shell.rs:830、src/data_page.rs:5198（RowOp::Apply → on_commit_clicked）、src/data_page.rs:commit_available、src/table_data.rs:535` |

### `status_bar.row1.ops.discard` 「撤销」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「撤销：放弃全部未提交的编辑与删除标记」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.discard`
- 同步：Linux done（§7 行 1；RowOp::Discard → discard_changes）
- 实现备注：只丢弃本地未提交的编辑与删除标记，不动已提交的数据；改动全部改回原值时顺手回滚空事务。与「取消」不是一回事：取消是中止在途批次并整批回滚（ADR 0018）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有未提交的编辑、删除标记或新行草稿，无在途请求 | 放弃全部未提交的改动（含新行草稿与删除标记），已打开的空事务一并回滚；数据回到最后一次读取的状态 | `src/shell.rs:789、src/data_page.rs:5156` |

### `status_bar.row1.ops.refresh` 「刷新」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「刷新：重新读取当前表的第一页」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.refresh`
- 同步：Linux done（§7 行 1「刷新单独按状态判定」；refresh_available + RowOp::Refresh）
- 实现备注：可用性单独判定，不读页内按钮（页内没有对应的刷新按钮）：已打开表、没有 RPC 在途、没有未提交的更改与未提交的新行。有未提交更改时不直接刷：走与翻页 / 排序 / 筛选同一条脏行守卫，先提示提交或放弃。F5 在表标签上走的是同一件事；状态栏不提供第二个刷新入口。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 已打开表、无 RPC 在途、无脏行与未提交新行 | 重新读取当前表的第一页（保持当前排序与筛选，回到第 1 页）；有未提交更改时被守卫拦下并说明原因 | `src/shell.rs:789、src/data_page.rs:5076、src/data_page.rs:5156` |

### `status_bar.row1.ops.cancel` 「取消」图标按钮（两用：取消查询 / 中止提交批次）

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「（随当前页切换：查询页「取消当前查询（Esc）」，数据页「取消在途的提交批次」）」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.cancel`
- 同步：Linux done（真机 QA（独立 app id + 隔离 XDG_CONFIG_HOME，MariaDB 无默认库档案）：① `SELECT SLEEP(10)` 执行中 `cancel 1` → 日志「[qa] 点了状态栏的「取消」（第 1 个查询标签）」「[query] 点击取消」「[query] 状态行：已取消 · 已保留 0 行」，`expect-query 1 state=cancelled`、`rows=0` 通过；再点一次 →「[qa] 状态栏「取消」已置灰：第 2 次点击被忽略（幂等）」（置灰即幂等，不发 RPC）。② `--qa-cancel-after-ms 1200` 同一条路径：「[qa] 点状态栏的「取消」（第 1 次）」→ 同一状态行文案 → 150 ms 后第 2 次点击落到置灰按钮。③ 可用性：查询在途 `row_ops().cancel=true`、空闲与各终态 false（单测 status_bar_cancel_is_enabled_only_while_a_query_runs），查询页 tooltip / 可访问名由 `cancel_copy(Query)` 给出「取消当前查询（Esc）」+ 置灰原因「当前没有执行中的查询」（单测 cancel_button_is_two_purpose_and_says_which_one）。数据页口径未改：动作分派（`data.perform_row_op(RowOp::Cancel)`）与可用性（`RowOpsState.cancel`）一行未动，tooltip 仍按 `RowOpsState.reason` 拼装。）
- 实现备注：图标用实心方块：主题的 process-stop 在 Yaru 是八边形、在 Adwaita 是叉号，两套主题两种样子，也不是「停止」的通用形态。**两用，按当前页分派动作、tooltip 与可访问名**（改动确认单 #7）——只改文案不改动作会让用户点了一枚说着「取消查询」却去回滚批次的按钮。查询页：查询在途（Loading / Streaming）时可点，点击取消查询（与 `Esc` 同一个方法），重复点幂等；空闲时置灰，tooltip「取消当前查询（Esc）（当前不可用：当前没有执行中的查询）」。查询页内**没有**取消按钮，这里是唯一入口。数据页：只在提交批次在途时可点，语义是中止本批并整批回滚（ADR 0018），不是撤销已提交的数据；置灰时的原因沿用 `RowOpsState.reason`，口径与改动前一致。停在 `Objects` 页 / 启动态：两个语义都不成立，置灰并说明原因。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前页是查询标签，且查询在途（Loading / Streaming） | 取消这条查询（与 Esc、QA 脚本同一个方法）：发 Cancel 并立即进入「已取消 · 已保留 N 行」；重复点幂等（第二次起只记日志、不发 RPC）。查询结束（含取消）后按钮自行置灰 | `src/sql_page.rs:1055、src/sql_page.rs:1100、src/sql_page.rs:521、src/shell.rs:1155、src/shell.rs:1221` |
| click | 提交批次在途（Submitting） | 中止本批并整批回滚已执行的行；重复点幂等（只写日志），提交结束后按钮自行置灰 | `src/shell.rs:1155、src/data_page.rs:5050、src/data_page.rs:5156` |
| load | 当前页切换到查询标签 / 表数据标签 / Objects 页 | 按钮的语义、tooltip 与可访问名跟着换：查询页「取消当前查询（Esc）」、数据页「取消在途的提交批次」、Objects 页两者都不成立（置灰 + 原因）；可用性各自按自己的在途状态给值 | `src/shell.rs:792、src/shell.rs:1203、src/ui.rs:1931` |

### `status_bar.row1.selected` 选中行数（N selected）

kind `control` · status `active` · parent `status_bar.row1`

- 样式：尺寸 `font_status`、`gap_md`
- 无障碍：可访问名「已选中行数」
- 同步：Linux done（§7 行 1 选中数；apply_snapshot 的 selected_count）
- 实现备注：文案固定为 `N selected`（拉丁词，不写「已选 N 行」）：与行 2 的 `Last Refresh:` 同一套拉丁读数。只在表标签上有效；Objects 页、查询标签、启动态一律隐藏，不显示 `0 selected`。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 表标签且选中行数大于 0 | 显示「N selected」（选中 1 行也出现，不只多选）；tooltip「已选中 N 行：Ctrl/Shift 可多选，删除按批量语义提交」；选中集清空时整格隐藏 | `src/shell.rs:1084、src/data_page.rs:5043` |

### `status_bar.row1.detail_toggle` 展示形式切换（按行浏览 / 字段 · 值）

kind `control` · status `active` · parent `status_bar.row1`

- 样式：尺寸 `icon_button_width`、`icon_button_height`、`gap_md`
- 无障碍：可访问名「字段 / 值：只看选中行的每个字段与它的值」；Tab 在行操作组之后、分页器之前
- 图标：`view.detail_toggle`
- 同步：Linux done（§7 行 1 展示形式切换；v0.2.5 RowOp::ToggleDetail 与快照回推 收尾用例选择数据行并打开详情，切换 Objects / 查询后检查详情按下态复位。 tests/ui/linux/results/run.FlRhTuGR/report.json）
- 实现备注：位置在分页器**前方**（§7 / workspace-model §7 的同一处摆放）：选中数 → 展示形式 → 分页器。它是 toggle，自己不记状态：按下只把「用户按了这一下」交给数据页，选中态由数据页快照推回，否则切标签 / 换表后按钮和屏幕会各说一套。按下态沿用 Adwaita 的 flat toggle 外观（未单独写主题 token，P1 定色）。只读连接也允许（它只是换一种看法，不写库）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有表标签且选中了一行（不要求可编辑） | 数据网格在按行浏览与「字段 | 值」两列之间切换：按下后只讲选中的那一行（字段名 + 当前值，与网格同一份显示文本），再按一次切回按行浏览；不可用时置灰，tooltip 按场景给「字段 / 值：只看选中行的每个字段与它的值（先选中一行）」「…（当前不可用：<原因>）」「…（再点一次切回按行浏览）」 | `src/shell.rs:891、src/data_page.rs:2074、src/shell.rs:1141` |
| load | 数据页推来新快照（切标签 / 换表 / 选中集被清空） | 选中态由快照的展示形式推回；选中集清空或换表时自动复位到按行浏览，不复位成「上次按下的样子」 | `src/shell.rs:1081、src/data_page.rs:2094` |

### `status_bar.row1.pager` 分页器（首页 / 上页 / 下页 / 末页 + 只读页码摘要）

kind `container` · status `active` · parent `status_bar.row1`

- 样式：尺寸 `pager_button_min_width`、`min_target`、`gap_sm`、`pager_font`
- 同步：Linux done（test_10_layout.py 查看中央工作区内四个分页动作，未变更分页逻辑。 tests/ui/linux/results/run.9ME2GefI/report.json 收尾用例切换两个独立表、Objects 和 SQL 后检查分页状态，并验证失连后分页禁用。 tests/ui/linux/results/run.FlRhTuGR/report.json）
- 实现备注：四个翻页按钮连成一组（组内间距 0）+ 只读摘要，两者间距 6（gap_sm）；整组共用一条 tooltip。整组 tooltip：「offset 分页（#118）：首页 / 上页 / 下页 / 末页；页码只读，跳转到远处页码不在本版提供」。首页 / 末页用自有图标（`|<` / `>|`）：主题的双尖括号与相邻的单箭头只差一条线，压到小字号后读不出是两档不同动作。没有表标签时摘要整格隐藏、四个按钮置灰但不隐藏（保持行 1 右组宽度不跳，避免切标签时整行抖动）。旧契约 §7 写「按钮整格隐藏」，2026-11-12 按实现定稿为置灰不隐藏（契约 §9.1 已收口）。按钮动作只交给当前数据页，由数据页决定目标 offset 并在有未提交更改时拒绝——状态栏不自己算页码。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 当前标签不是表标签 | 摘要整格隐藏（不留「未打开表」占位），四个翻页按钮一律置灰，不出现「看起来能点」的按钮 | `src/shell.rs:1059、src/shell.rs:1077、src/shell.rs:1166` |

### `status_bar.row1.pager.first` 「首页」按钮

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_button_min_width`、`min_target`
- 无障碍：可访问名「首页（offset = 0）」；Tab 在分页器组内依次 Tab 可达
- 图标：`pager.first`
- 同步：Linux done（§7 行 1 分页器四按钮；PagerAction::First）
- 实现备注：tooltip「首页（offset = 0）」；页码只读，按钮不显示页码数字。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表标签，且不在第一页（无未提交更改） | 跳到第一页（offset 归零），排序 / 筛选保持；有未提交更改时由数据页拒绝并说明原因 | `src/shell.rs:865、src/data_page.rs:331` |

### `status_bar.row1.pager.previous` 「上一页」按钮

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_button_min_width`、`min_target`
- 无障碍：可访问名「上一页」；Tab 在分页器组内依次 Tab 可达
- 图标：`pager.previous`
- 同步：Linux done（§7 行 1 分页器四按钮；PagerAction::Previous）
- 实现备注：tooltip「上一页」；第一页时置灰，不是隐藏。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表标签且不在第一页（无未提交更改） | 退回上一页（offset 减一页），排序 / 筛选保持 | `src/shell.rs:865、src/data_page.rs:331` |

### `status_bar.row1.pager.next` 「下一页」按钮

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_button_min_width`、`min_target`
- 无障碍：可访问名「下一页」；Tab 在分页器组内依次 Tab 可达
- 图标：`pager.next`
- 同步：Linux done（§7 行 1 分页器四按钮；PagerAction::Next）
- 实现备注：tooltip「下一页」；没有下一页时置灰，不靠「点了没反应」表达边界。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表标签且还有下一页（无未提交更改） | 前进一页（offset 加一页），排序 / 筛选保持 | `src/shell.rs:865、src/data_page.rs:331` |

### `status_bar.row1.pager.last` 「末页」按钮

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_button_min_width`、`min_target`
- 无障碍：可访问名「末页（需要总数）」；Tab 在分页器组内依次 Tab 可达
- 图标：`pager.last`
- 同步：Linux done（§7 行 1 分页器四按钮；PagerAction::Last 需要总数）
- 实现备注：tooltip「末页（需要总数）」：总数未知（驱动不报总数或计数超时）时置灰，不猜页数。总数三态（共 N / 约 N / 共 ?）只体现在摘要里，按钮不显示总数。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表标签且已知总页数、不在末页（无未提交更改） | 跳到最后一页；总数未知时按钮置灰（不发请求、不编造页数） | `src/shell.rs:865、src/data_page.rs:331` |

### `status_bar.row1.pager.summary` 只读页码摘要（第 N 页 / 共 M 页 · 总数）

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_font`
- 无障碍：可访问名「页码摘要」
- 同步：Linux done（§7 行 1 分页器只读摘要；apply_snapshot 的 pager_text）
- 实现备注：页码只读：本版没有页码输入框、没有跳转到远处页码。总数只有服务端给的时候才追加（共 N 行 / 约 N 行），未知就只写页码，不写 `?` 冒充。文案用拉丁数字与中文标点，字号按 pager_font 与翻页按钮的视觉高度对齐。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 表标签 | 写「第 P 页」，已知页数时写「第 P / M 页」，有总数时追加「 · 共 N 行」式总数；读取中写「读取中…」、读取失败写「读取失败」、断连写「已断开」；没有表标签时整格隐藏 | `src/shell.rs:1059、src/data_page.rs:1944` |

### `status_bar.row2` 全局状态行（SQL 摘要 · Last Refresh / 空态摘要 / MCP）

kind `container` · status `active` · parent `status_bar`

- 样式：槽位 `status_bar.row2`；尺寸 `row_height_tree`、`gap_xl`、`gap_md`
- 同步：Linux done（test_10_layout.py 全局状态语义容器内同时读到 SQL、Last Refresh 及 MCP 文案。 tests/ui/linux/results/run.9ME2GefI/report.json 收尾用例确认切到 Objects 或 SQL 后不显示前一个表的最后刷新。 tests/ui/linux/results/run.FlRhTuGR/report.json）
- 实现备注：只有两段：左段 SQL 摘要（可省略），右段 Last Refresh，空态时右段换成一行启动摘要（连接名，或「未连接 · 驱动 N · 浏览会话 M」）。全局状态行横跨窗口，独立于中央操作区。文案一律用 ASCII 冒号（`SQL:` / `Last Refresh:`）：全角冒号会把中文行盒高度带进来，两段不在同一基线（实测偏高 2px）。行号区间、连接名、库名、事务状态、上次耗时都不在这里：表名 / 库在标签名里，事务在表格工具条里。

### `status_bar.row2.sql` SQL 摘要（左段）

kind `control` · status `active` · parent `status_bar.row2`

- 样式：尺寸 `font_status`
- 无障碍：可访问名「当前 SQL 摘要」
- 同步：Linux done（tests/ui/linux/results/run.3jtK5EPM/report.json：已绑定 SQL 与行操作；tests/ui/linux/results/run.x16Mog9W/report.json：零连接和未绑定执行禁用；tests/ui/linux/results/run.d98WgmUQ/report.json 状态栏与断连用例通过。）
- 实现备注：单行省略：长语句截断成一行，全文进 tooltip。冒号是 ASCII 的 `SQL:`，不是全角「：」。当前标签是「字段 | 值」视图或正在编辑时也照旧显示这一页的摘要——摘要讲的是取数语句，不是展示形式。连接刚建立、还没有标签状态推送时，这一格会短暂显示「SQL: 已连接 · <服务端版本>」，首个标签状态到达后被覆盖。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 当前标签是表标签 | 写「SQL: <摘要>」：当前筛选 / 排序拼出的只读 `SELECT … LIMIT {offset}, {page_size}`（只展示、不执行，值以字面量呈现）；筛选或排序变化后跟着更新 | `src/shell.rs:1192、src/shell.rs:1105、src/data_page.rs:1944、src/browse_view.rs:821` |
| load | 当前标签是查询标签 | 写「SQL: <最近一次查询摘要>」：沿用查询页自己的摘要（单行），没执行过时由查询页给文案 | `src/ui.rs:1865、src/ui.rs:1866` |
| load | 当前标签是 Objects 页 | 重置为「SQL: 尚未执行」——Objects 页没有取数语句，不用上一张表的摘要蒙着 | `src/ui.rs:1879、src/shell.rs:925` |
| load | 连接断开 / 启动态未连接 | 断连写「SQL: 连接已断开」；启动态写「SQL: 尚未连接」 | `src/ui.rs::show_startup_workspace、src/ui.rs::activate` |

### `status_bar.row2.last_refresh` Last Refresh（右段）

kind `control` · status `active` · parent `status_bar.row2`

- 样式：尺寸 `font_status`、`gap_md`
- 无障碍：可访问名「上次读取时间」
- 同步：Linux done（test_10_layout.py 有事务的数据页在全局状态容器内读取 Last Refresh；Shell::apply_snapshot 与 tick_relative_time 处理不变。 tests/ui/linux/results/run.9ME2GefI/report.json）
- 实现备注：相对时间只到分钟：`<1m` / `Nm` / `Nh` / `Nd`，与文件列表同一套格式化函数。没有刷新过（从未读到数据）时整格隐藏，不写 `Last Refresh: —`。tooltip 给墙上时间 `HH:MM:SS` 并提示「offset 分页不保证跨页稳定」——相对时间只是一句提示，精确时刻在 tooltip 里。墙上时间由「现在减去已经过去的时长」反推（Instant 没有日历语义），转换失败时退回相对时间，不显示错误时间戳。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 读到过数据（有上次刷新时刻） | 写「Last Refresh: <相对时间>」；每 30 秒重算一次，数据页推送新快照时也重算 | `src/shell.rs:1094、src/shell.rs:1112、src/shell.rs:62、src/browse_view.rs:918` |
| hover | 该格可见 | tooltip 给墙上时间与分页提示：「上次读取时间：HH:MM:SS（offset 分页不保证跨页稳定，必要时点旁边的刷新）」 | `src/shell.rs:1099、src/shell.rs:1242` |

### `status_bar.row2.session` 空态摘要（连接名 / 未连接 · 驱动 N · 浏览会话 M）

kind `control` · status `active` · parent `status_bar.row2`

- 样式：尺寸 `font_status`、`gap_sm`
- 无障碍：可访问名「连接与会话摘要」
- 同步：Linux done（tests/ui/linux/results/run.x16Mog9W/report.json：真实握手和首次数据库连接，驱动摘要与连接状态。）
- 实现备注：只在**没有表标签**时出现：有表标签时这一行只留 SQL 摘要与 Last Refresh（表名 / 库 / 事务在别处已经有了）。启动态把连接名「未连接」与「驱动 N · 浏览会话 M」用「 · 」拼成一行，读作「未连接 · 驱动 0 · 浏览会话 0」。连接后显示连接名；断连后连接名被清空，只剩驱动与浏览会话计数。单行省略、最多 48 个字符宽，窗口变窄时这一格最先被压（右侧留 6 的边距，最后一个字不贴窗口边）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 没有表标签 | 显示连接名，或「未连接 · 驱动 N · 浏览会话 M」（N / M 来自启动态与连接注册表的真实计数）；有表标签时整格隐藏 | `src/shell.rs::StatusBar::refresh_session、src/startup.rs::idle_status_text、src/ui.rs::show_startup_workspace` |

## 标签条与查询标签（`tabs`）

工作区顶部的标签条，以及查询标签页里的动作行、编辑器与结果区：标签的新建 / 复用 / 上限 / 关闭守卫，查询的执行 / 取消、五态、结果表头与续拉。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `tabs` | 标签条与查询标签工作区 | area | active | done |
| `tabs.strip` | 　标签条容器 | container | active | done |
| `tabs.strip.tab` | 　　标签项（标题 · 关闭 ×） | row | active | done |
| `tabs.strip.tab.table` | 　　　表数据标签（标题与 tooltip） | item | active | done |
| `tabs.strip.tab.query` | 　　　查询标签（标题与 tooltip） | item | active | done |
| `tabs.strip.tab.close` | 　　　标签关闭按钮（×） | control | active | done |
| `tabs.strip.new` | 　　「＋」新建查询标签 | control | active | partial |
| `tabs.strip.menu` | 　　标签右键菜单 | control | active | done |
| `tabs.strip.list` | 　　可搜索标签列表 | control | active | done |
| `tabs.limit` | 　「无法新建标签」提示 | control | active | done |
| `tabs.query` | 　查询标签页 | container | active | done |
| `tabs.query.actions` | 　　动作行（连接选择器 · 库选择器 · 执行） | control | active | done |
| `tabs.query.connection_picker` | 　　查询标签的连接选择器 | control | active | done |
| `tabs.query.connection_picker.cancel_binding` | 　　　「取消绑定」 | control | active | done |
| `tabs.query.database_picker` | 　　查询标签的库选择器 | control | active | done |
| `tabs.query.editor` | 　　SQL 编辑器（行号沟 + 等宽文本） | control | active | done |
| `tabs.query.state` | 　　结果区状态行（五态） | control | active | done |
| `tabs.query.results` | 　　结果区 | container | active | done |
| `tabs.query.results.views` | 　　　结果区页签（消息 / 摘要 / 结果） | control | active | done |
| `tabs.query.results.message` | 　　　消息页（语句日志） | control | active | done |
| `tabs.query.results.summary` | 　　　摘要页（执行摘要） | container | active | done |
| `tabs.query.results.header` | 　　　结果表头（列名 · 原始类型） | control | active | done |
| `tabs.query.results.grid` | 　　　结果行区（虚拟化表格） | control | active | done |
| `tabs.query.results.more` | 　　　续拉（滚动到末尾自动续拉 / 「加载更多」） | control | active | done |
| `tabs.query.results.statements` | 　　　语句切换控件（多语句脚本） | control | removed | 已清 |
| `tabs.query.results.result_picker` | 　　　结果集选择（多结果集折叠） | control | active | done |
| `tabs.query.splitter` | 　　查询分隔把手（可拖动分隔） | control | active | done |
| `tabs.query.results.close` | 　　　关闭结果区按钮 | control | active | done |
| `tabs.close_dialog` | 　关闭标签确认框 | control | active | done |
| `tabs.close_dialog.discard` | 　　「放弃更改并关闭」/「回滚事务并关闭」 | item | active | done |
| `tabs.close_dialog.save` | 　　「保存并关闭」/「保存、提交事务并关闭」 | item | active | done |
| `tabs.close_dialog.cancel` | 　　「取消」 | item | active | done |

### `tabs` 标签条与查询标签工作区

kind `area` · status `active`

- 样式：槽位 `workspace.tabstrip`；尺寸 `tab_strip_height`
- 同步：Linux done（隔离 UI test_window_local_sort_and_cancellation、test_sort_during_transaction_edits_and_running_query 与 test_close_receipt_during_sort_preserves_surviving_order：真实排序、越界和 Escape 后实例、草稿、事务、在途查询及会话归属保持。）
- 实现备注：标签条在最上，下面是当前标签的内容区。`Objects` 固定页钉在最左、没有会话、不占「有会话标签」的 8 个名额。标签与内容同属一个 AdwTabView。关闭保护完成后，关闭请求对该 AdwTabPage 确认销毁；临时页面转移保留工作区记录和会话。新标签追加在末尾。拖动会修改 AdwTabView 的页面顺序，标签位次读取 pages()。

### `tabs.strip` 标签条容器

kind `container` · status `active` · parent `tabs`

- 样式：槽位 `workspace.tabstrip`；尺寸 `tab_strip_height`、`gap_sm`、`gap_xxs`、`min_target`、`objects_min_width`；颜色 `sqarvo_separator`
- 原生控件：gtk:AdwTabBar + AdwTabView
- 同步：Linux done（test_search_lists_all_tabs_and_filters_locally 验证 840 宽窗口下的边缘自动滚动、手动位置保留、Objects 与两个入口可达，以及显式激活定位。）
- 实现备注：固定 Objects 和原生普通标签滚动区分开。加号位于普通标签区末端的原生动作槽，右侧列表入口常驻。滚动区溢出时两项操作仍可访问。滚动、缩放与拖拽使用同一个 AdwTabView 页面模型。手动滚动保留用户的位置，主动激活会定位对应标签。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| scroll | 普通标签宽度超过当前工作区 | 可单行横向滚动，Objects、加号和所有标签入口保持可达。 | `src/tabs.rs::TabSet::new、src/tabs/strip_input.rs::TabSet::reveal_strip_page` |

### `tabs.strip.tab` 标签项（标题 · 关闭 ×）

kind `row` · status `active` · parent `tabs.strip`

- 样式：尺寸 `tab_min_width`、`tab_max_width`、`tab_height`、`tab_radius`、`tab_padding_h`、`status_dot_diameter`、`min_target`、`font_ui`；颜色 `window_fg_color`、`accent_bg_color`；状态 hover=`window_fg_color`、selected=`window_fg_color`
- 无障碍：可访问名「（按实例生成的完整标签名称）」；Tab 可聚焦；Ctrl+Tab / Ctrl+Shift+Tab 循环切换，Alt+1..9 直选第 N 个
- 图标：`tab.unsaved`
- 同步：Linux done（test_19_workspace_tabs.py 验证真实指针窗口内排序、越界、重新移入、Escape、固定 Objects、零业务请求和零原生摘页，以及重排后的 Alt 位次。取消后可激活、执行和关闭。）
- 实现备注：业务类型图标与未保存标记分别来自页面图标和原生 indicator。未保存状态来自 SQL、表数据及查询结果编辑，执行 SQL 或失败的保存不会清除编辑标记。原生标签在可访问名称和 Tooltip 暴露完整名称。右键菜单捕获实例 ID，关闭活动标签按 MRU 恢复，缺少有效历史时首先尝试右邻。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 标签存在且不是当前页 | 切到该标签：不重建会话、不发请求；窗口标题、状态栏、工具条高亮按新标签刷新；其它标签的筛选与排序面板被收起 | `src/tabs.rs:1372、src/tabs.rs:2184、src/tabs.rs:2300、src/tabs.rs:2313` |
| load | 当前页面的关闭请求已经过保护流程并确认销毁 | 销毁该页面的工作区记录并收起筛选面板，释放其独占会话（Cancel 在途查询 → Close 游标 → Disconnect），会话名额在释放得到确认后更新。 | `src/tabs.rs::TabSet::finish_approved_close、src/tabs.rs::TabSet::on_page_detached` |
| drag | 在普通标签按住主按钮并达到拖拽阈值 | 窗口内按原生模型重排，保持现有活动页和历史。Escape、越界或手势取消恢复原有存续实例的相对顺序；重新移入须发起新手势。整个排序没有业务请求或页面转移。 | `src/tabs/strip_input.rs` |

### `tabs.strip.tab.table` 表数据标签（标题与 tooltip）

kind `item` · status `active` · parent `tabs.strip.tab`

- 无障碍：可访问名「（对象名及必要的消歧义上下文）」
- 图标：`tab.data`
- 同步：Linux done（test_16_workspace_context.py 验证默认短名、双连接同名、工作区类型、长名可达、别名刷新、限定信息保留和外部重命名。隔离 MariaDB 探针 run.IrirBlo2 验证同一连接下的同名表各自加载正确数据，并保留限定名和完整可访问描述。）
- 实现备注：默认显示工作区原始名称。同名组按 catalog、Schema、连接或工作区类型消歧义。已增加的限定保留至该实例关闭，档案别名按实际归属刷新。Tooltip 提供完整对象名、实际库或 Schema 和安全连接名。窗口标题继续读取工作区原始名称。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 在对象树 / Objects 页双击表或视图，且没有同表标签、未达上限 | 追加并激活表数据标签，按完整资源身份建立独占会话。打开期间可复用同一实例，后台完成保留用户当前活动页。 | `src/tabs.rs:1806、src/tabs.rs:1843、src/tabs.rs:1914、src/tabs.rs:1947` |
| double_click | 已有档案、catalog、Schema、对象种类和对象身份完全一致的表数据工作区 | 只切到已有标签：不新建标签、不新建会话、不重载数据；复用检查在标签上限之前，已开满 8 个仍可切过去 | `src/tabs.rs:1815、src/tabs.rs:128、src/tabs.rs:1599` |
| load | 该标签的会话已建立 | tooltip 保留原始名、所属库或 Schema 与连接显示名，会话建立后阶段显示「会话 <id 前 8 位>」 | `src/tabs.rs:2563、src/tabs.rs:2273` |

### `tabs.strip.tab.query` 查询标签（标题与 tooltip）

kind `item` · status `active` · parent `tabs.strip.tab`

- 无障碍：可访问名「（查询 N 及同名时的必要上下文）」
- 图标：`tab.query`
- 同步：Linux done（test_19_workspace_tabs.py::test_sql_tab_unsaved_indicator_survives_execution：草稿编辑显示可访问状态，执行成功保留，清空后移除。test_18_unbound_query.py 覆盖实际绑定和迟到回执。）
- 实现备注：原始名称为「查询 N」，序号从 1 递增，已经关闭的序号保留不用。库与连接放 Tooltip。每次新建分配独立序号；出现与数据工作区同名时按类型和实际上下文消歧义。窗口标题及 tooltip 都采用实际绑定，重绑在途时沿用此前已确认的归属。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 新建查询标签 | 标题写「查询 N」，序号自 1 起递增。tooltip 只显示已结算的绑定，未确定库时写「未指定」。未绑定草稿的窗口标题只有查询名。 | `src/tabs.rs:101、src/tabs.rs:1888、src/tabs.rs:1895、src/tabs.rs:2273` |
| load | 该查询标签的会话已建立 | tooltip 的阶段换成「会话 <id 前 8 位>」，库与连接仍在后两段 | `src/tabs.rs:2563` |

### `tabs.strip.tab.close` 标签关闭按钮（×）

kind `control` · status `active` · parent `tabs.strip.tab`

- 样式：尺寸 `min_target`
- 无障碍：可访问名「关闭标签」
- 图标：`action.close_tab`
- 同步：Linux done（test_window_local_sort_and_cancellation 通过真实关闭按钮、中键与快捷键验证 SQL 守卫和释放回执；qa_query_target_tracks_the_original_tab_session 验证原生转移、迟到结果与批量释放失败。）
- 实现备注：×、中键、`Ctrl+W`、标签菜单和批量关闭统一进入 close-page 守卫。钉住的 Objects 不显示关闭按钮，关闭守卫拒绝针对它的请求。确认框已在显示时再点 × 只记日志，不叠第二个框。后台关闭保留活动页。活动关闭按对象页在内的 MRU 恢复；最后一个普通关闭后回到 Objects。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 非 Objects 页，未在转移，且无需要保护的内容或活动会话操作 | 确认该实例销毁后摘页面，释放其独占会话（Cancel 在途查询 → Close 游标 → Disconnect）。重绑期间关闭时摘除该标签的请求归属，晚到的会话由 worker 释放，收到真实结果才更新额度。 | `src/tabs.rs::TabSet::on_close_page、src/tabs.rs::TabSet::finish_approved_close` |
| click | 非 Objects 页，含未保存 SQL、脏行、待提交结果、活动事务或执行中的查询 | 先弹关闭确认（tabs.close_dialog），在用户答完之前保留标签和编辑状态 | `src/tabs.rs::TabSet::on_close_page、src/tabs.rs::TabSet::present_close_dialog` |
| load | 已受理受保护的关闭，处理结果在原生页面转移期间返回 | 等待页面转移结束后，按该关闭请求的结果继续。转移期间的摘页保留工作区及会话。 | `src/tabs.rs::TabSet::finish_close_page` |
| click | 该标签的确认框已在显示 | 忽略这次请求并记一行「关闭标签 `<id>` 的确认框已在显示」，不叠第二个框；答完（或关掉）那个框后再点才会重新弹 | `src/tabs.rs:2685` |
| click | 该标签是固定 `Objects` 页 | 不关闭，记一行「Objects 页固定存在，不可关闭」 | `src/tabs.rs:2668` |
| key Ctrl+W | 存在当前标签 | 关闭当前标签，统一走 close-page 守卫。若关闭有效，按实际存续工作区恢复活动标签。 | `src/ui.rs:328、src/tabs.rs:2867` |

### `tabs.strip.new` 「＋」新建查询标签

kind `control` · status `active` · parent `tabs.strip`

- 样式：尺寸 `min_target`
- 无障碍：可访问名「新建查询标签」
- 图标：`action.new_query`
- 平台差异：Linux：当前加号固定在原生滚动区末端的动作槽。非溢出时该槽可能与最后标签之间有留白，尚未达到紧随标签的目标。见行为契约 §9。
- 同步：Linux partial（tests/ui/linux/results/run.cmSUm8oW/report.json：加号可达、目标完整可读、取消确认不创建工作区。非溢出时沿用已接受的原生动作槽留白。 tests/ui/linux/results/run.kEQx6EUw/report.json：与工具条提供同一份完整目标描述，失效确认保留原工作区。）
- 实现备注：四个入口同一条路径，没有旁路：工具条「新建查询」、标签条 ＋、`Ctrl+T`、菜单「文件 → 新建查询标签」；成功后把焦点交给编辑器。使用与工具条相同的目标 tooltip 和可访问描述，摘要截断时显示同一目标确认，继续时校验捕获的目标。普通标签末端的原生动作槽。溢出时位于滚动视口之外。超限时弹 tabs.limit，连点保持单个提示框。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端就绪、工作区已建立 | 按导航优先、活动工作区回退解析目标。两者均无目标时可新建独立草稿，无需会话额度；明确不可用时提示。申请独立会话才检查 tabs.limit。新建后聚焦编辑器 | `src/tabs.rs:1363、src/tabs.rs:1867、src/tabs.rs:1915、src/ui.rs:263、src/ui.rs:4607` |
| key Ctrl+T | 后端就绪、工作区已建立 | 与点 ＋ 同一条 `win.new-tab`：新建「查询 N」并聚焦编辑器；需要申请会话且超限时提示 | `src/ui.rs:327、src/ui.rs:4769、src/tabs.rs:1867` |

### `tabs.strip.menu` 标签右键菜单

kind `control` · status `active` · parent `tabs.strip`

- 无障碍：可访问名「标签右键菜单」
- 原生控件：gtk:AdwTabView setup-menu + GtkPopoverMenu
- 同步：Linux done（test_batch_cancel_stops_at_dirty_instance 与 test_reveal_background_tab_uses_captured_object：真实右键捕获后台实例、定位完整资源、剪贴板复制名称、取消停止批量。隔离 GTK 验证视觉目标集合与异步失败保留额度。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| right_click | 指向标签 | 捕获该实例且保留当前活动页。支持关闭本页、其他、左侧、右侧、全部。批量按视觉顺序捕获固定目标，排除 Objects。取消或关闭失败时停止后续操作，已完成的关闭保留。批量结束按实际存续页恢复活动状态，也保留等待回执期间对存续页的显式激活。 | `src/tabs/strip_menu.rs、src/tabs/close_order.rs` |
| click | 菜单已捕获一个标签实例 | 复制名称使用该实例的原始完整名称。在导航中定位使用捕获的完整目标，未绑定查询没有可执行的定位命令。 | `src/tabs/strip_menu.rs::TabSet::install_strip_menu` |

### `tabs.strip.list` 可搜索标签列表

kind `control` · status `active` · parent `tabs.strip`

- 样式：尺寸 `min_target`
- 无障碍：可访问名「所有标签」
- 图标：`tab.list`
- 原生控件：gtk:GtkMenuButton + GtkPopover + GtkSearchEntry + GtkListView
- 同步：Linux done（test_search_lists_all_tabs_and_filters_locally：列表包含 Objects 和普通实例，按名称或连接上下文过滤、改词和清空，派发计数无变化，选中结果激活指定实例。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 标签栏可用 | 按实际原生页顺序列出全部已打开实例，含 Objects、类型、完整名称、连接和库上下文、活动及未保存状态。输入名称或上下文子串在本地筛选；选择结果按稳定实例激活并定位。 | `src/tabs/strip_menu.rs::TabSet::tab_list_button` |

### `tabs.limit` 「无法新建标签」提示

kind `control` · status `active` · parent `tabs`

- 样式：槽位 `dialogs`；尺寸 `badge_min_height`
- 无障碍：可访问名「无法新建标签」
- 同步：Linux done（tests/ui/linux/results/run.DmA8Fxr9/report.json：8 会话、关闭及申请中占额、满额重绑先释放旧会话、旧服务确认退出后恢复额度。tests/ui/linux/results/run.TIP6OYEv/runner.log：重复与旧进程回执。）
- 实现备注：上限为 8 个独立标签会话。申请中、已建立和关闭待确认均占额；Objects、未绑定草稿与已确认失效的会话不占额。旧进程确认退出后只释放该进程的额度。同一段文案不重复弹：连点 ＋ / Ctrl+T 只留一个框，日志里仍每次记一行拒绝原因。文案不是静默失败：必须同时给出上限、当前占用数量和释放后重试的建议。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 8 个独立会话名额已占用时申请新会话 | 拒绝申请并显示上限和当前占用。保留已有工作区与草稿，已打开对象仍可复用。同一提示已在显示时不重复弹 | `src/tabs.rs:2895、src/tabs.rs:72、src/tabs.rs:59` |
| click | 提示框在显示 | 「知道了」关闭提示框；本次请求不新建任何标签 | `src/tabs.rs:2895` |

### `tabs.query` 查询标签页

kind `container` · status `active` · parent `tabs`

- 样式：槽位 `workspace.content`；尺寸 `gap_page`
- 同步：Linux done（tests/ui/linux/test_18_unbound_query.py：草稿零 Connect/ExecuteQuery、首次连接后同一实例、SQLite 成功及失败、绑定取消、额度与真实释放。tests/ui/linux/mariadb_catalog_probe.py：首个查询确认目标库，初始库验证失败保留未绑定草稿。）
- 实现备注：页内自上而下：动作行 → 编辑器（行号沟 + 等宽文本）→ 分隔线 → 结果区（五态一行 → 表头 → 行区 → 续拉）。动作行最左端是两枚下拉框：连接选择器（`tabs.query.connection_picker`）在前、库选择器（`tabs.query.database_picker`）在后；两枚都**没有可见文字**，可访问名分别是「当前连接」「当前库」。绑定请求可从连接选择器旁取消；查询执行期间的取消在状态栏行 1 的两用按钮上（`status_bar.row1.ops.cancel`），与 `Esc` 是同一个方法。标签提示中的库名和连接名取实际会话上下文。库确认、请求值和选择器见 tabs.query.database_picker。本区只登记查询标签里的编辑器与结果区；表数据页的工具条、筛选、网格、分页、导出归 data_page。查询标签可以先以草稿编辑 SQL，显式绑定成功后使用自己的独立会话；换连接时按固定顺序重绑（取消在途查询 → 关游标 → 断开旧会话 → 用目标连接 Connect），关标签时由 worker 先 Cancel 在途查询、再 Close 游标、最后 Disconnect（§7.1 游标的 Close 时机同口径）。v0.2.5 版面收敛：这一页不再常驻 `会话建立中…`（会话行空着就不占位）、`当前为纯文本编辑，语法高亮将在后续版本提供`、`图例：NULL=…`、`已完成 · 已加载 N 行 · 共 N 行`、`语句 1：… · 查询 query_xxx`（摘要改写进日志 `[query] 语句摘要：…`）。只有失败 / 触顶 / 取消 / 断连 / 执行中需要一句原因时，状态行才出现文字。游标的关停时机（§7.1 六条）：重新执行前先关上一个、取消后关、关页后关、读到底由服务端释放、离开页时若有在途执行则取消并关（空闲游标保留，切回来还能续拉）、页面重建 / 窗口关闭时逐个关。8 个独立标签会话限制并发游标的数量。

### `tabs.query.actions` 动作行（连接选择器 · 库选择器 · 执行）

kind `control` · status `active` · parent `tabs.query`

- 样式：尺寸 `gap_md`、`gap_sm`、`min_target`
- 无障碍：可访问名「查询动作」
- 同步：Linux done（tests/ui/linux/results/run.3jtK5EPM/report.json：已绑定 SQL 与行操作；tests/ui/linux/results/run.x16Mog9W/report.json：零连接和未绑定执行禁用；tests/ui/linux/results/run.d98WgmUQ/report.json 状态栏与断连用例通过。）
- 实现备注：页内用于执行 SQL 的按钮为「执行」：可执行时可点，执行中置灰（**不再变成「取消」**，也不再有并排的取消按钮——改动确认单 #7）。取消搬到状态栏行 1 的两用「取消」上（`status_bar.row1.ops.cancel`），与 `Esc`、QA 脚本是同一个方法，可重复点击（第二次起只记日志、不发 RPC）。同一按钮按当前标签的选区实时切换为「执行」或「执行选中内容」，可访问名和 tooltip 同步。有选区时严格执行选中的文本，无选区执行全文。执行后保留文本和选区。未绑定时 SQL 可编辑，执行、事务及会话操作禁用并说明需要绑定连接。会话未就绪、断开或查询执行中禁止执行。动作行左端依次是 `tabs.query.connection_picker`（连接选择器）与 `tabs.query.database_picker`（库选择器），两枚都**没有可见文字**（可访问名「当前连接」/「当前库」，Tab 位不变）；其后是会话信息（有真实会话信息时才占位），右端才是「执行」。执行时把标签的**请求** catalog 带进 `ExecuteQueryRequest.catalog`（空 = 不改变会话当前库）；执行成功后回读一次 `GetSessionContext` 刷新显示。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 会话就绪且空闲，待执行范围内有 SQL | 按选区或全文执行；按钮置灰，等待结果首批 | `src/sql_page.rs:SqlPage::execute_editor_sql` |
| click | 待执行范围为空或只有空白 | 只写一行「SQL 不能为空：请输入后按 Ctrl+Enter」，保留已有结果，不发请求；空白选区也不会执行全文 | `src/sql_page.rs:SqlPage::execute_editor_sql` |
| key Ctrl+Enter | 没有查询在跑 | 与查询页执行按钮使用同一执行范围和状态校验 | `src/sql_page.rs:SqlPage::execute` |
| key Esc | 有查询在跑 | 取消查询：与状态栏行 1 的「取消」同一个方法（发 Cancel、立即进入「已取消」，已取到的行保留；重复按幂等）。空闲时同一按键分流为「清空结果区行选中」 | `src/sql_page.rs:521、src/sql_page.rs:69、src/results.rs:463` |

### `tabs.query.connection_picker` 查询标签的连接选择器

kind `control` · status `active` · parent `tabs.query`

- 样式：尺寸 `gap_xs`、`min_target`
- 无障碍：可访问名「当前连接」
- 原生控件：gtk:GtkDropDown
- 同步：Linux done（tests/ui/linux/results/run.cmSUm8oW/report.json：超长已绑定名可读且窗口仍可缩至 840×600；test_18_unbound_query.py：原生下拉绑定和取消流程。）
- 实现备注：摆放位置是 `tabs.query.actions` 那一行的**最左端**（库选择器之前、会话信息之前）：`parent` 记 `tabs.query` 与 `tabs.query.database_picker` 同一原因（id 已按 `tabs.query.connection_picker` 报备）。**没有可见文字**（改动确认单 #7）：未绑定显示「选择连接…」，下拉框列出已保存档案。绑定在途显示「取消绑定」。列表 = 注册表里**已保存的连接**（连接键 + 公共显示名）；显示名随注册表更新。当前值 = 本标签已结算的连接（`QueryBinding::bound`），由标签写进来。选中**同一条**连接是 no-op：不重建会话、不发任何 RPC（连点也不会多出会话）。目标档案缺少凭据时打开该档案的现有编辑对话框；取消保留草稿及此前的绑定。绑定在途时下拉框置灰并在 tooltip 写「正在把本标签的会话切到「<连接名>」…」：连点不会触发两轮 Connect。连接名空间不足时截断，原生 tooltip 和可访问文本仍提供完整名称。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前标签是查询标签，且选中了**另一条**已保存的连接 | 重绑本标签的会话，顺序固定：取消在途查询（与 Esc 同一个方法）→ 关游标（Close）→ 断开旧会话（Disconnect）→ 用目标连接参数 Connect（与新建标签同一条 `open_tab_session` 路径）。成功后刷新 `GetSessionContext`、把标签请求库**清空**（= 不改会话库）、tooltip 与窗口标题跟随新连接、库下拉候选按目标连接重新取；日志「[tabs] 标签 tab-q1 重绑连接：<旧名> → <新名>」「[tabs] 标签 tab-q1 重绑成功：…」 | `src/sql_page.rs:1581、src/sql_page.rs:1699、src/tabs.rs:3356、src/tabs.rs:2998、src/tabs.rs:563` |
| click | 同上，但目标连接的 Connect 失败（凭据缺失 / 连不上 / 驱动不存在） | **保留原绑定**并按原连接重新建立会话；结果区给出失败原因。恢复原会话或原库失败时保留原归属并标为失连，禁止执行；日志「[tabs] 标签 tab-q1 重绑失败（保留 <旧名>）：<code>」 | `src/tabs.rs:3399、src/sql_page.rs:766、src/sql_page.rs:1699` |
| click | 选中了当前绑定的那条连接 | no-op：不重建会话、不发 RPC，显示回写到绑定值；日志「[tabs] 标签 tab-q1 的连接选择器选中「<名>」= 当前绑定：不重建会话」 | `src/sql_page.rs:1581、src/tabs.rs:3356` |
| click | 有待提交的查询结果变更或活动事务时尝试切换连接 | 提示先处理待提交的结果或活动事务，保留当前会话和已有编辑 | `src/tabs.rs::TabSet::rebind_connection、src/sql_page.rs::SqlPage::has_pending_result_edits` |
| click | 已绑定查询的会话已失效，服务就绪且原会话额度已确认释放 | 页内横幅「重新连接」可申请该标签原档案的会话，按服务端真值恢复库上下文。保留 SQL，需要用户显式执行。 | `src/sql_page.rs::SqlPage::set_reconnect_available、src/tabs.rs::TabSet::rebind_connection` |

### `tabs.query.connection_picker.cancel_binding` 「取消绑定」

kind `control` · status `active` · parent `tabs.query.connection_picker`

- 样式：尺寸 `min_target`、`gap_xs`
- 无障碍：可访问名「取消绑定」
- 原生控件：gtk:GtkButton
- 同步：Linux done（tests/ui/linux/results/run.x16Mog9W/report.json：取消与关闭申请中的标签均会释放真实迟到的 SQLite 会话。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 标签正在申请或切换会话 | 保留 SQL、未保存状态与此前的绑定。回收取消后迟到的会话，清理获确认前保留会话额度。 | `src/tabs.rs::TabSet::cancel_binding、src/backend/worker.rs::TaskUpdate::TabConnected` |

### `tabs.query.database_picker` 查询标签的库选择器

kind `control` · status `active` · parent `tabs.query`

- 样式：尺寸 `gap_xs`、`min_target`
- 无障碍：可访问名「当前库」
- 原生控件：gtk:GtkDropDown
- 同步：Linux done（tests/ui/linux/mariadb_catalog_probe.py：由无默认库档案打开指定库，执行前标题展示已确认的库，SELECT DATABASE() 与同名表数据核对实际目标。隔离 GTK qa_query_target_tracks_the_original_tab_session 验证旧回执不能覆盖实际上下文。）
- 实现备注：摆放位置是 `tabs.query.actions` 那一行的左端、**连接选择器之后**（会话信息之前）：`parent` 记 `tabs.query` 是因为 id 的层级规则要求 `tabs.query.actions.*` 前缀，而本组件的 id 已按 `tabs.query.database_picker` 报备、不再改名。**没有可见文字**（改动确认单 #7）：原来那行「库」caption 已删，界面上只有下拉框本身；层名（库 / Schema）只出现在 tooltip 与可访问名「当前库」里。显示以 `ConnectionService` 返回的实际库上下文为准。没有默认库时显示「未指定库」（ADR 0030 §7）。执行成功后回读真值，刷新不改写请求用的 catalog（§8）。能力级不可用：驱动未在 `ConnectResponse.capabilities` 里声明 `session_catalog_switch` 时**整枚不渲染**（SQLite 的 main / temp / ATTACH 没有「切当前库」这个概念）——不是灰块、不是禁用，是根本没有这枚控件。会话未就绪 / 有未提交事务时**可见但禁用**，并用 tooltip 说明原因（「会话尚未建立：等标签会话就绪后才能切换库」/「事务已打开：先提交或回滚，再切换库」）。事务中切库由服务端返回 FAILED_PRECONDITION（`mariadb.catalog_switch_in_transaction`）。选中后走 `SetSessionContext`，用返回的上下文回读真值并刷新选择器与标签 tooltip；失败走查询页既有的失败提示路径，并把选择器恢复原值——切库失败时语句是按旧库跑的，绝不假装切成功（§6）。列表项 = 可见库，来源于既有 `MetadataService.ListNodes`（目录层：`parent.kind = CATALOG` 且不带名字，一页 500），首次需要时取一次并**按连接键**缓存（同一条连接的多个查询标签共用一份）。取不到时浮层里只给一行说明，不编造条目。标签 tooltip 的库段同步改成真值（无真值写「未指定」），不再写死「默认」；库名不进状态栏（v0.2.3.3）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 新建查询命令捕获了明确的库或 Schema | 支持切换的驱动通过 SetSessionContext 应用目标。其他驱动读取会话当前上下文并核对目标。服务端确认目标后才启用查询操作，并以实际库更新选择器、标题和标签提示。 | `src/tabs.rs::TabSet::on_session、TabSet::update_catalog_context` |
| click | 会话已就绪、没有未提交事务，驱动声明了 session_catalog_switch，且用户在下拉里选中了另一个库 | 发 `SetSessionContext`，切换在途期间暂停会话操作并显示此前的实际库。成功后把标签请求用的 catalog 改成该库（之后执行带上它）、用返回的上下文刷新选择器与 tooltip，日志记「标签 <tab> 已切到库 <db>（服务端真值：<db>）」 | `src/sql_page.rs:1207、src/sql_page.rs:1421、src/sql_page.rs:1343、src/tabs.rs:3076、src/backend/worker.rs:1806、src/backend/client.rs:529` |
| click | 同上，但服务端拒绝切换（事务中 / 无权限 / 库不存在 / 后端未实现） | 查询页显示该失败（`QueryFailure` 的结论 + 后端错误码）；选择器显示恢复成原值（服务端真值），标签请求用的 catalog 不变——不回落、不假装切成功 | `src/tabs.rs:3076、src/sql_page.rs:1411、src/sql_page.rs:982` |
| click | 选择器已渲染且可用，下拉首次展开（该连接还没有库列表缓存） | 按连接取一次 `ListNodes`（目录层）并缓存；取到后把库名填进列表，取不到就在浮层里留一行「取不到库列表：<原因>」，不摆假库名 | `src/tabs.rs:2943、src/tabs.rs:2975、src/sql_page.rs:1371、src/backend/metadata.rs:545` |

### `tabs.query.editor` SQL 编辑器（行号沟 + 等宽文本）

kind `control` · status `active` · parent `tabs.query`

- 样式：尺寸 `gap_md`、`gap_sm`、`font_data_cell`、`query_editor_min_height`、`query_editor_ratio`
- 无障碍：可访问名「SQL 编辑器」；Tab 新建查询标签后自动获得焦点
- 原生控件：gtk:GtkTextView（等宽）
- 同步：Linux done（隔离 AT-SPI：test_03_sql.py 的 test_execute_selection、test_execute_selection_exact_bounds 和 test_whitespace_selection_and_tab_independence 通过。通过原生 menu.popup 打开菜单，点击首项仅返回选中语句的两个结果集；取消选区后没有附加项，纯空白选区时首项禁用。覆盖多行、Unicode、正反选区、部分语句与全文选区。隔离 XTEST 的 test_execute_selection[mouse-menu] 通过：真实右击保留选区边界，反复开关只出现一个附加项，鼠标点击首项只执行选区，取消选区后恢复原生项数。）
- 实现备注：等宽文本、不折行、自动竖向滚动条；没有语法高亮，也不假装有（纯文本）。最小高度随密度档（query_editor_min_height），没有固定上限：编辑器与结果区用可拖动 GtkPaned 分配页高（见 tabs.query.splitter）；新标签结果区隐藏时编辑器占满整页。Linux 的编辑器没有行号沟（只有等宽 TextView）；行号沟是 §7.1 记录的 Windows 口径，本端待收敛。编辑器上方不写「当前为纯文本编辑，语法高亮将在后续版本提供」（v0.2.5 去掉）：没有语法高亮不靠一句自称说明，编辑器就是等宽纯文本。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 新建查询标签成功 | 键盘焦点直接落在编辑器上，可以马上输入 SQL | `src/tabs.rs:1915、src/sql_page.rs:339` |
| key Ctrl+Enter | 编辑器有焦点 | 有选区仅执行选区，无选区执行全文；与动作行共用状态校验 | `src/sql_page.rs:SqlPage::execute` |
| right_click | 编辑器存在非空文本选区 | 首项显示「执行选中内容」，原生编辑项保持原顺序；纯空白选区或不能执行时首项禁用。动作触发时重新读取选区，若选区已失效则忽略。无选区时不显示该项。 | `src/sql_page.rs:SqlPage::connect_execute_controls` |
| load | 新建查询标签（尚无任何 Execute 响应） | 编辑器占满整个内容区：结果区与分隔把手隐藏（GtkPaned 对隐藏子节点不测量也不绘制把手） | `src/sql_page.rs:273、src/sql_page.rs:1362` |

### `tabs.query.state` 结果区状态行（五态）

kind `control` · status `active` · parent `tabs.query`

- 样式：尺寸 `font_status`、`gap_xl`、`spinner_px`；颜色 `warning_color`
- 无障碍：可访问名「查询状态」
- 同步：Linux done（tests/ui/linux/test_18_unbound_query.py：绑定失败后 SQL 保留。tests/ui/linux/test_03_sql.py：查询错误、执行及结果状态。tests/ui/linux/mariadb_catalog_probe.py：拒绝库保留草稿、禁止执行，收到实际会话释放回执。）
- 实现备注：五态互斥且都带稳定文案：执行中带 spinner；空 / 错误 / 取消 / 只读 / 断开各一句，**没有**结果时同一句文案也做占位页的标题与说明。正常完成不再常驻「已完成 · 已加载 N 行 · 共 N 行」（v0.2.5 去掉）：只有失败 / 触顶 / 取消 / 断连 / 执行中需要一句话时才对用户显示。Error / Cancelled 下保留已加载行——状态说「失败」不等于把数据抹掉；触顶（客户端结果集上限）单独一态，不能显示成「查询失败」。Linux 同一状态集合的措辞：「执行中…」/「空结果集 · 共 0 行」/「错误 · 已保留 N 行」/「已取消 · 已保留 N 行」/「已达客户端上限 · 已保留 N 行」/「已断开 · 已保留 N 行」。多语句脚本（#190 / #231）：失败且 trailing metadata 两键齐全时，错误说明前加「前 N-1 条已执行，第 N 条失败」口径（失败语句的错误行写在消息页，见 tabs.query.results.message）；已执行但结果未返回的语句用独立占位态「已执行 · 结果未返回」（Undelivered，与「无结果集」区分：前者本有结果集但后端未保留游标）。每条语句的状态独立回写，切换结果集时状态行随当前呈现的那一个变。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 结果区状态变化（含连接申请失败） | 写一句状态：执行中「正在执行…（等待首批数据）」/ 空「尚未执行」或「空结果集…」/ 错误「查询失败：<文案>（<错误码>）」/ 取消「已取消：保留已取的 N 行（游标已释放，可重新执行）」/ 只读「只读结果：服务端声明该结果集只读」；错误与取消下保留已加载行；连接申请失败保留 SQL、未保存状态与此前确认的绑定。初始库验证失败时说明原因，保留不可执行的未绑定草稿。收到真实关闭回执后归还会话额度。 | `src/results.rs:1370、src/sql_page.rs:617、src/sql_page.rs:643` |
| load | 语句没有结果集（UPDATE / INSERT / DDL） | 不假装有结果：写「影响 N 行」而不是画一张空表（Linux 写「语句 N 影响 N 行（无结果集）」） | `src/sql_page.rs:551、src/sql_page.rs:555` |

### `tabs.query.results` 结果区

kind `container` · status `active` · parent `tabs.query`

- 样式：槽位 `workspace.content`；尺寸 `gap_xl`、`gap_sm`、`radius_md`、`frame_border_width`、`query_results_min_height`；颜色 `sqarvo_frame`
- 同步：Linux done（§5.11 版面收敛；ResultsView::new 的装配（src/results.rs:730）与 `.sqarvo-table-frame`；#189 显隐 src/sql_page.rs:2252（show_results）/ 2265（hide_results）；#231 定稿版式由编排者亲跑 tests/ui/linux/test_03_sql.py 10/10 PASSED（dbus-run-session 私有会话总线 + SQARVO_UI_BIN=worktree debug 二进制 + SQARVO_BACKEND_BIN=backend debug 二进制 + GDK_BACKEND=x11 DISPLAY=:0，隔离 XDG；本次复核再跑一次仍 10/10 PASS）：4 语句脚本可见页签 ["messages","summary","results"]、「结果」页签只 1 枚、默认页 results；20 条多行 INSERT 可见页签 ["messages","summary"]（没有「结果」）、默认页 messages、窗口外框宽 1400 → 1400（`[layout-audit] 1200x800` 与 `960x640` 均「14 项中 0 项超限」，修复前基线 3472）；AT-SPI 子树每个节点恰好 1 份（换原生 gtk::Stack 后 3 refs / 3 distinct）；单测 cargo test 494 passed / clippy --all-targets -- -D warnings 0 警告 / fmt 通过）
- 实现备注：自上而下（#231 定稿版式）：状态行 → 说明行（左侧＝结果区页签 tabs.query.results.views「消息 / 摘要 / 结果」；右侧＝状态圆点 + 「导出 ▾」+ 关闭结果区 ✕，顺序与行为不变——圆点与导出归 data_page / status_bar 的记录，关闭见 tabs.query.results.close）→ 三页内容（消息页 tabs.query.results.message、摘要页 tabs.query.results.summary、「结果」页）。说明行下方**没有**独立控件行：语句切换控件已退役（tabs.query.results.statements，status=removed），语句级导航只剩页签与结果页内的结果集下拉。「结果」页＝改造前的结果区内容原样，顶部多一枚结果集下拉（tabs.query.results.result_picker，仅多结果集时出现）：没有已加载行时把表格换成占位页（标题 + 一句说明），有行时保留表格：取消 / 错误 / 断连之后已加载的行仍可查看；表头 → 行区结构不变（tabs.query.results.header / .grid）。页签不新增一行：gtk::StackSwitcher 插进既有说明行左端，结果区高度零增量（外层 gtk::Stack 以 vhomogeneous=false 按可见页定尺寸；实测说明行高 34，与改造前基线逐字一致，见 sync.linux evidence）；三页随结果区整体显隐（#189 口径），没有独立显示路径。表格外框与表数据页、Objects 列表同一份（frame_border_width + radius_md），圆角靠 overflow=hidden 真正裁到内容；左右各让 gap_xl 与分栏线错开。不摆 `图例：NULL=…` 这类常驻说明（v0.2.5 去掉）：NULL 由单元格自己渲染（斜体 + 降透明度），不靠图例解释。语句摘要（`语句 1：… · 查询 query_xxx`）不常驻界面，改写进日志 `[query] 语句摘要：…（query_id=…）`，排障查日志。显隐（#189）：新标签结果区（含分隔把手）隐藏、编辑器占满整页；任何 Execute 响应（首批 / 续批 / 取消 / 失败 / 触顶）后显示；可用「关闭结果区」按钮主动关闭（见 tabs.query.results.close），关闭后回到隐藏态。显示后按默认页规则落页（见交互），用户之后手动切页不被续批覆盖。最小高度随密度档（query_results_min_height）；显示时编辑器:结果区默认 query_editor_ratio（0.4），可拖动调整（见 tabs.query.splitter）。「结果」页签按**有无结果集**显隐（#231）：任一语句带结果集才出现；全写脚本 / 脚本失败时整页 set_visible(false)，页签条上只剩 消息 / 摘要（不摆一枚永远空的页签）。「结果」页签**恒为 1 枚**，不按结果集拆页签——多结果集在页内折叠（tabs.query.results.result_picker）。多语句脚本（#190 / #231）：每个结果集一份独立网格状态（行模型 / 列 / 已加载行数 / 截断计数 / 游标），换入换出时已加载行不丢；续拉（FetchBatch）跟当前选中的结果集走。默认呈现**第一个**结果集（#190 的「最后一条 SELECT」在定稿版式下退役）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 任何 Execute 响应（show_started / show_batch / mark_cancelled / show_failure / show_limited） | 结果区显示（含分隔把手），默认编辑器占比 query_editor_ratio；已可见时幂等 | `src/sql_page.rs:2252、src/sql_page.rs:840、src/sql_page.rs:950、src/sql_page.rs:1002、src/sql_page.rs:1025、src/sql_page.rs:1095` |
| load | 新建标签，或「关闭结果区」完成 | 结果区隐藏：把手不可见也不可交互（GtkPaned 原生行为），编辑器占满整页；三页签与页内容随整体隐藏 | `src/sql_page.rs:376、src/sql_page.rs:2265、src/sql_page.rs:2378` |
| load | Execute 终态响应到达（首批到达 / 单语句读完 / 写语句完成 / 失败）；取消（mark_cancelled）与触顶（show_limited）只刷新内容、不抢用户当前页 | 按 default_tab_for 落默认页：任意语句有结果集 → 「结果」；全写脚本 → 「消息」；执行失败 → 「消息」并写错误行；还没有任何逐语句结论（防御路径）→ 「结果」。默认页是「结果」但没有结果集时改落「消息」（「结果」页签此时不出现，不能把用户送到不可达的页）。用户之后手动切页不再被覆盖 | `src/sql_page.rs:274、src/sql_page.rs:1241、src/sql_page.rs:871、src/sql_page.rs:914、src/sql_page.rs:933、src/sql_page.rs:1036、src/sql_page.rs:1072` |
| click | 结果区可见（页签在说明行左端） | 外层 gtk::Stack 换页（消息 / 摘要 / 结果）；只换显示内容，不触发查询、不改显隐、不动网格状态 | `src/results.rs:690、src/results.rs:723、src/results.rs:1489、src/results.rs:1495` |
| load | 多语句脚本执行成功且没有任何结果集（全是写语句） | 默认落「消息」页：日志逐条给完整 SQL + `> OK` / `> Affected rows: N` + `> Query Time:`；「结果」页签整页隐藏（页签条只剩 消息 / 摘要），不摆「无结果集」空页签 | `src/sql_page.rs:274、src/sql_page.rs:1241、src/sql_page.rs:1391、src/statements.rs:486` |
| load | 脚本里一个结果集都没有（全写 / 多语句失败） | 「结果」页签 set_visible(false)：页签条上不出现这一枚；若当前正停在「结果」页先把用户放回「消息」，不留不可达的当前页 | `src/results.rs:1377、src/sql_page.rs:886、src/sql_page.rs:1391、src/sql_page.rs:1444` |

### `tabs.query.results.views` 结果区页签（消息 / 摘要 / 结果）

kind `control` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `gap_xl`
- 无障碍：可访问名「结果视图页签」
- 原生控件：gtk:GtkStack + GtkStackSwitcher（add_titled + GtkStackPage.icon-name）
- 同步：Linux done（AT-SPI 只读实测（d781576 换原生容器后复测，本轮定稿版式再验）：1 组 page tab list name='结果视图页签' + 三枚 page tab「消息 / 摘要 / 结果」（selected / pressed 态随当前页），未复现 AdwViewSwitcher 窄/宽两枚同名对象；tests/ui/linux/test_03_sql.py 10/10 PASSED（编排者复跑，跑法见 tabs.query.results evidence）：test_results_view_tabs_exist_and_switch（三页签都在、单语句默认 results、切摘要读到键值、切消息读到完整 SQL + `> OK`、回结果页 3 行）、test_no_results_tab_when_all_writes（全写脚本 page tab 列表恰为 ['消息','摘要']））
- 实现备注：说明行（detail_row）左端三枚页签：「消息」（mail-unread-symbolic）、「摘要」（dialog-information-symbolic）、「结果」（view-grid-symbolic），图标 + 文字——页签标题由 gtk::Stack::add_titled 给、图标由 GtkStackPage.icon-name 给，无 adw Wide/Compact 策略概念；图标名是 Linux 侧系统 symbolic 取值，#232 时 Windows 按文案自取字形——图标选型不属行为承诺，样式人工确认。三页 id 为 messages / summary / results（页面 id 是 QA 断言与日志锚点，不是界面文案）；「结果」页就是既有网格 / 占位页内层 gtk::Stack，顶部再加结果集下拉（tabs.query.results.result_picker），原样搬入。显隐分两层：三页随结果区整体显隐（#189 口径，没有独立显示路径）；「结果」页另按**有无结果集**单独 set_visible(false)（#231 定稿版式）——全写脚本 / 脚本失败时页签条上只剩 消息 / 摘要。消息页与摘要页恒在。「结果」页签恒为 1 枚，不按结果集拆页签。页签容器选型硬约束（#231 改判结论，对三端均有约束力，非事故备忘）：任何页签容器必须以「可访问性子树唯一枚举」为选型门槛——从祖先沿 AT-SPI children 遍历容器时，每个可访问节点必须恰好出现一次（children 引用数 == distinct D-Bus path 数）。实测依据：adw::AdwViewStack + adw::AdwViewSwitcher 装配三页后，ViewStack 为每页生成一个页容器 accessible（AdwViewStackPage，panel），三个页容器都把当前可见页的子树再枚举一遍（3 行网格读成 9 refs / 3 distinct path），既有 test_execute_sql 由 3 虚增到 9，屏幕阅读器与遍历式计数同样被污染；换原生 gtk::Stack + gtk::StackSwitcher 后 3 refs / 3 distinct（每 path 恰好 1 份）。对照实验证明「两层 gtk::Stack 嵌套」无罪，故「结果」页内层 Stack 保留——缺陷在 ViewStack 的页容器对象模型本身，不在嵌套层数。Windows / macOS 选型时同样先按本平台可访问性树遍历验证唯一枚举，不得因观感或 API 便利选择会重复暴露可见子树的容器；页签控件的窄/宽双份同名可访问对象也随本次替换消失（StackSwitcher 只出一组 page tab list + 三枚 page tab）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 结果区可见（页签与说明行同显隐） | 外层 gtk::Stack 切到「消息 / 摘要 / 结果」；功能断言只读当前可见页（实测口径：结果区子树里每个可访问节点恰好枚举一次，3 refs / 3 distinct D-Bus path，见 tabs.query.results evidence） | `src/results.rs:723、src/results.rs:1489、src/results.rs:1495` |
| load | Execute 响应后结果区显示 | 按默认页规则落页（有任意结果集 → 「结果」；全写 / 失败 → 「消息」；取消与触顶不抢用户当前页；默认页是「结果」但没有结果集时改落「消息」） | `src/sql_page.rs:274、src/sql_page.rs:1241、src/results.rs:1489` |
| load | 本轮脚本有没有结果集发生变化（首批 / 多语句首批 / 多语句失败） | 「结果」页 set_visible(有结果集)：没有结果集时页签条上不出现这枚（只有 消息 / 摘要）；当前正停在「结果」页时先切回「消息」 | `src/results.rs:1377、src/sql_page.rs:886、src/sql_page.rs:1391、src/sql_page.rs:1444` |

### `tabs.query.results.message` 消息页（语句日志）

kind `control` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `gap_sm`、`gap_xl`
- 无障碍：可访问名「语句日志」
- 原生控件：gtk:GtkTextView（只读、等宽）∈ GtkScrolledWindow（横向策略 Never）
- 同步：Linux done（tests/ui/linux/test_03_sql.py::test_message_page_format PASS：4 语句脚本（2 多行 INSERT + 2 SELECT）消息页 4 段 / 17 行，首段逐字『INSERT INTO widgets (name, qty)\nVALUES ('qa1', 1)\n> Affected rows: 1\n> Query Time: 0.008 seconds』；断言「> Affected rows: 1」2 处、「> OK」2 处、「> Query Time:」4 处且全部匹配 `^> Query Time: \d+\.\d{3} seconds$`、段间正好 3 处空行、首段是完整 SQL 且全文无「（预览）」；tests/ui/linux/test_03_sql.py::test_multi_insert_message_and_summary PASS：20 条多行 INSERT → 20 段、20 行「> Affected rows: 1」、20 行耗时全部合规；QA 只读探针 `[qa] 消息页全文：…`；纯函数 message_log_text / seconds_text / split_statements 的单测在 src/statements.rs（cargo test 494 passed，编排者复核）；test_execute_selection 的按钮、快捷键和菜单用例均断言消息页只含实际执行的选中语句。）
- 实现备注：每条语句一段，段内顺序固定：**完整 SQL**（按分号切分实际执行的脚本得到的原文，末尾分号去掉）→ 结论行 → 耗时行；段与段之间空一行（`\n\n`）。文本由纯函数 message_log_text 组装，整段替换写进只读 GtkTextView（不按语句追加，避免残留上一轮结论）。结论行三种：查询类 / 已执行成功 → `> OK`（l10n tabs.results_msg_ok）；写语句 → `> Affected rows: N`（l10n tabs.results_msg_affected；影响行数未知时写 `> OK`，不拿 0 冒充「影响 0 行」）；失败 → `> Error: <可安全展示文案>`（l10n tabs.results_msg_error_line，文案与状态行 / tooltip 同源 `QueryFailure::message()`，可诊断但不泄露凭据与连接串，根 §9）。耗时行 `> Query Time: N.NNN seconds`（l10n tabs.results_msg_query_time；整数运算出三位小数，不经浮点）。数据源只有 `StatementSummary.elapsed_millis`（#238）：**0 = 未测量 → 整行不写**，绝不把未测量渲染成 0 秒。语句文本用完整 SQL：前端 split_statements 按分号切分实际执行的脚本（跳过字符串 / 注释里的分号），与后端 statement_index 按位置对齐；切出的条数与 statements.len() 不一致时整条退回后端 sql_preview 并在行首加「（预览）」前缀——绝不错位显示别人的 SQL。已知边界：不处理 MySQL 反斜杠转义 `\'`（按标准 `''` 计），含此类转义的脚本会退回预览。横向策略 Never + 按词换行（WordChar）+ 等宽字体，长 SQL 在页内换行，宽度永不把窗口撑开——这就是本单存在的理由（不撑宽回归条款见 tabs.query.results.statements 的退役背景与本条 sync.linux evidence）。页面可访问名「语句日志」挂在滚动容器上（GtkScrolledWindow 的 Label 属性），内层只读 TextView 不带名字：功能用例按「只读 + multi-line + 非 editable 的 text 节点」定位。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | Execute 终态响应到达（首批 / 完成 / 失败 / 取消 / 触顶） | 整段日志一次性重写（按已到达的逐语句结论组装）：每条语句一段「完整 SQL + 结论行 + 耗时行」，段间空一行；断连只刷新已到达结论，不写假终态 | `src/sql_page.rs:1183、src/sql_page.rs:1187、src/results.rs:1476、src/statements.rs:442` |
| load | 执行失败（单语句失败，或多语句失败位置两键齐全） | 失败语句写「> Error: …」行（同一段里跟在它的 SQL 后面），默认页落「消息」；成功脚本不出现任何错误行 | `src/sql_page.rs:1017、src/sql_page.rs:1036、src/statements.rs:402` |
| load | 某条语句的 StatementSummary.elapsed_millis = 0（未测量） | 该语句不写「> Query Time:」行（只有 SQL + 结论行），不显示 0 秒、也不估算 | `src/statements.rs:458、src/statements.rs:397` |

### `tabs.query.results.summary` 摘要页（执行摘要）

kind `container` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `gap_sm`、`gap_md`、`gap_xl`
- 无障碍：可访问名「执行摘要」
- 原生控件：gtk:键值行 Box（键 Label + 值 selectable Label）∈ GtkScrolledWindow（横向策略 Never）
- 同步：Linux done（tests/ui/linux/test_03_sql.py::test_summary_two_columns PASS：4 语句脚本摘要六项 {已执行语句=4, 成功=4, 失败=0, 开始时间=12:08:21, 结束时间=12:08:21, 总耗时=0.017 s}；断言三行计数 4/4/0、开始/结束匹配 `^\d{2}:\d{2}:\d{2}$`、总耗时匹配 `^\d+\.\d{3} s$`，并用可访问性父容器 path 断言「已执行语句/成功/失败」同属一列、「开始时间/结束时间/总耗时」同属另一列（两列不同）；tests/ui/linux/test_03_sql.py::test_multi_insert_message_and_summary PASS：20 条 INSERT → 20 · 20 · 0、结束时间已写、总耗时「N.NNN s」、底部「客户端计时」口径行在；QA 只读探针 `[qa] 摘要取值：…`；纯函数 totals 的单测在 src/statements.rs（cargo test 494 passed，编排者复核）；test_execute_selection 的按钮、快捷键和菜单用例均断言消息页只含实际执行的选中语句。）
- 实现备注：**两列三行**，列内顺序固定：左列 已执行语句 / 成功 / 失败，右列 开始时间 / 结束时间 / 总耗时（两列等宽、值右对齐且 selectable，用户能取走时间文本）。键 dim-label，未到结论的项显示占位「—」（l10n tabs.results_summary_pending）。计数由纯函数 totals：已执行 = 已有逐语句结论的条数（成功脚本 = 全量条目；失败脚本 = 到失败语句为止），失败 = 失败态条数，成功 = 其余。计时口径（决策 D3，明示限制）：**客户端计时**（提交 Execute → 到达终态，含 IPC 与渲染），不代表数据库服务端执行时间；**逐语句**的数据库侧耗时是消息页的 `Query Time`（协议 `StatementSummary.elapsed_millis`，#238）。页面底部一行写明这条口径（l10n tabs.results_summary_timing_note）。时间为本地墙钟 HH:MM:SS（秒精度），总耗时为 `HH:MM:SS` 之外的「N.NNN s」（l10n tabs.results_summary_duration_value，三位小数）；后到的终态覆盖先到的结束时刻；断连不改结束时间。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | Execute 提交（客户端计时开始） | 记录开始时间；结束时间与总耗时保持占位「—」，直到终态到达 | `src/sql_page.rs:1150、src/sql_page.rs:1183` |
| load | Execute 终态响应到达（首批 / 完成 / 失败 / 取消 / 触顶） | 六项一次性写入（计数取逐语句条目、时间取客户端钟），按两列三行落到固定位置；断连只刷新计数与已完成项，不改结束时间 | `src/sql_page.rs:1164、src/sql_page.rs:1188、src/sql_page.rs:1211、src/results.rs:1482、src/statements.rs:486` |

### `tabs.query.results.header` 结果表头（列名 · 原始类型）

kind `control` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `column_header_min_height`、`font_ui`、`font_weight_title`
- 无障碍：可访问名「结果列头」
- 同步：Linux done（§7.1 结果网格的列头口径；results.rs set_columns 的标题拼装）
- 实现备注：一列一个字头「列名 · 原始类型」；没有类型信息时只写列名，不编造类型。表头只作展示：点列头不改排序（排序只在表数据页的筛选面板里改，查询结果没有表头排序路径）。多语句脚本（#190）：v0.1「只显示第 1 段」限制两端都已退役——表头随当前呈现结果集的列元数据重建，逐语句切换走 tabs.query.results.result_picker；Windows 说明位只保留「结果集 K 段」（见 platform_notes.windows）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 首批数据到达 | 按列元数据重建列：每列一个表头「列名 · 原始类型」（无类型信息时只写列名）；多语句脚本按当前呈现结果集的列重建（Linux 见 tabs.query.results.result_picker） | `src/results.rs:1581、src/sql_page.rs:896、src/sql_page.rs:1532` |

### `tabs.query.results.grid` 结果行区（虚拟化表格）

kind `control` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `row_height_data`、`row_number_width`、`column_width`、`font_data_cell`、`font_weight_cell`
- 无障碍：可访问名「查询结果」；Tab 可聚焦；Esc 清空行选中（执行中的 Esc 是取消查询）
- 自绘：WinUI 3 的 GridView 不支持「虚拟化 + 列宽 + 行号」三者组合，ItemsControl 的虚拟化不带列宽拖动；结果区用自绘虚拟化网格，只建视口内的行、行号跨批次连续（§7.1）。
- 同步：Linux done（§7.1 结果网格的虚拟化口径；GtkColumnView + RowStore 与 Esc 清选中）
- 实现备注：虚拟化：只建视口内的行，滚动时不把整批行都做成控件；行号列跨批次连续（1 起、右对齐），宽表靠横向滚动而不是把列压碎。行号列与数据列的实际宽度目前是代码常量（行号 72、数据列 220，未进 metrics：metrics 里是 row_number_width / column_width），P1 收敛。取消 / 错误 / 断连之后不清空行区：已加载的行保留，状态行说明为什么停在这里。没有行级脏色 / 删除线之外的额外标记（查询标签的单表编辑归 §5.10，脏行数只在状态行与状态圆点）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key Esc | 结果区有选中行、没有查询在跑 | 清空行选中；执行中的 Esc 仍分流为「取消查询」 | `src/results.rs:463、src/results.rs:465` |

### `tabs.query.results.more` 续拉（滚动到末尾自动续拉 / 「加载更多」）

kind `control` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `min_target`
- 无障碍：可访问名「加载更多」
- 同步：Linux done（§7.1 续拉口径的 Linux 实现（滚动预取 + 单批在飞）；maybe_prefetch）
- 实现备注：每批行数由服务端分批返回；同一时刻最多一批在飞，触顶（客户端结果集上限）之后不再续拉。Linux 只有「滚动到接近末尾就自动续拉」这一条路径（没有单独的「加载更多」按钮）；§7.1 记录 Windows 另有可点按的「加载更多」入口。续拉是追加，不重跑 SQL、不换页；如果用户已经滚到底而服务端还有数据，状态行仍是「加载中 · 已加载 N 行」。多语句脚本（#190 / #231）：续拉按当前呈现的结果集（FetchBatch 带 statement_index）；每个结果集独立游标（page_token / has_more / 已加载行数），切走再切回从各自的游标继续；尚未首拉的结果集语句在选中时以空 page_token 发起第一批；整查询的 Close 等 FetchBatchResponse.complete（#187 语义：整查询交付完毕）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| scroll | 结果仍在分批读取（has_more），且滚动到接近末尾 | 自动发一次 FetchBatch 续拉下一批；同一时刻最多一批在飞，触顶后不再拉（暂停原因写日志） | `src/results.rs:1537、src/results.rs:1253` |
| load | 服务端返回的批次读到底（has_more=false） | 不再续拉：行区留在「已完成」，状态行回到正常态（不再常驻「已加载 N 行」） | `src/results.rs:1370、src/sql_page.rs:589` |

### `tabs.query.results.statements` 语句切换控件（多语句脚本）

kind `control` · status `removed` · parent `tabs.query.results`

- 样式：尺寸 `gap_sm`、`gap_xl`、`min_target`、`column_width`
- 无障碍：可访问名「语句切换行」
- 原生控件：gtk:—（v0.2.3.4 起不再渲染）
- 同步：Linux removed（代码已删：src/statements.rs 现在只剩纯规则（split_statements / entries_from_statements / message_log_text / totals），无 GTK 依赖；`StatementSwitcher` 与 results.rs 的切换控件槽位一并移除。tests/ui/linux/test_03_sql.py::test_multi_statement_does_not_widen_window 断言结果页面板里没有任何列出语句的 combo box（`[A.nm(c) for c in A.query(page, role_in=('combo box',))]` 里没有 INSERT 条目），且页签组宽在窗口宽之内）
- 实现备注：生命周期：v0.2.3.4 起退役。结果区不再有「页签下方那一行」——说明行（detail_row）下方直接是三页内容；多语句脚本的逐语句信息全部改由「消息」页承载（tabs.query.results.message：完整 SQL + `> OK` / `> Affected rows: N` / `> Query Time: …`），结果集切换改由结果页内的下拉承担（tabs.query.results.result_picker）。退役原因（保留记录）：该控件是 #231 撑宽缺陷的根因载体——按钮行既没有省略也没有横向滚动，多行语句条目把最小宽度经 GtkPaned 上抛成窗口最小宽度（基线实测把窗口撑到 3472）。改成固定宽度下拉后不再撑宽，但定稿版式进一步把它整行删掉：语句条目不需要任何独立行。曾用形态（历史）：Linux 先是 GtkBox + GtkToggleButton 按钮行，后改固定宽度 GtkDropDown + GtkStringList（宽高必须在同一次 set_size_request 里给全）；Windows 是 #190 的 ScrollViewer + StackPanel + Button 按钮行，且仍在代码里（sync.windows = present，待清理）。行为去向：条目文案「序号 · 类型词 · 行数/影响行数/状态」不再出现；同样的事实改在消息页按语句分段给出（失败语句写 `> Error: …`、影响行数写 `> Affected rows: N`）；「已执行 · 结果未返回」等占位态仍由结果页占位页表达（见 tabs.query.state）。

### `tabs.query.results.result_picker` 结果集选择（多结果集折叠）

kind `control` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `gap_xl`、`gap_sm`
- 无障碍：可访问名「结果集」
- 原生控件：gtk:GtkDropDown + GtkStringList
- 同步：Linux done（tests/ui/linux/test_03_sql.py::test_result_picker_folds_multiple_result_sets PASS（4 语句脚本 = 2 INSERT + 2 SELECT）：结果页出现 combo box「结果 1」，弹层条目恰为 ['结果 1', '结果 2']，默认结果 1 网格 1 行；切到「结果 2」后下拉 name='结果 2'、网格 2 行；切回「结果 1」仍只有它自己那 1 行（各自网格状态保留）。test_results_view_tabs_exist_and_switch 另断言单个结果集时没有这枚下拉。QA 只读探针 `[qa] 结果区版式：… 结果集下拉=True（2 项，选中 Some(1)）…`；AT-SPI 实测 name=选中条目文本、description 为空、combo box 无 action（换条目靠弹层 list 的 Selection + default.activate），定位口径与偏差见 impl_notes 与契约 §9.1）
- 实现备注：位置：「结果」页（tabs.query.results.views 的 results 页）内顶部、网格 / 占位页之上；不是独立一行、不在说明行里，也不按结果集拆页签——「结果」页签恒为 1 枚（维护者确认 #2）。显隐：结果集个数 N ≥ 2 才出现（N == 1 不渲染：一个结果集无需选择；N == 0 时「结果」页签本身不出现，控件随页隐藏）。条目文案「结果 {number}」（l10n tabs.results_result_picker_item）按**脚本顺序**编号：结果 1 是脚本里第一个带结果集的 SELECT，编号不是语句下标；模型是原生 GtkStringList，条目不支持逐项着色。默认选中第一个结果集（#231 定稿版式；#190 的「最后一条 SELECT」已退役）。程序化同步选中带防回环标志，不触发用户回调。换入换出：每个结果集一份独立网格状态（行模型 / 列 / 列头 / 已加载行数 / 截断计数 / 游标），切换时换入换出、已加载行不丢；续拉（FetchBatch）带当前结果集的 statement_index，切走再切回从各自游标继续；尚未首拉的结果集选中时以空 page_token 发起第一批。可访问性实测（GTK 4.22，AT-SPI）：这是一枚 combo box，但 **name = 当前选中条目文本**（实测「结果 1」），代码里的 update_property(Label,「结果集」) 不上 name、也没有 description（未设 tooltip）；展开后的弹层是 list box → list → list item，条目本身没有 click 动作。功能用例定位口径：role=combo box + name 前缀「结果 」；换条目走可访问性激活（选中 list item 后对弹层容器发 default.activate），不用坐标点击。产品口径的 a11y.name「结果集」与实测的偏差登记在契约 §9.1，不由注册表单方面改成实测值。Windows：结果集折叠未接入（见 #232）——Windows 仍是 #190 的独立按钮行（tabs.query.results.statements，sync.windows = present 待清理）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | Execute 首批到达，结果集个数 N ≥ 2 | 下拉出现并选中「结果 1」（默认呈现第一个结果集）；N == 1 时不出现，N == 0 时随「结果」页签一起隐藏 | `src/results.rs:1314、src/results.rs:1332、src/sql_page.rs:1391、src/sql_page.rs:1392` |
| click | 在下拉里选中另一个结果集（弹层点选或键盘选择） | 换入该结果集的行模型与列（已加载行不丢），状态行随选中结果集；尚未首拉的结果集立即发起第一批（空 page_token + statement_index）；续拉跟当前选中结果集；日志「[query] 结果集下拉：选中结果 k」 | `src/results.rs:796、src/sql_page.rs:433、src/sql_page.rs:1459、src/sql_page.rs:1474、src/sql_page.rs:1506` |
| load | 程序化同步（换入换出 / 重建条目后回写选中） | 下拉选中跟着当前呈现的结果集走，防回环标志挡住回调：不重复换模型、不重复发请求 | `src/results.rs:1336、src/sql_page.rs:1541` |

### `tabs.query.splitter` 查询分隔把手（可拖动分隔）

kind `control` · status `active` · parent `tabs.query`

- 样式：尺寸 `query_splitter_width`、`query_editor_min_height`、`query_results_min_height`、`query_editor_ratio`；颜色 `sqarvo_separator`
- 无障碍：可访问名「（原生 GtkPaned 分栏，无独立可访问名）」
- 原生控件：gtk:GtkPaned（vertical）
- 自绘：Linux 用原生 GtkPaned（vertical，L0）；WinUI 3 没有原生 splitter，Windows 自绘 Panel 把手（QuerySplitGrip，L2 复合件）：PointerPressed/Moved/Released 调 RowDefinition 高度，两端最小高度与默认比例读生成常量，同 #178 列宽拖拽口径。
- 同步：Linux done（src/sql_page.rs:259-299（GtkPaned 装配，本轮未改）/ 383（拖动回写）/ 1856-1870（比例应用与日志）；src/theme.rs:905（把手 CSS，确认单 #1：普通模式 background: transparent、HC @sqarvo_separator，min-height=query_splitter_width 热区不变）；单测 theme::tests::query_splitter_is_invisible_except_in_high_contrast 通过（cargo test 479/479）；#189 隔离 QA 日志「分隔位置应用 position=239（页高 603，占比 0.40）」+ AT-SPI 只读遍历（分栏容器 role=generic 暴露 CurrentValue、把手 role=separator）；热区拖动为本机 GUI 实测（逻辑未改，显示由单测断言））
- 实现备注：纵向 GtkPaned 替换旧固定 Separator（#189）：结果区隐藏时 GtkPaned 不测量也不绘制把手，编辑器占满整页；两端都可见时把手厚度来自主题 CSS（query_splitter_width）。确认单 #1（显示收敛）：普通模式把手**不绘制**——Linux CSS background 透明、Windows 中间 1px Rectangle Collapsed；query_splitter_width 厚度只作拖动热区，命中范围与改前逐像素一致。视觉边界只剩编辑器卡片底框与结果区的自然连接处；可发现性靠悬停热区时的系统 resize 光标（Linux GtkPaned 自带、Windows QuerySplitGrip 常驻 SizeNorthSouth），**不加 hover 显形**（决策已确认）。高对比例外保留静态分隔线（§8.6 HC 分支，HC 靠描边划区域边界）：Linux 按 stylesheet_for 的 high_contrast 参数出线；Windows 装配时按 ThemeService.HighContrast 决定 Rectangle 显隐，运行期切 HC 不即时刷新、重启生效（现有 SystemAppearanceChanged 链路只重设 RequestedTheme，覆盖不到该分支）。GtkPaned 的 min/max-position 只读：两端最小高度由子控件自身最小尺寸 + shrink=false 保证（编辑器 min_content_height=query_editor_min_height，结果区 size_request=query_results_min_height）。默认比例 query_editor_ratio（0.4，三档同值）；拖动回写只进内存（split_position），不持久化，下次显示时恢复；applying_split 防止程序化应用位置被记成拖动。GTK4 的 size-allocate 是虚函数不是信号：用结果区 map 信号 + 首次分配守卫在分配就绪后应用比例；首次分配前的 position 通知是 GtkPaned 钳制，不是用户拖动，不记录。密度档切换后 reapply_metrics 重应用两端最小高度与比例；把手厚度随主题 CSS 重建自动更新。可访问性现状（隔离 QA 只读 AT-SPI 遍历实测，GTK 4.20.3）：已按 §8.1 用 update_property 给 GtkPaned 容器挂 Label「查询编辑器与结果区分栏」，但该 GTK 版本的 AT-SPI 桥不暴露此名（容器 role=generic、Name 空）；容器实现 GtkAccessibleRange，暴露 CurrentValue=分隔位置（实测 239）；原生把手 role=separator、无独立名字。P1 跟进：分栏的可访问命名与键盘调节语义。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| drag | 结果区可见（热区可命中；普通模式不可见，HC 下可见静态线） | 调整编辑器 / 结果区高度占比，回写内存态（不持久化）；两端最小高度不被突破（shrink=false） | `src/sql_page.rs:386` |
| load | 结果区首次显示（map 信号，分配就绪） | 应用默认比例 query_editor_ratio；有拖动回写则恢复用户拖到的位置 | `src/sql_page.rs:396、src/sql_page.rs:1916` |

### `tabs.query.results.close` 关闭结果区按钮

kind `control` · status `active` · parent `tabs.query.results`

- 无障碍：可访问名「关闭结果区」
- 原生控件：gtk:GtkButton（window-close-symbolic，flat）
- 同步：Linux done（src/results.rs:450-457（按钮构造 + a11y）；src/sql_page.rs:1277（执行中置灰）/ 1377（丢弃确认）/ 1424（关闭）；#189 隔离 QA AT-SPI 只读遍历 name='关闭结果区' role=button + 截图 189-04（说明行右端 ×））
- 实现备注：位置：结果区说明行（detail_row）右端、「导出 ▾」右侧（#189）；原生 symbolic 图标 window-close-symbolic + flat（L0），无可见文字，可访问名「关闭结果区」，tooltip「关闭结果区（释放查询资源）」。语义 = Close RPC（幂等，复用既有客户端路径）+ 清 query_id + 重置编辑态 + 重置结果区 + 隐藏并让编辑器占满整页；编辑器里的 SQL 文本保留。状态性不可用保持可见（AGENTS.md §8.1）：执行中（Loading / Streaming）置灰 + tooltip「查询执行中，请先取消」；空闲恢复可用。有脏行（dirty_rows()>0）先弹丢弃确认：「放弃未提交的更改？」正文「关闭结果区将丢弃 N 行未提交的更改。」，按钮 取消 / 放弃更改（破坏性、默认取消、关闭=取消），只接受第一次 response——与数据页 AlertDialog 同一口径；QA 自动应答复用 respond_dialog（只在 QA 参数下启用，不是产品路径的后门）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 结果区可见、非执行中、无脏行 | 释放游标（Close RPC）并隐藏结果区，编辑器占满整页；日志「结果区已关闭：游标已释放，编辑器占满整页」 | `src/sql_page.rs:1421、src/sql_page.rs:1468、src/sql_page.rs:1482` |
| click | 有脏行（dirty_rows()>0） | 先弹「放弃未提交的更改？」（取消 / 放弃更改，破坏性、默认取消）；确认放弃才关闭，取消则保留结果区 | `src/sql_page.rs:1421、src/results.rs:1202、src/data_page.rs:5345` |
| load | 查询执行中（Loading / Streaming） | 按钮置灰，tooltip「查询执行中，请先取消」；空闲恢复可用，tooltip 回到「关闭结果区（释放查询资源）」 | `src/sql_page.rs:1321` |

### `tabs.close_dialog` 关闭标签确认框

kind `control` · status `active` · parent `tabs`

- 样式：槽位 `dialogs`；尺寸 `badge_min_height`
- 无障碍：可访问名「关闭标签？」
- 同步：Linux done（tests/ui/linux/results/run.x16Mog9W/report.json：空草稿直接关闭，有 SQL 时取消/放弃，执行成功后仍保护；tests/ui/linux/results/run.3jtK5EPM/report.json 已绑定 SQL 与提交回归。）
- 实现备注：在有未保存 SQL、脏行、待提交结果、活动事务或执行中的查询时出现；默认响应与关闭响应都是「取消」（关闭窗口的叉不会变成放弃）。正文列出关闭的实际影响；未绑定 SQL 的放弃确认只说明将丢失草稿。被 `close-page` 信号拦下的那次关闭必须由 `close_page_finish(page, confirm)` 收尾：不回话页面会停在「正在关闭」，之后再点关闭毫无反应。按钮的破坏性 / 建议档：放弃（或回滚）是破坏性、保存是建议、取消是默认；数据页只有事务没有脏行时只给「回滚事务并关闭」+「取消」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 关闭有需要保护的内容或活动会话操作的标签 | 标题「关闭标签？」。查询页列出未保存 SQL、结果更改、事务和执行影响，提供取消与放弃。数据页继续提供原来的脏行或事务确认。 | `src/tabs.rs:256、src/tabs.rs:2729` |

### `tabs.close_dialog.discard` 「放弃更改并关闭」/「回滚事务并关闭」

kind `item` · status `active` · parent `tabs.close_dialog`

- 无障碍：可访问名「放弃更改并关闭」
- 同步：Linux done（tests/ui/linux/results/run.d98WgmUQ/report.json：test_03_sql.py 多个场景经原生对话框放弃 SQL；tests/ui/linux/results/run.x16Mog9W/report.json 关闭绑定中页面的资源清理。）
- 实现备注：有脏行或需放弃 SQL 时文案是「放弃更改并关闭」；只有事务没有脏行时是「回滚事务并关闭」（关闭 = 回滚，不用「放弃」吓人）。两个文案都是破坏性档（destructive），默认焦点不落在它们上。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 确认框在显示 | 丢弃本地草稿与新增行（有事务时随会话断开由后端回滚），再关页面：被拦下的那次关闭用 `close_page_finish(page, true)` 收尾 | `src/tabs.rs:2790、src/tabs.rs:2802、src/tabs.rs:2806、src/tabs.rs:2713` |

### `tabs.close_dialog.save` 「保存并关闭」/「保存、提交事务并关闭」

kind `item` · status `active` · parent `tabs.close_dialog`

- 无障碍：可访问名「保存并关闭」
- 同步：Linux done（§4 保存并关闭；advance_close 的两段相位与失败保留）
- 实现备注：有事务时文案写「保存、提交事务并关闭」：在事务里只提交脏行就断开，服务端会把事务回滚 —— 用户以为「保存了」，实际什么都没留下。顺序固定：先提交脏行 → 若标签有未提交事务再提交事务 → 最后才摘页面；任一步失败都保留标签并在页内说明原因。建议档（suggested），但不做默认响应（默认是取消）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 确认框在显示且有脏行 | 先提交脏行 → 再提交标签自己的事务 → 最后摘页面；提交失败时标签保留，页内提示「保存失败：事务处于「<标签>」，标签未关闭。请处理事务后再关闭。」 | `src/tabs.rs:2808、src/tabs.rs:2359、src/tabs.rs:2401` |

### `tabs.close_dialog.cancel` 「取消」

kind `item` · status `active` · parent `tabs.close_dialog`

- 无障碍：可访问名「取消」
- 同步：Linux done（tests/ui/linux/results/run.DmA8Fxr9/report.json：草稿关闭取消。tests/ui/linux/results/run.ID18uRL0/report.json：关闭取消后同时保留 SQL、待提交结果行及提交能力。tests/ui/linux/results/run.TIP6OYEv/runner.log：延期重绑的取消交接。）
- 实现备注：默认响应与窗口关闭响应都是它：误按 Esc / 点框外不会丢掉未提交的更改。取消同样要给被拦下的关闭收尾（`close_page_finish(page, false)`），否则页面停在「正在关闭」、后续关闭请求全部失效。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 确认框在显示 | 标签与会话都保留，SQL、脏行与事务原样；这次被拦下的关闭请求收尾为「不关」（`close_page_finish(page, false)`） | `src/tabs.rs:2790、src/tabs.rs:2799、src/tabs.rs:2713` |

## 工作区 MCP 策略（`workspace`）

MCP 管理页的服务器、策略和 Agent 接入，以及启动态和已连接工作区状态栏指示。

| 组件 | 名称 | kind | 状态 | Linux |
|---|---|---|---|---|
| `workspace` | 工作区 MCP 策略 | area | active | done |
| `workspace.manage.menu.mcp_access` | 　　　MCP 数据访问 | item | active | done |
| `workspace.manage.menu.mcp_readonly` | 　　　MCP 只读 | item | active | done |
| `workspace.status.mcp_indicator` | 　　MCP 访问状态 | control | active | done |
| `workspace.mcp_management` | 　MCP 管理 | container | active | partial |
| `workspace.mcp_management.service` | 　　HTTP 服务 | control | active | done |
| `workspace.mcp_management.agent` | 　　Agent 接入 | control | active | done |
| `workspace.mcp_management.token` | 　　访问令牌管理 | item | active | done |

### `workspace` 工作区 MCP 策略

kind `area` · status `active`

- 同步：Linux done（tests/ui/linux/test_06_mcp_access.py 和 GTK --qa-mcp-* 验证真实交互。）

### `workspace.manage.menu.mcp_access` MCP 数据访问

kind `item` · status `active` · parent `workspace`

- 无障碍：可访问名「MCP 数据访问」；Tab MCP 管理页的权限管理组首项
- 原生控件：gtk:AdwActionRow + GtkSwitch
- 同步：Linux done（tests/ui/linux/test_06_mcp_access.py::test_mcp_access_menu_and_live_status 验证确认、revision 与安全默认值；test_07_mcp_http.py 通过管理页按钮启用与撤销。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已连接，策略读取成功，当前没有策略操作在途 | 启用时先显示档案数与授权范围，须确认。关闭时恢复安全默认值。勾选态以 gRPC 回读为准。取消不写入。 | `src/mcp_management.rs、src/mcp_access.rs` |

### `workspace.manage.menu.mcp_readonly` MCP 只读

kind `item` · status `active` · parent `workspace`

- 无障碍：可访问名「MCP 只读」；Tab MCP 管理页的 MCP 数据访问之后
- 原生控件：gtk:AdwActionRow + GtkSwitch
- 同步：Linux done（tests/ui/linux/test_06_mcp_access.py::test_mcp_read_only_confirmation 验证确认、取消、回读；test_07_mcp_http.py 验证管理页只读按钮唤起确认框。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 数据访问已启用，策略读取成功，当前没有策略操作在途 | 关闭只读时先显示档案数与风险，须确认。恢复只读直接提交。勾选态以 gRPC 回读为准。访问关闭时保留禁用项，原因见状态提示。 | `src/mcp_management.rs、src/mcp_access.rs` |

### `workspace.status.mcp_indicator` MCP 访问状态

kind `control` · status `active` · parent `workspace`

- 样式：槽位 `status_bar.row2`
- 无障碍：可访问名「根据策略读取结果显示的状态文本」；Tab 状态文本不进入 Tab 顺序
- 原生控件：gtk:GtkLabel
- 同步：Linux done（tests/ui/linux/test_06_mcp_access.py 验证状态的可访问名称，以及 CLI 开关变更后打开主菜单刷新状态。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 启动态或已连接工作区 | 常驻显示访问关闭、仅允许读取或只读已关闭，并反映加载、策略错误和断开状态。只读关闭时明确提示已授权的 Agent 可执行受控写入。 | `src/shell.rs:1271、src/mcp_access.rs:271` |
| click | GTK 窗口已连接后端，用户打开主菜单 | 从 gRPC 重新读取访问和只读策略，更新菜单及状态；反映外部 CLI 已提交的变更。 | `src/ui.rs:338、src/mcp_access.rs:123` |

### `workspace.mcp_management` MCP 管理

kind `container` · status `active` · parent `workspace`

- 无障碍：可访问名「MCP 管理」；Tab 打开后依次浏览 HTTP 服务、权限管理、Agent 接入、访问令牌管理
- 原生控件：gtk:AdwDialog + AdwPreferencesPage
- 同步：Linux partial（已接入原生关闭按钮，待在 GNOME Wayland 会话中完成 AT-SPI 验收。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 从 MCP 一级入口打开 | 回读管理配置、服务状态与数据库权限。后端断开时展示状态不可用。 | `frontend/linux/src/mcp_management.rs` |
| click | MCP 管理页已打开，点击标题栏原生关闭按钮 | 关闭 MCP 管理页。标题栏仅提供一个关闭按钮。 | `frontend/linux/src/mcp_management.rs` |

### `workspace.mcp_management.service` HTTP 服务

kind `control` · status `active` · parent `workspace.mcp_management`

- 无障碍：可访问名「HTTP 端口 / 复制 MCP 地址 / 保存端口 / 启动 HTTP 服务 / 停止 HTTP 服务」；Tab 管理页第一组，端口和地址复制同一设置行
- 原生控件：gtk:AdwActionRow + GtkEntry + GtkButton + GtkLabel
- 同步：Linux done（AT-SPI test_06_mcp_access.py 验证端口冲突；test_07_mcp_http.py 验证端口保存、启停和停服时禁用地址复制。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端连接已就绪。保存端口需要已停止。 | 服务按后端确认展示启动中、运行、停止中、已停止或失败，并展示回环地址和端口冲突原因。启停只显示当前对应操作，操作在途时不可重复提交。端口有效且变化才可保存。地址复制仅在后端返回实际监听 URL 时可用。 | `frontend/linux/src/mcp_management.rs` |

### `workspace.mcp_management.agent` Agent 接入

kind `control` · status `active` · parent `workspace.mcp_management`

- 无障碍：可访问名「复制提示词（http方式） / 复制提示词（stdio方式）」；Tab 管理页第三组
- 原生控件：gtk:AdwActionRow + GtkButton + AdwToastOverlay
- 同步：Linux done（AT-SPI test_06_mcp_access.py 验证旧入口移除；test_07_mcp_http.py 验证两种提示词与复制 Toast。GTK 单测 clipboard_output_uses_actual_listener_and_secret_reference 断言载荷。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 已回读后端状态和配置 | 复制 HTTP 或 stdio 提示词并展示 Toast。HTTP 使用实际监听 URL，stdio 使用验证过的命令。剪贴板不含凭据。 | `frontend/linux/src/mcp_management.rs` |

### `workspace.mcp_management.token` 访问令牌管理

kind `item` · status `active` · parent `workspace.mcp_management`

- 无障碍：可访问名「访问令牌管理」；Tab 管理页第四组
- 原生控件：gtk:AdwActionRow + AdwAlertDialog
- 同步：Linux done（test_07_mcp_http.py 验证管理入口、取消保留旧令牌、确认后重新生成和撤销。）

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已就绪 | 选择重新生成或撤销，再在独立确认框中确认。取消不发送操作。界面不展示或复制令牌。 | `frontend/linux/src/mcp_management.rs` |

## 待清理（按版本）

退役组件待所有 Active 平台清理或标明不适用后归档。

| 版本 | 组件 | 名称 | status | Linux | 理由 |
|---|---|---|---|---|---|
| v0.2.3.4 | `data_page.toolbar.commit` | 「提交」按钮（已移除；提交由状态栏「应用」承担） | removed | 已清 | 提交入口收敛到状态栏行 1「应用（✓）」（status_bar.row1.ops.apply，与 Ctrl+S 同一路径），工具条不再放第二个提交按钮；在途取消也由状态栏「取消」承担。条目保留到三端清理完成。 |
| v0.2.3.4 | `tabs.query.results.statements` | 语句切换控件（多语句脚本） | removed | 已清 | 整体并入消息页：逐条语句的完整 SQL + 结论行 + 耗时行写在「消息」页，结果集切换改由结果页内的下拉承担，独立控件行随之删除，避免额外一行与横向撑宽风险（维护者范围修订「确认 #2」，Issue #231） |
| v0.2.4 | `status_bar.gutter` | 左边沟（分栏线左侧整块） | removed | 已清 | 导航搜索已移入导航侧栏，状态栏全宽。 |
| v0.2.5 | `object_tree.tree.row.field` | 列 / 索引 / 触发器行 | removed | 已清 | 表和视图保留为导航叶节点。字段详情仍从数据表列头打开。索引和触发器的分页、重试和 SQLite 触发器原文 DDL 的查看与复制迁入 Inspector 表信息；原单组刷新由显式刷新表信息承接。 |
| v0.2.5 | `shell.menu.export_csv` | 表 → 导出 CSV | removed | 已清 | 主菜单暂时隐藏（改动确认单 #1 / #198）：导出入口收敛到表工具条「导出 ▾」与 Ctrl+Shift+E；条目保留，恢复时翻回 active |
| v0.2.5 | `shell.toolbar.functions` | 种类入口「Functions」 | removed | 已清 | 退役打开未实现说明页的 Functions 占位入口。已有实现且目标支持时可再引入实际管理命令。 |
| v0.2.5 | `shell.toolbar.queries` | 种类入口「Queries」 | removed | 已清 | 退役打开未实现说明页的 Queries 占位入口。已有实现且目标支持时可再引入实际管理命令。 |
