using System;
using System.Collections.Generic;
using System.Linq;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// 命令行开关。全部是只读诊断开关：不改尺寸、不改主题、不写 <c>window.json</c>。
/// </summary>
/// <remarks>
/// <para>
/// 开关名有两套历史写法：规范（<c>scaling-and-window-sizing.md</c> §9 的验收表）写
/// <c>--qa-report-monitor</c>，实现与 README 写 <c>--report-monitor</c>。两套都接受，
/// 因为验收脚本是照着规范抄的，而人手册是照着 README 抄的——让其中一边静默失效等于
/// 「跑了一次没输出的验收」。
/// </para>
/// <para>
/// 别名表只做「名字归一」，不新增行为：归一后的名字进同一份 flags 集合，
/// 后面所有判定都只看归一后的名字。
/// </para>
/// </remarks>
public sealed class AppOptions
{
    /// <summary>别名 → 规范名（只登记真实存在过的两种写法）。</summary>
    private static readonly Dictionary<string, string> Aliases = new(StringComparer.Ordinal)
    {
        ["qa-report-monitor"] = "report-monitor",
    };

    private AppOptions(
        bool reportTheme,
        bool reportMetrics,
        bool reportMonitor,
        bool reportShell,
        bool selfTestWindow,
        string? qaTier,
        string? qaConnect,
        string? qaMetadata,
        string? qaCatalog,
        bool noBackend,
        string? qaSignIn,
        string? qaQuery,
        int qaQueryPages)
    {
        ReportTheme = reportTheme;
        ReportMetrics = reportMetrics;
        ReportMonitor = reportMonitor;
        ReportShell = reportShell;
        SelfTestWindow = selfTestWindow;
        QaTier = qaTier;
        QaConnect = qaConnect;
        QaMetadata = qaMetadata;
        QaCatalog = qaCatalog;
        NoBackend = noBackend;
        QaSignIn = qaSignIn;
        QaQuery = qaQuery;
        QaQueryPages = Math.Clamp(qaQueryPages, 1, 8);
    }

    /// <summary>打印 <c>[theme]</c> 一行。</summary>
    public bool ReportTheme { get; }

    /// <summary>打印 <c>[metrics]</c> 一行。</summary>
    public bool ReportMetrics { get; }

    /// <summary>打印 <c>[monitor]</c> 一行（显示器 / 缩放 / 工作区 / 最终窗口尺寸 / 档位 / 来源 / 菜单行）。</summary>
    public bool ReportMonitor { get; }

    /// <summary>打印整棵控件树（UI Automation 的 ControlType + Name，缩进两格）。</summary>
    public bool ReportShell { get; }

    /// <summary>QA 专用：启动时切到指定档位（<c>compact</c> / <c>comfortable</c> / <c>large</c>）。</summary>
    public string? QaTier { get; }

    /// <summary>
    /// QA 专用：<c>--qa-connect=&lt;名称|@first&gt;</c>。后端就绪后跑一遍
    /// 「测试连接 → 连接 → 断开」，把证据打到 <c>[connect]</c> 行再退出。
    /// </summary>
    /// <remarks>
    /// 存在的理由：远程构建机上没有可靠的 UI 自动化（点菜单要人工审批），而「连接数据库是否真的能成」
    /// 必须被验证。密码来自环境变量 <c>SQARVO_QA_PASSWORD</c>，先写进系统凭据库再连接，
    /// 因此这一趟同时验证了 Credential Manager。
    /// </remarks>
    public string? QaConnect { get; }

    /// <summary>
    /// QA 专用：<c>--qa-metadata=&lt;名称|@first&gt;</c>。后端就绪后连接该条目，然后**真的**展开
    /// 对象树（库 → Tables → 第一张表）并把每一层的计数与列名打到 <c>[metadata]</c> 行；
    /// 与 <c>--report-shell</c> 同时给出时，随后打印的那棵控件树就是「展开后的树 + 已填充的 Objects 页」。
    /// </summary>
    /// <remarks>
    /// 存在的理由与 <see cref="QaConnect"/> 相同：远程构建机上没有可靠的 UI 自动化，而「元数据真的
    /// 拉到了没有」必须被验证。它走的是与用户点击完全相同的加载路径，不是另一套快路径。
    /// 密码同样来自环境变量 <c>SQARVO_QA_PASSWORD</c>（可选：已有凭据引用时不必给）。
    /// </remarks>
    public string? QaMetadata { get; }

    /// <summary>QA 专用：<c>--qa-catalog=&lt;库名&gt;</c>，给 <c>--qa-metadata</c> 指定要展开的库（省略时取第一个可见库）。</summary>
    public string? QaCatalog { get; }

    /// <summary>
    /// QA 专用：<c>--no-backend</c>。只开窗、不启动后端，用来隔离「界面自身」与「后端就绪后的接线」
    /// 哪一侧导致崩溃（远程构建机上一个回合只能验一件事）。
    /// </summary>
    public bool NoBackend { get; }

    /// <summary>
    /// QA 专用：<c>--qa-signin=&lt;名称|@first&gt;</c>。后端就绪后写入凭据并建立数据库会话，**保持窗口运行**，
    /// 供外部截图与手工点检（与 <c>--qa-connect</c> 的区别是后者跑完就退出）。
    /// </summary>
    public string? QaSignIn { get; }

    /// <summary>
    /// QA 专用：<c>--qa-query=&lt;sql&gt;</c>。后端就绪后连接该条目（<c>--qa-signin</c> 给出目标，否则取第一条），
    /// 然后**真的**新建一个查询标签、执行这条 SQL，等首批回来就把 <c>[query]</c> 证据打到 stdout；
    /// 与 <c>--qa-signin</c> 合用（它本身就是「保持运行供截图」的开关）时窗口留着供截图，否则打完就退出。
    /// </summary>
    /// <remarks>
    /// 存在的理由与 <see cref="QaMetadata"/> 相同：远程构建机上没有可靠的 UI 自动化，而「执行 SQL 真的能拿到
    /// 结果」「游标真的被 Close」必须被验证。它走与用户点击完全相同的路径（建标签 → Execute → 首批 → 渲染）。
    /// </remarks>
    public string? QaQuery { get; }

    /// <summary>`--qa-query-pages=N`：首批之后最多再续拉 N-1 批（验证 has_more 与计数）。1..8。</summary>
    public int QaQueryPages { get; }

    /// <summary>打印窗口尺寸算术与面板宽度换算的边界断言（纯函数自检，不建窗口）。</summary>
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
            .Select(argument => Canonical(argument.TrimStart('-')))
            .ToHashSet(StringComparer.Ordinal);

        return new AppOptions(
            reportTheme: flags.Contains("report-theme"),
            reportMetrics: flags.Contains("report-metrics"),
            reportMonitor: flags.Contains("report-monitor"),
            reportShell: flags.Contains("report-shell"),
            selfTestWindow: flags.Contains("self-test-window"),
            qaConnect: Value(commandLine, "--qa-connect="),
            qaMetadata: Value(commandLine, "--qa-metadata="),
            qaCatalog: Value(commandLine, "--qa-catalog="),
            noBackend: flags.Contains("no-backend"),
            qaSignIn: Value(commandLine, "--qa-signin="),
            qaQuery: Value(commandLine, "--qa-query="),
            qaQueryPages: Number(commandLine, "--qa-query-pages=", 1),
            qaTier: commandLine
                .Skip(1)
                .Where(argument => argument.StartsWith("--qa-tier=", StringComparison.Ordinal))
                .Select(argument => argument.Substring("--qa-tier=".Length).Trim().ToLowerInvariant())
                .FirstOrDefault());
    }

    /// <summary>取 <c>--name=value</c> 形式的取值（没有就给 <see langword="null"/>）。</summary>
    private static string? Value(string[] commandLine, string prefix) =>
        commandLine
            .Skip(1)
            .Where(argument => argument.StartsWith(prefix, StringComparison.Ordinal))
            .Select(argument => argument.Substring(prefix.Length).Trim())
            .FirstOrDefault();

    /// <summary>读一个整数开关；缺省或非法就用 <paramref name="fallback"/>（不抛、不打断启动）。</summary>
    private static int Number(string[] commandLine, string prefix, int fallback) =>
        int.TryParse(Value(commandLine, prefix), out var parsed) ? parsed : fallback;

    /// <summary>把开关名归一：去 <c>--</c>、转小写、别名换规范名。</summary>
    private static string Canonical(string flag)
    {
        var name = flag.ToLowerInvariant();
        return Aliases.TryGetValue(name, out var canonical) ? canonical : name;
    }

    /// <summary>已知开关清单（给文档用）。</summary>
    public static IReadOnlyList<string> KnownSwitches { get; } = new[]
    {
        "--report-theme",
        "--report-metrics",
        "--report-monitor（别名 --qa-report-monitor）",
        "--report-shell",
        "--self-test-window",
        "--qa-tier=<compact|comfortable|large>",
        "--qa-connect=<连接名称|@first>（配合环境变量 SQARVO_QA_PASSWORD）",
        "--qa-metadata=<连接名称|@first>（可选 --qa-catalog=<库名>；配合 SQARVO_QA_PASSWORD）",
        "--no-backend 只开窗、不启动后端（隔离崩溃源）",
        "--qa-signin=<连接名称|@first>（写凭据 + 连接后保持运行，供截图）",
        "--qa-query=<sql>（配合 --qa-signin 指定连接：建查询页 + 执行 SQL，打 [query] 证据）",
    };
}