# 规划文档与任务管理

## 三种内容，三个位置

| 类型 | 位置 | 作用 | 变更频率 |
|---|---|---|---|
| 产品规划（愿景、定位、需求、路线图） | 本仓库 `docs/product/` | 定义做什么、为什么做 | 低 |
| 开发计划（实施阶段、里程碑、发布、风险） | 本仓库 `docs/planning/` | 定义怎么做、先后顺序、验收标准 | 中 |
| 具体任务与进度 | GitHub Issues + Projects | 谁在做什么、当前状态、依赖 | 高 |

原则：**仓库保存长期有效的规划，项目管理工具保存动态执行状态。**
禁止在仓库与外部工具（Notion、文档平台）各维护一份计划。

## 目录

```text
docs/
├── product/
│   ├── vision.md            # 愿景与范围
│   ├── positioning.md       # 定位、竞品差异、非目标
│   ├── requirements.md      # 功能清单、优先级、状态
│   └── roadmap.md           # 版本目标（v0.1 → v0.2.x）
├── planning/
│   ├── README.md            # 本文件：分工与流程
│   ├── development-plan.md  # 技术实施顺序、阶段交付与验收
│   ├── milestones.md        # 里程碑（稳定，不随小任务改动）
│   ├── release-plan.md      # 版本范围、退出标准、发布检查
│   └── risk-register.md     # 风险登记与缓解
├── releases/                # 版本发布说明（能力、限制、验证记录）
├── architecture/            # 进程模型、模块边界、数据流
├── api/                     # gRPC / Protobuf 约定
├── database/                # Driver SPI 与数据库差异
├── ui-specification/        # 交互与设计原则
└── decisions/               # ADR
```

## 角色分工（人类与 Agent 相同）

| 层级 | 角色 | 维护内容 | 不应做 |
|---|---|---|---|
| L1 产品规划 | Product Owner / 规划 Agent | `docs/product/*`、ADR 的产品侧结论 | 直接改任务状态 |
| L2 项目管理 | Project Manager Agent | `docs/planning/*`、GitHub Milestones/Projects、依赖与优先级 | 改写产品需求 |
| L3 开发执行 | Development Agents | 代码、测试、Issue 状态、PR | 擅自修改规划文档与里程碑 |

进度反馈路径：开发 Agent 完成任务 → 更新 Issue 与 PR → PM 汇总进度与依赖变化 →
Product Owner 决定是否调整规划。避免多个 Agent 各自修改开发计划导致规划与实现脱节。

## 任务规范

每个任务对应一个 GitHub Issue，必须包含：

- 所属模块（`backend/crates/*`、`frontend/*`、`proto/*`、`docs/*`）；
- 所属里程碑（M0–M7，见 `milestones.md`）；
- 优先级（P0/P1/P2，与 `requirements.md` 对齐）；
- 前置依赖（Issue 编号）；
- 验收标准（可执行、可观察）；
- 关联 PR。

模板见 `.github/ISSUE_TEMPLATE/task.yml`（自动打 `task` 标签）。依赖关系落在三处：
Issue 正文写明编号、GitHub 原生 blocked-by 关系、Project「依赖」字段。

当前看板：<https://github.com/users/jacketzc/projects/2>（里程碑 M0–M7 均已建立，
Issue 通过原生 Milestone 字段挂载）。

| 项 | 类型 | 说明 |
|---|---|---|
| Status | 单选项 | Todo / In Progress / Done；Issue 关闭即视为完成 |
| 依赖 | 文本 | 前置 Issue 编号，如 `#1、#5`（表格视图列） |
| 看板 | 视图 | 按 Status 分列，用于推进 |
| 任务表 | 视图 | 列含 Status、Milestone、Labels、依赖、Linked pull requests |

路线图（Roadmap）视图需要 Project 日期字段并有值才会渲染，当前没有排期数据，
因此暂不建立；确定各里程碑日期后再加，避免留一个空视图。

## 自动检查

`scripts/check-tracking.sh` 把上述约定变成可执行的检查：

- 硬失败：Issue 缺里程碑；里程碑名不在 `milestones.md`；Issue 未加入 Project 看板；
- 警告：缺优先级标签（P0/P1/P2）、缺模块标签。

```bash
./scripts/check-tracking.sh              # 检查 open Issue
./scripts/check-tracking.sh --state all  # 全量核对历史 Issue
```

CI 由 `.github/workflows/tracking.yml` 在每日定时、手动触发、Issue 打开/编辑/重开时运行。
看板为公开时 `GITHUB_TOKEN` 即可读取；若改为私有，需在 Secrets 配置 `PROJECT_TOKEN`
（fine-grained PAT，Projects: read），否则看板检查降级为警告。

## 里程碑与版本

- 里程碑 M0–M7 是开发阶段的稳定锚点，见 `milestones.md`；
- 产品版本 v0.1–v0.2.x 是交付目标，见 `release-plan.md`；
- 单个任务延期不修改里程碑标准；只有范围或技术前提变化才改。

## 文档更新责任

- 修改架构边界、驱动范围、gRPC 契约、安全策略、平台支持时，必须同步更新对应文档；
- 阶段完成时更新 `milestones.md` 的「实际完成」列，并附验证命令与结果；
- 新增技术决策写入 `docs/decisions/`（ADR）。
