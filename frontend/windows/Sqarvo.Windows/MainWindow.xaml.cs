using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Layout;
using Sqarvo.Windows.Storage;
using Sqarvo.Windows.Theming;
using Sqarvo.Windows.Views;
using System;
using System.Runtime.InteropServices;

namespace Sqarvo.Windows;

/// <summary>
/// 主窗口：只管窗口层（尺寸、DPI、最小尺寸、密度档、标题栏、主题），界面在
/// <see cref="ShellView"/> 的六区域外壳里。
/// </summary>
/// <remarks>
/// 与 #163 相同的三条硬约束：一个 <see cref="Relayout"/> 入口、两套像素单位（DIP 计算、
/// 物理像素喂 <c>AppWindow</c>）、DPI 只从 <c>XamlRoot</c> 拿。
/// </remarks>
public sealed partial class MainWindow : Window
{
    /// <summary>几何恢复的容差（物理像素）。</summary>
    private const int GeometryTolerance = 32;

    private readonly WindowState _state;
    private readonly AppOptions _options;
    private ShellView _shell;
    private double _lastScale;
    private bool _relayouting;

    public MainWindow(WindowState state, AppOptions options)
    {
        ArgumentNullException.ThrowIfNull(state);
        ArgumentNullException.ThrowIfNull(options);

        _state = state;
        _options = options;

        try
        {
            InitializeComponent();

            _shell = new ShellView();
            _shell.QuitRequested += (_, _) => Close();
            ShellHost.Content = _shell;

            if (Content is FrameworkElement root)
            {
                root.ActualThemeChanged += OnActualThemeChanged;
                root.Loaded += OnRootLoaded;
            }

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

    /// <summary>最近一次 <see cref="Relayout"/> 的结果，供 <c>--report-monitor</c> 输出。</summary>
    public WindowLayoutSnapshot? LastLayout { get; private set; }

    /// <summary>六区域外壳（<c>--report-shell</c> 从这里开始枚举控件树）。</summary>
    public ShellView Shell => _shell;

    /// <summary>窗口层重算：显示器工作区 / 栅格化比例 / 档位 → 最小尺寸 + 窗口尺寸。</summary>
    public void Relayout(bool preferSavedGeometry)
    {
        if (_relayouting || Content?.XamlRoot is not { } root)
        {
            return;
        }

        _relayouting = true;
        try
        {
            var scale = root.RasterizationScale;
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

            ApplyMinimumSize(scale, workWidthDips, workHeightDips);

            string source;
            if (preferSavedGeometry && TryRestoreSavedGeometry(scale, out var saved))
            {
                AppWindow.MoveAndResize(saved);
                source = "记忆";
            }
            else
            {
                var (width, height) = WindowSizing.Recommended(Metrics.Tier, workWidthDips, workHeightDips);
                var physicalWidth = WindowSizing.DipsToPhysical(width, scale);
                var physicalHeight = WindowSizing.DipsToPhysical(height, scale);
                var x = workPhysical.X + Math.Max(0, (workPhysical.Width - physicalWidth) / 2);
                var y = workPhysical.Y + Math.Max(0, (workPhysical.Height - physicalHeight) / 2);
                AppWindow.MoveAndResize(new global::Windows.Graphics.RectInt32(x, y, physicalWidth, physicalHeight));
                source = "分档";
            }

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
                Source: source);

            _shell.UpdateTierLabel();
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[layout] 重算失败（{error.GetType().Name}: {error.Message}）");
        }
        finally
        {
            _relayouting = false;
        }
    }

    private void ApplyMinimumSize(double scale, int workWidthDips, int workHeightDips)
    {
        if (AppWindow.Presenter is not OverlappedPresenter presenter)
        {
            return;
        }

        var (minWidth, minHeight) = WindowSizing.MinimumWindow(Metrics.Tier, workWidthDips, workHeightDips);
        presenter.PreferredMinimumWidth = WindowSizing.DipsToPhysical(minWidth, scale);
        presenter.PreferredMinimumHeight = WindowSizing.DipsToPhysical(minHeight, scale);
    }

    private bool TryRestoreSavedGeometry(double scale, out global::Windows.Graphics.RectInt32 rect)
    {
        rect = default;

        if (!_state.HasGeometry || _state.WindowX is not { } x || _state.WindowY is not { } y)
        {
            return false;
        }

        var width = WindowSizing.DipsToPhysical(_state.WindowWidth!.Value, scale);
        var height = WindowSizing.DipsToPhysical(_state.WindowHeight!.Value, scale);

        // DisplayArea.FindAll() 在某些会话里会抛 InvalidCastException（ClassFactory 无法供应请求的类）；
        // 几何记忆是便利功能，读不到就当「没有记忆」，绝不能让 Relayout 整个失败。
        global::System.Collections.Generic.IReadOnlyList<DisplayArea> areas;
        try
        {
            areas = DisplayArea.FindAll();
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[layout] 枚举显示器失败（{error.GetType().Name}），丢弃几何记忆");
            return false;
        }

        foreach (var area in areas)
        {
            var work = area.WorkArea;
            var insideX = x >= work.X - GeometryTolerance && x <= work.X + work.Width - GeometryTolerance;
            var insideY = y >= work.Y - GeometryTolerance && y <= work.Y + work.Height - GeometryTolerance;
            if (insideX && insideY && width > 0 && height > 0)
            {
                rect = new global::Windows.Graphics.RectInt32(x, y, width, height);
                return true;
            }
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
        WindowStateStore.Save(_state);
        Relayout(preferSavedGeometry: false);
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
            MetricsResources.Apply(application.Resources);
        }

        var rebuilt = new ShellView();
        rebuilt.QuitRequested += (_, _) => Close();
        ShellHost.Content = rebuilt;
        _shell = rebuilt;
        rebuilt.RestoreSnapshot(snapshot);
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
        Relayout(preferSavedGeometry: true);
    }

    private void OnAppWindowChanged(AppWindow sender, AppWindowChangedEventArgs args)
    {
        if (args.DidPositionChange)
        {
            Relayout(preferSavedGeometry: false);
        }
    }

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

        if (_options.HasReports)
        {
            return;
        }

        SaveGeometry();
        _state.Tier = Metrics.TierId;
        _state.Theme = ThemeService.PreferenceName.ToLowerInvariant();
        WindowStateStore.Save(_state);
    }

    private void SaveGeometry()
    {
        var scale = _lastScale <= 0 ? 1 : _lastScale;
        var size = AppWindow.Size;
        var position = AppWindow.Position;

        _state.WindowWidth = WindowSizing.RoundInt(WindowSizing.PhysicalToDips(size.Width, scale));
        _state.WindowHeight = WindowSizing.RoundInt(WindowSizing.PhysicalToDips(size.Height, scale));
        _state.Maximized = AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Maximized };
        if (!_state.Maximized)
        {
            _state.WindowX = position.X;
            _state.WindowY = position.Y;
        }
    }

    private static string TierLabelOf(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableLabel,
        DensityTier.Large => Metrics.LargeLabel,
        _ => Metrics.CompactLabel,
    };

    /// <summary>显示器某一边（物理像素；只用于诊断输出，取不到就退回工作区尺寸）。</summary>
    private static int GetMonitorSpan(global::Windows.Graphics.RectInt32 workArea, bool width)
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
    string Source);