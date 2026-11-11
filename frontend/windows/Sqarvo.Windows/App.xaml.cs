using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Sqarvo.Core.Clients;
using Sqarvo.Core.Platform;
using Sqarvo.Core.Process;
using Sqarvo.Core.Protocol;
using Sqarvo.Windows.Connections;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Storage;
using Sqarvo.Windows.Theming;
using Sqarvo.Windows.Workspace;
using System;
using System.Diagnostics;
using System.Threading.Tasks;

namespace Sqarvo.Windows;

/// <summary>
/// 应用入口：创建主窗口，并持有**唯一**的后端会话（<see cref="Backend"/>）与连接配置存储
/// （<see cref="Profiles"/>）。
/// </summary>
/// <remarks>
/// <para>
/// 启动顺序（每一步都依赖前一步）：
/// </para>
/// <list type="number">
/// <item><description><c>InitializeComponent()</c>：加载 App.xaml（含 Tokens.xaml 的三套 ThemeDictionaries）。</description></item>
/// <item><description>解析命令行开关 → 需要诊断输出时接上父进程控制台。</description></item>
/// <item><description><c>--self-test-window</c>：纯函数断言，跑完即退出，不建窗口。</description></item>
/// <item><description>读 <c>window.json</c>：读失败回退默认值，不弹窗（见 <see cref="WindowStateStore"/>）。</description></item>
/// <item><description>定档位，并写主题偏好——<c>Application.RequestedTheme</c> <b>只能</b>在构造里写一次。</description></item>
/// <item><description><c>OnLaunched</c>：写尺寸资源 → 建后端会话 → 建窗口 → 窗口 <c>Loaded</c> 重算布局。</description></item>
/// <item><description>窗口可见后**异步**做后端握手（<see cref="BackendSession.StartAsync"/>），
/// 界面先出来，状态栏显示进度；握手失败不弹窗，原因写在状态栏。</description></item>
/// </list>
/// <para>
/// 退出：<c>AppWindow.Closing</c> 里先取消关闭、把后端停掉（最多等 5 秒），再真正关窗。
/// 直接同步阻塞 UI 线程会让窗口在白屏上卡住，而完全不等会留下孤儿后端进程。
/// 超过 5 秒就放弃等待——后端是以 <c>--parent-pid</c> 启动的，进程侧还有最后兜底。
/// </para>
/// </remarks>
public partial class App : Application
{
    /// <summary>退出时等待后端停机的最长时间（超过就放弃等待，交给后端侧的父进程监视兜底）。</summary>
    private static readonly TimeSpan ShutdownTimeout = TimeSpan.FromSeconds(5);

    private static readonly ConnectionStore SharedProfiles = new();

    /// <summary>
    /// 通道选项：普通 RPC 仍用默认 5 秒，数据库握手（<c>TestConnection</c> / <c>Connect</c>）
    /// 放宽到 30 秒。理由：这两个调用要在后端里完成 TCP 连接 + TLS + 认证，比本地 RPC 慢得多；
    /// Linux 客户端对同一对调用也是 30 秒。放宽只影响超时，不改变错误分类
    /// （超时仍然是 <c>DeadlineExceeded</c> → 「连接超时」）。
    /// </summary>
    private static readonly SqarvoChannelOptions DatabaseChannelOptions = new()
    {
        OperationTimeout = TimeSpan.FromSeconds(30),
    };

    private readonly AppOptions _options;
    private readonly WindowState _state;
    private Window? _window;
    private bool _reported;
    private bool _shutdownStarted;

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

    /// <summary>
    /// 应用级后端会话：六区域外壳（<c>ShellView</c>）与连接对话框都只读它的
    /// <see cref="BackendSession.State"/>，不各自维护「是否已连接」。
    /// </summary>
    public static BackendSession? Backend { get; private set; }

    /// <summary>连接配置存储（<c>%LOCALAPPDATA%\Sqarvo\connections.json</c>）。</summary>
    public static ConnectionStore Profiles => SharedProfiles;

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

        // 会话必须在建窗口之前就位：ShellView 构造时直接读 App.Backend（窗口层不传参，
        // 切密度档重建外壳时同样读这一个实例）。
        Backend = new BackendSession(new BackendSupervisor(new BackendSupervisorOptions
        {
            PathLayout = new DefaultPathLayout(),
            ConnectionFactory = endpoint => BackendConnection.Create(endpoint, DatabaseChannelOptions),
        }));

        var window = new MainWindow(_state, _options);
        _window = window;

        // 退出前停后端：取消这次关闭，停机完成后再真正关（不阻塞 UI 线程）。
        window.AppWindow.Closing += OnWindowClosing;

        window.Activate();

        ThemeService.ApplyToWindow(window);

        // 先试一次：XamlRoot 可能已经可用。第一次 Relayout 会改窗口尺寸，允许一帧跳动。
        window.Relayout(preferSavedGeometry: true);

        // 诊断开关只证明界面结构：不启动后端进程，也不碰凭据库。
        if (!_options.HasReports)
        {
            // 不 await：窗口先显示，握手进度通过 BackendState 写进状态栏。
            // StartAsync 不抛异常（失败落成 BackendReadiness.Failed）。
            _ = Backend.StartAsync();
        }

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

    /// <summary>
    /// 关窗先停后端：最多等 <see cref="ShutdownTimeout"/>，然后放行真正的关闭。
    /// </summary>
    private async void OnWindowClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (_shutdownStarted)
        {
            return;
        }

        args.Cancel = true;
        _shutdownStarted = true;

        var backend = Backend;
        if (backend is null)
        {
            _window?.Close();
            return;
        }

        var stopwatch = Stopwatch.StartNew();
        var shutdown = backend.ShutdownAsync();
        var finished = await Task.WhenAny(shutdown, Task.Delay(ShutdownTimeout));

        if (finished != shutdown)
        {
            ConsoleBridge.WriteDiagnostic(
                $"[shutdown] 后端停机超过 {ShutdownTimeout.TotalSeconds:0} 秒未完成，放弃等待（后端侧父进程监视兜底）");
        }
        else
        {
            try
            {
                await shutdown;
                ConsoleBridge.WriteDiagnostic($"[shutdown] 后端停机耗时 {stopwatch.ElapsedMilliseconds} ms");
            }
            catch (Exception error)
            {
                ConsoleBridge.WriteDiagnostic($"[shutdown] 后端停机失败：{error.GetType().Name}: {error.Message}");
            }
        }

        _window?.Close();
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