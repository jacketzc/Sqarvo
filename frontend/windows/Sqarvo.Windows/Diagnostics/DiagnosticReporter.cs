using Microsoft.UI.Xaml;
using Sqarvo.Windows.Layout;
using Sqarvo.Windows.Storage;
using Sqarvo.Windows.Theming;
using System;
using System.Collections.Generic;
using System.Globalization;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// 只读诊断输出。行格式是验收证据的一部分，改动要连同 frontend/windows/README.md 一起改。
/// </summary>
/// <remarks>
/// 这些开关只读：不改尺寸、不改主题、不写 <c>window.json</c>。之所以要真的建出窗口再打印，
/// 是因为「有效主题」「标题栏」「工作区与最终窗口尺寸」「菜单行 / 面板宽度」只有在窗口存在时才有值。
/// </remarks>
internal static class DiagnosticReporter
{
    /// <summary>按开关打印 <c>[theme]</c> / <c>[metrics]</c> / <c>[monitor]</c>。</summary>
    public static void Report(Window window, AppOptions options, WindowLayoutSnapshot? layout)
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

        if (options.ReportMonitor && layout is not null)
        {
            ConsoleBridge.WriteLine(MonitorLine(layout));
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
    /// <c>[metrics]</c> 一行：当前档位 + 关键值（外壳 / 工具条 / 状态栏高度、最小窗口、
    /// 侧栏与信息面板默认宽度、树行高 / 标签条高、菜单行、界面字号）。
    /// </summary>
    public static string MetricsLine()
        => $"[metrics] tier={Metrics.TierId} header={Metrics.HeaderHeight} toolbar={Metrics.ToolbarHeight} " +
           $"status={Metrics.StatusHeight} minWindow={Metrics.MinWindowWidth}x{Metrics.MinWindowHeight} " +
           $"sidebar={Metrics.SidebarWidth} inspector={Metrics.InspectorWidth} " +
           $"rowTree={Metrics.RowHeightTree} tabStrip={Metrics.TabStripHeight} " +
           $"menuRow={WindowSizing.MenuRowHeight(Metrics.Tier)} fontUi={Metrics.FontUi}";

    /// <summary>
    /// <c>[monitor]</c> 一行：显示器、缩放、工作区（物理 / DIP）、最终窗口（DIP / 物理）、档位、来源、
    /// 最大化状态、实测非客户区高度、菜单行高度、侧栏与信息面板宽度。
    /// </summary>
    /// <remarks>
    /// 「来源」= 分档（按工作区推荐）/ 记忆（<c>window.json</c> 里的几何）/ 记忆+最大化 /
    /// 保留（DPI 或工作区变了但保留当前矩形）/ 档位（切档重算）/ 最大化 / 最小化 / QA（诊断开关强制）。
    /// <c>titleBar</c> 是**实测**值（窗口物理高 ÷ 比例 − 内容实际高），量不到时写「待测」而不是编一个数。
    /// </remarks>
    public static string MonitorLine(WindowLayoutSnapshot layout)
    {
        var titleBar = layout.TitleBarDips > 0
            ? layout.TitleBarDips.ToString("0.#", CultureInfo.InvariantCulture)
            : "待测";
        return $"[monitor] display={layout.DisplayWidth}x{layout.DisplayHeight} scale={layout.Scale:0.##} " +
               $"workArea物理={layout.WorkAreaPhysicalWidth}x{layout.WorkAreaPhysicalHeight} " +
               $"工作区DIP={layout.WorkAreaDipsWidth}x{layout.WorkAreaDipsHeight} → " +
               $"窗口DIP={layout.WindowDipsWidth}x{layout.WindowDipsHeight} " +
               $"物理={layout.WindowPhysicalWidth}x{layout.WindowPhysicalHeight} " +
               $"tier={TierId(layout.Tier)} 来源={layout.Source} " +
               $"最大化={(layout.Maximized ? "是" : "否")} titleBar={titleBar} menuRow={layout.MenuRowDips} " +
               $"侧栏={layout.SidebarDips} 面板={layout.InspectorDips}";
    }

    /// <summary>
    /// <c>--self-test-window</c>：跑纯函数边界断言，打印每个分档边界与面板换算的实际取值。
    /// </summary>
    /// <returns>失败项数量（0 = 全通过）。</returns>
    public static int SelfTestWindow()
    {
        var failures = new List<string>(WindowSizing.SelfTest());

        foreach (var workWidth in new[] { 1199, 1200, 1699, 1700, 2199, 2200 })
        {
            var (width, height) = WindowSizing.Recommended(DensityTier.Compact, workWidth, 1000);
            var (minWidth, minHeight) = WindowSizing.MinimumWindow(DensityTier.Compact, workWidth, 1000);
            ConsoleBridge.WriteLine(
                $"[selftest] W={workWidth} → 窗口 {width}x{height}（最小 {minWidth}x{minHeight}，" +
                $"上限 {workWidth - WindowSizing.WidthMargin}x{1000 - WindowSizing.HeightMargin}）");
        }

        // 1920×1032（1920×1080 减 Windows 任务栏 48 后的逻辑工作区）那一列的三个推荐尺寸：
        // README 里的那组数字由这里断言，不再只写成「实测」——它们能在这台机器上被判对错。
        foreach (var (tier, expectedWidth, expectedHeight) in new[]
        {
            (DensityTier.Compact, 1400, 826),
            (DensityTier.Comfortable, 1750, 984),
            (DensityTier.Large, 1888, 984),
        })
        {
            var actual = WindowSizing.Recommended(tier, 1920, 1032);
            ConsoleBridge.WriteLine(
                $"[selftest] 1920x1032 {TierId(tier)} → 窗口 {actual.Width}x{actual.Height}（期望 {expectedWidth}x{expectedHeight}）");
            if (actual != (expectedWidth, expectedHeight))
            {
                failures.Add($"1920x1032 {TierId(tier)} 推荐尺寸 {actual.Width}x{actual.Height} ≠ 期望 {expectedWidth}x{expectedHeight}");
            }
        }

        foreach (var tier in new[] { DensityTier.Compact, DensityTier.Comfortable, DensityTier.Large })
        {
            ConsoleBridge.WriteLine(
                $"[selftest] {TierId(tier)} 菜单行 {WindowSizing.MenuRowHeight(tier)} DIP" +
                $"（系统标题栏基准 {WindowSizing.TitleBarBaseDips} DIP；最小点击区 {WindowSizing.MinTargetOf(tier)}）");
        }

        failures.AddRange(WindowState.SelfTest());

        foreach (var failure in failures)
        {
            ConsoleBridge.WriteLine($"[selftest] 失败：{failure}");
        }

        ConsoleBridge.WriteLine(
            failures.Count == 0
                ? "[selftest] 窗口尺寸 / 菜单行 / 面板宽度断言全部通过"
                : $"[selftest] 窗口尺寸 / 菜单行 / 面板宽度断言 {failures.Count} 项失败");

        return failures.Count;
    }

    private static string TierId(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => "comfortable",
        DensityTier.Large => "large",
        _ => "compact",
    };
}