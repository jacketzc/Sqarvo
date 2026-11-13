# UI 组件总览

> 本文件由 `scripts/check-ui-sync.sh gen` 生成，源是
> [`components/*.toml`](components/)，**不要手改**。
> 字段与规则见 [`components/README.md`](components/README.md)、
> [ADR 0027](../decisions/0027-ui-component-registry.md)。

共 187 个组件 / 9 个功能区。同步列：`done` 已实现并按本规范核对过，
`partial` 部分实现（差异见组件明细的「平台」），`—` 未实现，`na` 该平台不做，
`blocked` 平台能力做不到；`已清` / `待清` 只用于退役组件，是该端「代码删了 / 界面隐藏了」
与「还留着」的清理进度（见文末「待清理」）。

## 表数据页（`data_page`）

表数据页（含查询标签复用同一套编辑态）：单行工具条与事务、筛选与排序面板、列显隐、数据网格与就地编辑、脏行标记与批量提交、offset 分页、导出 CSV / INSERT / 转储表、数据行右键菜单、按行浏览与单行「字段 · 值」两种展示形式，以及只读 / 空 / 错误 / 断连四态。

| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |
|---|---|---|---|---|---|---|
| `data_page` | 表数据页（整页） | area | active | done | partial | — |
| `data_page.toolbar` | 　单行工具条 | container | active | done | partial | — |
| `data_page.toolbar.commit` | 　　「提交」按钮（待提交 N 条；在途变「取消提交」） | control | active | done | partial | — |
| `data_page.toolbar.begin_txn` | 　　「开始事务」按钮 | control | active | done | partial | — |
| `data_page.toolbar.commit_txn` | 　　「提交事务」按钮 | control | active | done | partial | — |
| `data_page.toolbar.rollback_txn` | 　　「回滚事务」按钮 | control | active | done | partial | — |
| `data_page.toolbar.status_cluster` | 　　工具条状态簇（spinner · 过程消息 · 状态圆点） | container | active | done | — | — |
| `data_page.toolbar.status_dot` | 　　状态圆点（灰 / 事务棕 / 脏行强调色） | control | active | done | — | — |
| `data_page.toolbar.filter` | 　　「筛选」（漏斗）按钮 | control | active | done | — | — |
| `data_page.toolbar.columns` | 　　「列显隐」（眼睛）按钮 | control | active | done | — | — |
| `data_page.toolbar.columns.card` | 　　　列显隐卡片（复选清单 + 说明） | container | active | done | — | — |
| `data_page.toolbar.export` | 　　「导出 ▾」菜单按钮 | control | active | done | — | — |
| `data_page.export_menu` | 　导出菜单（三项出口） | container | active | done | — | — |
| `data_page.export_menu.csv` | 　　「导出 CSV…」 | item | active | done | — | — |
| `data_page.export_menu.insert` | 　　「导出为 INSERT 语句…」 | item | active | done | — | — |
| `data_page.export_menu.dump` | 　　「转储表…」 | item | active | done | — | — |
| `data_page.badges` | 　表头上方徽标行 | container | active | done | — | — |
| `data_page.grid` | 　数据网格（表格外框 · 行号列 · 表头 · 数据行） | container | active | done | partial | — |
| `data_page.grid.header` | 　　列头（列名 · 原始类型 · 排序标记） | row | active | done | partial | — |
| `data_page.grid.row_number` | 　　行号列（行号 / 脏行 / 标删 / 新行） | cell | active | done | partial | — |
| `data_page.grid.cell` | 　　数据单元格（值 · NULL / 空串 · 脏色） | cell | active | done | partial | — |
| `data_page.editor` | 　单元格编辑器（标题 · 输入框 · 设为 NULL · 应用 / 取消） | container | active | done | partial | — |
| `data_page.row_menu` | 　数据行右键菜单 | container | active | done | — | — |
| `data_page.row_menu.delete` | 　　「删除行」 | item | active | done | — | — |
| `data_page.row_menu.copy_insert` | 　　「复制为 ▸ 复制为 INSERT」 | item | active | done | — | — |
| `data_page.detail` | 　单行「字段 · 值」展示形式 | container | active | done | — | — |
| `data_page.filter_panel` | 　筛选与排序面板（草稿 · 应用 / 清空） | container | active | done | — | — |
| `data_page.filter_panel.condition_composer` | 　　条件编辑组（列 · 算子 · 值 · 添加条件） | container | active | done | — | — |
| `data_page.filter_panel.condition_row` | 　　条件行（列 · 算子 · 值，可就地编辑） | row | active | done | — | — |
| `data_page.filter_panel.sort_composer` | 　　排序编辑组（列 · 方向 · 添加排序） | container | active | done | — | — |
| `data_page.filter_panel.sort_row` | 　　排序键行（列名 · asc / desc · 上移 / 下移 / 删除） | row | active | done | — | — |
| `data_page.txn_chooser` | 　「开始事务」卡片（隔离级别 · 只读） | container | active | done | — | — |
| `data_page.placeholder` | 　占位页（尚未打开 / 正在读取 / 空表 / 本页没有数据） | container | active | done | — | — |
| `data_page.banner` | 　页首横幅（只读 / 断连 / 错误） | control | active | done | partial | — |
| `data_page.errors` | 　逐行提交失败列表 | container | active | done | partial | — |
| `data_page.query_edit` | 　查询结果的单表编辑（复用同一份草稿与提交路径） | container | active | done | — | — |

### `data_page` 表数据页（整页）

kind `area` · status `active`

- 样式：槽位 `workspace.content`；尺寸 `table_actions_height`、`row_height_data`、`grid_padding_start`
- 同步：Linux done（v0.2.5 Linux 客户端；§5.1–§5.10 逐条对照 data_page.rs（begin_open / show_open / 四态））；Windows partial（§7.1 记录数据页（#173）已落地，但版面是「页内动作行 + 状态栏行 2」，没有页内徽标行 / 逐行错误列表 / 占位页；差异见本文件各条目的 windows note。）；macOS none
- 实现备注：自上而下：横幅 → 单行工具条 → 表头上方徽标行 → 逐行错误列表 → 表格外框（网格 / 字段 · 值）→ 占位页。页内浮层只有两块：列显隐卡片与「开始事务」卡片；筛选与排序面板是挂在漏斗按钮上的 popover（不进主窗口排版）。表格外框与 Objects 列表共用同一份 .sqarvo-table-frame（外框线宽 frame_border_width、圆角 radius_lg），圆角靠 overflow=hidden 真正裁到内容。行号列 32、表头 26、数据行 32 都只来自 metrics；页面本身不另存一份数字。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 打开表 / 换表 / 重新读取 | 清空上一屏的草稿、选中、逐行错误与「最近一次操作」注记，退回按行浏览，占位页写「正在读取表数据」+「等待服务端返回第一页…」，工具条消息同步「正在读取表数据…」 | `src/data_page.rs:4228、src/data_page.rs:4261` |

### `data_page.toolbar` 单行工具条

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `table_actions_height`、`row_height_data`
- 同步：Linux done（§5.1 单行工具条；data_page.rs new() 的 controls 行 + --qa-layout-audit「表格工具条 / 状态簇」两项）；Windows partial（§7.1：页内动作行是「上一页 / 下一页 / 加载更多…」+ 编辑动作，没有筛选 / 列显隐 / 导出，也没有状态簇；事务与脏行写在状态栏行 2。）；macOS none
- 实现备注：只有一行：提交 / 开始事务（开事务后原位换成 提交事务 回滚事务）──弹性空白── [spinner · 过程消息 · 状态圆点] 筛选 列显隐 导出 ▾。这一行**没有**「撤销」「删除」「新增」「刷新」：撤销与删除在状态栏行操作组（↶ / −），删除另有行右键菜单，新增只有状态栏 +，刷新只有状态栏 ⟳。四个动作按钮共用一套内距（按钮盒 36 宽 × 32 高）；「开始事务」是中性动作：不挂 flat，也不用 suggested-action / destructive-action。行内容高度钉在 row_height_data，上 8 / 下 6 留白；spinner、消息与圆点全部 valign=Center，与图标按钮同一垂直中线。状态簇紧贴漏斗左侧；960 宽下只有过程消息按省略号收缩，行本身不溢出。

### `data_page.toolbar.commit` 「提交」按钮（待提交 N 条；在途变「取消提交」）

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「提交」；Tab 工具条内第一枚动作按钮
- 同步：Linux done（§5.1 提交；§8.2 提交行变更确认；data_page.rs on_commit_clicked / request_commit / confirm_batch）；Windows partial（§7.1：Ctrl+S / 状态栏「应用（✓）」提交；超过 100 行时不弹对话框，改成「再按一次 Ctrl+S 确认」并带 acknowledge_risk；提交走 ApplyRowChanges（带 transaction_id），全部成功再 Commit。）；macOS none
- 实现备注：文案随待提交条数变化（提交（N），N = 脏行 + 新行草稿 + 标记删除）；在途时同一按钮变「取消提交」，取消中变「正在取消…」——文案由批次状态决定，不在视图层拼。可用性只有一个读数来源（数据页按钮的 sensitive）：状态栏「应用（✓）」直接读它，不重复判定。只读原因一旦成立，本按钮恒禁用，并把原因写进过程消息的 tooltip（不能只靠置灰暗示）。提交顺序：有新行时 `InsertRows` **先行**（整批原子，任一行失败则整批回滚），成功后才提交脏行；插入失败则一条脏行都不提交——避免库里留下半套状态。单批上限 500 行：超出时只执行前 N 条，其余留待下次，横幅「单批上限 500 行：本次只执行了前 N 条，还有 M 条未提交——请再次点「提交更改」续传。」；逐行失败的行留在草稿里，可改完再提交。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 可编辑、有待提交变更、没有 RPC 在途 | 走风险判定后提交：逐行更新与删除按主键定位；按钮文案「提交（N）」，状态行「正在提交 M 条变更（批次 …）：点「取消提交」可在行边界中止」 | `src/data_page.rs:5086、src/data_page.rs:5147、src/submit_batch.rs:434` |
| click | 变更超过 100 行或含删除（本地判定） | 弹「确认提交变更？」：正文给更新 / 删除行数，超过 100 行时追加「变更数超过 100 行的风险阈值…」；按钮 取消 / 确认提交（破坏性，默认取消）；取消则「已取消提交（未发送任何变更）」。服务端要求 acknowledge_risk 时再走同一个对话框 | `src/table_data.rs:489、src/table_data.rs:515、src/data_page.rs:5199` |
| click | 提交在途 | 同一按钮变成「取消提交」且可点：请求取消后显示「正在取消…」，整批回滚、界面保留脏行与编辑文本（不采信任何部分结果），可再次点「提交」重试；取消中 / 已结束的重复点击幂等 | `src/data_page.rs:5115、src/submit_batch.rs:412、src/submit_batch.rs:449` |
| load | 只读 / 已断开 / RPC 在途 / 没有待提交变更 | 按钮禁用（不弹错误）；只读与断连时过程消息的 tooltip 给同一句原因 | `src/data_page.rs:6579、src/data_page.rs:4213` |

### `data_page.toolbar.begin_txn` 「开始事务」按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「开始事务」；Tab 工具条内、紧跟「提交」
- 同步：Linux done（§5.1 开始事务 / §5.2；begin_transaction + begin_tooltip；驱动不支持时落只读原因 capability.unsupported）；Windows partial（§7.1：第一次暂存才 Begin（没有改动就没有事务），没有隔离级别卡片与只读勾选；驱动不支持显式事务时如实退回自动提交。）；macOS none
- 实现备注：打开事务后**隐藏**（不是置灰）：同一位置换成「提交事务 / 回滚事务」。有未提交的更改时置灰，tooltip 给原因「还有 N 行未提交的更改：先提交或放弃它们再开事务」——最容易踩到的那一条优先。页内按钮、win.begin-txn 动作与 QA 都落到 begin_transaction 这同一条命令，被挡住时状态行写同一句话、不打开卡片。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 没有打开的事务、未断开、没有未提交的更改（脏行 / 标记删除 / 新行草稿） | 打开「开始事务」卡片（隔离级别 + 只读勾选），不直接发 RPC | `src/data_page.rs:5368、src/data_page.rs:3643` |
| load | 有待提交的更改 | 按钮置灰，tooltip「还有 N 行未提交的更改：先提交或放弃它们再开事务」；点了不打开卡片，状态行同一句 | `src/data_page.rs:6567、src/data_page.rs:6579` |
| load | 驱动未声明显式事务 / 行编辑能力 | 如实退回自动提交、不假装事务存在：状态圆点与 tooltip 首段仍是「自动提交」，日志与 tooltip 写明 capability.unsupported；Linux 侧同一能力位先落到只读原因上（横幅「只读：驱动未声明行编辑能力」+ 编辑类按钮禁用），事务棕只在事务真的打开时出现 | `src/table_data.rs:952、src/data_page.rs:6705` |

### `data_page.toolbar.commit_txn` 「提交事务」按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「提交事务」；Tab 有事务时出现在工具条动作区
- 同步：Linux done（§5.1 / §5.2 事务按钮互斥与失败态；commit_transaction + sync_badge）；Windows partial（§7.1：Ctrl+S = ApplyRowChanges（带 transaction_id）→ 全部成功再 Commit；没有「提交事务」文字按钮，失败行保留在原处可重试。）；macOS none
- 实现备注：只在有活动事务时可见（与「开始事务」原位互斥）；失败态**不可提交**，只剩回滚。有脏行时拒绝：先「提交」或「放弃更改」，与工具条按钮同一套谓词。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有活动事务且没有脏行 | 提交事务；成功后丢弃尚未提交的界面更改并重新读取当前表，圆点从棕回到灰 | `src/data_page.rs:5386、src/data_page.rs:4604` |
| click | 有活动事务但有脏行 | 拒绝并写「还有 N 行更改未提交：先点「提交更改」或「放弃更改」再提交事务」 | `src/data_page.rs:5386、src/data_page.rs:6579` |

### `data_page.toolbar.rollback_txn` 「回滚事务」按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「回滚事务」；Tab 有事务时出现在「提交事务」右侧
- 同步：Linux done（§5.1 回滚事务 / §8.2 回滚确认；rollback_transaction 的 AlertDialog）；Windows partial（§7.1：Ctrl+Z / 状态栏「撤销（↶）」= 丢弃暂存 + Rollback，不弹确认对话框；有未提交改动时关页 / 关窗第一次只提示。）；macOS none
- 实现备注：破坏性动作（destructive-action）；失败态仍可用——「事务失败（只能回滚）」时这是唯一出口。回滚完成 = 事务结束：丢弃界面草稿并重新读取当前表，让屏幕回到数据库真实状态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有活动事务（含失败态） | 先确认「回滚事务？」（有脏行时正文写「回滚事务将丢弃数据库侧的变更，并放弃 N 行未提交的界面更改。」），按钮 取消 / 回滚（破坏性，默认取消）；确认后回滚，圆点回到灰 | `src/data_page.rs:5413、src/data_page.rs:4604` |

### `data_page.toolbar.status_cluster` 工具条状态簇（spinner · 过程消息 · 状态圆点）

kind `container` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`status_dot_diameter`
- 无障碍：可访问名「操作状态」
- 同步：Linux done（§5.1 工具条状态簇与「操作结果收进圆点」；set_state / set_status / note_action）；Windows none（§7.1 未记录工具条状态簇：Windows 的脏行数 / 提交中 / 已回滚写在状态栏行 2 右侧。）；macOS none
- 实现备注：自左向右：spinner（有 RPC 在途时可见）→ 本次操作的临时消息 → 状态圆点；整体在漏斗左侧，与图标按钮同一垂直中线。过程消息只留两类：①「正在…」进行中（正在提交… / 正在取消提交（批次 …）… / 正在读取表数据…，读取完成后擦掉）；② warning / error（权限不足、类型不可编辑、行已标记删除、提交逐行失败…）。已经发生的操作结果（第 N 行已标记删除（提交前可撤销）、已放弃未提交的更改（含新增行））不占可见文案，只写进圆点 tooltip 末段「最近一次操作：…」。这一处**不重复**行数 / 页码 / 总数：那些只在状态栏分页器显示一次。撤销（放弃未提交的更改，状态栏行操作组 ↶ / RowOp::Discard）与删除（−）的结果同样不占可见文案：前者把 ●N / ✕N / ＋ 清回普通行号，后者把行标删；两句都只写进圆点 tooltip 的「最近一次操作：…」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 有 RPC 在途 | spinner 可见并转动，消息写「正在读取表数据…」/「正在提交 M 条变更（批次 …）：点「取消提交」可在行边界中止」/「正在取消提交（批次 …）：等驱动在行边界停下，已执行的行会整批回滚…」 | `src/data_page.rs:6427、src/submit_batch.rs:449` |
| load | 读取完成 | 擦掉「正在读取…」，消息位留空（不写行数 / 页码）；最近一次读取摘要只进状态栏那格的 tooltip | `src/data_page.rs:6442、src/data_page.rs:6494` |

### `data_page.toolbar.status_dot` 状态圆点（灰 / 事务棕 / 脏行强调色）

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `status_dot_diameter`；颜色 `warning_color`、`accent_bg_color`
- 无障碍：可访问名「事务与未提交更改的状态」；Tab 不可聚焦（不占 Tab 位），仍在无障碍树里
- 自绘：没有原生等价物：GTK / Adw 没有「状态圆点」控件；用 GtkBox + CSS 画 7px 圆（尺寸走 metrics.status_dot_diameter，颜色走 token），命中区仍是 24×24 的 GtkButton。
- 同步：Linux done（§5.1 状态圆点三态 / §5.2 事务状态的可见表达；sync_status_dot + update_dirty_label）；Windows none（§7.1 未记录工具条状态圆点：数据页状态按优先序写在状态栏行 2 右侧（只读 / 自动提交 / 未提交 N 行 / 提交中… / 已回滚）。）；macOS none
- 实现备注：视觉是一枚 status_dot_diameter 的实心圆（约漏斗墨迹的一半），命中区仍是 24×24；focusable=false，但 tooltip 同时登记为可访问名。三态互斥、优先级自上而下：灰 = 没有未提交的更改、也没有打开的事务；@warning_color = 事务已打开（含只读 / 失败态）；@accent_bg_color = 有脏行 / 新行（压在事务态之上）。没有打开的表时整枚隐藏——圆点说的是「这张表格此刻怎样」。原来常驻的两段文字（自动提交徽标、脏行 N · 待提交 M 条）不再占可见文案，只作 tooltip 与 QA 断言的文案来源。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| hover | 已打开表 | tooltip 三段：第一段写事务状态 + 脏行 N · 待提交 M 条（含 K 个删除）· 新行 1 行，第二段是事务说明（「自动提交：每条语句自成一个事务」/「事务中：提交事务之前，改动只在当前会话可见」/「事务失败：只能回滚」/「已断开：事务已由后端回滚」），有已发生结果时第三段「最近一次操作：…」（如「第 2 行已标记删除（提交前可撤销）」「已放弃未提交的更改（含新增行）」） | `src/data_page.rs:6705、src/data_page.rs:6528` |
| load | 状态变化（开始 / 提交 / 回滚 / 失败 / 标记删除 / 放弃） | 颜色随状态实时切换（有脏行即强调色，否则事务打开即棕，否则灰）；状态变化本身**不**在过程消息位写任何可见文字 | `src/data_page.rs:6705、src/data_page.rs:4604` |

### `data_page.toolbar.filter` 「筛选」（漏斗）按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「筛选与排序」；Tab 工具条次要动作组内第一枚
- 图标：`action.filter`
- 同步：Linux done（§5.1 / §5.3 能力位与面板开合；toggle_filter_panel + show_open）；Windows none（§7.1 未记录：Windows 数据页没有筛选 / 排序入口。）；macOS none
- 实现备注：图标是项目自有的 sqarvo-filter-symbolic：放大镜会被读成「搜索」，Adwaita / Yaru 都没有漏斗。驱动能力位 filtering=false 时按钮**隐藏**（不是点了报错）；没有列时一并禁用。面板是挂在它上面的 popover（autohide=false），锚点落在面板上沿偏右（0.618）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 驱动声明支持筛选且已打开表 | 打开 / 收起筛选与排序面板；打开时把**已应用**的筛选与排序复制成草稿 | `src/data_page.rs:2380、src/data_page.rs:2389` |
| load | 打开表时读到能力位 filtering=false | 漏斗隐藏；排序入口随之不生效；出现「驱动不支持排序/筛选」徽标 | `src/data_page.rs:4298、src/data_page.rs:4397` |

### `data_page.toolbar.columns` 「列显隐」（眼睛）按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「列显隐」；Tab 工具条次要动作组内第二枚
- 图标：`action.columns`
- 同步：Linux done（§5.1 列显隐 / §5.3 列显隐卡片；open_column_visibility）；Windows none（§7.1 未记录：Windows 数据页没有列显隐入口。）；macOS none
- 实现备注：只影响渲染：不改变查询、复制与导出口径（导出始终包含全部列）。没有列时隐藏；有列时不依赖驱动能力位（与筛选取舍相反）。与漏斗互斥：打开它会把筛选面板 popdown。面板形态与筛选面板一致：挂在按钮上的 GtkPopover（autohide=false），不再是主窗口里的页内卡片。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前表有列 | 打开 / 收起挂在按钮上的「列显隐」popover（复选框清单）；再点一次收起 | `src/data_page.rs:2337、src/data_page.rs:1371` |

### `data_page.toolbar.columns.card` 列显隐卡片（复选清单 + 说明）

kind `container` · status `active` · parent `data_page.toolbar.columns`

- 样式：尺寸 `radius_lg`、`row_height_data`
- 无障碍：可访问名「列显隐」
- 同步：Linux done（§5.3 列显隐卡片；cc_card 构建 + on_column_choice_toggled）；Windows none（§7.1 未记录：Windows 数据页没有列显隐卡片。）；macOS none
- 实现备注：`GtkPopover`（`autohide=false`），挂在眼睛按钮上：标题「列显隐」+ 分区标题「列」+ 复选框清单 + ✕ + 说明行；宽度取 `metrics.filter_panel_width`，清单高度与筛选面板的条件清单同口径（140 上限 / 60 下限）。样式与筛选面板**同一套**：容器 `sqarvo-fs`，标题 `fs-title`，分区 `fs-section`，清单 `fs-list`，复选行 `fs-row`（24px 行高、淡底、1px 描边）。说明行原文：「隐藏列只影响这张表格的渲染；查询、复制与导出口径不变。」；复选标签是「列名 · 原始类型」，tooltip 追加「取消勾选只隐藏这一列，不影响数据与导出口径」。换表 / 刷新后按新列清单重建，隐藏状态复位（旧下标对新表没有意义）。是独立 popup surface，不在主窗口 render tree 里：截图取证要用 `qa::save_widget` 拍控件本身。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 卡片已打开、该列在清单里 | 勾选 = 显示该列、取消勾选 = 隐藏该列；只重排表头与单元格，数据与导出口径不变；可以隐藏到只剩行号 | `src/data_page.rs:2361、src/data_page.rs:2316` |
| click | 卡片已打开 | 点 ✕ 收起 popover，隐藏选择保持不变 | `src/data_page.rs:1893` |

### `data_page.toolbar.export` 「导出 ▾」菜单按钮

kind `control` · status `active` · parent `data_page.toolbar`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`
- 无障碍：可访问名「导出」；Tab 工具条次要动作组内第三枚（最右）
- 图标：`action.export`
- 同步：Linux done（§5.1 导出 ▾ / §5.7 入口；MenuButton + 三个菜单项走 export_ui::begin）；Windows none（§7.1 未记录：Windows 数据页没有导出入口（对象树条目同样写明「数据页导出亦未实现」）。）；macOS none
- 实现备注：图标 + 下拉箭头（always_show_arrow）：**一个入口三种出口**，不是单击即导出的图标；图标仍是 document-save-symbolic。tooltip：「导出：CSV / INSERT 语句（当前页或整张表）/ 转储表；本页已加载 N 行（Ctrl+Shift+E 直接导出 CSV）」。有已加载行且没有 RPC 在途时可用；在途时禁用（避免写出半套状态）。Ctrl+Shift+E 与菜单「表 → 导出 CSV」仍是**直接导出 CSV**（不弹格式菜单），与这一格不是同一条路径。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有已加载行、没有 RPC 在途 | 展开三项菜单：「导出 CSV…」「导出为 INSERT 语句…」「转储表…」；单击图标先展开菜单，范围（当前页 / 整张表）仍由紧接着的对话框显式选择 | `src/data_page.rs:1075、src/data_page.rs:1698` |
| load | 没有已加载行（或正在提交） | 按钮禁用；没有可导出内容时不弹菜单、只写一行日志 | `src/data_page.rs:6579` |

### `data_page.export_menu` 导出菜单（三项出口）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`
- 原生控件：gtk:GtkPopoverMenu / winui:— / swift:—
- 同步：Linux done（§5.7 三种出口共用一条流程；data_page new() 的 export popover + export_ui::begin）；Windows none（§7.1 未记录：无导出菜单。）；macOS none
- 实现备注：挂在「导出 ▾」按钮上的 GtkPopover（popup surface，窗口自渲染截图拍不到）；点任一项先收起菜单再执行。三项都进入 export_ui::begin 这同一条流程：范围 → 文件 → 确认 → 覆盖 → 进度 / 取消 → 完成，差别只在写出的格式与文件过滤器。菜单里没有第三条之外的路径（没有「复制到剪贴板」「导出全部结果」这类重复出口）。查询标签的结果区复用同一份「导出 ▾」（入口在 tabs 的查询结果区、由本区记录）：只有 CSV / INSERT 两项，**没有**「转储表」；范围固定为已加载行、没有「整张表」，不重跑 SQL；没有已加载行时提示「没有可导出的行」+「当前标签没有已加载的行：先执行查询，再导出。」。

### `data_page.export_menu.csv` 「导出 CSV…」

kind `item` · status `active` · parent `data_page.export_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「导出 CSV…」
- 同步：Linux done（§5.7 CSV 出口 / §6.1 导出口径与公式注入防护；export_ui::begin(ExportKind::Csv)）；Windows none（§7.1 未记录：无导出。）；macOS none
- 实现备注：范围对话框问当前页与整张表，默认当前页，整张表必须显式选择。确认标题「导出已加载的 N 行为 CSV？」/「导出整张表为 CSV？」；正文给写出口径（空字段 = SQL NULL、空字符串写成空引号串、二进制写占位 (BLOB n bytes)、Decimal 保留原文、日期时间 ISO 8601）与截断警告。公式注入防护开关**只给 CSV**（文本值以等号 / 加号 / 减号 / at 号 / 制表符 / 回车开头时前置单引号），默认开启。文件对话框标题「导出为CSV」、过滤 *.csv、默认 sqarvo-export.csv；目标已存在时再确认一次，默认不覆盖。当前页导出的数据口径：写页面当前的值，含未提交编辑（对话框里写明）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有已加载行 | 选范围 → 选文件 → 确认「导出已加载的 N 行为 CSV？」→ 写文件；完成提示写已写行数、截断数与防护开关状态；在文件对话框取消则不写文件、不报错 | `src/export_ui.rs:437、src/export_ui.rs:506、src/export_ui.rs:695` |
| click | 计划为空或总数为 0（明确知道 0 行） | 提示「没有可导出的行」，不进入文件选择器（转储表例外：空表也允许，只写建表语句） | `src/export_ui.rs:437` |

### `data_page.export_menu.insert` 「导出为 INSERT 语句…」

kind `item` · status `active` · parent `data_page.export_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「导出为 INSERT 语句…」
- 同步：Linux done（§5.7 INSERT 写出口径；export_ui::begin(ExportKind::Insert) + export 的 sql_literal / write_insert）；Windows none（§7.1 未记录：无导出。）；macOS none
- 实现备注：范围同样问当前页 / 整张表；确认标题「导出已加载的 N 行为 INSERT 语句？」/「导出整张表为 INSERT 语句？」。写出口径：一行一条 INSERT INTO 目标表 (列清单) VALUES (值清单);（LF 行尾）；NULL 写 NULL、空串写空引号串；数字 / Decimal 原文；日期时间加引号 ISO 8601；BLOB 写 NULL（客户端拿不到原始字节），文件末尾注释写明数量。标识符与字符串按驱动：SQLite 双引号，MariaDB 反引号 + 反斜杠转义。文件对话框标题「导出为INSERT 语句」、过滤 *.sql、默认 sqarvo-insert.sql。目标表名（查询结果导出时才需要推导）：只接受**唯一一个**顶层 FROM（跳过字符串 / 注释 / 括号）并取 `[库.]表` 的最后一段；JOIN 外的第二个 FROM、`FROM (`、没有 FROM 时推断不出，写占位名 `result` 并在文件头加注释「结果集不携带来源表名：目标表写成占位名 result，导入前请替换成真实表名。」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有已加载行 | 选范围 → 选文件 → 确认 → 写 SQL 脚本；不做公式注入防护（SQL 不经过表格软件）；截断单元格在文件末尾追加一行注释 | `src/export_ui.rs:437、src/export_ui.rs:695、src/export.rs:348` |

### `data_page.export_menu.dump` 「转储表…」

kind `item` · status `active` · parent `data_page.export_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「转储表…」
- 同步：Linux done（§5.7 转储表内容；export_ui::begin_object_dump + run_whole_table_export）；Windows none（§7.1 未记录：无转储表入口。）；macOS none
- 实现备注：**不问范围**（天然整表）：确认标题「转储表 <表名>？」。文件内容：文件头注释（Sqarvo 导出：表名与列数）→ 可选注释 → 建表语句（现场 GetDdl；exact=false 时注明「建表语句由客户端拼装」）→ 每行一条 INSERT；列名与建表语句在同一后台线程、同一会话里读。空表也允许导出，只写建表语句；默认文件名 <表名>.sql，文件对话框标题「导出为SQL 脚本」。同一个动作另有对象树表节点右键菜单入口（登记在 object_tree 区），两条入口共用 export_ui::begin_object_dump。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 已打开表（有浏览会话） | 确认「转储表 <表名>？」→ 选文件 → 进度对话框（已写行数 / 截断单元格数）+「取消」；取消或失败不留半截文件与临时文件 | `src/export_ui.rs:490、src/export_ui.rs:1197、src/export_ui.rs:1418` |

### `data_page.badges` 表头上方徽标行

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`gap_sm`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_on_soft`
- 无障碍：可访问名「表状态徽标」
- 同步：Linux done（§5.1 表头上方徽标行 / §5.6 权限不足；update_badges + show_failure）；Windows none（§7.1 未记录页内徽标行：只读 / 无主键 / 驱动不支持只作为编辑被拒的原因出现在提示里。）；macOS none
- 实现备注：六类徽标：只读 / 无主键 / 视图（写种类名）/ 无权限 / 未统计 / 驱动不支持排序筛选；每枚带解释性 tooltip，有任意一枚时整行可见。不随状态变化消失：用户不用去看禁用的按钮猜为什么不能编辑。「无权限」在读取或提交返回 permission 类错误码时置上，不只靠一句状态行。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 打开表（可叠加权限 / 统计 / 能力位结果） | 按条件逐枚追加：只读（tooltip 给具体原因）、无主键（「无主键表：不能稳定定位行，因此只读；浏览、排序、筛选与计数仍可用」）、视图（「视图：数据来自查询定义，编辑受限…」）、无权限（「当前账号对该对象没有权限…」）、未统计（「精确计数超过预算，已跳过…」）、驱动不支持排序/筛选 | `src/data_page.rs:4397` |
| load | 读取 / 提交返回 permission 类错误 | 置上「无权限」徽标并常驻，直到重新打开表 | `src/data_page.rs:4523` |

### `data_page.grid` 数据网格（表格外框 · 行号列 · 表头 · 数据行）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`column_header_min_height`、`row_number_width`、`column_width`、`frame_border_width`、`radius_lg`、`grid_padding_start`；颜色 `theme_bg_color`、`theme_fg_color`
- 无障碍：可访问名「数据网格」
- 原生控件：gtk:GtkColumnView / winui:自绘虚拟化行区（§7.1） / swift:—
- 同步：Linux done（§5.4 网格结构 / §5.5 offset 分页与总数三态；build_columns + navigate + scroll_to_top）；Windows partial（§7.1：行号列 row_number_width（行号 = offset + 下标 + 1）、单行表头、row_height_data / column_width、横向滚动一致；**每页 200 行**，翻页入口是页内「上一页 / 下一页 / 加载更多…」（offset = 已取行数续拉），页码写「第 N 页 / 共 ? 页」。）；macOS none
- 实现备注：结构：行号列（固定 row_number_width，行号 = offset + 行下标 + 1，**不是**主键）+ 单行表头 + 数据行。列宽按**本页内容**自适应：表头文案与已加载值里最长文字 + 单元格内距，下限 64、上限 column_width；拖过列边界或双击自适应过的列记为用户宽度，翻页与刷新不再覆写（64 与内距是代码常量，未进 metrics）。表头条带 26 内容 + 上下 2 内距（column_header_min_height）；底色取 theme_bg_color，文字与数据行同档。横向滚动条常驻可见（关掉 overlay 自动淡出），宽表只横向滚动、不挤压右侧信息面板；窗口最大化后横向滚动位置回到第一列。滚动到底**不**自动取下一页（与 offset 分页互斥），翻页只由状态栏分页器驱动。页码与总数只在状态栏分页器出现一次（status_bar.row1.pager / .summary）：三态是「共 N 行」/「约 N 行」（估计值必须带「约」）/「共 ? 行」（总数未知或计数超时写问号，不编造数字；Linux 未知时只写页码，Windows 写「共 ? 行」）；计数超预算时写「未统计（超出计数预算）」并出现「未统计」徽标。每页 500 行（TABLE_PAGE_SIZE，实际 page_size 随第一页返回）；翻页 = 整页替换 + 回到顶部。刷新（状态栏 ⟳ / 表菜单）走**同一条** dirty_guard：有脏行 / 标删 / 新行时被拒绝并说明原因，按钮禁用时 tooltip 附同一句——数据页只给谓词与文案，按钮本身在 status_bar。末页需要总页数：总数未知时 can_last = false（按钮禁用），点了也不发请求，状态行写「<动作>不可用（没有对应页）」；驱动不声明 totals 与非精确计数同一写法（不显示总数）。新行占位行是**网格里的最后一行**（行号列写 ＋，行号 0 = 尚未落库）：提交成功后按返回的主键定位再赋真实行号；整批全部省略时提交即 INSERT DEFAULT VALUES（自增 / 默认值列由数据库填）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 可编辑且没有待插入行（入口：状态栏行操作组 +，工具条与菜单里都没有） | 在末尾追加一行草稿并进入可编辑态：有默认值的列显示数据库默认值原文、自增 / 无默认列写「省略（交给数据库默认值）」；已有待插入行时先选中它并写「已经有一行待插入的新行：先提交或放弃它」，不再追加；每格双击填值，点「提交」才发 InsertRows | `src/data_page.rs:3099、src/data_page.rs:4988、src/data_page.rs:5049` |
| load | 打开表 / 翻页 / 刷新（无未提交更改） | 整页替换并写「正在读取 offset N …（<动作>）」；成功后回到第一行；翻页中再翻页写「上一页仍在读取：稍候再翻页」，不发第二次请求 | `src/data_page.rs:3011、src/data_page.rs:4272、src/data_page.rs:4388、src/backend/table.rs:27` |
| load | 有脏行、标记删除或新行草稿 | 翻页 / 排序 / 筛选 / 刷新一律拒绝，写「有未提交的更改：先提交或「放弃更改」再翻页/排序/筛选（翻页会替换整页数据）」 | `src/data_page.rs:3048、src/data_page.rs:3011` |
| scroll | 滚到本页最后一行 | 不自动取下一页（offset 分页互斥），页脚不出现「加载更多」 | `src/data_page.rs:4388` |

### `data_page.grid.header` 列头（列名 · 原始类型 · 排序标记）

kind `row` · status `active` · parent `data_page.grid`

- 样式：尺寸 `column_header_min_height`、`column_width`、`font_ui`、`font_weight_title`；状态 hover=`sqarvo_brand_soft`
- 无障碍：可访问名「（行内容：列名 · 原始类型）」
- 同步：Linux done（§5.3 表头排序标记 / §5.4 点列头；refresh_header_tooltips + header_title_text；Linux 表头无 PK 标记（见 impl_notes））；Windows partial（§7.1：列头「列名 · 原始类型（· PK）」，没有类型信息时只写列名；列宽固定 column_width，超宽走横向滚动；没有排序标记与点击查看列属性。）；macOS none
- 实现备注：表头只有**一行**：标题文本是「列名 · 原始类型」（类型为空时只写列名）；整个标题可点，不是只有文字。主键标记只出现在声明了它的实现里（Windows 写「· PK」）；Linux 的表头不带主键标记，主键进信息面板的列页与徽标。拖宽把手在列右缘 6px 内，落在那里的松开不算「点击列头」。排序标记跟在标题末尾（▲ / ▼ / ▲2），与筛选面板共用同一份排序状态；表头本身不参与排序。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该列存在（松开点不在列右缘 6px 内） | 信息面板切到该列的列页（与树上列节点同一个页面，不重发 RPC）；**不改排序** | `src/data_page.rs:4013、src/data_page.rs:4056` |
| load | 打开表 / 排序变化 / 列显隐变化 | 标题写「列名 · 原始类型」，有排序键时追加 ▲ / ▼ / ▲2（数字是优先级）；tooltip「点击查看列属性：<列名>」 | `src/data_page.rs:763、src/data_page.rs:6194` |
| drag | 指针落在列右缘的拖宽把手 | 改列宽并记为**用户宽度**（翻页 / 刷新 / 自动列宽都不再覆写）；双击把手按内容自适应同样记为用户宽度 | `src/data_page.rs:5910、src/data_page.rs:5931` |

### `data_page.grid.row_number` 行号列（行号 / 脏行 / 标删 / 新行）

kind `cell` · status `active` · parent `data_page.grid`

- 样式：尺寸 `row_number_width`、`font_status`、`font_weight_ui`；颜色 `accent_bg_color`
- 无障碍：可访问名「（行内容：行号或未提交标记）」
- 同步：Linux done（§5.4 行号标记；build_columns 的 number_factory（●N / ✕N / ＋ 与四条 tooltip））；Windows partial（§7.1：脏行在行号列显示 ●N（tooltip「有未提交的更改」）、被其它会话删除的行显示 ✕N；行号 = offset + 下标 + 1，右对齐等宽。未记录新行占位「＋」。）；macOS none
- 实现备注：宽度固定 row_number_width、右对齐、等宽字体、色弱一档；行号不是主键，也不可拖宽。四种写法互斥，按优先级：已删除（服务端报告行已不在）写 ✕N → 新行占位写 ＋ → 已标记删除写 ✕N → 有未提交更改写 ●N → 普通行号。放弃（状态栏行操作组 ↶ / 放弃未提交的更改）后 ●N / ✕N / ＋ 都回到普通行号；一次成功提交后被删行整行消失、脏行变普通行号。●N 的颜色是系统强调色（语义色），脏单元格的浅底是品牌浅档——两者刻意不同：一个是「有未提交的更改」，一个是「改过的格子」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 行已加载 | 普通行写行号（tooltip「第 N 行」）；脏行写「●N」（「第 N 行 · 有未提交的更改」）；标删写「✕N」（「第 N 行 · 已标记删除（提交前可撤销）」）；新行写「＋」（「新行 · 尚未提交（双击单元格填值）」）；被其它会话删除的行同样写「✕N」并给删除线与压暗 | `src/data_page.rs:5845、src/data_page.rs:5851、src/data_page.rs:5857、src/data_page.rs:5839` |
| hover | 行号格有标记 | tooltip 说明这一格为什么不是普通数字（有未提交的更改 / 已标记删除（提交前可撤销）/ 已删除 / 新行尚未提交） | `src/data_page.rs:5868` |

### `data_page.grid.cell` 数据单元格（值 · NULL / 空串 · 脏色）

kind `cell` · status `active` · parent `data_page.grid`

- 样式：尺寸 `row_height_data`、`column_width`、`font_data_cell`、`font_weight_cell`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_on_soft`、`sqarvo_brand_mid`；状态 selected=`sqarvo_brand_mid`
- 无障碍：可访问名「（行内容：单元格值）」
- 同步：Linux done（§5.4 单击 / 双击 / 脏行底色 / NULL 与空串；cell_factory bind + edit_block_reason）；Windows partial（§7.1：NULL 用弱色斜体 NULL、空串保持空白；双击或 F2 进入编辑；编辑被拒逐条给原因（只读表、主键列、行不可定位、值被截断、类型不在就地编辑范围）。）；macOS none
- 实现备注：NULL 写「NULL」（弱色斜体，tooltip「SQL NULL（与空字符串不同）」）；空串**不能**与 NULL 长得一样：Linux 写空引号串，Windows 留空（§7.1 明确「空串保持空白」）。数值与二进制右对齐（便于比较），文本左对齐；超长文本省略号截断，全文进 tooltip。脏单元格是品牌浅档实底 + 对应深色前景（成对写，选中行里也不继承白字）；标记删除的行整行删除线 + 压暗。服务端截断的值 tooltip 追加「该值被截断：不能编辑（提交截断值会写坏数据）」，且不允许进入编辑。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 行存在 | 只选中行（Ctrl / Shift 多选），不改数据；状态栏选中数出现 N selected | `src/data_page.rs:1237、src/data_page.rs:6206` |
| double_click | 单元格可编辑 | 打开编辑（见 data_page.editor）；不可编辑时状态行给具体原因：「该行已标记删除：撤销删除后才能编辑」「列 X 的值被服务端截断…」等，不弹空编辑器 | `src/data_page.rs:6231、src/data_page.rs:5460、src/data_page.rs:5523` |
| key Esc | 网格有焦点、有选中行 | 清空行选中（不影响 SQL 编辑器与对话框） | `src/data_page.rs:1248` |

### `data_page.editor` 单元格编辑器（标题 · 输入框 · 设为 NULL · 应用 / 取消）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`
- 无障碍：可访问名「编辑单元格」
- 原生控件：gtk:GtkPopover / winui:就地 TextBox（§7.1） / swift:—
- 同步：Linux done（§5.4 双击单元格 / 编辑提示 / 应用失败就地显示；open_cell_editor + open_editor）；Windows partial（§7.1：格内就地编辑（双击 / F2），Enter 暂存、Esc 取消、点别处按暂存；编辑被拒五类原因逐条说明；没有「设为 NULL」勾选与类型提示行。）；macOS none
- 实现备注：同一个编辑器被三处复用：数据网格、单行「字段 · 值」视图的值列、查询结果区的单表编辑（open_cell_editor 是唯一实现）。标题「编辑 <列> · <类型>」；输入框 +「设为 NULL（SQL 空值）」勾选 + 提示行 +「应用」/「取消」。编辑器内容宽 320 是代码常量，未进 metrics。进入编辑时值**不被全选**、光标停在末尾（双击的目的是改值；全选会让选中底色压住原值，误按一个字符就整段替换），Ctrl+A 仍可全选。可空列提示「空文本在文本列表示空字符串；要写 NULL 请勾选上方选项」；不可空列提示「该列不允许 NULL：请输入合法值」且 NULL 勾选禁用。值转换失败时错误就地显示、编辑框不关（可以在原处改）。写库时机：应用只改**界面草稿**，不落库；真正写库只有「提交」（ApplyRowChanges）。新行用**另一套**编辑器（open_insert_editor）：多一个「省略该列（交给数据库默认值 / 自增）」态，与「设为 NULL」/ 文本三态互斥，省略的列不出现在 InsertRows 请求里；有默认值的列在省略态显示默认值原文（dim 样式 + 提示），不猜实际值。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 输入合法（或勾选了设为 NULL） | 值写进界面草稿：单元格加脏色、行号变「●N」、状态圆点转强调色；编辑框收起。写库仍要等「提交」 | `src/data_page.rs:570、src/data_page.rs:5578、src/data_page.rs:5615` |
| click | 编辑框已打开 | 「取消」直接收起、不改草稿（与从未打开等价） | `src/data_page.rs:570` |
| key Esc | 编辑框已打开 | 收起编辑框并放弃本次输入（Linux：popover 的 Esc 收起等同取消）；不写草稿、不写库 | `src/data_page.rs:570、src/data_page.rs:646` |
| key Enter | 就地编辑态（Windows 口径） | 把输入提交到**本地暂存**（不落库）、单元格转脏；Linux 的编辑框是 应用 / 取消 两个按钮的形态，Enter 不提交（见 platform_notes.linux），最终草稿与提交口径一致 | `src/data_page.rs:570、src/data_page.rs:5615` |
| key F2 | 网格有选中单元格 | 进入编辑（与双击同一入口）；Linux 未绑 F2 —— 双击是唯一编辑入口（见 platform_notes.linux） | `src/data_page.rs:5460` |
| click | 编辑框已打开、点了编辑框以外的任何位置 | Linux：popover 自动收起，等同取消（没点「应用」就什么都没改）；Windows：按「暂存」处理，输入进本地草稿 | `src/data_page.rs:578、src/data_page.rs:570` |

### `data_page.row_menu` 数据行右键菜单

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`
- 无障碍：可访问名「数据行菜单」
- 原生控件：gtk:GtkPopoverMenu / winui:— / swift:—
- 同步：Linux done（§5.8 数据行右键菜单；show_row_menu + context_menu 模块）；Windows none（整片未实现：数据页没有「删除行」（标记 ✕N、删除线、撤销、提交生成 DELETE 都没有），右键菜单的两个条目都无从接起；`Views/ShellView.Data.cs:2041-2043` 目前只把行删除按钮禁用并写「请用 SQL」。已记 docs/planning/windows-frontend-parity-plan.md §8.2。）；macOS none
- 实现备注：原生 GtkPopoverMenu（与系统右键菜单同一外观与键盘操作）；每个条目一个 gio::SimpleAction，动作组挂在**菜单自己**身上，名字一定解析得到。手势挂在**行控件**上（label → cell → row 爬两层，一行只挂一次，用 sqarvo-row-menu-host 类标记）：单元格文字、列分隔线、行分隔线、末列右侧空白都算右键位置；表格下方空白不属于任何行、不响应。未选中的行先被选中，已多选时保留多选，菜单动作永远作用于当前选中集。「复制为」的二级菜单展开在条目**右侧**（默认向下会压住下方条目）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| right_click | 已打开表 | 按点击位置弹出菜单：「删除行」、「复制为 ▸ 复制为 INSERT」；不可编辑时「删除行」灰掉（delete_available = false），「复制为 INSERT」仍可用 | `src/data_page.rs:2136、src/data_page.rs:2157、src/data_page.rs:2161、src/context_menu.rs:71` |
| click | 菜单打开中 | 点菜单以外的任何地方（其它行、工具条、外面板）一律收起 | `src/context_menu.rs:77` |

### `data_page.row_menu.delete` 「删除行」

kind `item` · status `active` · parent `data_page.row_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「删除行」
- 同步：Linux done（§5.8 删除行；delete_selected_row + delete_note）；Windows none（§7.1 未记录数据行右键菜单。）；macOS none
- 实现备注：与状态栏 − / RowOp::Delete 完全同一方法（delete_selected_row），可用性同一谓词（delete_available）。只标记待删除，不立刻写库；新行占位行不在删除范围。结果不占可见文案，只写状态圆点 tooltip 末段「最近一次操作：…」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 可编辑且有选中行 | 把选中行标记为待删除：行号变「✕N」、整行删除线 + 压暗；圆点注记「第 N 行已标记删除（提交前可撤销）」或「已标记删除 N 行（批量提交前可撤销）」，一次「提交」才真正写库 | `src/data_page.rs:5272、src/data_page.rs:5017、src/data_page.rs:5337` |

### `data_page.row_menu.copy_insert` 「复制为 ▸ 复制为 INSERT」

kind `item` · status `active` · parent `data_page.row_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「复制为 INSERT」
- 同步：Linux done（§5.8 复制为 INSERT；copy_selected_as_insert + export 的 multi_row_insert）；Windows none（§7.1 未记录数据行右键菜单。）；macOS none
- 实现备注：把选中行合成**一条多值** INSERT（目标表 + 列清单 + 多组 VALUES，行间换行）写进系统剪贴板，用选中行的**当前值**（含未提交编辑）。BLOB / 非有限浮点写 NULL，并在末尾附一行 sqarvo 注释；标识符按该连接驱动引号。只读也要能导出数据：本条目在只读连接下仍可用。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有选中行 | 合成多值 INSERT 写入剪贴板；成功后圆点注记「已复制 N 行为 INSERT 语句」，失败写 warning 说明原因 | `src/data_page.rs:2016、src/data_page.rs:2161` |

### `data_page.detail` 单行「字段 · 值」展示形式

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`frame_border_width`、`radius_lg`
- 无障碍：可访问名「字段 / 值视图」
- 同步：Linux done（§5.9 展示形式切换；toggle_detail_view / set_detail_mode / fill_detail / detail_cell_factory）；Windows none（§7.1 未记录展示形式切换（Windows 只有按行浏览）。）；macOS none
- 实现备注：与网格共用同一块表格外框（sqarvo-table-frame），由视图栈切换；进场是 120ms 交叉淡入。两列：字段（220，可拖）与值（自适应）；行 = 该行的每个字段，值取**当前值**（含未提交编辑），显示文本与网格**同一份**（NULL 为 NULL；空串在 Linux 写空引号串、Windows 留空）。220 是代码常量，未进 metrics。只讲**第一个**选中的行（多选时）；日志「[data] 展示形式：字段 / 值（栈当前页 detail）」。入口是状态栏分页器**前方**的 toggle（登记在 status_bar.row1.detail_toggle，属状态栏区）；本条目只登记这块内容与它的编辑 / 复位语义。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有选中行、当前是按行浏览（入口：状态栏的展示形式 toggle） | 表格外框内换成「字段 | 值」两列，只讲第一个选中行；按钮呈按下态（选中态由数据页快照推回，不在按钮自己记） | `src/data_page.rs:2053、src/data_page.rs:2093、src/data_page.rs:1291` |
| click | 已在「字段 / 值」视图 | 再按一次同一按钮切回按行浏览（同一动作幂等切换），网格原样回来，选中与脏行不变 | `src/data_page.rs:2053、src/data_page.rs:2073` |
| double_click | 「值」列、该值可改（不是只读连接 / 只读事务 / 截断列 / 已标记删除的行） | 打开与网格同一个编辑器（行号 = 详情当前行、列号 = 字段下标）；改完立即按当前值重填整页（日志「详情第 N 行：id=2 · name=…」），提交仍走「提交」与脏行守卫 | `src/data_page.rs:688、src/data_page.rs:753` |
| load | 没有选中行 / 换表 / 重新读取 / 选中集清空 | 没有选中行时不切视图，状态行「先选中一行，再切换到「字段 / 值」视图」；换表与选中集清空**强制**回到按行浏览并清空详情数据 | `src/data_page.rs:2053、src/data_page.rs:4228` |

### `data_page.filter_panel` 筛选与排序面板（草稿 · 应用 / 清空）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`；尺寸 `filter_panel_width`、`filter_column_width`、`radius_lg`、`row_height_data`
- 无障碍：可访问名「筛选与排序」
- 原生控件：gtk:GtkPopover / winui:— / swift:—
- 同步：Linux done（§5.3 打开 / 关闭 / 应用 / 清空 / 草稿模型；open_filter_panel + apply_browse_options + clear_filter）；Windows none（§7.1 未记录筛选与排序面板（Windows 数据页没有筛选 / 排序入口）。）；macOS none
- 实现备注：挂在漏斗按钮上的 GtkPopover（autohide=false），锚点落在上沿的黄金分割点（0.618，偏右）；面板宽 filter_panel_width。面板里的改动是**草稿**：点「应用」才同时生效（筛选 + 排序一起提交，并回到第 1 页）；关闭面板丢弃草稿。标题右侧写「已应用」/「有未应用的更改」（warning + tooltip「点应用才生效；关闭会丢弃」）。面板里只有一种字号（12px，与对象树一致）：正文、按钮、下拉、输入框、说明字统一，层级只靠字重与颜色；列表行 24 高。底部只有一个调整区：分隔线 + 右对齐的「清空」「应用」；文案不带作用域，也不写说明段落。两个区：筛选条件（列 + 算子 + 值 → 条件行，多条按 AND）与排序（列 + 方向 → 排序键，列表顺序即优先级）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 打开面板 | 把**已应用**的筛选与排序复制成草稿并重建列下拉；状态文案回到「已应用」；错误提示清除 | `src/data_page.rs:2389、src/data_page.rs:2919` |
| click | 草稿与已应用状态不同 | 「应用」：未知列或算子缺值则面板内报错 + 状态行 error，不发请求；成功则写回唯一状态、回到第 1 页、重发浏览请求、状态行「筛选与排序已应用（N 条条件 · M 个排序键）· 回到第 1 页」；**面板不关闭** | `src/data_page.rs:2941` |
| click | 有未提交更改 | 「应用」被拒绝，状态行「有未提交的更改：先提交或「放弃更改」再翻页/排序/筛选（翻页会替换整页数据）」，草稿保留 | `src/data_page.rs:2941、src/data_page.rs:3048` |
| click | 面板已打开 | 「清空」只清**草稿**（条件与排序键），仍需点「应用」才生效；状态行「条件与排序已清空（点「应用筛选与排序」恢复全表）」 | `src/data_page.rs:2997` |
| click | 面板已打开 | 点 ✕（或再点漏斗）关闭面板，未应用的改动丢弃 | `src/data_page.rs:2411、src/data_page.rs:1773` |
| key Esc | 面板内有行内编辑态 | 先收起行内编辑；没有编辑态时才关面板（一个 Esc 不该把整张面板关掉） | `src/data_page.rs:1792` |

### `data_page.filter_panel.condition_composer` 条件编辑组（列 · 算子 · 值 · 添加条件）

kind `container` · status `active` · parent `data_page.filter_panel`

- 样式：尺寸 `filter_column_width`、`row_height_data`
- 无障碍：可访问名「添加筛选条件」
- 同步：Linux done（§5.3 添加筛选条件；add_filter_condition + read_condition_editor）；Windows none（§7.1 未记录。）；macOS none
- 实现备注：列下拉固定 filter_column_width（不随面板拉伸）；算子下拉只写符号（= ≠ ≤ LIKE IS NULL 等）且**不显示下拉箭头**；值是单行输入框。算子选 IS NULL / IS NOT NULL 时不需要值：输入框禁用并清空。点「添加条件」把这一组追加到**草稿**（不改正在显示的数据），值输入框清空，条件出现在下方清单。校验先在本地说清楚：缺值报「<算子> 需要一个值（列 X · 类型）」，未知列同样在面板内报错，不发请求。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有列清单、算子需要值且值非空（或算子为 IS NULL 类） | 条件追加到草稿并渲染成条件行（多条按 AND 组合）；日志「筛选条件已加入草稿：<列> <算子> <值>（尚未应用）」 | `src/data_page.rs:2487、src/data_page.rs:2546` |
| click | 算子下拉可见 | 只列出符号算子；选 IS NULL / IS NOT NULL 后值输入框禁用并清空（这两条不需要值） | `src/data_page.rs:1401、src/data_page.rs:2518` |
| click | 算子需要值但值被清空 | 不加入条件，面板内红字「<算子> 需要一个值（列 X · 类型）」 | `src/data_page.rs:2518、src/data_page.rs:2530` |

### `data_page.filter_panel.condition_row` 条件行（列 · 算子 · 值，可就地编辑）

kind `row` · status `active` · parent `data_page.filter_panel`

- 样式：尺寸 `row_height_data`；颜色 `sqarvo_brand_soft`；状态 hover=`sqarvo_brand_soft`
- 无障碍：可访问名「（行内容：列 算子 值）」
- 同步：Linux done（§5.3 编辑条件；condition_label_row / condition_editor_row / commit_condition_edit）；Windows none（§7.1 未记录。）；macOS none
- 实现备注：只读态是一行文字（列 + 符号算子 + 值）+ 行尾一个 ✗；点这一行**就地展开**为三件套编辑器，不弹二级对话框。编辑态与只读态同一套外观：同底色（不加强调色）、同行高、同字号，只把文字换成控件。已加的条件是淡底色 + 圆角的实体条（品牌浅档的 55%，悬停变成浅档实色）；行尾始终只有一个 ✗。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该条件行不是编辑态 | 就地展开为列 / 算子 / 值编辑器；回车或 ✓ 提交，✗ 或点面板空白处收起 | `src/data_page.rs:2681、src/data_page.rs:2601` |
| key Enter | 条件行处于编辑态 | 提交这一行修改（只改草稿），收起编辑器；校验失败时不收起并在面板内报错 | `src/data_page.rs:2725` |
| click | 条件行存在 | 点行尾 ✗ 从草稿里删掉这条条件（仍需点「应用」才影响数据） | `src/data_page.rs:2767、src/data_page.rs:2570` |

### `data_page.filter_panel.sort_composer` 排序编辑组（列 · 方向 · 添加排序）

kind `container` · status `active` · parent `data_page.filter_panel`

- 样式：尺寸 `filter_column_width`、`row_height_data`
- 无障碍：可访问名「添加排序键」
- 同步：Linux done（§5.3 排序键；add_sort_key）；Windows none（§7.1 未记录。）；macOS none
- 实现备注：列下拉与筛选同宽（filter_column_width）；方向下拉只写 asc / desc（SQL 词，不翻译、不加方向图标、不显示箭头）。点「添加排序」把键加入**草稿**；同一列再加一次只改方向，不产生重复键。驱动不支持排序时面板内报「当前驱动不支持排序」，不加入。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有列清单且驱动支持排序 | 列 + 方向加入草稿的排序键列表（列表顺序即优先级）；日志「排序键已加入草稿：<列> <asc|desc>（尚未应用）」 | `src/data_page.rs:2780、src/data_page.rs:2810` |
| click | 驱动排序能力位为 false | 不加入，面板内报「当前驱动不支持排序」 | `src/data_page.rs:2788` |

### `data_page.filter_panel.sort_row` 排序键行（列名 · asc / desc · 上移 / 下移 / 删除）

kind `row` · status `active` · parent `data_page.filter_panel`

- 样式：尺寸 `row_height_data`；颜色 `sqarvo_brand_soft`；状态 hover=`sqarvo_brand_soft`
- 无障碍：可访问名「（行内容：列名 asc 或 desc）」
- 同步：Linux done（§5.3 排序键行；render_sort_keys + toggle_sort_key_direction / move_sort_key / remove_sort_key）；Windows none（§7.1 未记录。）；macOS none
- 实现备注：行文本是「列名  asc|desc」，**不写序号**：从上到下就是优先级，序号与上移 / 下移表达的是同一件事。行尾三个线条图标按钮：上移（第一条禁用）、下移（最后一条禁用）、删除（✕）；与面板其它 ✗ 同一风格。改动只进草稿，仍需点「应用」；与表头的 ▲ / ▼ 标记共用同一份排序状态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 排序键行存在 | 点列名切换 asc ↔ desc（只改草稿），列表重排并刷新标题状态 | `src/data_page.rs:2875、src/data_page.rs:2837` |
| click | 该键不是第一条 / 不是最后一条 | 上移 / 下移把它在列表里挪一位，列表顺序即 SQL 的排序优先级 | `src/data_page.rs:2893` |
| click | 排序键行存在 | 点 ✕ 删除这个排序键（仍需「应用」才生效） | `src/data_page.rs:2906` |

### `data_page.txn_chooser` 「开始事务」卡片（隔离级别 · 只读）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `overlay`；尺寸 `radius_lg`
- 无障碍：可访问名「开始事务」
- 同步：Linux done（§5.2 打开「开始事务」卡片；open_txn_chooser + render_isolation_options + confirm_begin_transaction）；Windows none（§7.1 未记录隔离级别卡片：Windows 第一次暂存时直接 Begin（带 transaction_id），没有级别选择。）；macOS none
- 实现备注：页内模态卡片（不是独立窗口，也不是 popover）：标题「开始事务」+ 隔离级别选项 +「只读」勾选 + 底部 取消 / 开始事务。不支持的隔离级别**在开始前**就禁用并标「· 不可用」，说明行给原因——不让用户点完才知道不支持。每次打开都按当前驱动重建选项（换库 / 换驱动后不能沿用旧能力表）；卡片宽 460 是代码常量，未进 metrics。打开卡片前已经过 begin_transaction 的自检（无事务、未断开、无未提交更改），被挡住时根本不打开。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 工具条「开始事务」可用 | 卡片出现：隔离级别（默认选中第一个可用项）、「只读」勾选、按钮「开始事务」/「取消」 | `src/data_page.rs:3643、src/data_page.rs:3653` |
| click | 已选可用隔离级别 | 「开始事务」按所选级别（及只读勾选）发 RPC；在途提示「开始事务：<隔离级别> · 读写」，事务状态落定后清除，圆点转棕 | `src/data_page.rs:3753、src/data_page.rs:1745` |
| click | 卡片已打开 | 「取消」或 ✕ 收起卡片，不开始事务、不发 RPC | `src/data_page.rs:1749` |

### `data_page.placeholder` 占位页（尚未打开 / 正在读取 / 空表 / 本页没有数据）

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`
- 无障碍：可访问名「数据页占位」
- 同步：Linux done（§5.6 打开表（加载中）/ 空表；begin_open + show_open 的占位页分支）；Windows none（§7.1 未记录数据页占位页文案（只记录总数三态与页码写法）。）；macOS none
- 实现备注：四段文案互斥：尚未打开表 / 正在读取表数据 / 空表 / 本页没有数据；「空」与「错」严格区分（错误走横幅 + 逐行错误列表，不占这一页）。空态与越界页必须给**原因**，不能只写「没有数据」。图标与说明只用 dim 文字，不放大色块。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 打开表 / 翻页请求已发出 | 「正在读取表数据」+「等待服务端返回第一页…」 | `src/data_page.rs:4261、src/data_page.rs:1313` |
| load | 第一页 0 行 | 「空表」+「该表存在，但当前条件下没有任何行」/「该表存在，但目前没有任何行」（有筛选时写前者）；offset 越界时写「本页没有数据」+「offset 超出总数（服务端如实返回空行）；回首页或刷新再看」 | `src/data_page.rs:4342` |

### `data_page.banner` 页首横幅（只读 / 断连 / 错误）

kind `control` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；颜色 `warning_color`、`sqarvo_brand_soft`、`sqarvo_brand_on_soft`
- 无障碍：可访问名「数据页横幅」
- 同步：Linux done（§5.6 只读 / 断连；show_read_only_banner + mark_disconnected）；Windows partial（§7.1：只读与失败原因写在状态栏提示行（关页 / 关窗守卫也走提示行，不弹对话框）；没有页首横幅。）；macOS none
- 实现备注：只读横幅常驻（直到刷新事务状态）：正文是「<标题>：<解释>」，例如「只读：该表没有主键：无主键表只有无稳定排序的浏览顺序，无法稳定定位行，因此不能编辑…」。断连横幅写明「未提交的 N 行变更已丢弃；数据库事务已由后端强制回滚」（无脏行时省略中间一句），已加载数据**保留**为只读。后端失效时的横幅带一枚「重启 Sqarvo」按钮（重启确认属 shell 区）。横幅不替代徽标行与状态行：三者同时在，各自说明不同的层级（原因 / 能力 / 这一刻的动作结果）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 服务端标记只读（表级 / 视图 / 无主键 / 只读事务） | 横幅常驻显示只读原因；编辑类按钮禁用；过程消息 tooltip 给同一原因（不能只靠禁用暗示） | `src/data_page.rs:6802、src/data_page.rs:4334` |
| load | 会话丢失或后端断开 | 丢弃未提交更改并写「已断开：<原因> 未提交的 N 行变更已丢弃；数据库事务已由后端强制回滚。」；状态行「已断开 · 已加载 N 行（只读）」；已加载行保留 | `src/data_page.rs:4666` |
| load | 读取 / 提交 / 事务操作失败 | 横幅写服务端给的失败文案（错误码进日志与状态行），状态行转 error，逐行失败另见 data_page.errors；**已加载的行保留**、不清屏，可重试 | `src/data_page.rs:4523、src/data_page.rs:4595` |

### `data_page.errors` 逐行提交失败列表

kind `container` · status `active` · parent `data_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`gap_xs`
- 无障碍：可访问名「提交失败的行」
- 同步：Linux done（§5.4 提交结果；show_applied → show_failures）；Windows partial（§7.1：失败的行保留在原处可重试，主键被删的行报 table_data.row_deleted、影响行数不符报 table_data.rows_affected_mismatch；没有逐行列表控件。）；macOS none
- 实现备注：位置在工具条下方、表格上方；一行一条「第 N 行：[错误码] 原因」，可选中复制。只列**这一次提交里失败的行**；成功的行变干净，失败的行保留在原处可重试。没有失败时整块隐藏（不占高度）；取消提交后整块清空（不采信任何部分结果）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 提交批次里有失败行 | 逐行列出「第 N 行：[code] 原因」（如 table_data.row_deleted / table_data.rows_affected_mismatch）；块可见、已加载行保留；状态行与横幅同时给汇总 | `src/data_page.rs:6810、src/data_page.rs:4487` |

### `data_page.query_edit` 查询结果的单表编辑（复用同一份草稿与提交路径）

kind `container` · status `active` · parent `data_page`

- 无障碍：可访问名「查询结果编辑态」
- 同步：Linux done（§5.10 查询结果的单表编辑（v0.2.5）；sql_page try_enable_edit / on_applied，result_edit，results 重绘）；Windows none（§7.1 明确「查询结果网格在本版不支持编辑」（结果区 ReadOnly 文案即此口径），没有 + − ✓ ↶ 与暂存。）；macOS none
- 实现备注：查询标签的结果区默认只读；当结果**等价于一张表的当前页**时，状态栏的 + − ✓ ↶ 对它生效，口径与表数据页一致（同一份 TableDraft / InsertDraft / ApplyRowChanges）。三个条件同时成立才装上编辑态：结果终态、SQL 是唯一顶层 FROM 的简单 SELECT（无 JOIN / 集合运算 / GROUP BY / 多表逗号）、结果列覆盖目标表全部主键列。编辑用与网格同一个 open_cell_editor；提交目标表由 SQL 页显式给出，会话是本标签自己的会话。本版边界（仍成立）：新增行一次只有一行；限流（Limited）结果不启用编辑；查询标签没有提交确认对话框（风险只写进状态行与日志）。行级脏色与删除线在 v0.2.5 **已落地**：改过的单元格 `.sqarvo-cell-dirty`、标删的行 `.sqarvo-row-deleted`（删除线 + 0.55 透明度）、新行占位整行按「待提交」着色，与数据页同一套 class；`.sqarvo-row-deleted` 原先只定义在数据页自己的 provider 里，查询标签没打开过数据页时根本没加载，现搬到全局样式表（theme::stylesheet）两处共用。重绘不是重跑：编辑 / 标删 / 新增 / 提交 / 放弃之后按草稿重绘（重跑会丢掉未提交的草稿）；只有**提交成功**才自动重跑同一条语句。重绘范围：小结果集（≤ 5000 行，RELOAD_ROW_LIMIT）整表重绘、只发一次 items_changed；更大的结果集只重绘变过的行，但仍回到首行。提交落定后只重绘本批变过的行（dirty_indices），不再把已加载的每一行都 items_changed 一遍。回到的是**首行首列**（scroll_home）：只把行滚到 0 不够——GtkColumnView 会为露出「焦点所在的那一列」而横向滚动，在最后一列改完值再提交就会停在那里（用户报「刷新后跳到最后一列」）；做法是点名第一列 + 下一帧再兜一次横向 adjustment（焦点恢复发生在那之后），有变化时写日志「[query] 水平滚动复位：X → 0」。每次重跑重建列也走这条。状态圆点是**常驻**的（不是「有脏行才出现」）：没有未提交更改时是灰点、tooltip「没有未提交的更改」；有脏行 / 标删 / 新行时强调色，tooltip「自动提交 · 脏行 N · 待提交 M 条 ⏎ 自动提交：每条语句自成一个事务」口径同表工具条同一枚圆点（同一套 CSS 与文案）。表事件的归属：路由层先判「该标签没有浏览状态」再投给 SQL 页（tabs::route_table），**不能**让它撞上「通道不符」被整批丢弃——否则提交后既没有回填也不会自动重跑（日志「丢弃事件：标签 tab-q1 是查询标签，不接收表数据事件」就是这条路径走错了）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 结果终态、唯一顶层 FROM 的简单 SELECT、覆盖全部主键列 | 装上编辑态：行操作组可用，日志「[query] 结果可编辑：表 users，3 行，主键 [id]」；状态圆点常驻（灰 = 没有未提交的更改，有脏行转强调色） | `src/sql_page.rs:708、src/result_edit.rs:45、src/results.rs:605` |
| load | 上述任一不成立 | 行操作组整体置灰，tooltip 写明具体原因：「结果不是单表查询（JOIN / 聚合 / 子查询）：不能新增、编辑或删除」/「结果里没有主键列 X：定位不到要改的行，不能编辑」/「这张表没有主键：不能新增、编辑或删除」/「读取主键失败：…」——不静默灰掉 | `src/sql_page.rs:715、src/sql_page.rs:740、src/result_edit.rs:78` |
| double_click | 结果可编辑、单元格可改 | 打开与网格同一个编辑器；确定后按当前值重绘该行（未提交前不写库），并立刻重算行操作可用性（第一次编辑后 ✓ 即可点） | `src/results.rs:742、src/sql_page.rs:731` |
| click | 可编辑、没有未提交改动、没有待插入行（入口：状态栏 +） | 结果末尾追加一行占位，各列写「（默认）」；双击各列填值，提交时按「省略未填列、交给数据库默认值」生成单行 INSERT（日志「[query] 新行草稿：第 N 行（占位）」）；已有未提交改动时提示「已经有未提交的更改：先提交或放弃，再新增行」，有占位行时提示「已经有一行待插入的新行：先提交或放弃它」 | `src/results.rs:672、src/sql_page.rs:956、src/result_edit.rs:198` |
| click | 可编辑且有选中行（入口：状态栏 −） | 把选中行标记待删除（不立刻写库），可 ↶ 放弃；重绘后整行删除线 + 压暗，状态行「已标记删除 N 行（提交前可以放弃）」 | `src/sql_page.rs:968、src/result_edit.rs:176` |
| click | 有脏行 / 待插入行（入口：状态栏 ✓ / Ctrl+S） | 走 ApplyRowChanges；全部成功后**自动重跑同一条语句**（results.statement()，不是编辑器里的当前文本），结果区写「已提交 N 行 · 已自动重新执行查询」/「已插入 N 行 · 已自动重新执行查询」；有失败行时**不重跑**，失败行留在草稿里 | `src/sql_page.rs:801、src/sql_page.rs:841、src/sql_page.rs:380` |
| click | 有脏行（入口：状态栏 ↶） | 全部编辑、删除标记与新行草稿一起清掉，按草稿重绘并回到首行首列，状态行「已放弃未提交的更改」 | `src/sql_page.rs:1020、src/results.rs:544` |
| load | 有未提交的更改且点了刷新（⟳） | 拒绝并写「有未提交的更改：先提交或放弃，再刷新」（重跑会换掉整个结果集，草稿没有落点） | `src/sql_page.rs:954` |

## 对话框（`dialogs`）

连接对话框（新建 / 编辑两种模式共用一张表面）与关于对话框。连接对话框是唯一配置连接参数、测试连接、把密码交给系统凭据库的入口；关于对话框只陈述版本、协议与后端诊断，不依赖后端可用。

| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |
|---|---|---|---|---|---|---|
| `dialogs` | 对话框区 | area | active | done | partial | — |
| `dialogs.connection` | 　连接对话框（新建 / 编辑） | container | active | done | partial | — |
| `dialogs.connection.header` | 　　标题与副标题 | control | active | done | — | — |
| `dialogs.connection.driver` | 　　驱动下拉 | control | active | done | — | — |
| `dialogs.connection.fields` | 　　连接参数表单（按驱动族显隐） | container | active | done | partial | — |
| `dialogs.connection.fields.alias` | 　　　别名（可选） | control | active | done | — | — |
| `dialogs.connection.fields.host` | 　　　主机 | control | active | done | partial | — |
| `dialogs.connection.fields.port` | 　　　端口 | control | active | done | partial | — |
| `dialogs.connection.fields.username` | 　　　用户名 | control | active | done | partial | — |
| `dialogs.connection.fields.database` | 　　　数据库（可选） | control | active | done | partial | — |
| `dialogs.connection.fields.password` | 　　　密码 | control | active | done | partial | — |
| `dialogs.connection.fields.tls` | 　　　TLS 模式（五档） | control | active | done | partial | — |
| `dialogs.connection.fields.file_path` | 　　　数据库文件（含「浏览」） | control | active | done | — | — |
| `dialogs.connection.fields.read_only` | 　　　「只读打开」 | control | active | done | — | — |
| `dialogs.connection.credential_note` | 　　凭据说明行（「密码只写入系统凭据库」） | control | active | done | partial | — |
| `dialogs.connection.status` | 　　状态行（校验 / 测试结果 / 失败） | control | active | done | partial | — |
| `dialogs.connection.actions` | 　　底部操作条（状态行 + 两个按钮） | container | active | done | partial | — |
| `dialogs.connection.actions.test` | 　　　「测试连接」 | control | active | done | partial | — |
| `dialogs.connection.actions.save` | 　　　「保存并连接」（建议动作） | control | active | done | — | — |
| `dialogs.connection.actions.cancel` | 　　　「取消」 | control | active | done | — | — |
| `dialogs.about` | 　关于对话框 | container | active | done | — | — |
| `dialogs.about.app_info` | 　　名称 / 版本 / 一句说明 | control | active | done | — | — |
| `dialogs.about.protocol` | 　　后端版本与协议 | control | active | done | — | — |
| `dialogs.about.diagnostics` | 　　后端诊断字段 | control | active | done | — | — |
| `dialogs.about.privacy_note` | 　　保密说明行 | control | active | done | — | — |

### `dialogs` 对话框区

kind `area` · status `active`

- 样式：槽位 `dialogs`；尺寸 `gap_page`
- 同步：Linux done（dialog.rs（AdwDialog + AdwPreferencesPage）与 ui.rs show_about 逐条对照）；Windows partial（§7.1 只记录了「新建连接对话框」一行；编辑模式与关于对话框未记录。）；macOS none
- 实现备注：只收独立于某个动作、有自己生命周期的表面：新建 / 编辑连接对话框、关于对话框。确认框跟随触发它的区：关闭标签确认在 tabs，删除 / 断开连接确认在 object_tree，后端重启确认在 shell —— 它们不是本区的组件。两个对话框都是 Attached Modal 表面（AdwDialog 附着主窗口呈现），不新开顶层窗口；同一时刻各只有一个实例。

### `dialogs.connection` 连接对话框（新建 / 编辑）

kind `container` · status `active` · parent `dialogs`

- 样式：槽位 `dialogs`；尺寸 `gap_md`、`gap_page`
- 无障碍：可访问名「新建连接」；Tab 默认动作是「保存并连接」（Enter 即保存并连接）
- 原生控件：gtk:AdwDialog + AdwPreferencesPage / winui:待核 / swift:—
- 同步：Linux done（§2.3 编辑连接 + dialog.rs build / apply_saved 复核）；Windows partial（§7.1 只记录新建模式（Views/ConnectionDialog.xaml）；编辑模式、标题与宽度未记录。）；macOS none
- 实现备注：宽度 560 是代码常量，未进 metrics（P1 补齐）。标题始终是「新建连接」，编辑模式不改标题：两种模式的差异只在预填的字段与凭据引用上，不在措辞上。标题栏左侧只有「取消」；主体是「连接参数」表单 + 凭据说明两块；底部是状态行与两个提交按钮。字段显隐由 driver_form 的一张描述表决定，界面代码里没有按驱动 ID 分支：加驱动不改界面代码。系统凭据库不可用或密码写入失败时如实报错，不回退明文保存；对话框只收集输入与展示结果，RPC 在 worker 线程执行。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 后端已握手（未握手时工具条「连接」与菜单项禁用；动作本身也直接返回，只记日志「后端尚未连接，忽略「新建连接」」，不发任何请求） | 打开对话框：标题「新建连接」、副标题「未选择驱动」，驱动下拉默认选中 MariaDB（存在时）；已打开时只把它前置，不叠第二个 | `src/ui.rs:1103、src/ui.rs:1105、src/ui.rs:1108、src/dialog.rs:130` |
| load | 从对象树右键「编辑连接…」进入，或按保存凭据连接时发现缺密码 | 用已保存连接预填非敏感字段（驱动、主机、端口、库、用户名、文件路径、只读、TLS 档位）；密码行留空，已有 credential_ref 时复用它并在状态行提示「已填入保存的连接：密码将从系统凭据库读取」，引用为空时提示「已填入保存的连接；请输入密码后连接」；不自动连接，用户仍需点「测试连接」或「保存并连接」 | `src/dialog.rs:582、src/dialog.rs:611、src/dialog.rs:614、src/ui.rs:1122` |
| load | 从对象树 / 启动态按保存的凭据连接时凭据缺失（配置里没有引用，或后端回 credential.not_found） | 打开预填对话框，并在状态行写「这条连接缺少密码，请编辑后重试（<原因>）」：原因分别是「配置里没有凭据引用」或「<stage>: <message>（credential.not_found）」；不发连接 RPC，真正的失败分类由用户重新提交后的结果给出 | `src/ui.rs:490、src/ui.rs:1193、src/ui.rs:1220、src/dialog.rs:546` |

### `dialogs.connection.header` 标题与副标题

kind `control` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `font_title`、`font_panel`
- 无障碍：可访问名「连接对话框标题」
- 同步：Linux done（dialog.rs apply_selection / set_pending 复核）；Windows none（§7.1 未记录）；macOS none
- 实现备注：副标题只说「当前驱动是谁」，不写主机 / 用户：那两样在表单里本身可见。标题字号由 AdwWindowTitle 决定，未进 metrics。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 对话框构建或驱动切换 | 副标题 = 当前驱动的显示名（后端 ListDrivers 返回的 display_name）；没有可选驱动时写「未选择驱动」 | `src/dialog.rs:425、src/dialog.rs:135` |
| load | 「测试连接」或「保存并连接」的请求在飞 | 副标题改为「正在与数据库握手…」；回包后恢复为驱动显示名 | `src/dialog.rs:485、src/dialog.rs:501` |

### `dialogs.connection.driver` 驱动下拉

kind `control` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「驱动」；Tab 可聚焦；↑ / ↓ 换驱动
- 同步：Linux done（dialog.rs apply_selection 与 DriverChoice::from_summary 复核）；Windows none（§7.1 未记录）；macOS none
- 实现备注：清单只来自后端 SystemService.ListDrivers，前端不硬编码；默认值偏好 MariaDB 只是肌肉记忆，不是清单。下拉标签形如「显示名（driver_id）」；前端没有表单的驱动照样列出，标签追加「前端暂无表单」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 对话框构建 | 列出后端全部驱动并默认选中 MariaDB（不存在时选第一个）；后端没返回驱动时只显示「（后端未返回任何驱动）」，下拉与两个提交按钮一并禁用，状态行写「后端没有注册任何驱动（ListDrivers 为空）：无法新建连接」 | `src/dialog.rs:141、src/dialog.rs:272、src/dialog.rs:444` |
| load | 切换驱动 | 按驱动族重排字段可见性、按能力重建 TLS 档位标签、补回被清空的主机 / 端口默认值；前端没有该驱动表单时清空并禁用提交，状态行写「此版本前端没有驱动「<driver_id>」的连接表单：请升级前端，或换一个驱动」 | `src/dialog.rs:390、src/dialog.rs:293、src/dialog.rs:436` |

### `dialogs.connection.fields` 连接参数表单（按驱动族显隐）

kind `container` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `gap_xs`、`gap_md`
- 平台差异：Windows：§7.1：MariaDB 字段为主机 / 端口 / 用户名 / 密码 / 库 / TLS 五档；文件型驱动的数据库文件路径与只读打开未记录。
- 同步：Linux done（driver_form.rs fields_for 描述表 + dialog.rs 显隐接线）；Windows partial（§7.1 只记录 MariaDB 字段（主机 / 端口 / 用户名 / 密码 / 库 / TLS 五档）；文件型驱动的路径与只读未记录。）；macOS none
- 实现备注：服务器型驱动显示主机 / 端口 / 库 / 用户名 / TLS / 密码；文件型驱动只显示数据库文件与只读打开。隐藏的字段不进入 ConnectionParams：不是「显示了但不填」。分组标题「连接参数」；顺序固定，不随驱动重排。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 驱动族确定（服务器型 / 文件型 / 前端不认识） | 按描述表显示或隐藏各行；切回服务器型驱动时空着的主机与端口补回默认值（用户没改过就不该看到空主机） | `src/dialog.rs:197、src/driver_form.rs:91、src/dialog.rs:411` |

### `dialogs.connection.fields.alias` 别名（可选）

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「别名（可选）」；Tab 连接参数组内第二行（驱动之后、主机 / 文件路径之前）
- 同步：Linux done（cargo test：别名不进身份键 / 旧配置可读 / 别名不脱敏连接参数）；Windows none；macOS none
- 实现备注：所有驱动都显示（不随驱动族显隐）：别名不是连接参数，SQLite 与服务器型驱动都能取。只影响对象树连接行与信息面板的标题；不参与连接身份键、不进 gRPC 连接参数、不做会话显示名。留空 = 没有别名，界面回落到派生名；保存时去掉首尾空白。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 对话框已打开（新建或编辑），点了「保存并连接」 | 别名随其余非敏感字段一起写入 connections.json 的 alias；旧配置没有该字段时按空别名读取 | `src/dialog.rs:190、src/driver_form.rs:172、src/backend/worker.rs:4018` |

### `dialogs.connection.fields.host` 主机

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「主机」
- 同步：Linux done（dialog.rs 默认值 + driver_form.rs 校验文案）；Windows partial（§7.1 只写「主机」是 MariaDB 字段；默认值、空值校验与错误文案未记录。）；macOS none
- 实现备注：新建时的初值与切回服务器型驱动时的补值是 127.0.0.1；文件型驱动下不显示。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 切到服务器型驱动且主机为空 | 主机补回 127.0.0.1（用户改过就不动） | `src/dialog.rs:160、src/dialog.rs:411` |
| click | 提交时主机为空 | 状态行写「请填写主机」，不发任何 RPC | `src/driver_form.rs:169、src/dialog.rs:700` |

### `dialogs.connection.fields.port` 端口

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「端口」
- 同步：Linux done（driver_form.rs 端口解析与两条文案）；Windows partial（§7.1 只写「端口」是 MariaDB 字段；范围校验与文案未记录。）；macOS none
- 实现备注：新建时的初值是 3306；输入框是数字用途（不是数字键盘强制的自绘控件）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 切到服务器型驱动且端口为空 | 端口补回 3306 | `src/dialog.rs:164、src/dialog.rs:414` |
| click | 提交时端口不是数字，或不在 1-65535 之间 | 状态行写「端口必须是 1-65535 之间的数字（当前：<输入>）」（空值或 0 写不带当前值的同一句），不发任何 RPC | `src/driver_form.rs:176、src/driver_form.rs:181` |

### `dialogs.connection.fields.username` 用户名

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「用户名」
- 同步：Linux done（driver_form.rs 用户名校验）；Windows partial（§7.1 只写「用户名」是 MariaDB 字段；必填校验与文案未记录。）；macOS none

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 提交时用户名为空（服务器型驱动） | 状态行写「请填写用户名」，不发任何 RPC | `src/driver_form.rs:190、src/dialog.rs:700` |

### `dialogs.connection.fields.database` 数据库（可选）

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「数据库」
- 同步：Linux done（dialog.rs 行标题 + driver_form.rs default_name）；Windows partial（§7.1 只写「库」是 MariaDB 字段；留空语义与显示名规则未记录。）；macOS none
- 实现备注：行标题本身就是说明：「数据库（可选：留空则连接后浏览所有库）」，不靠 tooltip 才说清可选。空库名是合法值：语义是「没有默认库」，不是缺必填项。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 服务器型驱动且库名留空 | 按合法值处理：会话显示名不拼斜杠（用户@主机），连上后浏览所有库 | `src/dialog.rs:169、src/driver_form.rs:186、src/driver_form.rs:246` |

### `dialogs.connection.fields.password` 密码

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「密码」
- 平台差异：Windows：§7.1：密码只进 Windows 凭据管理器，落盘只有 credential_ref；密码行的留空与复用规则未记录。
- 同步：Linux done（dialog.rs apply_saved（留空 + 复用引用）+ driver_form.rs 密码校验）；Windows partial（§7.1 记录密码只进 Windows 凭据管理器、落盘只有 credential_ref；密码行的留空与复用规则未记录。）；macOS none
- 实现备注：输入框是密码专用的输入行（自带显示 / 隐藏切换），内容不进日志、不进配置。密码永不回显：没有「读出旧密码」的路径，编辑模式下这一行一定是空的。密码随草稿交给 worker 后即被对话框丢弃，本模块不保存它。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 编辑模式（有已保存连接） | 密码行留空，并复用已有的 credential_ref：后端从操作系统凭据库取密码，界面不展示任何已存密码；引用为空时状态行提示「已填入保存的连接；请输入密码后连接」 | `src/dialog.rs:606、src/dialog.rs:611` |
| click | 提交时密码为空且没有 credential_ref | 状态行写「请输入密码」，不发任何 RPC | `src/driver_form.rs:200、src/dialog.rs:700` |

### `dialogs.connection.fields.tls` TLS 模式（五档）

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「TLS 模式」
- 同步：Linux done（dialog.rs rebuild_tls_model / tls_subtitle_for + TlsMode::label_for）；Windows partial（§7.1 只写「TLS 五档」；档位文案、随驱动能力的退化说明未记录。）；macOS none
- 实现备注：五档固定顺序：禁用（disabled）/ 优先（preferred）/ 必需（required）/ 校验 CA（verify-ca）/ 完整校验（verify-full）；默认「优先」。标签随驱动能力重建：驱动做不到只校链时，第四档写「校验 CA（verify-ca，本驱动等同 verify-full）」并给副标题「本驱动无法只校验证书链：仍会校验主机名」。文件型驱动没有 TLS：不显示该行，落盘的 tls_mode 是 disabled，不写一个假的安全策略。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 服务器型驱动 | 按当前驱动能力重建五档标签，并把选中档位映射回新列表（换 model 会让选中失效，重建后显式选回同一档） | `src/dialog.rs:465、src/dialog.rs:737、src/backend/connection.rs:109` |
| load | 选中「校验 CA」且驱动不支持只校链 | 该行副标题写「本驱动无法只校验证书链：仍会校验主机名」；切到别的档位时副标题立刻消失 | `src/dialog.rs:477、src/dialog.rs:750` |

### `dialogs.connection.fields.file_path` 数据库文件（含「浏览」）

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「数据库文件」
- 同步：Linux done（dialog.rs open_file_chooser + driver_form.rs 路径校验）；Windows none（§7.1 未记录）；macOS none
- 实现备注：路径可手输，也可点行尾「浏览」用系统文件选择器（标题「选择 SQLite 数据库文件」，过滤 *.db、*.sqlite、*.sqlite3）。文件选择器取消不算错误：不写行、不记错误日志。:memory: 是合法路径：不要求文件存在。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 文件型驱动 | 打开系统文件选择器；选中后只把路径写进输入行，其余流程与手输完全相同（取消则什么都不改） | `src/dialog.rs:186、src/dialog.rs:763` |
| click | 提交时路径为空或文件不存在 | 状态行写「请填写数据库文件路径（例如 /home/me/app.db）」或「数据库文件不存在：<路径>（请选择已有文件，或先用 sqlite3 创建）」，不发任何 RPC | `src/driver_form.rs:207、src/driver_form.rs:214` |

### `dialogs.connection.fields.read_only` 「只读打开」

kind `control` · status `active` · parent `dialogs.connection.fields`

- 样式：槽位 `dialogs`；尺寸 `font_ui`、`min_target`
- 无障碍：可访问名「只读打开」
- 同步：Linux done（driver_form.rs 只读 + 内存库互斥规则）；Windows none（§7.1 未记录）；macOS none
- 实现备注：副标题「以只读方式连接：数据库会拒绝任何写入（数据页会显示只读原因）」；只对文件型驱动显示。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 已勾选只读且路径是 :memory: | 状态行写「内存数据库（:memory:）不支持只读模式」，不发任何 RPC（与后端同一条规则，提前在界面说清） | `src/driver_form.rs:221、src/dialog.rs:192` |

### `dialogs.connection.credential_note` 凭据说明行（「密码只写入系统凭据库」）

kind `control` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `font_status`
- 无障碍：可访问名「密码只写入系统凭据库」
- 同步：Linux done（dialog.rs credential_note 构建与显隐）；Windows partial（§7.1 记录密码只进 Windows 凭据管理器、落盘只有 credential_ref；对话框内的说明行未记录。）；macOS none
- 实现备注：只说事实，不说安全承诺以外的内容：配置里只有一个随机引用。不需要密码的驱动隐藏这一行（说了也没有含义）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 当前驱动需要密码 | 显示两行：「密码只写入系统凭据库」与「连接配置只保存一个随机引用；系统凭据库不可用时如实报错，不会回退明文保存」 | `src/dialog.rs:209、src/dialog.rs:405` |

### `dialogs.connection.status` 状态行（校验 / 测试结果 / 失败）

kind `control` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `font_status`
- 无障碍：可访问名「连接状态」
- 平台差异：Windows：§7.1：测试连接就地展示延迟与服务端版本；失败给「标题 + 后端文案 + 稳定错误码」；状态行的位置与两行样式未记录。
- 同步：Linux done（§7.1 失败三段文案口径 + dialog.rs show_status / show_failure / show_test_report）；Windows partial（§7.1 记录「测试连接」就地展示延迟与服务端版本、失败给「标题 + 后端文案 + 稳定错误码」；状态行的位置与样式未记录。）；macOS none
- 实现备注：单行区，失败时按两行显示三段：分类主标题、后端文案、稳定错误码（形如「认证失败：用户名或密码错误」/「<detail> [<code>]」），可换行、左对齐、占满可用宽度。成功 / 失败 / 说明三种语义只靠样式类区分，不换控件。状态行只在对话框里，不落到状态栏、不弹第二个提示框。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 保存前本地校验不通过 | 写校验原因（例如「请填写主机」「端口必须是 1-65535 之间的数字（当前：abc）」「请输入密码」），不发任何 RPC，对话框保持打开 | `src/driver_form.rs:145、src/dialog.rs:700` |
| load | 「测试连接」成功 | 写「连接成功：延迟 <N> ms · 服务端版本 <version>」；后端未返回版本时写「未知（后端未返回版本）」，并把返回的凭据引用留在对话框里复用 | `src/dialog.rs:509、src/ui.rs:612` |
| load | 测试或保存失败 | 两行：分类主标题（如「认证失败：用户名或密码错误」「系统凭据库不可用（不会回退明文保存）」「连接超时」），下一行是后端文案加「[稳定错误码]」（如 mariadb.auth_failed / credential.not_found / timeout） | `src/dialog.rs:530、src/backend/connection.rs:479、src/backend/connection.rs:513` |
| load | 切换驱动，或上一次的错误已不适用 | 状态行收起，旧驱动的错误不留在屏幕上 | `src/dialog.rs:432、src/dialog.rs:570` |

### `dialogs.connection.actions` 底部操作条（状态行 + 两个按钮）

kind `container` · status `active` · parent `dialogs.connection`

- 样式：槽位 `dialogs`；尺寸 `gap_md`、`gap_xl`、`fs_button_min_height`、`min_target`
- 同步：Linux done（dialog.rs actions 布局 + set_pending 禁用清单）；Windows partial（§7.1 只记录「测试连接」一个按钮；操作条布局、另两个按钮与「进行中整体禁用」未记录。）；macOS none
- 实现备注：状态行左对齐占满，两个按钮右对齐；底栏上下留白 gap_md、左右留白 gap_xl。对话框本身没有「断开中」这一态：连接 / 断开中的重复点击由连接注册表忽略（见 object_tree 连接行），本对话框只在请求在飞期间整体禁用。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 「测试连接」或「保存并连接」的请求在飞 | 两个按钮、驱动下拉与全部输入行一并禁用（sensitive=false），避免重复提交；回包后按驱动与能力恢复可用性 | `src/dialog.rs:485、src/dialog.rs:717` |

### `dialogs.connection.actions.test` 「测试连接」

kind `control` · status `active` · parent `dialogs.connection.actions`

- 样式：槽位 `dialogs`；尺寸 `fs_button_min_height`、`min_target`
- 无障碍：可访问名「测试连接」
- 同步：Linux done（dialog.rs 测试按钮接线 + Event::Tested 回填）；Windows partial（§7.1 记录「测试连接」就地展示延迟与服务端版本；按钮可用性判定未记录。）；macOS none
- 实现备注：不落盘：测试成功只把凭据引用留在对话框里，配置与凭据都不写（写盘只发生在「保存并连接」）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有可用驱动（ListDrivers 非空且前端认识该驱动），请求不在飞 | 先做本地校验，通过后整体置为禁用、状态行写「正在测试连接…」，调 TestConnection；成功回「连接成功：延迟 … ms · 服务端版本 …」，失败回分类错误两行 | `src/dialog.rs:227、src/dialog.rs:700、src/ui.rs:612` |
| click | 没有可用驱动（后端未返回驱动，或前端没有该驱动的表单） | 按钮禁用，点不动：不发测试请求，也不退回一份写死的 MariaDB 表单 | `src/dialog.rs:420、src/dialog.rs:153` |

### `dialogs.connection.actions.save` 「保存并连接」（建议动作）

kind `control` · status `active` · parent `dialogs.connection.actions`

- 样式：槽位 `dialogs`；尺寸 `fs_action_min_height`、`min_target`；颜色 `sqarvo_brand_deep`、`sqarvo_brand_on_deep`
- 无障碍：可访问名「保存并连接」
- 同步：Linux done（dialog.rs request + ui.rs submit / Event::ConnectResult 路径）；Windows none（§7.1 未记录）；macOS none
- 实现备注：建议动作：实底用品牌深档 + 深档前景（与工具条「连接」同一条 token），hover / active 只叠半透明，不退回系统强调色。落盘只有两件事：connections.json 里的连接条目（含 credential_ref）与系统凭据库里的密码条目。保存成功即建立会话并关闭对话框，落到工作区；保存失败则对话框留着、状态行给原因。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 本地校验通过，驱动可用 | 整体置为禁用、状态行写「正在保存密码并建立会话…」→ 密码写入系统凭据库 → 配置只落 credential_ref → 连接成功后关闭对话框并落入工作区 | `src/dialog.rs:700、src/ui.rs:1382、src/ui.rs:641` |
| click | 本地校验不通过 | 不提交：状态行给原因，对话框保持打开，用户改正后可再点 | `src/dialog.rs:701` |
| click | 后端 worker 不可用（已退出或未握手） | 状态行写「本地后端未连接」（错误码 backend.unavailable），按钮恢复可点，不假装已保存 | `src/ui.rs:1399、src/ui.rs:1401、src/backend/connection.rs:489` |

### `dialogs.connection.actions.cancel` 「取消」

kind `control` · status `active` · parent `dialogs.connection.actions`

- 样式：槽位 `dialogs`；尺寸 `fs_button_min_height`、`min_target`
- 无障碍：可访问名「取消」
- 同步：Linux done（dialog.rs cancel 接线与注释）；Windows none（§7.1 未记录）；macOS none
- 实现备注：在标题栏左侧，不在底部操作条里：破坏性为零的出口不需要挤在提交按钮旁边。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 对话框已打开 | 直接关闭：不写配置、不写凭据、不建会话；再次打开从默认值重新开始 | `src/dialog.rs:138、src/dialog.rs:286` |

### `dialogs.about` 关于对话框

kind `container` · status `active` · parent `dialogs`

- 样式：槽位 `dialogs`；尺寸 `gap_page`、`gap_md`、`gap_lg`
- 无障碍：可访问名「关于 Squarvo」
- 原生控件：gtk:AdwDialog（自绘内容） / winui:— / swift:—
- 同步：Linux done（§1.4 帮助 → 关于 + ui.rs show_about 逐行对照）；Windows none（§7.1 未记录）；macOS none
- 实现备注：自绘对话框：宽度 620、高度 520 是代码常量，未进 metrics（P1 补齐）。不依赖后端：后端未握手时照样能打开，诊断区块改写端点文件与「后端：尚未握手完成」。内容顺序固定：应用名与版本 → 「后端诊断」标题 → 诊断逐行 → 保密说明。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 帮助 → 关于 Squarvo（任意状态，不依赖后端） | 打开对话框；已打开时只把它前置，不叠第二个 | `src/ui.rs:5289、src/ui.rs:5290、src/ui.rs:4623` |
| load | 内容高于可视区 | 纵向滚动，横向永不滚动；长路径按字符折行，不让一个没有空格的长字符串把对话框顶宽 | `src/ui.rs:5365、src/ui.rs:5351` |

### `dialogs.about.app_info` 名称 / 版本 / 一句说明

kind `control` · status `active` · parent `dialogs.about`

- 样式：槽位 `dialogs`；尺寸 `font_panel`
- 无障碍：可访问名「Sqarvo 版本」
- 同步：Linux done（ui.rs show_about 应用信息三行）；Windows none（§7.1 未记录）；macOS none
- 实现备注：版本号取编译期包版本，不写死文案；字号来自 Adw 的 title-1 与 dim-label，未进 metrics。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 对话框打开 | 三行：「Sqarvo」、「版本 <包版本>」、「轻量级原生数据库客户端（Linux 前端：GTK4 + libadwaita）」 | `src/ui.rs:5301、src/ui.rs:5304、src/ui.rs:5307` |

### `dialogs.about.protocol` 后端版本与协议

kind `control` · status `active` · parent `dialogs.about`

- 样式：槽位 `dialogs`；尺寸 `font_status`
- 无障碍：可访问名「后端版本与协议」
- 同步：Linux done（ui.rs backend_diagnostic_lines 首行与未握手分支）；Windows none（§7.1 未记录）；macOS none

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 后端已握手 | 写「后端版本：<service_version>（协议 <protocol_version>，<build_profile> 构建）」 | `src/ui.rs:5243` |
| load | 后端未握手 | 不写版本与协议行，诊断区块改写「端点文件：<路径>」与「后端：尚未握手完成」——不编造版本号 | `src/ui.rs:5326` |

### `dialogs.about.diagnostics` 后端诊断字段

kind `control` · status `active` · parent `dialogs.about`

- 样式：槽位 `dialogs`；尺寸 `font_status`、`gap_lg`
- 无障碍：可访问名「后端诊断」
- 同步：Linux done（ui.rs backend_diagnostic_lines + 会话行拼装）；Windows none（§7.1 未记录）；macOS none
- 实现备注：逐行一个标签，全部可选中复制（对报障有用），逐字折行。版本不一致必须显式写出来，不能静默：可能连到了旧版本后端或读到了过期端点文件。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 后端已握手 | 逐行给：传输方式、后端进程 PID（由前端启动：是 / 否）、端点文件、版本不一致时「注意：端点记录的后端版本 … 与 GetVersion 返回的 … 不一致。」、已注册驱动「显示名（driver_id）」清单（无驱动时写「已注册驱动：（后端未返回任何驱动）」） | `src/ui.rs:5243、src/ui.rs:5261、src/ui.rs:5268` |
| load | 有工作区 | 追加连接名、初始会话短 ID（「连接对话框建立的初始会话：…（未领养时随退出释放）」）与逐条标签会话行 | `src/ui.rs:5331、src/ui.rs:5334` |

### `dialogs.about.privacy_note` 保密说明行

kind `control` · status `active` · parent `dialogs.about`

- 样式：槽位 `dialogs`；尺寸 `font_status`
- 无障碍：可访问名「保密说明」
- 同步：Linux done（ui.rs show_about 结尾说明行）；Windows none（§7.1 未记录）；macOS none

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 对话框打开 | 写「本对话框只在本机显示，不含密码、凭据引用与 token。」（诊断字段里也确实没有这三样） | `src/ui.rs:5357` |

## 信息面板（`inspector`）

右侧常驻面板：面板外壳与固定宽度、空态留白、连接 / 库 / 表 / 列四种视图、[DDL] 与 [表信息] 两页，以及状态页与跨标签事务提示。内容只由「选中」事件驱动。

| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |
|---|---|---|---|---|---|---|
| `inspector` | 信息面板 | area | active | done | partial | — |
| `inspector.header` | 　标题块（标题 · 副标题）与分隔线 | container | active | done | partial | — |
| `inspector.empty` | 　空态（内容区留白） | container | active | done | partial | — |
| `inspector.connection` | 　连接视图（标题 · 副标题 · 字段行） | container | active | done | done | — |
| `inspector.field` | 　字段行（标签 · 值） | row | active | done | done | — |
| `inspector.catalog` | 　库 / 分组视图（标题 · 副标题 · 计数） | container | active | done | partial | — |
| `inspector.object` | 　表 / 视图视图（标题 · 切换条 · 两页） | container | active | done | partial | — |
| `inspector.pane_switch` | 　[DDL] / [表信息] 切换条 | container | active | done | partial | — |
| `inspector.pane_switch.ddl` | 　　「DDL」按钮 | control | active | done | done | — |
| `inspector.pane_switch.info` | 　　「表信息」按钮 | control | active | done | done | — |
| `inspector.object.info` | 　　表信息页（列 · 主键 · 索引 · 触发器 · 已加载行数） | container | active | done | partial | — |
| `inspector.object.info.columns` | 　　　列清单（列名 · 类型 · 可空 · 主键） | container | active | done | done | — |
| `inspector.object.info.columns.row` | 　　　　列行（列名 · 类型 · 可空 · 主键标记） | row | active | done | done | — |
| `inspector.object.info.pk` | 　　　主键节（主键列 · 读不到时的说明行） | control | active | done | partial | — |
| `inspector.object.ddl` | 　　DDL 正文 | container | active | done | done | — |
| `inspector.object.ddl.warning` | 　　　DDL 琥珀警示（exact=false） | control | active | done | done | — |
| `inspector.column` | 　列视图（六项属性） | container | active | done | — | — |
| `inspector.state` | 　状态页（不支持 · 无从判断） | control | active | done | — | — |
| `inspector.txn_hint` | 　跨标签事务提示行 | control | active | done | — | — |
| `inspector.object.info.status` | 　　　表信息提示行（读取中 · 读取失败） | control | active | done | partial | — |

### `inspector` 信息面板

kind `area` · status `active`

- 样式：槽位 `inspector`；尺寸 `inspector_width`、`inspector_min`、`inspector_max`、`inspector_gap`
- 同步：Linux done（§6 面板存在 / 面板内容由谁驱动；shell.rs 分栏 clamp 与 window_state.rs 窗口级记忆）；Windows partial（§7.1 只记「SplitView 右侧信息面板」：宽度拖拽、窗口级记忆、选中驱动换页未记录。）；macOS none
- 实现备注：区域 06：工作区与面板共用一条 GtkPaned，面板是 end child，常驻显示（v0.2.3.2 起没有折叠按钮、没有 Ctrl+I，也没有 Ctrl+I 的菜单项）。分隔只由 Paned 自己的分栏线提供，面板不再画 border-left；面板只留内边距（inspector_gap）。宽度默认 inspector_width，钳在 inspector_min 与 inspector_max 之间；启动态、连接态、切换连接读同一个值。宽度落盘是窗口级记忆（独立窗口状态文件），不写进 connections.json。面板内容只由「选中」事件驱动（树、Objects 页列表、数据网格列头）；鼠标经过不改面板。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| drag | 面板常驻 | 拖动分栏线改宽度：按像素跟随，松手后写入窗口级记忆；宽度钳在 inspector_min 与 inspector_max 之间，越界就地回弹 | `src/shell.rs:249、src/shell.rs:230、src/window_state.rs:235` |
| load | 对象树的选中发生变化 | 按选中种类换页：连接 → 连接视图；库 / schema → 库视图；表 / 视图 / 物化视图 → 表视图；列 → 列视图；索引 / 触发器 → 状态页；序列 / 函数 / 过程 / 类型 → 只有 DDL；分组行或没有选中对象 → 空态 | `src/ui.rs:2101、src/inspector.rs:630` |

### `inspector.header` 标题块（标题 · 副标题）与分隔线

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_sm`、`gap_lg`、`gap_none`
- 同步：Linux done（§6 空态保留面板外壳；inspector.rs::new 的 header 与 Separator）；Windows partial（§7.1 记标题 = 连接名 / 库名、副标题 = 驱动 / 「库」；标题块留白与分隔线未记录。）；macOS none
- 实现备注：标题用 heading 类、单行省略；副标题 dim-label + caption，跟着选中换成驱动 / 种类 / 「库」。标题与副标题之间不留间距（gap_none）：两块读成一组；标题块下沿与分隔线之间留一段（gap_lg），副标题不贴线。分隔线是面板根容器的直接子控件，从左端铺满内容区；切换条在分隔线下方。空态与选中态共用这一套外壳：只有标题文字回到「信息面板」、副标题清空。

### `inspector.empty` 空态（内容区留白）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_xl`
- 同步：Linux done（§6 没有选中对象；§5 空态与选中态共用外壳（inspector.rs::show_empty））；Windows partial（无选中时内容区留白（RenderInspector 的默认分支只清空 InspectorBody，外壳与「信息面板」标题保留）；但选中分组行时 Windows 走库视图（带「当前分组」字段），没有回到规范要求的空态。）；macOS none
- 实现备注：空页是一个不带子控件的盒子：标题仍写「信息面板」、分隔线仍在，内容区留白；说明文字由标题承担（§5）。启动态还没有连上任何数据库时也走这一页，与连接态共用同一份实现，不存在「启动态无面板」这一档。分组行被选中时同样落到这一页：分组没有对象信息，面板不另立一套空白文案。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中分组行，或树里没有任何选中对象 | 保留面板外壳（标题「信息面板」+ 分隔线），副标题清空，内容区留白 | `src/inspector.rs:576、src/ui.rs:2152` |

### `inspector.connection` 连接视图（标题 · 副标题 · 字段行）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_sm`、`gap_md`、`gap_lg`
- 平台差异：Windows：§7.1：字段行给驱动、地址、默认库、用户、TLS、凭据引用（不含密码）、服务端版本、会话前 8 位；不记连接延迟与会话数。
- 同步：Linux done（§6 选中连接；inspector.rs::connection_facts 与 show_connection）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:937；frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:954；frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:963）；macOS none
- 实现备注：字段行顺序固定：驱动、地址、默认库、用户、TLS、凭据引用、服务端版本、连接延迟、会话数。地址是 host:port 或文件路径，不拼用户名与库名（它们各有独立字段）。服务器型驱动没有默认库时写「未指定」（吞掉整行会像漏了字段）；文件型驱动没有 TLS 一层，这一行整行不出现。凭据引用只写 credential_ref，面板里没有明文密码。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中连接行且连接在配置里 | 标题 = 连接名、副标题 = 驱动；字段行给驱动、地址（host:port 或文件路径）、默认库、用户、TLS、凭据引用（不含密码）、服务端版本、连接延迟、会话数；拿不到的字段不出现 | `src/ui.rs:2122、src/inspector.rs:586、src/inspector.rs:132` |
| load | 选中连接行但连接已不在配置里 | 状态页「连接已不在配置里」+「connections.json 里已经没有这条连接（config.invalid）。刷新对象树后重试。」 | `src/inspector.rs:590` |

### `inspector.field` 字段行（标签 · 值）

kind `row` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_sm`、`gap_none`
- 无障碍：可访问名「（行内容：字段标签 + 值）」
- 同步：Linux done（§5 拿不到的字段不出现；inspector.rs::fill_facts）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1256；frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1262）；macOS none
- 实现备注：两行垂直布局：标签 dim-label、值同字号同字重（font_panel，不加粗）；值逐字折行、可选中复制。值是长 token（如凭据引用）时按字符折行：按词折行会把面板的最小宽度顶大，面板就拖不回 inspector_width。值为空白时整行不渲染，所以「拿不到」表现为这一行不出现（§5），不用占位符号冒充。连接、库、列三种视图共用这一份行模板，不各自拼一套。

### `inspector.catalog` 库 / 分组视图（标题 · 副标题 · 计数）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_sm`、`gap_md`、`gap_lg`
- 同步：Linux done（§6 选中库 / catalog；inspector.rs::catalog_facts 与 tree.group_counts 接线）；Windows partial（标题 /「库」副标题与已展开分组的表、视图计数都实现了；但缺「计数说明：按已展开的结果统计（该层还有下一页）」，且分组行选中时进入本视图而不是回到空态。）；macOS none
- 实现备注：字段：名称、表计数、视图计数；计数只来自树里已经展开过的分组层，打开一个库不顺带扫整库。某个分组还没展开过时那一行不出现（不主动全量统计，也不写 0 冒充空库）。该层还有下一页时补一行「计数说明：按已展开的结果统计（该层还有下一页）」，不把当前页当总数。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中库 / schema 行 | 标题 = 库名、副标题「库」；字段行给名称与表 / 视图计数，计数只来自已展开的分组层，没展开过的那一行不出现 | `src/ui.rs:2132、src/inspector.rs:609、src/inspector.rs:167` |
| load | 选中的是分组行（Tables / Views / Functions / Queries 等） | 分组行没有对象信息：面板回到空态，标题回到「信息面板」 | `src/ui.rs:2152` |

### `inspector.object` 表 / 视图视图（标题 · 切换条 · 两页）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `gap_sm`、`gap_none`
- 同步：Linux done（§6 选中表 / 视图；inspector.rs::show_object 的 Table 分支）；Windows partial（表 / 视图的标题、两个独立按钮与两页切换都实现了；但序列 / 函数 / 过程 / 类型照样出现切换条并会去读列（规范要求这类对象只显示 DDL 页、切换条隐藏），标题也没有完整路径 tooltip。）；macOS none
- 实现备注：标题 = 对象名、副标题 = 对象种类；标题 tooltip 给完整路径。顶部固定出现切换条，默认落在 DDL 页；两页共用一个 stack，同一时刻只有一页可见。只切面板内的页，不改标签与树的选中。对象名与种类来自选中节点，不等 GetDdl / GetColumns 返回：切换期间面板不会先空一下。序列 / 函数 / 过程 / 类型没有表信息页：只显示 DDL 正文一页，切换条整条隐藏（不是出现切换条却点不动）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中表 / 视图 / 物化视图 | 标题 = 对象名、副标题 = 种类；显示 [DDL] / [表信息] 切换条，按当前标签记住的页恢复（默认 DDL） | `src/inspector.rs:630、src/inspector.rs:657` |
| load | 切换标签（看的还是同一个对象） | 只换「表页选择」的记忆键，按新标签记住的页恢复 DDL / 表信息；不重发 GetDdl / GetColumns，也不重建标签 | `src/inspector.rs:762、src/ui.rs:2248` |
| load | 选中序列 / 函数 / 过程 / 类型（有 DDL 的种类） | 标题 = 对象名、副标题 = 种类（标题 tooltip 给完整路径）；只显示 DDL 正文一页，不出现 [DDL] / [表信息] 切换条，也没有表信息页 | `src/inspector.rs:640、src/inspector.rs:697、src/inspector.rs:700` |

### `inspector.pane_switch` [DDL] / [表信息] 切换条

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `min_target`、`radius_sm`、`gap_xxs`、`gap_sm`、`gap_xs`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_mid`；状态 hover=`sqarvo_brand_soft`、selected=`sqarvo_brand_mid`
- 原生选型说明：原生 GtkStackSwitcher 的样式与命中区不符合「两个按钮各自贴边、各自 tooltip」，改用两个 set_group 的 GtkToggleButton 拼装（控件原生）。
- 同步：Linux done（§5 两个独立 toggle；theme.rs 的 .sqarvo-pane-switch 规则）；Windows partial（两个独立 Button（不是 ToggleButton、也不是分段容器）已实现；但切换条对表 / 视图以外的种类也出现，规范要求它只在表 / 视图视图里出现，按钮也没有 tooltip。）；macOS none
- 实现备注：两个独立 toggle，不用 .linked 分段容器：选中块是贴着文字的小圆角块，四角都圆。命中区仍不小于 min_target，缩的是内边距；hover 是 brand_soft 的半透明档，选中是 brand_mid 的半透明档（实底会把小号正文压成一块砖）。拉丁标签「DDL」比「表信息」多一小段上边距做光学补偿：行盒居中会让前者看着偏高。切换条与标题块、表信息行、DDL 正文共用一条左基准线（分隔线的左顶点）。切换条只在表 / 视图视图里出现；其它视图与状态页隐藏。

### `inspector.pane_switch.ddl` 「DDL」按钮

kind `control` · status `active` · parent `inspector.pane_switch`

- 样式：槽位 `inspector`；尺寸 `min_target`、`radius_sm`
- 无障碍：可访问名「显示该对象的 DDL」；Tab 可聚焦；面板内 Tab 次序为 DDL → 表信息
- 同步：Linux done（§6 切换表页；inspector.rs::install_toggles / set_pane）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1053）；macOS none
- 实现备注：文字标签「DDL」，tooltip 同时登记为可访问名。与「表信息」互斥：按下本按钮即取消另一个的按下态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前是表 / 视图视图 | 切到 DDL 页；选择记到当前标签（标签生命周期内有效），不改变标签与树的选中 | `src/inspector.rs:549、src/inspector.rs:779` |

### `inspector.pane_switch.info` 「表信息」按钮

kind `control` · status `active` · parent `inspector.pane_switch`

- 样式：槽位 `inspector`；尺寸 `min_target`、`radius_sm`
- 无障碍：可访问名「列、主键、索引与触发器、已加载行数」；Tab 可聚焦；面板内 Tab 次序为 DDL → 表信息
- 同步：Linux done（§6 切换表页；inspector.rs::install_toggles / apply_pane）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1058）；macOS none
- 实现备注：文字标签「表信息」，tooltip 同时登记为可访问名。与「DDL」互斥：按下本按钮即取消另一个的按下态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 当前是表 / 视图视图 | 切到表信息页；选择记到当前标签（标签生命周期内有效），不改变标签与树的选中 | `src/inspector.rs:549、src/inspector.rs:783` |

### `inspector.object.info` 表信息页（列 · 主键 · 索引 · 触发器 · 已加载行数）

kind `container` · status `active` · parent `inspector.object`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_lg`、`gap_sm`
- 同步：Linux done（§6 表信息页；inspector.rs::render_table_info 的五节）；Windows partial（§7.1 只记表信息的列一节（四列表头）；主键 / 索引 / 触发器 / 已加载行数几节未记录。）；macOS none
- 实现备注：分节渲染：列、主键、索引、触发器、行；没有数据的节整节不出现。正文与 DDL 正文同字号同字重（font_panel、不加粗），层级只靠 dim 标签与分节间距。索引 / 触发器读取失败时那一节换成「读取失败」+「<文案>（<稳定错误码>）」，已读回的列仍然显示。内容多了纵向滚动，宽度不够时横向滚动，不把字段挤成两三字一行。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 切到表信息页且列 / 主键 / 索引 / 触发器至少一项已返回 | 按节渲染已读回的内容：列、主键、索引、触发器、已加载行数；索引与触发器读取失败时保留错误码 | `src/inspector.rs:1015、src/inspector.rs:1037、src/inspector.rs:1107` |

### `inspector.object.info.columns` 列清单（列名 · 类型 · 可空 · 主键）

kind `container` · status `active` · parent `inspector.object.info`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_xxs`、`gap_sm`
- 平台差异：Linux：Linux 渲染为每列一张卡片：标题「列名 · 类型」+ 可空 / 默认值 / 字符集 / 排序规则四行；主键另起一节，不画四列表头。
- 同步：Linux done（§6 表信息给列与主键；inspector.rs 列节的渲染）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1203；frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1208）；macOS none
- 实现备注：列的四个口径：列名、类型、可空（可空 / 不可空（NOT NULL）两态）、主键标记。主键读不到时整列不打任何主键标记，也不把「读不到」画成「不是主键」。列清单为空时写「服务端返回了空列清单」，不画空表头。列名过长单行省略，全文进 tooltip。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 列已读回（GetColumns 成功） | 按列顺序给出列名、类型、可空、主键标记；主键读不到时整列不打标记 | `src/inspector.rs:1037、src/inspector.rs:1044、src/column_info.rs:95` |

### `inspector.object.info.columns.row` 列行（列名 · 类型 · 可空 · 主键标记）

kind `row` · status `active` · parent `inspector.object.info.columns`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_xxs`、`gap_sm`
- 无障碍：可访问名「（行内容：列名 + 类型 + 可空 + 主键标记）」
- 同步：Linux done（§6 表信息页的列；inspector.rs 列卡片的 compact_row 四行）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1208；frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1230）；macOS none
- 实现备注：不是可点行：列属性只在面板里读，点它不打开标签、不发请求。默认值三态与两档缺失分开写：「未知（服务端未提供）」不等于「无默认值」，字符集 / 排序规则缺失写「不适用」。行高随内容（可空与默认值可能折行），不套用网格的 row_height_data。

### `inspector.object.info.pk` 主键节（主键列 · 读不到时的说明行）

kind `control` · status `active` · parent `inspector.object.info`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_xs`、`gap_sm`
- 无障碍：可访问名「主键」
- 平台差异：Linux：说明走在表信息页顶部的提示行（info_status），不在主键节里；主键节只在 GetPrimaryKey 读回后渲染。
- 同步：Linux done（§6 表信息给主键；inspector.rs::primary_key_label 与失败路径 info_note）；Windows partial（主键读不到时的说明行与列尾的 PK 标记都有；但没有规范要求的主键节「主键列」一行，也没有「无主键」这句，主键只能从列尾标记读出来。）；macOS none
- 实现备注：复合主键按主键内位置排序后用逗号连起来，不按名字排序。确实没有主键时写「无主键」；读不到主键是另一回事，给一行说明，不写成「无主键」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | GetPrimaryKey 读回 | 「主键列」一行按主键内顺序给列名（复合主键按位置排、逗号分隔）；确实没有主键时写「无主键」 | `src/inspector.rs:1077、src/inspector.rs:192` |
| load | GetPrimaryKey 失败 | 给一行说明（错误 headline + 「<文案>（<稳定错误码>）」），列清单里不画任何主键标记 | `src/inspector.rs:915、src/inspector.rs:1008` |

### `inspector.object.ddl` DDL 正文

kind `container` · status `active` · parent `inspector.object`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_md`、`gap_lg`、`gap_none`
- 原生控件：gtk:GtkTextView / winui:等宽 TextBox（§7.1） / swift:—
- 平台差异：Windows：§7.1：DDL 正文用等宽字体、不折行、横向滚动、可选中复制；正文上方只在 exact=false 时有一行琥珀警示。；Linux：Linux 侧不单独设等宽字族：§5 要求 DDL 正文与表信息同字体同字号，正文走 .sqarvo-panel-text。
- 同步：Linux done（§6 DDL 页；ddl.rs::DdlPanel::new 的 TextView 与 ScrolledWindow）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1145；frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1159）；macOS none
- 实现备注：正文与表信息同字号同字重（font_panel）、不折行；长语句横向滚动，滚动条常驻（关掉 overlay 自动淡出，能滚到的地方要看得见）。正文可选中，复制走选中 + Ctrl+C；页内不画来源徽标、来源说明行与「复制」按钮。服务端返回空 DDL 时正文区写「服务端返回了空 DDL」+ 说明，不当成对象没有结构。没有浏览会话时正文区换状态页「这条连接没有浏览会话」+「对象所属连接的会话已断开：在对象树里点该连接重新连接后再试。」

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 切到 DDL 页（选中表 / 视图时默认落在这一页） | 发一次 GetDdl 并只显示正文：不折行、横向滚动且滚动条常驻、可选中复制；读取中写「正在读取 DDL…」+「等待 GetDdl 返回」 | `src/ddl.rs:231、src/ddl.rs:153、src/ddl.rs:167` |
| load | GetDdl 失败 | 正文区换状态页：标题给错误 headline、说明行给「<文案>（<稳定错误码>）」；§7.1 的文案口径为「DDL 读取失败：<文案>（<错误码>）」，不是空面板 | `src/ddl.rs:302、src/ddl.rs:312` |

### `inspector.object.ddl.warning` DDL 琥珀警示（exact=false）

kind `control` · status `active` · parent `inspector.object.ddl`

- 样式：槽位 `inspector`；尺寸 `font_status`、`gap_sm`、`gap_xxs`；颜色 `warning_color`
- 无障碍：可访问名「DDL 来源警示」
- 同步：Linux done（§6 exact=false 时正文上方有琥珀警示；ddl.rs 的 warning 标签与样式表）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1129；frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:1132）；macOS none
- 实现备注：警示行在正文上方、可折行，颜色只走 @warning_color（与状态行的警告文字同源）。exact=true（数据库原文）时整行不出现：原文不再另写来源说明。文案里带来源说明（拼装器版本与缺失项），用户能看出这份 DDL 缺了什么。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | GetDdl 返回 exact=false（客户端拼装） | 正文上方出现一行琥珀警示「⚠ 这份 DDL 由客户端拼装，不是数据库原文；信息有损失 · <来源说明>」 | `src/ddl.rs:284、src/ddl.rs:286、src/ddl.rs:91` |

### `inspector.column` 列视图（六项属性）

kind `container` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_sm`、`gap_md`、`gap_lg`
- 同步：Linux done（§6 选中列（树 / 网格列头）；inspector.rs::show_column 与 load_columns_for_column）；Windows none（§7.1 未记录）；macOS none
- 实现备注：字段顺序固定：列名、原始类型、可空、默认值、字符集、排序规则。默认值四态分开写：「未知（服务端未提供）」/「无默认值」/「默认 NULL」/ 表达式原文（原文为空写「表达式（服务端未给出文本）」）。字符集 / 排序规则缺失写「不适用」；这两种缺失是数据库没有这一层，不是面板没读到。从树上的列节点进来时副标题补所属表名（「列 · 表名 · 类型」），从网格列头进来只有类型。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中列节点且父节点是表 / 视图 | 读父表的 GetColumns 再挑出这一列：标题 = 列名、副标题「列 · <表名> · <类型>」，字段行给 列名、原始类型、可空、默认值、字符集、排序规则 | `src/inspector.rs:672、src/inspector.rs:959、src/column_info.rs:156` |
| click | 点数据网格的列头（该表已打开） | 进同一个列页：用 GetColumns 已有模型，不重发 RPC | `src/inspector.rs:716、src/ui.rs:2204` |

### `inspector.state` 状态页（不支持 · 无从判断）

kind `control` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_lg`、`gap_md`
- 无障碍：可访问名「面板状态」
- 同步：Linux done（§6 选中索引 / 触发器、选中列（无父表）；inspector.rs::StateText）；Windows none（§7.1 未记录）；macOS none
- 实现备注：权限不足、对象不存在、不支持种类都给可读文案 + 稳定错误码。状态页会挤掉 DDL 与表信息两页的可见性，切换条同时隐藏：面板不会出现「空页 + 切换条」这种半截状态。换对象时上一次的状态页文案要清掉，防止别的对象读到旧错误。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 选中索引 / 触发器（MariaDB 没有独立 DDL） | 状态页「该对象种类不支持查看 DDL」+「<种类>没有独立的 DDL：定义在所属表的 DDL 里（metadata.object_kind_unsupported）。换一个表 / 视图再试。」；不是空面板 | `src/inspector.rs:704、src/inspector.rs:250` |
| load | 选中的列节点不在表 / 视图下面（拿不到父表） | 状态页「列的详细属性需要知道所属的表」+「从对象树里表的子节点进入列，才能读到 GetColumns 的完整属性。（metadata.object_not_found）」 | `src/inspector.rs:688` |

### `inspector.txn_hint` 跨标签事务提示行

kind `control` · status `active` · parent `inspector`

- 样式：槽位 `inspector`；尺寸 `font_status`、`gap_md`
- 无障碍：可访问名「未提交事务提示」
- 同步：Linux done（§6 跨标签事务提示；inspector.rs::set_txn_hint）；Windows none（§7.1 未记录）；macOS none
- 实现备注：一行可折行的 caption，没有未提交事务时整行隐藏（不占位）。位置在标题块与分隔线之间：读面板内容之前先看到「别处还有没提交的事务」。当前挂的 sqarvo-inspector-warning 类在 theme.rs 里还没有对应规则，颜色随正文色——警示色待补（未进 theme.rs 的 token）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 同一 catalog 的其它标签有未提交事务 | 面板顶部常驻一行「同一 catalog 的其它标签还有未提交事务：<标签名…>」；没有则整行隐藏 | `src/inspector.rs:809、src/ui.rs:1832` |

### `inspector.object.info.status` 表信息提示行（读取中 · 读取失败）

kind `control` · status `active` · parent `inspector.object.info`

- 样式：槽位 `inspector`；尺寸 `font_panel`、`gap_sm`
- 无障碍：可访问名「表信息状态」
- 同步：Linux done（§6 表信息页的失败路径；inspector.rs::info_note 与 load_table 的提示行）；Windows partial（§7.1 只记「列读取失败：…」；「正在读取列、主键与索引…」与没有浏览会话时的说明未记录。）；macOS none
- 实现备注：提示行在表信息页最上面、可折行；有分节渲染出来后它退场。读取失败时提示行留在最上面，已读回的分节继续显示（不清空、不盖成空面板）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 表信息页开始读取 | 写「正在读取列、主键与索引…」；没有浏览会话时写「这条连接没有浏览会话：列属性读不到（metadata.object_not_found）。在对象树里重新连接后再试。」 | `src/inspector.rs:872、src/inspector.rs:873` |
| load | 列 / 主键读取失败 | 提示行给错误 headline + 「<文案>（<稳定错误码>）」；§7.1 的文案口径为「列读取失败：<文案>（<错误码>）」，不是空面板 | `src/inspector.rs:1008、src/inspector.rs:893` |

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
- 同步：Linux done（v0.2.5 Linux 客户端；§2.1 逐条对照 tree.rs）；Windows partial（F5 已接（`:273` 起的 KeyboardAccelerator：当前是表数据页就刷那张表并走脏行守卫，否则 `RebuildTree()` 并把「正在重新读取对象树…」写进状态行行 2 左格）；侧栏仍是 TreeView + SplitView，无搜索沟、无侧栏提示行。）；macOS none
- 实现备注：侧栏只有对象树：没有页脚按钮、没有刷新按钮、没有平铺列表、没有分页统计。搜索框与搜索状态行不画在侧栏里，放在状态栏左边沟（见 object_tree.search），宽度跟随分栏线。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key F5 | 工作区存在，且当前标签不是表数据页（查询标签 / Objects 页 / 启动态） | 整棵树（全部连接）重建：作废已连接连接的层再重读连接层，状态行写「正在重新读取对象树…」；当前标签是表数据页时 F5 刷的是那张表，不重建树 | `src/ui.rs:2495、src/ui.rs:2510、src/tree.rs:1453` |

### `object_tree.tree` 树本体

kind `container` · status `active` · parent `object_tree`

- 样式：槽位 `sidebar`；尺寸 `row_height_tree`、`badge_min_height`
- 无障碍：可访问名「对象树」；Tab 可聚焦；Tab 进入后 ↑ / ↓ 移动选中
- 原生控件：gtk:GtkListView / winui:TreeView / swift:—
- 同步：Linux done（§2.5 键盘表 + tree.rs install_handlers）；Windows partial（TreeView 承载，↑ / ↓ / Enter 同语义；无自定义按键控制器差异待核。）；macOS none
- 实现备注：GtkListView + 嵌套 ListStore，每层一个 Level，层级按 source 复用（收起不作废层）。折叠后重新展开复用已读内容、不发请求。层级链固定：`My Connections`（固定标题行，不是可折叠节点）→ 连接（来自 connections.json，与是否已连接无关）→ 库 / schema → Tables / Views / Functions / Queries → 对象；表 → Fields / Indexes / Foreign Keys / Triggers；视图下直接列出列，没有分组层。树没有自定义按键控制器：除 ↑ / ↓ / Enter 与 Ctrl+F 外沿用 GTK 默认；↑ / ↓ 是 ListView + SingleSelection 的默认行为，代码不额外接线。

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

- 无障碍：可访问名「（行内容：连接名或别名 + 连接态徽标）」
- 图标：`tree.connection`
- 同步：Linux done（§2.2 / §2.1 连接行；object_activation 与 connect_saved）；Windows partial（已保存连接 + 徽标、双击连接与展开同语义；目录页大小 200（Linux 500）。）；macOS none
- 实现备注：行首文字优先显示**别名**（`SavedConnection.alias`），别名为空或只有空白时回落到派生名（`user@host[/db]`、`文件名（SQLite）`）。别名只影响展示：连接身份键、会话显示名、标题规则与断开/删除用的键仍是被派生名与目标算出的那一份。tooltip 第一行是别名优先的标题；有别名时同一行里补「（连接名 …）」，免得同一个目标的第二条连接看不出区别。行尾是连接态徽标（已连接 / 未连接 / 连接中… / 断开中…），与搜索结果同一份文案；搜索同时匹配别名与连接名。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 连接行 | 已连接 → 展开 / 收起；未连接 → 按保存的凭据连接；凭据缺失 → 明确提示并打开预填对话框；连接中 / 断开中 → 忽略并写日志 | `src/tree.rs:769、src/tree.rs:1688、src/ui.rs:845` |
| load | 已连接 | 首次展开发一次目录层 ListNodes 列出库，每页 500（NODE_PAGE_SIZE），带自己的分页 token | `src/tree.rs:2858` |

### `object_tree.tree.row.catalog` 库 / schema 行

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：库 / schema 名）」
- 图标：`object.catalog`、`object.schema`
- 同步：Linux done（§2.1 展开库 / schema）；Windows partial（展开库 0 次 RPC、四个分组行一致；分层与徽标文案待逐条核对。）；macOS none
- 实现备注：库与 schema 不合并成一层：驱动给 schema 层就多一层，重名库靠「会话 + 库名」精确匹配。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 行可展开 | 展开 / 收起（展开库只出现分组行，分组仍保持收起） | `src/tree.rs:769` |
| load | 展开库 / schema | 0 次 RPC：只建 Tables / Views / Functions / Queries 四个静态分组行，且不自动展开 | `src/tree.rs:2858` |

### `object_tree.tree.row.group` 分组行（Tables / Views / Functions / Queries；表下 Fields / Indexes / Foreign Keys / Triggers）

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：分组名）」
- 图标：`tree.group.tables`、`tree.group.views`、`tree.group.functions`、`tree.group.queries`、`tree.table_group.fields`、`tree.table_group.indexes`、`tree.table_group.foreign_keys`、`tree.table_group.triggers`
- 同步：Linux done（§2.1 分组与空态文案）；Windows partial（四个分组行一致；Functions / Queries 只有入口；表下四组未实现（表节点是叶子）；分组页大小 200。）；macOS none
- 实现备注：未实现的分组照实写说明，不发请求：Functions / Queries 无 kinds，Foreign Keys 是 capability.unsupported。未实现分组展开只写一行说明：「函数列表尚未实现（点开看说明，不发请求）」「已保存的查询尚未实现（点开看说明）」「驱动未提供外键列表（capability.unsupported）：需要后端增加外键能力后再显示，这里不猜测、不拼装」。空分组的文案按组不同（这一组没有表 / 视图 / 函数 / 已保存的查询；这张表没有字段 / 索引 / 触发器）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 分组行 | 展开 / 收起 | `src/tree.rs:769` |
| load | 展开某个分组 | 发一次 ListNodes(filter.kinds=…)，该层有自己的分页 token；Functions / Queries 不请求；Foreign Keys 写 capability.unsupported 说明 | `src/tree.rs:2858、src/tree.rs:376` |
| load | 服务端返回空 | 说明行按组给文案：「（这一组没有表）」「（这一组没有视图）」「（这一组没有函数）」「（还没有保存的查询）」「（这张表没有字段 / 索引 / 触发器）」 | `src/tree.rs:365、src/tree.rs:446、src/tree.rs:507` |

### `object_tree.tree.row.object` 表 / 视图行

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：对象名 + 种类）」
- 图标：`object.table`、`object.view`
- 同步：Linux done（§2.2 双击表 / 视图）；Windows partial（双击 / Enter 打开数据页、同表复用标签一致；表节点无 Fields / Indexes / FK / Triggers 子组。）；macOS none
- 实现备注：可打开优先于可展开：表虽然有四组子节点，双击仍是打开数据标签。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 行可打开 | 打开表数据标签（同一张表在对象名 + catalog + 连接键三者一致时复用已有标签，不新建会话、不重载数据） | `src/tree.rs:769、src/tabs.rs:1608` |
| load | 展开表节点 | 0 次 RPC：只建 Fields / Indexes / Foreign Keys / Triggers 四个静态分组行，且不自动展开任何一组；展开某一组才发那一次 ListNodes | `src/tree.rs:2681、src/tree.rs:2704` |

### `object_tree.tree.row.field` 列 / 索引 / 触发器行

kind `row` · status `active` · parent `object_tree.tree.row`

- 无障碍：可访问名「（行内容：列 / 索引 / 触发器名）」
- 图标：`object.column`、`object.index`、`object.trigger`、`object.type`
- 同步：Linux done（§2.2 双击列 / 索引 / 触发器）；Windows none（表节点是叶子，四组尚未实现（§9 已知偏差）。）；macOS none
- 实现备注：叶子对象：没有子层、没有菜单、不发请求。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 叶子对象行 | 无动作（不发请求） | `src/tree.rs:769` |

### `object_tree.tree.row.placeholder` 占位行（读取中 / 加载更多 / 读取失败 / 空说明 / 未实现）

kind `row` · status `active` · parent `object_tree.tree.row`

- 样式：尺寸 `row_height_tree`、`font_status`
- 无障碍：可访问名「占位行」
- 图标：`tree.note.loading`、`tree.note.warning`、`tree.note.info`
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

- 样式：槽位 `status_bar.left_gutter`；尺寸 `font_ui`、`secondary_min_height`
- 无障碍：可访问名「搜索连接、库、表」；Tab Ctrl+F 聚焦；Esc 清空并回到树页
- 同步：Linux done（§2.4 搜索；Ctrl+F / Enter / Esc 三键）；Windows none（无搜索框。）；macOS none
- 实现备注：placeholder「搜索连接、库、表...」；匹配语义是 LIKE %关键词%，不做分词。连接名对注册表里全部连接在客户端做大小写不敏感 contains；库 / 表 / 视图只查已打开（已连接、有浏览会话）的连接，逐库发 ListNodes 名称包含，服务端下推 LIKE，前端不拼通配符。

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
- 实现备注：结果行复用 object_tree.tree.row：行结构、行高、行尾文字与树逐控件一致。v0.2.3.4 起不再有命中高亮 / 加粗：命中行与树里同一行像素一致。只出现「连接名命中」或「其中有命中」的连接；命中的库 / 分组作为祖先一起出现并默认展开；同一库只一行，多个表命中归到同一个 Tables 下；祖先行箭头可收起 / 再展开，收起状态按路径记、重新搜索仍生效。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表 / 视图行 | 打开该对象的数据标签（与树双击同一条 open）；侧栏仍停在结果页，搜索框与命中数原样保留 | `src/tree.rs:2094、src/tree.rs:4501` |
| click | 库行 | 在树里逐层展开祖先并选中（切到树页） | `src/tree.rs:3469、src/tree.rs:2094` |
| click | 连接行 | 按该连接「点连接」同一条路径（未连接则连接、已连接则展开），并清空搜索框退出筛选 | `src/tree.rs:2094` |
| click | 分组行 | 只有行内箭头能收起 / 展开；点行本身没有可点目标、不触发打开 | `src/tree.rs:2094` |

### `object_tree.ctx_menu` 右键菜单

kind `container` · status `active` · parent `object_tree`

- 样式：槽位 `overlay`
- 原生控件：gtk:GtkPopoverMenu / winui:— / swift:—
- 同步：Linux done（§2.3 右键菜单）；Windows none（对象树没有右键菜单（§7.1 未记录）。）；macOS none
- 实现备注：贴着点击位置的 GtkPopover；点任一条目先收起菜单再执行动作；占位行不给菜单。条目按行种类给、顺序固定：连接（已连接）断开连接 → 刷新连接 → 编辑连接… → 删除连接；连接（未连接 / 失败）连接（使用已保存的凭据）→ 编辑连接… → 删除连接；表 打开 → 转储表…；视图只有 打开；库 / schema 刷新库；分组 刷新这一组。没有「查看 DDL」条目：DDL 在信息面板的 [DDL] 页；视图没有「转储表」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| right_click | 非占位行 | 按行种类给条目；占位行与列 / 索引 / 触发器行直接返回（menu_items 为空） | `src/tree.rs:3191、src/tree.rs:3313` |

### `object_tree.ctx_menu.connect_saved` 「连接（使用已保存的凭据）」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「连接（使用已保存的凭据）」
- 同步：Linux done（§2.3 连接（使用已保存的凭据））；Windows none（无树右键菜单。）；macOS none
- 实现备注：只读 credential_ref，不读也不写明文密码。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接未连接 / 失败 | 与左键双击同一条 connect_saved；失败给出可读原因 | `src/tree.rs:3230、src/ui.rs:845` |

### `object_tree.ctx_menu.disconnect` 「断开连接」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「断开连接」
- 同步：Linux done（§2.3 断开连接）；Windows none（无树右键菜单。）；macOS none
- 实现备注：只关闭该连接的浏览会话（树 / 搜索 / DDL）；数据页用另一条会话，已加载数据仍可查看；其它连接的浏览会话不受影响。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该连接已连接 | 先确认「断开连接？」（说明影响范围；按钮 取消 / 断开连接，破坏性默认取消），确认后才断开 | `src/tree.rs:3230、src/ui.rs:2954` |

### `object_tree.ctx_menu.refresh_connection` 「刷新连接」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「刷新连接」
- 同步：Linux done（§2.3 刷新连接）；Windows none（无树右键菜单。）；macOS none
- 实现备注：就地清空该层 store 再发第一页，不收起子树；一层都没建立过时不发请求。只在右键菜单里，没有快捷键。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该连接已连接 | 重读这条连接已经建立的层；一层都没有时侧栏底部提示行写「这条连接还没有展开过：展开时会重新读取」 | `src/tree.rs:3407、src/tree.rs:3376、src/tree.rs:1936` |

### `object_tree.ctx_menu.refresh_catalog` 「刷新库」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「刷新库」
- 同步：Linux done（§2.3 刷新库）；Windows none（无树右键菜单。）；macOS none

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 库 / schema 节点 | 重读该库下已展开的分组与直接子对象层；分组全收起时只清缓存，提示行写「这个库的分组都收起着：展开时重新读取」 | `src/tree.rs:3434、src/tree.rs:1936` |

### `object_tree.ctx_menu.refresh_group` 「刷新这一组」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「刷新这一组」
- 同步：Linux done（§2.3 刷新这一组）；Windows none（无树右键菜单。）；macOS none
- 实现备注：静态分组行（Groups / TableGroups）没有服务端内容，刷新直接返回。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 分组节点 | 已展开（层已建立）→ 重读该组第一页；未展开过 → 不发请求，提示行写「这个分组还没展开：展开时会重新读取」（4 秒后自行收起）并记日志 | `src/tree.rs:3347、src/tree.rs:1936` |

### `object_tree.ctx_menu.edit_connection` 「编辑连接…」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「编辑连接…」
- 同步：Linux done（§2.3 编辑连接；--qa-tree-menu-connection + --qa-tree-menu-pick 编辑连接… → 日志「已用已保存连接…预填对话框」）；Windows none（无树右键菜单。）；macOS none
- 实现备注：密码永不回显：已有 credential_ref 时密码行留空、复用引用；不会自动连接。菜单的 popover 锚在**树视图**上（不是行控件）：GtkListView 会回收行控件，锚在行上时树一刷新菜单就消失，点击落到树行 →「点了没反应」。编辑时若已存在对话框，先丢弃旧实例再按这条连接的键重建并预填：对已关闭的 AdwDialog 调 present() 是空操作。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接存在于配置里 | 打开连接对话框并预填非敏感字段（含别名） | `src/tree.rs:3347、src/ui.rs:1107` |

### `object_tree.ctx_menu.delete_connection` 「删除连接」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「删除连接」
- 同步：Linux done（§2.3 删除连接）；Windows none（无树右键菜单。）；macOS none
- 实现备注：配置删除与密钥删除分开报告：配置删除失败提示「删除连接失败」；配置已删而密钥没删提示「配置已删除，密钥条目未删除（<code>）：<detail>」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 连接存在于配置里 | 二次确认「删除连接？」（写明从 connections.json 删除、树节点与浏览会话一并消失、其它连接不受影响）；确认后关会话 → 删配置 → 尽量删密钥条目 | `src/ui.rs:1310、src/ui.rs:454` |

### `object_tree.ctx_menu.open` 「打开」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「打开」
- 同步：Linux done（§2.3 表 / 视图节点）；Windows none（无树右键菜单。）；macOS none

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表 / 视图节点 | 与双击同一条路径打开数据标签 | `src/tree.rs:3191、src/tree.rs:3230` |

### `object_tree.ctx_menu.dump_table` 「转储表…」

kind `item` · status `active` · parent `object_tree.ctx_menu`

- 样式：尺寸 `font_ui`
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

## Objects 固定页（`objects_page`）

工作区最左侧的固定页：标题「Objects」+ 三列表格（行号 · 表名 · 类型）+ 状态行 +「加载更多…」。列表内容跟着对象树的选中换作用域，页面自己发 ListNodes 并持有自己的分页 token；不新建会话、不反写树的选中。

| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |
|---|---|---|---|---|---|---|
| `objects_page` | Objects 固定页 | area | active | done | partial | — |
| `objects_page.tab` | 　固定页页签（pinned，无关闭按钮） | control | active | done | partial | — |
| `objects_page.table` | 　三列表格（行号 · 表名 · 类型） | container | active | done | partial | — |
| `objects_page.table.row` | 　　行模板（行号 · 图标 + 名称 · 类型） | row | active | done | partial | — |
| `objects_page.table.row.number` | 　　　行号格 | cell | active | done | partial | — |
| `objects_page.table.row.name` | 　　　表名格（图标 + 名称） | cell | active | done | partial | — |
| `objects_page.table.row.kind` | 　　　类型格 | cell | active | done | partial | — |
| `objects_page.status` | 　状态行 | control | active | done | partial | — |
| `objects_page.more` | 　「加载更多…」 | control | active | done | done | — |
| `objects_page.empty` | 　空态页 | control | active | done | — | — |

### `objects_page` Objects 固定页

kind `area` · status `active`

- 样式：槽位 `workspace.content`；尺寸 `gap_xl`、`gap_md`
- 同步：Linux done（§3 页面结构逐条对照 tabs.rs（ObjectsPage::new 的 root 只有 title + stack））；Windows partial（§7.1 记录「固定 Objects 页 + 三列表格 + 状态行 + 加载更多…」的存在；页内标题、无作用域说明行的版面与页内距未逐条记录。）；macOS none
- 实现备注：页内自上而下只有四件东西：标题「Objects」、三列表格、状态行、「加载更多…」；标题是静态标签（13 semibold，无交互），v0.2.3.3 起页面没有作用域说明行 —— 作用域只由列表内容表达。页面没有自己的数据库会话：列表读这条连接的浏览会话（MetadataHub），与标签会话无关；关掉标签也不会动它。启动态（未连接）用同一个 ObjectsPage 实现，只换空态文案（见 objects_page.empty）。页内距 12 / 元素间距 8（gap_xl / gap_md），与数据页不同：这里没有工具条。

### `objects_page.tab` 固定页页签（pinned，无关闭按钮）

kind `control` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.tabstrip`；尺寸 `objects_min_width`、`tab_strip_height`
- 无障碍：可访问名「Objects」；Tab 可聚焦；固定在标签条首位
- 图标：`tab.objects`
- 同步：Linux done（§3 固定页存在 / 关闭 Objects；add_objects_page 的 set_page_pinned 与 on_close_page 的 Objects 分支）；Windows partial（§7.1 记录「Objects 固定页仍钉在首位、IsClosable=false，不占标签名额」与「宽 objects_min_width、不可关闭」；紧凑只显示图标、图标 view-grid-symbolic 未记录。）；macOS none
- 实现备注：AdwTabPage:pinned = true：固定在最左、紧凑（只显示图标）、没有关闭按钮，界面上不存在可点的 × —— 比「点关闭时拒绝」更贴近固定页语义。页签文本「Objects」，图标 view-grid-symbolic（与主工具条种类入口同一枚）。不占标签名额：8 个上限只数「有会话的标签」，Objects 不在其中，开满 8 个仍能切回本页。启动态建的是同款固定页：startup.rs 自建 ObjectsPage 后同样 set_page_pinned(true)，并另装一个兜底 close-page 拦截（日志同一句）——两档的固定页语义与文案只有一份。页签 id 固定 OBJECTS_TAB_ID，由 TabSet::new 在其它标签之前建一次（add_objects_page），工作区里不会出现第二个 Objects 页。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 工作区建立 / 启动态落地 | 在标签条首位建出本页：标题「Objects」、图标 view-grid-symbolic、pinned（最左、紧凑、无关闭按钮、不占 8 个标签名额）；首屏是空态页，不是空白列表 | `src/tabs.rs:1745、src/tabs.rs:1761、src/tabs.rs:1765、src/startup.rs:178` |
| click | 页签可见 | 只切到本页（select_index(0)）：不重发请求、不换作用域；随后清掉行选中（列表内容与信息面板都不动），窗口标题 / 状态栏 / 工具条高亮按当前种类刷新 | `src/tabs.rs:1361、src/tabs.rs:1366` |
| key Ctrl+W | 当前页是 Objects | 拒绝关闭（Propagation::Stop）并记一行「Objects 页固定存在，不可关闭」；页面不动、没有确认框 | `src/tabs.rs:2645、src/tabs.rs:2657` |
| click | 中键点页签，或任何程序化 close_page 请求（当前页是 Objects） | 与 Ctrl+W 同一条 on_close_page：拒绝（Propagation::Stop）并记同一句日志；页面不动、不弹确认框——关闭请求的来源不影响结论 | `src/tabs.rs:1377、src/tabs.rs:2655` |

### `objects_page.table` 三列表格（行号 · 表名 · 类型）

kind `container` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`row_number_width`、`column_width`、`column_header_min_height`、`frame_border_width`、`radius_md`、`font_ui`、`font_weight_title`；颜色 `sqarvo_frame`、`sqarvo_separator`
- 无障碍：可访问名「对象列表」；Tab 可聚焦；Tab 进入后 ↑ / ↓ 移动选中
- 原生控件：gtk:GtkColumnView / winui:待核 / swift:—
- 同步：Linux done（§3 页面结构 / 列宽列序 / 作用域五种来源 / 作用域未变；grid 与 selection 接线逐条对照）；Windows partial（§7.1 记录三列表格、行号宽 row_number_width、同库同组不重发 ListNodes；第二列表头写「名称」（Linux「表名」），「类型」列宽 120 与行高 row_height_data 未记录。）；macOS none
- 实现备注：GtkColumnView + GtkSingleSelection：autoselect(false)、can_unselect(true) —— 进页不自动选中首行，选中是用户的动作、也是信息面板的输入。列序固定「行号 → 表名 → 类型」，set_reorderable(false)：拖不乱；表头文本就是这三个词，单行。行号列 fixed_width 取 row_number_width 且 resizable(false)（不可拖）；表名列 expand + 可拖，初值 column_width；「类型」列 fixed_width 120 是代码常量 OBJECTS_KIND_COLUMN_WIDTH，未进 metrics.toml（P1 补齐）。行高 row_height_data，行 / 列分隔线由 GTK 画（颜色同源 @sqarvo_separator）；外框挂在 ScrolledWindow 上（.sqarvo-table-frame：frame_border_width + radius_md，overflow hidden 裁首末行底色），行与单元格不加整线。横竖滚动条常驻可见（overlay_scrolling(false)）：把「表名」拖宽后仍能滚回行号列。换密度档时只重设行号 / 表名两列宽度（reapply_tier），不重建列表、不重发请求。整个表格与数据网格逐像素同口径：行高、表头高度、悬停 / 选中底色、外框都是同一组选择器（theme.stylesheet 里 .sqarvo-grid 与 .sqarvo-objects 并列）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key ↑ / ↓ | 表格有焦点且已有行 | 移动选中（SingleSelection 默认行为）；选中变化与单击同效 —— 同步右侧信息面板 | `src/tabs.rs:680、src/tabs.rs:917` |
| key Enter | 有选中行 | 等同于双击：打开该表的数据标签（同一个 activate 信号，同一张表复用已有标签） | `src/tabs.rs:938、src/tabs.rs:1750` |
| load | 树里选中库 / 它下面的 Tables 分组 / 表内分组（Fields 等）/ 某张表 | 列出该库的表（filter.kinds=[TABLE]）；页面自己请求，树没展开过也能看到完整列表 | `src/ui.rs:1346、src/ui.rs:1357、src/ui.rs:2093` |
| load | 树里选中 Views 分组 / 某个视图 | 列出该库的视图（[VIEW, MATERIALIZED_VIEW]） | `src/ui.rs:1354、src/ui.rs:1360、src/ui.rs:1366` |
| load | 树里选中 Functions / Queries 分组 | 不发请求、不摆假数据：切到空态页写未实现说明，并清空列表与分页 token | `src/ui.rs:1369、src/ui.rs:1372、src/tabs.rs:984` |
| load | 树里选中连接行 / 占位行（没有作用域） | 回到空态页：不是空白，也不残留上一批行；分页 token 一并清掉 | `src/ui.rs:1377、src/tabs.rs:1013` |
| load | 树里换了选中，但作用域仍是同一个库、同一类（例如从库切到它下面某张表） | 不重发 ListNodes：行保留、列表不闪；只清掉行高亮（高亮属于上一次浏览） | `src/tabs.rs:1032、src/tabs.rs:1044、src/tabs.rs:1047` |
| load | 从别的标签切回本页 | 清掉行选中；列表内容与右侧信息面板都不动，不重发请求 | `src/tabs.rs:1366、src/tabs.rs:1202` |

### `objects_page.table.row` 行模板（行号 · 图标 + 名称 · 类型）

kind `row` · status `active` · parent `objects_page.table`

- 样式：槽位 `workspace.content`；尺寸 `row_height_data`、`gap_sm`、`gap_md`、`icon_box`；颜色 `sqarvo_brand_soft`、`sqarvo_brand_mid`、`sqarvo_brand_on_mid`；状态 hover=`sqarvo_brand_soft`、selected=`sqarvo_brand_mid`
- 无障碍：可访问名「（行内容：行号 + 对象名 + 种类）」
- 同步：Linux done（§3 单击行 / 双击回车行 + theme.stylesheet 的行状态规则）；Windows partial（§7.1 只记录 ↑ / ↓ 移动、Enter 打开（同步信息面板、不反写树的选中）；双击打开与悬停 / 选中底色未记录。）；macOS none
- 实现备注：hover 实际是 alpha(@sqarvo_brand_soft, 0.55)；选中 @sqarvo_brand_mid + 前景 @sqarvo_brand_on_mid，与对象树行、数据网格行同一条规则（三个选择器并列，选中在前、hover 在后靠顺序让中档赢）。行只由可见行创建控件（GtkColumnView 虚拟化），几百行也不卡主线程；模型里只放数据。单元格上下内距归零，行高才落在 row_height_data（否则「图标 + 名称」的格子会被 Adwaita 撑到 35）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 行存在 | 只选中：进入选中态并同步右侧信息面板；不反写树的选中（会形成回环并跳走浏览位置），不打开、不发请求 | `src/tabs.rs:917、src/tabs.rs:1008` |
| double_click | 行存在 | 打开表数据标签（GtkColumnView::activate → 与树双击同一条 open_table；同一张表复用已有标签，不新建会话、不重载数据） | `src/tabs.rs:938、src/tabs.rs:1795` |
| hover | 行存在 | 底色换成品牌浅档（alpha 55%），前景不动；已选中的行 hover 仍是中档，不会退回系统强调色 | `src/theme.rs:314` |

### `objects_page.table.row.number` 行号格

kind `cell` · status `active` · parent `objects_page.table.row`

- 样式：尺寸 `row_number_width`、`font_status`、`font_weight_ui`
- 无障碍：可访问名「（行内容：第 N 行）」
- 同步：Linux done（§3 行内容（行号列）；number_factory 的 bind 与 tooltip）；Windows partial（§7.1 记录行号宽 row_number_width、右对齐、等宽字体；「第 N 行」tooltip 与 11px 字号未记录。）；macOS none
- 实现备注：行号 = 模型位置 + 1（不是主键）：右对齐、11px monospace、色弱一档（.sqarvo-row-number）；tooltip「第 N 行」。回收的行控件可能还挂着旧行：item() 为空时清空文本，不显示上一行的行号。

### `objects_page.table.row.name` 表名格（图标 + 名称）

kind `cell` · status `active` · parent `objects_page.table.row`

- 样式：尺寸 `column_width`、`icon_box`、`gap_md`、`gap_sm`、`font_data_cell`、`font_weight_cell`
- 无障碍：可访问名「（行内容：图标 + 对象名）」
- 同步：Linux done（§3 行内容（表名列：图标 + 名称 + tooltip）；name_factory 的 bind）；Windows partial（§7.1 记录第二列为「名称」（宽 column_width）；表头文案写「名称」、种类图标与 tooltip 文案未记录。）；macOS none
- 实现备注：GtkBox（图标 + Label）：图标与对象树共用 kind_icon 映射（表 view-grid、视图 view-reveal），尺寸取 icon_box；名称 xalign 0、hexpand、过长省略号。单元格内距与数据网格一致（左右各 gap_sm，图标与名称间距 gap_md）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| hover | 行存在 | 悬停显示 tooltip「<名称>（<种类>）· 双击打开」（tooltip 挂在整格上，不是只挂在文字上） | `src/tabs.rs:759` |

### `objects_page.table.row.kind` 类型格

kind `cell` · status `active` · parent `objects_page.table.row`

- 样式：尺寸 `font_data_cell`、`font_weight_cell`、`gap_sm`、`gap_md`
- 无障碍：可访问名「（行内容：表 / 视图 / 物化视图）」
- 同步：Linux done（§3 行内容（类型列）；kind_factory 的 setup / bind）；Windows partial（§7.1 记录「类型」列取值（表 / 视图 / 物化视图）；次要信息弱色与列宽 120 未记录。）；macOS none
- 实现备注：文案来自 TableEntry::kind_label，只有「表」「视图」「物化视图」三种；dim-label 次要信息色弱一档，过长省略号。「物化视图」四个汉字要放得下 —— 这也是该列固定宽度的来由（120，未进 metrics.toml）。

### `objects_page.status` 状态行

kind `control` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.content`；尺寸 `font_status`、`gap_sm`
- 无障碍：可访问名「对象列表状态」
- 同步：Linux done（§3 读取中 / 读取成功 / 空结果 / 读取失败 / 没有浏览会话五条）；Windows partial（§7.1 记录四种文案，但措辞不同：有结果写「已加载 N 个对象（还有更多）」（Linux「共 N 个对象（还有更多）」）；「这条连接没有浏览会话…」未记录。）；macOS none
- 实现备注：单行 Label，可换行（set_wrap(true)），dim-label + caption，左对齐；v0.2.4 起在「加载更多…」下方。「共 N 个对象」的 N 是**已加载行数**，不是服务端 total_size —— 协议只给「还有更多」，页面不编造总数。五种状态互斥且都有文案：读取中 / 有结果 / 空结果 / 读取失败 / 没有浏览会话；状态行与空态页也不共存（空态时状态行清空）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 作用域已变、请求在飞 | 写「正在读取…」（首次首页与「加载更多…」续页都写这一句） | `src/tabs.rs:1065、src/tabs.rs:1078` |
| load | 首页或续页返回、有行 | 写「共 N 个对象」；还有下一页时追加「（还有更多）」 | `src/tabs.rs:1117` |
| load | 请求成功但 0 条（本页与累计都是空） | 写「（这个库里没有这类对象）」 | `src/tabs.rs:1121、src/tabs.rs:1122` |
| load | 请求失败 | 写「读取失败：<headline>（<code>）」；同时隐藏「加载更多…」，已取到的行保留在原处 | `src/tabs.rs:1126、src/tabs.rs:1127` |
| load | 该连接没有浏览会话（展开连接前 / 已断开） | 写「这条连接没有浏览会话：展开连接后重试」，不发请求 | `src/tabs.rs:1060、src/tabs.rs:1062` |

### `objects_page.more` 「加载更多…」

kind `control` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.content`；尺寸 `min_target`、`font_ui`、`gap_sm`
- 无障碍：可访问名「加载更多」
- 同步：Linux done（§3「加载更多…」；connect_clicked 与 has_more 驱动 set_visible）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:673；frontend/windows/Sqarvo.Windows/Views/ShellView.Metadata.cs:882）；macOS none
- 实现备注：扁平按钮、左对齐、无图标；默认隐藏，只有服务端给了下一页 token 才出现。分页是「接着取」而不是翻页：新行接在现有行后面，不跳页、不清已加载行、不改变滚动位置以外的状态。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 按钮可见（还有下一页） | 按上次作用域 + 分页 token 继续拉下一页，新行追加在后面；请求期间状态行写「正在读取…」 | `src/tabs.rs:905、src/tabs.rs:1069` |
| load | 首页或续页返回 | 有下一页才显示「加载更多…」；没有下一页、空结果、读取失败、空态（无作用域）一律隐藏 | `src/tabs.rs:1115、src/tabs.rs:1126` |

### `objects_page.empty` 空态页

kind `control` · status `active` · parent `objects_page`

- 样式：槽位 `workspace.content`；尺寸 `font_title`、`font_panel`
- 无障碍：可访问名「对象列表空态」
- 同步：Linux done（§3 空态文案（连接态 / 启动态）+ Functions / Queries 未实现说明）；Windows none（§7.1 未记录（Objects 页的空态页文案与 Functions / Queries 未实现说明））；macOS none
- 实现备注：adw::StatusPage，与「列表 + 状态行」在同一个 GtkStack 里互斥：要么列表，要么空态页，不存在半截。启动态与连接态是同一份实现，只换文案（set_empty_text 改的是默认文案）；未实现说明是临时文案，回到空态时恢复默认值，不残留。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 连接态、树里没有可用作用域（选中连接 / 占位行） | 标题「没有选中对象」，说明「在左侧对象树里选中一个库（或它下面的 Tables / Views），这里会列出该库的表或视图。」 | `src/tabs.rs:854、src/tabs.rs:1013` |
| load | 启动态（未连接） | 标题「没有选中对象」，说明「从左侧选择一个连接开始：双击一条已保存的连接即可连接。」 | `src/startup.rs:180、src/startup.rs:182` |
| load | 树里选中 Functions 分组 | 标题「函数列表尚未实现」，说明「驱动没有提供 Functions 列表（契约里没有该种类的拉取能力），所以这里不摆假数据。」；列表与分页 token 一并清空 | `src/tabs.rs:520、src/tabs.rs:984` |
| load | 树里选中 Queries 分组 | 标题「已保存的查询尚未实现」，说明「本版没有「已保存的查询」这一层：查询只存在于打开着的查询标签里。」 | `src/tabs.rs:524、src/tabs.rs:984` |
| load | 从空态 / 未实现说明回到空态 | 恢复默认文案（上一次的临时说明不残留），并清空状态行与「加载更多…」 | `src/tabs.rs:971、src/tabs.rs:1022` |

## 窗口壳与主工具条（`shell`）

窗口本身（最小尺寸、几何恢复、关闭守卫）、标题与副标题、主菜单、主工具条（连接 / 新建查询 / 四个种类入口）、界面密度三档，以及握手期的 Loading、握手失败错误页与后端失效后的重启入口。

| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |
|---|---|---|---|---|---|---|
| `shell` | 窗口外壳 | area | active | done | partial | — |
| `shell.window` | 　主窗口（最小尺寸 · 几何恢复 · 关闭守卫入口） | container | active | done | partial | — |
| `shell.header` | 　标题栏（窗口外壳） | container | active | done | partial | — |
| `shell.title` | 　标题与副标题 | control | active | done | — | — |
| `shell.menu` | 　主菜单 | control | active | done | partial | — |
| `shell.menu.new_connection` | 　　文件 → 新建连接 | item | active | done | partial | — |
| `shell.menu.new_tab` | 　　文件 → 新建查询标签 | item | active | done | — | — |
| `shell.menu.quit` | 　　文件 → 退出 | item | active | done | — | — |
| `shell.menu.export_csv` | 　　表 → 导出 CSV | item | active | done | — | — |
| `shell.menu.about` | 　　帮助 → 关于 Squarvo | item | active | done | — | — |
| `shell.toolbar` | 　主工具条 | container | active | done | partial | — |
| `shell.toolbar.connect` | 　　「连接」 | control | active | done | partial | — |
| `shell.toolbar.new_query` | 　　「新建查询」 | control | active | done | — | — |
| `shell.toolbar.tables` | 　　种类入口「Tables」 | control | active | done | partial | — |
| `shell.toolbar.views` | 　　种类入口「Views」 | control | active | done | partial | — |
| `shell.toolbar.functions` | 　　种类入口「Functions」 | control | active | done | partial | — |
| `shell.toolbar.queries` | 　　种类入口「Queries」 | control | active | done | partial | — |
| `shell.density` | 　界面密度档（紧凑 / 舒适 / 大） | container | active | done | — | — |
| `shell.density.compact` | 　　「紧凑（100%）」单选项 | item | active | done | — | — |
| `shell.density.comfortable` | 　　「舒适（125%）」单选项 | item | active | done | — | — |
| `shell.density.large` | 　　「大（150%）」单选项 | item | active | done | — | — |
| `shell.error` | 　错误页（握手失败 / 无工作区断开） | container | active | done | — | — |
| `shell.error.retry` | 　　「重试」/「重新连接」 | control | active | done | — | — |
| `shell.backend_failure` | 　后端失效恢复（「重启 Sqarvo」对话框与重启流程） | container | active | done | — | — |

### `shell` 窗口外壳

kind `area` · status `active`

- 样式：尺寸 `header_height`、`toolbar_height`、`status_height`、`min_window_width`、`min_window_height`
- 同步：Linux done（§1.1 / §1.2 外壳结构；ui.rs activate 的 header → toolbar → slot 挂载顺序）；Windows partial（六区域外壳：系统 caption 按钮 + 自绘标题行（图标 / 标题 + 副标题 / 汉堡菜单）+ 工具条（Grid + Button，不是 CommandBar）+ TreeView + TabView + 两行状态栏 + SplitView 信息面板；工具条种类入口未接线、无界面密度菜单。）；macOS none
- 实现备注：自外向内 = 标题栏（区域 01）+ 主工具条（区域 02）+ 页面槽位；工具条挂在标题栏之下、不随工作区重建。Linux 没有独立菜单行：菜单收进标题栏右侧的主菜单按钮（workspace-model 附录 A 第 2 条）；Windows 同样没有独立菜单行——菜单收进自绘标题行右侧的汉堡按钮（v0.2.5 起）。页面槽位轮流承载 Loading / 启动态外壳 / 工作区 / 错误页，四者互斥。

### `shell.window` 主窗口（最小尺寸 · 几何恢复 · 关闭守卫入口）

kind `container` · status `active` · parent `shell`

- 样式：尺寸 `min_window_width`、`min_window_height`
- 无障碍：可访问名「Sqarvo 主窗口」
- 同步：Linux done（§1.1 最小尺寸 / 几何恢复 / 关闭窗口；window_size.rs relayout + apply，ui.rs connect_close_request）；Windows partial（几何恢复含位置、最大化写还原边界、DPI / 工作区变化单入口（§1.6）；脏行守卫换成「状态栏提示 + 第二次放行」的不弹框口径（§7.1 数据页关窗守卫），与 Linux 的确认框差一条。）；macOS none
- 实现备注：最小尺寸由 window_size::relayout 算出后交给 set_size_request，GTK 转成 toplevel 的最小尺寸（Wayland 走 xdg_toplevel.set_min_size，X11 走 WM normal hints）。位置不恢复是平台事实：GTK4 没有读写 toplevel 位置的 API，window_x / window_y 照常写入与校验但不拿来挪窗口（blocked 的替代口径见 README「同步」一节）。关闭是一次性的：确认过一次（`close_confirmed`）后再关不二次弹窗，已进入关闭流程（`closing == true`）时的重复关闭请求直接放行，不重复跑清理。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 窗口创建 / 每次重排 | 最小窗口按显示器逻辑工作区收敛：min(960·T, 工作区宽-32) × min(640·T, 工作区高-48)（T = 密度档，取值来自 metrics.toml）；`--window-size` 给更小的值时被抬到当次下限并记 `[monitor] 目标窗口 … 低于最小窗口 …` | `src/window_size.rs:205、src/window_size.rs:136、src/ui.rs:171` |
| load | window.json 里有记忆的尺寸 / 最大化 | 尺寸与最大化恢复、**位置不恢复**；记忆矩形不落在任何当前显示器几何内（±32 容差）时丢弃几何、改用分档推荐并记 `[monitor] 几何越界已丢弃：…` | `src/window_size.rs:365、src/window_state.rs:209` |
| load | window.json 记的几何属于旧档（记忆尺寸的档位 ≠ 当前档） | 按当前档重算窗口并记 `[monitor] 档位导致窗口放大：1200x800 → 1888x1000（记忆尺寸属于 compact，当前 large）`；记忆矩形越界的校验规则不变（±32 容差内才恢复，否则丢弃改推荐尺寸） | `src/window_size.rs:365、src/window_size.rs:429` |
| click | 点标题栏的关闭按钮 | 先过脏行守卫：任一个表数据标签有未提交的更改（脏行 / 标记删除 / 未提交的新增行）时**不关窗**，弹「有未提交的更改」确认框（逐表列出；本体不在这里登记），选「取消」什么都不做、「放弃更改并退出」才继续。确认过一次后再关不再二次弹窗；继续后显示「正在停止后端并清理端点文件…」，等 worker 回报 `ShutdownComplete` 才销毁窗口 | `src/ui.rs:414、src/ui.rs:985、src/ui.rs:972` |
| key Ctrl+Shift+W | 窗口存在 | 与标题栏关闭按钮同一条 `window.close()` → 同一个 close_request（脏行守卫照旧） | `src/ui.rs:204、src/ui.rs:414` |

### `shell.header` 标题栏（窗口外壳）

kind `container` · status `active` · parent `shell`

- 样式：槽位 `header`；尺寸 `header_height`、`font_ui`、`gap_md`；颜色 `sqarvo_separator`
- 平台差异：Windows：自绘标题行（v0.2.5）：`ExtendsContentIntoTitleBar = true` + `SetTitleBar(拖拽区)`，窗口按钮仍是系统的；行高取 `header_height`（64 / 80 / 96），右侧内距 = `AppWindow.TitleBar.RightInset / RasterizationScale` 给 caption 按钮让位。整行不进拖拽区的只有左侧图标带（24 + gap_sm）与右侧汉堡按钮。
- 同步：Linux done（§1.1 外壳；ui.rs activate 的 AdwHeaderBar + WindowTitle + 主菜单按钮）；Windows partial（有自绘标题行（图标 / 标题 / 副标题 / 汉堡菜单，v0.2.5 起）；标题固定 `Sqarvo`，副标题取后端状态文案（`未连接` / `已连接 · 驱动 N · 浏览会话 M` / `已断开：原因`），在 `ApplyBackendState` 一处更新，不跟标签走。）；macOS none
- 实现备注：Linux 用 AdwHeaderBar：左侧留给系统窗口控件，中间是标题 + 副标题，右侧只有一个主菜单按钮。没有独立菜单行、没有窗口内自绘控制按钮（三端都用系统控件）。

### `shell.title` 标题与副标题

kind `control` · status `active` · parent `shell`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「窗口标题」
- 同步：Linux done（§1.1 六条标题 / 副标题规则；show_startup_workspace / show_workspace / TabSet::refresh_current）；Windows none（§7.1 未记录窗口标题 / 副标题的更新规则。）；macOS none
- 实现备注：由 AdwWindowTitle 承载：字号随窗口继承 font_ui，字重由 Adwaita 的内部样式决定，`.sqarvo-title` 类只用在页内标题上。标题跟着**当前标签**变，副标题是连接级信息、不随标签改 —— 这是两条不同的刷新路径。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 握手成功且还没有数据库连接 | 标题「Sqarvo」、副标题「未连接」 | `src/ui.rs:1561` |
| load | 连接数据库成功 | 标题「<连接名> - <驱动显示名>」；副标题「已连接 · <显示名>（<driver_id>）· <服务端版本>」，版本缺失时写「未知（后端未返回）」 | `src/ui.rs:1720、src/ui.rs:1767、src/ui.rs:1770` |
| load | 切换标签 | 标题跟着当前标签变：`Objects` → 「<连接名> - <驱动>」；表 / 查询标签 → 「<标签标题> (<连接名>)」；副标题不变（连接级信息） | `src/tabs.rs:2289、src/tabs.rs:2307、src/tabs.rs:135` |
| load | 后端断开 | 标题「Sqarvo」、副标题「已断开」；已建立的工作区保留（已加载的行仍可查看），所有标签标记失连 | `src/ui.rs:587、src/tabs.rs:2629` |
| load | 后端握手失败 | 标题回到「Sqarvo」、副标题「连接失败」 | `src/ui.rs:567、src/ui.rs:3774` |
| load | 用户选择「重启 Sqarvo」 | 副标题「正在重启…」，标题「Sqarvo」 | `src/ui.rs:1300` |

### `shell.menu` 主菜单

kind `control` · status `active` · parent `shell`

- 样式：槽位 `menubar`；尺寸 `min_target`、`icon_box`
- 无障碍：可访问名「主菜单（文件 / 表 / 视图 / 帮助）」；Tab 可聚焦；Enter / Space 展开菜单，Esc 收起
- 图标：`menu.open`
- 原生控件：gtk:MenuButton(GMenu → PopoverMenu) / winui:Button(Flyout: MenuFlyout + MenuFlyoutSubItem) / swift:—
- 同步：Linux done（§1.4 菜单栏；build_main_menu 的四组子菜单与占位项）；Windows partial（Windows 用标题行右侧汉堡按钮的 MenuFlyout 承载同一批条目（v0.2.5 起），不是 GTK 的菜单按钮；§7.1 只逐条记录了文件 → 新建连接…，其余条目的集合与禁用口径未记录。）；macOS none
- 实现备注：图标是 `open-menu-symbolic` 的扁平按钮，tooltip 原文是「主菜单（文件 / 表 / 帮助）」——「视图」只在菜单里，tooltip 没列，是现状。「查询」子菜单整项不挂（不留空菜单）；「关闭标签」与表的事务 / 刷新 / 行列操作只保留 `win.*` 动作与快捷键，菜单里不重复一份。「视图 → 界面密度」的条目是占位项（id=tier-section）：GMenu 没有逐项 tooltip、也不能逐项禁用，弹出时被 shell.density 的自绘小节替换。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 窗口存在 | 展开主菜单：文件（新建连接 / 新建查询标签 / 退出）、表（导出 CSV）、视图（界面密度）、帮助（关于 Squarvo）；弹出时刷新界面密度的勾选与逐项可用性，并把自绘小节挂进占位项 | `src/ui.rs:184、src/ui.rs:4590、src/ui.rs:5216` |

### `shell.menu.new_connection` 文件 → 新建连接

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「新建连接」
- 同步：Linux done（§1.4 文件 → 新建连接；open_connection_dialog_for 的未就绪兜底与「只前置」分支）；Windows partial（菜单项与 Ctrl+N 走同一个处理器（ShowNewConnectionDialogAsync），未就绪时整项禁用；但工具条「连接」是另一条判定与另一个动作（直接连选中的已保存连接，不打开新建连接对话框），也没有逐项 tooltip 原因。）；macOS none
- 实现备注：与工具条「连接」、`Ctrl+N` 同一条判定与同一个动作（`win.new-connection`）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手 | 打开连接对话框；对话框已打开时只把它前置（不叠第二个）。未就绪时该项禁用，处理器里还有一条兜底日志「后端尚未连接，忽略「新建连接」」 | `src/ui.rs:4751、src/ui.rs:1103` |
| key Ctrl+N | 后端已握手 | 与菜单项同一个动作 `win.new-connection` | `src/ui.rs:320` |

### `shell.menu.new_tab` 文件 → 新建查询标签

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「新建查询标签」
- 同步：Linux done（§1.4 文件 → 新建查询标签；set_workspace_ready 与 new-tab 接线）；Windows none（§7.1 只记录 `Ctrl+T` 新建查询页，没有记录「文件 → 新建查询标签」这个菜单项。）；macOS none
- 实现备注：与工具条「新建查询」、标签条 `＋`、`Ctrl+T` 同一条 `TabSet::new_query_tab`。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且工作区存在（`set_workspace_ready` 统一开关） | 新建查询标签并聚焦编辑器；未就绪 / 未连接时禁用（不是点了没反应） | `src/ui.rs:4754、src/ui.rs:4715、src/tabs.rs:1856` |
| key Ctrl+T | 后端已握手且工作区存在 | 与菜单项同一个动作 `win.new-tab` | `src/ui.rs:327` |

### `shell.menu.quit` 文件 → 退出

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「退出」
- 同步：Linux done（§1.1 退出应用；quit_app 的「先关窗」分支与 accel 绑定）；Windows none（§7.1 未记录「退出」菜单项与退出守卫。）；macOS none
- 实现备注：菜单项走 `app.quit`，与 `Ctrl+Q`、D-Bus 同一个入口；有窗口时先关窗，因此脏行守卫照常生效。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 任意 | 有窗口时先 `window.close()`（脏行未提交会先弹确认框），最后一个窗口关完后 GApplication 自然退出；没有窗口时才直接 `app.quit()`。两种情况 `connect_shutdown` 都会停后端并 join 子进程，不留孤儿进程 | `src/ui.rs:4596、src/main.rs:353` |
| key Ctrl+Q | 任意 | `app.quit` 加速度器；与菜单项、D-Bus 走同一条 `quit_app` | `src/main.rs:76` |

### `shell.menu.export_csv` 表 → 导出 CSV

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「导出 CSV」
- 同步：Linux done（§1.4 表 → 导出 CSV；export-csv 动作与 workspace ready 开关）；Windows none（§7.1 明确记录 Windows 数据页未实现导出 CSV。）；macOS none
- 实现备注：与表工具条 `导出 ▾` 菜单的「导出 CSV…」、`Ctrl+Shift+E` 同一条导出入口；固定 CSV。INSERT 语句与转储表只在表工具条 / 结果区 / 树右键菜单里，菜单不重复。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且工作区存在；当前标签有没有可导出内容由动作自己判定 | 导出当前结果页 / 当前页为 CSV；当前标签没有可导出的结果页时只写日志「当前标签没有可导出的结果页」，不弹空对话框 | `src/ui.rs:4601、src/ui.rs:4945` |
| key Ctrl+Shift+E | 同上 | 直接导出 CSV（与菜单项同一个 `win.export-csv`） | `src/ui.rs:356` |

### `shell.menu.about` 帮助 → 关于 Squarvo

kind `item` · status `active` · parent `shell.menu`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「关于 Squarvo」
- 同步：Linux done（§1.4 帮助 → 关于 Squarvo；show_about 的「已打开只前置」与诊断行）；Windows none（§7.1 未记录「帮助 → 关于」菜单项。）；macOS none
- 实现备注：不依赖后端：握手前也能打开（诊断字段会写「未连接」/ 空）。对话框本体（版本、协议、后端诊断字段的排版）归 dialogs 区；这里只登记入口。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 任意 | 打开「关于 Squarvo」对话框：版本、协议、后端诊断字段（不含密码、凭据引用与 token）；已打开时只前置，不叠第二个 | `src/ui.rs:4623、src/ui.rs:5289、src/ui.rs:4957` |

### `shell.toolbar` 主工具条

kind `container` · status `active` · parent `shell`

- 样式：槽位 `toolbar`；尺寸 `toolbar_height`、`toolbar_padding_h`、`gap_sm`、`min_target`、`font_ui`；颜色 `sqarvo_separator`
- 无障碍：可访问名「主工具条」
- 同步：Linux done（§1.2 主工具条结构与「窗口级、不随工作区重建」；Toolbar::new / set_backend_ready / log_availability）；Windows partial（有 CommandBar 与六个入口（§7.1）；种类入口未接线、无高亮联动，工具条高度按 Metrics 取档但对外壳的分配未核对。）；macOS none
- 实现备注：窗口级控件（在标题栏之下创建），不随工作区重建：窗口构建后、拿到 worker 之前先置未就绪；握手完成置就绪；后端失效 / 退出 / 重启回退未就绪。左组 = 连接 / 新建查询；竖分隔线；种类入口组（Tables / Views / Functions / Queries，纯拉丁文字、与「新建查询」同一套 flat 样式）；其余空间弹性留白。六个按钮的可用性只按这一份清单统计，日志 `[toolbar] 后端未握手：0/6 个按钮可用` / `[toolbar] 后端已就绪：6/6 个按钮可用`；启动态「新建查询」禁用时是 5/6。四个种类入口共用一条目标库解析：取树里**当前选中项所在的库**（选中的表 / 视图 / 分组也算），只有什么都没选中时才退回活动连接的默认库；多连接按「会话 ID + 库名」精确匹配，避免同名 `main` 跳到别的连接（`selected_catalog` / `catalog_row` 的 require_session 一遍）。种类入口是纯拉丁文字，与中文混排时字面会偏上，由 `.sqarvo-kind label` 的 margin-top 补回来（按钮盒子与命中区不变）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 后端就绪状态变化 | 六个按钮同时切换可用性与 tooltip，并打一行可用性日志；菜单侧走同一个状态（`WindowActions::set_backend_ready`），不出现「菜单禁用、按钮可点」 | `src/shell.rs:862、src/ui.rs:4666、src/shell.rs:840` |
| load | 当前标签切换 / 每次标签状态刷新 | 高亮跟着当前标签：表数据标签 → 该表的种类；`Objects` 页 → 页面**正在列**的种类（树里换分组导致页换作用域时，高亮在同一次刷新里跟上）；查询标签 → 无高亮。同一时刻最多一个按钮带 `sqarvo-kind-active` | `src/ui.rs:1840、src/shell.rs:958` |

### `shell.toolbar.connect` 「连接」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`
- 无障碍：可访问名「连接」
- 同步：Linux done（§1.2「连接」；CONNECT_TOOLTIP / CONNECT_TOOLTIP_NOT_READY 与 suggested-action 的摘挂）；Windows partial（可用性读 BackendState.IsReady、未就绪禁用并逐条给 tooltip 原因（§7.1）；按钮的强调色与禁用时的强调色摘除未记录。几何 v0.2.5 修正：高取 `min_target`(24)，此前误用 `badge_min_height`(26)，比 Linux 高 2 DIP（`Views/ShellView.xaml:261-262`）。）；macOS none
- 实现备注：主工具条上唯一的强调色按钮（`suggested-action`）；禁用时**摘掉**该类 —— Adwaita 的 disabled suggested 仍是橙底，视觉上像可点。tooltip：就绪时「打开连接对话框（Ctrl+N）」；未就绪时「后端尚未就绪：正在连接本地后端，握手完成后才能新建连接」。连接入口只有这一个（标题栏不放连接按钮）；连接对话框也能从「文件 → 新建连接」/ `Ctrl+N` 打开。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手 | 打开连接对话框；对话框已打开时只把它前置，不叠第二个。禁用时不发动作，只在处理器里留一条日志兜底 | `src/shell.rs:721、src/ui.rs:1096、src/ui.rs:1103` |

### `shell.toolbar.new_query` 「新建查询」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`
- 无障碍：可访问名「新建查询」
- 同步：Linux done（§1.2「新建查询」；apply_new_query 的两条件与三段 tooltip）；Windows none（v0.2.5 起与 Linux 同尺寸：高取 `min_target`(24)（此前 26）。（`Views/ShellView.xaml:294-295`））；macOS none
- 实现备注：两个独立条件同时满足才可点：后端已握手 **且** 已连接数据库（工作区存在）；两者正交，任一不满足都禁用。tooltip 三态：启用「新建查询标签（Ctrl+T）」；未连接「尚未连接数据库：先连接再新建查询标签」；后端未就绪「后端尚未就绪：握手完成且连接数据库后才能新建查询标签」。与标签条 `＋`、`Ctrl+T`、菜单「文件 → 新建查询标签」同一条 `TabSet::new_query_tab`。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且工作区存在 | 新建查询标签并聚焦编辑器；工作区不存在时只写日志「尚未连接数据库：先连接再新建查询标签」 | `src/shell.rs:895、src/ui.rs:1065、src/tabs.rs:1856` |

### `shell.toolbar.tables` 种类入口「Tables」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`、`gap_xs`；颜色 `sqarvo_brand_mid`、`sqarvo_brand_on_mid`；状态 selected=`sqarvo_brand_mid`
- 无障碍：可访问名「Tables」
- 图标：`tree.group.tables`
- 同步：Linux done（§1.3 点 Tables；show_objects_for_kind 的 select_kind → show_objects → 高亮）；Windows partial（入口是 AppBarButton，本片仍未接线：点了不切 Objects 页、不高亮，tooltip 说明去树里点分组（§7.1）。）；macOS none
- 实现备注：纯拉丁文字、无图标；与「新建查询」同一套 flat 按钮（同高同内距）。高亮实现为加 `sqarvo-kind-active` 类（中档实底 + 对应前景），同一时刻最多一个按钮带这个类。tooltip：就绪「在 Objects 页列出当前库的表（等同在对象树里选中 Tables 分组）」；未就绪「后端尚未就绪：握手完成后可用」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且树里能定位到库 | 在树里**一次选中**该库的 `Tables` 分组行（库未展开先展开），切到 `Objects` 页并按 `filter.kinds=[TABLE]` 拉第一页；信息面板随树选中切换；工具条高亮 `Tables` | `src/ui.rs:1078、src/ui.rs:1909、src/shell.rs:958` |
| click | 树里定位不到库（尚未连接 / 树里没有库） | 只写日志「种类入口「Tables」无法定位库：…」，界面不变化（§9 已知偏差）；启动态则把固定 `Objects` 页摆到前面并**保持空态**，日志「尚未连接数据库，Objects 页保持空态」 | `src/ui.rs:1942、src/startup.rs:298` |

### `shell.toolbar.views` 种类入口「Views」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`、`gap_xs`；颜色 `sqarvo_brand_mid`、`sqarvo_brand_on_mid`；状态 selected=`sqarvo_brand_mid`
- 无障碍：可访问名「Views」
- 图标：`tree.group.views`
- 同步：Linux done（§1.3 点 Views；KindGroup::Views 与 kinds 组合）；Windows partial（入口是 AppBarButton，本片仍未接线（§7.1），与 Tables 同一状态。）；macOS none
- 实现备注：与 `Tables` 同一套按钮与同一条联动路径，只有分组与 kinds 不同。tooltip：就绪「在 Objects 页列出当前库的视图（等同在对象树里选中 Views 分组）」；未就绪「后端尚未就绪：握手完成后可用」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且树里能定位到库 | 在树里一次选中该库的 `Views` 分组行，切到 `Objects` 页并按 `filter.kinds=[VIEW, MATERIALIZED_VIEW]` 拉第一页；信息面板随树选中切换；工具条高亮 `Views` | `src/ui.rs:1078、src/ui.rs:1904、src/ui.rs:1920` |
| click | 树里定位不到库（尚未连接 / 树里没有库） | 只写日志，界面不变化（与 `Tables` 同一条分支）；启动态把固定 `Objects` 页摆到前面并保持空态 | `src/ui.rs:1942、src/startup.rs:298` |

### `shell.toolbar.functions` 种类入口「Functions」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`、`gap_xs`；颜色 `sqarvo_brand_mid`、`sqarvo_brand_on_mid`；状态 selected=`sqarvo_brand_mid`
- 无障碍：可访问名「Functions」
- 图标：`tree.group.functions`
- 同步：Linux done（§1.3 点 Functions 与「定位不到库」分支；unavailable_copy 原文）；Windows partial（入口是 AppBarButton，未接线；树里 Functions 分组展开只写「尚未实现（点开看说明，不发请求）」（§7.1）。）；macOS none
- 实现备注：有入口、没有列表能力：点了照样联动（树里选中分组行），页面如实说明原因，不摆假数据。tooltip：就绪「函数列表：驱动尚未提供（点开看说明）」；未就绪「后端尚未就绪：握手完成后可用」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且树里能定位到库 | 树里照样选中 `Functions` 分组行（联动两向都要通）；`Objects` 页写「函数列表尚未实现」+「驱动没有提供 Functions 列表（契约里没有该种类的拉取能力），所以这里不摆假数据。」；**0 次 RPC**；工具条高亮 `Functions` | `src/ui.rs:1931、src/tabs.rs:518、src/ui.rs:1928` |
| click | 树里定位不到库 | 仍然切到 `Objects` 页并显示未实现说明（不静默、不留空白页） | `src/ui.rs:1948` |

### `shell.toolbar.queries` 种类入口「Queries」

kind `control` · status `active` · parent `shell.toolbar`

- 样式：尺寸 `min_target`、`font_ui`、`gap_xs`；颜色 `sqarvo_brand_mid`、`sqarvo_brand_on_mid`；状态 selected=`sqarvo_brand_mid`
- 无障碍：可访问名「Queries」
- 图标：`tree.group.queries`
- 同步：Linux done（§1.3 点 Queries；unavailable_copy 的 Queries 原文）；Windows partial（入口是 AppBarButton，未接线；树里 Queries 分组展开只写「尚未实现（点开看说明，不发请求）」（§7.1）。）；macOS none
- 实现备注：与 `Functions` 同构：有入口、0 次 RPC、页面如实说明。tooltip：就绪「已保存的查询：尚未实现（点开看说明）」；未就绪「后端尚未就绪：握手完成后可用」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端已握手且树里能定位到库 | 树里选中 `Queries` 分组行；`Objects` 页写「已保存的查询尚未实现」+「本版没有「已保存的查询」这一层：查询只存在于打开着的查询标签里。」；**0 次 RPC**；工具条高亮 `Queries` | `src/ui.rs:1931、src/tabs.rs:524、src/ui.rs:1928` |
| click | 树里定位不到库 | 仍然切到 `Objects` 页并显示未实现说明（与 Functions 同一条分支） | `src/ui.rs:1948` |

### `shell.density` 界面密度档（紧凑 / 舒适 / 大）

kind `container` · status `active` · parent `shell`

- 样式：槽位 `overlay`；尺寸 `font_ui`、`font_status`、`gap_xs`
- 无障碍：可访问名「界面密度」；Tab 菜单里三档为一组单选项，Tab 进入、↑ / ↓ 切换、Space 选择
- 原生选型说明：GMenu 没有逐项 tooltip 与逐项禁用，所以菜单里只放一个占位项，弹出时换成 PopoverMenu 里的一组 GtkCheckButton（控件仍是原生，菜单模型自组装）。
- 同步：Linux done（§1.5 全部行为；density::switch 的六步与 ensure_startup_tier 的守卫）；Windows none（§7.1 未记录「视图 → 界面密度」菜单及其勾选 / 逐项禁用 / tooltip 语义（档位本身见 §1.6）。）；macOS none
- 实现备注：「视图 → 界面密度」这一项本身是 GMenu 的占位项：GMenu 没有逐项 tooltip、也不能逐项禁用，弹出时用 `PopoverMenu::add_child` 换成这一节（标题「界面密度」+ 三个 GtkCheckButton），视图子菜单只为它而存在。三档的数字唯一真值是 metrics.toml（每个尺寸 / 字号 / 行高 / 图标盒都直接取当前档的写死整数，不做「基准 × 倍率」）。不做的事：不用 `Ctrl+滚轮`（滚轮在数据网格与 SQL 编辑器里已有语义）；不提供小于 100% 的档位；切档不重建标签（未提交编辑与滚动位置保留）；系统 DPI 与系统字号不乘进档位。切档后「表格区优先保留 600」是代码常量，未进 metrics。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key Ctrl+Plus / Ctrl+KP_Add / Ctrl+= | 窗口存在 | 升一档（`win.tier-up`）：目标档可用就切；已经在「大（150%）」时什么都不做并记「[tier] 已经是最高档（large），Ctrl+Plus 忽略」；目标档在当前显示器放不下时不切换、不写盘并记「[tier] … 不可用（需要 W×H），未切换」 | `src/ui.rs:361、src/ui.rs:4978` |
| key Ctrl+minus / Ctrl+KP_Subtract | 窗口存在 | 降一档（`win.tier-down`）；已在「紧凑（100%）」时记「[tier] 已经是最低档（compact），Ctrl+minus 忽略」 | `src/ui.rs:365、src/ui.rs:4987` |
| key Ctrl+0 | 窗口存在 | 直接切到「紧凑（100%）」（`win.tier-reset`，不是「回退一个可用档」）；已经是紧凑时该项禁用 | `src/ui.rs:366、src/ui.rs:4996、src/ui.rs:5091` |
| load | 启动 | 启动档位优先级：`--tier`（QA，取证用）→ `window.json` 的 `tier` → 紧凑（100%）；日志 `[tier] 启动档位 …（来源=默认/记忆/QA）`。已保存的档位当前放不下时**回退到下一个可用档**并记 `[tier] … 在当前工作区 … 不可用（需要 W×H），回退 …`，不静默钳到紧凑；连紧凑都放不下时保留紧凑、窗口收敛到工作区 | `src/ui.rs:135、src/density.rs:33、src/window_size.rs:237` |
| load | 切档成功 | 同一次动作里：① 四套样式表（主壳 / 数据页 / 对象树 / DDL）用同一个 provider 重载；② 窗口按新档重算并钳到工作区（日志「[monitor] 档位导致窗口放大：…」）；③ 侧栏 / 信息面板按 **compact 归一值 × 新档**换算（往返不漂移），新窗口放不下表格区下限时收到各自下限并记日志；④ 已建列的固定宽度、行高、字号与图标按钮命中区按新档重设；⑤ 档位写入 `window.json` | `src/density.rs:78、src/shell.rs:591、src/theme.rs:241` |

### `shell.density.compact` 「紧凑（100%）」单选项

kind `item` · status `active` · parent `shell.density`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「紧凑（100%）」
- 同步：Linux done（§1.5 菜单入口；build_tier_menu 与 apply_tier_choice）；Windows none（§7.1 未记录本单选项。）；macOS none
- 实现备注：当前档打勾；程序化同步勾选时不触发 action（guard 标志）。是 `Ctrl+0` 的目标档，也是 `Ctrl+minus` 的下界。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该档在当前显示器可用 | 切到紧凑档（走 shell.density 记的同一套切档流程）；已经是当前档时不重复重排并记「[tier] 已经是 compact 档（100%），不重复重排」；tooltip 原文「当前档位：紧凑（100%）」/「切换到 紧凑（100%）；窗口会按档位一起放大 / 收小」 | `src/ui.rs:5167、src/ui.rs:5132、src/density.rs:104` |

### `shell.density.comfortable` 「舒适（125%）」单选项

kind `item` · status `active` · parent `shell.density`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「舒适（125%）」
- 同步：Linux done（§1.5 菜单入口与禁用 tooltip；sync_tier_checks）；Windows none（§7.1 未记录本单选项。）；macOS none
- 实现备注：与其它两档同一组单选；本档放不下时禁用（`sensitive=false`）并给缺多少的 tooltip。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该档在当前显示器可用 | 切到「舒适（125%）」；目标档在当前显示器放不下时不切换、不写盘（该项此时也已禁用），tooltip 原文「当前显示器放不下 125%：需要 <W>×<H> 逻辑工作区（当前 <w>×<h>）」或「切换到 舒适（125%）；窗口会按档位一起放大 / 收小」 | `src/ui.rs:5096、src/ui.rs:5132、src/window_size.rs:230` |

### `shell.density.large` 「大（150%）」单选项

kind `item` · status `active` · parent `shell.density`

- 样式：尺寸 `font_ui`
- 无障碍：可访问名「大（150%）」
- 同步：Linux done（§1.5 升一档上界与菜单禁用；refresh_tier_actions 的 available 判定）；Windows none（§7.1 未记录本单选项。）；macOS none
- 实现备注：本档的最小窗口最大，因此最容易被禁用：这是「不静默钳到 100%」的直接表现。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 该档在当前显示器可用 | 切到「大（150%）」；放不下时禁用并给「当前显示器放不下 150%：需要 <W>×<H> 逻辑工作区（当前 <w>×<h>）」；已是本档时 tooltip 写「当前档位：大（150%）」 | `src/ui.rs:5096、src/ui.rs:5132` |

### `shell.error` 错误页（握手失败 / 无工作区断开）

kind `container` · status `active` · parent `shell`

- 样式：槽位 `workspace.content`；尺寸 `font_title`、`font_status`、`gap_page`
- 无障碍：可访问名「无法连接后端」
- 同步：Linux done（§1.1 后端握手失败 / 后端断开；show_error 与 show_disconnected）；Windows none（§7.1 只记录「断开即回未连接、原因与错误码写状态栏」，没有核对错误页表面。）；macOS none
- 实现备注：同一张 StatusPage 承载两种状态：握手失败「无法连接后端」与无工作区时的「已断开连接」；有工作区时断开**不**换页（保留已加载的行并标失连）。工具条留在错误页上方并保持全部禁用，日志「[toolbar] 后端未握手：0/6 个按钮可用」—— 错误页不是「退化成没有工具栏」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 后端握手失败 | 主区域换成错误页：标题「无法连接后端」、副标题「连接失败」、描述为错误原文 + 「重试」按钮；标题栏回到「Sqarvo」；工具条保留在上方并保持全部禁用 | `src/ui.rs:3774、src/ui.rs:567` |
| load | 后端断开且没有工作区 | 同一张 StatusPage 换成「已断开连接」+ 断开原因 + 「重新连接」；标题「Sqarvo」、副标题「已断开」 | `src/ui.rs:3792、src/ui.rs:602` |

### `shell.error.retry` 「重试」/「重新连接」

kind `control` · status `active` · parent `shell.error`

- 样式：尺寸 `min_target`、`font_ui`
- 无障碍：可访问名「重试」
- 同步：Linux done（§1.1 错误页「重试」；retry 闭包的未就绪回退与 worker.connect）；Windows none（§7.1 未记录重试 / 重新连接入口。）；macOS none
- 实现备注：同一枚按钮：握手失败页写「重试」，断开页写「重新连接」，走同一个回调。强调色 + pill 外形；点击后回到 Loading，而不是停在原地重试。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 错误页 / 断开页可见 | 回到 Loading「准备连接本地后端…」；若之前是就绪状态，工具条与菜单先回未就绪（并打一行可用性日志），然后重新握手 | `src/ui.rs:369、src/ui.rs:3786` |

### `shell.backend_failure` 后端失效恢复（「重启 Sqarvo」对话框与重启流程）

kind `container` · status `active` · parent `shell`

- 样式：槽位 `overlay`；尺寸 `min_target`、`font_ui`
- 无障碍：可访问名「Sqarvo 需要重启」
- 同步：Linux done（§1.1 后端失效 / 重启 Sqarvo；show_backend_failure_dialog 与 restart_software）；Windows none（§7.1：断开后回「未连接」并在状态栏给原因与错误码、不自动重启后端；没有「重启 Sqarvo」横幅 / 对话框的记录。）；macOS none
- 实现备注：横幅控件本身画在标签页（SQL 编辑器条）与数据页里，按钮文案「重启 Sqarvo」；本组件只登记窗口级的对话框与重启流程，横幅的位置归各自区的组件。对话框文案不写「后端进程已退出」这类内部细节，只说「本地服务已失效」—— 进程死活不是用户能判断的事。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 点任一标签的后端失效横幅「重启 Sqarvo」，或数据页错误横幅 | 弹 AdwAlertDialog「Sqarvo 需要重启」+「本地服务已失效，Sqarvo 无法继续读写数据库。重启会关闭所有标签页，未提交的更改会丢失。」；按钮：取消 / 查看日志 / 重启 Sqarvo（破坏性，默认取消） | `src/ui.rs:1274、src/sql_page.rs:128` |
| click | 对话框里选「重启 Sqarvo」 | 不新开进程：释放工作区与全部标签会话 → 清后端会话登记 → 工具条 / 菜单回未就绪 → 标题「Sqarvo」、副标题「正在重启…」→ 回到 Loading「正在重启 Sqarvo…」→ 重新握手 | `src/ui.rs:1300` |
| click | 对话框里选「查看日志」 | 打开日志窗口；不重启、不改会话状态 | `src/ui.rs:1282` |

## 启动态外壳（`startup`）

握手期间的 Loading 页，以及握手成功后、还没有连接数据库时的六区域外壳：侧栏是与连接态同一个对象树（全部「未连接」）、主区域是固定 Objects 页的空态、信息面板常驻留白、状态栏写「未连接」。这里登记的是「启动态」这一档表面，不重复登记表结构。

| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |
|---|---|---|---|---|---|---|
| `startup` | 启动态外壳 | area | active | done | — | — |
| `startup.loading` | 　Loading 页（标题 + 握手阶段文案 + 居中 spinner） | container | active | done | — | — |
| `startup.loading.spinner` | 　　加载 spinner | control | active | done | — | — |
| `startup.loading.stage` | 　　握手阶段文案 | control | active | done | — | — |
| `startup.workspace` | 　启动态工作区外壳（侧栏 + Objects + 信息面板 + 状态栏） | container | active | done | — | — |

### `startup` 启动态外壳

kind `area` · status `active`

- 样式：槽位 `startup`；尺寸 `sidebar_width`、`inspector_width`、`status_height`
- 同步：Linux done（§1.1 应用启动；startup.rs 全模块为启动态唯一实现）；Windows none（§7.1 未记录启动态（握手成功、未连接数据库）外壳；只记录六区域外壳存在。）；macOS none
- 实现备注：启动态不是独立一套界面：握手成功后 ui.rs 用 StartupWorkspace 组装六个区域，侧栏 / 标签条 / 信息面板与连接态是同一套实现，连上数据库后由 Workspace 整体替换。后端诊断信息（版本 / 协议 / PID / 端点文件）不在启动态出现，在「帮助 → 关于 Squarvo」里。启动顺序（窗口级，全在 ui.rs::activate）：建窗口并把 headerbar（ui.rs:177-199）、主工具条与页面槽位（ui.rs:270-271）挂进根容器 → toolbar.set_backend_ready(false)（ui.rs:274，随即打一行 0/6）→ show_loading「准备连接本地后端…」（ui.rs:275）→ WorkerHandle::spawn（ui.rs:277）→ 每个 Event::Stage 改写说明行（ui.rs:504-506）→ Event::Connected 置就绪、标题回 Sqarvo /「未连接」、show_startup_workspace（ui.rs:538-548）。页面槽位里 Loading 与启动态外壳互斥，任一时刻只有一个。

### `startup.loading` Loading 页（标题 + 握手阶段文案 + 居中 spinner）

kind `container` · status `active` · parent `startup`

- 样式：槽位 `startup`；尺寸 `spinner_px`、`gap_xl`
- 同步：Linux done（§1.1 应用启动（先 Loading）；show_loading 组装 StatusPage + spinner）；Windows none（§7.1 未记录）；macOS none
- 实现备注：AdwStatusPage：标题「正在连接后端」，说明行 = 当前握手阶段文案，子控件是 spinner（居中、上方留 gap_xl）。页面只占窗口的页面槽位：Loading 期间侧栏、标签条、信息面板、状态栏都还不存在（窗口根是 headerbar + 主工具条 + 页面槽位）。窗口标题（副标题改「正在连接…」）归 shell 的标题组件，本区不登记；后端握手失败后的错误页同样归 shell。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 应用启动 / 重新握手（worker 尚未回报 Connected） | 页面槽位换成居中 StatusPage：标题「正在连接后端」、说明行写当前阶段文案、spinner 同时开始旋转；主工具条在此之前已存在但六个按钮全禁用 | `src/ui.rs:1543、src/ui.rs:275、src/ui.rs:274` |

### `startup.loading.spinner` 加载 spinner

kind `control` · status `active` · parent `startup.loading`

- 样式：尺寸 `spinner_px`
- 无障碍：可访问名「正在连接后端」
- 同步：Linux done（show_loading 的 GtkSpinner + set_size_request(spinner_px) + start()）；Windows none（§7.1 未记录）；macOS none
- 实现备注：GtkSpinner：边长取 spinner_px（请求值，由 GTK 决定实际绘制），构造后立即 start()。阶段推进走同一个 show_loading：set_page 先清空页面槽位再挂新的 StatusPage 与 spinner（ui.rs:1532-1554），上一帧的动画状态不保留；两帧之间只差一行说明，视觉上仍是同一处居中 spinner、没有静止帧。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | Loading 页出现 | 立即开始旋转（无静止帧）；阶段推进只换说明行，spinner 一直在转（整页替换时不出现静止帧、不闪） | `src/ui.rs:1549、src/ui.rs:1552` |

### `startup.loading.stage` 握手阶段文案

kind `control` · status `active` · parent `startup.loading`

- 无障碍：可访问名「握手阶段」
- 同步：Linux done（supervisor 六阶段文案 + Event::Stage 逐条改写说明行）；Windows none（§7.1 未记录）；macOS none
- 实现备注：阶段文案是六条固定原文，由后端 supervisor 的握手阶段枚举给（worker 以 Event::Stage 逐条推给界面），界面不拼接、不翻译。六条原文：「读取端点描述文件…」「校验协议版本与后端进程…」「启动后端进程 sqarvo-backend…」「等待后端就绪（最多 10 秒）…」「建立 gRPC 通道并附带本地会话 token…」「调用 SystemService.GetVersion 核对版本…」。说明行不总是阶段原文：show_loading 的说明是调用方给的字符串——首帧 / 重新握手「准备连接本地后端…」（ui.rs:275、ui.rs:380）、重启「正在重启 Sqarvo…」（ui.rs:1318）、关窗前「正在停止后端并清理端点文件…」（ui.rs:407）；标题恒为「正在连接后端」，界面只换说明行。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 收到 Event::Stage（握手推进） | 说明行改写为阶段原文（六条之一）；只有文本变化，不出现进度百分比、不加按钮、不改标题 | `src/backend/supervisor.rs:56、src/ui.rs:504、src/ui.rs:1547` |

### `startup.workspace` 启动态工作区外壳（侧栏 + Objects + 信息面板 + 状态栏）

kind `container` · status `active` · parent `startup`

- 样式：槽位 `startup`；尺寸 `sidebar_width`、`inspector_width`、`status_height`、`tab_strip_height`
- 同步：Linux done（§1.1 应用启动（落地工作区）+ §7 状态栏行 2；show_startup_workspace 与 StartupWorkspace::new）；Windows none（§7.1 未记录启动态外壳；只记录六区域外壳与固定 Objects 页存在。）；macOS none
- 实现备注：握手成功即整体替换 Loading，不再停在旧版诊断页；窗口标题回 `Sqarvo` /「未连接」（标题归 shell 区）。侧栏是与连接态同一个对象树（同一份连接注册表）：启动态所有连接都处于「未连接」，点一下即按保存的凭据连接（行级语义见 object_tree 区）。面板宽度读窗口级记忆（与连接态共用同一个值），启动态拖一次会被带进连上后的工作区。标签条右侧的 ＋ 保留在启动态但置为不可用（tooltip「先连接数据库再新建查询标签」）；该按钮的登记权归标签条所属区，本区只在结果里提到它。主区域落地就是固定 Objects 页的空态：标题「没有选中对象」，说明「从左侧选择一个连接开始：双击一条已保存的连接即可连接。」。页面本体（页签、三列表格、状态行）与这一档文案已由 objects_page 区登记（objects_page.tab / objects_page.empty），本区只保证启动态停在这一页、不换内容。这一页是 startup.rs 自己建的（不走 tabs::add_objects_page）：页签标题 / 图标 / pinned 与连接态同语义（startup.rs:188-192），关闭请求有兜底日志「[startup] Objects 页固定存在，不可关闭」（startup.rs:195-198），页内 open 回调换成日志「[startup] 尚未连接数据库，忽略打开表」（startup.rs:184-186，启动态不会有可打开的行）。页签结构仍归 objects_page 区，本区只记这一份构造。信息面板是同一套外壳：未选中对象时内容区留白；选中一条已保存的连接时按 inspector 区显示连接卡片，只是没有服务端版本 / 延迟两行（未连接时那两行不出现，不编造，startup.rs:110-135）。启动态摘要不属于本区：状态栏把连接名「未连接」与「驱动 N · 浏览会话 M」拼成一行「未连接 · 驱动 0 · 浏览会话 0」，登记在 status_bar 区（status_bar.row2.session）；本区只把 N / M 的真实取值（驱动数照抄握手快照、浏览会话数为 0）交给它。启动态没有表标签：状态栏行 1 的行操作全部禁用并给原因「未连接：从左侧选择一个连接开始」，SQL 摘要写「尚未连接」——行 1 的结构与原因文案归 status_bar 区。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 握手成功（Event::Connected） | 外壳落地：侧栏换成对象树（已保存连接全部「未连接」）、主区域放固定 Objects 页、信息面板常驻且内容区留白、状态栏写「未连接」；连上数据库前不存在表标签 | `src/ui.rs:1561、src/ui.rs:1599、src/startup.rs:99` |
| load | 主工具条是窗口级控件（不随外壳重建） | 握手完成前六个按钮全部禁用（日志「[toolbar] 后端未握手：0/6 个按钮可用」）；外壳建立时「新建查询」仍禁用、其余五个可用（5/6，tooltip「尚未连接数据库：先连接再新建查询标签」） | `src/startup.rs:237、src/shell.rs:862、src/shell.rs:912` |
| load | 连接注册表为空（没有已保存的连接） | 侧栏不列行，显示状态页：标题「还没有保存的连接」，说明「用工具条的「连接」保存一条连接；它会作为 My Connections 下的节点出现。」；树状态行写「连接注册表为空」。树本体与行模板不在本区重复登记，本条目只记启动态会看到什么 | `src/tree.rs:1520、src/tree.rs:3179` |
| click | 点主工具条任一 种类入口（Tables / Views / Functions / Queries）；后端已握手、未连接 | 把固定 Objects 页摆到最前并**保持空态**：说明行仍是「从左侧选择一个连接开始：双击一条已保存的连接即可连接。」，不列库 / 表、不换成「未实现」说明（不编造内容）；日志「[startup] 种类入口「<名称>」：尚未连接数据库，Objects 页保持空态」 | `src/startup.rs:298、src/ui.rs:1086` |

## 状态栏（`status_bar`）

窗口底部两行：左边沟（对象树搜索区）、行 1 的行操作与分页器、行 2 的 SQL 摘要与 Last Refresh。它是同一个动作的第二个入口——可用性与读数全部来自当前标签，不在这里另存一份状态。

| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |
|---|---|---|---|---|---|---|
| `status_bar` | 状态栏 | area | active | done | partial | — |
| `status_bar.gutter` | 　左边沟（分栏线左侧整块） | container | active | done | partial | — |
| `status_bar.row1` | 　行 1（行操作 · 选中数 · 展示形式 · 分页器） | container | active | done | partial | — |
| `status_bar.row1.ops` | 　　行操作按钮组（新增 / 删除 / 应用 / 撤销 / 刷新 / 取消） | container | active | done | partial | — |
| `status_bar.row1.ops.add` | 　　　「新增」图标按钮 | control | active | done | partial | — |
| `status_bar.row1.ops.delete` | 　　　「删除」图标按钮 | control | active | done | partial | — |
| `status_bar.row1.ops.apply` | 　　　「应用」图标按钮 | control | active | done | partial | — |
| `status_bar.row1.ops.discard` | 　　　「撤销」图标按钮 | control | active | done | partial | — |
| `status_bar.row1.ops.refresh` | 　　　「刷新」图标按钮 | control | active | done | partial | — |
| `status_bar.row1.ops.cancel` | 　　　「取消」图标按钮 | control | active | done | partial | — |
| `status_bar.row1.selected` | 　　选中行数（N selected） | control | active | done | — | — |
| `status_bar.row1.detail_toggle` | 　　展示形式切换（按行浏览 / 字段 · 值） | control | active | done | — | — |
| `status_bar.row1.pager` | 　　分页器（首页 / 上页 / 下页 / 末页 + 只读页码摘要） | container | active | done | partial | — |
| `status_bar.row1.pager.first` | 　　　「首页」按钮 | control | active | done | — | — |
| `status_bar.row1.pager.previous` | 　　　「上一页」按钮 | control | active | done | — | — |
| `status_bar.row1.pager.next` | 　　　「下一页」按钮 | control | active | done | — | — |
| `status_bar.row1.pager.last` | 　　　「末页」按钮 | control | active | done | — | — |
| `status_bar.row1.pager.summary` | 　　　只读页码摘要（第 N 页 / 共 M 页 · 总数） | control | active | done | partial | — |
| `status_bar.row2` | 　行 2（SQL 摘要 · Last Refresh / 空态摘要） | container | active | done | partial | — |
| `status_bar.row2.sql` | 　　SQL 摘要（左段） | control | active | done | partial | — |
| `status_bar.row2.last_refresh` | 　　Last Refresh（右段） | control | active | done | — | — |
| `status_bar.row2.session` | 　　空态摘要（连接名 / 未连接 · 驱动 N · 浏览会话 M） | control | active | done | partial | — |

### `status_bar` 状态栏

kind `area` · status `active`

- 样式：尺寸 `status_height`、`row_height_tree`；颜色 `sqarvo_separator`、`sqarvo_status_fg`、`window_fg_color`
- 平台差异：Windows：同一副骨架（`ShellView.xaml` 的 `StatusBand`）：贴底、总高 `StatusHeight`、两行各一档行高的网格；列 0 = 左边沟（宽 = 侧栏 + `GridPaddingEnd`）、列 1 = 一条 `Stretch` 到底的竖分割线、列 2 = 两行网格。行 1 左格是后端状态线、右格是六个行操作按钮 + 搜索 + 结果行数 + 只读页码；行 2 左格是 SQL 摘要或一次性提示、右格是事务状态 · 会话名 · `Last Refresh`。尺寸只走 `Metrics`、颜色只走 `ThemeResource`（底色 `SqarvoBandBrush`、竖线 `SqarvoDividerBrush`）。
- 同步：Linux done（v0.2.5 Linux 客户端；§7 两行结构逐条对照 shell.rs（StatusBar::new / apply_snapshot））；Windows partial（§7.1 只记了「两行状态栏外壳」这一句；行 1 / 行 2 的真实分格与文案来源见 `platform_notes.windows`（`已连接 · 驱动 N · 浏览会话 M` 在行 1 左格，不在 §7.1 说的行 2 右格）。#170 的视觉重做已整体回退，现状是 #169 稳定版外壳，#172 / #173 对状态栏只换文案。）；macOS none
- 实现备注：两行各 24（row_height_tree），总高 status_height = 48（v0.2.4 从 56 收到 48）；两行的高度不单独进 metrics，48 = 2 × 24 由这两个 key 共同表达。整条贴窗口底部（valign = End），上边界一条 1px 分隔线（@sqarvo_separator）：1px 描边不进 metrics。只有两种前景色：可用按钮与输入值一个色（@window_fg_color），说明文字与禁用按钮同一个灰（@sqarvo_status_fg）；状态栏里不允许出现第三档灰。行 1 与行 2 之外没有第三行：记录位置、连接名、库名、事务状态、耗时都不在状态栏（v0.2.3.3 删除），右下角也没有第二个刷新按钮。左边沟在构造时按侧栏宽度给初值，之后跟着分栏线走；搜索框垂直居中、不撑高状态栏。

### `status_bar.gutter` 左边沟（分栏线左侧整块）

kind `container` · status `active` · parent `status_bar`

- 样式：槽位 `status_bar.left_gutter`；尺寸 `sidebar_width`、`status_height`；颜色 `sqarvo_separator`
- 平台差异：Windows：同一个沟：`StatusGutterColumn` 宽 = 侧栏 + `GridPaddingEnd`（拖分栏线同步），里面一个 `TreeSearchBox`（宽 = 侧栏、垂直居中、**禁用**：搜索尚未接线，tooltip 给「对象树搜索（当前不可用：搜索尚未接线，服务端 filter.name.contains 已就绪）」）；搜索框下方没有搜索状态行。
- 同步：Linux done（§7 左边沟（宽度跟分栏、竖线到底）；set_gutter_width 与 mount_search）；Windows partial（沟与宽度联动已按 §7 做（`UpdateStatusGutter`：沟宽 = 侧栏 + `GridPaddingEnd`、竖线 `Stretch` 到底）；搜索框在场但恒禁用（未接线），「下方一行搜索状态」没有。§7.1 未记录这个沟。）；macOS none
- 实现备注：宽度 = 侧栏当前宽度（set_gutter_width），右侧一条竖分割线一直延伸到底，与上面的分栏线同一根轴。沟里只放对象树搜索框与它下方一行搜索状态（归属对象树功能区，见 object_tree.search / .box / .status）；搜索框本体不在这里重复登记。沟本身不加内边距：搜索框的左右 8（gap_md）记在搜索框自己的 margin 上，改宽度不会把它撑开。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| drag | 拖动侧栏与工作区之间的分栏线 | 左边沟宽度跟随分栏位置同步，竖分割线与分栏线始终同一根轴；搜索框不把沟撑宽，也不撑高状态栏 | `src/shell.rs:1301、src/shell.rs:1250` |

### `status_bar.row1` 行 1（行操作 · 选中数 · 展示形式 · 分页器）

kind `container` · status `active` · parent `status_bar`

- 样式：槽位 `status_bar.row1`；尺寸 `row_height_tree`、`gap_md`
- 平台差异：Windows：行 1 实为两格：左格 `StatusLineText` = 后端状态线（`已连接 · 驱动 N · 浏览会话 M` / `连接中 · 驱动 0 · 浏览会话 0` / `未连接 · 驱动 0 · 浏览会话 0`，tooltip 带 `[<错误码>]`）；右格依次是六个行操作按钮、搜索按钮（恒禁用）、结果行数（查询页写 `N 行 / 已取 M 行`）、只读页码 `第 N 页 / 共 ? 页`。选中数、展示形式切换、四个翻页按钮都不在行 1。
- 同步：Linux done（§7 行 1；shell.rs 行 1 三组的组装与 audit_groups 自检）；Windows partial（行 1 存在：左格后端状态线、右格六个行操作按钮 + 搜索（禁用）+ 结果行数 + 只读页码；选中数与展示形式切换没有，翻页动作在数据页动作行。§7.1 只记了「应用 / 撤销 / 取消」三枚按钮，与 XAML 不符，以 `platform_notes.windows` 为准。）；macOS none
- 实现备注：左起就是行操作：左边沟已经整块留给对象树，行 1 里没有「连接过滤」也没有第二个刷新按钮。中间一个可扩展空白把右组（选中数 · 展示形式 · 分页器）推到窗口右端；间距 8（gap_md）。行 1 自己没有交互与读数：可用性来自 status_bar.row1.ops 的六个按钮、分页器来自 status_bar.row1.pager，本组件只记布局。

### `status_bar.row1.ops` 行操作按钮组（新增 / 删除 / 应用 / 撤销 / 刷新 / 取消）

kind `container` · status `active` · parent `status_bar.row1`

- 样式：尺寸 `icon_button_width`、`icon_button_height`；状态 disabled=`sqarvo_status_fg`
- 平台差异：Windows：六枚按钮都在（`ButtonRowAdd` / `Delete` / `Apply` / `Revert` / `Refresh` / `Cancel`）；可用性由 `ApplyDataStatus` / `ApplyQueryStatus` 逐枚设 `IsEnabled` 并换 tooltip。新增 / 删除本片恒禁用，理由是「新增行 / 删除行不在本片（#173 只做单元格编辑与提交）：请用 SQL」；其余四枚的禁用原因是 per-button 短句（如「当前没有待提交的更改」），不拼 Linux 的「（当前不可用：<原因>）」。
- 同步：Linux done（§7 行 1 行操作与可用性来源；apply_row_ops / show_no_table / show_query_ops）；Windows partial（§7.1 只记了「应用 / 撤销 / 取消」三枚；XAML 有六枚（新增 / 删除恒禁用，见 `platform_notes.windows`），禁用态 tooltip 是 per-button 短句，不是 Linux 的「（当前不可用：<原因>）」。）；macOS none
- 实现备注：六个图标按钮连成一组（linked，组内间距 0），图标盒与命中区统一走 icon_button_width / icon_button_height，不按单个字形调大小。禁用态：前景换 @sqarvo_status_fg 并叠 opacity 0.45（v0.2.5 加，为「激活 / 非激活一眼可辨」）；同时用 opacity: 1 / filter: none 压掉 Adwaita `.flat:disabled` 与 `dim-label` 自带的透明度，颜色只由这两条规则决定。组尾一条竖分隔线（1px、按行高居中）把行操作与右侧读数隔开；分隔线没有交互，不单独登记。按钮只显示图标，tooltip 同时登记为可访问名；tooltip 文案是动作本身（不带「新增行：」这类前缀），可访问名仍是构建时那句带前缀的话。这里是同一个动作的第二个入口：点击直接走数据页 / 查询页的同一套方法，状态栏不复制任何业务逻辑。§7 把「查询标签没有可编辑表格」概括为一句话；实现按结果可编辑性给更具体的原因（只读结果、新行还空着、没有选中行各一句），见 sql_page 的行操作可用性。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 当前标签是表标签 | 六个按钮的可用性与页内按钮同一条判定（直接读页内按钮的 sensitive，或用数据页的显式谓词，不重复判定）；「刷新」单独判定（已打开表、无 RPC 在途、无脏行、无未提交新行）；「取消」只在提交批次在途时可点；全部禁用时每个 tooltip 追加「（当前不可用：<原因>）」，原因取数据页当前状态（如「正在读取数据」「连接已断开」「没有可提交的更改」） | `src/shell.rs:1468、src/shell.rs:1478、src/data_page.rs:4936、src/data_page.rs:4969、src/data_page.rs:5027` |
| load | 当前标签不是表标签（Objects 页 / 查询标签 / 启动态未连接） | 六个按钮全部禁用并给原因：Objects 页「Objects 页：在对象树里打开表或新建查询标签」；未连接「未连接：从左侧选择一个连接开始」；查询标签按结果可编辑性给原因（例如「结果只读：执行单表查询后才能编辑」）；同时分页摘要、选中数、Last Refresh 整格隐藏 | `src/shell.rs:1511、src/ui.rs:1878、src/startup.rs:251、src/sql_page.rs:912` |

### `status_bar.row1.ops.add` 「新增」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「新增行：在当前表末尾新增一行」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.add`
- 平台差异：Windows：按钮在（`ButtonRowAdd`），但本片恒禁用、无点击处理：tooltip「新增行不在本片（#173 只做单元格编辑与提交）：请用 SQL」。§7.1 的新增入口在数据页动作行与双击表。
- 同步：Linux done（§7 行 1 六个按钮；RowOp::Add → begin_insert）；Windows partial（按钮画出但点击无效（能力性未实现）；§7.1 未记录状态栏新增。契约 §9.1 已裁决：能力性不可用**不渲染**，Windows 端删掉按钮后本项改 `na`。）；macOS none
- 实现备注：工具条上已没有这个按钮（v0.2.4 删除）：它是状态栏 `+` 与 `win.add-row` 的唯一入口，可用性由数据页显式谓词给出。禁用时 tooltip 变成「在当前表末尾新增一行（当前不可用：<原因>）」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 可编辑的表标签，且没有在途请求、没有未提交的新行草稿 | 在当前表末尾插入一行草稿（与页内新增同一条路径，不落库）：新行可逐格填值，填完要按「应用」才提交 | `src/shell.rs:1066、src/data_page.rs:5049` |

### `status_bar.row1.ops.delete` 「删除」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「删除选中行：标记为待删除（提交前可撤销）」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.delete`
- 平台差异：Windows：按钮在（`ButtonRowDelete`），本片恒禁用、无点击处理：tooltip「删除行不在本片（#173 只做单元格编辑与提交）：请用 SQL」。标记删除只在数据页动作行与数据行右键菜单。
- 同步：Linux done（§7 行 1 六个按钮；RowOp::Delete → delete_selected_row）；Windows partial（按钮画出但点击无效（能力性未实现）；§7.1 未记录状态栏删除。契约 §9.1 已裁决：能力性不可用**不渲染**，Windows 端删掉按钮后本项改 `na`。）；macOS none
- 实现备注：v0.2.5 起工具条上的「删除」已删，入口在这里与数据行右键菜单；两处共用同一条可用性谓词。只标记、不发 RPC：多条选中按批量语义一次标记。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 可编辑的表标签且有选中行，无在途请求 | 把选中行标记为待删除（提交前可「撤销」回来），不立即发请求 | `src/shell.rs:1066、src/data_page.rs:5049` |

### `status_bar.row1.ops.apply` 「应用」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「应用：提交所有脏行的变更」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.apply`
- 平台差异：Windows：Windows **有**这枚按钮（`ButtonRowApply`，可访问名「提交变更」），与 `Ctrl+S` 同一条 `SubmitDataAsync`；>100 行时**不弹对话框**，改成「再按一次 Ctrl+S 确认」并带 `acknowledge_risk`（§7.1 与 Linux 的差异）；无脏行时 tooltip 是「当前没有待提交的更改」，不加「（当前不可用：…）」后缀。
- 同步：Linux done（§7 行 1；RowOp::Apply → on_commit_clicked（与页内按钮同一状态））；Windows partial（§7.1 记录状态栏「应用（✓）」与 Ctrl+S 同一路径、>100 行改「再按一次确认」；禁用态 tooltip 句式未记录。）；macOS none
- 实现备注：与数据页的「提交更改」是同一个按钮状态：有脏行才亮，没有脏行时 tooltip 说明「没有可提交的更改」。危险操作判定不在这里另写一套：本地判定（>100 行或含任何删除）与服务端 acknowledge_risk 都走数据页的同一条路径。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有脏行且没有在途提交 | 提交本批变更（全部成功后再 Commit）；变更 >100 行或含任何删除时先弹「确认提交变更？」（正文给更新 / 删除行数与删除警告，破坏性默认取消；取消 →「已取消提交（未发送任何变更）」），服务端要求 acknowledge_risk 时再走同一个对话框 | `src/shell.rs:1066、src/data_page.rs:5049、src/table_data.rs:489` |

### `status_bar.row1.ops.discard` 「撤销」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「撤销：放弃全部未提交的编辑与删除标记」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.discard`
- 平台差异：Windows：`ButtonRowRevert` 与 `Ctrl+Z` 同一 `RevertDataAsync`；无未提交更改时 tooltip「当前没有未提交的更改」；改动全部改回原值时顺手回滚空事务（与 Linux 同一条规则）。
- 同步：Linux done（§7 行 1；RowOp::Discard → discard_changes）；Windows partial（§7.1 记录状态栏「撤销（↶）」与 Ctrl+Z 同一路径；可用性判定与禁用态 tooltip 未记录。）；macOS none
- 实现备注：只丢弃本地未提交的编辑与删除标记，不动已提交的数据；改动全部改回原值时顺手回滚空事务。与「取消」不是一回事：取消是中止在途批次并整批回滚（ADR 0018）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有未提交的编辑、删除标记或新行草稿，无在途请求 | 放弃全部未提交的改动（含新行草稿与删除标记），已打开的空事务一并回滚；数据回到最后一次读取的状态 | `src/shell.rs:1066、src/data_page.rs:5049` |

### `status_bar.row1.ops.refresh` 「刷新」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「刷新：重新读取当前表的第一页」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.refresh`
- 平台差异：Windows：按钮在且可点（`ButtonRowRefresh` → `RefreshDataAsync`）：可用性 = 未加载中、未提交、无未提交更改，与 §7「刷新单独按状态判定」一致；被拦时 tooltip「刷新：有未提交的更改或请求进行中」，不是「（当前不可用：…）」。§7.1 只记了页内刷新。
- 同步：Linux done（§7 行 1「刷新单独按状态判定」；refresh_available + RowOp::Refresh）；Windows partial（可用性口径与 Linux 一致；文案是 per-button 短句，没有「（当前不可用：<原因>）」句式。）；macOS none
- 实现备注：可用性单独判定，不读页内按钮（页内没有对应的刷新按钮）：已打开表、没有 RPC 在途、没有未提交的更改与未提交的新行。有未提交更改时不直接刷：走与翻页 / 排序 / 筛选同一条脏行守卫，先提示提交或放弃。F5 在表标签上走的是同一件事；状态栏不提供第二个刷新入口。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 已打开表、无 RPC 在途、无脏行与未提交新行 | 重新读取当前表的第一页（保持当前排序与筛选，回到第 1 页）；有未提交更改时被守卫拦下并说明原因 | `src/shell.rs:1066、src/data_page.rs:4969、src/data_page.rs:5049` |

### `status_bar.row1.ops.cancel` 「取消」图标按钮

kind `control` · status `active` · parent `status_bar.row1.ops`

- 样式：尺寸 `icon_button_width`、`icon_button_height`
- 无障碍：可访问名「取消：中止正在执行的提交批次（已执行的行会整批回滚）」；Tab 在行操作组内依次 Tab 可达
- 图标：`action.cancel`
- 平台差异：Windows：行 1 这枚「取消」两用：查询页按 `busy`（执行中 / 待执行）可点，点击取消查询、重复点幂等；数据页按 `tab.Loading || submitting` 可点，点击 `CancelDataRequests` 并在行 2 左格写「已取消表数据请求（已取到的行保留在页面上）」。停在别的页时禁用，tooltip「取消：当前没有执行中的查询」。§7.1 只记了查询页那一种。
- 同步：Linux done（§7「取消只在批次在途时可点」；RowOpsState.cancel + request_cancel_submit）；Windows partial（§7.1：查询页执行中「取消」可点、重复点幂等；数据页的取消可用性（`Loading || submitting`）与两者共用的工具提示见 `platform_notes.windows`。）；macOS none
- 实现备注：图标用实心方块：主题的 process-stop 在 Yaru 是八边形、在 Adwaita 是叉号，两套主题两种样子，也不是「停止」的通用形态。只在提交批次在途时可点；重复点幂等，只写日志，不会误触发新提交。语义是中止本批并整批回滚，不是撤销已提交的数据（ADR 0018）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 提交批次在途（Submitting） | 中止本批并整批回滚已执行的行；重复点幂等（只写日志），提交结束后按钮自行置灰 | `src/shell.rs:1066、src/data_page.rs:4943、src/data_page.rs:5049` |

### `status_bar.row1.selected` 选中行数（N selected）

kind `control` · status `active` · parent `status_bar.row1`

- 样式：尺寸 `font_status`、`gap_md`
- 无障碍：可访问名「已选中行数」
- 同步：Linux done（§7 行 1 选中数；apply_snapshot 的 selected_count）；Windows none（行 1 没有选中数：XAML 里没有 `N selected` 这一格，§7.1 也未记录。）；macOS none
- 实现备注：文案固定为 `N selected`（拉丁词，不写「已选 N 行」）：与行 2 的 `Last Refresh:` 同一套拉丁读数。只在表标签上有效；Objects 页、查询标签、启动态一律隐藏，不显示 `0 selected`。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 表标签且选中行数大于 0 | 显示「N selected」（选中 1 行也出现，不只多选）；tooltip「已选中 N 行：Ctrl/Shift 可多选，删除按批量语义提交」；选中集清空时整格隐藏 | `src/shell.rs:1429、src/data_page.rs:4936` |

### `status_bar.row1.detail_toggle` 展示形式切换（按行浏览 / 字段 · 值）

kind `control` · status `active` · parent `status_bar.row1`

- 样式：尺寸 `icon_button_width`、`icon_button_height`、`gap_md`
- 无障碍：可访问名「字段 / 值：只看选中行的每个字段与它的值」；Tab 在行操作组之后、分页器之前
- 图标：`view.detail_toggle`
- 同步：Linux done（§7 行 1 展示形式切换；v0.2.5 RowOp::ToggleDetail 与快照回推）；Windows none（Windows 没有展示形式切换：XAML 与代码里都没有「字段 | 值」视图，§7.1 未记录。）；macOS none
- 实现备注：位置在分页器**前方**（§7 / workspace-model §7 的同一处摆放）：选中数 → 展示形式 → 分页器。它是 toggle，自己不记状态：按下只把「用户按了这一下」交给数据页，选中态由数据页快照推回，否则切标签 / 换表后按钮和屏幕会各说一套。按下态沿用 Adwaita 的 flat toggle 外观（未单独写主题 token，P1 定色）。只读连接也允许（它只是换一种看法，不写库）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 有表标签且选中了一行（不要求可编辑） | 数据网格在按行浏览与「字段 | 值」两列之间切换：按下后只讲选中的那一行（字段名 + 当前值，与网格同一份显示文本），再按一次切回按行浏览；不可用时置灰，tooltip 按场景给「字段 / 值：只看选中行的每个字段与它的值（先选中一行）」「…（当前不可用：<原因>）」「…（再点一次切回按行浏览）」 | `src/shell.rs:1168、src/data_page.rs:2053、src/shell.rs:1486` |
| load | 数据页推来新快照（切标签 / 换表 / 选中集被清空） | 选中态由快照的展示形式推回；选中集清空或换表时自动复位到按行浏览，不复位成「上次按下的样子」 | `src/shell.rs:1426、src/data_page.rs:2073` |

### `status_bar.row1.pager` 分页器（首页 / 上页 / 下页 / 末页 + 只读页码摘要）

kind `container` · status `active` · parent `status_bar.row1`

- 样式：尺寸 `pager_button_min_width`、`min_target`、`gap_sm`、`pager_font`
- 平台差异：Windows：行 1 右端只有只读页码 `第 N 页 / 共 ? 页`（`PagerText`，`DataStatusText.PageLabel`），没有四个翻页按钮、没有页码输入；上一页 / 下一页 / 加载更多都在数据页动作行。
- 同步：Linux done（§7 行 1 分页器；PAGER_NOTE + apply_snapshot 的可见性与可用性）；Windows partial（只有只读页码一格，没有四枚翻页按钮；取数动作在数据页动作行，§7.1 未记录状态栏分页器。）；macOS none
- 实现备注：四个翻页按钮连成一组（组内间距 0）+ 只读摘要，两者间距 6（gap_sm）；整组共用一条 tooltip。整组 tooltip：「offset 分页（#118）：首页 / 上页 / 下页 / 末页；页码只读，跳转到远处页码不在本版提供」。首页 / 末页用自有图标（`|<` / `>|`）：主题的双尖括号与相邻的单箭头只差一条线，压到小字号后读不出是两档不同动作。没有表标签时摘要整格隐藏、四个按钮置灰但不隐藏（保持行 1 右组宽度不跳，避免切标签时整行抖动）。旧契约 §7 写「按钮整格隐藏」，2026-11-12 按实现定稿为置灰不隐藏（契约 §9.1 已收口）。按钮动作只交给当前数据页，由数据页决定目标 offset 并在有未提交更改时拒绝——状态栏不自己算页码。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 当前标签不是表标签 | 摘要整格隐藏（不留「未打开表」占位），四个翻页按钮一律置灰，不出现「看起来能点」的按钮 | `src/shell.rs:1394、src/shell.rs:1422、src/shell.rs:1511` |

### `status_bar.row1.pager.first` 「首页」按钮

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_button_min_width`、`min_target`
- 无障碍：可访问名「首页（offset = 0）」；Tab 在分页器组内依次 Tab 可达
- 图标：`pager.first`
- 同步：Linux done（§7 行 1 分页器四按钮；PagerAction::First）；Windows none（§7.1 未记录状态栏分页器（翻页在数据页动作行）。）；macOS none
- 实现备注：tooltip「首页（offset = 0）」；页码只读，按钮不显示页码数字。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表标签，且不在第一页（无未提交更改） | 跳到第一页（offset 归零），排序 / 筛选保持；有未提交更改时由数据页拒绝并说明原因 | `src/shell.rs:1142、src/data_page.rs:319` |

### `status_bar.row1.pager.previous` 「上一页」按钮

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_button_min_width`、`min_target`
- 无障碍：可访问名「上一页」；Tab 在分页器组内依次 Tab 可达
- 图标：`pager.previous`
- 同步：Linux done（§7 行 1 分页器四按钮；PagerAction::Previous）；Windows none（§7.1 未记录状态栏分页器（翻页在数据页动作行）。）；macOS none
- 实现备注：tooltip「上一页」；第一页时置灰，不是隐藏。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表标签且不在第一页（无未提交更改） | 退回上一页（offset 减一页），排序 / 筛选保持 | `src/shell.rs:1142、src/data_page.rs:319` |

### `status_bar.row1.pager.next` 「下一页」按钮

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_button_min_width`、`min_target`
- 无障碍：可访问名「下一页」；Tab 在分页器组内依次 Tab 可达
- 图标：`pager.next`
- 同步：Linux done（§7 行 1 分页器四按钮；PagerAction::Next）；Windows none（§7.1 未记录状态栏分页器（翻页在数据页动作行）。）；macOS none
- 实现备注：tooltip「下一页」；没有下一页时置灰，不靠「点了没反应」表达边界。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表标签且还有下一页（无未提交更改） | 前进一页（offset 加一页），排序 / 筛选保持 | `src/shell.rs:1142、src/data_page.rs:319` |

### `status_bar.row1.pager.last` 「末页」按钮

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_button_min_width`、`min_target`
- 无障碍：可访问名「末页（需要总数）」；Tab 在分页器组内依次 Tab 可达
- 图标：`pager.last`
- 同步：Linux done（§7 行 1 分页器四按钮；PagerAction::Last 需要总数）；Windows none（§7.1 未记录状态栏分页器（翻页在数据页动作行）。）；macOS none
- 实现备注：tooltip「末页（需要总数）」：总数未知（驱动不报总数或计数超时）时置灰，不猜页数。总数三态（共 N / 约 N / 共 ?）只体现在摘要里，按钮不显示总数。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 表标签且已知总页数、不在末页（无未提交更改） | 跳到最后一页；总数未知时按钮置灰（不发请求、不编造页数） | `src/shell.rs:1142、src/data_page.rs:319` |

### `status_bar.row1.pager.summary` 只读页码摘要（第 N 页 / 共 M 页 · 总数）

kind `control` · status `active` · parent `status_bar.row1.pager`

- 样式：尺寸 `pager_font`
- 无障碍：可访问名「页码摘要」
- 平台差异：Windows：同一格文案 `第 N 页 / 共 ? 页`（总数已知时 `第 N / M 页`）：总数未知一律 `?`，没有「共 N 行 / 约 N 行」后缀，也没有「读取中…」「已断开」这类中间态（由页内状态行承担）。
- 同步：Linux done（§7 行 1 分页器只读摘要；apply_snapshot 的 pager_text）；Windows partial（有只读页码，没有总数后缀与中间态文案；§7.1 未记录状态栏页码摘要。）；macOS none
- 实现备注：页码只读：本版没有页码输入框、没有跳转到远处页码。总数只有服务端给的时候才追加（共 N 行 / 约 N 行），未知就只写页码，不写 `?` 冒充。文案用拉丁数字与中文标点，字号按 pager_font 与翻页按钮的视觉高度对齐。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 表标签 | 写「第 P 页」，已知页数时写「第 P / M 页」，有总数时追加「 · 共 N 行」式总数；读取中写「读取中…」、读取失败写「读取失败」、断连写「已断开」；没有表标签时整格隐藏 | `src/shell.rs:1394、src/data_page.rs:1923` |

### `status_bar.row2` 行 2（SQL 摘要 · Last Refresh / 空态摘要）

kind `container` · status `active` · parent `status_bar`

- 样式：槽位 `status_bar.row2`；尺寸 `row_height_tree`、`gap_xl`、`gap_md`
- 平台差异：Windows：行 2 两格与 §7 不同：左格是 `SQL: <摘要>` 或**一次性提示**（`_notice` 优先：编辑被拒原因 / 提交结果 / 守卫说明 / 标签上限），右格依次是**事务状态**、会话名、`Last Refresh`。事务状态取 `只读` / `自动提交` / `未提交 N 行` / `提交中…` / `已回滚`（优先序 提交中 → 只读 → 有脏行 → 已回滚 → 自动提交），无显式事务时 tooltip 写「事务：<label>（没有显式事务：提交时按自动提交执行）」——Linux 状态栏没有这一格（v0.2.3.3 删掉，事务在表格工具条）。
- 同步：Linux done（§7 行 2；workspace-model §7 的两段排版与 ASCII 冒号约束）；Windows partial（§7.1 只记了行 2 左格 `SQL: <摘要>` 与一行连接状态；代码现状是左格 SQL 摘要 / 提示、右格 事务状态 · 会话名 · `Last Refresh`（连接状态线在行 1 左格，见 `platform_notes.windows`）。）；macOS none
- 实现备注：只有两段：左段 SQL 摘要（可省略），右段 Last Refresh，空态时右段换成一行启动摘要（连接名，或「未连接 · 驱动 N · 浏览会话 M」）。左段紧贴行操作正下方（同一左边界），不跨到窗口最左。文案一律用 ASCII 冒号（`SQL:` / `Last Refresh:`）：全角冒号会把中文行盒高度带进来，两段不在同一基线（实测偏高 2px）。行号区间、连接名、库名、事务状态、上次耗时都不在这里：表名 / 库在标签名里，事务在表格工具条里。

### `status_bar.row2.sql` SQL 摘要（左段）

kind `control` · status `active` · parent `status_bar.row2`

- 样式：尺寸 `font_status`
- 无障碍：可访问名「当前 SQL 摘要」
- 平台差异：Windows：这一格在 Windows 兼作一次性提示行：`_notice`（编辑被拒原因 / 提交结果 / 守卫说明 / 「新建连接失败：<文案>」/「无法新建标签：标签上限 8 个（当前已开 N 个）：请先关闭一个标签再新建」）优先于摘要；断连与未就绪也写这里（`已断开：<文案>`、`SQL: 尚未执行`）；数据页没有 SQL 摘要，写 `表 <表名> · <页码> · <总数>`。
- 同步：Linux done（§7 行 2 左段三种来源 + 断连 / 未执行文案）；Windows partial（§7.1 只有查询页的 `SQL: <摘要>`（单行、折叠空白、80 字符上限；没执行过写「尚未执行」）与断连时把原因写进行 2 左格；数据页左格写的是 `表 <表名> · 第 N 页 / 共 ? 页 · 共 N 行`（`DataStatusText.Summary`），不是 `SQL: … LIMIT …`。契约 §9.1 已裁决：`SqlSummaryText` 补 `IsTextSelectionEnabled`（断连原因要可复制）；Windows 端待实现。）；macOS none
- 实现备注：单行省略：长语句截断成一行，全文进 tooltip。冒号是 ASCII 的 `SQL:`，不是全角「：」。当前标签是「字段 | 值」视图或正在编辑时也照旧显示这一页的摘要——摘要讲的是取数语句，不是展示形式。连接刚建立、还没有标签状态推送时，这一格会短暂显示「SQL: 已连接 · <服务端版本>」，首个标签状态到达后被覆盖。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 当前标签是表标签 | 写「SQL: <摘要>」：当前筛选 / 排序拼出的只读 `SELECT … LIMIT {offset}, {page_size}`（只展示、不执行，值以字面量呈现）；筛选或排序变化后跟着更新 | `src/shell.rs:1537、src/shell.rs:1450、src/data_page.rs:1923、src/browse_view.rs:821` |
| load | 当前标签是查询标签 | 写「SQL: <最近一次查询摘要>」：沿用查询页自己的摘要（单行），没执行过时由查询页给文案 | `src/ui.rs:1865、src/ui.rs:1866` |
| load | 当前标签是 Objects 页 | 重置为「SQL: 尚未执行」——Objects 页没有取数语句，不用上一张表的摘要蒙着 | `src/ui.rs:1879、src/shell.rs:1202` |
| load | 连接断开 / 启动态未连接 | 断连写「SQL: 连接已断开」；启动态写「SQL: 尚未连接」 | `src/ui.rs:600、src/startup.rs:248` |

### `status_bar.row2.last_refresh` Last Refresh（右段）

kind `control` · status `active` · parent `status_bar.row2`

- 样式：尺寸 `font_status`、`gap_md`
- 无障碍：可访问名「上次读取时间」
- 平台差异：Windows：行 2 右格第三段 `RefreshAgeText` 默认写「Last Refresh: —」——没刷新过时是占位破折号，不隐藏；代码里没有 30 秒重算的定时器，也没有墙上时间 tooltip（§7.1 未记录这一格）。行数读数不在这一格：查询页由行 1 的 `LastRefreshText` 写 `N 行 / 已取 M 行`（还有更多时 `? 行 / 已取 N 行（还有更多）`）。
- 同步：Linux done（§7 行 2 右段；apply_snapshot / tick_relative_time / clock_time）；Windows none（Windows 有这一格但是静态占位「Last Refresh: —」：§7.1 未记录相对时间、30 秒重算与 tooltip。）；macOS none
- 实现备注：相对时间只到分钟：`<1m` / `Nm` / `Nh` / `Nd`，与文件列表同一套格式化函数。没有刷新过（从未读到数据）时整格隐藏，不写 `Last Refresh: —`。tooltip 给墙上时间 `HH:MM:SS` 并提示「offset 分页不保证跨页稳定」——相对时间只是一句提示，精确时刻在 tooltip 里。墙上时间由「现在减去已经过去的时长」反推（Instant 没有日历语义），转换失败时退回相对时间，不显示错误时间戳。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 读到过数据（有上次刷新时刻） | 写「Last Refresh: <相对时间>」；每 30 秒重算一次，数据页推送新快照时也重算 | `src/shell.rs:1439、src/shell.rs:1457、src/shell.rs:137、src/browse_view.rs:918` |
| hover | 该格可见 | tooltip 给墙上时间与分页提示：「上次读取时间：HH:MM:SS（offset 分页不保证跨页稳定，必要时点旁边的刷新）」 | `src/shell.rs:1444、src/shell.rs:1587` |

### `status_bar.row2.session` 空态摘要（连接名 / 未连接 · 驱动 N · 浏览会话 M）

kind `control` · status `active` · parent `status_bar.row2`

- 样式：尺寸 `font_status`、`gap_sm`
- 无障碍：可访问名「连接与会话摘要」
- 平台差异：Windows：这一格在 Windows 是 `SessionText`：列出所有已连接连接的**名字**（多个用「 · 」拼），没有会话写「未连接」；驱动 / 浏览会话计数在行 1 左的后端状态线里，不重复写这一格。
- 同步：Linux done（§7 行 2 空态摘要；refresh_session + session_row_visible + idle_status_text）；Windows partial（§7.1 记的是 `已连接 · 驱动 N · 浏览会话 M`（GetHealth 真实计数，5 秒一轮；未就绪 `未连接 · 驱动 0 · 浏览会话 0`，另有 `连接中 · 驱动 0 · 浏览会话 0` 一档）；代码把这条放在行 1 左格，行 2 这一格只写会话名，见 `platform_notes.windows`；「有表标签时整格隐藏」未记录。契约 §9.1 已裁决：与 Linux 一致，当前是表数据标签时整格不出现；Windows 端待实现。）；macOS none
- 实现备注：只在**没有表标签**时出现：有表标签时这一行只留 SQL 摘要与 Last Refresh（表名 / 库 / 事务在别处已经有了）。启动态把连接名「未连接」与「驱动 N · 浏览会话 M」用「 · 」拼成一行，读作「未连接 · 驱动 0 · 浏览会话 0」。连接后显示连接名；断连后连接名被清空，只剩驱动与浏览会话计数。单行省略、最多 48 个字符宽，窗口变窄时这一格最先被压（右侧留 6 的边距，最后一个字不贴窗口边）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 没有表标签 | 显示连接名，或「未连接 · 驱动 N · 浏览会话 M」（N / M 来自启动态与连接注册表的真实计数）；有表标签时整格隐藏 | `src/shell.rs:1558、src/shell.rs:1578、src/startup.rs:66、src/startup.rs:243` |

## 标签条与查询标签（`tabs`）

工作区顶部的标签条，以及查询标签页里的动作行、编辑器与结果区：标签的新建 / 复用 / 上限 / 关闭守卫，查询的执行 / 取消、五态、结果表头与续拉。

| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |
|---|---|---|---|---|---|---|
| `tabs` | 标签条与查询标签工作区 | area | active | done | partial | — |
| `tabs.strip` | 　标签条容器 | container | active | done | partial | — |
| `tabs.strip.tab` | 　　标签项（标题 · 关闭 ×） | row | active | done | partial | — |
| `tabs.strip.tab.table` | 　　　表数据标签（标题与 tooltip） | item | active | done | partial | — |
| `tabs.strip.tab.query` | 　　　查询标签（标题与 tooltip） | item | active | done | partial | — |
| `tabs.strip.tab.close` | 　　　标签关闭按钮（×） | control | active | done | partial | — |
| `tabs.strip.new` | 　　「＋」新建查询标签 | control | active | done | partial | — |
| `tabs.limit` | 　「无法新建标签」提示 | control | active | done | partial | — |
| `tabs.query` | 　查询标签页 | container | active | done | partial | — |
| `tabs.query.actions` | 　　动作行（执行 / 取消） | control | active | done | done | — |
| `tabs.query.editor` | 　　SQL 编辑器（行号沟 + 等宽文本） | control | active | done | partial | — |
| `tabs.query.state` | 　　结果区状态行（五态） | control | active | done | done | — |
| `tabs.query.results` | 　　结果区 | container | active | done | partial | — |
| `tabs.query.results.header` | 　　　结果表头（列名 · 原始类型） | control | active | done | done | — |
| `tabs.query.results.grid` | 　　　结果行区（虚拟化表格） | control | active | done | partial | — |
| `tabs.query.results.more` | 　　　续拉（滚动到末尾自动续拉 / 「加载更多」） | control | active | done | done | — |
| `tabs.close_dialog` | 　关闭标签确认框 | control | active | done | — | — |
| `tabs.close_dialog.discard` | 　　「放弃更改并关闭」/「回滚事务并关闭」 | item | active | done | — | — |
| `tabs.close_dialog.save` | 　　「保存并关闭」/「保存、提交事务并关闭」 | item | active | done | — | — |
| `tabs.close_dialog.cancel` | 　　「取消」 | item | active | done | — | — |

### `tabs` 标签条与查询标签工作区

kind `area` · status `active`

- 样式：槽位 `workspace.tabstrip`；尺寸 `tab_strip_height`
- 同步：Linux done（v0.2.5 Linux 客户端；§4 十一条逐条对照 tabs.rs（标签条 + 查询标签））；Windows partial（TabView 承载：固定 Objects 页（IsClosable=false、不占名额）+ 可关闭的查询 / 数据页；标签条外形与密度档 §7.1 未记录。）；macOS none
- 实现备注：标签条在最上，下面是当前标签的内容区。`Objects` 固定页钉在最左、没有会话、不占「有会话标签」的 8 个名额。标签与内容同属一个 AdwTabView：先摘页面再释放会话（on_page_detached），避免闭包拿着已销毁的控件。标签顺序即打开顺序；拖动标签会改 AdwTabView 的顺序，标签位次按 pages() 取而不是按创建顺序。

### `tabs.strip` 标签条容器

kind `container` · status `active` · parent `tabs`

- 样式：槽位 `workspace.tabstrip`；尺寸 `tab_strip_height`、`gap_sm`、`gap_xxs`、`min_target`、`objects_min_width`；颜色 `sqarvo_separator`
- 原生控件：gtk:AdwTabBar + AdwTabView / winui:TabView / swift:—
- 同步：Linux done（§4 标签条；theme.rs `.sqarvo-tab-strip` / `.sqarvo-tab-bar` 规则）；Windows partial（TabView 自带标签条（查询页可关闭、Objects 钉首位）；标签条上的 ＋ 按钮与拖拽重排 §7.1 未记录。）；macOS none
- 实现备注：标签条只有一条下边线（@sqarvo_separator 的 1px）；libadwaita 的 `tabbar .box` 自带一条 shade，已压掉——两条 1px 线紧贴时会被看成「分割线画了两遍」。标签条常驻不自动隐藏（autohide=false）；标签自身的最小点击区不小于 min_target，压下去的是外层留白。固定 Objects 页按 objects_min_width 宽度钉在最左，没有关闭按钮可用的语义（点 × 由 on_close_page 拒绝）。

### `tabs.strip.tab` 标签项（标题 · 关闭 ×）

kind `row` · status `active` · parent `tabs.strip`

- 样式：尺寸 `min_target`、`font_ui`；颜色 `window_fg_color`；状态 hover=`window_fg_color`、selected=`window_fg_color`
- 无障碍：可访问名「（标签内容：标题）」；Tab 可聚焦；Ctrl+Tab / Ctrl+Shift+Tab 循环切换，Alt+1..9 直选第 N 个
- 同步：Linux done（§4 切换标签 / 标签被摘掉两行；select_page 与 on_page_detached）；Windows partial（标签可切、可关；切换不重建会话与 detach 时释放会话的顺序 §7.1 未记录。）；macOS none
- 实现备注：hover 与选中都是 @window_fg_color 的透明档（灰），不是品牌紫：标签回答「现在看的是哪一页」这种位置问题，不该和主操作按钮抢同一层视觉。行结构：标题（单行，超宽省略）+ 关闭 ×。`Objects` 页也带 ×，但点它会被 on_close_page 拒绝并记日志（libadwaita 1.9 的 AdwTabPage 没有「不可关闭」属性）。标签本身不显示会话状态：会话在不在看 tooltip 的「会话 <id 前 8 位>」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 标签存在且不是当前页 | 切到该标签：不重建会话、不发请求；窗口标题、状态栏、工具条高亮按新标签刷新；其它标签的筛选与排序面板被收起 | `src/tabs.rs:1361、src/tabs.rs:2173、src/tabs.rs:2289、src/tabs.rs:2302` |
| load | 页面已从标签条摘掉（detach） | 释放该标签自己的会话（Cancel 在途查询 → Close 游标 → Disconnect），并收起它的筛选面板；没有当前标签时选中第一个 | `src/tabs.rs:2819、src/tabs.rs:2834、src/tabs.rs:2839` |

### `tabs.strip.tab.table` 表数据标签（标题与 tooltip）

kind `item` · status `active` · parent `tabs.strip.tab`

- 无障碍：可访问名「（行内容：<对象名> @ <catalog>）」
- 同步：Linux done（§4 表数据标签标题 / 打开表 / 复用三行；table_tab_title 与 tooltip_for）；Windows partial（标签名 `表名 @ 库`（库为空写「默认」）、同表复用、上限 8、有未提交改动带 `*` 已落地（§7.1 数据页 #173）；tooltip 的「会话 <id 前 8 位>」§7.1 未记录。）；macOS none
- 实现备注：标题固定是 `<对象名> @ <catalog>`；catalog 为空写「默认」——不编造库名，也不把连接名塞进标签。tooltip 是 `<标题> · 标签会话`；会话建立后阶段换成 `会话 <id 前 8 位>`。窗口标题仍是 `<标签标题> (<连接名>)`（副标题是连接级信息，不随标签改）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| double_click | 在对象树 / Objects 页双击表或视图，且没有同表标签、未达上限 | 新建表数据标签：标题「<对象名> @ <catalog>」（catalog 空写「默认」），并为该标签请求一条**属于它自己的**数据库会话（Connect）；成功后切到该标签 | `src/tabs.rs:1795、src/tabs.rs:1832、src/tabs.rs:1903、src/tabs.rs:1936` |
| double_click | 已有对象名 + catalog + 连接键三者完全一致的表标签 | 只切到已有标签：不新建标签、不新建会话、不重载数据；复用检查在标签上限之前，已开满 8 个仍可切过去 | `src/tabs.rs:1804、src/tabs.rs:122、src/tabs.rs:1588` |
| load | 该标签的会话已建立 | tooltip 的阶段换成「会话 <id 前 8 位>」；表标签的 tooltip 不写库名（库已经在标题里） | `src/tabs.rs:2552、src/tabs.rs:2262` |

### `tabs.strip.tab.query` 查询标签（标题与 tooltip）

kind `item` · status `active` · parent `tabs.strip.tab`

- 无障碍：可访问名「（行内容：查询 N）」
- 平台差异：Windows：标签名后缀 `*` = 编辑器内容与「按下执行那一刻的编辑器内容」不同（v0.1 没有「保存查询」，标记口径就是「未执行」）；Linux 的标题只有「查询 N」，不带任何标记。
- 同步：Linux done（§4 查询标签标题；query_tab_title 递增计数器与 tooltip_for）；Windows partial（标签名 `查询 N`、`*`（编辑器内容 ≠ 执行那一刻的内容，v0.1 没有「保存查询」）、上限 8 已落地（§7.1 #172）；标签 tooltip 里的库 / 连接 / 会话三段 §7.1 未记录。）；macOS none
- 实现备注：标题只有「查询 N」：**不带** `@ 库`。序号自 1 起递增、不复用已关闭的序号（关掉「查询 2」后下一个仍是「查询 3」）。库与连接放 tooltip：`查询 N · 标签会话 · 库 <catalog> · 连接 <连接名>`，会话建立后阶段换成 `会话 <id 前 8 位>`。多连接各自开着「查询 1」时靠 tooltip 区分。窗口标题仍是 `<标签标题> (<连接名>)`。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 新建查询标签 | 标题写「查询 N」，序号自 1 起递增且不复用已关闭的序号；tooltip 写「查询 N · 标签会话 · 库 <catalog> · 连接 <连接名>」（catalog 空写「默认」） | `src/tabs.rs:95、src/tabs.rs:1877、src/tabs.rs:1884、src/tabs.rs:2262` |
| load | 该查询标签的会话已建立 | tooltip 的阶段换成「会话 <id 前 8 位>」，库与连接仍在后两段 | `src/tabs.rs:2552` |

### `tabs.strip.tab.close` 标签关闭按钮（×）

kind `control` · status `active` · parent `tabs.strip.tab`

- 样式：尺寸 `min_target`
- 无障碍：可访问名「关闭标签」
- 图标：`action.close_tab`
- 同步：Linux done（§4 关闭标签（× / Ctrl+W）与重复关闭请求；on_close_page 与 close_current）；Windows partial（`Ctrl+W` 关当前页、页面可关闭（IsClosable）已落地；关闭守卫与重复关闭请求 §7.1 未记录（数据页的关页守卫是「状态栏二次提示」，见 §7.1 数据页）。）；macOS none
- 实现备注：× 与 `Ctrl+W` 走同一条 `close-page`：只有这一条关闭入口，没有第二套旁路。`Objects` 固定页的 × 点了也不关：on_close_page 拒绝并记一行日志（AdwTabPage 没有「不可关闭」属性）。确认框已在显示时再点 × 只记日志，不叠第二个框。菜单里已不列「关闭标签」：入口只有标签上的 × 与 `Ctrl+W`，没有第三条旁路（菜单裁剪口径见契约 §1.4）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 非 Objects 页，且没有脏行、没有未提交事务 | 直接关标签：摘页面 → 释放该标签会话（Cancel 在途查询 → Close 游标 → Disconnect） | `src/tabs.rs:2645、src/tabs.rs:2662` |
| click | 非 Objects 页，且有脏行或有未提交事务 | 先拦下这次关闭并弹三选确认框（tabs.close_dialog）；在用户答完之前标签不收场 | `src/tabs.rs:2662、src/tabs.rs:2677、src/tabs.rs:2681` |
| click | 该标签的确认框已在显示 | 忽略这次请求并记一行「关闭标签 `<id>` 的确认框已在显示」，不叠第二个框；答完（或关掉）那个框后再点才会重新弹 | `src/tabs.rs:2674` |
| click | 该标签是固定 `Objects` 页 | 不关闭，记一行「Objects 页固定存在，不可关闭」 | `src/tabs.rs:2657` |
| key Ctrl+W | 存在当前标签 | 关闭当前标签：与点 × 同一条 `close-page`（有脏行 / 事务时同样先弹三选） | `src/ui.rs:328、src/tabs.rs:2856` |

### `tabs.strip.new` 「＋」新建查询标签

kind `control` · status `active` · parent `tabs.strip`

- 样式：尺寸 `min_target`
- 无障碍：可访问名「新建查询标签」
- 图标：`action.new_query`
- 同步：Linux done（§4 新建查询标签（四个入口）；＋ 按钮接线与 win.new-tab 动作）；Windows partial（`Ctrl+T` 新建查询页已落地；标签条上的 ＋ 按钮 §7.1 未记录（新建入口只写了 Ctrl+T 与菜单）。）；macOS none
- 实现备注：四个入口同一条路径，没有旁路：工具条「新建查询」、标签条 ＋、`Ctrl+T`、菜单「文件 → 新建查询标签」；成功后把焦点交给编辑器。超限时不新建、只弹 tabs.limit（同一段文案不重复弹）；连点 ＋ 不会堆一摞对话框。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 后端就绪、工作区已建立 | 未达上限 → 新建「查询 N」并聚焦编辑器；已达上限 → 不新建，弹 tabs.limit 的「无法新建标签」提示 | `src/tabs.rs:1352、src/tabs.rs:1856、src/tabs.rs:1904、src/ui.rs:263、src/ui.rs:4595` |
| key Ctrl+T | 后端就绪、工作区已建立 | 与点 ＋ 同一条 `win.new-tab`：新建「查询 N」并聚焦编辑器；超限时同样只弹提示 | `src/ui.rs:327、src/ui.rs:4757、src/tabs.rs:1856` |

### `tabs.limit` 「无法新建标签」提示

kind `control` · status `active` · parent `tabs`

- 样式：槽位 `dialogs`；尺寸 `badge_min_height`
- 无障碍：可访问名「无法新建标签」
- 平台差异：Windows：超限提示走**状态栏**一行文字（§7.1：不弹假对话框），文案与 Linux 同一句「标签上限 8 个（当前已开 N 个）：请先关闭一个标签再新建」；Linux 是 `AdwAlertDialog`。名额也略窄：Windows 数的是查询页（与后端 `QueryClient.MaxConcurrentQueries` 对齐），Linux 数的是全部「有会话标签」（表数据 + 查询）。
- 同步：Linux done（§4 标签上限；limit_message 与 show_limit）；Windows partial（上限 8 与同一段文案已落地，但走**状态栏**提示，不弹对话框（§7.1：不弹假对话框）。）；macOS none
- 实现备注：上限是 8 个「有会话标签」（表数据 + 查询）；`Objects` 固定页不占名额。常量 MAX_TABS 与后端 QueryRegistry.max_concurrent 对齐。同一段文案不重复弹：连点 ＋ / Ctrl+T 只留一个框，日志里仍每次记一行拒绝原因。文案不是静默失败：必须同时给出上限、当前已开数量和「先关一个」的建议。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 同时打开的有会话标签达到 8 时请求新建（工具条 / ＋ / Ctrl+T / 菜单任一路径） | 拒绝新建并弹 AdwAlertDialog：标题「无法新建标签」，正文「标签上限 8 个（当前已开 N 个）：请先关闭一个标签再新建」，按钮「知道了」；同一段文案已在显示时不重复弹 | `src/tabs.rs:2884、src/tabs.rs:72、src/tabs.rs:59` |
| click | 提示框在显示 | 「知道了」关闭提示框；本次请求不新建任何标签 | `src/tabs.rs:2884` |

### `tabs.query` 查询标签页

kind `container` · status `active` · parent `tabs`

- 样式：槽位 `workspace.content`；尺寸 `gap_page`
- 同步：Linux done（§5.11 版面收敛；SqlPage::new 的组装顺序）；Windows partial（查询页整体在代码里搭（#172，ShellView 一行未改）：动作行 / 编辑器 / 五态 / 表头 / 行区 / 加载更多；分隔线与留白 §7.1 未记录。）；macOS none
- 实现备注：页内自上而下：动作行 → 编辑器（行号沟 + 等宽文本）→ 分隔线 → 结果区（五态一行 → 表头 → 行区 → 续拉）。本区只登记查询标签里的编辑器与结果区；表数据页的工具条、筛选、网格、分页、导出归 data_page。每个查询标签有**自己的**会话；关标签时由 worker 先 Cancel 在途查询、再 Close 游标、最后 Disconnect（§7.1 游标的 Close 时机同口径）。v0.2.5 版面收敛：这一页不再常驻 `会话建立中…`（会话行空着就不占位）、`当前为纯文本编辑，语法高亮将在后续版本提供`、`图例：NULL=…`、`已完成 · 已加载 N 行 · 共 N 行`、`语句 1：… · 查询 query_xxx`（摘要改写进日志 `[query] 语句摘要：…`）。只有失败 / 触顶 / 取消 / 断连 / 执行中需要一句原因时，状态行才出现文字。游标的关停时机（§7.1 六条）：重新执行前先关上一个、取消后关、关页后关、读到底由服务端释放、离开页时若有在途执行则取消并关（空闲游标保留，切回来还能续拉）、页面重建 / 窗口关闭时逐个关。8 个标签上限保证并发游标不超过 8 个。

### `tabs.query.actions` 动作行（执行 / 取消）

kind `control` · status `active` · parent `tabs.query`

- 样式：尺寸 `gap_md`、`gap_sm`、`min_target`
- 无障碍：可访问名「查询动作」
- 同步：Linux done（§5.11 不可用态；执行 / 取消按钮与 Ctrl+Enter、Esc 分流）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Query.cs:289；frontend/windows/Sqarvo.Windows/Views/ShellView.Query.cs:1093；frontend/windows/Sqarvo.Windows/Views/ShellView.Query.cs:700）；macOS none
- 实现备注：按钮文案与可用性随状态互斥：空闲 → 「执行」可点、「取消」不可点；执行中 → 主按钮变「取消」（Linux 是并排两枚：执行置灰、取消可点）。取消可重复点击，第二次起只记日志、不发 RPC。空 SQL 只写一行说明，不清掉已有结果；执行中再按执行 / Ctrl+Enter 直接忽略，不排队。动作行左端是会话信息（有真实会话信息时才占位，不再写「会话建立中…」）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 没有查询在跑，编辑器里有 SQL | 执行编辑器里的 SQL；按钮在运行中变成「取消」（同一入口）；成功后结果区进入「执行中…」并等首批 | `src/sql_page.rs:139、src/sql_page.rs:341、src/sql_page.rs:358、src/sql_page.rs:1052` |
| click | 编辑器为空或只有空白 | 只写一行「SQL 不能为空：请输入后按 Ctrl+Enter」，不清掉已有结果、不发请求 | `src/sql_page.rs:343` |
| key Ctrl+Enter | 没有查询在跑 | 与点「执行」同一条方法（不存在「只有按钮能用」的旁路） | `src/sql_page.rs:300、src/sql_page.rs:341` |
| click | 有查询在跑 | 发 Cancel 并立即进入「已取消」；重复点幂等（不发 RPC、不报错），已取到的行保留 | `src/sql_page.rs:143、src/sql_page.rs:395` |
| key Esc | 有查询在跑 | 取消查询（同一按键在空闲时分流为「清空结果区行选中」） | `src/sql_page.rs:288、src/sql_page.rs:395` |

### `tabs.query.editor` SQL 编辑器（行号沟 + 等宽文本）

kind `control` · status `active` · parent `tabs.query`

- 样式：尺寸 `gap_md`、`gap_sm`、`font_data_cell`
- 无障碍：可访问名「SQL 编辑器」；Tab 新建查询标签后自动获得焦点
- 原生控件：gtk:GtkTextView（等宽） / winui:TextBox（AcceptsReturn） / swift:—
- 同步：Linux done（§5.11 版面收敛；editing TextView（等宽、不折行、Ctrl+Enter 捕获阶段））；Windows partial（行号沟 + 等宽不折行 TextBox（AcceptsReturn、自动滚动条）已实现；但新建查询标签后焦点不落编辑器（Focus 只在 --qa-query 探针里调），要先手点一次才能输入。）；macOS none
- 实现备注：等宽文本、不折行、自动竖向滚动条；没有语法高亮，也不假装有（纯文本）。高度是内容高度区间（min 140 / max 220，未进 metrics：P1 补 key），超出后在编辑器内部滚动。Linux 的编辑器没有行号沟（只有等宽 TextView）；行号沟是 §7.1 记录的 Windows 口径，本端待收敛。编辑器上方不写「当前为纯文本编辑，语法高亮将在后续版本提供」（v0.2.5 去掉）：没有语法高亮不靠一句自称说明，编辑器就是等宽纯文本。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 新建查询标签成功 | 键盘焦点直接落在编辑器上，可以马上输入 SQL | `src/tabs.rs:1904、src/sql_page.rs:336` |
| key Ctrl+Enter | 编辑器有焦点 | 执行编辑器里的 SQL（与动作行的「执行」同一方法）；执行中重复按被忽略 | `src/sql_page.rs:300、src/sql_page.rs:341` |

### `tabs.query.state` 结果区状态行（五态）

kind `control` · status `active` · parent `tabs.query`

- 样式：尺寸 `font_status`、`gap_xl`、`spinner_px`；颜色 `warning_color`
- 无障碍：可访问名「查询状态」
- 同步：Linux done（§5.11 保留的一句话；results.rs set_state 的可见性与 CSS 档）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Query.cs:1066；frontend/windows/Sqarvo.Windows/Workspace/QueryModel.cs:379）；macOS none
- 实现备注：五态互斥且都带稳定文案：执行中带 spinner；空 / 错误 / 取消 / 只读 / 断开各一句，**没有**结果时同一句文案也做占位页的标题与说明。正常完成不再常驻「已完成 · 已加载 N 行 · 共 N 行」（v0.2.5 去掉）：只有失败 / 触顶 / 取消 / 断连 / 执行中需要一句话时才对用户显示。Error / Cancelled 下保留已加载行——状态说「失败」不等于把数据抹掉；触顶（客户端结果集上限）单独一态，不能显示成「查询失败」。Linux 同一状态集合的措辞：「执行中…」/「空结果集 · 共 0 行」/「错误 · 已保留 N 行」/「已取消 · 已保留 N 行」/「已达客户端上限 · 已保留 N 行」/「已断开 · 已保留 N 行」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 结果区状态变化 | 写一句状态：执行中「正在执行…（等待首批数据）」/ 空「尚未执行」或「空结果集…」/ 错误「查询失败：<文案>（<错误码>）」/ 取消「已取消：保留已取的 N 行（游标已释放，可重新执行）」/ 只读「只读结果：服务端声明该结果集只读」；错误与取消下保留已加载行 | `src/results.rs:1043、src/sql_page.rs:609、src/sql_page.rs:635` |
| load | 语句没有结果集（UPDATE / INSERT / DDL） | 不假装有结果：写「影响 N 行」而不是画一张空表（Linux 写「语句 N 影响 N 行（无结果集）」） | `src/sql_page.rs:548、src/sql_page.rs:552` |

### `tabs.query.results` 结果区

kind `container` · status `active` · parent `tabs.query`

- 样式：槽位 `workspace.content`；尺寸 `gap_xl`、`gap_sm`、`radius_md`、`frame_border_width`；颜色 `sqarvo_frame`
- 同步：Linux done（§5.11 版面收敛；ResultsView::new 的装配与 `.sqarvo-table-frame`）；Windows partial（状态行 + 表头 + 行区 + 「加载更多」已落地；状态圆点与「导出 ▾」属其它区（data_page / status_bar），§7.1 只记了导出菜单存在。）；macOS none
- 实现备注：自上而下：状态行 + 说明位（右侧是状态圆点与「导出 ▾」，两者归 data_page / status_bar 的记录）→ 结果表头 → 行区。没有已加载行时把表格换成占位页（标题 + 一句说明），有行时保留表格：取消 / 错误 / 断连之后已加载的行仍可查看。表格外框与表数据页、Objects 列表同一份（frame_border_width + radius_md），圆角靠 overflow=hidden 真正裁到内容；左右各让 gap_xl 与分栏线错开。不摆 `图例：NULL=…` 这类常驻说明（v0.2.5 去掉）：NULL 由单元格自己渲染（斜体 + 降透明度），不靠图例解释。语句摘要（`语句 1：… · 查询 query_xxx`）不常驻界面，改写进日志 `[query] 语句摘要：…（query_id=…）`，排障查日志。

### `tabs.query.results.header` 结果表头（列名 · 原始类型）

kind `control` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `column_header_min_height`、`font_ui`、`font_weight_title`
- 无障碍：可访问名「结果列头」
- 同步：Linux done（§7.1 结果网格的列头口径；results.rs set_columns 的标题拼装）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Query.cs:1122；frontend/windows/Sqarvo.Windows/Workspace/QueryModel.cs:68；frontend/windows/Sqarvo.Windows/Workspace/QueryModel.cs:402）；macOS none
- 实现备注：一列一个字头「列名 · 原始类型」；没有类型信息时只写列名，不编造类型。表头只作展示：点列头不改排序（排序只在表数据页的筛选面板里改，查询结果没有表头排序路径）。多结果集在表头**上方**补一行说明（「结果集 K 段」，K > 1 时再补「只显示第 1 段」）；Linux 目前只把多语句写进日志。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 首批数据到达 | 按列元数据重建列：每列一个表头「列名 · 原始类型」（无类型信息时只写列名）；多结果集时表头上方一行「结果集 K 段」，K > 1 再补「只显示第 1 段」 | `src/results.rs:968、src/sql_page.rs:544、src/sql_page.rs:561` |

### `tabs.query.results.grid` 结果行区（虚拟化表格）

kind `control` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `row_height_data`、`row_number_width`、`column_width`、`font_data_cell`、`font_weight_cell`
- 无障碍：可访问名「查询结果」；Tab 可聚焦；Esc 清空行选中（执行中的 Esc 是取消查询）
- 同步：Linux done（§7.1 结果网格的虚拟化口径；GtkColumnView + RowStore 与 Esc 清选中）；Windows partial（视口内建行 + 上下高度占位 + 表头与行区横向同步都实现了；但结果行不可选中，Esc 清空行选中（含 a11y.tab 那条）没有实现。）；macOS none
- 实现备注：虚拟化：只建视口内的行，滚动时不把整批行都做成控件；行号列跨批次连续（1 起、右对齐），宽表靠横向滚动而不是把列压碎。行号列与数据列的实际宽度目前是代码常量（行号 72、数据列 220，未进 metrics：metrics 里是 row_number_width / column_width），P1 收敛。取消 / 错误 / 断连之后不清空行区：已加载的行保留，状态行说明为什么停在这里。没有行级脏色 / 删除线之外的额外标记（查询标签的单表编辑归 §5.10，脏行数只在状态行与状态圆点）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| key Esc | 结果区有选中行、没有查询在跑 | 清空行选中；执行中的 Esc 仍分流为「取消查询」 | `src/results.rs:379、src/results.rs:382` |

### `tabs.query.results.more` 续拉（滚动到末尾自动续拉 / 「加载更多」）

kind `control` · status `active` · parent `tabs.query.results`

- 样式：尺寸 `min_target`
- 无障碍：可访问名「加载更多」
- 同步：Linux done（§7.1 续拉口径的 Linux 实现（滚动预取 + 单批在飞）；maybe_prefetch）；Windows done（frontend/windows/Sqarvo.Windows/Views/ShellView.Query.cs:457；frontend/windows/Sqarvo.Windows/Views/ShellView.Query.cs:817；frontend/windows/Sqarvo.Windows/Workspace/QueryModel.cs:52）；macOS none
- 实现备注：每批行数由服务端分批返回；同一时刻最多一批在飞，触顶（客户端结果集上限）之后不再续拉。Linux 只有「滚动到接近末尾就自动续拉」这一条路径（没有单独的「加载更多」按钮）；§7.1 记录 Windows 另有可点按的「加载更多」入口。续拉是追加，不重跑 SQL、不换页；如果用户已经滚到底而服务端还有数据，状态行仍是「加载中 · 已加载 N 行」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| scroll | 结果仍在分批读取（has_more），且滚动到接近末尾 | 自动发一次 FetchBatch 续拉下一批；同一时刻最多一批在飞，触顶后不再拉（暂停原因写日志） | `src/results.rs:1181、src/results.rs:1003` |
| load | 服务端返回的批次读到底（has_more=false） | 不再续拉：行区留在「已完成」，状态行回到正常态（不再常驻「已加载 N 行」） | `src/results.rs:1043、src/sql_page.rs:585` |

### `tabs.close_dialog` 关闭标签确认框

kind `control` · status `active` · parent `tabs`

- 样式：槽位 `dialogs`；尺寸 `badge_min_height`
- 无障碍：可访问名「关闭标签？」
- 同步：Linux done（§4 关闭对话框；close_prompt 与 present_close_dialog）；Windows none（§7.1 未记录（Windows 的关页守卫是状态栏二次提示，不弹对话框）。）；macOS none
- 实现备注：只在有脏行或有未提交事务时出现；默认响应与关闭响应都是「取消」（关闭窗口的叉不会变成放弃）。正文至少一行「关闭「<标题>」会同时断开它自己的数据库会话。」——先说会话，再说脏行 / 事务。被 `close-page` 信号拦下的那次关闭必须由 `close_page_finish(page, confirm)` 收尾：不回话页面会停在「正在关闭」，之后再点关闭毫无反应。按钮的破坏性 / 建议档：放弃（或回滚）是破坏性、保存是建议、取消是默认；只有事务没有脏行时只给「回滚事务并关闭」+「取消」。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| load | 关闭一个有脏行或有未提交事务的标签 | 标题「关闭标签？」；正文至少一行「关闭「<标题>」会同时断开它自己的数据库会话。」；有脏行追加「该标签有 N 行未提交的更改。关闭会丢失这些更改：可以选择先保存，或直接放弃。」；有事务追加「该标签有未提交事务（<标签>）：关闭将**回滚该标签的事务**，其它标签不受影响。」 | `src/tabs.rs:250、src/tabs.rs:2718` |

### `tabs.close_dialog.discard` 「放弃更改并关闭」/「回滚事务并关闭」

kind `item` · status `active` · parent `tabs.close_dialog`

- 无障碍：可访问名「放弃更改并关闭」
- 同步：Linux done（§4 放弃并关闭 / 关闭对话框按钮；answer_close 的 Discard 分支）；Windows none（§7.1 未记录）；macOS none
- 实现备注：有脏行时文案是「放弃更改并关闭」；只有事务没有脏行时是「回滚事务并关闭」（关闭 = 回滚，不用「放弃」吓人）。两个文案都是破坏性档（destructive），默认焦点不落在它们上。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 确认框在显示 | 丢弃本地草稿与新增行（有事务时随会话断开由后端回滚），再关页面：被拦下的那次关闭用 `close_page_finish(page, true)` 收尾 | `src/tabs.rs:2779、src/tabs.rs:2791、src/tabs.rs:2795、src/tabs.rs:2702` |

### `tabs.close_dialog.save` 「保存并关闭」/「保存、提交事务并关闭」

kind `item` · status `active` · parent `tabs.close_dialog`

- 无障碍：可访问名「保存并关闭」
- 同步：Linux done（§4 保存并关闭；advance_close 的两段相位与失败保留）；Windows none（§7.1 未记录）；macOS none
- 实现备注：有事务时文案写「保存、提交事务并关闭」：在事务里只提交脏行就断开，服务端会把事务回滚 —— 用户以为「保存了」，实际什么都没留下。顺序固定：先提交脏行 → 若标签有未提交事务再提交事务 → 最后才摘页面；任一步失败都保留标签并在页内说明原因。建议档（suggested），但不做默认响应（默认是取消）。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 确认框在显示且有脏行 | 先提交脏行 → 再提交标签自己的事务 → 最后摘页面；提交失败时标签保留，页内提示「保存失败：事务处于「<标签>」，标签未关闭。请处理事务后再关闭。」 | `src/tabs.rs:2797、src/tabs.rs:2348、src/tabs.rs:2390` |

### `tabs.close_dialog.cancel` 「取消」

kind `item` · status `active` · parent `tabs.close_dialog`

- 无障碍：可访问名「取消」
- 同步：Linux done（§4 取消；answer_close 的 Cancel 分支与 finish_close_page(false)）；Windows none（§7.1 未记录）；macOS none
- 实现备注：默认响应与窗口关闭响应都是它：误按 Esc / 点框外不会丢掉未提交的更改。取消同样要给被拦下的关闭收尾（`close_page_finish(page, false)`），否则页面停在「正在关闭」、后续关闭请求全部失效。

| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |
|---|---|---|---|
| click | 确认框在显示 | 标签与会话都保留，脏行与事务原样；这次被拦下的关闭请求收尾为「不关」（`close_page_finish(page, false)`） | `src/tabs.rs:2779、src/tabs.rs:2788、src/tabs.rs:2702` |
