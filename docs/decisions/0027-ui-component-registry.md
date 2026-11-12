# ADR 0027：UI 组件注册表（组件为单位的跨端契约）

- 状态：**已接受（P0 试点）**
- 日期：2026-11-12
- 关联：ADR 0023（区域一致与原生控件）、ADR 0025（UI 行为契约的单一来源，**本 ADR 取代其未评审方案**）、
  ADR 0026（缩放与窗口尺寸）；AGENTS.md §8.6（尺寸与颜色真值）、§14.1（文档单一来源）、§14.2（行为契约）
- 上游：[ui-behavior-contract.md](../ui-specification/ui-behavior-contract.md)、
  [workspace-model.md](../ui-specification/workspace-model.md)、
  [metrics.toml](../ui-specification/metrics.toml)

## 背景

三端一致目前靠一份按功能章节写的行为契约（657 行）+ 一份结构规范 + 一段自由文字的
Windows 现状（契约 §7.1）。它能回答「点了会发生什么」，但回答不了三个三端开发每天都要问的问题：

1. **这个东西叫什么？** 契约里的「对象树行」「分组行」没有稳定标识，代码注释、QA 断言、
   Issue、PR 之间只能靠中文名词互相对齐，改个措辞就断链。
2. **哪一端做到哪一步？** 只有 §7.1 一段叙述。已知的真实差异散在正文与 §9 里：Windows 元数据
   每页 200 条（Linux 500）、Windows 表节点是叶子（Linux 有 Fields / Indexes / Foreign Keys /
   Triggers 四组）、Windows 读取失败行点按即重试（Linux 没有这条路径）、GTK4 没有读写 toplevel
   位置的 API。没有一份可查询的矩阵，这些差异只能靠人读完全文才知道。
3. **改了会牵动谁？** 契约只承诺「改了要同步改代码」，但改动之后另外两端是否还对齐，没有载体。

另外，P0 实施时实测到一件具体的事：契约 §2.3 记右键菜单位于 `src/tree.rs:2702`，而
`menu_items` 现在在 `src/tree.rs:3191` —— **行号已经漂了近 500 行**。这不是文档纪律问题，
是文档结构与代码结构的耦合方式问题：行号进了正文，就必须有人持续修。

ADR 0025 曾提出「动作目录 + 状态机 + 回放 fixture」，其动作目录的意图与本 ADR 相同，但它
停留在讨论稿，且只覆盖动作、不覆盖「组件存在性与样式归属」。本 ADR 取代它：动作不是独立
实体，是组件的 `interaction`。

## 备选方案

1. **继续手写单一 Markdown，按组件分节。** 300–500 个组件 × 三端矩阵 × 逐态样式，会是
   3000+ 行，且矩阵无法被机器校验。放弃。
2. **组件注册表（TOML）+ 生成可读文档 + 派生式同步标记**（采用）。数据可校验、可查询、
   可生成多种视图；人读的产物由脚本生成，不做手改。
3. **把契约做成代码里的属性 / 注解，文档由代码生成。** 三端都要解析各自的源文件，GTK 的
   控件树与 WinUI 的 XAML 结构差异太大，抽取规则会变成维护负担；且「规范」会被实现反推，
   失去「先定规范再实现」的意义。放弃。
4. **只做三端同步矩阵、不改组件化。** 矩阵的每一行仍然要靠中文名指认，无法被脚本定位到
   代码，等于把 §7.1 放大十倍。放弃。

## 决策

采用方案 2。三层职责，互不复制：

| 层 | 位置 | 管什么 |
|---|---|---|
| 组件注册表 | `docs/ui-specification/components/<area>.toml` | 组件存在性、层级、样式引用、交互、废弃、三端同步 —— **唯一真值** |
| 可读产物 | `docs/ui-specification/ui-components.md` | 由注册表生成的总览与明细，供人读与 PR review，**不得手改** |
| 校验器 | `scripts/check-ui-sync.sh`（`check` / `gen` / `mark`） | 结构、引用、哈希一致性；CI 阻断 |

### 1. 命名

- `id`：ASCII、小写、点号分层、发布后不改名。`display_name` 是中文显示名，可随文案演进。
- 层级必须与 `parent` 一致，脚本强制：`object_tree.tree.row.conn` 的父是
  `object_tree.tree.row`。这样 `object_tree.` 前缀一 grep 就是一棵子树。
- **状态不进 id**：一行的默认 / 悬停 / 选中是一个组件 × `style.states`，不是三个组件。
- 根是九个功能区：`shell` / `object_tree` / `objects_page` / `tabs` / `data_page` /
  `inspector` / `status_bar` / `dialogs` / `startup`。

### 2. 属性

`id`、`parent`、`display_name`、`kind`、`status`、`superseded_by`、`style`（`slot` /
`metrics` / `tokens` / `states`）、`a11y`、`interaction[]`、`impl_notes`、
`platform_notes`、`sync`。字段定义与粒度硬规则见
[components/README.md](../ui-specification/components/README.md)。

两条不可妥协的约束：

- **样式只放引用。** 尺寸只写 `metrics.toml` 的 key，颜色只写主题 token；组件里出现 px 或
  `#RRGGBB` 一律算缺陷（AGENTS.md §8.6）。三端取整规则不同，任何第二份数字都会静默漂 1px；
  alpha、1px 描边这类细节写 `impl_notes`，不进规范哈希。
- **粒度三条件**：能独立命名、有独立交互或独立可见状态、三端能各自判断「做到没有」。
  缺一就下沉成父组件的状态或子字段。容器与背景板只记 `style`，不编造交互。

`status` 是**生命周期**，不是运行时可用性：`removed`（这版不要了）必须带 `removed_in` +
`removed_reason`，条目**不删** —— 别的端要靠它知道「要去掉什么」；该组件的 `sync` 改用
`removed`（已清）/ `present`（还留着）/ `na` 表达清理进度、不记哈希，三端清零后才归档到
`components/_archived.toml`。运行时灰掉是另一回事（`style.states.disabled` + 前置条件）。

### 3. 同步：派生而非纪律

每个组件的规范内容（除 `sync` / `impl_notes` / `platform_notes`）算一个 12 位哈希，
存在 `sync.<plat>.spec_hash`；该端「按这一版规范核对过」才写哈希。

- 任何人改了规范 → 哈希变 → **三端标记自动过期**，不需要谁记得去清。`check` 会列出来。
- 要恢复 `done`，必须按新规范核对一遍，再 `mark` 写入当前哈希（可附 `commit` / `evidence`）。
- 状态五值：`done` / `partial` / `none` / `na`（产品不做）/ `blocked`（平台能力做不到，
  落契约 §9）。`partial` / `na` / `blocked` 必须给说明，不接受「未同步」四个字了事。
- CI 硬失败只针对结构错误（id 重复、parent 断链、metrics / token 不存在、源码引用不存在、
  `state=done` 却没有哈希）。「某端未同步」不阻断合并 —— 否则只会逼人乱点 `done`。

## 理由与权衡

- **收益 1：偏差第一次可查询。** P0 试点一个区就把四条 Windows 差异从散文变成了
  `sync.windows = partial` + `note`：页大小 200 vs 500、表节点是叶子、失败行可点按重试、
  §7.1 未记录对象树的右键菜单与搜索入口（待核）。这些以前要读完整节才拼得出来。
- **收益 2：改动影响面可算。** 改一个组件的规范，`check` 直接列出哪几端过期。
- **代价 1：组件数量与维护税。** 全量约 300–500 个组件。初版是人力成本，长期是「每次改动
  ×3 端核对」的税。缓解：只有跨端语义进哈希，各端实现细节进 `impl_notes`；`--strict` 在
  P1 前不用于 CI。
- **代价 2：`done` 可能被滥用。** 缓解：`done` 要求当前哈希 + 证据；`mark` 只改哈希、不改
  `state`，人必须显式写状态。
- **代价 3：双份真值过渡期。** P0–P1 期间行为叙述仍在旧契约里。过渡规则写进 AGENTS.md
  §14.3：改行为要同时改旧契约本节与注册表对应组件，P1 结束后删除旧契约章节。
- **不做**：逐控件像素对齐（ADR 0023 已定「按语义不按像素」）；把 `a11y` 与文案 key 一次
  性补齐（P1 起，缺了只报警告）。

## 迁移

| 阶段 | 内容 | 完成判据 |
|---|---|---|
| P0（本次） | 规范 + 校验器 + CI + `object_tree` 一个区试点 | `check` 通过；生成物与注册表一致；旧契约未改动（只加一行指针） |
| P1 | 按功能区迁完 Linux 已实现的全部；旧契约章节退役为指针；`a11y` / 文案 key 补齐 | 旧契约只剩 §8 全局规则与 §9 偏差；注册表覆盖全部用户可达组件 |
| P2 | 填实三端矩阵：Windows 逐块核对 §7.1（差异进 `note`），macOS 记 `none` | 每个组件的三端状态都有依据，不再有「不知道」 |
| P3 | CI 收紧（`--strict`）、PR 模板加勾选项、旧契约删除 | 新组件不进注册表无法合并 |

**进度（2026-11-12）**：P0 完成。P1 **完成**——九个功能区、186 个组件；旧契约 §1–§7 与
`workspace-model.md` §3–§7 已退役为指针，行为事实先逐条补进注册表（fan-out 9 个 agent，
共 325 行契约条目复核、补了 60 余条 interaction / impl_notes）。P2 部分完成：Windows 按 §7.1
逐块核对并写成 `partial` / `done` / `none`；缺口是实现缺口（166 条），不是文档缺口。P3 未做
（`--strict`、PR 模板、CI 收紧）。发版门禁见 AGENTS.md §14.4。

## 复核条件

出现以下任一情况时重审本 ADR：

1. 组件总数超过 600，或每次改动实际要动的组件数中位数 > 8 ——说明粒度太细；
2. 连续两个里程碑出现「为过 CI 而乱标 `done`」——说明约束位置错了，应改为只对新增组件强制；
3. Windows / macOS 因控件模型差异导致 `blocked` 占比超过 20%——说明契约在描述 GTK 而不是
   描述产品，需要把该部分降为 `platform_notes` 而非规范；
4. 有人能证明注册表与生成物长期不一致（`gen` 后 `git diff` 非空）超过两次。