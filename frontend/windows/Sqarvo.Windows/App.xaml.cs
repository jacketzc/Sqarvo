using Microsoft.UI.Xaml;

namespace Sqarvo.Windows;

/// <summary>
/// 应用入口。当前只创建主窗口，尚未启动后端进程或建立 gRPC 连接。
/// </summary>
public partial class App : Application
{
    private Window? _window;

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _window = new MainWindow();
        _window.Activate();
    }
}
