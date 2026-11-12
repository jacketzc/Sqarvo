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
- 根是功能区（`area`），目前九个：`shell` / `object_tree` / `objects_page` / `tabs` /
  `data_page` / `inspector` / `status_bar` / `dialogs` / `startup`。
- `display_name` 是中文显示名，**可以随文案演进**，不参与稳定性承诺。
- **状态不进 id**：一条对象树行的默认 / 悬停 / 选中不是三个组件，是一个组件的三个
  `style.states`、或一条 `interaction` 的结果。

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
| `status` | 是 | `active` / `deprecated` / `removed` |
| `superseded_by` | `deprecated` 时必填 | 指向替代它的组件 id |
| `style.slot` | 建议 | 布局槽，见下方清单 |
| `style.metrics` | 建议 | `metrics.toml` 的 key 名（**不写数字**） |
| `style.tokens` | 建议 | 主题 token 名（**不写 `#RRGGBB`**），必须能在 `frontend/linux/src/theme.rs` 找到 `@<name>` |
| `style.states` | 可选 | 键限 `hover` / `active` / `selected` / `focus` / `disabled`，值是 token 名 |
| `a11y.name` / `a11y.tab` | P1 补 | 可访问名与 Tab 次序；缺了报警告，不失败。`（行内容：…）` 表示名字由行内容生成（列表行没有静态名） |
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