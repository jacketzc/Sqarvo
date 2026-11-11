using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Layout;
using Sqarvo.Windows.Storage;
using Sqarvo.Windows.Theming;
using Sqarvo.Windows.Views;
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using RectInt32 = global::Windows.Graphics.RectInt32;

namespace Sqarvo.Windows;

/// <summary>
/// 主窗口：只管窗口层（尺寸、DPI、最小尺寸、密度档、标题栏、主题），界面在
/// <see cref="ShellView"/> 的六区域外壳里。
/// </summary>
/// <remarks>
/// <para>
/// 与 #163 相同的三条硬约束：一个重算入口（<see cref="RelayoutCore"/>）、两套像素单位
/// （DIP 计算、物理像素喂 <c>AppWindow</c>）、DPI 只从 <c>XamlRoot</c> 拿。
/// </para>
/// <para>
/// #168 起再加一条：**位置与尺寸变化不重算窗口**。用户拖动窗口得到的矩形就是他要的矩形，
/// 重算会把窗口按推荐尺寸拉回屏幕中央（这是原来最刺眼的一条缺陷）。位置 / 尺寸事件只做两件事：
/// 记下还原边界（<c>_normalRect</c>）、按当前显示器刷新最小尺寸。
/// </para>
/// </remarks>
public sealed partial class MainWindow : Window
{
    /// <summary>几何恢复的容差（物理像素）。</summary>
    private const int GeometryTolerance = 32;

    /// <summary>重算的重入上限（正常路径最多两趟：本趟 + 事件里排进来的那一趟）。</summary>
    private const int MaxRelayoutPasses = 3;

    /// <summary>系统标题栏与度量口径的允许偏差（DIP）：超了就记一行诊断。</summary>
    private const double TitleBarToleranceDips = 2;

    private readonly WindowState _state;
    private readonly AppOptions _options;
    private ShellView _shell;
    private double _lastScale;
    private bool _relayouting;
    private bool _relayoutPending;
    private RelayoutReason _pendingReason;
    private bool _xamlRootHooked;
    private XamlRoot? _xamlRoot;

    /// <summary>启动几何只应用一次：App 与 Loaded 都会请求 Startup，重复应用会让「刚刚还原过」的窗口再跳一次。</summary>
    private bool _startupGeometryApplied;
    private RectInt32 _normalRect;
    private bool _hasNormalRect;
    private double _measuredTitleBarDips;
    private bool _titleBarDeviationLogged;
    private int _appliedMinPhysicalWidth;
    private int _appliedMinPhysicalHeight;

    public MainWindow(WindowState state, AppOptions options)
    {
        CrashLog.Trace("main.ctor.begin");
        ArgumentNullException.ThrowIfNull(state);
        ArgumentNullException.ThrowIfNull(options);

        _state = state;
        _options = options;

        try
        {
            InitializeComponent();
        CrashLog.Trace("main.ctor.after-init");

            _shell = CreateShell();
            ShellHost.Content = _shell;

            if (Content is FrameworkElement root)
            {
                root.ActualThemeChanged += OnActualThemeChanged;
                root.Loaded += OnRootLoaded;
                root.Unloaded += OnRootUnloaded;
            }

            // 内容区尺寸变化是「非客户区高度」的唯一可靠来源（AppWindow.Size 含非客户区）。
            RootGrid.SizeChanged += OnRootSizeChanged;

            RegisterOemTierAccelerators();

            AppWindow.Changed += OnAppWindowChanged;
            ThemeService.SystemAppearanceChanged += OnSystemAppearanceChanged;
            Closed += OnClosed;
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[boot] 主窗口构造失败：{error.GetType().FullName}: {error.Message}");
            throw;
        }
    }

    /// <summary>触发窗口层重算的原因。</summary>
    /// <remarks>
    /// 位置变化与尺寸变化**不在**这个枚举里：它们只更新几何记忆，绝不重算（#168 第 2 条）。
    /// </remarks>
    private enum RelayoutReason
    {
        /// <summary>启动：优先恢复 <c>window.json</c> 的几何，没有记忆才按工作区分档。</summary>
        Startup,

        /// <summary>切密度档：按新档重算推荐尺寸并居中（规范 §2.3 要求窗口跟着放大）。</summary>
        TierChanged,

        /// <summary>DPI / 工作区变化：保留当前 DIP 矩形（只钳制），不改推荐尺寸。</summary>
        DisplayChanged,
    }

    /// <summary>最近一次重算的结果，供 <c>--report-monitor</c> 输出。</summary>
    public WindowLayoutSnapshot? LastLayout { get; private set; }

    /// <summary>六区域外壳（<c>--report-shell</c> 从这里开始枚举控件树）。</summary>
    public ShellView Shell => _shell;

    /// <summary>
    /// 运行期的重算入口（启动与切档共用）：<paramref name="preferSavedGeometry"/> 为真时
    /// 优先恢复 <c>window.json</c> 的几何。
    /// </summary>
    /// <remarks>
    /// 保留这个布尔重载是为了「一个入口」的调用面不分裂：真实分支在 <see cref="RelayoutCore"/>，
    /// 这里只把布尔翻译成原因。
    /// </remarks>
    public void Relayout(bool preferSavedGeometry)
        => Relayout(preferSavedGeometry ? RelayoutReason.Startup : RelayoutReason.TierChanged);

    /// <summary>
    /// 唯一的重算入口：显示器 / DPI / 档位的变化都从这里进。
    /// </summary>
    /// <remarks>
    /// 重入（例如重算途中 XamlRoot 又报了新比例）不会丢：标记 pending 后在本趟结束后再跑一趟，
    /// 但有次数上限——否则「改尺寸 → 事件 → 再改尺寸」会变成死循环。
    /// </remarks>
    private void Relayout(RelayoutReason reason)
    {
        if (_relayouting)
        {
            _relayoutPending = true;
            _pendingReason = reason;
            return;
        }

        if (Content?.XamlRoot is null)
        {
            return;
        }

        _relayouting = true;
        try
        {
            var passes = 0;
            var current = reason;
            while (true)
            {
                _relayoutPending = false;
                RelayoutCore(current);
                passes++;

                if (!_relayoutPending)
                {
                    break;
                }

                if (passes >= MaxRelayoutPasses)
                {
                    ConsoleBridge.WriteDiagnostic($"[layout] 重算连续 {passes} 趟仍未收敛，放弃本轮的后续请求");
                    break;
                }

                current = _pendingReason;
            }
        }
        finally
        {
            _relayouting = false;
        }
    }

    /// <summary>一趟重算：读工作区与比例 → 定档位 → 最小尺寸 → 窗口矩形 → 快照。</summary>
    private void RelayoutCore(RelayoutReason reason)
    {
        if (Content?.XamlRoot is not { } root)
        {
            return;
        }

        try
        {
            var scale = root.RasterizationScale <= 0 ? 1 : root.RasterizationScale;
            _lastScale = scale;

            var display = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Nearest);
            var workPhysical = display.WorkArea;
            var workWidthDips = (int)Math.Floor(WindowSizing.PhysicalToDips(workPhysical.Width, scale));
            var workHeightDips = (int)Math.Floor(WindowSizing.PhysicalToDips(workPhysical.Height, scale));

            var requested = Metrics.ParseTier(_state.Tier);
            var resolved = WindowSizing.ResolveTier(requested, workWidthDips, workHeightDips);
            if (resolved != requested)
            {
                ConsoleBridge.WriteDiagnostic(
                    $"[layout] 档位 {requested} 放不下（工作区 {workWidthDips}×{workHeightDips}），回退到 {resolved}");
            }

            if (resolved != Metrics.Tier)
            {
                RebuildShell(resolved);
            }

            CrashLog.Trace("main.relayout.min-size");
            ApplyMinimumSize(scale, workWidthDips, workHeightDips);

            CrashLog.Trace("main.relayout.geometry.begin");
            var source = ApplyWindowGeometry(reason, scale, workPhysical, workWidthDips, workHeightDips);
            CrashLog.Trace("main.relayout.geometry.done");

            UpdateChromeMeasurements();

            var size = AppWindow.Size;
            LastLayout = new WindowLayoutSnapshot(
                DisplayWidth: GetMonitorSpan(workPhysical, width: true),
                DisplayHeight: GetMonitorSpan(workPhysical, width: false),
                Scale: scale,
                WorkAreaPhysicalWidth: workPhysical.Width,
                WorkAreaPhysicalHeight: workPhysical.Height,
                WorkAreaDipsWidth: workWidthDips,
                WorkAreaDipsHeight: workHeightDips,
                WindowDipsWidth: WindowSizing.RoundInt(WindowSizing.PhysicalToDips(size.Width, scale)),
                WindowDipsHeight: WindowSizing.RoundInt(WindowSizing.PhysicalToDips(size.Height, scale)),
                WindowPhysicalWidth: size.Width,
                WindowPhysicalHeight: size.Height,
                Tier: Metrics.Tier,
                Source: source,
                TitleBarDips: _measuredTitleBarDips,
                MenuRowDips: WindowSizing.MenuRowHeight(Metrics.Tier),
                Maximized: IsMaximized,
                SidebarDips: _shell.SidebarWidth,
                InspectorDips: _shell.InspectorWidth);

            _shell.UpdateTierLabel();
        }
        catch (Exception error)
        {
            // 只记类型+消息不足以定位（WinRT 的 ClassFactory 失败看不到是哪一步），
            // 这里把完整异常（含堆栈）同时写进诊断日志与崩溃文件。
            ConsoleBridge.WriteDiagnostic($"[layout] 重算失败（{error.GetType().Name}: {error.Message}）");
            CrashLog.Write("layout", error);
        }
    }

    /// <summary>按原因决定窗口矩形并应用，返回来源名（诊断行的「来源=」）。</summary>
    private string ApplyWindowGeometry(
        RelayoutReason reason,
        double scale,
        RectInt32 workPhysical,
        int workWidthDips,
        int workHeightDips)
    {
        // 最小化 / 最大化时窗口矩形由系统管着：改它只会得到一帧跳动和一个错位的还原点。
        if (IsMinimized)
        {
            return "最小化";
        }

        if (IsMaximized)
        {
            _startupGeometryApplied = true;
            return "最大化";
        }

        // 启动几何只应用一次（App 的首次调用与 Loaded 会各来一次，第二次不再动窗口）。
        var restoreStartup = reason == RelayoutReason.Startup && !_startupGeometryApplied;
        if (restoreStartup)
        {
            _startupGeometryApplied = true;
        }

        if (restoreStartup && TryRestoreSavedGeometry(scale, out var saved))
        {
            ApplyGeometry(saved);
            if (_state.Maximized && Maximize())
            {
                return "记忆+最大化";
            }

            return "记忆";
        }

        if (reason == RelayoutReason.DisplayChanged
            && TryKeepCurrentGeometry(scale, workPhysical, workWidthDips, workHeightDips, out var kept))
        {
            ApplyGeometry(kept);
            return "保留";
        }

        var (width, height) = WindowSizing.Recommended(Metrics.Tier, workWidthDips, workHeightDips);
        ApplyGeometry(Centered(workPhysical, width, height, scale));
        return reason == RelayoutReason.TierChanged ? "档位" : "分档";
    }

    /// <summary>把矩形喂给 <c>AppWindow</c>（物理像素），并记成还原边界。</summary>
    /// <remarks>
    /// 拒绝退化矩形，并把 WinRT 的 <c>ArgumentException</c> 挡在这里：
    /// <c>MoveAndResize</c> 收到 0 或负尺寸会抛「参数错误」，而它常被事件回调（<c>AppWindow.Changed</c>）
    /// 间接调用——那里没有 try/catch，异常会直接把进程带走（表现就是 0xc000027b 秒退）。
    /// </remarks>
    private void ApplyGeometry(RectInt32 rect)
    {
        if (rect.Width <= 0 || rect.Height <= 0)
        {
            ConsoleBridge.WriteDiagnostic($"[layout] 拒绝应用非法几何 {rect.Width}×{rect.Height}@{rect.X},{rect.Y}");
            return;
        }

        try
        {
            AppWindow.MoveAndResize(rect);
        }
        catch (ArgumentException error)
        {
            ConsoleBridge.WriteDiagnostic($"[layout] 应用几何被拒（{rect.Width}×{rect.Height}@{rect.X},{rect.Y}）：{error.Message}");
            CrashLog.Write("geometry", error);
            return;
        }

        if (!IsMaximized)
        {
            _normalRect = rect;
            _hasNormalRect = true;
        }
    }

    /// <summary>按推荐尺寸在工作区里居中。</summary>
    private static RectInt32 Centered(RectInt32 workPhysical, int widthDips, int heightDips, double scale)
    {
        var physicalWidth = WindowSizing.DipsToPhysical(widthDips, scale);
        var physicalHeight = WindowSizing.DipsToPhysical(heightDips, scale);
        var x = workPhysical.X + Math.Max(0, (workPhysical.Width - physicalWidth) / 2);
        var y = workPhysical.Y + Math.Max(0, (workPhysical.Height - physicalHeight) / 2);
        return new RectInt32(x, y, physicalWidth, physicalHeight);
    }

    /// <summary>
    /// 保留当前窗口的 DIP 矩形（DPI / 工作区变化时用）：尺寸收到 [最小窗口, 工作区 - 边距]，
    /// 位置收进工作区。**不**用推荐尺寸——那等于把用户摆好的窗口再居一次。
    /// </summary>
    private bool TryKeepCurrentGeometry(double scale, RectInt32 workPhysical, int workWidthDips, int workHeightDips, out RectInt32 rect)
    {
        rect = default;

        var size = AppWindow.Size;
        var position = AppWindow.Position;
        if (size.Width <= 0 || size.Height <= 0)
        {
            return false;
        }

        var (minWidth, minHeight) = WindowSizing.MinimumWindow(Metrics.Tier, workWidthDips, workHeightDips);
        var maxWidth = Math.Max(minWidth, workWidthDips - WindowSizing.WidthMargin);
        var maxHeight = Math.Max(minHeight, workHeightDips - WindowSizing.HeightMargin);

        var widthDips = Math.Clamp(WindowSizing.RoundInt(WindowSizing.PhysicalToDips(size.Width, scale)), minWidth, maxWidth);
        var heightDips = Math.Clamp(WindowSizing.RoundInt(WindowSizing.PhysicalToDips(size.Height, scale)), minHeight, maxHeight);

        var physicalWidth = WindowSizing.DipsToPhysical(widthDips, scale);
        var physicalHeight = WindowSizing.DipsToPhysical(heightDips, scale);

        var x = Math.Clamp(position.X, workPhysical.X, workPhysical.X + Math.Max(0, workPhysical.Width - physicalWidth));
        var y = Math.Clamp(position.Y, workPhysical.Y, workPhysical.Y + Math.Max(0, workPhysical.Height - physicalHeight));

        rect = new RectInt32(x, y, physicalWidth, physicalHeight);
        return true;
    }

    /// <summary>最小尺寸走 <c>OverlappedPresenter.PreferredMinimum*</c>（物理像素、含边框）。</summary>
    private void ApplyMinimumSize(double scale, int workWidthDips, int workHeightDips)
    {
        if (AppWindow.Presenter is not OverlappedPresenter presenter)
        {
            return;
        }

        CrashLog.Trace($"main.min-size.presenter-ok tier={Metrics.Tier}");

        var (minWidth, minHeight) = WindowSizing.MinimumWindow(Metrics.Tier, workWidthDips, workHeightDips);
        var physicalWidth = WindowSizing.DipsToPhysical(minWidth, scale);
        var physicalHeight = WindowSizing.DipsToPhysical(minHeight, scale);

        CrashLog.Trace($"main.min-size.values {minWidth}x{minHeight} -> {physicalWidth}x{physicalHeight}");

        // 拖窗口时 Changed 会连发几十次：值没变就不要碰 presenter（每次写都会触发一轮布局）。
        if (physicalWidth == _appliedMinPhysicalWidth && physicalHeight == _appliedMinPhysicalHeight)
        {
            return;
        }

        CrashLog.Trace("main.min-size.set-width");
        presenter.PreferredMinimumWidth = physicalWidth;
        CrashLog.Trace("main.min-size.set-height");
        presenter.PreferredMinimumHeight = physicalHeight;
        CrashLog.Trace("main.min-size.done");
        _appliedMinPhysicalWidth = physicalWidth;
        _appliedMinPhysicalHeight = physicalHeight;
    }

    /// <summary>
    /// 把记住的几何换算成可用的窗口矩形；落在任何显示器之外或读不到显示器信息时一律放弃。
    /// </summary>
    /// <remarks>
    /// WinRT 的 <c>IReadOnlyList&lt;DisplayArea&gt;</c> 在部分会话里连 <c>GetEnumerator()</c> 都会抛
    /// <c>InvalidCastException: ClassFactory 无法供应请求的类</c>，所以**枚举整段**都要兜住，
    /// 而不是只 try 住 <c>FindAll()</c>：上一版正是因为异常发生在 <c>foreach</c> 里，
    /// Relayout 半途炸掉，随后在 <c>ApplyWindowGeometry</c> 里连锁崩成 0xc000027b。
    /// 几何记忆只是便利功能，读不到就当「没有记忆」，绝不能让布局失败。
    /// </remarks>
    private bool TryRestoreSavedGeometry(double scale, out RectInt32 rect)
    {
        rect = default;

        if (!_state.HasGeometry || _state.WindowX is not { } x || _state.WindowY is not { } y)
        {
            return false;
        }

        var width = _state.WindowWidth!.Value;
        var height = _state.WindowHeight!.Value;

        try
        {
            var areas = DisplayArea.FindAll();

            // 索引用 Count/index 而不是 foreach：GetEnumerator() 本身就是要兜住的那一步。
            for (var index = 0; index < areas.Count; index++)
            {
                var work = areas[index].WorkArea;
                var insideX = x >= work.X - GeometryTolerance && x <= work.X + work.Width - GeometryTolerance;
                var insideY = y >= work.Y - GeometryTolerance && y <= work.Y + work.Height - GeometryTolerance;
                if (!insideX || !insideY)
                {
                    continue;
                }

                // 记忆尺寸也要过一遍窗口气的规则：旧档位 / 旧显示器上存的尺寸可能比当前工作区还大。
                var areaWidthDips = (int)Math.Floor(WindowSizing.PhysicalToDips(work.Width, scale));
                var areaHeightDips = (int)Math.Floor(WindowSizing.PhysicalToDips(work.Height, scale));
                var (minWidth, minHeight) = WindowSizing.MinimumWindow(Metrics.Tier, areaWidthDips, areaHeightDips);
                var clampedWidth = Math.Clamp(width, minWidth, Math.Max(minWidth, areaWidthDips - WindowSizing.WidthMargin));
                var clampedHeight = Math.Clamp(height, minHeight, Math.Max(minHeight, areaHeightDips - WindowSizing.HeightMargin));

                if (clampedWidth != width || clampedHeight != height)
                {
                    ConsoleBridge.WriteDiagnostic($"[layout] 记忆尺寸 {width}×{height} 收到工作区允许的 {clampedWidth}×{clampedHeight}");
                }

                rect = new RectInt32(
                    x,
                    y,
                    WindowSizing.DipsToPhysical(clampedWidth, scale),
                    WindowSizing.DipsToPhysical(clampedHeight, scale));
                return true;
            }
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[layout] 读取显示器信息失败（{error.GetType().Name}），丢弃几何记忆");
            CrashLog.Write("display", error);
            rect = default;
            return false;
        }

        ConsoleBridge.WriteDiagnostic($"[layout] 保存的几何 ({x},{y}) 已不在任何显示器工作区内，丢弃并改用分档推荐");
        return false;
    }

    /// <summary>切换密度档：重建外壳 → 重算窗口（StaticResource 不会自己重算）。</summary>
    private void SwitchTier(DensityTier tier)
    {
        if (tier == Metrics.Tier)
        {
            return;
        }

        SnapshotWorkArea(out var workWidth, out var workHeight);
        if (!WindowSizing.TierAvailable(tier, workWidth, workHeight))
        {
            _shell.NotifyNotImplemented(
                $"当前显示器放不下 {TierLabelOf(tier)}（需要 {WindowSizing.MinWindowWidth(tier)}×{WindowSizing.MinWindowHeight(tier)}）");
            return;
        }

        RebuildShell(tier);
        _state.Tier = Metrics.TierId;
        Relayout(RelayoutReason.TierChanged);

        // 重算之后再写盘：这时 _normalRect 已经是新档位的窗口矩形，
        // 切档后的几何与档位落在同一次写入里（契约 §1.5 ⑤）。
        CaptureGeometry();
        WindowStateStore.Save(_state);
    }

    /// <summary>
    /// QA 专用：在运行时切一次档位并把来源标成 <c>QA</c>（<c>--qa-tier=&lt;id&gt;</c>）。
    /// </summary>
    /// <remarks>
    /// 为什么需要它：切档的真实入口是键盘快捷键，自动化里很难稳定注入；
    /// 而「切档后窗口跟着改大小」这条必须能被重复复现。这个入口只做与快捷键完全相同的事
    /// （<see cref="SwitchTier"/>），然后把来源标注改掉，不引入第二套逻辑。
    /// </remarks>
    public void SwitchTierForQa(DensityTier tier)
    {
        SwitchTier(tier);

        if (LastLayout is { } layout)
        {
            LastLayout = layout with { Source = "QA" };
        }
    }

    /// <summary>整棵内容树重建：新外壳实例 + 重写的尺寸资源 + 回放状态。</summary>
    private void RebuildShell(DensityTier tier)
    {
        var snapshot = _shell.CaptureSnapshot();

        Metrics.SetTier(tier);

        if (Application.Current is { } application)
        {
            // 先清旧档位的 Size.* 键：残留键会被新页面里已解析的 StaticResource 命中，
            // 症状是「切了档但某一个尺寸没变」。清完再写新档位。
            MetricsResources.Clear(application.Resources);
            MetricsResources.Apply(application.Resources);
        }

        var rebuilt = CreateShell();
        ShellHost.Content = rebuilt;
        _shell = rebuilt;
        rebuilt.RestoreSnapshot(snapshot);
    }

    /// <summary>建一个外壳实例并把两个出口接上（窗口层是唯一负责持久化的一层）。</summary>
    private ShellView CreateShell()
    {
        var shell = new ShellView(_state);
        shell.QuitRequested += (_, _) => Close();
        shell.PanelWidthsChanged += OnPanelWidthsChanged;
        return shell;
    }

    /// <summary>
    /// 侧栏 / 信息面板拖动结束：归一化写回 <c>window.json</c>，再按档位换算回来校验一次。
    /// </summary>
    /// <remarks>
    /// 归一化用**档位**系数（<c>Metrics.*Scale</c>），不是系统 DPI（spec §7 规则 1）。
    /// 写完用 <see cref="WindowState.SidebarWidthFor"/> / <see cref="WindowState.InspectorWidthFor"/>
    /// 把值换回当前档位再落到界面上：界面上显示的就是下次启动会恢复的值，避免「存 630、
    /// 恢复 520」这种看起来像设置被吃掉的差异。
    /// </remarks>
    private void OnPanelWidthsChanged(object? sender, EventArgs args)
    {
        var tier = Metrics.Tier;
        var scale = WindowSizing.ScaleOf(tier);

        _state.LastSidebarWidth = WindowState.Normalize(_shell.SidebarWidth, scale);
        _state.LastInspectorWidth = WindowState.Normalize(_shell.InspectorWidth, scale);

        var sidebar = _state.SidebarWidthFor(tier);
        var inspector = _state.InspectorWidthFor(tier);
        _shell.ApplyPanelWidths(sidebar, inspector);

        // 顺手把当前窗口矩形一起带上：这次写盘不是关闭时那次，几何不能停在上一个会话。
        CaptureGeometry();
        WindowStateStore.Save(_state);

        ConsoleBridge.WriteDiagnostic(
            $"[panel] 侧栏 {sidebar} → 归一 {_state.LastSidebarWidth}；信息面板 {inspector} → 归一 {_state.LastInspectorWidth}（档位 {Metrics.TierId}）");
    }

    private void SnapshotWorkArea(out int workWidthDips, out int workHeightDips)
    {
        var scale = Content?.XamlRoot?.RasterizationScale ?? _lastScale;
        var work = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Nearest).WorkArea;
        workWidthDips = (int)Math.Floor(WindowSizing.PhysicalToDips(work.Width, scale <= 0 ? 1 : scale));
        workHeightDips = (int)Math.Floor(WindowSizing.PhysicalToDips(work.Height, scale <= 0 ? 1 : scale));
    }

    /// <summary>
    /// 补上主键盘的 Ctrl+= / Ctrl+-（OEM 键 187 / 189）。
    /// </summary>
    /// <remarks>
    /// VirtualKey 里只有数字键盘的 Add / Subtract 有名字，OEM 键只能用数值构造；
    /// XAML 里写 Key="187" 会在解析时失败（枚举无此成员），所以在 code-behind 注册。
    /// </remarks>
    private void RegisterOemTierAccelerators()
    {
        var plus = new KeyboardAccelerator
        {
            Key = (global::Windows.System.VirtualKey)187,
            Modifiers = global::Windows.System.VirtualKeyModifiers.Control,
        };
        plus.Invoked += OnTierUp;
        RootGrid.KeyboardAccelerators.Add(plus);

        var minus = new KeyboardAccelerator
        {
            Key = (global::Windows.System.VirtualKey)189,
            Modifiers = global::Windows.System.VirtualKeyModifiers.Control,
        };
        minus.Invoked += OnTierDown;
        RootGrid.KeyboardAccelerators.Add(minus);
    }

    private void OnRootLoaded(object sender, RoutedEventArgs args)
    {
        HookXamlRoot();
        Relayout(RelayoutReason.Startup);
    }

    private void OnRootUnloaded(object sender, RoutedEventArgs args) => UnhookXamlRoot();

    /// <summary>订阅 <c>XamlRoot.Changed</c>：DPI 只能从这里拿（<c>DisplayInformation</c> 在 WinUI 3 抛异常）。</summary>
    private void HookXamlRoot()
    {
        if (_xamlRootHooked || Content?.XamlRoot is not { } root)
        {
            return;
        }

        _xamlRoot = root;
        root.Changed += OnXamlRootChanged;
        _xamlRootHooked = true;
    }

    /// <summary>退订（幂等）：外壳重建不换 XamlRoot，所以只在窗口卸载 / 关闭时调用。</summary>
    private void UnhookXamlRoot()
    {
        if (!_xamlRootHooked)
        {
            return;
        }

        if (_xamlRoot is { } root)
        {
            root.Changed -= OnXamlRootChanged;
        }

        _xamlRoot = null;
        _xamlRootHooked = false;
    }

    private void OnXamlRootChanged(XamlRoot sender, XamlRootChangedEventArgs args)
    {
        var scale = sender.RasterizationScale <= 0 ? 1 : sender.RasterizationScale;

        if (Math.Abs(scale - _lastScale) > 0.001)
        {
            // DPI / 缩放档变化：与窗口重算走同一个入口。原因标成 DisplayChanged，
            // 因此保留当前 DIP 矩形、不居中、不抢推荐尺寸。
            ConsoleBridge.WriteDiagnostic($"[layout] 栅格化比例 {_lastScale:0.##} → {scale:0.##}，重算窗口层");
            Relayout(RelayoutReason.DisplayChanged);
            return;
        }

        // 纯尺寸变化（不是 DPI）：只刷新最小尺寸与非客户区测量，几何归 AppWindow.Changed 记账。
        RefreshMinimumSize();
        UpdateChromeMeasurements();
    }

    private void OnRootSizeChanged(object sender, SizeChangedEventArgs args)
        => UpdateChromeMeasurements(forceLayout: false);

    /// <summary>
    /// 窗口位置 / 尺寸变化。
    /// </summary>
    /// <remarks>
    /// 两类事件都**不**重算窗口：位置变化重算会把窗口拉回屏幕中央（P0 体验缺陷），
    /// 尺寸变化重算会把用户刚拖出来的尺寸抢回推荐值。这里只做记账与最小尺寸刷新。
    /// </remarks>
    private void OnAppWindowChanged(AppWindow sender, AppWindowChangedEventArgs args)
    {
        if (args.DidPositionChange)
        {
            RememberWindowGeometry();

            // 可能整窗跨到另一块显示器了：最小尺寸按新的工作区刷新，但**不动**几何。
            RefreshMinimumSize();
        }

        if (args.DidSizeChange)
        {
            RememberWindowGeometry();
            UpdateChromeMeasurements();
        }
    }

    /// <summary>记下非最大化时的矩形（= 最大化后的还原边界）；最大化 / 最小化尺寸不算。</summary>
    private void RememberWindowGeometry()
    {
        if (IsMaximized || IsMinimized)
        {
            return;
        }

        var size = AppWindow.Size;
        if (size.Width <= 0 || size.Height <= 0)
        {
            return;
        }

        var position = AppWindow.Position;
        _normalRect = new RectInt32(position.X, position.Y, size.Width, size.Height);
        _hasNormalRect = true;
    }

    /// <summary>只刷新最小尺寸（位置变化路径用：不动窗口矩形，因此不会「抢回」位置）。</summary>
    private void RefreshMinimumSize()
    {
        if (Content?.XamlRoot is not { } root)
        {
            return;
        }

        var scale = root.RasterizationScale <= 0 ? 1 : root.RasterizationScale;
        try
        {
            var work = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Nearest).WorkArea;
            var workWidthDips = (int)Math.Floor(WindowSizing.PhysicalToDips(work.Width, scale));
            var workHeightDips = (int)Math.Floor(WindowSizing.PhysicalToDips(work.Height, scale));
            CrashLog.Trace("main.relayout.min-size");
            ApplyMinimumSize(scale, workWidthDips, workHeightDips);
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[layout] 刷新最小尺寸失败（{error.GetType().Name}: {error.Message}）");
        }
    }

    /// <summary>
    /// 测量非客户区（系统标题栏）高度并记一次偏差诊断。
    /// </summary>
    /// <remarks>
    /// 这是「不再靠硬编码 40」的落点：菜单行高度由度量推出（<see cref="WindowSizing.MenuRowHeight"/>），
    /// 而系统标题栏的真实高度是**平台事实**，只能量。两者不一致时写一行诊断并如实报出外壳总高，
    /// 不为了让总高等于度量去裁剪系统标题栏（那是系统的区域，裁了会切到窗口按钮）。
    /// </remarks>
    private void UpdateChromeMeasurements(bool forceLayout = true)
    {
        // 比例只从 XamlRoot 拿；还没拿到（窗口未布局）就先不测，否则量出来的是「1x 下的高度」。
        var scale = Content?.XamlRoot?.RasterizationScale ?? _lastScale;
        if (scale <= 0)
        {
            return;
        }

        var measured = MeasureTitleBarDips(scale, forceLayout);
        if (measured <= 0)
        {
            return;
        }

        _measuredTitleBarDips = measured;

        if (_titleBarDeviationLogged || Math.Abs(measured - WindowSizing.TitleBarBaseDips) <= TitleBarToleranceDips)
        {
            return;
        }

        _titleBarDeviationLogged = true;
        var menuRow = WindowSizing.MenuRowHeight(Metrics.Tier);
        ConsoleBridge.WriteDiagnostic(
            $"[chrome] 系统标题栏实测 {measured:0.#} DIP，度量口径 {WindowSizing.TitleBarBaseDips} DIP：" +
            $"菜单行 {menuRow} DIP、外壳总高 {(measured + menuRow):0.#} DIP（不裁剪系统区域）");
    }

    /// <summary>实测非客户区高度（DIP）：窗口物理高 ÷ 比例 − 内容实际高。</summary>
    /// <remarks>
    /// <paramref name="forceLayout"/> 只在「布局回调之外」为真：在 SizeChanged 里再调
    /// <c>UpdateLayout()</c> 会重入布局，所以那条路径用当前已算好的 ActualHeight。
    /// </remarks>
    private double MeasureTitleBarDips(double scale, bool forceLayout)
    {
        try
        {
            if (forceLayout)
            {
                // 强制一次同步布局：否则读到的 ActualHeight 是上一帧的，减法会得到 0 或负数。
                RootGrid.UpdateLayout();
            }

            return WindowSizing.MeasuredNonClientDips(AppWindow.Size.Height, scale, RootGrid.ActualHeight);
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[chrome] 非客户区高度测量失败（{error.GetType().Name}）");
            return 0;
        }
    }

    /// <summary>最大化（首次恢复记忆里的「最大化」也走这里）。</summary>
    private bool Maximize()
    {
        if (AppWindow.Presenter is not OverlappedPresenter presenter)
        {
            return false;
        }

        presenter.Maximize();
        return true;
    }

    private bool IsMaximized => AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Maximized };

    private bool IsMinimized => AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized };

    private void OnSystemAppearanceChanged(object? sender, EventArgs e)
        => DispatcherQueue.TryEnqueue(() => ThemeService.ApplyToWindow(this));

    private void OnActualThemeChanged(FrameworkElement sender, object args)
        => ThemeService.ApplyTitleBar(this);

    private void OnTierReset(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        SwitchTier(DensityTier.Compact);
    }

    private void OnTierUp(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        if (Metrics.Tier < DensityTier.Large)
        {
            SwitchTier((DensityTier)((int)Metrics.Tier + 1));
        }
    }

    private void OnTierDown(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        if (Metrics.Tier > DensityTier.Compact)
        {
            SwitchTier((DensityTier)((int)Metrics.Tier - 1));
        }
    }

    private void OnCloseTab(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        var tab = _shell.WorkspaceTabCount;
        _shell.NotifyNotImplemented(tab <= 1 ? "Ctrl+W：只有固定的 Objects 页，没有可关闭的标签" : "Ctrl+W 关闭标签");
    }

    private void OnQuit(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        Close();
    }

    private void OnNewConnection(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _shell.NotifyNotImplemented("Ctrl+N 新建连接");
    }

    private void OnNewQueryTab(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _shell.NotifyNotImplemented("Ctrl+T 新建查询标签");
    }

    private void OnClosed(object sender, WindowEventArgs args)
    {
        ThemeService.SystemAppearanceChanged -= OnSystemAppearanceChanged;

        if (_xamlRootHooked)
        {
            UnhookXamlRoot();
        }

        if (_options.HasReports)
        {
            return;
        }

        SaveGeometry();
        _state.Tier = Metrics.TierId;
        _state.Theme = ThemeService.PreferenceName.ToLowerInvariant();
        WindowStateStore.Save(_state);
    }

    /// <summary>
    /// 关窗时记几何：尺寸 / 位置写**还原边界**（非最大化矩形），并按当前是否最大化写
    /// <see cref="WindowState.Maximized"/>。最大化时 <c>AppWindow.Size</c> 是屏幕大小，
    /// 拿它当记忆会在下次启动得到一个贴满屏幕的普通窗口。
    /// </summary>
    private void SaveGeometry()
    {
        CaptureGeometry();

        ConsoleBridge.WriteDiagnostic(
            $"[layout] 写回几何 {_state.WindowWidth}×{_state.WindowHeight}@({_state.WindowX},{_state.WindowY}) 最大化={_state.Maximized}");
    }

    /// <summary>
    /// 把当前窗口矩形搬进 <see cref="WindowState"/>（不写盘）。
    /// </summary>
    /// <remarks>
    /// 单独拆出来是因为「面板拖动后立刻写盘」那条路径也要带上几何：否则一次拖动会把
    /// 上一会话的窗口矩形再写一遍，进程崩溃后窗口位置就退回上一会话的值。
    /// </remarks>
    private void CaptureGeometry()
    {
        var scale = _lastScale <= 0 ? 1 : _lastScale;

        if (!IsMaximized && !IsMinimized)
        {
            RememberWindowGeometry();
        }

        if (_hasNormalRect && _normalRect.Width > 0 && _normalRect.Height > 0)
        {
            _state.WindowWidth = WindowSizing.RoundInt(WindowSizing.PhysicalToDips(_normalRect.Width, scale));
            _state.WindowHeight = WindowSizing.RoundInt(WindowSizing.PhysicalToDips(_normalRect.Height, scale));
            _state.WindowX = _normalRect.X;
            _state.WindowY = _normalRect.Y;
        }

        _state.Maximized = IsMaximized;
    }

    private static string TierLabelOf(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableLabel,
        DensityTier.Large => Metrics.LargeLabel,
        _ => Metrics.CompactLabel,
    };

    /// <summary>显示器某一边（物理像素；只用于诊断输出，取不到就退回工作区尺寸）。</summary>
    private static int GetMonitorSpan(RectInt32 workArea, bool width)
    {
        try
        {
            var info = new MonitorInfo { cbSize = Marshal.SizeOf<MonitorInfo>() };
            var monitor = MonitorFromPoint(new Point { X = workArea.X + 1, Y = workArea.Y + 1 }, 2);
            if (monitor != IntPtr.Zero && GetMonitorInfo(monitor, ref info))
            {
                return width ? info.rcMonitor.Right - info.rcMonitor.Left : info.rcMonitor.Bottom - info.rcMonitor.Top;
            }
        }
        catch (Exception)
        {
            // 忽略：诊断数字缺失比编一个数字诚实。
        }

        return width ? workArea.Width : workArea.Height;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Point
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Rect32
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MonitorInfo
    {
        public int cbSize;
        public Rect32 rcMonitor;
        public Rect32 rcWork;
        public int dwFlags;
    }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr MonitorFromPoint(Point pt, uint flags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);
}

/// <summary>一次窗口层重算的结果（诊断输出用）。</summary>
public sealed record WindowLayoutSnapshot(
    int DisplayWidth,
    int DisplayHeight,
    double Scale,
    int WorkAreaPhysicalWidth,
    int WorkAreaPhysicalHeight,
    int WorkAreaDipsWidth,
    int WorkAreaDipsHeight,
    int WindowDipsWidth,
    int WindowDipsHeight,
    int WindowPhysicalWidth,
    int WindowPhysicalHeight,
    DensityTier Tier,
    string Source,
    double TitleBarDips,
    int MenuRowDips,
    bool Maximized,
    int SidebarDips,
    int InspectorDips);