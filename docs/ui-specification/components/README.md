# UI 组件注册表（`components/*.toml`）

本目录是 Sqarvo **界面组成的唯一真值**：页面上每个可独立命名、可独立交互、可独立验收的
一点，都在这里有一条记录。它回答四个问题：

1. 这个东西**叫什么、从属于谁**（`id` / `parent`）
2. 它**长什么样**（`style`：布局槽 + 尺寸 key + 主题 token，不写具体数字与色值）
3. 它**点了会发生什么**（`interaction`：触发、前置条件、结果、源码依据）
4. **三端各自做到哪一步**（`sync`：done / partial / none / na / blocked）

决策与理由见 [ADR 0027](../../decisions/0027-ui-component-registry.md)；迁移分阶段进行，
旧的 [ui-behavior-contract.md](../ui-behavior-contract.md) 在 P1 完成前仍是行为叙述的来源。

`ui-components.md` 由脚本生成，**不要手改**。

## 怎么用

```bash
scripts/check-ui-sync.sh check          # 结构 / 引用 / 哈希检查；有硬错误返回 1
scripts/check-ui-sync.sh check --strict # 额外把「同步标记过期」也算失败
scripts/check-ui-sync.sh gen            # 重新生成 docs/ui-specification/ui-components.md
scripts/check-ui-sync.sh mark --id object_tree.row --platform linux --evidence "--qa-layout-audit 通过"
```

`mark` 只做一件事：把这个组件在该端记下的 `spec_hash` 更新为**当前**规范哈希，意思是
「这一端已经按这一版规范核对过了」。它不改 `state` —— 做到什么程度仍要人写。

## 命名

- `id`：ASCII、小写、点号分层、**一旦发布不再改名**。正则
  `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$`。
- 层级必须与 `parent` 一致：`object_tree.tree.row.conn` 的 `parent` 是
  `object_tree.tree.row`（行模板挂在树容器下，行种类挂在行模板下）。
- 根是功能区（`area`），目前九个：`shell`（窗口壳、标题、主工具条、菜单栏、密度档、错误页）/
  `object_tree` / `objects_page` / `tabs`（标签条 + 查询标签的编辑器与结果区）/
  `data_page`（表数据页：工具条、筛选、网格、编辑、分页、导出、行右键）/
  `inspector` / `status_bar` / `dialogs`（连接对话框、关于）/ `startup`（启动态外壳、Loading）。
- `display_name` 是中文显示名，**可以随文案演进**，不参与稳定性承诺。
- **状态不进 id**：一条对象树行的默认 / 悬停 / 选中不是三个组件，是一个组件的三个
  `style.states`、或一条 `interaction` 的结果。

### 区域边界（避免同一个东西登记两次）

- 一个组件只属于一个区：`parent` 表达归属，`style.slot` 表达摆放位置。例：
  `object_tree.search` 归属对象树，但摆在 `status_bar.left_gutter`。
- 确认框 / 提示框跟随**触发它的动作**所属的区：关闭标签的确认框在 `tabs` 下，删除连接的
  确认框在 `object_tree` 下。`dialogs` 只收独立于某个动作、有自己生命周期的表面：新建 /
  编辑连接对话框、关于对话框。
- 跨区引用只出现在 `interaction.result` 的说明文字里，不允许把别的区的 id 用作 `parent`。

## 粒度（硬规则）

一个组件必须同时满足三条，缺一就下沉成父组件的状态或子字段：

1. 能独立命名（QA 能单独指认它）；
2. 有独立交互，或有自己的可见状态；
3. 三端可以各自判断「做到了没有」。

因此：容器与背景板没有交互，只记 `style`；列表行模板与它的行种类是父子组件，不重复记
样式；搜索结果行**不是**新组件（复用 `object_tree.row`，见 §2.4）。

## 字段

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` / `parent` / `display_name` / `kind` | 是 | `parent` 为空仅限区域根 |
| `status` | 是 | `active` / `deprecated` / `removed`，语义见「生命周期」一节 |
| `removed_in` | `deprecated` / `removed` 时必填 | 从哪个版本起不要它，形如 `v0.3.0` |
| `removed_reason` | `removed` 时必填 | 一句话：被合并 / 产品砍掉 / 不再提供 |
| `superseded_by` | 可选 | 有替代组件时指向它的 id（必须存在） |
| `style.slot` | 建议 | 布局槽，见下方清单 |
| `style.metrics` | 建议 | `metrics.toml` 的 key 名（**不写数字**） |
| `style.tokens` | 建议 | 主题 token 名（**不写 `#RRGGBB`**），必须能在 `frontend/linux/src/theme.rs` 找到 `@<name>` |
| `style.states` | 可选 | 键限 `hover` / `active` / `selected` / `focus` / `disabled`，值是 token 名 |
| `a11y.name` / `a11y.tab` | P1 补 | 可访问名与 Tab 次序；缺了报警告，不失败。`（行内容：…）` 表示名字由行内容生成（列表行没有静态名） |
| `native_control` | 选型有意义时 | `gtk:<控件> / winui:<控件> / swift:<控件>`；未实现写 `—`，未核对写 `待核`。纯文本、行模板不必填 |
| `hand_drawn` | 可选 | `true` = 视觉是自绘的（不是原生控件渲染），此时必须给 `why_not_native` |
| `why_not_native` | `hand_drawn=true` 时 | 为什么没有用原生控件；也可以用在 `hand_drawn` 之外，解释「为什么没用最直觉的那个原生控件」 |
| `interaction[]` | 有交互时必填 | `trigger` / `precondition` / `result` / `source`，`key` 触发必带 `keys` |
| `impl_notes[]` | 可选 | 某端的实现细节（不参与哈希、不是规范） |
| `platform_notes.<plat>` | 可选 | 平台差异与限制（不参与哈希） |
| `sync.<plat>` | 是（三端各一条） | 见下 |

`kind`：`area` / `container` / `control` / `row` / `cell` / `item` / `slot`。

`trigger`：`click` / `double_click` / `right_click` / `hover` / `focus` / `key` / `drag` /
`scroll` / `load`。

`style.slot` 目前可用（P0 暂定，workspace-model 发布正式槽位 id 后以它为准）：
`header`、`menubar`、`toolbar`、`sidebar`、`workspace.content`、`workspace.tabstrip`、
`inspector`、`status_bar.left_gutter`、`status_bar.row1`、`status_bar.row2`、`dialogs`、
`startup`、`overlay`。

**样式只放引用，不放数字和色值。** `metrics.toml` 是尺寸的唯一真值，主题 token 是颜色的
唯一真值（AGENTS.md §8.6）。组件条目里出现 `200`、`#A66FA0` 一律视为缺陷 —— 三端取整
规则不同，第二份数字必然漂 1px；alpha、1px 描边这类细节写进 `impl_notes`。

## 原生控件优先（L0–L3）

新界面一律先找系统原生控件（见 AGENTS.md §8.1）。四档：

| 档 | 含义 | 处理 |
|---|---|---|
| L0 | 原生控件，默认样式 | 正常 |
| L1 | 原生控件 + 主题定制（样式落在原生控件与它的行 / 格上） | 正常，仍算原生用法 |
| L2 | 自绘视觉，或把原生控件自组装成工具包没有的复合件 | 允许；必须在组件里登记 `hand_drawn` / `why_not_native`，**登记即可，不另开 ADR** |
| L3 | 有原生等价物却自己实现（自绘菜单、对话框、表格、标签条） | 禁止；确需例外先写 ADR |

- 三端只承诺**行为**一致（条目、顺序、可用性、快捷键、文案）；分隔线、图标、字号从俗。
- 原生控件名记在 `native_control`，没实现的端写 `—`，没核对过的写 `待核` —— 不要拿猜测填空。

## 生命周期：active / deprecated / removed

「这版不要它了」是产品决定，不是运行时状态。三值：

| status | 含义 | 必填 |
|---|---|---|
| `active` | 在用 | — |
| `deprecated` | 这版还在用，下版会被取代或移除 | `removed_in`（有替代时加 `superseded_by`） |
| `removed` | 这版起产品不需要 | `removed_in` + `removed_reason` |

- **removed 的条目不许直接删。** 删了条目，别的端就失去「要去掉什么」的对照物，正好违背这条设计
  的目的。三端都清干净后，才把它归档到 `components/_archived.toml`（保留可查：上一版删过什么）。
- 退役组件（`deprecated` / `removed`）的 `sync` 只允许三种值：`removed`（该端已按退役处理：
  代码删了，或界面隐藏了）/ `present`（还留着，待清理）/ `na`（该端本来就没有）。**这三种不记
  `spec_hash`** —— 退役没有「按规范实现」这回事，只有一个清理进度。
- **单端不做 ≠ 全局退役**：macOS 不做密度三档是 `sync.macos = "na"`，不是 `status = "removed"`。
- 动态查询：`scripts/check-ui-sync.sh pending` 列出所有退役组件与各端清理进度，
  `--release v0.3.0` 只看某一版。

## 两个「不可用」不要混

| 概念 | 写法 | 语义 |
|---|---|---|
| 产品不想要它了 | `status = "removed"` + `removed_in` | 代码删，或界面暂时隐藏；条目保留到三端清零 |
| 控件在、当前不可用 | `style.states.disabled` / 交互里的前置条件 | 可见 + 置灰 + 原因（见下节） |

## 禁用项：能力性 vs 状态性

- **能力性不可用**（这条连接 / 这个驱动 / 这一端根本不支持，近期也不会支持）：**不出现**。
  菜单项不列出、按钮不渲染，不要留一个永远点不动的灰控件。
- **状态性不可用**（当前上下文暂不满足：未连接、没选中、没脏行、只读、RPC 在途）：**可见 +
  置灰 + 给原因**（tooltip / 状态行），因为用户下一步就能让它可用。
- 既有例外：对象树里未实现的分组仍显示一行说明（行为契约 §2.1 的「不静默」决定）。要改这条，
  先改 §2.1 与 `object_tree.tree.row.group`。

## 同步（`sync`）

```toml
[component.sync.linux]
state = "done"            # done | partial | none | na | blocked
spec_hash = "9f2c1a0b3c4d" # 该端核对时对应的规范哈希；doing/partial 必填
commit = "7436cd5"         # 核对时的提交（可空）
evidence = "--qa-layout-audit 通过"  # 证据（可空，done 时缺失只报警告）
```

- `done`：样式 + 交互都按本规范实现。**必须带当前 `spec_hash`**，否则 `check` 判过期。
- `partial`：做了一部分。差异写 `platform_notes.<plat>`，别写「未同步」了事。
- `none`：完全没做。
- `na`：产品上就不做（如 macOS 不做界面密度三档），须给 `note`。
- `blocked`：平台能力做不到（如 GTK4 没有读写 toplevel 位置的 API），须给 `note` 并进
  行为契约 §9 已知偏差。

**过期是派生的，不靠人记得清理**：哈希覆盖组件除 `sync` / `impl_notes` /
`platform_notes` 之外的全部内容。任何人改了规范，三端标记自动失效，`check` 会列出来；
谁要恢复 `done`，就得按新规范核对一遍再 `mark`。这样「改了就清标记」不需要纪律。

`--strict` 用于 CI 之后收紧：结构错误始终失败，过期在 P1 起可设为失败。