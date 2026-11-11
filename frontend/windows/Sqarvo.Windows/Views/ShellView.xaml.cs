using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Sqarvo.Core.Process;
using Sqarvo.Windows.Connections;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Layout;
using Sqarvo.Windows.Storage;
using Sqarvo.Windows.Theming;
using Sqarvo.Windows.Workspace;
using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using Windows.System;

namespace Sqarvo.Windows.Views;

/// <summary>工作区状态（六态里的前三个；接后端后按真实状态切换）。</summary>
public enum WorkspaceState
{
    /// <summary>后端不可用或数据库未连接。</summary>
    Disconnected = 0,

    /// <summary>正在连接（后端启动中）。</summary>
    Loading = 1,

    /// <summary>已就绪但没有选中对象。</summary>
    Empty = 2,
}

/// <summary>
/// 六区域外壳：菜单行 / 主工具条 / 对象树 / 工作区 / 状态栏 / 信息面板。
/// </summary>
/// <remarks>
/// <para>
/// #169 起这里真正接上了 <see cref="BackendSession"/>：菜单、工具条、状态栏、对象树全部由
/// <see cref="BackendState"/> 这一份快照渲染（见 <see cref="ApplyBackendState"/>），
/// 各控件**不各自**维护「是否已连接」的布尔量。健康探测由本页的
/// <c>DispatcherTimer</c> 每 <see cref="BackendSupervisor.HealthPollInterval"/> 拉一次。
/// </para>
/// <para>
/// 未实现的入口保持禁用，并在 tooltip 里**逐项**说明原因（不写统一一句「尚未实现」）：
/// 对象树分组/分页与元数据是 #170，SQL 编辑器与标签是 #171，导出与表数据编辑在其后。
/// 禁止出现假连接、假行数、假耗时。
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

    /// <summary>
    /// 连接节点下的加载态占位（子树在 #170 接通元数据前是空的，但状态必须可见）。
    /// </summary>
    private const string LoadingPlaceholder = "正在加载对象列表…（元数据服务随后续工作包接通，子树暂为空）";

    private readonly ConnectionStore _store;
    private readonly CancellationTokenSource _lifetime = new();
    private readonly Dictionary<string, TreeViewNode> _nodes = new(StringComparer.Ordinal);
    private readonly HashSet<string> _connectingKeys = new(StringComparer.Ordinal);

    /// <summary>
    /// 每个连接节点上一次画出来的徽标文案。徽标变了才重建那一行，避免每 5 秒一次的健康轮询
    /// 把整棵树重建一遍（重建会丢掉展开状态与键盘焦点）。
    /// </summary>
    private readonly Dictionary<string, string> _badges = new(StringComparer.Ordinal);
    private readonly DispatcherTimer _healthTimer;

    private BackendSession? _session;
    private IReadOnlyList<ConnectionProfile> _profiles = Array.Empty<ConnectionProfile>();
    private string? _notice;
    private bool _loaded;
    private bool _dialogOpen;
    private bool _connecting;
    private bool _healthInFlight;

    /// <summary>用户点了「文件 → 退出」。</summary>
    public event EventHandler? QuitRequested;

    /// <summary>侧栏 / 信息面板宽度变了（拖动结束或键盘微调后触发一次）。</summary>
    public event EventHandler? PanelWidthsChanged;

    public ShellView(WindowState state)
    {
        CrashLog.Trace("shell.ctor.begin");
        ArgumentNullException.ThrowIfNull(state);

        _state = state;

        InitializeComponent();
        CrashLog.Trace("shell.ctor.after-init");

        _store = App.Profiles;

        ApplyMetrics();
        WireMenu();
        SetWorkspaceState(WorkspaceState.Disconnected);

        _healthTimer = new DispatcherTimer { Interval = BackendSupervisor.HealthPollInterval };
        _healthTimer.Tick += OnHealthTick;

        Loaded += OnLoaded;
        Unloaded += OnUnloaded;

        CrashLog.Trace("shell.ctor.before-state");
        ApplyBackendState();
        CrashLog.Trace("shell.ctor.done");
    }

    /// <summary>当前工作区状态。</summary>
    public WorkspaceState State { get; private set; } = WorkspaceState.Disconnected;

    /// <summary>标签数量（Objects 页恒为 1；查询标签要等 #171）。</summary>
    public int WorkspaceTabCount => WorkspaceTabs.TabItems.Count;

    /// <summary>侧栏当前宽度（DIP，等于布局里那一列的像素宽度）。</summary>
    public int SidebarWidth => SidebarColumn.Width.IsAbsolute
        ? (int)Math.Round(SidebarColumn.Width.Value)
        : Metrics.SidebarWidth;

    /// <summary>信息面板当前宽度（DIP）。</summary>
    public int InspectorWidth => (int)Math.Round(InspectorSplit.OpenPaneLength);

    /// <summary>页面里跨越「重建」的临时状态（切密度档会整页重建）。</summary>
    /// <remarks>
    /// 只带「用户选的标签」这种纯界面状态：后端状态、已连接的连接、状态栏文案都从
    /// <see cref="BackendSession"/> 重新渲染，不从快照里回放——否则重建后可能显示过期状态。
    /// </remarks>
    public sealed record ViewSnapshot(int SelectedTab);

    /// <summary>切密度档前收集状态。</summary>
    public ViewSnapshot CaptureSnapshot() => new(WorkspaceTabs.SelectedIndex);

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

    /// <summary>把一条「为什么这个入口不可用」的说明写进状态栏（不弹假对话框）。</summary>
    /// <param name="action">可直接展示的原因（窗口层也会用它报告档位放不下之类的情况）。</param>
    public void NotifyNotImplemented(string action)
    {
        _notice = action;
        ApplyBackendState();
    }

    /// <summary>更新标签页标题上的档位提示（窗口层调）。</summary>
    public void UpdateTierLabel()
    {
        var tip = $"当前密度档：{Metrics.TierLabel}（Ctrl+0 / Ctrl+Plus / Ctrl+minus 切换）";
        ToolTipService.SetToolTip(WorkspaceTabs, tip);
        AutomationProperties.SetName(WorkspaceTabs, $"工作区标签 · {tip}");
    }

    // ------------------------------------------------------------------ 生命周期

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        CrashLog.Trace("shell.loaded.begin");
        if (_loaded)
        {
            return;
        }

        _loaded = true;

        _session = App.Backend;
        if (_session is not null)
        {
            _session.StateChanged += OnBackendStateChanged;
        }

        if (!string.Equals(Environment.GetEnvironmentVariable("SQARVO_NO_PROFILES"), "1", StringComparison.Ordinal))
        {
            _ = LoadProfilesAsync();
            CrashLog.Trace("shell.loaded.profiles");
        }
        else
        {
            CrashLog.Trace("shell.loaded.profiles-skipped");
        }
        ApplyBackendState();
        CrashLog.Trace("shell.loaded.done");
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        // WinUI 3 的 DispatcherTimer.IsEnabled 只读，启停只能走 Start/Stop。
        _healthTimer.Stop();

        if (_session is not null)
        {
            _session.StateChanged -= OnBackendStateChanged;
        }

        // 页面被重建（切密度档）或窗口关闭：取消在途 RPC，避免回调打到已经不在树上的控件。
        if (!_lifetime.IsCancellationRequested)
        {
            _lifetime.Cancel();
        }
    }

    /// <summary>读已保存的连接（文件 I/O 在线程池，不占 UI 线程）。</summary>
    private async Task LoadProfilesAsync()
    {
        try
        {
            var store = _store;
            _profiles = await Task.Run(() => store.Load()).ConfigureAwait(true);

            if (store.LastLoadNotice is { Length: > 0 } notice)
            {
                // 读失败/合并过重复条目：如实写进状态栏，不弹窗、不伪造列表。
                _notice = notice;
            }
        }
        catch (Exception error)
        {
            _profiles = Array.Empty<ConnectionProfile>();
            _notice = $"连接列表读取失败：{error.Message}";
        }

        RebuildTree();
        ApplyBackendState();
    }

    // ------------------------------------------------------------------ 渲染（唯一入口）

    /// <summary>
    /// 按 <see cref="BackendState"/> 这一份快照渲染菜单、工具条、状态栏与树。
    /// </summary>
    /// <remarks>所有「后端是否可用」的判断都走 <see cref="BackendState.IsReady"/>，别处不再判断。</remarks>
    private void ApplyBackendState()
    {
        var state = _session?.State ?? BackendState.Initial;
        var ready = state.IsReady;
        var selected = SelectedProfile();

        MenuNewConnection.IsEnabled = ready;

        // 四个种类入口：可用性读同一个后端就绪位（不各自盘状态）；当前种类的选中底/选中文字
        // 是 XAML 里的声明（IsChecked），种类切换要等对象树与元数据（#171）再接线。
        ButtonTables.IsEnabled = ready;
        ButtonViews.IsEnabled = ready;
        ButtonFunctions.IsEnabled = ready;
        ButtonQueries.IsEnabled = ready;

        var selectedConnected = selected is not null && _session?.IsConnected(selected.Key) == true;
        ButtonConnect.IsEnabled = ready && selected is not null && !selectedConnected;
        ToolTipService.SetToolTip(
            ButtonConnect,
            ButtonConnect.IsEnabled
                ? $"连接 {selected!.Name}（使用已保存的凭据）"
                : !ready
                    ? "本地后端尚未就绪：连接入口在状态栏显示「已连接」之后才能用"
                    : selectedConnected
                        ? $"{selected!.Name} 已经连着了：断开入口在后续工作包（浏览会话与对象树一起做）"
                        : "先在左侧对象树里选中一条已保存的连接");

        // 状态栏右格：后端自报的真实计数（未就绪时不写假数字）。
        LastRefreshText.Text = state.StatusLine;
        ToolTipService.SetToolTip(
            LastRefreshText,
            state.Code.Length == 0 ? state.Message : $"{state.Message} [{state.Code}]");

        // 状态栏左格：默认是「SQL: —」；未就绪或刚操作完时说明原因（可选中复制）。
        SqlSummaryText.Text = _notice ?? (ready ? "SQL: —" : state.Message);

        SetWorkspaceState(
            ready
                ? WorkspaceState.Empty
                : state.Readiness == BackendReadiness.Starting
                    ? WorkspaceState.Loading
                    : WorkspaceState.Disconnected);

        // 只有就绪时才轮询健康：断连后停在 Disconnected 并保留原因，不静默重启后端。
        if (ready)
        {
            _healthTimer.Start();
        }
        else
        {
            _healthTimer.Stop();
        }

        UpdateConnectedNodes();
    }

    private async void OnHealthTick(object? sender, object e)
    {
        if (_session is null || _healthInFlight || !_session.State.IsReady)
        {
            return;
        }

        _healthInFlight = true;
        try
        {
            await _session.RefreshHealthAsync(_lifetime.Token);
        }
        catch (OperationCanceledException)
        {
            // 页面重建或退出：正常路径。
        }
        finally
        {
            _healthInFlight = false;
        }
    }

    private void OnBackendStateChanged(object? sender, BackendState state)
    {
        // 事件可能来自线程池（RPC 续体），一律切回 UI 线程再改控件。
        DispatcherQueue.TryEnqueue(ApplyBackendState);
    }

    // ------------------------------------------------------------------ 对象树

    private void RebuildTree()
    {
        if (ObjectTree.RootNodes.Count == 0)
        {
            return;
        }

        var root = ObjectTree.RootNodes[0];
        root.Children.Clear();
        _nodes.Clear();
        _badges.Clear();

        foreach (var profile in _profiles)
        {
            var badge = BadgeFor(profile);
            var node = new TreeViewNode
            {
                Content = BuildConnectionRow(profile, badge),
                IsExpanded = false,
                HasUnrealizedChildren = false,
            };

            _badges[profile.Key] = badge;
            _nodes[profile.Key] = node;
            root.Children.Add(node);
        }

        root.IsExpanded = true;
    }

    /// <summary>刷新每个连接节点的徽标与「加载态」子树（不重建节点，展开状态与选中都保留）。</summary>
    private void UpdateConnectedNodes()
    {
        foreach (var profile in _profiles)
        {
            if (!_nodes.TryGetValue(profile.Key, out var node))
            {
                continue;
            }

            var connected = _session?.IsConnected(profile.Key) == true;

            // 徽标变了才重画那一行：健康轮询每 5 秒一次，整树重画会把焦点与展开状态抖掉。
            var badge = BadgeFor(profile);
            if (!_badges.TryGetValue(profile.Key, out var previous) || !string.Equals(previous, badge, StringComparison.Ordinal))
            {
                node.Content = BuildConnectionRow(profile, badge);
                _badges[profile.Key] = badge;
            }

            if (connected && node.Children.Count == 0)
            {
                node.Children.Add(new TreeViewNode { Content = LoadingPlaceholder });
                node.IsExpanded = true;
            }
            else if (!connected && node.Children.Count > 0)
            {
                node.Children.Clear();
            }
        }
    }

    /// <summary>
    /// 画一行连接节点：图标位 + 名称 + 右对齐徽标，行高 <c>row_height_tree</c>。
    /// </summary>
    /// <remarks>
    /// 形状在 <c>Views/ConnectionRow.xaml</c>（颜色只能从 ThemeResource 来，所以必须留在 XAML），
    /// 这里只负责把连接塞进去并挂上 <c>Tag</c>——它在 <see cref="ResolveProfile"/> 里把
    /// 「点了哪一行」换回「哪条连接」，不依赖 Content 的类型。
    /// </remarks>
    private static object BuildConnectionRow(ConnectionProfile profile, string badge)
    {
        if (string.Equals(Environment.GetEnvironmentVariable("SQARVO_SAFE_TREE"), "1", StringComparison.Ordinal))
        {
            CrashLog.Trace("tree.safe-mode");
            return $"{profile.Name} · {badge}";
        }

        try
        {
            var row = new ConnectionRow { Tag = profile };
            row.Show(profile.Name, badge);
            return row;
        }
        catch (Exception error)
        {
            // 行模板加载失败不能让整棵树（乃至进程）挂掉：退回纯文本，把原因写进崩溃日志。
            ConsoleBridge.WriteDiagnostic($"[tree] 连接行模板加载失败（{error.GetType().Name}: {error.Message}）");
            CrashLog.Write("tree-row", error);
            return $"{profile.Name} · {badge}";
        }
    }

    private string BadgeFor(ConnectionProfile profile)
    {
        if (_connectingKeys.Contains(profile.Key))
        {
            return "连接中…";
        }

        return _session?.IsConnected(profile.Key) == true ? "已连接" : "未连接";
    }

    /// <summary>
    /// 当前选中的连接。节点的 Content 是拼出来的 <see cref="ConnectionRow"/>，所以按「节点身份」
    /// 反查连接，不再依赖 Content 的类型（那是形状，不是数据）。
    /// </summary>
    private ConnectionProfile? SelectedProfile()
    {
        var selected = ObjectTree.SelectedNode;
        if (selected is null)
        {
            return null;
        }

        foreach (var profile in _profiles)
        {
            if (_nodes.TryGetValue(profile.Key, out var node) && ReferenceEquals(node, selected))
            {
                return profile;
            }
        }

        return null;
    }

    private void OnTreeSelectionChanged(TreeView sender, TreeViewSelectionChangedEventArgs args)
        => ApplyBackendState();

    private async void OnTreeItemInvoked(TreeView sender, TreeViewItemInvokedEventArgs args)
    {
        // InvokedItem 在「手工建节点」时可能是节点本身，也可能是节点的 Content（我们画的 ConnectionRow），
        // 两种都认。认不出来（例如点了根节点 `My Connections`）就什么都不做——绝不拿「当前选中项」凑数，
        // 那会让点一下标题就去连上一个不相干的连接。
        var profile = ResolveProfile(args.InvokedItem)
            ?? (ObjectTree.SelectedNode?.Depth > 0 ? SelectedProfile() : null);
        if (profile is null)
        {
            return;
        }

        // 已连接 → 展开/收起（与 Linux 客户端同一个交互）；未连接 → 用已保存的凭据连接。
        if (_session?.IsConnected(profile.Key) == true)
        {
            if (_nodes.TryGetValue(profile.Key, out var existing))
            {
                existing.IsExpanded = !existing.IsExpanded;
            }

            return;
        }

        await ConnectProfileAsync(profile);
    }

    private ConnectionProfile? ResolveProfile(object? item) => item switch
    {
        ConnectionProfile profile => profile,
        TreeViewNode node => ResolveProfile(node.Content),
        ConnectionRow { Tag: ConnectionProfile tagged } => tagged,
        _ => null,
    };

    private async Task ConnectProfileAsync(ConnectionProfile profile)
    {
        if (_session is null || _connecting)
        {
            return;
        }

        if (!_session.State.IsReady)
        {
            _notice = $"本地后端尚未就绪：{_session.State.Message}";
            ApplyBackendState();
            return;
        }

        _connecting = true;
        _connectingKeys.Add(profile.Key);
        _notice = $"正在连接 {profile.Name}…";
        ApplyBackendState();

        try
        {
            var open = await _session.ConnectAsync(profile, _lifetime.Token);
            var version = string.IsNullOrWhiteSpace(open.ServerVersion) ? "未知" : open.ServerVersion;
            _notice = $"{profile.Name} 已连接：驱动 {open.DriverId} · 服务端 {version}";

            // 立刻刷一次健康，让状态栏的「浏览会话」马上反映新会话（不必等 5 秒）。
            await _session.RefreshHealthAsync(_lifetime.Token);
        }
        catch (OperationCanceledException)
        {
            _notice = null;
        }
        catch (Exception error)
        {
            _notice = $"{profile.Name} 连接失败：{SqarvoFailure.FromException(error)}";
        }
        finally
        {
            _connecting = false;
            _connectingKeys.Remove(profile.Key);
            ApplyBackendState();
        }
    }

    // ------------------------------------------------------------------ 新建连接

    private async Task ShowNewConnectionDialogAsync()
    {
        if (_session is null || _dialogOpen)
        {
            return;
        }

        if (!_session.State.IsReady)
        {
            _notice = $"本地后端尚未就绪：{_session.State.Message}";
            ApplyBackendState();
            return;
        }

        _dialogOpen = true;
        try
        {
            var dialog = new ConnectionDialog(_session, _store) { XamlRoot = XamlRoot };
            var result = await dialog.ShowAsync();

            if (result != ContentDialogResult.Primary || dialog.Attempt is not { } attempt)
            {
                return;
            }

            // 保存成功：重新读盘（唯一真值仍是文件），让新条目按身份出现在树里。
            _profiles = await Task.Run(() => _store.Load()).ConfigureAwait(true);
            RebuildTree();

            // 选中刚连上的那条：用户一眼看到「连的是哪一个」，工具条「连接」也随之说明状态。
            if (_nodes.TryGetValue(attempt.Profile.Key, out var created))
            {
                ObjectTree.SelectedNode = created;
            }

            var version = string.IsNullOrWhiteSpace(attempt.Open.ServerVersion) ? "未知" : attempt.Open.ServerVersion;
            _notice = $"{attempt.Profile.Name} 已连接：延迟 {attempt.LatencyMillis} ms · 服务端 {version}";
            if (attempt.ConfigWarning is { Length: > 0 } warning)
            {
                _notice = $"{_notice}；{warning}";
            }

            await _session.RefreshHealthAsync(_lifetime.Token);
        }
        catch (Exception error)
        {
            _notice = $"新建连接失败：{SqarvoFailure.FromException(error)}";
        }
        finally
        {
            _dialogOpen = false;
            ApplyBackendState();
        }
    }

    // ------------------------------------------------------------------ 面板宽度（窗口层 / 拖动）

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

    // ------------------------------------------------------------------ 菜单 / 工具条

    private void ApplyMetrics()
    {
        CrashLog.Trace("shell.metrics.begin");
        // 菜单行高度 = 外壳高 − 系统标题栏基准 × 档位 → 24 / 30 / 36。
        // 基准由度量推出（compact 外壳 64 − 菜单行 24 = 40），这里没有尺寸字面量。
        MenuBand.Height = WindowSizing.MenuRowHeight(Metrics.Tier);

        // 侧栏 / 信息面板：默认宽度来自度量，持久化值是 compact 归一值（state 负责换算）。
        ApplyPanelWidths(_state.SidebarWidthFor(Metrics.Tier), _state.InspectorWidthFor(Metrics.Tier));

        InspectorTitle.FontSize = Metrics.FontTitle;

        // ---- 工具条：内边距是 Thickness（结构体，不能写 StaticResource），所以在这里从度量取。
        var toolbarPadding = new Thickness(Metrics.ToolbarPaddingH, 0, Metrics.ToolbarPaddingH, 0);
        ButtonConnect.Padding = toolbarPadding;
        ButtonNewQuery.Padding = toolbarPadding;

        // 种类入口的 tab 形态：圆角 radius_sm，四个入口与「新建查询」同一套内边距。
        var kindRadius = new CornerRadius(Metrics.RadiusSm);
        foreach (var kind in new[] { ButtonTables, ButtonViews, ButtonFunctions, ButtonQueries })
        {
            kind.Padding = toolbarPadding;
            kind.CornerRadius = kindRadius;
        }

        // 竖向分隔线：上下留 margin，线高 = 工具条高 − 2 × inspector_gap。
        ToolbarSeparator.Margin = new Thickness(Metrics.InspectorGap, Metrics.InspectorGap, Metrics.InspectorGap, Metrics.InspectorGap);

        // ---- 对象树：容器左内边距 grid_padding_start（行内边距在 BuildConnectionRow 里）。
        ObjectTree.Padding = new Thickness(Metrics.GridPaddingStart, 0, Metrics.GridPaddingEnd, 0);

        // ---- 标签条 / Objects 固定页：宽度 objects_min_width、高 tab_strip_height。
        ObjectsTab.MinWidth = Metrics.ObjectsMinWidth;
        ObjectsTab.MinHeight = Metrics.TabStripHeight;
        ObjectsPage.Padding = new Thickness(Metrics.GridPaddingStart, 0, Metrics.GridPaddingEnd, 0);
        ObjectsPageTitle.Margin = new Thickness(0, Metrics.GridPaddingEnd, 0, Metrics.GridPaddingEnd);

        // ---- 信息面板：三级层级共用一条左基准线（grid_padding_start）。
        InspectorHeader.Margin = new Thickness(Metrics.GridPaddingStart, Metrics.GridPaddingEnd, Metrics.GridPaddingStart, Metrics.GridPaddingEnd);
        InspectorSections.Margin = new Thickness(Metrics.GridPaddingStart, 0, Metrics.GridPaddingStart, Metrics.GridPaddingEnd);
        InspectorBody.Margin = new Thickness(Metrics.GridPaddingStart, 0, Metrics.GridPaddingEnd, Metrics.GridPaddingEnd);

        // 空态正文不横跨整块工作区（窗口 3840 宽时一行说明会拉成一条长线）：
        // 列宽取 filter_panel_width（430 / 538 / 645），是规范里唯一的「正文列宽」。
        StateDisconnected.MaxWidth = Metrics.FilterPanelWidth;
        StateLoading.MaxWidth = Metrics.FilterPanelWidth;
        StateEmpty.MaxWidth = Metrics.FilterPanelWidth;

        MenuBand.Padding = new Thickness(Metrics.ToolbarPaddingH, 0, Metrics.ToolbarPaddingH, 0);
        ToolbarBand.Padding = new Thickness(Metrics.ToolbarPaddingH, 0, Metrics.ToolbarPaddingH, 0);

        // 状态栏左边沟要跟树同一条左边界（沟宽 = 侧栏宽），所以水平内边距只留右端。
        StatusBand.Padding = new Thickness(0, 0, Metrics.ToolbarPaddingH, 0);

        UpdateTierLabel();
    }

    private void WireMenu()
    {
        // 每个入口各自说明「为什么现在不可用」，不写统一一句「尚未接入后端」。
        ToolTipService.SetToolTip(MenuNewConnection, "新建 MariaDB 连接：保存并连接后对象树出现连接节点");
        ToolTipService.SetToolTip(
            MenuNewQueryTab,
            "新建查询标签尚未实现：SQL 编辑器与查询标签在工作包 #171，现在只有固定的 Objects 页");
        ToolTipService.SetToolTip(
            MenuExportCsv,
            "导出 CSV 尚未实现：还没有结果集可以导出（查询面 #171 之后）");

        ToolTipService.SetToolTip(
            ButtonNewQuery,
            "新建查询尚未实现：SQL 编辑器在工作包 #171");
        ToolTipService.SetToolTip(
            ButtonTables,
            "Tables：在 Objects 页列出当前库的表（列表与对象树分组在工作包 #171）");
        ToolTipService.SetToolTip(
            ButtonViews,
            "Views：在 Objects 页列出当前库的视图（列表与对象树分组在工作包 #171）");
        ToolTipService.SetToolTip(
            ButtonFunctions,
            "Functions：驱动尚未提供函数列表（点开只说明，不摆假数据）");
        ToolTipService.SetToolTip(
            ButtonQueries,
            "Queries：已保存的查询尚未实现（查询标签在工作包 #171）");

        ToolTipService.SetToolTip(
            TreeSearchBox,
            "对象树搜索尚未实现：随元数据树（工作包 #171）一起提供");

        // 信息面板的分节标题：现在没有选中对象，两个分节都不可切。
        ToolTipService.SetToolTip(InspectorDdlTab, "DDL 页：先在对象树里选中一个表或视图");
        ToolTipService.SetToolTip(InspectorInfoTab, "表信息页：先在对象树里选中一个表或视图");

        // 分页器：offset 分页（首页 / 上页 / 下页 / 末页），页码只读；现在没有结果集可翻。
        ToolTipService.SetToolTip(ButtonPageFirst, "首页：还没有结果集（表数据页在工作包 #171 之后）");
        ToolTipService.SetToolTip(ButtonPagePrev, "上一页：还没有结果集（表数据页在工作包 #171 之后）");
        ToolTipService.SetToolTip(ButtonPageNext, "下一页：还没有结果集（表数据页在工作包 #171 之后）");
        ToolTipService.SetToolTip(ButtonPageLast, "末页：还没有结果集（表数据页在工作包 #171 之后）");

        // 状态栏行操作：表数据编辑与查询面都在后续工作包；逐个说明，不合并成一句。
        ToolTipService.SetToolTip(ButtonRowAdd, "新增行尚未实现：表数据编辑在后续工作包（现在没有可编辑的结果集）");
        ToolTipService.SetToolTip(ButtonRowDelete, "删除行尚未实现：表数据编辑在后续工作包（现在没有可编辑的结果集）");
        ToolTipService.SetToolTip(ButtonRowApply, "应用修改尚未实现：表数据编辑在后续工作包（没有待提交的脏行）");
        ToolTipService.SetToolTip(ButtonRowRevert, "撤销修改尚未实现：表数据编辑在后续工作包（没有待撤销的脏行）");
        ToolTipService.SetToolTip(ButtonRowRefresh, "刷新尚未实现：元数据与结果集刷新随工作包 #171");
        ToolTipService.SetToolTip(ButtonRowCancel, "取消查询尚未实现：查询面在工作包 #171");

        MenuQuit.Click += (_, _) => QuitRequested?.Invoke(this, EventArgs.Empty);
        MenuAbout.Click += OnAboutClick;
        MenuNewConnection.Click += async (_, _) => await ShowNewConnectionDialogAsync();
        MenuNewQueryTab.Click += (_, _) => NotifyNotImplemented(
            "新建查询标签尚未实现：SQL 编辑器在工作包 #171");
        MenuExportCsv.Click += (_, _) => NotifyNotImplemented(
            "导出 CSV 尚未实现：还没有结果集可以导出");

        ButtonConnect.Click += async (_, _) =>
        {
            if (SelectedProfile() is { } profile)
            {
                await ConnectProfileAsync(profile);
            }
        };

        ButtonNewQuery.Click += (_, _) => NotifyNotImplemented(
            "新建查询尚未实现：SQL 编辑器在工作包 #171");

        // 种类入口只是「显示当前种类」的切换按钮：这里只保证同一时刻最多一个高亮，
        // 不保存「当前种类」（那是页面与对象树的渲染结果，等 #171 接线）。
        ButtonTables.Checked += OnKindChecked;
        ButtonViews.Checked += OnKindChecked;
        ButtonFunctions.Checked += OnKindChecked;
        ButtonQueries.Checked += OnKindChecked;

        ObjectTree.ItemInvoked += OnTreeItemInvoked;
        ObjectTree.SelectionChanged += OnTreeSelectionChanged;

        // Ctrl+N：与「文件 → 新建连接…」同一个入口（窗口层也注册了同名快捷键，
        // 内层 accelerator 先拿到事件，设 Handled 后外层不会再执行）。
        var newConnection = new KeyboardAccelerator { Key = VirtualKey.N, Modifiers = VirtualKeyModifiers.Control };
        newConnection.Invoked += OnNewConnectionAccelerator;
        KeyboardAccelerators.Add(newConnection);
    }

    // ---- 种类入口 ----------------------------------------------------------------

    /// <summary>
    /// 保证四个种类入口同一时刻最多一个处于选中态。
    /// </summary>
    /// <remarks>
    /// 只做「互斥」这一件事：本类不保存「当前种类」，当前种类是 Objects 页 / 对象树的渲染结果
    /// （#171 接线后由那边推 IsChecked）。没有这段，用户点一下会在工具条上留下两个高亮。
    /// </remarks>
    private void OnKindChecked(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleButton checkedOne)
        {
            return;
        }

        foreach (var other in new[] { ButtonTables, ButtonViews, ButtonFunctions, ButtonQueries })
        {
            if (!ReferenceEquals(other, checkedOne) && other.IsChecked == true)
            {
                other.IsChecked = false;
            }
        }
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

    private async void OnNewConnectionAccelerator(
        KeyboardAccelerator sender,
        KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        await ShowNewConnectionDialogAsync();
    }

    /// <summary>
    /// 「关于 Sqarvo」：只写真实信息（前端信息 + 后端真实状态/版本），不编端点与数据。
    /// </summary>
    private async void OnAboutClick(object sender, RoutedEventArgs args)
    {
        var state = _session?.State ?? BackendState.Initial;
        var version = state.Version is { } backend
            ? $"{backend.ServiceVersion}（协议 {backend.ProtocolVersion}，{backend.BuildProfile}）"
            : "未知";

        var dialog = new ContentDialog
        {
            Title = "关于 Squarvo",
            Content =
                "Sqarvo · Windows 前端\n" +
                "原生数据库客户端（WinUI 3 + Windows App SDK + C#）\n" +
                $"后端状态：{state.Describe()}\n" +
                $"后端版本：{version}\n" +
                $"驱动：{state.Drivers.Count} 个" +
                (state.Drivers.Count == 0 ? string.Empty : "（" + string.Join("、", DriverLabels(state)) + "）") + "\n" +
                "已实现：连接配置、测试连接、建立/断开会话、对象树连接层。\n" +
                "未实现：元数据树（#170）、SQL 编辑器与结果集（#171）、表数据编辑与导出。\n" +
                $"当前密度档：{Metrics.TierLabel}",
            CloseButtonText = "关闭",
            XamlRoot = XamlRoot,
        };

        await dialog.ShowAsync();
    }

    private static IEnumerable<string> DriverLabels(BackendState state)
    {
        foreach (var driver in state.Drivers)
        {
            yield return driver.Label;
        }
    }
}