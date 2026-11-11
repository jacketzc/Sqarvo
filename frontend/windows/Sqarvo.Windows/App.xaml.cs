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
using System.Linq;
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
        CrashLog.Trace("app.ctor.begin");
        InitializeComponent();
        CrashLog.Trace("app.ctor.after-init");

        // 未处理异常一律落盘：WinUI 的启动期崩溃在事件日志里只有出错模块，没有托管堆栈。
        // FirstChance：stowed exception 在托管侧没有堆栈，只有「抛出那一刻」能拿到抛点与调用链。
        // 只记录我们正在追的那一类（0x80040111 / LimitedAccessFeatures），否则日志会被噪音淹掉。
        AppDomain.CurrentDomain.FirstChanceException += (_, eventArgs) =>
        {
            var error = eventArgs.Exception;
            var hresult = error.HResult;
            if (hresult == unchecked((int)0x80040111)
                || (error.Message?.Contains("LimitedAccessFeatures", StringComparison.Ordinal) ?? false))
            {
                CrashLog.Write("firstchance", error);
            }
        };

        UnhandledException += (_, eventArgs) => CrashLog.Write("unhandled", eventArgs.Exception);
        AppDomain.CurrentDomain.UnhandledException += (_, eventArgs) =>
            CrashLog.Write("domain", eventArgs.ExceptionObject as Exception);
        TaskScheduler.UnobservedTaskException += (_, eventArgs) =>
        {
            CrashLog.Write("task", eventArgs.Exception);
            eventArgs.SetObserved();
        };

        _options = AppOptions.Parse(Environment.GetCommandLineArgs());
        if (_options.HasReports
            || _options.QaConnect is { Length: > 0 }
            || _options.QaMetadata is { Length: > 0 }
            || _options.QaQuery is { Length: > 0 }
            || _options.NoBackend)
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
        CrashLog.Trace("app.ctor.metrics");

        ThemeService.Initialize(this, _state.ThemePreference);
        CrashLog.Trace("app.ctor.theme");
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
        CrashLog.Trace("app.launch.begin");
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

        CrashLog.Trace("app.launch.window.begin");
        var window = new MainWindow(_state, _options);
        CrashLog.Trace("app.launch.window.done");
        _window = window;

        // 退出前停后端：取消这次关闭，停机完成后再真正关（不阻塞 UI 线程）。
        window.AppWindow.Closing += OnWindowClosing;

        window.Activate();
        CrashLog.Trace("app.launch.activated");

        ThemeService.ApplyToWindow(window);

        // 先试一次：XamlRoot 可能已经可用。第一次 Relayout 会改窗口尺寸，允许一帧跳动。
        window.Relayout(preferSavedGeometry: true);
        CrashLog.Trace("app.launch.relayout");

        // 诊断开关只证明界面结构：不启动后端进程，也不碰凭据库。--no-backend 是同一类隔离手段，
        // 区别是它把窗口留在前台（诊断开关打完就退，没法观察运行期崩溃）。
        // 例外：--qa-metadata 要连真库，它会启动后端；此时即使同时给了 --report-shell 也照常走
        // 「连接 → 展开 → 打印控件树」这条链路（否则后端根本不会启动，探针什么也拿不到）。
        // 两个「要连真库」的 QA 链路（元数据 / 查询）都必须真的把后端拉起来，否则探针什么也拿不到。
        var needsBackendForProbe = _options.QaMetadata is { Length: > 0 } || _options.QaQuery is { Length: > 0 };
        if ((!_options.HasReports || needsBackendForProbe) && !_options.NoBackend)
        {
            // 不 await：窗口先显示，握手进度通过 BackendState 写进状态栏。
            // StartAsync 不抛异常（失败落成 BackendReadiness.Failed）。
            _ = Backend.StartAsync();

            if (_options.QaQuery is { Length: > 0 } qaQuery)
            {
                // 查询链路（#172）：连上之后**真的**建一个查询页、执行 SQL，把 [query] 证据打出来。
                // 与 --qa-signin 合用时不退出（那条开关的语义就是「保持运行供截图」）。
                _ = RunQaQueryAsync(window, qaQuery, keepAlive: _options.QaSignIn is { Length: > 0 });
                return;
            }

            if (_options.QaConnect is { Length: > 0 } qaConnect)
            {
                // QA 链路：不依赖鼠标点界面，直接把「握手 → 测试连接 → 连接 → 断开」跑一遍。
                _ = RunQaConnectAsync(qaConnect, keepAlive: false);
                return;
            }

            if (_options.QaSignIn is { Length: > 0 } qaSignIn)
            {
                // 与 --qa-connect 同一条链路，但不退出也不断开：留给外部截图与手工点检。
                _ = RunQaConnectAsync(qaSignIn, keepAlive: true);
                return;
            }

            if (_options.QaMetadata is { Length: > 0 } qaMetadata)
            {
                // 元数据链路（#171）：连上之后**真的**展开对象树，再把控件树打出来。
                _ = RunQaMetadataAsync(window, qaMetadata);
                return;
            }
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

    /// <summary>
    /// <c>--qa-connect=&lt;名称|@first&gt;</c>：等后端就绪后跑一遍连接链路，打印 <c>[connect]</c> 证据后退出。
    /// </summary>
    /// <param name="target">已保存连接的名称，或 <c>@first</c>（取第一条）。</param>
    /// <remarks>
    /// 后端起不来、条目不存在、测试连接失败都只打印证据，不弹窗——这个开关的读者是验收脚本。
    /// 密码从 <c>SQARVO_QA_PASSWORD</c> 读，先 <c>CredentialService.StoreCredential</c> 写进系统凭据库
    /// 再连接，因此一趟同时验证了 Windows Credential Manager 与真实 gRPC 链路。
    /// </remarks>
    private async Task RunQaConnectAsync(string target, bool keepAlive)
    {
        try
        {
            var session = Backend;
            if (session is null)
            {
                ConsoleBridge.WriteLine("[connect] 没有后端会话");

                return;
            }

            var deadline = DateTimeOffset.UtcNow.AddSeconds(30);
            while (!session.State.IsReady && DateTimeOffset.UtcNow < deadline)
            {
                await Task.Delay(200).ConfigureAwait(true);
            }

            var state = session.State;
            ConsoleBridge.WriteLine(
                $"[connect] backend={state.Readiness} drivers={state.RegisteredDrivers} version={state.Version?.ServiceVersion ?? "?"} message={state.Message}");

            if (!state.IsReady)
            {
                return;
            }

            var profiles = Profiles.Load();
            var name = target.Trim();
            var profile = string.Equals(name, "@first", StringComparison.OrdinalIgnoreCase)
                ? profiles.FirstOrDefault()
                : profiles.FirstOrDefault(candidate => string.Equals(candidate.Name, name, StringComparison.Ordinal));

            if (profile is null)
            {
                ConsoleBridge.WriteLine($"[connect] 找不到连接条目 {name}（已保存 {profiles.Count} 条）");

                return;
            }

            var password = Environment.GetEnvironmentVariable("SQARVO_QA_PASSWORD");
            if (!string.IsNullOrEmpty(password))
            {
                var reference = await session.StoreCredentialAsync(
                    string.IsNullOrEmpty(profile.CredentialRef) ? null : profile.CredentialRef,
                    password).ConfigureAwait(true);

                ConsoleBridge.WriteLine($"[connect] credential_ref={reference}");
                profile = profile with { CredentialRef = reference };

                // 与连接对话框一致：密码写进系统凭据库后，引用要落回配置，否则下次启动又要重输。
                Profiles.Upsert(profile);
                ConsoleBridge.WriteLine("[connect] profile 已回写 connections.json");
            }

            var test = await session.TestConnectionAsync(profile).ConfigureAwait(true);
            ConsoleBridge.WriteLine(
                $"[connect] test reachable={test.Reachable} latency={test.LatencyMillis}ms server={test.ServerVersion} warnings={string.Join("|", test.Warnings)}");

            var open = await session.ConnectAsync(profile).ConfigureAwait(true);
            ConsoleBridge.WriteLine(
                $"[connect] connect driver={open.DriverId} server={open.ServerVersion} session={open.SessionId} capabilities={string.Join("|", open.Capabilities)} sessions={session.OpenConnections.Count}");

            if (keepAlive)
            {
                // 等一拍让事件走完 DispatcherQueue，再报告界面侧看到的状态（排查「连上了但界面没变」）。
                await Task.Delay(1500).ConfigureAwait(true);
                ConsoleBridge.WriteLine(
                    $"[connect] keep-alive sessions={session.OpenConnections.Count} 界面={Backend?.State.ActiveSessions}");
                ConsoleBridge.WriteLine($"[connect] view={(_window as MainWindow)?.Shell.DescribeProfiles() ?? "(无窗口)"}");

                // 截图要看到「展开的库 / 分组 / 表 + Objects 表格 + 面板内容」：给 --qa-signin 配上
                // --qa-catalog 时，走与 --qa-metadata 同一条展开与选择路径（同一条代码，不另写一套）。
                if (_options.QaCatalog is { Length: > 0 } catalog && (_window as MainWindow)?.Shell is { } shell)
                {
                    var expanded = await shell
                        .ProbeMetadataAsync(profile.Key, catalog, CancellationToken.None)
                        .ConfigureAwait(true);

                    ConsoleBridge.WriteLine($"[connect] signin-metadata={(expanded ? "ok" : "incomplete")}");
                }

                return;
            }

            var closed = await session.DisconnectAsync(profile.Key).ConfigureAwait(true);
            ConsoleBridge.WriteLine(
                $"[connect] disconnect closed={closed} sessions={session.OpenConnections.Count}");
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteLine($"[connect] failed {error.GetType().Name}: {error.Message}");
        }
        finally
        {
            // keepAlive（--qa-signin）时窗口留给外部截图与手工点检，不自动退出。
            if (!keepAlive)
            {
                Close();
            }
        }
    }

    /// <summary>
    /// <c>--qa-metadata=&lt;名称|@first&gt;</c>：等后端就绪 → 连接该条目 → 真的展开对象树
    /// （库 → Tables → 第一张表）→ 打印 <c>[metadata]</c> 证据 → 按需打印控件树后退出。
    /// </summary>
    /// <param name="window">主窗口（探针要读它的外壳）。</param>
    /// <param name="target">已保存连接的名称，或 <c>@first</c>。</param>
    /// <remarks>
    /// 与 <see cref="RunQaConnectAsync"/> 同一套取舍：不弹窗、只打印证据；失败也算「跑过一趟」，
    /// 因为验收要看的是「这一趟发生了什么」，而不是进程退出码。
    /// </remarks>
    private async Task RunQaMetadataAsync(MainWindow window, string target)
    {
        try
        {
            var session = Backend;
            if (session is null)
            {
                ConsoleBridge.WriteLine("[metadata] 没有后端会话");
                return;
            }

            var deadline = DateTimeOffset.UtcNow.AddSeconds(30);
            while (!session.State.IsReady && DateTimeOffset.UtcNow < deadline)
            {
                await Task.Delay(200).ConfigureAwait(true);
            }

            var state = session.State;
            ConsoleBridge.WriteLine(
                $"[metadata] backend={state.Readiness} drivers={state.RegisteredDrivers} version={state.Version?.ServiceVersion ?? "?"}");

            if (!state.IsReady)
            {
                return;
            }

            var profiles = Profiles.Load();
            var name = target.Trim();
            var profile = string.Equals(name, "@first", StringComparison.OrdinalIgnoreCase)
                ? profiles.FirstOrDefault()
                : profiles.FirstOrDefault(candidate => string.Equals(candidate.Name, name, StringComparison.Ordinal));

            if (profile is null)
            {
                ConsoleBridge.WriteLine($"[metadata] 找不到连接条目 {name}（已保存 {profiles.Count} 条）");
                return;
            }

            var password = Environment.GetEnvironmentVariable("SQARVO_QA_PASSWORD");
            if (!string.IsNullOrEmpty(password))
            {
                var reference = await session.StoreCredentialAsync(
                    string.IsNullOrEmpty(profile.CredentialRef) ? null : profile.CredentialRef,
                    password).ConfigureAwait(true);
                profile = profile with { CredentialRef = reference };
            }

            var test = await session.TestConnectionAsync(profile).ConfigureAwait(true);
            ConsoleBridge.WriteLine(
                $"[metadata] test reachable={test.Reachable} latency={test.LatencyMillis}ms server={test.ServerVersion}");

            var open = await session.ConnectAsync(profile).ConfigureAwait(true);
            ConsoleBridge.WriteLine(
                $"[metadata] connect driver={open.DriverId} server={open.ServerVersion} session={open.SessionId}");

            // 壳外建立的连接要让树知道（正常路径由 ApplyBackendState 驱动）。
            window.Shell.RefreshFromBackend();

            var complete = await window.Shell
                .ProbeMetadataAsync(profile.Key, _options.QaCatalog, CancellationToken.None)
                .ConfigureAwait(true);

            ConsoleBridge.WriteLine($"[metadata] probe={(complete ? "ok" : "incomplete")}");
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteLine($"[metadata] failed {error.GetType().Name}: {error.Message}");
        }
        finally
        {
            Report(window);
        }
    }

    /// <summary>
    /// <c>--qa-query=&lt;sql&gt;</c>：等后端就绪 → 连接（目标取 <c>--qa-signin</c>，否则第一条）→
    /// **真的**新建查询页并执行 SQL → 打印 <c>[query]</c> 证据 → 关掉查询页（验证 Close）后退出；
    /// 与 <c>--qa-signin</c> 合用时保持运行，供截图。
    /// </summary>
    /// <param name="window">主窗口（查询页挂在它的 <see cref="MainWindow.Shell"/> 上）。</param>
    /// <param name="sql">要执行的 SQL。</param>
    /// <param name="keepAlive">是否保持运行（<c>--qa-signin</c> 同时给出时为真）。</param>
    /// <remarks>
    /// 与 <see cref="RunQaMetadataAsync"/> 同一套取舍：不弹窗、只打印证据；失败也算「跑过一趟」。
    /// 密码同样来自 <c>SQARVO_QA_PASSWORD</c>（可选：已有凭据引用时不必给）。
    /// </remarks>
    private async Task RunQaQueryAsync(MainWindow window, string sql, bool keepAlive)
    {
        try
        {
            var session = Backend;
            if (session is null)
            {
                ConsoleBridge.WriteLine("[query] 没有后端会话");
                return;
            }

            var deadline = DateTimeOffset.UtcNow.AddSeconds(30);
            while (!session.State.IsReady && DateTimeOffset.UtcNow < deadline)
            {
                await Task.Delay(200).ConfigureAwait(true);
            }

            var state = session.State;
            ConsoleBridge.WriteLine(
                $"[query] backend={state.Readiness} drivers={state.RegisteredDrivers} version={state.Version?.ServiceVersion ?? "?"}");

            if (!state.IsReady)
            {
                return;
            }

            var profiles = Profiles.Load();
            var name = string.IsNullOrWhiteSpace(_options.QaSignIn) ? "@first" : _options.QaSignIn!.Trim();
            var profile = string.Equals(name, "@first", StringComparison.OrdinalIgnoreCase)
                ? profiles.FirstOrDefault()
                : profiles.FirstOrDefault(candidate => string.Equals(candidate.Name, name, StringComparison.Ordinal));

            if (profile is null)
            {
                ConsoleBridge.WriteLine($"[query] 找不到连接条目 {name}（已保存 {profiles.Count} 条）");
                return;
            }

            var password = Environment.GetEnvironmentVariable("SQARVO_QA_PASSWORD");
            if (!string.IsNullOrEmpty(password))
            {
                var reference = await session.StoreCredentialAsync(
                    string.IsNullOrEmpty(profile.CredentialRef) ? null : profile.CredentialRef,
                    password).ConfigureAwait(true);
                profile = profile with { CredentialRef = reference };
            }

            var open = await session.ConnectAsync(profile).ConfigureAwait(true);
            ConsoleBridge.WriteLine(
                $"[query] connect driver={open.DriverId} server={open.ServerVersion} session={open.SessionId}");

            // 壳外建立的连接要让「新建查询」的可用性判定看到（正常路径由 ApplyBackendState 驱动）。
            window.Shell.RefreshFromBackend();

            var complete = await window.Shell
                .ProbeQueryAsync(sql, _options.QaQueryPages, CancellationToken.None)
                .ConfigureAwait(true);
            ConsoleBridge.WriteLine($"[query] probe={(complete ? "ok" : "incomplete")}");

            if (keepAlive)
            {
                // 截图要看到「编辑器 + 结果网格 + 状态栏行 2」：窗口留着，不关查询页（游标也还开着）。
                await Task.Delay(500).ConfigureAwait(true);
                ConsoleBridge.WriteLine($"[query] keep-alive tabs={window.Shell.QueryTabCount}");

                if (_options.ReportShell)
                {
                    // 与 --report-shell 合用：这时候打出来的控件树里**有**查询页（编辑器 / 结果行 / 状态行）。
                    ShellTreeDumper.Dump(window);
                }

                return;
            }
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteLine($"[query] failed {error.GetType().Name}: {error.Message}");
        }
        finally
        {
            if (!keepAlive)
            {
                // 顺序要紧：先 Report（控件树里还看得到查询页；它排队的关窗晚于本段同步代码），
                // 再关页取证 Close——与用户点 × 完全同一条路径。
                Report(window);
                var closedTabs = window.Shell.CloseAllQueryTabsForQa();
                ConsoleBridge.WriteLine($"[query] closed-tabs={closedTabs} open-tabs={window.Shell.QueryTabCount}");
            }
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