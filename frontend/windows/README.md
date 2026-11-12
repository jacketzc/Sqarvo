# Windows 前端（WinUI 3 + C#）

## 开工前置：先读口径，再写第一行 XAML

Windows 端的窗口尺寸、密度档、主题与本地化口径已经定下，**不要自己发明一套数字**：

| 要读的 | 管什么 |
|---|---|
| [`docs/ui-specification/scaling-and-window-sizing.md`](../../docs/ui-specification/scaling-and-window-sizing.md) | 三轴拆分（系统 DPI / 密度档 / 字号）、按逻辑工作区分档的窗口尺寸、最小窗口收敛、`window.json` v2、Windows 映射要点（§8） |
| [`docs/ui-specification/metrics.toml`](../../docs/ui-specification/metrics.toml) | 三档密度的**每一个**尺寸数字（构建期生成 C#，不在运行时算 `基准 × scale`） |
| [`docs/decisions/0026-ui-scaling-window-sizing.md`](../../docs/decisions/0026-ui-scaling-window-sizing.md) | 为什么是三档、为什么不复刻 Linux 紧凑密度、为什么切档要重建页面 |
| [`docs/decisions/0023-cross-platform-shell.md`](../../docs/decisions/0023-cross-platform-shell.md) | 哪一层必须三端一致（区域 / 语义 / 快捷键）、哪一层原生实现 |
| [`AGENTS.md` §8.2 / §8.6](../../AGENTS.md) | 项目级约束（含「XAML 里不写 `#RRGGBB`」「未打包没有 `ApplicationData`」） |

四条最容易写错的：

1. **两套像素单位**：XAML 是 DIP，`AppWindow.Resize` / `MoveAndResize` / `DisplayArea.WorkArea`
   是**物理像素**。尺寸在 DIP 里算，喂给 `AppWindow` 前乘一次 `XamlRoot.RasterizationScale`。
2. **DPI 变化只能从 `XamlRoot.Changed` 拿**，`DisplayInformation.GetForCurrentView()` 在 WinUI 3 抛异常。
3. **最小尺寸**用 `OverlappedPresenter.PreferredMinimumWidth/Height`（物理像素、含边框），
   不要同时设 `Window.MinWidth/MinHeight`，两者会互相覆盖。
4. **主题**：`Application.RequestedTheme` 只能在启动时设；运行中切换用元素级
   `FrameworkElement.RequestedTheme`；**标题栏要单独处理**，否则暗色窗口顶上是白的。

跟踪 Issue：#162（度量表生成 + 主题三档 + 标题栏 + 未打包持久化）、#163（窗口尺寸 / DPI / 最小尺寸 /
密度档重建）、#168（窗口层缺陷：DPI 订阅、位置 / 尺寸事件分流、最大化恢复、面板可拖、度量单一口径）。

## 状态

界面骨架 + 度量与主题自检页。**尚未启动后端、尚未建立 gRPC 连接**；工具条上的入口都还没有真实动作。

六区域外壳（#164）：系统标题栏（`AppWindow`）+ 窗口内菜单行（`MenuBar`）→ 主工具条（`CommandBar`）→
对象树（`TreeView`）→ 工作区（`TabView`，首标签 `Objects` 用 `IsClosable=false` + 固定首位 + 禁拖
等价实现 pinned）→ 自绘双行状态栏 → 信息面板（`SplitView` 右面板）。菜单只有 文件 / 表 / 帮助 三个集合：
文件（新建连接… / 新建查询标签 / 退出）、表（导出 CSV…）、帮助（关于 Squarvo）；未实现的入口全部
禁用并 tooltip「尚未接入后端」。

### 六区域的形态与密度（#170，已按五块重做）

#164 只保证「区域都在」。#170 的第一版把六区域整体重写（`CommandBar` + `AppBarButton`、
`ToggleButton` 种类入口、`TabView.TabWidthMode=SizeToContent`、`ProgressRing`、`FontIcon`），
在真机上 5/5、3/3 **启动即崩**（`0xc000027b`），已整体回退。

现在的做法是**分五块、每块一个可单独 revert 的提交，且一律不引入新控件类型**：

- **工具条**：`Grid` 四列。左动作组是两个 `Button`（内容 `Border` + `TextBlock`，图标用
  `Segoe Fluent Icons` 的**字形**，不是 `FontIcon`）；「连接」的强调色落在**内层 `Border`** 上——
  `Button` 模板的 Normal 视觉态会盖掉 `Button.Background` 的本地值，写在 Button 上等于没写（真机踩过）。
  → 1px 竖分隔（`ControlStrokeColorSecondaryBrush`；`DividerStrokeColorDefaultBrush` 太淡，1px 看不见）
  → 四个种类入口是 `Border` + `TextBlock`，选中项浅底 + SemiBold，未选中次要色。
- **标签条**：`Objects` 固定页 `IsClosable=false`、恒在最左，页头是字形 + 短文字；查询页与数据页可关。
  `TabView` 自身属性一个都不动（`TabWidthMode` 保持默认 `Equal`，代价是单标签会被拉满整条）。
- **状态栏**：两行。行 1 = 状态文案 + 六个行操作图标（`SqarvoStatusIconButton` 样式把框架
  `MinHeight=32` 压到 `icon_button_height`）+ 搜索 + 结果行数 + 分页器（`第 ? / ? 页`，不写死 `0 / 0`）；
  行 2 = `SQL:` 摘要 + 事务状态（`TransactionText`）+ 会话名 + `Last Refresh`。
- **树与间距**：行高 `row_height_tree`、行字号 `font_ui`（框架默认 14 比空态标题还大）；树右留
  `tree_arrow_box`；工具条下缘与状态栏上缘各一条 1px 线；空态居中、主标题 SemiBold。
- **文案**：树根 / 分组 / 空态 / 列头与 Linux 客户端逐字一致（每条改动在提交说明里附了
  `frontend/linux/src` 的出处行）。

树行**没有**做到「徽标右对齐弱色、分组加粗」：现在整行文本仍由 `TreeViewNode.Content` 的
`ToString()` 提供，`OnTreeItemInvoked` 按 `Content` 运行时类型分派——换行模板就等于改分派，#170
的回退边界就在这里。前置条件是先做「节点 → 行身份映射」（见 issue #173）。

NULL 与空串的渲染口径：查询结果把空文本渲染成 `''`（证明「有值但为空」），表数据页保持空白；
两者的 NULL 都是**三级文字色 + 斜体**（只用二级色在真机上与正文分不出来，`TextFillColorTertiaryBrush`）。


本机（Linux）能做的检查只有四项，**不能**代表已编译：

```bash
# XAML 良构
python3 -c "import xml.dom.minidom,glob;[xml.dom.minidom.parse(p) for p in glob.glob('frontend/windows/Sqarvo.Windows/**/*.xaml',recursive=True)]"
# 没有 #RRGGBB
grep -rn '#[0-9A-Fa-f]\{6\}' frontend/windows/Sqarvo.Windows/ --include=*.xaml
# Click= / Tapped= 都有同名处理器
grep -rno 'Click="[^"]*"\|Tapped="[^"]*"' frontend/windows/Sqarvo.Windows/ --include=*.xaml
# x:Name 不重复（按文件）
```

已接通（#162）：

- **三档密度度量**：`docs/ui-specification/metrics.toml` → 构建期由
  `frontend/windows/tools/gen-metrics.ps1` 生成 `obj/gen/Metrics.g.cs`（不提交）。页面 XAML 只引用
  `{StaticResource Size.*}`，code-behind 里 `Padding` / `BorderThickness` 也全部从 `Metrics` 取，
  所以尺寸只有一个来源，运行时不做「基准 × scale」。
- **主题三档**：`Tokens.xaml` 的三套 `ThemeDictionaries`（Default / Light / Dark / HighContrast）
  全部别名到系统 token；启动期 `Application.RequestedTheme` 只在明确选了 Light/Dark 时写，
  运行中切换走元素级 `RequestedTheme`；标题栏单独处理（跟随系统时
  `TitleBarTheme.UseDefaultAppMode`，旧 API 回退 `DwmSetWindowAttribute`）。
- **未打包持久化**：`%LOCALAPPDATA%\Sqarvo\window.json`（schema v2，与 Linux 同一份；写入用
  「临时文件 + 替换」，读失败回退默认值，不弹窗）。

诊断开关（只读，打印一行后退出；`stdout` 只有证据行，内部诊断走 `stderr`）：

```powershell
dotnet run -c Debug -p:Platform=x64 -- --report-theme --report-metrics
# 若 dotnet run 找错输出目录，直接跑 bin\x64\Debug\net8.0-windows10.0.19041.0\Sqarvo.Windows.exe
```

```text
[theme] requested=System effective=Dark highContrast=False accent=#258292 titleBar=UseDefaultAppMode
[metrics] tier=compact header=64 toolbar=44 status=48 minWindow=960x640 sidebar=237 inspector=280 rowTree=24 fontUi=12
```

窗口层与外壳（#163 / #164）另有四个开关：

```text
--report-monitor          显示器 / 缩放 / 工作区（物理与 DIP）/ 最终窗口 / 档位 / 来源 / 最大化 / 实测非客户区高
--report-shell            UI Automation 枚举整棵控件树（ControlType + Name，缩进两格）
--self-test-window        窗口尺寸算术与面板宽度换算的边界断言（纯函数，不建窗口；失败以非 0 退出）
--qa-tier=<id>            QA 专用：启动后切到该密度档并把来源标成 QA，用来复现「切档后窗口跟着改大小」
--qa-connect=<名称|@first> QA 专用：握手 → 测试连接 → 连接 → 断开，打 [connect] 行后退出
--qa-metadata=<名称|@first> QA 专用：连接后**真的**展开对象树（库 → Tables → 第一张表），
                          把每层计数与列名打到 [metadata] 行；可与 --report-shell 合用
--qa-catalog=<库名>        给 --qa-metadata 指定要展开的库（省略时取第一个可见库）
--qa-query=<sql>           QA 专用：连接后**真的**建一个查询标签并执行这条 SQL，首批回来即打 [query]
                          证据（state / rows / columns / has_more / query_id / columns= / first-row= /
                          status= / open-cursors=）；连接目标取 --qa-signin（否则第一条）。
                          与 --qa-signin 合用时不退出（供截图），否则关页验证 Close 后退出
--qa-data=<库.表>          QA 专用：连接后**真的**开数据页 → 翻一页 → 改一格并提交 → 回读验证 → 还原 →
                          再改一格并撤销，打 [data] 证据；连接目标取 --qa-signin（否则第一条）。
                          与 --qa-signin 合用时不退出（供截图），否则关页验证回滚与 Close 后退出。
                          注意：**会写入再还原开发库里的一个单元格**，不要指向生产库
```

### 查询面（#172）

标签条新增可关闭的 `查询 N` 页（`Objects` 固定页仍在首位、不可关）；`Ctrl+T` 新建、`Ctrl+W` 关当前页、
`Ctrl+Enter` 执行、`Ctrl+.` 取消。查询页**整页在代码里搭**（`Views/ShellView.Query.cs`），
`ShellView.xaml` 一行未改——#170 的教训是外壳控件形态一动就可能秒崩，所以这一片只加内容：

- 编辑器：等宽多行 `TextBox` + 行号沟（跟随编辑器内部 `ScrollViewer` 滚动）；**有选中就只执行选中部分**。
- 结果网格：列头「列名 · 原始类型」+ 行号列 + 数据列；**只建视口内的行**（上下用高度占位撑总高），
  表头与行区横向同步滚动；每批 200 行，滚动到接近末尾或点「加载更多」续拉 `FetchBatch`。
- 五态：Loading / Empty / Error（文案 + 稳定错误码）/ Cancelled（保留已取行）/ ReadOnly；
  状态栏行 2 换成 `SQL: <摘要>` 与 `N 行 / 已取 M 行`（总数未知写 `?`，不编造）。
- 资源纪律：一个标签一个游标、标签上限 8；重新执行 / 取消 / 关页 / 读到底 / 离开页面（有在途执行时）
  都必须 Close，页面重建与退出时逐个 Close。

### 表数据面（#173）

双击树上的表 / 视图、选中后按 `Enter`、或右键「浏览数据」都可以打开一个可关闭的数据页
（同一张表复用已有标签，上限 8 个，`Objects` 固定页不占名额）。数据页同样**整页在代码里搭**
（`Views/ShellView.Data.cs`），`ShellView.xaml` 一行未改：

- 网格：行号列（行号 = offset + 下标 + 1，不是主键）+ 单行表头「列名 · 原始类型（· PK）」+ 数据行；
  NULL 用弱色斜体 `NULL`，**空串保持空白**（要给空串就留空）；主键列头用强调色标 `PK`。
- 分页：每页 200 行（`BrowseTable` 的 `offset` 分页），上一页 / 下一页 / 「加载更多…」
  （`offset = 已取行数` 续拉，不换页）；页码 `第 N 页 / 共 ? 页`，总数三态 `共 N 行` / `约 N 行` / `共 ? 行`。
- 编辑：双击单元格或 `F2` 进入编辑，`Enter` 提交到**本地暂存**（不落库）、`Esc` 取消；脏行行号显示 `●N`。
  `Ctrl+S` / 状态栏「应用（✓）」提交，`Ctrl+Z` / 「撤销（↶）」回滚；超过 100 行时改成「再按一次 Ctrl+S 确认」。
  编辑被拒逐条说明原因（只读表 / 主键列 / 行不可定位 / 值被截断 / 类型不在就地编辑范围）。
- 事务：状态栏行 2 右侧 `只读` / `自动提交` / `未提交 N 行` / `提交中…` / `已回滚`；第一次暂存才 `Begin`，
  提交成功才 `Commit`，撤销 / 关页 / 退出回滚。
- 守卫：有未提交改动时翻页 / 刷新被拒绝；关页第一次 `×` 只提示、第二次才放弃；关窗第一次被拦、第二次放行并回滚。
- 纯逻辑（暂存合并、脏行判定、冲突与只读判定、状态文案）在 `Workspace/DataModel.cs`，本机用
  `win-data-probe` 跑 `ALL PASS`；不在本片：排序 / 筛选下推、新增 / 删除行、导出。

本机（Linux）能做的只有 Core 构建 / 测试 + 结构检查 + 替身类型编译；**XAML 与实机行为只能在 Windows 上验**。

`--qa-metadata` 的存在理由：远程构建机上没有可靠的 UI 自动化，而「元数据真的拉到了没有」必须被验证。
它走与用户点击完全相同的加载路径（`EnsureCatalogsAsync` / `EnsureGroupLoadedAsync` / `Select`），
不是另一套快路径；`SQARVO_QA_PASSWORD` 给出时先写进 Windows 凭据库再连接。

行的**字段格式**如下（下面的数值是格式示例，**不是**任何一次实测记录；真实值以实机输出为准）：

```text
[metadata] backend=<Readiness> drivers=<N> version=<版本>
[metadata] catalogs=<N> names=<库名列表>
[metadata] catalog=<库名> page-items=<N> page-error=<错误码或 ->
[metadata] tree-tables=<N> tree-error=<错误码或 ->
[metadata] table=<对象名> columns=<N> pk-error=<错误码或 -> ddl-len=<字符数或 -1> ddl-exact=<True|False|-> ddl-error=<错误码或 ->
[metadata] columns=<列名:类型[:PK][,...]>（最多 8 列）
[metadata] probe=<ok|incomplete>
```

`--report-monitor` 与规范（`scaling-and-window-sizing.md` §9）里的 `--qa-report-monitor` **等价**：
`Diagnostics/AppOptions.cs` 的别名表把两套写法归一到同一个开关，验收脚本照规范抄也能跑。

`[monitor]` 行的字段（改动要连同这里一起改）：

```text
[monitor] display=3840x2160 scale=2 workArea物理=3840x2064 工作区DIP=1920x1032 → 窗口DIP=1400x826 物理=2800x1652 tier=compact 来源=分档 最大化=否 titleBar=40 menuRow=24 侧栏=237 面板=280
```

- `来源`：`分档`（按工作区推荐）/ `记忆`（`window.json` 的几何）/ `记忆+最大化` / `保留`（DPI 或工作区
  变了但保留当前矩形）/ `档位`（切档重算）/ `最大化` / `最小化` / `QA`（诊断开关强制）。
- `titleBar` 是**实测**值（窗口物理高 ÷ 栅格化比例 − XAML 内容实际高），量不到时写 `待测`，不编数字。

下面这组数字是**按规范 §5.3 推算**的（3840×2160@200%，逻辑工作区 1920×1032），并且已经被
`--self-test-window` 逐条断言——所以它们不是「某次实测的记录」，而是在任何机器上都能复核的期望值；
真实窗口尺寸以 `[monitor]` 行为准：

```text
compact     → 窗口DIP=1400x826  物理=2800x1652
comfortable → 窗口DIP=1750x984  物理=3500x1968
large       → 窗口DIP=1888x984  物理=3776x1968   （宽被 W-32 封顶）
```

## 窗口层行为（#168）

窗口层的四类事件**不再互相借道**（原来位置变化会走推荐尺寸路径，把窗口拉回屏幕中央）：

- **位置变化**（`AppWindow.Changed.DidPositionChange`）：只记账（还原边界）+ 按当前显示器刷新最小
  尺寸，**不重算**窗口矩形。
- **尺寸变化**（`DidSizeChange`）：只更新几何记忆，不抢回推荐尺寸。
- **DPI / 缩放**（`XamlRoot.Changed` → `RasterizationScale`）：与窗口重算同一个入口，保留当前 DIP
  矩形（只钳到工作区与最小窗口），不居中。
- **切档**：按该档推荐尺寸重算并居中（规范 §2.3 要求窗口跟着放大）。

最大化恢复按**还原边界**语义：`window.json` 里的 `window_width/height/x/y` 是**非最大化**矩形，
`maximized=true` 时先还原该矩形再最大化，所以系统「还原」回到的是用户上次的窗口大小。

侧栏与信息面板的分隔线本身就是拖动条（`Thumb`，不引入第三方 GridSplitter）：范围取 `sidebar_min..max`
与 `inspector_min..max`，默认宽度取 `sidebar_width` / `inspector_width`；松手后按 compact 归一值写回
`window.json`，再用 `SidebarWidthFor` / `InspectorWidthFor` 换算回当前档校验。

验证（Windows PowerShell）：

```powershell
cd frontend\windows\Sqarvo.Windows
dotnet build -c Debug -p:Platform=x64
dotnet run -c Debug -p:Platform=x64 -- --self-test-window
dotnet run -c Debug -p:Platform=x64 -- --report-monitor --report-metrics --qa-report-monitor
```

拖动实测（人工）：拖窗口到屏幕一角后松手 → 窗口必须留在原地；拖窗口右下角改尺寸 → 尺寸不被改回；
最大化后关窗再启动 → 仍是最大化，且「还原」回到上次的非最大化矩形；拖两条分隔线到两端 → 宽度停在
`sidebar_min/max` / `inspector_min/max`；重启应用 → 两条分隔线宽度保持；`Ctrl+Plus` 切到 150% 再切回
`Ctrl+0` → 面板宽度回到 237 / 280（不漂移）。

已知边界：`AccessibilitySettings.HighContrastChanged` 在未打包 WinUI 3 应用里订阅会抛
`COMException`，高对比状态改由 `HKCU\Control Panel\Accessibility\HighContrast\Flags` 兜底读取；
系统主题 / 强调色的跟随本身不受影响（那是 WinUI 与 `ThemeResource` 的职责）。

## 通信与进程层：`Sqarvo.Core`（可在 Linux 上编译与验证）

WinUI 的 XAML 不能在 Linux 上编译，而本仓库的 CI 与主要开发环境是 Linux。因此**与 UI 无关的
一切**（端点文件、Transport、后端进程生命周期、gRPC 通道与 token、7 个 service 的封装）都在
普通 `net8.0` 类库 `frontend/windows/Sqarvo.Core` 里，`Sqarvo.Windows` 只引用它：

```text
Sqarvo.Windows → Sqarvo.Core → Grpc.Net.Client / Google.Protobuf
```

- **契约生成的唯一位置**是 `Sqarvo.Core`（`<Protobuf Include="../../../proto/**/*.proto" .../>`），
  `Sqarvo.Windows.csproj` 不再有自己的 `<Protobuf>` 项，否则两处各生成一份同名类型。
- Core 里**不允许**出现 `Microsoft.UI` / `Windows.UI` / Windows App SDK；平台差异走
  `Platform/` 下的 `IPidLiveness`、`IProcessLauncher`、`IPathLayout`（默认实现内部按
  `#if WINDOWS` 分支）。
- 业务代码只接触 `BackendSupervisor` 与 `Clients/*`；`GrpcChannel`、`*ServiceClient`、token
  metadata、deadline 都封装在 Core 内部（`ArchitectureTests` 会守住这条线）。

分层与设计理由见 [`docs/planning/windows-frontend-parity-plan.md`](../../docs/planning/windows-frontend-parity-plan.md) §3。

验证（Linux 上就能跑，含对**真后端进程**的集成测试）：

```bash
cd frontend/windows
dotnet build Sqarvo.Core.Tests/Sqarvo.Core.Tests.csproj
dotnet test  Sqarvo.Core.Tests/Sqarvo.Core.Tests.csproj --logger "console;verbosity=detailed"

# 集成测试需要先构建后端（Linux 上用 --listen 走与 Windows 相同的回环 TCP 路径）
cd backend && cargo build -p sqarvo-backend-service
```

集成测试固定端点文件路径（`DefaultPathLayout.ForEndpointFile`）并通过 `XDG_RUNTIME_DIR`
让子进程写到同一位置，参数里带 `--listen 127.0.0.1:0`；因此它跑的是 Windows 端真正使用的
TCP + token 路径，而不是 Linux 的 Unix 域套接字。

## 环境

- Windows 10 1809+ / Windows 11
- .NET SDK 8+
- Windows App SDK（通过 NuGet 还原，无需单独安装）
- Visual Studio 2022 的「Windows 应用开发」工作负载（推荐）

## 构建

```powershell
cd frontend\windows\Sqarvo.Windows
dotnet build -c Debug -p:Platform=x64
dotnet run -c Debug -p:Platform=x64
```

未打包（unpackaged）模式，需要 Windows App SDK 运行时；也可以改用 MSIX 打包项目。

## 说明

- `Microsoft.WindowsAppSDK` 版本按 NuGet 当前稳定版填写，但**未在本仓库的验证环境（Linux）构建过**。
  首次在 Windows 上构建时请确认目标框架与包版本匹配，并在 PR 中记录实际构建结果。
- `proto/` 中的契约通过 `Grpc.Tools` 在构建时生成 C# 客户端，生成代码不提交；生成发生在
  `Sqarvo.Core`（见上节），`Sqarvo.Windows` 只消费 Core 的封装。
- 禁止引入 WebView2 来实现业务界面；复杂控件优先使用 WinUI 原生控件。
- UI 线程禁止执行同步网络/磁盘/数据库操作；gRPC 调用必须可取消，并处理组件销毁后的回调。
