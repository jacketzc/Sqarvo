using Microsoft.UI.Xaml;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Storage;
using Sqarvo.Windows.Theming;
using System;

namespace Sqarvo.Windows;

/// <summary>
/// 主窗口骨架（#162 范围内是「度量与主题自检页」，六区域外壳在 #164）。
/// </summary>
/// <remarks>
/// 两条与 WinUI 相关的注意事项：
/// <list type="bullet">
/// <item>XAML 里表达不了的尺寸（<c>Thickness</c> / <c>CornerRadius</c> 这类结构体）在这里
/// 从 <see cref="Metrics"/> 取，避免在 XAML 里写字面量——见 <see cref="ApplyMetrics"/>。</item>
/// <item>系统主题 / 高对比 / 强调色变化的事件可能在非 UI 线程触发，
/// 更新控件前必须回到 <see cref="DispatcherQueue"/>。</item>
/// </list>
/// </remarks>
public sealed partial class MainWindow : Window
{
    private readonly WindowState _state;
    private readonly AppOptions _options;

    /// <summary>创建主窗口。</summary>
    public MainWindow(WindowState state, AppOptions options)
    {
        ArgumentNullException.ThrowIfNull(state);
        ArgumentNullException.ThrowIfNull(options);

        _state = state;
        _options = options;

        InitializeComponent();

        ApplyMetrics();
        UpdateLabels();

        // 元素级主题变化（含跟随系统时系统切主题）会触发这里，比 UISettings 事件可靠：
        // 后者在未打包的 WinUI 3 里可能订阅不到。
        if (Content is FrameworkElement root)
        {
            root.ActualThemeChanged += OnActualThemeChanged;
        }

        ThemeService.SystemAppearanceChanged += OnSystemAppearanceChanged;
        Closed += OnClosed;
    }

    private void OnActualThemeChanged(FrameworkElement sender, object args)
    {
        ThemeService.ApplyTitleBar(this);
        UpdateLabels();
    }

    /// <summary>
    /// 把 XAML 表达不了的尺寸写进去（<c>Padding</c> / <c>BorderThickness</c> 是结构体，
    /// 不能直接吃 <c>Size.*</c> 的 double 资源）。
    /// </summary>
    private void ApplyMetrics()
    {
        var horizontal = Metrics.ToolbarPaddingH;

        HeaderBand.Padding = new Thickness(horizontal, 0, horizontal, 0);
        ToolbarBand.Padding = new Thickness(horizontal, 0, horizontal, 0);

        // 1px 描边在所有档位都保持 1px（spec §3.2），所以它不随档位缩放。
        ContentCard.BorderThickness = new Thickness(Metrics.BorderWidthPxNeverScaled);

        var cardPadding = Metrics.InspectorGap;
        ContentCard.Padding = new Thickness(cardPadding);

        StatusText.Margin = new Thickness(horizontal, 0, horizontal, 0);
        ThemeText.Margin = new Thickness(0, 0, horizontal, 0);
    }

    /// <summary>刷新三处文案：档位、主题状态、度量摘要。</summary>
    private void UpdateLabels()
    {
        TierText.Text = $"· {Metrics.TierLabel}";

        // 高对比时不再报「暗色 / 浅色」：HC 下 WinUI 的 ActualTheme 仍报 Light/Dark，
        // 说「生效 高对比」才是用户看到的东西。
        var highContrast = ThemeService.HighContrast;
        var effective = highContrast
            ? "高对比"
            : (ThemeService.IsDarkEffective(this) ? "暗色" : "浅色");

        ThemeText.Text =
            $"主题 {ThemeService.PreferenceName} · 生效 {effective} · 强调色 {ThemeService.AccentHex}";

        MetricsSummaryText.Text =
            $"header {Metrics.HeaderHeight} · toolbar {Metrics.ToolbarHeight} · status {Metrics.StatusHeight} · " +
            $"minWindow {Metrics.MinWindowWidth}×{Metrics.MinWindowHeight} · " +
            $"sidebar {Metrics.SidebarWidth}（可拖 {Metrics.SidebarMin}–{Metrics.SidebarMax}） · " +
            $"inspector {_state.InspectorWidthFor(Metrics.Tier)}（可拖 {Metrics.InspectorMin}–{Metrics.InspectorMax}） · " +
            $"树行高 {Metrics.RowHeightTree} · 界面字号 {Metrics.FontUi}";

        // 空态文案是真实读数：没有后端、没有驱动、没有浏览会话。
        StatusText.Text = "未连接 · 驱动 0 · 浏览会话 0";
    }

    private void OnSystemAppearanceChanged(object? sender, EventArgs e)
    {
        // UISettings.ColorValuesChanged 在后台线程触发，控件只能在 UI 线程改。
        DispatcherQueue.TryEnqueue(() =>
        {
            ThemeService.ApplyToWindow(this);
            UpdateLabels();
        });
    }

    private void OnClosed(object sender, WindowEventArgs args)
    {
        ThemeService.SystemAppearanceChanged -= OnSystemAppearanceChanged;

        // 诊断开关是只读的：`--report-*` 的进程不写持久化文件。
        if (_options.HasReports)
        {
            return;
        }

        _state.Tier = Metrics.TierId;
        _state.Theme = ThemeService.PreferenceName.ToLowerInvariant();
        WindowStateStore.Save(_state);
    }
}