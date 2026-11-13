using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Sqarvo.Windows.Theming;
using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Threading.Tasks;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// <c>--qa-layout-audit=&lt;路径&gt;</c>：把「几个关键具名元素的实际布局」数出来，一行一个 JSON。
/// </summary>
/// <remarks>
/// <para>
/// 存在的理由：布局验收现在靠肉眼比截图，说不清「差 2 DIP 是哪一层算错的」。这里直接读运行时几何
/// （<c>ActualWidth</c> / <c>ActualHeight</c> / 相对窗口的坐标 / 字号），CSS 式的"看起来不对"就变成
/// 有数字的 diff。
/// </para>
/// <para>
/// 元素按 <c>x:Name</c> 在 <see cref="Views.ShellView"/> 的命名域里找：找不到就输出
/// <c>{"name":"X","missing":true}</c>，**不静默跳过**——静默跳过会让「清单少了一行」和「元素真的没了」
/// 看起来一样。
/// </para>
/// <para>
/// 坐标一律相对于窗口内容（<c>ShellView</c> 根），因此与截图里的像素位置可以直接对照（乘上
/// <c>rasterization_scale</c> 就是物理像素）。
/// </para>
/// </remarks>
internal static class LayoutAudit
{
    /// <summary>
    /// 审计清单：<c>(对外名字, 实际 x:Name)</c>。左边是验收表里的名字，右边是本仓库实际存在的名字
    /// （有三个是历史改名：TabStrip→WorkspaceTabs、StatusBandSecondLine→SqlSummaryText、
    /// ButtonRefresh→ButtonRowRefresh；ColumnDefinitions 与面板列没有对应控件，见下方注释）。
    /// </summary>
    private static readonly (string Name, string Target)[] Entries =
    {
        ("HeaderBand", "HeaderBand"),
        ("HeaderTitle", "HeaderTitle"),
        ("HeaderSubtitle", "HeaderSubtitle"),
        ("HeaderMenuButton", "HeaderMenuButton"),
        ("ToolbarBand", "ToolbarBand"),
        ("ButtonConnect", "ButtonConnect"),
        ("ButtonNewQuery", "ButtonNewQuery"),
        ("ButtonRefresh", "ButtonRowRefresh"),
        ("ToolbarKinds", "ToolbarKinds"),
        ("TabStrip", "WorkspaceTabs"),
        ("SidebarColumn", "SidebarColumn"),
        ("InspectorColumn", "InspectorBody"),
        ("StatusBand", "StatusBand"),
        ("StatusBandSecondLine", "SqlSummaryText"),
        ("ObjectsTable", "ObjectsTable"),
        ("ObjectsHeaderRow", "ObjectsTableHeader"),
        ("InspectorTitle", "InspectorTitle"),
    };

    /// <summary>等布局稳定（根元素有尺寸）后输出审计；<paramref name="path"/> 为 <see langword="null"/> 时打到 stdout。</summary>
    public static async Task RunAsync(MainWindow window, string? path)
    {
        var lines = new List<string>();

        try
        {
            var shell = window.Shell;
            var root = shell.AuditRoot;

            // 刚 Activate 时 ActualWidth 还是 0：等到根元素真的量出尺寸再取数，最多等 5 秒。
            var deadline = DateTimeOffset.UtcNow.AddSeconds(5);
            while (root.ActualWidth <= 0 && DateTimeOffset.UtcNow < deadline)
            {
                await Task.Delay(100);
            }

            // 再给一帧：TreeView / 表格的模板展开在同一帧里还没跑完。
            await Task.Delay(200);

            foreach (var (name, target) in Entries)
            {
                // 单个元素取数失败不能拖垮整份审计：写一行 error，继续下一个。
                try
                {
                    lines.Add(Describe(shell, root, name, target));
                }
                catch (Exception error)
                {
                    lines.Add(
                        "{\"name\":\"" + Escape(name) + "\",\"error\":\""
                        + Escape(error.GetType().Name + ": " + error.Message
                            + " (0x" + error.HResult.ToString("X8", CultureInfo.InvariantCulture) + ")") + "\"}");
                    ConsoleBridge.WriteDiagnostic("[layout] " + name + " 取数失败：" + error);
                }
            }

            // 数据网格列宽（--qa-layout-audit 扩展）：每个数据列一行，读共享列宽状态。
            // 表头与所有行都用绝对值 GridLength 从这份状态取值，因此它就是实际渲染宽度；
            // 没有激活的数据页时不静默跳过，给一行明确的 missing。
            var columnWidths = shell.ActiveDataColumnWidths();
            if (columnWidths is null)
            {
                lines.Add("{\"name\":\"DataColumn\",\"missing\":true}");
            }
            else
            {
                for (var index = 0; index < columnWidths.Count; index++)
                {
                    lines.Add(
                        "{\"name\":\"DataColumn." + index.ToString(CultureInfo.InvariantCulture) + "\""
                        + ",\"actual_w\":" + Number(columnWidths[index]) + "}");
                }
            }

            var scale = shell.XamlRoot?.RasterizationScale ?? 1d;
            lines.Add(
                "{\"tier\":\"" + Metrics.TierId + "\""
                + ",\"rasterization_scale\":" + Number(scale)
                + ",\"window_w\":" + Number(root.ActualWidth)
                + ",\"window_h\":" + Number(root.ActualHeight) + "}");
        }
        catch (Exception error)
        {
            lines.Add("{\"audit_error\":\"" + Escape(error.GetType().Name + ": " + error.Message) + "\"}");
        }

        Emit(lines, path);
    }

    /// <summary>取一个元素的几何；取不到就给一行 <c>missing</c>。</summary>
    private static string Describe(FrameworkElement shell, FrameworkElement root, string name, string target)
    {
        var element = shell.FindName(target);

        if (element is null)
        {
            return "{\"name\":\"" + Escape(name) + "\",\"missing\":true}";
        }

        double width;
        double height;
        double x;
        double y;

        if (element is ColumnDefinition column)
        {
            // ColumnDefinition 不是 FrameworkElement：宽度取它自己的 ActualWidth，高度取宿主 Grid 的高度，
            // x 取前面几列的宽度之和（再叠上宿主 Grid 相对根元素的偏移）。
            var owner = FindOwningGrid(root, column);
            if (owner is null)
            {
                return "{\"name\":\"" + Escape(name) + "\",\"missing\":true}";
            }

            var offset = Offset(owner, root);
            var before = 0d;
            foreach (var other in owner.ColumnDefinitions)
            {
                if (ReferenceEquals(other, column))
                {
                    break;
                }

                before += other.ActualWidth;
            }

            width = column.ActualWidth;
            height = owner.ActualHeight;
            x = offset.X + before;
            y = offset.Y;
        }
        else if (element is FrameworkElement framework)
        {
            var offset = Offset(framework, root);
            width = framework.ActualWidth;
            height = framework.ActualHeight;
            x = offset.X;
            y = offset.Y;
        }
        else
        {
            return "{\"name\":\"" + Escape(name) + "\",\"missing\":true}";
        }

        var json = "{\"name\":\"" + Escape(name) + "\""
            + ",\"actual_w\":" + Number(width)
            + ",\"actual_h\":" + Number(height)
            + ",\"x\":" + Number(x)
            + ",\"y\":" + Number(y);

        // ColumnDefinition 不是 UIElement：它没有字号，跳过（对 ColumnDefinition 调 VisualTreeHelper 会抛 COMException）。
        var fontSize = element is FrameworkElement measured ? FontSizeOf(measured) : null;
        if (fontSize is { } size)
        {
            json += ",\"font_size\":" + Number(size);
        }

        return json + "}";
    }

    /// <summary>元素左上角相对 <paramref name="root"/> 的坐标（DIP）。</summary>
    private static (double X, double Y) Offset(FrameworkElement element, FrameworkElement root)
    {
        try
        {
            var point = element.TransformToVisual(root).TransformPoint(new global::Windows.Foundation.Point(0, 0));

            return (point.X, point.Y);
        }
        catch (ArgumentException)
        {
            // 不在同一棵可视树里（理论上不会发生）：退回 0，至少把尺寸交出去。
            return (0, 0);
        }
    }

    /// <summary>找到「列定义属于它」的那个 <see cref="Grid"/>。</summary>
    private static Grid? FindOwningGrid(DependencyObject node, ColumnDefinition column)
    {
        if (node is Grid grid && grid.ColumnDefinitions.Contains(column))
        {
            return grid;
        }

        var count = VisualTreeHelper.GetChildrenCount(node);
        for (var index = 0; index < count; index++)
        {
            var found = FindOwningGrid(VisualTreeHelper.GetChild(node, index), column);
            if (found is not null)
            {
                return found;
            }
        }

        return null;
    }

    /// <summary>元素自身或其内容里第一个字号（<see cref="TextBlock"/> / <see cref="Control"/>），没有就给 <see langword="null"/>。</summary>
    private static double? FontSizeOf(DependencyObject? element)
    {
        switch (element)
        {
            case null:
                return null;
            case TextBlock text:
                return text.FontSize;
            case Control control:
                return control.FontSize;
        }

        if (element is not UIElement)
        {
            return null;
        }

        try
        {
            var count = VisualTreeHelper.GetChildrenCount(element);
            for (var index = 0; index < count; index++)
            {
                var found = FontSizeOf(VisualTreeHelper.GetChild(element, index));
                if (found is not null)
                {
                    return found;
                }
            }
        }
        catch (Exception)
        {
            // 个别节点（模板里的宿主对象）取子节点会抛；字号拿不到就当没有，不影响几何输出。
        }

        return null;
    }

    /// <summary>数字统一格式：最多两位小数、固定小数点（避免中文区域把小数点写成逗号）。</summary>
    private static string Number(double value) =>
        Math.Round(value, 2).ToString("0.##", CultureInfo.InvariantCulture);

    /// <summary>JSON 字符串转义（名字都是 ASCII，这里只为不炸掉输出）。</summary>
    private static string Escape(string value)
    {
        var builder = new System.Text.StringBuilder(value.Length + 8);
        foreach (var ch in value)
        {
            switch (ch)
            {
                case '"':
                    builder.Append("\\\"");
                    break;
                case '\\':
                    builder.Append("\\\\");
                    break;
                case '\n':
                    builder.Append("\\n");
                    break;
                case '\r':
                    builder.Append("\\r");
                    break;
                default:
                    builder.Append(ch);
                    break;
            }
        }

        return builder.ToString();
    }

    /// <summary>输出：给了路径写文件（并打一行汇总），否则逐行打 stdout。</summary>
    private static void Emit(List<string> lines, string? path)
    {
        if (string.IsNullOrWhiteSpace(path))
        {
            foreach (var line in lines)
            {
                ConsoleBridge.WriteLine(line);
            }

            return;
        }

        try
        {
            var full = Path.GetFullPath(path);
            var folder = Path.GetDirectoryName(full);
            if (!string.IsNullOrEmpty(folder))
            {
                Directory.CreateDirectory(folder);
            }

            File.WriteAllLines(full, lines);
            ConsoleBridge.WriteLine($"[layout] path={full} lines={lines.Count}");
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteLine($"[layout] failed {error.GetType().Name}: {error.Message}");
        }
    }
}