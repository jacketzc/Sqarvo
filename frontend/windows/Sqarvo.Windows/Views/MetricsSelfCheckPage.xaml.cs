using Microsoft.UI.Xaml.Controls;
using Sqarvo.Windows.Theming;
using System.Collections.Generic;
using System.Globalization;

namespace Sqarvo.Windows.Views;

/// <summary>
/// 工作区内容页：列出当前档位的全部度量键，并带一个「未提交编辑」输入框。
/// </summary>
/// <remarks>
/// 这一页的关键能力是**可从模型重建**：切密度档时整页会被重新构造
/// （<c>StaticResource</c> 只解析一次，改资源字典不会让已解析的界面变，见 ADR 0026 决策 5），
/// 重建前后的草稿文本与滚动位置由 <see cref="CaptureSnapshot"/> / <see cref="RestoreSnapshot"/> 保住。
/// </remarks>
public sealed partial class MetricsSelfCheckPage : UserControl
{
    /// <summary>页面里「不该随重建丢失」的临时 UI 状态。</summary>
    public sealed record ViewSnapshot(string Draft, double ScrollOffset);

    public MetricsSelfCheckPage()
    {
        InitializeComponent();
        FillRows();
        UpdateSummary();
    }

    /// <summary>收集重建前的临时状态。</summary>
    public ViewSnapshot CaptureSnapshot()
        => new(DraftBox.Text, RowsScroller.VerticalOffset);

    /// <summary>重建后回放临时状态（滚动位置要等布局完成才生效，用 UpdateLayout 逼一次）。</summary>
    public void RestoreSnapshot(ViewSnapshot? snapshot)
    {
        if (snapshot is null)
        {
            return;
        }

        DraftBox.Text = snapshot.Draft;
        UpdateLayout();
        RowsScroller.ChangeView(null, snapshot.ScrollOffset, null, disableAnimation: true);
    }

    private void UpdateSummary()
    {
        SummaryText.Text =
            $"当前档位 {Metrics.TierLabel}：header {Metrics.HeaderHeight} · toolbar {Metrics.ToolbarHeight} · " +
            $"status {Metrics.StatusHeight} · minWindow {Metrics.MinWindowWidth}×{Metrics.MinWindowHeight} · " +
            $"sidebar {Metrics.SidebarWidth}（可拖 {Metrics.SidebarMin}–{Metrics.SidebarMax}） · " +
            $"inspector {Metrics.InspectorWidth}（可拖 {Metrics.InspectorMin}–{Metrics.InspectorMax}） · " +
            $"树行高 {Metrics.RowHeightTree} · 数据行 {Metrics.RowHeightData} · 界面字号 {Metrics.FontUi}";
    }

    /// <summary>把当前档位的 41 个尺寸键逐行列出（值来自生成文件，不在界面里算）。</summary>
    private void FillRows()
    {
        var rows = new List<string>();
        Metrics.ForEachSizeToken((key, value) =>
            rows.Add($"{key.Replace("Size.", string.Empty, System.StringComparison.Ordinal),-28}{value.ToString("0", CultureInfo.InvariantCulture),6}"));

        RowsList.ItemsSource = rows;
    }
}