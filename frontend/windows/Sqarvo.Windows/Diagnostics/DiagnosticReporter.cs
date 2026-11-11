using Microsoft.UI.Xaml;
using Sqarvo.Windows.Theming;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// 只读诊断输出。行格式是验收证据的一部分，改动要连同 frontend/windows/README.md 一起改。
/// </summary>
/// <remarks>
/// 这两个开关只读：不改尺寸、不改主题、不写 <c>window.json</c>。之所以要真的建出窗口再打印，
/// 是因为「有效主题」与「标题栏实际写入值」只有在窗口存在时才有值。
/// </remarks>
internal static class DiagnosticReporter
{
    /// <summary>按开关打印 <c>[theme]</c> / <c>[metrics]</c>。</summary>
    public static void Report(Window window, AppOptions options)
    {
        ArgumentNullException.ThrowIfNull(window);
        ArgumentNullException.ThrowIfNull(options);

        if (options.ReportTheme)
        {
            ConsoleBridge.WriteLine(ThemeLine(window));
        }

        if (options.ReportMetrics)
        {
            ConsoleBridge.WriteLine(MetricsLine());
        }
    }

    /// <summary>
    /// <c>[theme]</c> 一行：请求的偏好、生效的明暗、高对比、系统强调色、标题栏实际写入值。
    /// </summary>
    public static string ThemeLine(Window window)
    {
        var titleBar = ThemeService.ApplyTitleBar(window);
        var effective = ThemeService.IsDarkEffective(window) ? "Dark" : "Light";
        return $"[theme] requested={ThemeService.PreferenceName} effective={effective} " +
               $"highContrast={ThemeService.HighContrast} accent={ThemeService.AccentHex} titleBar={titleBar}";
    }

    /// <summary>
    /// <c>[metrics]</c> 一行：当前档位 + 8 个关键值（外壳 / 工具条 / 状态栏高度、最小窗口、
    /// 侧栏与信息面板宽度、树行高、界面字号）。
    /// </summary>
    public static string MetricsLine()
        => $"[metrics] tier={Metrics.TierId} header={Metrics.HeaderHeight} toolbar={Metrics.ToolbarHeight} " +
           $"status={Metrics.StatusHeight} minWindow={Metrics.MinWindowWidth}x{Metrics.MinWindowHeight} " +
           $"sidebar={Metrics.SidebarWidth} inspector={Metrics.InspectorWidth} " +
           $"rowTree={Metrics.RowHeightTree} fontUi={Metrics.FontUi}";
}