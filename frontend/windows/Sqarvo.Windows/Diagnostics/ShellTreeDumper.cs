using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Media;
using System;
using System.Linq;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// <c>--report-shell</c>：用 UI Automation 枚举并打印整棵控件树（<c>ControlType</c> + <c>Name</c>，
/// 缩进两格），供语义验收（区域是否都在、顺序是否正确、图标按钮有没有中文名）。
/// </summary>
/// <remarks>
/// 只打印**有名字**的元素：WinUI 模板里的匿名内部元素（ScrollBar 的 RepeatButton、网格线之类）
/// 有几十个，全打出来会把六个区域淹掉。名字来自 <c>AutomationProperties.Name</c> 或 <c>x:Name</c>，
/// 缩进按「已打印祖先」的层数算，因此输出是结构树而不是像素树。
/// </remarks>
internal static class ShellTreeDumper
{
    private static readonly string[] RegionNames =
    {
        "MenuBand", "ToolbarBand", "ObjectTree", "WorkspaceTabs", "StatusBand", "InspectorSplit", "InspectorBody",
    };

    /// <summary>从窗口内容开始打印。</summary>
    public static void Dump(Window window)
    {
        ArgumentNullException.ThrowIfNull(window);

        if (window.Content is not DependencyObject content)
        {
            ConsoleBridge.WriteLine("[shell] 窗口内容为空");
            return;
        }

        ConsoleBridge.WriteLine("[shell] 控件树（ControlType \"Name\"，缩进 = 层级）");
        Walk(content, depth: 0);
    }

    private static void Walk(DependencyObject node, int depth)
    {
        var count = VisualTreeHelper.GetChildrenCount(node);
        for (var index = 0; index < count; index++)
        {
            var child = VisualTreeHelper.GetChild(node, index);
            if (child is not FrameworkElement element || element.Visibility != Visibility.Visible)
            {
                continue;
            }

            // 只打「有语义名字」的元素：可访问名（AutomationProperties.Name）优先，
            // 其次是六个区域容器自己的 x:Name。模板内部名字（LayoutRoot 之类）不打印，
            // 否则六个区域会被几十个 ScrollBar 部件、ContentPresenter 淹掉。
            var name = AutomationProperties.GetName(element);
            if (string.IsNullOrEmpty(name) && RegionNames.Contains(element.Name))
            {
                name = element.Name;
            }

            if (!string.IsNullOrEmpty(name))
            {
                var peer = FrameworkElementAutomationPeer.FromElement(element);
                var controlType = peer is null
                    ? element.GetType().Name
                    : peer.GetAutomationControlType().ToString();

                ConsoleBridge.WriteLine($"{new string(' ', depth * 2)}{controlType} \"{name}\"");
                Walk(child, depth + 1);
                continue;
            }

            // 匿名容器：不打印、不增加层级（结构树只按有名字的元素分层）。
            Walk(child, depth);
        }
    }
}