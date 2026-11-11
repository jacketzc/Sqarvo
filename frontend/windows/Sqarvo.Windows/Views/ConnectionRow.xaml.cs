using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Sqarvo.Windows.Theming;
using System;

namespace Sqarvo.Windows.Views;

/// <summary>
/// 对象树的一行连接节点：图标位 + 名称 + 右对齐徽标（#170）。
/// </summary>
/// <remarks>
/// 形状与颜色在 <c>ConnectionRow.xaml</c>（颜色只用 ThemeResource，代码里没有画刷字符串），
/// 这里只做两件事：把 <see cref="Metrics"/> 里的结构体尺寸（内边距）写进去、按连接状态切徽标。
/// </remarks>
public sealed partial class ConnectionRow : UserControl
{
    /// <summary>已连接的徽标文案（与 <c>ShellView.BadgeFor</c> 同一份口径）。</summary>
    private const string ConnectedBadge = "已连接";

    public ConnectionRow()
    {
        InitializeComponent();

        // Thickness 是结构体，XAML 里只能写字面量；所以内边距在这里从度量取。
        // 右边留 tree_arrow_box：让徽标右缘与「展开箭头位」同一条基线，行尾不会顶到边缘。
        RowRoot.Padding = new Thickness(Metrics.GridPaddingEnd, 0, Metrics.TreeArrowBox, 0);
        NodeName.Margin = new Thickness(Metrics.GridPaddingEnd, 0, Metrics.GridPaddingEnd, 0);
    }

    /// <summary>填入一条连接的显示信息：名称 + 徽标（徽标决定颜色档，不影响行高）。</summary>
    /// <param name="name">连接名（超长按省略号截断）。</param>
    /// <param name="badge">徽标文案：已连接 / 未连接 / 连接中…。</param>
    public void Show(string name, string badge)
    {
        NodeName.Text = name;

        var connected = string.Equals(badge, ConnectedBadge, StringComparison.Ordinal);
        BadgeConnected.Text = connected ? badge : string.Empty;
        BadgeConnected.Visibility = connected ? Visibility.Visible : Visibility.Collapsed;
        BadgeIdle.Text = connected ? string.Empty : badge;
        BadgeIdle.Visibility = connected ? Visibility.Collapsed : Visibility.Visible;
    }
}