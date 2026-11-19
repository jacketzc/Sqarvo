# docs/AGENTS.md — 文档、注册表与门禁细则

作用域：`docs/**`、`README.md` / `README.zh-CN.md` 成对规则、ADR 约定、UI 组件注册表与其
生成物、任务跟踪约定。根 `AGENTS.md` §14 是绑定摘要；本文件是全文权威。文档同步的
**影响范围矩阵以根 §14 为准**，此处不重复。

## 14.1 文档分层与唯一真值

| 层 | 位置 | 归属 | 变更频率 |
|---|---|---|---|
| 产品规划（愿景、定位、需求、路线图） | `docs/product/` | 产品负责人 | 低 |
| 开发规划（计划、里程碑、发布、风险） | `docs/planning/` | 项目管理角色 | 中 |
| 技术事实（架构、API、Driver SPI、UI） | `docs/architecture/`、`docs/api/`、`docs/database/`、`docs/ui-specification/` | 架构/模块 owner | 随代码 |
| UI 组件注册表（存在性、样式引用、三端同步） | `docs/ui-specification/components/` | UI owner | 每次改组件 |
| 决策 | `docs/decisions/` | 决策作者 | 追加式 |
| 任务、状态、依赖 | GitHub Issues / Projects | 开发 agent | 高 |

- 仓库内放稳定规划，动态执行状态放 GitHub Issues/Projects；不在仓库内维护第二份任务清单，不把计划文档复制到外部工具（Notion、wiki）。一份计划，一个地方。
- 任务 Issue 必须关联模块、里程碑、优先级、需求 ID、依赖与验收标准；PR 关联 Issue 与里程碑。`scripts/check-tracking.sh`（CI：`.github/workflows/tracking.yml`）校验里程碑与 Project 成员，缺优先级/模块标签报 warning。
- 开发 agent 更新 Issue 与 PR，不擅自改写产品需求、里程碑、开发计划，除非被明确指派该角色。
- 里程碑只有附验证证据记录（命令与结果）才可标完成。
- **ADR**：架构边界、驱动增删、协议、凭据存储、IPC、安全策略、平台基线/打包/依赖变更，以及方案取舍需要留痕的决定，必须写 ADR（context、备选方案、决定、理由、权衡、重新评估条件）。ADR 追加式：新决策新开文件，被取代的 ADR 标 superseded 而非删改。
- **README 成对**：`README.md`（英文）与 `README.zh-CN.md`（中文）是一个交付物，必须同一提交变更；保持结构一致（章节顺序、列表数、链接集）与顶部语言切换行；措辞按语言本地化，但事实（命令、平台支持、实测数字、链接）绝不分歧。`scripts/check-readme-pair.py`（CI：readme.yml）校验切换链接、相对链接集合与结构计数；文案与事实仍需人工复核。
- 用户可见功能要加用户文档；每个新配置项要记录默认值、持久化位置与安全含义。

## 14.2 UI 行为来源（分工现状：已按 ADR 0025 / 0027 迁移完成）

- 逐组件行为（动作、可用性条件、结果文案、快捷键、源码依据、三端同步状态）唯一真值 = 注册表 `components/*.toml`。
- `ui-behavior-contract.md` 只保留**全局规则**（§8：快捷键、危险操作确认、五态、失败文案硬约束）与**已知偏差**（§9）；其 §1–§7 是各功能区指向注册表的指针，不再承载行为叙述。
- `workspace-model.md` 只保留区域划分与布局算术；视觉规格（尺寸、间距、颜色）在 `metrics.toml` 与主题 token，文档只引用 token 名不重述数字。
- 冲突处理：单组件行为按注册表；全局规则按契约 §8；版式按 workspace-model；三者都不许与代码矛盾。代码与注册表不一致 → 记契约 §9.1「待决策」（代码如此 / 文档如此 / 建议），不许静默、也不许由实现者单独敲定产品语义。
- 改全局规则（快捷键、危险确认、状态语义、错误文案）→ 改契约 §8 并更新所有受影响组件；改单组件行为 → 只改该组件条目 + `mark`。

## 14.3 组件注册表（绑定规则全文）

- **改 UI 之前先报组件名并请维护者确认范围**——这是产品口径门（哪个东西、叫什么名），不是逐文件审批。授权任务已明确点名组件或界面区域时，视为范围已确认，直接执行；范围含糊时先在 Issue/对话里问。
- 改动落在未登记功能区（若有）：先说明「该区还没进注册表」、将来归属哪区哪一级，再动手；不临时自造 `id`。
- 新增或改动用户可见组件，必须在**同一 PR** 改注册表。`id` 发布后不改名；`display_name` 可随文案演进。
- 样式只写 `metrics.toml` 的 key 名与主题 token 名；条目出现 `px` 数字或 `#RRGGBB` 一律算缺陷（同根 §8.6：三端取整不同，第二份数字必漂）。
- `sync.<plat>.state = "done"` 必须带当前 `spec_hash`。规范改动使三端标记自动失效；恢复 `done` 须按新规范重新核对，再跑 `scripts/check-ui-sync.sh mark`。`partial` / `na` / `blocked` 必须写清具体差异，不接受「未同步」四字。
- `sync.<plat>.evidence` 只写**交互与功能**证据：命令、断言、QA 开关输出、可访问性名称/控件文本读取结果。截图、录屏、像素比较不算验收证据（ADR 0029）；历史条目已引用的截图不回改，新登记不再以截图为准。
- `ui-components.md` 是生成物：禁止手改；CI 跑 `gen` 并与工作树比对，有差异即失败。
- **生命周期代替删除**：「这版不要了」→ `status = "removed"` + `removed_in`（如 `v0.3.0`）+ `removed_reason`；在用但将被取代/移除 → `deprecated`。不许直接删条目——其他端要靠它知道「要去掉什么」；三端清干净后才归档到 `components/_archived.toml`。退役组件 `sync` 只记 `removed`（该端已清）/ `present`（还留着）/ `na`，不记 `spec_hash`；单端不做记 `na`，那不算全局退役。查询用 `scripts/check-ui-sync.sh pending`。
- 未实现的平台能力记 `sync = "blocked"` 并进契约 §9，不得只标「未同步」。
- 字段与粒度规范见 `docs/ui-specification/components/README.md`。

## 14.4 发版门禁：已支持平台的 UI 行为必须同步（绑定）

每次在 GitHub 发布 Release（含预发布）前，注册表里**所有 `active` / `deprecated` 组件**在当前支持平台上的 `sync` 必须是 `done`，或 `na` / `blocked`（并有说明）。

- **当前支持平台：Linux、Windows。** macOS 尚未接入，`sync.macos` 不参与门禁；接入时把它加进 `scripts/check-ui-sync.sh` 的 `SUPPORTED_PLATFORMS`，并同步根 §14.4、本节与 §14.1 分层表。
- 门禁命令：`scripts/check-ui-sync.sh release-check [--release v0.3.0]`。有缺口返回非 0，并逐条列出「哪个组件、哪个平台、什么状态」；带 `--release` 时同时检查该版本要删的组件是否已清干净。
- 算缺口：`partial` / `none` / `present` / 标记过期（规范改了没重新核对）。不算缺口：`na`（该平台不做）、`blocked`（平台能力做不到且已登记契约 §9）。
- 确实要带已知差异发版：在发布说明（`docs/releases/<version>.md`）单列「未同步的 UI 行为」一节，写明组件 `id`、平台、原因、计划版本，并在对应 Issue 取得维护者明确批准；不允许静默发布。
- 与 14.3 分工：14.3 管日常（改了规范就要重新核对），本节管发版（发版前不允许留缺口）。
- 任何「重构/提效/授权自主执行」类任务都不构成绕过本门禁的理由；门禁脚本（`check-ui-sync.sh`、`check-tracking.sh`、`check-readme-pair.py`、`xaml_lint.py`）与对应 workflow 不得因文档结构调整而被停用或放宽。
