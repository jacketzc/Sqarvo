using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Sqarvo.Windows.Theming;
using System;
using System.Collections.Generic;

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
/// </remarks>
public sealed partial class ShellView : UserControl
{
    /// <summary>系统标题栏高度（Windows 提供，不由我们自绘）。</summary>
    private const int SystemTitleBarHeight = 40;

    /// <summary>外壳条里属于「菜单行」的那部分高度。</summary>
    private const int MenuRowHeight = 24;

    /// <summary>用户点了「文件 → 退出」。</summary>
    public event EventHandler? QuitRequested;

    public ShellView()
    {
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

    private void ApplyMetrics()
    {
        // 外壳 = 系统标题栏 40 + 窗口内菜单行；菜单行高度由规范的外壳高度反推。
        MenuBand.Height = Math.Max(MenuRowHeight, Metrics.HeaderHeight - SystemTitleBarHeight);

        // 侧栏宽度与状态栏左边沟同宽（分栏线在同一根轴上）。
        ObjectTree.Width = Metrics.SidebarWidth;
        TreeSearchBox.Width = Metrics.SidebarWidth;

        InspectorTitle.FontSize = Metrics.FontTitle;
        InspectorSplit.OpenPaneLength = Metrics.InspectorWidth;

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