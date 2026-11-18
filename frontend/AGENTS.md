# frontend/AGENTS.md — 原生前端细则

作用域：`frontend/windows`（WinUI 3 / C#）、`frontend/linux`（GTK4 + libadwaita / Rust）、
`frontend/macos`（SwiftUI / Swift，尚未接入门禁）。共享文案目录 `l10n/` 位于 `frontend/`
之外：在 `l10n/` 内直接编辑时以 `../l10n/AGENTS.md`（指针）为入口，本文件规则不会跨目录
自动加载；生成器在 `tools/l10n_gen.py`。根 `AGENTS.md` §1–§5、§9、§13、§15 恒定适用；本文件承载根 §4.1、§8、
§12 完整细则与 §14.3/14.4 的前端侧执行。UI 真值文档：`docs/ui-specification/`（metrics.toml、
scaling-and-window-sizing.md、ui-behavior-contract.md、components/ 注册表）。
平台级规则目前较少，暂不设 `frontend/<platform>/AGENTS.md`；某平台积累出独立的长期规则后再单列。

## 4.1 前端职责与禁止事项

负责：原生窗口、导航、菜单、工作区、对话框与交互；SQL 编辑器、结果表、对象树渲染；输入预校验与交互反馈；gRPC 调用与取消、分页、进度、后端生命周期处理；平台集成（系统凭据库、文件选择、通知、托盘、快捷键、安装包）。

禁止：实现数据库网络协议或直连用户数据库；跨端复制驱动/连接池/SQL 执行逻辑；绕过 gRPC 直用后端内部模块；UI 线程做数据库请求、大文件处理或其他长任务；把数据库密码写入普通配置、日志、崩溃报告、遥测。

## 8. 原生前端规范

### 8.1 通用要求
- 不使用 Electron、Chromium、CEF、WebView2、WKWebView、WebKitGTK 或任何嵌入式 Web runtime 作为 UI；不用 HTML/CSS/JS 页面仿原生界面。业务交互用原生控件实现；必要时复用纯算法库或生成协议代码是允许的。
- **优先平台自带控件**，四档：**L0** 原生控件默认样式；**L1** 原生控件 + 主题 token 定制（含其自带单元格/行）；**L2** 平台没有的复合件自绘或手搭——允许，但须在注册表登记 `hand_drawn` / `why_not_native`（登记即可，不需 ADR）；**L3** 手搓平台已有的东西（自己的菜单/对话框/表格/标签条）——禁止，先 ADR 再实现。
- **「没实现」不是灰化占位。** 能力级不可用（此连接/此驱动/此平台根本不支持）：不出现——不列该项、不渲染该按钮。状态级不可用（无选中、无脏行、只读、RPC 在途）：可见 + 禁用 + tooltip/状态行说明原因（用户一步之内可使其可用）。唯一例外：对象树的「未实现」分组行（行为契约 §2.1 有意保留）。
- 跨端一致的是**行为**：条目、顺序、可用性、快捷键、文案必须一致；不一致要记契约 §9。尺寸（含字号**与字重**）、间距、颜色永远来自 `metrics.toml` / 主题 token，三端同源；只有**字体族**可按平台不同并记入 `platform_notes.*`（如「Windows 用 Segoe UI」）。不得把工具库自带样式表的数值当「平台惯例」。
- 图标码点必须配官方字形名（`docs/ui-specification/icons.toml` 的 `winui` + `winui_name`），不许猜码点；CI（`scripts/check-ui-sync.py`）校验。
- 三端保持功能与核心交互语义一致，不要求像素级一致，也不强行共享 UI 代码。

### 8.2 Windows
- WinUI 3 + Windows App SDK，C#；经生成的 gRPC 客户端与后端通信，隔离在通信层后。
- 遵循 Windows 惯例：窗口、菜单、键盘输入、主题、DPI、文件选择、可访问性。
- 不为实现复杂控件默认引入 WebView2。
- 标题栏主题必须随深色/浅色切换保持同步；XAML 静态检查：`scripts/xaml_lint.py`。

### 8.3 Linux
- GTK4 + libadwaita + gtk-rs；UI 更新必须在 GTK 主线程，数据库请求与文件处理跑后台并把结果安全派发回主线程，绝不阻塞主循环。
- libadwaita 组件按 GNOME HIG；同时为非 GNOME 桌面提供合理行为（不假设用户必在 GNOME Shell）。
- 文件、凭据、主题、桌面集成经 Linux 平台适配层；凭据走 Secret Service（[ADR 0014](../docs/decisions/0014-linux-credential-store.md)）。

### 8.4 macOS
- SwiftUI 为主，复杂桌面控件在有明确理由时用 AppKit；遵循 macOS 菜单、快捷键、窗口、工具栏、文件对话框、系统凭据管理惯例。
- 用 Swift Concurrency 或适当异步机制，主线程不做阻塞数据库操作；生成的 gRPC 代码隔离在通信层后。
- macOS 前端未接入发版门禁：`sync.macos` 不参与 release-check（接入当日加进 `check-ui-sync.sh` 的 `SUPPORTED_PLATFORMS` 并同步根 §14.4）。

### 8.5 SQL 编辑器与数据网格（核心组件）
不得用简单多行文本框或普通列表凑生产功能。
- 编辑器渐进支持：大文本高效更新、多方言语法高亮、行号/当前行/选区/缩进、查找替换、多语句与当前语句执行、补全与元数据提示、大文件与长文本响应。
- 网格渐进支持：虚拟化/按需渲染、分页/批量拉取（默认不全量入内存）、列宽调整/排序/过滤/复制、NULL 与二进制/日期/Decimal/JSON 正确显示、编辑状态与脏标记、提交与回滚、大字段展开/延迟加载、明确的只读状态。
- 引入控件库/编辑器组件前验证：许可证、跨平台支持、可访问性、维护状态、大数据性能。不以流行度作为依据。

### 8.6 尺寸、密度、主题与本地化（绑定；决策 ADR 0026/0027/0028/0031）
- **不硬编码第二份尺寸。** Linux 运行时解析 `docs/ui-specification/metrics.toml`；Windows/macOS 构建期生成常量，生成失败即构建失败。禁止代码计算 `base × scale`（三端取整不同会漂 1px）。
- 密度只有三档：`compact` 100%（默认）、`comfortable` 125%、`large` 150%。加档位必须扩 `metrics.toml` 与不变量测试；低于 100% 按设计拒绝（破坏 24px 最小目标与 CJK 13px 字距下限）。
- 系统 DPI/缩放系数绝不乘进应用 metrics；GTK 逻辑像素与 XAML DIP 本已密度无关，仅在与 `AppWindow` / `DisplayArea` / `GdkMonitor::geometry` 交互时换算物理像素。
- 窗口尺寸取自**逻辑工作区**，绝非屏幕分辨率；最小窗口 `min(960·T, 工作区−32) × min(640·T, 工作区−48)`；窗口不得超出工作区（含 1080p@200%，逻辑 960×540）。
- 颜色只用主题 token（`@define-color` / `ThemeResource`）；界面 CSS/XAML 出现 `#RRGGBB` 即缺陷。唯一例外是品牌色板：Linux `frontend/linux/src/theme.rs` 的 `LIGHT_PALETTE`/`DARK_PALETTE` 与 Windows `frontend/windows/Sqarvo.Windows/Tokens.xaml` 的 `SqarvoBrand*` brushes 必须逐值相同（每主题三档）。高对比度两端都用系统颜色。品牌面（主操作填充、当前类型高亮、强调文字/标记）三端一律用品牌中调；系统强调色只留给平台自绘物（焦点框、系统菜单、系统对话框）。Linux 在关键处按 `AdwStyleManager::is_high_contrast()` 分支。
- 深色模式各端默认跟随系统；用户可在「视图 → 深色模式」（跟随系统/浅色/深色）覆盖，持久化为 `window.json` 的 `theme`。Linux 用 `AdwStyleManager::set_color_scheme`；Windows 启动设 `Application.RequestedTheme`、运行时用元素级 `FrameworkElement.RequestedTheme` 切换并保持标题栏同步。不实现私有 light-only 回退。
- 新的用户可见字符串一律进 `l10n/` 共享文案目录（schema 见 `l10n/README.md`，构建期由 `tools/l10n_gen.py` 生成 Rust catalog / .resw，ADR 0031），即便当前只发中文也不写内联字面量；区名对齐 UI 注册表，不发明第二套命名。新交互控件必须有可访问性名称与确定的 Tab 顺序。
- 改以上任何数值或规则：`metrics.toml`、`scaling-and-window-sizing.md` 与（涉行为时）`ui-behavior-contract.md` 同一 PR 更新。

## 12. C# / Swift / GTK 工程规范

- 各语言官方 formatter 与静态分析必须通过（Linux：rustfmt/clippy；Windows：XAML 检查与分析器；macOS：Swift 格式与 Xcode 分析）。
- UI、状态管理、gRPC 客户端、平台服务适配器分层；View/Widget 不放业务逻辑。
- 所有异步路径处理取消、异常与组件销毁后到达的回调；UI 状态必须能表达 Loading / Empty / Success / Error / Cancelled / Disconnected。
- UI 线程不做同步网络、磁盘、数据库操作。
- 引入第三方控件前验证：原生集成度、许可证、可访问性、键盘支持、高 DPI/缩放行为、大数据性能。

## 13. UI 验收（ADR 0029，全文约束）

- 只验证**交互与功能**：控件是否响应、状态是否正确流转、可访问性名称/控件文本是否符合预期、控件是否在预期位置。
- 不做样式验证与图像识别：不像素比较、不读截图判外观、不断言颜色/间距/圆角/密度/对齐——样式由人工确认。
- 注册表 `sync.*.evidence` 只写：命令、断言、QA 开关输出、可访问性名称/控件文本读取结果。历史条目里的截图不回改，新登记不再以截图为准。
- `--qa-*` 开关是验收辅助通道，不算用户可达行为。

## 14.3 / 14.4 注册表与发版门禁（前端侧执行；全文 `docs/AGENTS.md`）

- 改 UI 前先报组件 `id` + `display_name` 确认范围（授权任务已点名组件时视为已确认）；新增/改动用户可见组件必须同 PR 更新 `docs/ui-specification/components/*.toml`。
- `id` 发布后不改名；`display_name` 可随文案演进。样式字段只写 metrics key 与 token 名。
- `sync.<plat>.state = "done"` 必须带当前 `spec_hash`；规范变更使三端标记自动失效，恢复 `done` 须按新规范重新核对后跑 `scripts/check-ui-sync.sh mark`。`partial`/`na`/`blocked` 必须写清差异；`blocked` 同时记行为契约 §9。
- 「这版不要它了」用生命周期：`status = "removed"` + `removed_in` + `removed_reason`（在用将被取代用 `deprecated`）；不许直接删条目（其他端要靠它知道「要去掉什么」），三端清完才归档 `_archived.toml`。
- `ui-components.md` 是生成物，CI 比对有差异即失败；发版门禁见根 §14.4，前端任务不得以「重构授权」为由绕过 mark 与 release-check。
