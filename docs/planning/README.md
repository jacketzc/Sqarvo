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
│   └── roadmap.md           # 版本目标（v0.x → v1.0）
├── planning/
│   ├── README.md            # 本文件：分工与流程
│   ├── development-plan.md  # 技术实施顺序、阶段交付与验收
│   ├── milestones.md        # 里程碑（稳定，不随小任务改动）
│   ├── release-plan.md      # 版本范围、退出标准、发布检查
│   └── risk-register.md     # 风险登记与缓解
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
- 所属里程碑（M0–M6，见 `milestones.md`）；
- 优先级（P0/P1/P2，与 `requirements.md` 对齐）；
- 前置依赖（Issue 编号）；
- 验收标准（可执行、可观察）；
- 关联 PR。

模板见 `.github/ISSUE_TEMPLATE/task.yml`。Project 建议视图：Roadmap、Board
（待办/进行中/审核中/完成）、Table（含依赖字段）、Milestones。

## 里程碑与版本

- 里程碑 M0–M6 是开发阶段的稳定锚点，见 `milestones.md`；
- 产品版本 v0.1–v1.0 是交付目标，见 `release-plan.md`；
- 单个任务延期不修改里程碑标准；只有范围或技术前提变化才改。

## 文档更新责任

- 修改架构边界、驱动范围、gRPC 契约、安全策略、平台支持时，必须同步更新对应文档；
- 阶段完成时更新 `milestones.md` 的「实际完成」列，并附验证命令与结果；
- 新增技术决策写入 `docs/decisions/`（ADR）。
