using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
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
/// 主窗口：外壳条 / 工具条 / 可重建的页面宿主 / 状态栏，并负责窗口层（尺寸、DPI、
/// 最小尺寸、密度档切换）。
/// </summary>
/// <remarks>
/// <para>
/// 窗口层只有一个入口 <see cref="Relayout"/>：换显示器、改系统缩放、改分辨率、切档位
/// 全部走它。给它们各写一套逻辑，早晚会出现「拖到另一块屏后窗口没变、但尺寸档变了」。
/// </para>
/// <para>
/// 两套像素单位（spec §8.1）：XAML 是 DIP；<c>AppWindow.Resize</c> / <c>MoveAndResize</c> /
/// <c>DisplayArea.WorkArea</c> 是物理像素。「选多大」一律在 DIP 里算
/// （<see cref="WindowSizing"/> 全是 DIP），只在喂给 <c>AppWindow</c> 前后各换算一次。
/// </para>
/// <para>
/// DPI 变化只能从 <c>XamlRoot</c> 拿：<c>DisplayInformation.GetForCurrentView()</c> 在
/// WinUI 3 会抛异常。
/// </para>
/// </remarks>
public sealed partial class MainWindow : Window
{
    /// <summary>几何恢复的容差（物理像素）：显示器被拔掉 / 分辨率变化时的常见偏移。</summary>
    private const int GeometryTolerance = 32;

    private readonly WindowState _state;
    private readonly AppOptions _options;
    private MetricsSelfCheckPage _page;
    private double _lastScale;
    private bool _relayouting;

    public MainWindow(WindowState state, AppOptions options)
    {
        ArgumentNullException.ThrowIfNull(state);
        ArgumentNullException.ThrowIfNull(options);

        _state = state;
        _options = options;

        InitializeComponent();

        _page = new MetricsSelfCheckPage();
        PageHost.Content = _page;

        ApplyChromeMetrics();
        UpdateLabels();

        if (Content is FrameworkElement root)
        {
            root.ActualThemeChanged += OnActualThemeChanged;
            root.Loaded += OnRootLoaded;
        }

        AppWindow.Changed += OnAppWindowChanged;
        ThemeService.SystemAppearanceChanged += OnSystemAppearanceChanged;
        Closed += OnClosed;
    }

    /// <summary>最近一次 <see cref="Relayout"/> 的结果，供 <c>--report-monitor</c> 输出。</summary>
    public WindowLayoutSnapshot? LastLayout { get; private set; }

    /// <summary>窗口层重算：显示器工作区 / 栅格化比例 / 档位 → 最小尺寸 + 窗口尺寸。</summary>
    /// <param name="preferSavedGeometry">
    /// true = 优先恢复 <c>window.json</c> 里的几何（启动时）；false = 按当前工作区重算
    /// （换显示器、改缩放、切档位时）。
    /// </param>
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

            // 档位守卫：保存的档位在当前显示器上可能放不下，往下一档回退并记一行。
            var requested = Metrics.ParseTier(_state.Tier);
            var resolved = WindowSizing.ResolveTier(requested, workWidthDips, workHeightDips);
            if (resolved != requested)
            {
                ConsoleBridge.WriteDiagnostic(
                    $"[layout] 档位 {requested} 放不下（工作区 {workWidthDips}×{workHeightDips}），回退到 {resolved}");
            }

            if (resolved != Metrics.Tier)
            {
                RebuildForTier(resolved);
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

                // 居中放入工作区；窗口比工作区大时贴左上（不会跑到屏幕外）。
                var x = workPhysical.X + Math.Max(0, (workPhysical.Width - physicalWidth) / 2);
                var y = workPhysical.Y + Math.Max(0, (workPhysical.Height - physicalHeight) / 2);
                AppWindow.MoveAndResize(new global::Windows.Graphics.RectInt32(x, y, physicalWidth, physicalHeight));
                source = "分档";
            }

            var size = AppWindow.Size;
            LastLayout = new WindowLayoutSnapshot(
                DisplayWidth: GetMonitorWidth(workPhysical),
                DisplayHeight: GetMonitorHeight(workPhysical),
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

            UpdateLabels();
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

    /// <summary>最小尺寸：只设 <see cref="OverlappedPresenter"/>（物理像素、含边框）。</summary>
    /// <remarks>
    /// 不要同时设 <c>Window.MinWidth/MinHeight</c>（DIP、仅客户区）：两者会互相覆盖，
    /// 症状是「设了最小值还能拖更小」。
    /// </remarks>
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

    /// <summary>保存的几何是否仍落在某块显示器工作区内（±32 容差），否则丢弃。</summary>
    private bool TryRestoreSavedGeometry(double scale, out global::Windows.Graphics.RectInt32 rect)
    {
        rect = default;

        if (!_state.HasGeometry || _state.WindowX is not { } x || _state.WindowY is not { } y)
        {
            return false;
        }

        var width = WindowSizing.DipsToPhysical(_state.WindowWidth!.Value, scale);
        var height = WindowSizing.DipsToPhysical(_state.WindowHeight!.Value, scale);

        foreach (var area in DisplayArea.FindAll())
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

    /// <summary>切换密度档：改档位 → 重写尺寸资源 → 重建页面 → 回放状态 → 重算窗口。</summary>
    private void SwitchTier(DensityTier tier)
    {
        if (tier == Metrics.Tier)
        {
            return;
        }

        // 不可用的档位不写入偏好（spec §2.2）。
        SnapshotWorkArea(out var workWidth, out var workHeight);
        if (!WindowSizing.TierAvailable(tier, workWidth, workHeight))
        {
            var (minWidth, minHeight) = (WindowSizing.MinWindowWidth(tier), WindowSizing.MinWindowHeight(tier));
            StatusText.Text = $"当前显示器放不下 {TierLabelOf(tier)}：需要 {minWidth}×{minHeight} 逻辑工作区";
            return;
        }

        var snapshot = _page.CaptureSnapshot();
        RebuildForTier(tier, snapshot);

        _state.Tier = Metrics.TierId;
        WindowStateStore.Save(_state);

        Relayout(preferSavedGeometry: false);
    }

    /// <summary>真正的「重建页面」：新的页面实例 + 重写的尺寸资源 + 回放状态。</summary>
    private void RebuildForTier(DensityTier tier, MetricsSelfCheckPage.ViewSnapshot? snapshot = null)
    {
        snapshot ??= _page.CaptureSnapshot();

        Metrics.SetTier(tier);

        if (Application.Current is { } application)
        {
            // StaticResource 不会因为资源字典变化而重算，所以必须重建页面（ADR 0026 决策 5）。
            MetricsResources.Apply(application.Resources);
        }

        var rebuilt = new MetricsSelfCheckPage();
        PageHost.Content = rebuilt;
        _page = rebuilt;

        ApplyChromeMetrics();
        rebuilt.RestoreSnapshot(snapshot);
        UpdateLabels();
    }

    private void SnapshotWorkArea(out int workWidthDips, out int workHeightDips)
    {
        var scale = Content?.XamlRoot?.RasterizationScale ?? _lastScale;
        var work = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Nearest).WorkArea;
        workWidthDips = (int)Math.Floor(WindowSizing.PhysicalToDips(work.Width, scale <= 0 ? 1 : scale));
        workHeightDips = (int)Math.Floor(WindowSizing.PhysicalToDips(work.Height, scale <= 0 ? 1 : scale));
    }

    /// <summary>XAML 表达不了的尺寸（<c>Thickness</c> 是结构体）从 Metrics 取。</summary>
    private void ApplyChromeMetrics()
    {
        var horizontal = Metrics.ToolbarPaddingH;

        HeaderBand.Padding = new Thickness(horizontal, 0, horizontal, 0);
        ToolbarBand.Padding = new Thickness(horizontal, 0, horizontal, 0);
        StatusText.Margin = new Thickness(horizontal, 0, horizontal, 0);
        ThemeText.Margin = new Thickness(0, 0, horizontal, 0);

        var pagePadding = Metrics.InspectorGap;
        PageHostBorder.Padding = new Thickness(pagePadding);
        PageHostBorder.BorderThickness = new Thickness(Metrics.BorderWidthPxNeverScaled);
    }

    private void UpdateLabels()
    {
        TierText.Text = $"· {Metrics.TierLabel}";

        var highContrast = ThemeService.HighContrast;
        var effective = highContrast
            ? "高对比"
            : (ThemeService.IsDarkEffective(this) ? "暗色" : "浅色");

        ThemeText.Text = $"主题 {ThemeService.PreferenceName} · 生效 {effective} · 强调色 {ThemeService.AccentHex}";

        ToolbarText.Text =
            $"Ctrl+0 紧凑 · Ctrl+Plus 舒适/大 · Ctrl+minus 降档 · 六区域外壳见 #164";

        var layout = LastLayout;
        StatusText.Text = layout is null
            ? "未连接 · 驱动 0 · 浏览会话 0"
            : $"未连接 · 驱动 0 · 浏览会话 0 · 窗口 {layout.WindowDipsWidth}×{layout.WindowDipsHeight} DIP " +
              $"(物理 {layout.WindowPhysicalWidth}×{layout.WindowPhysicalHeight}, scale {layout.Scale:0.##}, 来源 {layout.Source})";
    }

    private void OnRootLoaded(object sender, RoutedEventArgs args)
    {
        // 第一次 Relayout：此时 XamlRoot 才可用（spec §6.3 允许首帧跳动）。
        Relayout(preferSavedGeometry: true);
    }

    private void OnAppWindowChanged(AppWindow sender, AppWindowChangedEventArgs args)
    {
        // 拖到另一块显示器 / 改分辨率：位置或尺寸变了都要按新工作区重算。
        if (args.DidPositionChange || args.DidSizeChange)
        {
            Relayout(preferSavedGeometry: false);
        }
    }

    private void OnSystemAppearanceChanged(object? sender, EventArgs e)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            ThemeService.ApplyToWindow(this);
            UpdateLabels();
        });
    }

    private void OnActualThemeChanged(FrameworkElement sender, object args)
    {
        ThemeService.ApplyTitleBar(this);
        UpdateLabels();
    }

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

    private static string TierLabelOf(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableLabel,
        DensityTier.Large => Metrics.LargeLabel,
        _ => Metrics.CompactLabel,
    };

    private void OnClosed(object sender, WindowEventArgs args)
    {
        ThemeService.SystemAppearanceChanged -= OnSystemAppearanceChanged;

        // 诊断开关是只读的：`--report-*` 的进程不写持久化文件。
        if (_options.HasReports)
        {
            return;
        }

        SaveGeometry();
        _state.Tier = Metrics.TierId;
        _state.Theme = ThemeService.PreferenceName.ToLowerInvariant();
        WindowStateStore.Save(_state);
    }

    /// <summary>把当前窗口几何写进内存模型（宽度存 DIP、位置存物理像素）。</summary>
    private void SaveGeometry()
    {
        var scale = _lastScale <= 0 ? 1 : _lastScale;
        var size = AppWindow.Size;
        var position = AppWindow.Position;

        _state.WindowWidth = WindowSizing.RoundInt(WindowSizing.PhysicalToDips(size.Width, scale));
        _state.WindowHeight = WindowSizing.RoundInt(WindowSizing.PhysicalToDips(size.Height, scale));

        // 最大化时不记位置与尺寸：否则下次启动会以最大化尺寸恢复成「普通窗口」。
        _state.Maximized = AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Maximized };
        if (!_state.Maximized)
        {
            _state.WindowX = position.X;
            _state.WindowY = position.Y;
        }
    }

    /// <summary>显示器宽度（只用于诊断输出；物理像素）。</summary>
    private static int GetMonitorWidth(global::Windows.Graphics.RectInt32 workArea)
        => GetMonitorSpan(workArea, width: true);

    /// <summary>显示器高度（只用于诊断输出；物理像素）。</summary>
    private static int GetMonitorHeight(global::Windows.Graphics.RectInt32 workArea)
        => GetMonitorSpan(workArea, width: false);

    private static int GetMonitorSpan(global::Windows.Graphics.RectInt32 workArea, bool width)
    {
        // DisplayArea 只暴露工作区；分辨率靠 GDI 取（PerMonitorV2 下是物理像素）。
        // 取不到就退回工作区尺寸，不编一个数字。
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