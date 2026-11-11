using System;
using System.Collections.Generic;
using System.Linq;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// 命令行开关。全部是只读诊断开关：不改尺寸、不改主题、不写 <c>window.json</c>。
/// </summary>
public sealed class AppOptions
{
    private AppOptions(bool reportTheme, bool reportMetrics, bool reportMonitor, bool reportShell, bool selfTestWindow)
    {
        ReportTheme = reportTheme;
        ReportMetrics = reportMetrics;
        ReportMonitor = reportMonitor;
        ReportShell = reportShell;
        SelfTestWindow = selfTestWindow;
    }

    /// <summary>打印 <c>[theme]</c> 一行。</summary>
    public bool ReportTheme { get; }

    /// <summary>打印 <c>[metrics]</c> 一行。</summary>
    public bool ReportMetrics { get; }

    /// <summary>打印 <c>[monitor]</c> 一行（显示器 / 缩放 / 工作区 / 最终窗口尺寸 / 档位 / 来源）。</summary>
    public bool ReportMonitor { get; }

    /// <summary>打印整棵控件树（UI Automation 的 ControlType + Name，缩进两格）。</summary>
    public bool ReportShell { get; }

    /// <summary>打印窗口尺寸算术的边界断言（纯函数自检，不建窗口）。</summary>
    public bool SelfTestWindow { get; }

    /// <summary>是否有任何「打印后退出」的开关。</summary>
    public bool HasReports => ReportTheme || ReportMetrics || ReportMonitor || ReportShell || SelfTestWindow;

    /// <summary>是否需要窗口（自检是纯函数，不需要）。</summary>
    public bool NeedsWindow => ReportTheme || ReportMetrics || ReportMonitor || ReportShell;

    /// <summary>从完整命令行解析（第一项是 exe 路径，跳过）。</summary>
    public static AppOptions Parse(string[] commandLine)
    {
        ArgumentNullException.ThrowIfNull(commandLine);

        var flags = commandLine
            .Skip(1)
            .Where(argument => argument.StartsWith("--", StringComparison.Ordinal))
            .Select(argument => argument.TrimStart('-').ToLowerInvariant())
            .ToHashSet(StringComparer.Ordinal);

        return new AppOptions(
            reportTheme: flags.Contains("report-theme"),
            reportMetrics: flags.Contains("report-metrics"),
            reportMonitor: flags.Contains("report-monitor"),
            reportShell: flags.Contains("report-shell"),
            selfTestWindow: flags.Contains("self-test-window"));
    }

    /// <summary>已知开关清单（给文档用）。</summary>
    public static IReadOnlyList<string> KnownSwitches { get; } = new[]
    {
        "--report-theme",
        "--report-metrics",
        "--report-monitor",
        "--report-shell",
        "--self-test-window",
    };
}