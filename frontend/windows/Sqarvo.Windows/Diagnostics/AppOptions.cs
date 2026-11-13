using System;
using System.Collections.Generic;
using System.IO;
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
        ["qa-screenshot"] = "screenshot",
        ["qa-screenshot-delay"] = "screenshot-delay",
        ["qa-layout-audit"] = "layout-audit",
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
        int qaQueryPages,
        string? qaData,
        string? screenshotPath,
        double screenshotDelaySeconds,
        bool layoutAudit,
        string? layoutAuditPath,
        (int Index, double Width)? qaColumnWidth)
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
        QaData = qaData;
        ScreenshotPath = screenshotPath;
        ScreenshotDelaySeconds = screenshotDelaySeconds < 0 ? 0 : screenshotDelaySeconds;
        LayoutAudit = layoutAudit;
        LayoutAuditPath = layoutAuditPath;
        QaColumnWidth = qaColumnWidth;
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

    /// <summary>
    /// QA 专用：<c>--qa-data=&lt;库.表&gt;</c>。后端就绪后连接 <c>--qa-signin</c> 指定的条目（否则第一条），
    /// 然后**真的**打开表数据页：翻一页 → 改一格并提交 → 还原 → 再改一格并撤销，把 <c>[data]</c> 证据打到 stdout。
    /// </summary>
    /// <remarks>
    /// 存在的理由与 <see cref="QaQuery"/> 相同：远程构建机上没有可靠的 UI 自动化，而「表数据真的能读、
    /// 提交真的落库、撤销真的回滚」必须被验证。它走与用户点击完全相同的路径（同一个打开 / 翻页 / 暂存 / 提交）。
    /// 探针会先写入再还原开发库的一个单元格，**不要指向生产库**。
    /// </remarks>
    public string? QaData { get; }

    /// <summary>
    /// 诊断专用：<c>--screenshot=&lt;路径&gt;</c>（别名 <c>--qa-screenshot</c>）。窗口激活后延迟
    /// <see cref="ScreenshotDelaySeconds"/> 秒，用 <c>RenderTargetBitmap</c> 渲染窗口内容并存 PNG。
    /// </summary>
    /// <remarks>
    /// 存在的理由：外部截图（GDI BitBlt / 剪贴板）会抓到别的窗口，而且每次都要过审批；界面自身的
    /// 渲染结果才是"用户看到的界面"。只给开关不给值时写到 <c>%TEMP%\sqarvo-shot.png</c>。
    /// </remarks>
    public string? ScreenshotPath { get; }

    /// <summary><c>--screenshot-delay=&lt;秒&gt;</c>：截图前等多久（默认 2.0，负数按 0 处理）。</summary>
    public double ScreenshotDelaySeconds { get; }

    /// <summary>
    /// 诊断专用：<c>--qa-layout-audit[=&lt;路径&gt;]</c>（别名 <c>--layout-audit</c>）。逐个打印具名元素的
    /// 实际布局（宽 / 高 / 相对窗口的 x、y / 字号），末尾打印档位与缩放。不给路径就打到 stdout。
    /// </summary>
    public bool LayoutAudit { get; }

    /// <summary><c>--qa-layout-audit</c> 的输出路径；<see langword="null"/> = 打到 stdout。</summary>
    public string? LayoutAuditPath { get; }

    /// <summary>
    /// QA 钩子：<c>--qa-column-width=&lt;列序号&gt;:&lt;宽度&gt;</c>。只在诊断路径里把某数据列设成用户宽度
    /// （拖拽动作无法脚本模拟，用它做截图 / 布局审计取证）；不改窗口几何、不写 window.json。
    /// <b>只在 --qa-data 的数据页路径里消费</b>（<c>RunQaDataAsync</c>）：单独给这个开关不会有任何效果。
    /// </summary>
    public (int Index, double Width)? QaColumnWidth { get; }

    /// <summary>打印窗口尺寸算术与面板宽度换算的边界断言（纯函数自检，不建窗口）。</summary>
    public bool SelfTestWindow { get; }

    /// <summary>是否有任何「打印后退出」的开关。</summary>
    public bool HasReports => ReportTheme || ReportMetrics || ReportMonitor || ReportShell || SelfTestWindow;

    /// <summary>是否需要窗口（自检是纯函数，不需要）。</summary>
    public bool NeedsWindow => ReportTheme || ReportMetrics || ReportMonitor || ReportShell;

    /// <summary>
    /// 关窗时是否跳过写 <c>window.json</c>：诊断开关不该改用户记忆的窗口几何
    /// （<c>--report-*</c> 一直如此；新加的截图 / 布局审计开关同样只读）。
    /// </summary>
    public bool SuppressesGeometrySave => HasReports || ScreenshotPath is not null || LayoutAudit;

    /// <summary>是否有任何「窗口起来之后才有话可说」的诊断开关（截图 / 布局审计）。</summary>
    public bool HasWindowDiagnostics => ScreenshotPath is not null || LayoutAudit;

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
            qaData: Value(commandLine, "--qa-data="),
            screenshotPath: ScreenshotTarget(commandLine),
            screenshotDelaySeconds: Seconds(commandLine, "--screenshot-delay=", 2.0),
            layoutAudit: Has(commandLine, "--qa-layout-audit", "--layout-audit"),
            layoutAuditPath: FirstValue(commandLine, "--qa-layout-audit=", "--layout-audit="),
            qaColumnWidth: ParseColumnWidth(commandLine),
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

    /// <summary>某个开关是否出现（<c>--name</c> 或 <c>--name=…</c> 都算；别名由调用方一并给出）。</summary>
    private static bool Has(string[] commandLine, params string[] names) =>
        commandLine
            .Skip(1)
            .Any(argument => names.Any(name =>
                string.Equals(argument, name, StringComparison.OrdinalIgnoreCase)
                || argument.StartsWith(name + "=", StringComparison.OrdinalIgnoreCase)));

    /// <summary>多个前缀里第一个非空取值（<c>--qa-x=…</c> 与 <c>--x=…</c> 两种写法）。</summary>
    private static string? FirstValue(string[] commandLine, params string[] prefixes)
    {
        foreach (var prefix in prefixes)
        {
            var value = Value(commandLine, prefix);
            if (!string.IsNullOrWhiteSpace(value))
            {
                return value;
            }
        }

        return null;
    }

    /// <summary>
    /// <c>--screenshot</c> 的目标路径：给了开关但没给值 → <c>%TEMP%\sqarvo-shot.png</c>；
    /// 没给开关 → <see langword="null"/>（不截图）。
    /// </summary>
    private static string? ScreenshotTarget(string[] commandLine)
    {
        if (!Has(commandLine, "--screenshot", "--qa-screenshot"))
        {
            return null;
        }

        var explicitPath = FirstValue(commandLine, "--screenshot=", "--qa-screenshot=");
        return explicitPath ?? System.IO.Path.Combine(System.IO.Path.GetTempPath(), "sqarvo-shot.png");
    }

    /// <summary>读一个整数开关；缺省或非法就用 <paramref name="fallback"/>（不抛、不打断启动）。</summary>
    private static int Number(string[] commandLine, string prefix, int fallback) =>
        int.TryParse(Value(commandLine, prefix), out var parsed) ? parsed : fallback;

    /// <summary>读一个小数开关（秒）；缺省或非法就用 <paramref name="fallback"/>。</summary>
    private static double Seconds(string[] commandLine, string prefix, double fallback) =>
        double.TryParse(
            Value(commandLine, prefix),
            System.Globalization.NumberStyles.Float,
            System.Globalization.CultureInfo.InvariantCulture,
            out var parsed)
            ? parsed
            : fallback;

    /// <summary>
    /// 解析 <c>--qa-column-width=&lt;列序号&gt;:&lt;宽度&gt;</c>：列序号从 0 起，宽度是 DIP。
    /// 非法 / 越界写法一律返回 null（诊断开关不该因为格式问题打断启动）。
    /// </summary>
    private static (int Index, double Width)? ParseColumnWidth(string[] commandLine)
    {
        var raw = Value(commandLine, "--qa-column-width=");
        if (string.IsNullOrWhiteSpace(raw))
        {
            return null;
        }

        var parts = raw.Split(':', 2);
        if (parts.Length != 2
            || !int.TryParse(parts[0], System.Globalization.NumberStyles.Integer,
                System.Globalization.CultureInfo.InvariantCulture, out var index)
            || index < 0
            || !double.TryParse(parts[1], System.Globalization.NumberStyles.Float,
                System.Globalization.CultureInfo.InvariantCulture, out var width)
            || !double.IsFinite(width))
        {
            ConsoleBridge.WriteDiagnostic(
                $"[colwidth] 无法解析 --qa-column-width={raw}（用法：<列序号>:<宽度>）");
            return null;
        }

        return (index, width);
    }

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
        "--qa-data=<库.表>（配合 --qa-signin 指定连接：开数据页 + 翻页 + 改一格提交 + 撤销，打 [data] 证据；会写开发库）",
        "--qa-column-width=<列序号>:<宽度>（QA 钩子：诊断路径上把某列设成用户宽度，供截图 / 布局审计取证；拖拽本身无法脚本模拟）",
    };
}