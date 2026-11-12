using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Sqarvo.Windows.Theming;
using Sqarvo.Windows.Workspace;
using System.Collections.Generic;

namespace Sqarvo.Windows.Views;

/// <summary>
/// 外壳 chrome 的**结构体尺寸**（Thickness / CornerRadius / GridLength）与状态栏渲染。
/// </summary>
/// <remarks>
/// <para>
/// 为什么单独一个 partial 文件：XAML 里能直接用的资源类型只有 double / int
/// （<c>{StaticResource Size.*}</c> 可以喂 Height / FontSize / Spacing），
/// <c>Thickness</c> / <c>CornerRadius</c> 这类结构体在 XAML 里只能写字面量，
/// 而「尺寸只有一个来源」是硬约束（AGENTS.md §8.6），所以这些值在这里从
/// <see cref="Metrics"/> 取，一处集中，不散进各个打包器。
/// </para>
/// <para>
/// 本文件只做**外形与渲染**，不碰业务判定：带 Metrics 的方法只设尺寸，带 Status 的方法只把
/// <see cref="BackendState"/> 这一份快照写进状态栏的两个格子里。业务方法仍在
/// <c>ShellView.xaml.cs</c> / <c>ShellView.Query.cs</c> / <c>ShellView.Metadata.cs</c>，
/// 方法名与调用点都没动（只是把「往状态栏写什么」这一步搬到了这里）。
/// </para>
/// </remarks>
public sealed partial class ShellView
{
    /// <summary>工具条：两个动作按钮与四个种类入口的内边距 / 圆角（高度与字号在 XAML 里取 Size.*）。</summary>
    private void ApplyToolbarMetrics()
    {
        // 动作按钮：水平内边距 toolbar_padding_h，圆角 radius_sm。
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

    /// <summary>
    /// 对象树的行容器留白：整棵树右缩 grid_padding_end，右侧留 tree_arrow_box（徽标 / 箭头同一根轴）。
    /// </summary>
    /// <remarks>
    /// 行高与行字号在 <c>SqarvoTreeRowStyle</c> 里（item 级），这里只给树控件本身的内边距——
    /// <see cref="Thickness"/> 没法从 double 资源转，所以放在代码里取 Metrics。
    /// </remarks>
    private void ApplyTreeChrome()
    {
        ObjectTree.Padding = new Thickness(Metrics.GridPaddingEnd, 0, Metrics.TreeArrowBox, 0);
    }

    /// <summary>
    /// 状态栏的两格：行 1 左 = 后端状态（已连接 / 连接中 / 未连接 · 驱动 N · 浏览会话 N），
    /// 行 2 右 = 当前会话名（没有会话就是「未连接」）。
    /// </summary>
    /// <param name="state">后端状态快照；唯一来源，不在这里另存布尔量。</param>
    private void ApplyStatusChrome(BackendState state)
    {
        // 行 1 左格：文案口径来自 BackendState.StatusLine（真实计数，未就绪时不写假数字）。
        StatusLineText.Text = state.StatusLine;
        ToolTipService.SetToolTip(
            StatusLineText,
            state.Code.Length == 0 ? state.Message : $"{state.Message} [{state.Code}]");

        // 行 2 右格：Linux 的 session 格写「连接名 · 空态摘要」，这里只写真实存在的会话名。
        var names = new List<string>();
        foreach (var profile in _profiles)
        {
            if (_session?.IsConnected(profile.Key) == true)
            {
                names.Add(profile.Name);
            }
        }

        var text = names.Count == 0 ? "未连接" : string.Join(" · ", names);
        SessionText.Text = text;
        ToolTipService.SetToolTip(SessionText, text);
    }
}