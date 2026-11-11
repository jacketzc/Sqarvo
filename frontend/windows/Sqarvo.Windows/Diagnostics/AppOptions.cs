using System;
using System.Collections.Generic;
using System.Linq;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// 命令行开关。当前只有只读诊断开关（<c>--report-*</c>）：
/// 它们不改尺寸、不改主题，只把「现在到底是什么状态」打印出来。
/// </summary>
public sealed class AppOptions
{
    private AppOptions(bool reportTheme, bool reportMetrics, bool selfTestWindow)
    {
        ReportTheme = reportTheme;
        ReportMetrics = reportMetrics;
        SelfTestWindow = selfTestWindow;
    }

    /// <summary>打印 <c>[theme]</c> 一行。</summary>
    public bool ReportTheme { get; }

    /// <summary>打印 <c>[metrics]</c> 一行。</summary>
    public bool ReportMetrics { get; }

    /// <summary>打印窗口尺寸算术的边界断言（#163 的纯函数自检）。</summary>
    public bool SelfTestWindow { get; }

    /// <summary>是否有任何「打印后退出」的开关。</summary>
    public bool HasReports => ReportTheme || ReportMetrics || SelfTestWindow;

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
            selfTestWindow: flags.Contains("self-test-window"));
    }

    /// <summary>已知开关清单（给 <c>--help</c> 与文档用）。</summary>
    public static IReadOnlyList<string> KnownSwitches { get; } = new[]
    {
        "--report-theme",
        "--report-metrics",
        "--self-test-window",
    };
}