using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Sqarvo.Windows.Theming;

namespace Sqarvo.Windows.Views;

/// <summary>
/// 外壳 chrome 的**结构体尺寸**（Thickness / CornerRadius / GridLength）与状态栏渲染。
/// </summary>
/// <remarks>
/// <para>
/// 为什么单独一个 partial 文件：XAML 里能直接用自己的类型只有 double / int 资源
/// （<c>{StaticResource Size.*}</c> 可以喂 Height / FontSize / Spacing），
/// <c>Thickness</c> / <c>CornerRadius</c> 这类结构体在 XAML 里只能写字面量，
/// 而「尺寸只有一个来源」是硬约束（AGENTS.md §8.6），所以这些值在这里从
/// <see cref="Metrics"/> 取，一处集中，不散进各个打包器。
/// </para>
/// <para>
/// 本文件只做**外形**，不碰业务：方法名里带 Metrics 的只设置尺寸，带 Status 的只把
/// <see cref="Workspace.BackendState"/> 这一份快照渲染到状态栏那两个 TextBlock 上。
/// 业务方法仍在 <c>ShellView.xaml.cs</c> / <c>ShellView.Query.cs</c> / <c>ShellView.Metadata.cs</c>，
/// 名字与调用点都没动。
/// </para>
/// </remarks>
public sealed partial class ShellView
{
    /// <summary>工具条：两个动作按钮与四个种类入口的内边距 / 圆角（高度与字号在 XAML 里取 Size.*）。</summary>
    private void ApplyToolbarMetrics()
    {
        // 动作按钮：水平内边距 toolbar_padding_h，圆角 radius_sm（与系统 Button 的 4px 同档的轻微圆角）。
        ConnectChip.Padding = new Thickness(Metrics.ToolbarPaddingH, 0, Metrics.ToolbarPaddingH, 0);
        ConnectChip.CornerRadius = new CornerRadius(Metrics.RadiusSm);
        NewQueryChip.Padding = new Thickness(Metrics.ToolbarPaddingH, 0, Metrics.ToolbarPaddingH, 0);
        NewQueryChip.CornerRadius = new CornerRadius(Metrics.RadiusSm);

        // 种类入口：同一套圆角，内边距取 inspector_gap（比动作按钮紧一档，四个入口才排得下）。
        foreach (var chip in new[] { ButtonTables, ButtonViews, ButtonFunctions, ButtonQueries })
        {
            chip.Padding = new Thickness(Metrics.InspectorGap, 0, Metrics.InspectorGap, 0);
            chip.CornerRadius = new CornerRadius(Metrics.RadiusSm);
        }
    }
}