# Windows 前端（WinUI 3 + C#）

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
