# AGENTS.md

本文件是 Agent 工作入口，只保留**对所有任务都适用**的全局约束。模块细则在各目录的
AGENTS.md（就近生效；跨模块任务要读全相关模块的规范，不要假定任何 Markdown 被自动加载——
读过才算存在）。模块文件**镜像本文件章节号**（如 `backend/AGENTS.md` §6 即根 §6 的细则），
外部文档按根章节号引用。设计细节的权威在 `docs/`，本文件不复制细则。

## 1. 项目定位

- 开源、轻量、跨平台的原生桌面数据库管理客户端：Windows WinUI 3/C#，Linux GTK4 + libadwaita/Rust，macOS SwiftUI/Swift。
- 数据库能力集中在独立 Rust 后端伴生进程；前后端 gRPC + Protobuf；驱动经 Driver SPI 接入。
- **任何平台都不使用 Electron / Chromium / CEF / WebView2 作为 UI runtime**，不以 HTML/CSS/JS 仿原生界面。产品定位红线，无例外。
- 可参考 Navicat 等成熟客户端的用户流程，但必须独立设计：不复制专有代码、素材、图标、文案或未公开实现细节，不宣称与其有关联。

## 2. 技术栈与部署

- 后端：Rust + Tokio，独立伴生进程；换运行时须 ADR 说明理由与边界。gRPC + Protobuf 契约见 §7。
- 本地通信默认同机前后端两进程。访问控制（硬要求）：Linux/macOS 优先 Unix Domain Socket + 严格文件权限；Windows 优先命名管道 + ACL，若当前 gRPC 栈不能可靠支持，可临时用仅绑 `127.0.0.1` 的 TCP + 随机会话令牌。
- **绝不**默认监听 `0.0.0.0` / 公网 / 局域网地址；**绝不**暴露无认证管理接口。传输藏在抽象之后，业务层不依赖具体 socket/pipe/port 实现。
- 远程后端、团队协作、TLS 与身份校验是后续扩展；第一阶段不为未规划的远程形态过度设计。CI 与门禁见 `.github/workflows/`（ci、ui-contract、tracking、readme、packaging）。

## 3. 产品与体验红线

- 长列表与大结果集必须虚拟化 / 分页 / 增量拉取，不默认全量加载；前端主线程不做数据库请求与大文件处理。
- UI 必须能明确表达：加载中、空数据、错误、取消、只读、事务状态、断开。
- 可能破坏数据或不可逆的操作按风险给出相称的影响范围提示；不给普通查询加无意义确认弹窗。
- 不清除需求、测试、错误处理就不实现占位功能；未实现能力必须明确标注不可用（呈现规则见 §8.1）。
- 每个发布版本的范围由 `docs/product/` 的需求与路线图决定；能力开发顺序见 §5 文档索引。

## 4. 架构边界（细则见各模块 AGENTS.md §4；冲突时以本节红线为准并按缺陷修正）

- 前端不得：实现数据库网络协议或直连用户数据库、跨端复制驱动/连接池/SQL 逻辑、绕过 gRPC 访问后端内部、在 UI 线程做 IO、把密码写入配置/日志/崩溃报告/遥测。
- 后端不得：包含平台 UI 逻辑，依赖 WinUI/GTK/SwiftUI/AppKit。
- Protobuf 是通信契约唯一真值；生成代码不手改。前端可预校验输入，后端必须再校验权限、参数范围、资源状态与操作合法性。
- 不把 Rust 内部结构、Trait 对象或第三方驱动类型直接暴露进跨进程协议；数据库访问一律经 Driver SPI（§6）。

## 5. 模块规范地图

| 工作范围 | 必读入口 | 承载细则 |
|---|---|---|
| `backend/`（apps/backend-service、apps/sqarvo-mcp、crates/*） | `backend/AGENTS.md` | §6 Driver SPI、§7.3/7.4 实现侧、§10、§11、驱动测试 |
| `frontend/`（windows、linux、macos） | `frontend/AGENTS.md` | §8 全部、§12、UI 验收与注册表前端侧 |
| `l10n/` 共享文案目录 | `l10n/AGENTS.md`（指针）→ `frontend/AGENTS.md` §8.6、`l10n/README.md` | 文案条目与生成链 |
| `proto/` | `proto/AGENTS.md` | §7 契约设计、兼容演进、错误语义、生成管理 |
| `docs/`（专题规范、ADR、UI 注册表、发布说明） | `docs/AGENTS.md` | §14 全文（分层、注册表细则、发版门禁、README 成对） |
| `tests/`、`scripts/`、`packaging/` | 本文件 §13、§14 + 相关模块文件（UI 功能用例先读 `tests/ui/README.md`） | 契约/集成/性能/安全测试；UI 功能用例；门禁脚本 |

**作用域与阅读策略（重要）**：指令发现按「就近目录」生效——工具**不会**因为本表存在映射，
就把其他目录的 AGENTS.md 自动应用到当前文件。本表是**人工阅读清单**：

- 任务开始时：按预期影响范围对照本表只读命中的模块文件，不必默认读全部模块规范。
- 阅读触发：`backend/**` → backend 文件；`frontend/**` → frontend 文件；`.proto` 或其生成物
  → proto 文件（契约改动几乎总是跨模块，同时读受影响消费方）；用户可见组件行为 → 另读
  `docs/AGENTS.md` §14.3；`l10n/**` → `l10n/AGENTS.md` 指针。**根 `README*.md`、`docs/**`、
  `scripts/` 门禁脚本没有更靠外的模块文件可被自动加载——动手前主动读 `docs/AGENTS.md`**
  （README 成对、§14.4 门禁、脚本不得停用或放宽）。
- 任务范围实质变化时，重新评估阅读清单。
- 详细设计文档（`metrics.toml`、`driver-spi.md`、行为契约、ADR 等）按任务需要再读：
  不递归通读 `docs/`，不重复读取内容未变化的长文档；但需要深入调查时不受此限。
- 目录布局以仓库实际为准，不在文档维护第二棵树（`dist/` 为发布产物、`tools/` 为生成器）。

## 6. Driver SPI 红线（全文：`backend/AGENTS.md` §6；设计真值：`docs/database/driver-spi.md`）

- 项目自有稳定 SPI，与具体驱动库解耦；驱动为 `backend/crates/drivers/<db>/` workspace crate，不引入动态库插件 ABI，不设第二处驱动目录。
- 不假设所有数据库等价：Database/Catalog/Schema 等概念不折叠；元数据保留原类型名、限定名与精度/长度/符号/时区/字符集/排序规则/默认值。
- 值不得全退化为 String（§6.3 细则）；不支持的能力显式返回 unsupported，绝不伪装成功。
- 方言与 DDL 归对应驱动，不在通用业务逻辑散落数据库名分支；阻塞驱动用 blocking pool，不占 Tokio worker。

## 7. gRPC / Protobuf 红线（全文：`proto/AGENTS.md`；命名真值：`docs/api/grpc-conventions.md`）

- 服务按业务域划分，新增前先确认现有域能否扩展；包名带显式版本（如 `product.query.v1`）。
- 已发布字段号永不复用，删除用 `reserved`；新字段给合理默认语义；不兼容变更 = §15.2 先确认项。
- 大结果集默认禁止整包序列化返回：查询走 Query ID + 批量/流式 + 取消 + 背压；表浏览走游标分页并定义排序稳定性。
- 错误用稳定错误码 + 可安全展示信息，绝不回传密码、完整连接串、内部堆栈；取消传播到驱动且幂等。
- 改契约后重新生成全部受影响客户端并跑契约测试；不得靠无限放大 gRPC 消息上限解决大结果问题。

## 8. 原生前端红线（全文：`frontend/AGENTS.md` §8）

- 优先平台原生控件，四档 L0–L3：平台已有的不自造（L3 禁止，先 ADR 再实现）；自绘组件须登记注册表（`hand_drawn`/`why_not_native`）。
- 「未实现」不是灰化占位：能力级不可用不出现在界面；状态级不可用可见 + 禁用 + 原因（唯一例外：对象树占位分组行）。
- 跨端一致的是**行为**（条目、顺序、可用性、快捷键、文案）；尺寸/字重/间距/颜色一律来自 `docs/ui-specification/metrics.toml` 与主题 token，禁止代码算 `base × scale`；仅字体族按平台差异并记 `platform_notes.*`。
- 颜色只用 token，唯一例外是品牌色板（Linux/Windows 两份取值必须一致，见 frontend §8.6）；深色模式默认跟随系统。
- SQL 编辑器与数据网格是核心组件，不得用普通文本框/列表凑数；变更尺寸/规则须同 PR 更新 metrics、缩放文档与（涉行为时）行为契约。

## 9. 安全与隐私红线（任何任务不得弱化；本节改动属 §15.2）

- 密码、token、私钥只进系统凭据库（Windows Credential Manager / macOS Keychain / Linux Secret Service）；普通配置只存引用或非敏感设置；**绝不明文存储数据库密码**。
- 凭据只在必要期间驻留内存；日志、遥测、崩溃报告、测试快照、错误响应中绝不出现密码、完整连接串或含密信息；错误文案可诊断但不泄露凭据、完整连接串、私有本机路径。
- 导入/导出校验路径与权限，防目录穿越与意外覆盖；执行 SQL 保持数据库既有权限语义，不默认用高权限账号；高危操作说明影响范围。
- 本地 IPC 强制访问控制（§2）；未来远程连接必须 TLS 并验证服务端身份；默认不上传用户 SQL、schema、数据样本、连接信息；未来遥测须显式同意、最小化、可退出。
- 升级依赖时评审安全公告、许可证与供应链风险；仓库绝不提交真实密码、token、私钥、证书或真实连接串。漏洞报告流程见 `SECURITY.md`。

## 10. 性能与资源红线（全文：`backend/AGENTS.md` §10）

- 优化以可复现基准为前提；无测量不宣称「零开销」「极致性能」。
- 查询结果、导入导出、对象树、大文件设内存上限；后台任务设并发上限、超时与取消；不用无界 channel/任务/缓存制造流畅假象。
- 每个查询、连接、cursor、临时文件有明确生命周期与清理路径；客户端断开后未释放的 Session/Cursor/临时文件必须可回收。

## 11. Rust 编码红线（全文：`backend/AGENTS.md` §11）

- rustfmt + clippy 必须过；豁免 lint 要局部抑制并写明原因。错误类型带上下文、可分类，不一律转 String。
- `unwrap()/expect()/panic` 仅用于已确立不变式处；不跨 `.await` 持锁；`unsafe` 写明安全不变式并加针对性测试；公开类型与复杂算法有文档注释。

## 12. C# / Swift / GTK 准则（全文：`frontend/AGENTS.md` §12）

UI、状态、gRPC 客户端、平台适配分层；View/Widget 不放业务逻辑；UI 线程不做同步网络/磁盘/数据库；每条异步路径处理取消、异常与销毁后回调；UI 状态机可表达 Loading/Empty/Success/Error/Cancelled/Disconnected。

## 13. 测试与验证基线

- 检查类型齐备：单元（规则/类型转换/方言/错误映射/生命周期）、驱动集成（经 `driver-testkit` 符合性）、契约（Protobuf 生成与兼容性）、端到端、性能、安全（日志脱敏、凭据、IPC、路径、泄露）。
- 用户可见行为（交互、状态流转、可访问性）的验证以**该平台自动化功能用例**为载体；三端用例库的共同契约（语义选择器、输出摘要、隔离、PASS/FAIL/BLOCKED 如实报告）以 `tests/ui/README.md` 为真值，各端命令与环境限制见各端 README 与 `justfile`（Linux 现为 `tests/ui/linux/`，入口 `just ui-test-smoke|sql|all`；Windows/macOS 首次涉及该类验收时按同一定位建立）。
  - UI 功能开发、改动、缺陷修复与功能验收：必须先读 `tests/ui/README.md` 与对应端 README；验证一律优先复用既有用例与复用操作，已有用例覆盖的行为**不得再人工反复点界面**。
  - 新增/变更 UI 行为：同一 PR 内按组件 `id` 找到既有用例更新，无覆盖则**先补用例、再改再验**；纯样式调整且交互行为不变的不强制新增功能用例（样式由人工确认，ADR 0029）。
  - 改动后执行**最小相关子集**并如实报告 **PASS / FAIL / BLOCKED** 三态（BLOCKED = 该验证在当前环境无法执行，必须写明原因，不得写成通过；与注册表 `sync = "blocked"` 状态值是两回事）。
  - 例行 UI 功能测试不含视觉回归、样式断言、截图比对、OCR 或 AI 视觉识别；截图仅当任务验收条件明确声明时允许，且不触发自动视觉分析。
- **验证范围按变更影响与风险选档，档位不是强制阶梯**：Level 1 已覆盖风险就不机械上跳；跨模块风险明显可直接高档；没有任何任务被要求必须全量测试。命令以仓库实际为准（`just` 目标见 `justfile`）：
  - **L1 局部**：`cd backend && cargo fmt --all -- --check`、`cargo clippy --all-targets -D warnings`、`cargo test -p <crate>`（再以 `--test <名>` 收窄）；Linux 前端同套命令在 `frontend/linux/`（独立 cargo 工作区）；Windows：`cd frontend/windows && dotnet build Sqarvo.Windows/Sqarvo.Windows.csproj -p:Platform=x64 -c Debug`、`dotnet test Sqarvo.Core.Tests/Sqarvo.Core.Tests.csproj -c Debug`、仓库根 `python scripts/xaml_lint.py`。
  - **L2 模块**：`just test`（后端全量含 UDS gRPC 冒烟）或 `cd frontend/linux && cargo test`；驱动集成按 `tests/integration/` 编排（mariadb），SQLite 端到端 `cargo test -p sqarvo-rpc-server --test grpc_sqlite_e2e`。
  - **L3 跨模块**：Protobuf 字段或 Driver SPI 接口改动、前后端行为联动、共享组件改动 → 重新生成各端客户端 + 契约/e2e 测试 + 实际构建受影响前端；UI 注册表改动 → `scripts/check-ui-sync.sh gen` 与 `mark`。
  - **L4 全量**：重大架构调整、发布前、高风险系统性变更或明确要求 → `just check` + 各模块全量 + 门禁（`scripts/check-ui-sync.sh release-check`、`scripts/check-tracking.sh`、`python scripts/check-readme-pair.py`）。
- 跑不了的平台按 **BLOCKED + 原因**报告该项，不声称通过（三态口径见上）；测试只用专用数据库与合成数据，禁连生产库或使用真实凭据。
- UI 验收只验**交互与功能**（响应、状态流转、可访问性名称、控件在位），不做像素比较与图像识别，样式由人工确认（ADR 0029）；注册表 `evidence` 只收命令/断言/可访问性读取结果。

## 14. 文档与决策（全文：`docs/AGENTS.md`；本节为有约束力的摘要）

- **14.1 唯一真值**：产品规划 `docs/product/`、开发规划 `docs/planning/`、技术真值 `docs/architecture|api|database|ui-specification/`、UI 组件注册表 `docs/ui-specification/components/*.toml`（`ui-components.md` 是生成物禁止手改）、ADR `docs/decisions/`（追加式）、任务状态 GitHub Issues/Projects（`scripts/check-tracking.sh` 校验）。可多处摘要，不得重复维护细则；里程碑无验证证据不得标完成；不在仓库外复制计划、不在仓库内建第二任务清单。
- **影响范围驱动同步**：架构变→架构文档/ADR；API 变→契约+生成+契约测试+`docs/api/`；UI 行为变→注册表+mark（全局规则→契约 §8）；功能行为变→对应文档；纯内部实现不碰无关文档。`README.md` 与 `README.zh-CN.md` 是一个交付物，同一提交共改（`scripts/check-readme-pair.py`）。
- **14.2/14.3 注册表（绑定）**：逐组件行为唯一真值=注册表（契约只留全局规则与已知偏差）。改/增用户可见组件必须同 PR 改注册表并报组件 `id`（授权任务已点名组件时视为已确认范围）；`id` 发布不改名；条目出现 `px`/`#RRGGBB` 即缺陷；规范改动使三端 mark 失效，重核后 `scripts/check-ui-sync.sh mark`；退役走 `status=removed` 生命周期，不许删条目。
- **14.4 发版门禁（不得绕过）**：发布前 `scripts/check-ui-sync.sh release-check` 必须通过——支持平台（当前 Linux、Windows；macOS 未接入不计）所有 active/deprecated 组件 `sync` 为 done/na/blocked（有说明）。带缺口发版须在 `docs/releases/<版本>.md` 单列「未同步的 UI 行为」并取得维护者批准，不允许静默发布。

## 15. Agent 执行策略（风险分级；取代原 §15.0 逐次确认单机制）

- **15.1 已授权任务范围 = 允许自主执行**：修改相关实现与测试、修订直接关联文档、修复本次变更引发的编译/格式/自检失败、服务目标的局部重构——无需逐文件逐次申请。执行中查 `git status`，不覆盖他人未提交改动。
- **15.2 必须停下先问维护者**：超出授权范围的大改（发现要做不下时报告再授权，不「顺手扩大范围」）；重大架构调整；公共 API / Protobuf 不兼容变更；生产环境或真实用户数据；删除/覆盖重要数据与不可逆操作；修改安全策略或降低保护水平；引入新 runtime 或大型依赖。
- **15.3 Git 操作授权——三项权限相互独立（重要）**：修改文件、`git commit`、`git push` 是三个独立权限，授予其一**不隐含**其他：「先做 / 直接做 / 开始开发」只授权任务范围内的文件修改，不授权提交；commit 仅当用户明确要求提交时执行；push 只授权推送**已有**提交，不自动授权新建 commit；「完成后提交并推送」则三项依序授权。授权不要求固定措辞，按当前任务与上下文判断，**但不得把模糊表述解释为更高权限**；已明确授权的操作不反复确认。本仓库惯例「直推 `main`、提交主题中文 `<模块>：<描述>`」只回答「该操作获批后怎么做」，分支策略本身不构成 commit 或 push 授权；各项授权语原样记入 Issue/PR 作为执行依据（授权内文件修改无需逐次确认，见 §15.1）。
- Git 只读操作（`status`/`diff`/`log`/`show`/`branch` 查询）随时自由使用；动手前检查 `git status`，不覆盖、不丢弃其他用户或 agent 的未提交改动，也不把无关变更纳入自己的提交（`git add` 点名路径，不整仓 add）。
- 破坏性或不可逆操作——force push、`reset --hard`、`clean`、rebase/重写已发布历史、删远端分支——逐项需要明确点名该操作的授权；处置已发布事故另按 §15.4 与维护者批准执行。
- 多 agent / 多工作线并行：同一文件不允许多个写入方；同一工作树避免并发提交，跨 Issue 并行时用独立 `git worktree` + 分支，或事先约定不相交的文件集合。
- **15.4 归因禁令**：不加 `Co-authored-by:` / `Signed-off-by:` 等第三方或泛用 agent  trailer——GitHub 按邮箱归属到陌生账号且已发布提交不可追回。
- **15.5 禁止事项**：不伪造命令输出/测试结果/连接结果；不手改生成 Protobuf/gRPC 代码；不绕过 Driver SPI 硬编码数据库；不以 Web runtime 冒充跨平台原生；不把未实现/未测试/占位功能描述成已完成；无验证证据不标里程碑完成。

## 16. 完成标准（Definition of Done）与交接

**完成判定**（文档、Bug 修复、功能、重构按实际影响套用，不搞一刀切）：

- 实现满足任务的明确要求，且不违反适用的 §1–§10 红线；
- 相关接口、契约与注册表和代码保持一致（同步范围按 §14 矩阵）；
- **代码写进文件不等于完成**：按 §13 选档实际执行验证并拿到结果后才作完成声明；无法执行的项报告为 **BLOCKED + 原因**（§13 三态）；
- 不含无关改动（不顺手修别的，发现问题报告另开任务）；
- 已知限制、风险与未完成项如实报告。

**收尾报告**至少：完成内容（改了什么、影响模块）/ 设计取舍 / 验证（命令 + 真实结果；UI 只述交互与功能，样式人工确认）/ 未完成 / 风险与后续。不要只回「Done」，不要把计划中的工作描述成已实现。

**轻量任务记录**：复杂、长上下文或需跨会话继续的任务，允许留一份简短记录（目标 / 已完成 / 关键决策 / 受影响文件 / 已执行验证 / 未解决问题 / 下一步）；简单任务不强制，不要求每次开发产出 Markdown 报告，不复制仓库已有规范。子 Agent 交接以「结论 + 文件位置 + 验证证据 + 待决事项」为先，主 Agent 负责最终集成与验证（§17）。任务记录放 Issue 或会话工作区，不作为第二任务清单进仓库（§14.1）。

## 17. 子 Agent 委派

- 简单或局部任务默认主 Agent 直接完成；不为用而委派，不为了用子 Agent 刻意拆分任务。
- 适合委派：可独立并行的工作线、高上下文探索/审计、测试审查。是否委派由复杂度、上下文成本、并行收益与冲突风险综合判断，不设固定数量配额。
- 每个子 Agent 职责单一而窄；同一文件不允许多个 Agent 同时修改；同一探索任务不重复委派。
- 主 Agent 对最终代码质量、集成结果与测试负责：子 Agent 汇报是自称，涉及外部副作用（上传、远端写、发布）必须回读实际 URL/路径/状态后才可转述「完成」。
- 成本控制：不反复读大文件与构建日志；开发期优先定向测试；任务意外变贵时，先报告进度与阻塞点，再决定是否扩大范围。
