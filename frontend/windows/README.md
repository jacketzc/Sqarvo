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

界面骨架。**尚未启动后端、尚未建立 gRPC 连接**；主窗口顶部固定显示警告信息。

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
