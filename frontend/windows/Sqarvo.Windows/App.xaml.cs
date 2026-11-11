using Microsoft.UI.Xaml;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Storage;
using Sqarvo.Windows.Theming;
using System;

namespace Sqarvo.Windows;

/// <summary>
/// 应用入口。当前只创建主窗口，尚未启动后端进程或建立 gRPC 连接。
/// </summary>
/// <remarks>
/// 启动顺序是固定的，每一步都依赖前一步：
/// <list type="number">
/// <item><c>InitializeComponent()</c>：加载 App.xaml（含 Tokens.xaml 的三套 ThemeDictionaries）。</item>
/// <item>解析命令行开关 → 需要诊断输出时接上父进程控制台。</item>
/// <item><c>--self-test-window</c>：纯函数断言，跑完即退出，不建窗口。</item>
/// <item>读 <c>window.json</c>：读失败回退默认值，不弹窗（见 <see cref="WindowStateStore"/>）。</item>
/// <item>定档位，并写主题偏好——<c>Application.RequestedTheme</c> <b>只能</b>在构造里写一次。</item>
/// <item><c>OnLaunched</c>：把该档位的尺寸写进 <c>Application.Resources</c>，再建窗口。</item>
/// <item>窗口 <c>Loaded</c> → <c>MainWindow.Relayout()</c>：读 XamlRoot 的缩放与显示器工作区，
/// 算最小尺寸与窗口尺寸（#163）。</item>
/// </list>
/// </remarks>
public partial class App : Application
{
    private readonly AppOptions _options;
    private readonly WindowState _state;
    private Window? _window;
    private bool _reported;

    public App()
    {
        InitializeComponent();

        _options = AppOptions.Parse(Environment.GetCommandLineArgs());
        if (_options.HasReports)
        {
            ConsoleBridge.Ensure();
        }

        _state = WindowStateStore.Load();

        // 自检是纯函数：不需要窗口，也不需要显示器。
        if (_options.SelfTestWindow)
        {
            Environment.ExitCode = DiagnosticReporter.SelfTestWindow() == 0 ? 0 : 1;
            return;
        }

        Metrics.SetTier(Metrics.ParseTier(_state.Tier));

        ThemeService.Initialize(this, _state.ThemePreference);
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        if (_options.SelfTestWindow)
        {
            Exit();
            return;
        }

        // App 构造里读 Application.Resources 会抛 E_UNEXPECTED（WinUI 尚未就绪），
        // 所以尺寸资源只能在这里写，且必须在建窗口之前。
        MetricsResources.Apply(Resources);

        // WinRT 事件订阅在构造里同样会抛 E_UNEXPECTED，第一次订阅放在这里。
        ThemeService.StartListening();

        var window = new MainWindow(_state, _options);
        _window = window;
        window.Activate();

        ThemeService.ApplyToWindow(window);

        // 先试一次：XamlRoot 可能已经可用。第一次 Relayout 会改窗口尺寸，允许一帧跳动。
        window.Relayout(preferSavedGeometry: true);

        if (!_options.HasReports)
        {
            return;
        }

        // 诊断要读「窗口层重算之后」的结果：XamlRoot 没就绪就等 Loaded 再打一次。
        if (window.LastLayout is not null)
        {
            Report(window);
            return;
        }

        if (window.Content is FrameworkElement content)
        {
            void OnLoaded(object sender, RoutedEventArgs eventArgs)
            {
                content.Loaded -= OnLoaded;
                window.Relayout(preferSavedGeometry: true);
                Report(window);
            }

            content.Loaded += OnLoaded;
        }
    }

    private void Report(MainWindow window)
    {
        if (_reported)
        {
            return;
        }

        _reported = true;

        // QA 档位：在这一刻切，窗口层已经算过第一次（切档 → 重算窗口 → 打印，来源标 QA）。
        if (_options.QaTier is { Length: > 0 } qaTier)
        {
            window.SwitchTierForQa(Metrics.ParseTier(qaTier));
        }

        DiagnosticReporter.Report(window, _options, window.LastLayout);

                if (_options.ReportShell)
                {
                    ShellTreeDumper.Dump(window);
                }

        // 诊断开关是「取一行证据」，不是交互模式：打印完就退出。
        window.DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, Close);
    }

    private void Close()
    {
        _window?.Close();
        Exit();
    }
}