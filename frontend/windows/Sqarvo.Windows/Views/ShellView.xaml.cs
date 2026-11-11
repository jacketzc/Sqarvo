using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Layout;
using Sqarvo.Windows.Storage;
using Sqarvo.Windows.Theming;
using System;

namespace Sqarvo.Windows.Views;

/// <summary>工作区状态（六态里的前三个在结构上已就位，本轮只有 Disconnected 可达）。</summary>
public enum WorkspaceState
{
    /// <summary>尚未连接任何数据库（本轮唯一可达状态）。</summary>
    Disconnected = 0,

    /// <summary>正在连接（结构位；接后端后可切）。</summary>
    Loading = 1,

    /// <summary>已连接但没有选中对象（结构位）。</summary>
    Empty = 2,
}

/// <summary>
/// 六区域外壳：菜单行 / 主工具条 / 对象树 / 工作区 / 状态栏 / 信息面板。
/// </summary>
/// <remarks>
/// <para>
/// 本轮**不接后端**：工具条、菜单项、行操作全部禁用并统一 tooltip「尚未接入后端」；
/// 状态栏写真实空态（未连接 · 驱动 0 · 浏览会话 0）。禁止出现假连接、假行数、假耗时。
/// </para>
/// <para>
/// 尺寸全部来自 <see cref="Metrics"/>（构建期生成）：高度/字号在 XAML 里用
/// <c>{StaticResource Size.*}</c>，宽度与 <c>Thickness</c> 这类结构体在
/// <see cref="ApplyMetrics"/> 里从 Metrics 取。
/// </para>
/// <para>
/// #168：侧栏与信息面板可拖。拖动只在**本类**里改布局（每帧钳到档位区间），
/// 松手后才通过 <see cref="PanelWidthsChanged"/> 交给窗口层归一化写盘——
/// 拖动过程中不碰磁盘，也不改 <see cref="Metrics"/>。
/// </para>
/// </remarks>
public sealed partial class ShellView : UserControl
{
    /// <summary>面板宽度的拖动累计值（Thumb 的 DragDelta 是增量，自己累加比每帧取整更稳）。</summary>
    private double _sidebarDrag;
    private double _inspectorDrag;

    private readonly WindowState _state;

    /// <summary>用户点了「文件 → 退出」。</summary>
    public event EventHandler? QuitRequested;

    /// <summary>侧栏 / 信息面板宽度变了（拖动结束或键盘微调后触发一次）。</summary>
    public event EventHandler? PanelWidthsChanged;

    public ShellView(WindowState state)
    {
        ArgumentNullException.ThrowIfNull(state);

        _state = state;

        InitializeComponent();
        ApplyMetrics();
        WireMenu();
        SetWorkspaceState(WorkspaceState.Disconnected);
        UpdateChrome("尚未接入后端：工具条 / 菜单 / 行操作全部禁用", "尚未接入后端");
    }

    /// <summary>当前工作区状态。</summary>
    public WorkspaceState State { get; private set; } = WorkspaceState.Disconnected;

    /// <summary>标签数量（Objects 页恒为 1；查询标签要等接后端）。</summary>
    public int WorkspaceTabCount => WorkspaceTabs.TabItems.Count;

    /// <summary>侧栏当前宽度（DIP，等于布局里那一列的像素宽度）。</summary>
    public int SidebarWidth => SidebarColumn.Width.IsAbsolute
        ? (int)Math.Round(SidebarColumn.Width.Value)
        : Metrics.SidebarWidth;

    /// <summary>信息面板当前宽度（DIP）。</summary>
    public int InspectorWidth => (int)Math.Round(InspectorSplit.OpenPaneLength);

    /// <summary>页面里跨越「重建」的临时状态（切密度档会整页重建）。</summary>
    public sealed record ViewSnapshot(int SelectedTab, string StatusRight);

    /// <summary>切密度档前收集状态。</summary>
    public ViewSnapshot CaptureSnapshot()
        => new(WorkspaceTabs.SelectedIndex, LastRefreshText.Text);

    /// <summary>重建后回放状态。</summary>
    public void RestoreSnapshot(ViewSnapshot? snapshot)
    {
        if (snapshot is null)
        {
            return;
        }

        if (snapshot.SelectedTab >= 0 && snapshot.SelectedTab < WorkspaceTabs.TabItems.Count)
        {
            WorkspaceTabs.SelectedIndex = snapshot.SelectedTab;
        }

        if (!string.IsNullOrEmpty(snapshot.StatusRight))
        {
            LastRefreshText.Text = snapshot.StatusRight;
        }
    }

    /// <summary>切换工作区状态（三态互斥）。</summary>
    public void SetWorkspaceState(WorkspaceState state)
    {
        State = state;
        StateDisconnected.Visibility = state == WorkspaceState.Disconnected ? Visibility.Visible : Visibility.Collapsed;
        StateLoading.Visibility = state == WorkspaceState.Loading ? Visibility.Visible : Visibility.Collapsed;
        StateEmpty.Visibility = state == WorkspaceState.Empty ? Visibility.Visible : Visibility.Collapsed;
        ObjectsPage.Visibility = state == WorkspaceState.Empty ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>把「尚未接入后端」这类提示写进状态栏（不弹假对话框）。</summary>
    public void NotifyNotImplemented(string action)
        => UpdateChrome($"{action}：尚未接入后端", $"未连接 · 驱动 0 · 浏览会话 0");

    /// <summary>更新状态栏两行的文案。</summary>
    public void UpdateChrome(string sqlSummary, string rightText)
    {
        SqlSummaryText.Text = sqlSummary;
        LastRefreshText.Text = rightText;
    }

    /// <summary>更新标签页标题上的档位提示（窗口层调）。</summary>
    public void UpdateTierLabel()
    {
        var tip = $"当前密度档：{Metrics.TierLabel}（Ctrl+0 / Ctrl+Plus / Ctrl+minus 切换）";
        ToolTipService.SetToolTip(WorkspaceTabs, tip);
        AutomationProperties.SetName(WorkspaceTabs, $"工作区标签 · {tip}");
    }

    /// <summary>
    /// 把侧栏 / 信息面板设成给定宽度（DIP），越界一律钳到当前档位区间。
    /// </summary>
    /// <remarks>
    /// 两个面板的可拖区间来自 metrics（<c>sidebar_min/max</c>、<c>inspector_min/max</c>），
    /// 窗口层在写盘后会用 <see cref="WindowState.SidebarWidthFor"/> /
    /// <see cref="WindowState.InspectorWidthFor"/> 的换算结果再调一次本方法，
    /// 所以界面显示的就是下次启动会恢复的值。
    /// </remarks>
    public void ApplyPanelWidths(int sidebarWidth, int inspectorWidth)
    {
        var sidebar = WindowState.ClampSidebarWidth(sidebarWidth, Metrics.Tier);
        var inspector = WindowState.ClampInspectorWidth(inspectorWidth, Metrics.Tier);

        SidebarColumn.MinWidth = Metrics.SidebarMin;
        SidebarColumn.MaxWidth = Metrics.SidebarMax;
        SidebarColumn.Width = new GridLength(sidebar);

        // 状态栏左边沟与侧栏同一根轴：沟宽 = 侧栏 + 分隔条（分隔条那条竖线就是分栏线）。
        StatusGutterColumn.Width = new GridLength(sidebar + Metrics.GridPaddingEnd);
        TreeSearchBox.Width = sidebar;

        InspectorSplit.OpenPaneLength = inspector;
    }

    private void ApplyMetrics()
    {
        // 菜单行高度 = 外壳高 − 系统标题栏基准 × 档位 → 24 / 30 / 36。
        // 基准由度量推出（compact 外壳 64 − 菜单行 24 = 40），这里没有尺寸字面量。
        MenuBand.Height = WindowSizing.MenuRowHeight(Metrics.Tier);

        // 侧栏 / 信息面板：默认宽度来自度量，持久化值是 compact 归一值（state 负责换算）。
        ApplyPanelWidths(_state.SidebarWidthFor(Metrics.Tier), _state.InspectorWidthFor(Metrics.Tier));

        InspectorTitle.FontSize = Metrics.FontTitle;

        // Objects 固定页：宽度 objects_min_width，标签条高 tab_strip_height，
        // 内容内边距 grid_padding_start / grid_padding_end（与 Linux 的布局算术同一套数）。
        ObjectsTab.MinWidth = Metrics.ObjectsMinWidth;
        ObjectsTab.MinHeight = Metrics.TabStripHeight;
        ObjectsPage.Padding = new Thickness(Metrics.GridPaddingStart, 0, Metrics.GridPaddingEnd, 0);

        MenuBand.Padding = new Thickness(Metrics.ToolbarPaddingH, 0, Metrics.ToolbarPaddingH, 0);
        ToolbarBand.Padding = new Thickness(Metrics.ToolbarPaddingH, 0, Metrics.ToolbarPaddingH, 0);
        StatusBand.Padding = new Thickness(Metrics.ToolbarPaddingH, 0, Metrics.ToolbarPaddingH, 0);

        UpdateTierLabel();
    }

    private void WireMenu()
    {
        // 禁用的是「打开真对话框」的入口：本轮没有后端，弹出来也是假的。
        foreach (var item in new Control[] { MenuNewConnection, MenuNewQueryTab, MenuExportCsv })
        {
            ToolTipService.SetToolTip(item, "尚未接入后端");
        }

        MenuQuit.Click += (_, _) => QuitRequested?.Invoke(this, EventArgs.Empty);
        MenuAbout.Click += OnAboutClick;
        MenuNewConnection.Click += (_, _) => NotifyNotImplemented("新建连接");
        MenuNewQueryTab.Click += (_, _) => NotifyNotImplemented("新建查询标签");
        MenuExportCsv.Click += (_, _) => NotifyNotImplemented("导出 CSV");

        ButtonConnect.Click += (_, _) => NotifyNotImplemented("连接");
        ButtonNewQuery.Click += (_, _) => NotifyNotImplemented("新建查询");
    }

    // ---- 侧栏拖动 ----------------------------------------------------------------

    private void OnSidebarSplitterDragStarted(object sender, DragStartedEventArgs args)
        => _sidebarDrag = SidebarWidth;

    private void OnSidebarSplitterDragDelta(object sender, DragDeltaEventArgs args)
    {
        _sidebarDrag = WindowState.ClampSidebarWidth(
            WindowSizing.RoundInt(_sidebarDrag + args.HorizontalChange),
            Metrics.Tier);
        ApplyPanelWidths((int)_sidebarDrag, InspectorWidth);
    }

    private void OnSidebarSplitterDragCompleted(object sender, DragCompletedEventArgs args)
        => CommitPanelWidths();

    private void OnSidebarSplitterDoubleTapped(object sender, DoubleTappedRoutedEventArgs args)
    {
        args.Handled = true;
        ApplyPanelWidths(Metrics.SidebarWidth, InspectorWidth);
        CommitPanelWidths();
    }

    private void OnSidebarSplitterKeyDown(object sender, KeyRoutedEventArgs args)
    {
        var step = Metrics.InspectorGap;
        if (args.Key == global::Windows.System.VirtualKey.Left)
        {
            ApplyPanelWidths(SidebarWidth - step, InspectorWidth);
        }
        else if (args.Key == global::Windows.System.VirtualKey.Right)
        {
            ApplyPanelWidths(SidebarWidth + step, InspectorWidth);
        }
        else
        {
            return;
        }

        args.Handled = true;
        CommitPanelWidths();
    }

    // ---- 信息面板拖动 ------------------------------------------------------------

    private void OnInspectorSplitterDragStarted(object sender, DragStartedEventArgs args)
        => _inspectorDrag = InspectorWidth;

    private void OnInspectorSplitterDragDelta(object sender, DragDeltaEventArgs args)
    {
        // 面板在右边：指针向左（负增量）= 面板变宽。
        _inspectorDrag = WindowState.ClampInspectorWidth(
            WindowSizing.RoundInt(_inspectorDrag - args.HorizontalChange),
            Metrics.Tier);
        ApplyPanelWidths(SidebarWidth, (int)_inspectorDrag);
    }

    private void OnInspectorSplitterDragCompleted(object sender, DragCompletedEventArgs args)
        => CommitPanelWidths();

    private void OnInspectorSplitterDoubleTapped(object sender, DoubleTappedRoutedEventArgs args)
    {
        args.Handled = true;
        ApplyPanelWidths(SidebarWidth, Metrics.InspectorWidth);
        CommitPanelWidths();
    }

    private void OnInspectorSplitterKeyDown(object sender, KeyRoutedEventArgs args)
    {
        var step = Metrics.InspectorGap;
        if (args.Key == global::Windows.System.VirtualKey.Left)
        {
            // 面板在右边：向左 = 变宽。
            ApplyPanelWidths(SidebarWidth, InspectorWidth + step);
        }
        else if (args.Key == global::Windows.System.VirtualKey.Right)
        {
            ApplyPanelWidths(SidebarWidth, InspectorWidth - step);
        }
        else
        {
            return;
        }

        args.Handled = true;
        CommitPanelWidths();
    }

    /// <summary>把当前宽度交给窗口层（归一化 → 写 window.json → 换算回来校验）。</summary>
    private void CommitPanelWidths()
    {
        ConsoleBridge.WriteDiagnostic($"[panel] 拖动结束：侧栏 {SidebarWidth} DIP，信息面板 {InspectorWidth} DIP");
        PanelWidthsChanged?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>「关于 Squarvo」：只写真实信息（版本 + 未连接状态），不编后端版本/端点。</summary>
    private async void OnAboutClick(object sender, RoutedEventArgs args)
    {
        var dialog = new ContentDialog
        {
            Title = "关于 Squarvo",
            Content =
                "Sqarvo · Windows 前端骨架\n" +
                "原生数据库客户端（WinUI 3 + Windows App SDK + C#）\n" +
                "尚未连接后端：本版本不执行任何数据库操作，也不包含任何真实数据。\n" +
                $"当前密度档：{Metrics.TierLabel}",
            CloseButtonText = "关闭",
            XamlRoot = XamlRoot,
        };

        await dialog.ShowAsync();
    }
}