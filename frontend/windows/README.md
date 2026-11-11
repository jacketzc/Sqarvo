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

跟踪 Issue：#162（度量表生成 + 主题三档 + 标题栏 + 未打包持久化）、#163（窗口尺寸 / DPI / 最小尺寸 / 密度档重建）。

## 状态

界面骨架 + 度量与主题自检页。**尚未启动后端、尚未建立 gRPC 连接**；工具条上的入口都还没有真实动作。

六区域外壳（#164）：系统标题栏（`AppWindow`）+ 窗口内菜单行（`MenuBar`）→ 主工具条（`CommandBar`）→
对象树（`TreeView`）→ 工作区（`TabView`，首标签 `Objects` 用 `IsClosable=false` + 固定首位 + 禁拖
等价实现 pinned）→ 自绘双行状态栏 → 信息面板（`SplitView` 右面板）。菜单只有 文件 / 表 / 帮助 三个集合：
文件（新建连接… / 新建查询标签 / 退出）、表（导出 CSV…）、帮助（关于 Squarvo）；未实现的入口全部
禁用并 tooltip「尚未接入后端」。

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

已知边界：`AccessibilitySettings.HighContrastChanged` 在未打包 WinUI 3 应用里订阅会抛
`COMException`，高对比状态改由 `HKCU\Control Panel\Accessibility\HighContrast\Flags` 兜底读取；
系统主题 / 强调色的跟随本身不受影响（那是 WinUI 与 `ThemeResource` 的职责）。

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
- `proto/` 中的契约通过 `Grpc.Tools` 在构建时生成 C# 客户端，生成代码不提交。
- 禁止引入 WebView2 来实现业务界面；复杂控件优先使用 WinUI 原生控件。
- UI 线程禁止执行同步网络/磁盘/数据库操作；gRPC 调用必须可取消，并处理组件销毁后的回调。
