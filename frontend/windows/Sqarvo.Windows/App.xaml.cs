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
/// <item>读 <c>window.json</c>：读失败回退默认值，不弹窗（见 <see cref="WindowStateStore"/>）。</item>
/// <item>定档位，并写主题偏好——<c>Application.RequestedTheme</c> <b>只能</b>在构造里写一次。</item>
/// <item><c>OnLaunched</c>：把该档位的尺寸写进 <c>Application.Resources</c>，再建窗口。
/// 尺寸资源必须早于第一个窗口的 XAML 解析，否则 <c>StaticResource Size.*</c> 找不到键。</item>
/// </list>
/// </remarks>
public partial class App : Application
{
    private readonly AppOptions _options;
    private readonly WindowState _state;
    private Window? _window;

    public App()
    {
        InitializeComponent();

        _options = AppOptions.Parse(Environment.GetCommandLineArgs());
        if (_options.HasReports)
        {
            ConsoleBridge.Ensure();
        }

        _state = WindowStateStore.Load();

        // 档位：本轮（#162）只认持久化里的值与默认值；按显示器可用性守卫在 #163 落地。
        Metrics.SetTier(Metrics.ParseTier(_state.Tier));

        ThemeService.Initialize(this, _state.ThemePreference);
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // App 构造里读 Application.Resources 会抛 E_UNEXPECTED（WinUI 尚未就绪），
        // 所以尺寸资源只能在这里写，且必须在建窗口之前。
        MetricsResources.Apply(Resources);

        // WinRT 事件订阅在构造里同样会抛 E_UNEXPECTED，第一次订阅放在这里。
        ThemeService.StartListening();

        var window = new MainWindow(_state, _options);
        _window = window;
        window.Activate();

        ApplyProvisionalWindowSize(window);

        // 标题栏与元素级主题必须在窗口存在、内容已解析之后应用一次。
        ThemeService.ApplyToWindow(window);

        if (!_options.HasReports)
        {
            return;
        }

        DiagnosticReporter.Report(window, _options);

        // 诊断开关是「取一行证据」，不是交互模式：打印完就退出。
        window.DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, Close);
    }

    private void Close()
    {
        _window?.Close();
        Exit();
    }

    /// <summary>
    /// 临时窗口尺寸：用规范参考窗口 1200×800（DIP），钳到工作区内。
    /// </summary>
    /// <remarks>
    /// <para>
    /// #163 会用「按逻辑工作区分四档 + 最小窗口收敛」的规则替换掉这里，本单只保证截图与
    /// 验收基线一致。
    /// </para>
    /// <para>
    /// 两条单位事实（spec §8.1）：XAML 的宽高是 DIP；<c>AppWindow.Resize</c> 与
    /// <c>DisplayArea.WorkArea</c> 都是<b>物理像素</b>。所以「选多大」在 DIP 里算，
    /// 喂给 <see cref="Microsoft.UI.Windowing.AppWindow"/> 之前乘一次
    /// <c>XamlRoot.RasterizationScale</c>。
    /// </para>
    /// <para>
    /// <c>Content.XamlRoot</c> 在 <c>Activate()</c> 之后未必立刻可用，拿不到时挂到内容的
    /// <c>Loaded</c> 上再试一次（允许首帧跳动）。
    /// </para>
    /// </remarks>
    private static void ApplyProvisionalWindowSize(Window window)
    {
        void Resize()
        {
            try
            {
                if (window.Content?.XamlRoot is not { } xamlRoot)
                {
                    return;
                }

                var scale = xamlRoot.RasterizationScale;
                var display = Microsoft.UI.Windowing.DisplayArea.GetFromWindowId(
                    window.AppWindow.Id,
                    Microsoft.UI.Windowing.DisplayAreaFallback.Primary);

                var width = Math.Min(
                    (int)Math.Round(Metrics.ReferenceWindowWidth * scale, MidpointRounding.AwayFromZero),
                    display.WorkArea.Width);
                var height = Math.Min(
                    (int)Math.Round(Metrics.ReferenceWindowHeight * scale, MidpointRounding.AwayFromZero),
                    display.WorkArea.Height);

                window.AppWindow.Resize(new global::Windows.Graphics.SizeInt32(width, height));
            }
            catch (Exception error)
            {
                // 尺寸是「好看」的问题，不是正确性问题：失败不该让应用起不来。
                ConsoleBridge.WriteDiagnostic($"[window] 应用初始尺寸失败（{error.GetType().Name}）");
            }
        }

        if (window.Content?.XamlRoot is not null)
        {
            Resize();
            return;
        }

        if (window.Content is FrameworkElement content)
        {
            void OnLoaded(object sender, RoutedEventArgs args)
            {
                content.Loaded -= OnLoaded;
                Resize();
            }

            content.Loaded += OnLoaded;
        }
    }
}