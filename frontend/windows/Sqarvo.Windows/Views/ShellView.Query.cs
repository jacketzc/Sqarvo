using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Sqarvo.Core.Clients;
using Sqarvo.Services.V1;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Theming;
using Sqarvo.Windows.Workspace;
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Windows.System;
using Windows.UI.Text;

namespace Sqarvo.Windows.Views;

/// <summary>
/// <see cref="ShellView"/> 的查询面（#172）：查询标签、SQL 编辑器、执行 / 取消、结果网格与分批续拉。
/// </summary>
/// <remarks>
/// <para>
/// **只加内容，不改外壳**：#170 把六区域外形重写后在真机上秒级崩溃（5/5、3/3）已整体回退，
/// 所以这里一行 XAML 都不加——查询页整体在代码里用**已经在用的那几种控件**（Grid / Border /
/// Rectangle / TextBlock / TextBox / ScrollViewer / Button / TabViewItem）搭出来，工具条、菜单行、
/// 标签条与状态栏的外形一个像素都不动（状态栏只是换文案）。
/// </para>
/// <para>
/// **不依赖「改属性会自动重画」**：WinUI 3 的 TreeView / TabView 换掉数据对象后不会重画，
/// 所以结果区每次状态变化都**重建**行容器（<see cref="RenderQueryRows"/> 里先 clear 再建），
/// 表头同样重建。行数是虚拟化的：只建视口内的那几十行，上下用占位行撑出总高度。
/// </para>
/// <para>
/// **资源纪律**（8 并发上限 + 每次执行结束必须 Close）：
/// <list type="bullet">
/// <item><description>一个标签同一时刻最多 1 个游标；标签上限 8，因此进程内游标不会超过 8。</description></item>
/// <item><description>重新执行 → 先 Close 旧游标；取消 → Cancel 后 Close；关页 → 取消在途请求后 Close；
/// 读到底（<c>complete</c>）→ 服务端已释放，句柄随即丢弃；切页 → 若有在途执行，取消并 Close
/// （见 <see cref="OnWorkspaceTabSelectionChanged"/> 的注释）。</description></item>
/// <item><description>页面被重建（切密度档）或窗口关闭：<c>Unloaded</c> 里取消全部在途请求并 Close 全部游标。</description></item>
/// </list>
/// </para>
/// </remarks>
public sealed partial class ShellView
{
    /// <summary>Close 的独立超时：句柄释放不能把界面卡住（与 <see cref="QueryHandle"/> 的 dispose 同一取舍）。</summary>
    private static readonly TimeSpan CursorCloseTimeout = TimeSpan.FromSeconds(2);

    /// <summary>服务端执行超时：一元 <c>Execute</c> 没有回传取消通道，长语句只能靠 deadline 兜住。</summary>
    private static readonly TimeSpan ExecutionTimeout = TimeSpan.FromSeconds(30);

    /// <summary>视口高度还量不到时先按这么多行建容器（数量，不是尺寸）。</summary>
    private const int MinimumRenderedRows = 20;

    /// <summary>视口上下各多建几行，滚动时不会露出空白。</summary>
    private const int OverscanRows = 4;

    /// <summary>VK_OEM_PERIOD（<c>.</c>）：<c>Windows.System.VirtualKey</c> 没有具名成员，只能按码值判。</summary>
    private const int OemPeriodVirtualKey = 0xBE;

    private readonly Dictionary<TabViewItem, QueryTab> _queryTabs = new();

    /// <summary>下一个查询标签序号（1 起，**不复用**已关闭的序号）。</summary>
    private int _nextQueryNumber = 1;

    /// <summary>当前选中的查询页（<see langword="null"/> = 停在 Objects 页）。</summary>
    private QueryTab? _activeQueryTab;

    // ------------------------------------------------------------------ 标签

    /// <summary>
    /// 新建查询标签（工具条「新建查询」、菜单「文件 → 新建查询标签」、`Ctrl+T`、QA 共用这一条路径）。
    /// </summary>
    /// <param name="sql">预填到编辑器的 SQL（QA 用；用户路径为 <see langword="null"/>）。</param>
    /// <param name="select">建好后是否切到这一页。</param>
    /// <returns>新建的标签；被上限或「未连接」拒绝时为 <see langword="null"/>（原因写在状态栏）。</returns>
    private QueryTab? CreateQueryTab(string? sql = null, bool select = true)
    {
        if (_queryTabs.Count >= QueryLimits.MaxTabs)
        {
            _notice = $"无法新建标签：标签上限 {QueryLimits.MaxTabs} 个（当前已开 {_queryTabs.Count} 个）：请先关闭一个标签再新建";
            ApplyBackendState();
            return null;
        }

        if (_session is null || !_session.State.IsReady)
        {
            _notice = "无法新建查询标签：本地后端尚未就绪（等状态栏显示「已连接」后重试）";
            ApplyBackendState();
            return null;
        }

        var connectionKey = ResolveConnectionKey();
        if (connectionKey is null)
        {
            _notice = "无法新建查询标签：尚未连接数据库（先在左侧对象树里选中一条已保存的连接并连接）";
            ApplyBackendState();
            return null;
        }

        var state = new QueryTabState(_nextQueryNumber++)
        {
            ConnectionKey = connectionKey,
            ConnectionName = ConnectionNameOf(connectionKey),
            CatalogName = CatalogOf(connectionKey),
            SessionId = _session.OpenResult(connectionKey)?.SessionId ?? string.Empty,
        };

        var tab = BuildQueryPage(state);

        // 先记账再赋值：TextChanged 里会用 LastEditorText 判断 `*`，顺序反了会让新建页闪一下「未保存」。
        tab.LastEditorText = sql ?? string.Empty;
        tab.Editor.Text = tab.LastEditorText;
        UpdateGutter(tab);
        UpdateQueryTabHeader(tab);

        _queryTabs[tab.Item] = tab;
        WorkspaceTabs.TabItems.Add(tab.Item);

        if (select)
        {
            WorkspaceTabs.SelectedItem = tab.Item;
        }

        _activeQueryTab = CurrentQueryTab();
        ConsoleBridge.WriteDiagnostic($"[query] 新建标签 {state.Title}（库 {state.CatalogName}，连接 {state.ConnectionName}）");
        RenderQuery(tab);
        ApplyBackendState();
        return tab;
    }

    /// <summary>当前选中的查询页（Objects 页或没有标签时为 <see langword="null"/>）。</summary>
    private QueryTab? CurrentQueryTab() =>
        WorkspaceTabs.SelectedItem is TabViewItem item && _queryTabs.TryGetValue(item, out var tab) ? tab : null;

    /// <summary>当前标签的连接：查询页自己记的键 › 树里选中的连接 › 第一条已连接的会话。</summary>
    private string? ResolveConnectionKey()
    {
        if (CurrentQueryTab() is { } active && active.State.ConnectionKey.Length > 0)
        {
            return active.State.ConnectionKey;
        }

        if (SelectedProfile() is { } profile && _session?.IsConnected(profile.Key) == true)
        {
            return profile.Key;
        }

        foreach (var key in _session?.OpenConnections.Keys ?? Enumerable.Empty<string>())
        {
            return key;
        }

        return null;
    }

    /// <summary>某条连接的当前库（信息面板 / 树里的目录层已经读到的话；读不到就空串，不编造）。</summary>
    private string CatalogOf(string connectionKey)
    {
        if (!_nodes.TryGetValue(connectionKey, out var connection))
        {
            return string.Empty;
        }

        foreach (var child in connection.Children)
        {
            if (child.Content is TreeRow { Payload: CatalogPayload payload } && payload.Reference.Catalog.Length > 0)
            {
                return payload.Reference.Catalog;
            }
        }

        return string.Empty;
    }

    /// <summary>标签名（`查询 N`，有未执行/未保存的修改时带 <c>*</c>）与 tooltip（库与连接在这里）。</summary>
    private void UpdateQueryTabHeader(QueryTab tab)
    {
        var title = tab.State.Dirty ? $"{tab.State.Title} *" : tab.State.Title;
        tab.Item.Header = title;

        var catalog = tab.State.CatalogName.Length == 0 ? "默认" : tab.State.CatalogName;
        var connection = tab.State.ConnectionName.Length == 0 ? "未知连接" : tab.State.ConnectionName;
        var detail = tab.State.Dirty ? " · 编辑器内容与最近一次执行不同" : string.Empty;
        var tip = $"{tab.State.Title} · 库 {catalog} · 连接 {connection}{detail}";
        ToolTipService.SetToolTip(tab.Item, tip);
        AutomationProperties.SetName(tab.Item, tab.State.Dirty ? $"{tab.State.Title}（未保存）" : tab.State.Title);
    }

    // ------------------------------------------------------------------ 页面的控件树

    /// <summary>一个查询页的控件引用 + 游标生命周期（只有这一份状态）。</summary>
    private sealed class QueryTab
    {
        public TabViewItem Item { get; set; } = new();

        public QueryTabState State { get; set; } = new(1);

        /// <summary>页面根（挂到 <see cref="TabViewItem.Content"/> 上的那棵树）。</summary>
        public Grid Root { get; set; } = new();

        public TextBox Editor { get; set; } = new();

        public TextBlock Gutter { get; set; } = new();

        public ScrollViewer GutterScroll { get; set; } = new();

        public Button RunButton { get; set; } = new();

        public TextBlock StateText { get; set; } = new();

        public TextBlock MetaText { get; set; } = new();

        /// <summary>表头容器（没有列时整块收起：空态不该显示一条空表头）。</summary>
        public Grid HeaderHost { get; set; } = new();

        public ScrollViewer HeaderScroll { get; set; } = new();

        public Grid HeaderGrid { get; set; } = new();

        public ScrollViewer BodyScroll { get; set; } = new();

        public Grid RowsGrid { get; set; } = new();

        public Button LoadMore { get; set; } = new();

        /// <summary>当前打开的游标（<see langword="null"/> = 没有占用并发名额）。</summary>
        public QueryHandle? Handle { get; set; }

        /// <summary>在途 RPC 的取消源（执行 / 取批共用一条）。</summary>
        public CancellationTokenSource? Cts { get; set; }

        /// <summary>页已经关掉（回调不许再碰它的控件）。</summary>
        public bool Closed { get; set; }

        /// <summary>一次执行已经开始但还没走到 <c>Execute</c>（释放旧游标的那一小段窗口也要挡住重复 Ctrl+Enter）。</summary>
        public bool RunPending { get; set; }

        /// <summary>表头与行区横向滚动同步的重入闸。</summary>
        public bool SyncingScroll { get; set; }

        /// <summary>行区实际的列数（列元数据与行的单元格数取大的那个）。</summary>
        public int ColumnCount { get; set; }

        /// <summary>编辑器内部 ScrollViewer（行号沟跟随它滚动；找不到就退化成不同步）。</summary>
        public ScrollViewer? EditorScroll { get; set; }

        /// <summary>上一次写进编辑器的文本（判断 <c>*</c> 用）。</summary>
        public string LastEditorText { get; set; } = string.Empty;
    }

    /// <summary>
    /// 建一个查询页：动作行 → 编辑器（行号 + 等宽多行）→ 五态行 → 结果表头 → 结果行区 → 「加载更多」。
    /// </summary>
    /// <remarks>
    /// 全部用已有控件类型：TextBox（编辑器）、TextBlock（行号 / 单元格 / 状态）、ScrollViewer（滚动与虚拟化视口）、
    /// Grid（列）、Button（执行 / 加载更多）、Rectangle（分隔线）。没有 DataTemplate、没有绑定、
    /// 没有 ListView——这三样在 Linux 上完全无法验证，而 WinUI 的模板/容器构造是上次崩溃的可疑区。
    /// </remarks>
    private QueryTab BuildQueryPage(QueryTabState state)
    {
        var tab = new QueryTab { State = state };

        var root = new Grid
        {
            Padding = new Thickness(Metrics.GridPaddingStart, 0, Metrics.GridPaddingEnd, 0),
        };
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(2, GridUnitType.Star) });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(3, GridUnitType.Star) });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        // ---- 动作行：执行 / 取消（同一个入口切换文案）+ 快捷键提示 ----
        var actions = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = Metrics.GridPaddingEnd,
            VerticalAlignment = VerticalAlignment.Center,
        };

        tab.RunButton = new Button
        {
            Content = "执行",
            FontSize = Metrics.FontUi,
            FontWeight = TypeTokens.Ui,
            MinWidth = Metrics.ColumnWidth / 2,
            Height = Metrics.SecondaryMinHeight,
            Padding = new Thickness(Metrics.InspectorGap, 0, Metrics.InspectorGap, 0),
        };
        var captured = tab;
        tab.RunButton.Click += (_, _) => ToggleRun(captured);
        ToolTipService.SetToolTip(tab.RunButton, "执行编辑器中的 SQL（Ctrl+Enter）；选中文本时只执行选中部分");
        AutomationProperties.SetName(tab.RunButton, "执行 SQL");
        actions.Children.Add(tab.RunButton);

        var hint = new TextBlock
        {
            Text = "Ctrl+Enter 执行 · Ctrl+. 取消 · 选中一段则只执行选中部分 · Ctrl+T 新建 / Ctrl+W 关闭",
            FontSize = Metrics.FontStatus,
            FontWeight = TypeTokens.Ui,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        if (ThemeBrush("SqarvoTextSecondaryBrush") is { } hintBrush)
        {
            hint.Foreground = hintBrush;
        }

        actions.Children.Add(hint);
        Grid.SetRow(actions, 0);
        root.Children.Add(actions);

        // ---- 编辑器：行号沟 + 等宽多行 TextBox ----
        var editorArea = new Grid { Margin = new Thickness(0, Metrics.GridPaddingEnd, 0, Metrics.GridPaddingEnd) };
        editorArea.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.RowNumberWidth) });
        editorArea.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        tab.Gutter = new TextBlock
        {
            Text = "1",
            FontFamily = MonoFont(),
            FontSize = Metrics.FontDataCell,
            FontWeight = TypeTokens.Ui,
            TextAlignment = TextAlignment.Right,
            TextWrapping = TextWrapping.NoWrap,
            IsTextSelectionEnabled = false,
            Padding = new Thickness(0, Metrics.GridPaddingEnd, Metrics.GridPaddingEnd, 0),
        };
        if (ThemeBrush("SqarvoTextSecondaryBrush") is { } gutterBrush)
        {
            tab.Gutter.Foreground = gutterBrush;
        }

        tab.GutterScroll = new ScrollViewer
        {
            Content = tab.Gutter,
            VerticalScrollBarVisibility = ScrollBarVisibility.Hidden,
            VerticalScrollMode = ScrollMode.Enabled,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            HorizontalScrollMode = ScrollMode.Disabled,
            IsTabStop = false,
            IsHitTestVisible = false,
        };
        Grid.SetColumn(tab.GutterScroll, 0);
        editorArea.Children.Add(tab.GutterScroll);

        tab.Editor = new TextBox
        {
            AcceptsReturn = true,
            TextWrapping = TextWrapping.NoWrap,
            FontFamily = MonoFont(),
            FontSize = Metrics.FontDataCell,
            FontWeight = TypeTokens.Ui,
            IsSpellCheckEnabled = false,
            PlaceholderText = "输入 SQL…（Ctrl+Enter 执行；选中一段则只执行选中部分）",
        };

        // WinUI 3 没有 TextBox.VerticalScrollBarVisibility（那是 WPF）：滚动条挂在 ScrollViewer 的
        // 附加属性上，XAML 里写 ScrollViewer.VerticalScrollBarVisibility，代码里就是 Set*。
        ScrollViewer.SetVerticalScrollBarVisibility(tab.Editor, ScrollBarVisibility.Auto);
        ScrollViewer.SetHorizontalScrollBarVisibility(tab.Editor, ScrollBarVisibility.Auto);
        AutomationProperties.SetName(tab.Editor, "SQL 编辑器");
        tab.Editor.TextChanged += (_, _) => OnQueryEditorChanged(captured);
        tab.Editor.PreviewKeyDown += (_, args) => OnQueryEditorKeyDown(captured, args);
        tab.Editor.Loaded += (_, _) => HookGutterSync(captured);
        Grid.SetColumn(tab.Editor, 1);
        editorArea.Children.Add(tab.Editor);

        Grid.SetRow(editorArea, 1);
        root.Children.Add(editorArea);

        // ---- 五态一行（Error / Cancelled 时行区仍保留已取到的数据） ----
        tab.StateText = new TextBlock
        {
            Text = state.StateText,
            FontSize = Metrics.FontStatus,
            FontWeight = TypeTokens.Ui,
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(0, 0, 0, Metrics.GridPaddingEnd),
        };
        AutomationProperties.SetName(tab.StateText, "查询状态");
        Grid.SetRow(tab.StateText, 2);
        root.Children.Add(tab.StateText);

        tab.MetaText = new TextBlock
        {
            Text = state.MetaText,
            FontSize = Metrics.FontPanel,
            FontWeight = TypeTokens.Ui,
            TextTrimming = TextTrimming.CharacterEllipsis,
            Margin = new Thickness(0, 0, 0, Metrics.GridPaddingEnd),
        };
        if (ThemeBrush("SqarvoTextSecondaryBrush") is { } metaBrush)
        {
            tab.MetaText.Foreground = metaBrush;
        }

        AutomationProperties.SetName(tab.MetaText, "结果集说明");

        // ---- 结果表头（横向与行区同步滚动，纵向固定） ----
        var headerHost = new Grid();
        tab.HeaderHost = headerHost;
        headerHost.RowDefinitions.Add(new RowDefinition { Height = new GridLength(Metrics.ColumnHeaderMinHeight) });
        headerHost.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        tab.HeaderGrid = new Grid { Height = Metrics.ColumnHeaderMinHeight };
        tab.HeaderScroll = new ScrollViewer
        {
            Content = tab.HeaderGrid,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
            VerticalScrollMode = ScrollMode.Disabled,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Hidden,
            HorizontalScrollMode = ScrollMode.Enabled,
            IsTabStop = false,
        };
        Grid.SetRow(tab.HeaderScroll, 0);
        headerHost.Children.Add(tab.HeaderScroll);

        var divider = new Rectangle
        {
            Height = Metrics.BorderWidthPxNeverScaled,
            Fill = ThemeBrush("SqarvoDividerBrush"),
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        Grid.SetRow(divider, 1);
        headerHost.Children.Add(divider);

        // ---- 结果行区：一个 ScrollViewer，内容由 RenderQueryRows 按视口重建 ----
        tab.RowsGrid = new Grid();
        tab.BodyScroll = new ScrollViewer
        {
            Content = tab.RowsGrid,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollMode = ScrollMode.Enabled,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollMode = ScrollMode.Enabled,
            IsTabStop = true,
        };
        AutomationProperties.SetName(tab.BodyScroll, "查询结果");
        tab.BodyScroll.ViewChanged += (_, _) => OnQueryBodyScrolled(captured);
        Grid.SetRow(tab.BodyScroll, 4);
        root.Children.Add(tab.BodyScroll);

        tab.HeaderScroll.ViewChanged += (_, _) => SyncQueryScroll(captured, fromBody: false);

        // ---- 「加载更多」：服务端还有下一页时才出现 ----
        tab.LoadMore = new Button
        {
            Content = LoadMoreText,
            Visibility = Visibility.Collapsed,
            FontSize = Metrics.FontUi,
            FontWeight = TypeTokens.Ui,
            Height = Metrics.SecondaryMinHeight,
            Padding = new Thickness(Metrics.InspectorGap, 0, Metrics.InspectorGap, 0),
            HorizontalAlignment = HorizontalAlignment.Left,
            Margin = new Thickness(0, Metrics.GridPaddingEnd, 0, 0),
        };
        tab.LoadMore.Click += (_, _) => _ = FetchMoreAsync(captured, reason: "加载更多");
        AutomationProperties.SetName(tab.LoadMore, "查询结果加载更多");
        Grid.SetRow(tab.LoadMore, 5);
        root.Children.Add(tab.LoadMore);

        tab.Root = root;

        var item = new TabViewItem
        {
            Header = state.Title,
            IsClosable = true,
            Content = root,
            // 标签字号取度量（font_ui）：Objects 固定页的标签也是这个字号，两种页并排不会一大一小。
            // × 是 TabView 模板里的关闭按钮，命中区由框架给（24×24 档，≥ 规范要求的 16），
            // 这里不覆盖模板（覆盖模板 = #170 的崩溃面）。
            FontSize = Metrics.FontUi,
            FontWeight = TypeTokens.Ui,
        };
        AutomationProperties.SetName(item, state.Title);
        ToolTipService.SetToolTip(item, $"{state.Title} · 查询标签（Ctrl+W 关闭）");
        tab.Item = item;

        // 「段数 / 列数 / 查询 id」那一行与表头共用一个容器：表头与行区之间不插文字，列不会错位。
        var resultsHeader = new Grid();
        resultsHeader.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        resultsHeader.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Grid.SetRow(tab.MetaText, 0);
        resultsHeader.Children.Add(tab.MetaText);
        Grid.SetRow(headerHost, 1);
        resultsHeader.Children.Add(headerHost);
        Grid.SetRow(resultsHeader, 3);
        root.Children.Add(resultsHeader);

        return tab;
    }

    // ------------------------------------------------------------------ 键盘 / 滚动

    /// <summary>编辑器里的快捷键：Ctrl+Enter 执行、Ctrl+. 取消（在**捕获阶段**处理，不会被 TextBox 吃掉）。</summary>
    private void OnQueryEditorKeyDown(QueryTab tab, KeyRoutedEventArgs args)
    {
        if (!IsControlDown())
        {
            return;
        }

        if (args.Key == VirtualKey.Enter)
        {
            args.Handled = true;
            ToggleRun(tab);
            return;
        }

        if ((int)args.Key == OemPeriodVirtualKey || args.Key == VirtualKey.Decimal)
        {
            args.Handled = true;
            _ = CancelQueryAsync(tab);
        }
    }

    /// <summary>Ctrl 是否按着（<c>InputKeyboardSource</c> 在 WinUI 3 上不可靠，这里读 KeyRoutedEventArgs 的修饰键）。</summary>
    private static bool IsControlDown()
    {
        try
        {
            var state = global::Microsoft.UI.Input.InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control);
            return state.HasFlag(global::Windows.UI.Core.CoreVirtualKeyStates.Down);
        }
        catch (Exception error)
        {
            // 读修饰键失败不该让编辑器崩：退回「当作没按 Ctrl」，用户还有「执行」按钮。
            ConsoleBridge.WriteDiagnostic($"[query] 读 Ctrl 状态失败：{error.GetType().Name}: {error.Message}");
            return false;
        }
    }

    /// <summary>行号沟跟随编辑器滚动（找不到内部 ScrollViewer 就不同步：行号仍在，只是不跟手）。</summary>
    private void HookGutterSync(QueryTab tab)
    {
        if (tab.EditorScroll is not null || tab.Closed)
        {
            return;
        }

        tab.EditorScroll = FindScrollViewer(tab.Editor);
        if (tab.EditorScroll is not { } inner)
        {
            ConsoleBridge.WriteDiagnostic($"[query] {tab.State.Title}：没找到编辑器内部 ScrollViewer，行号不跟随滚动");
            return;
        }

        inner.ViewChanged += (_, _) =>
        {
            if (tab.Closed)
            {
                return;
            }

            tab.GutterScroll.ChangeView(null, inner.VerticalOffset, null, disableAnimation: true);
        };
    }

    private static ScrollViewer? FindScrollViewer(DependencyObject root)
    {
        var count = VisualTreeHelper.GetChildrenCount(root);
        for (var index = 0; index < count; index++)
        {
            var child = VisualTreeHelper.GetChild(root, index);
            if (child is ScrollViewer viewer)
            {
                return viewer;
            }

            if (FindScrollViewer(child) is { } nested)
            {
                return nested;
            }
        }

        return null;
    }

    /// <summary>行区滚动：重画视口内的行 + 判断要不要续拉下一批。</summary>
    private void OnQueryBodyScrolled(QueryTab tab)
    {
        if (tab.Closed)
        {
            return;
        }

        SyncQueryScroll(tab, fromBody: true);
        RenderQueryRows(tab);
        MaybePrefetch(tab);
    }

    /// <summary>表头与行区的横向位置互相同步（纵向由各自负责，表头不滚）。</summary>
    private void SyncQueryScroll(QueryTab tab, bool fromBody)
    {
        if (tab.SyncingScroll || tab.Closed)
        {
            return;
        }

        tab.SyncingScroll = true;
        try
        {
            if (fromBody)
            {
                tab.HeaderScroll.ChangeView(tab.BodyScroll.HorizontalOffset, null, null, disableAnimation: true);
            }
            else
            {
                tab.BodyScroll.ChangeView(tab.HeaderScroll.HorizontalOffset, null, null, disableAnimation: true);
            }
        }
        catch (Exception error)
        {
            // 滚动同步是外观补偿，失败不该把界面带崩（表头不动，行仍可滚）。
            ConsoleBridge.WriteDiagnostic($"[query] 横向滚动同步失败：{error.GetType().Name}: {error.Message}");
        }
        finally
        {
            tab.SyncingScroll = false;
        }
    }

    // ------------------------------------------------------------------ 编辑 / 执行

    private void OnQueryEditorChanged(QueryTab tab)
    {
        if (tab.Closed)
        {
            return;
        }

        UpdateGutter(tab);

        // `*` = 编辑器内容与最近一次执行不同（v0.1 没有「保存查询」，标记的口径就是「还没执行」）。
        var dirty = !string.Equals(tab.Editor.Text ?? string.Empty, tab.LastEditorText, StringComparison.Ordinal);
        if (dirty != tab.State.Dirty)
        {
            tab.State.Dirty = dirty;
            UpdateQueryTabHeader(tab);
        }
    }

    private void UpdateGutter(QueryTab tab)
    {
        var text = tab.Editor.Text ?? string.Empty;
        var lines = 1;
        foreach (var character in text)
        {
            if (character == '\n')
            {
                lines++;
            }
        }

        var builder = new StringBuilder();
        for (var line = 1; line <= lines; line++)
        {
            if (line > 1)
            {
                builder.Append('\n');
            }

            builder.Append(line.ToString(CultureInfo.InvariantCulture));
        }

        tab.Gutter.Text = builder.ToString();
    }

    /// <summary>执行 / 取消共用的入口（按钮与快捷键同一条路径）。</summary>
    private void ToggleRun(QueryTab tab)
    {
        if (tab.State.Busy || tab.RunPending)
        {
            _ = CancelQueryAsync(tab);
            return;
        }

        _ = ExecuteQueryAsync(tab, reason: "执行");
    }

    /// <summary>执行：选中优先 → Execute → 首批 → 五态。</summary>
    private async Task ExecuteQueryAsync(QueryTab tab, string reason)
    {
        if (tab.Closed || tab.State.Busy || tab.RunPending)
        {
            // 已经在执行：忽略重复的 Ctrl+Enter，避免两条查询排在同一个会话里。
            return;
        }

        tab.RunPending = true;

        var editorTextAtExecute = tab.Editor.Text ?? string.Empty;
        var sql = QuerySql.SelectedOrAll(
            editorTextAtExecute,
            tab.Editor.SelectionStart,
            tab.Editor.SelectionLength);

        if (sql.Trim().Length == 0)
        {
            tab.RunPending = false;
            _notice = "SQL 不能为空：请输入后按 Ctrl+Enter";
            ApplyBackendState();
            return;
        }

        var queries = _session?.Query;
        if (queries is null)
        {
            tab.RunPending = false;
            tab.State.MarkFailure("本地后端尚未就绪", "backend.not_acquired");
            RenderQuery(tab);
            return;
        }

        var sessionId = _session?.OpenResult(tab.State.ConnectionKey)?.SessionId ?? string.Empty;
        if (sessionId.Length == 0)
        {
            tab.RunPending = false;
            tab.State.MarkFailure("这条连接还没有数据库会话：先连接再执行", "query.no_session");
            RenderQuery(tab);
            return;
        }

        var open = _queryTabs.Values.Count(item => item.Handle is not null);
        if (open >= QueryClient.MaxConcurrentQueries)
        {
            tab.RunPending = false;
            tab.State.MarkFailure(
                $"并发查询已达上限 {QueryClient.MaxConcurrentQueries}（其它标签占着游标）",
                "query.too_many");
            RenderQuery(tab);
            return;
        }

        // 上一次执行结束：先释放旧游标（8 个名额里不能留僵尸）。
        await ReleaseCursorAsync(tab, "重新执行").ConfigureAwait(true);

        tab.State.SessionId = sessionId;
        tab.State.BeginExecution(sql);
        tab.ColumnCount = 0;
        _notice = null;
        RenderQuery(tab);
        ConsoleBridge.WriteDiagnostic(
            $"[query] {tab.State.Title} {reason}：{QuerySql.Summary(sql, 60)}（{sql.Length} 字符）");

        var cts = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        tab.Cts = cts;

        try
        {
            var handle = await queries
                .ExecuteQueryAsync(
                    sessionId,
                    sql,
                    QueryLimits.BatchSize,
                    autoCommit: true,
                    executionTimeout: ExecutionTimeout,
                    cts.Token)
                .ConfigureAwait(true);

            if (tab.Closed || cts.IsCancellationRequested)
            {
                // 执行途中被取消 / 关页：刚拿到的句柄必须立刻释放，否则就白占一个名额。
                await CloseHandleAsync(tab, handle, "执行途中被取消").ConfigureAwait(true);
                if (!tab.Closed)
                {
                    tab.State.MarkCancelled("执行途中被取消");
                    RenderQuery(tab);
                }

                return;
            }

            tab.Handle = handle;
            tab.State.ApplyFirstBatch(handle, ReadOnlyOf(handle));
            UpdateColumnCount(tab);

            // `*` 的口径：编辑器内容与「按下执行那一刻的编辑器内容」不同（执行期间继续打字仍要标出来）。
            tab.LastEditorText = editorTextAtExecute;
            tab.State.Dirty = !string.Equals(tab.Editor.Text ?? string.Empty, editorTextAtExecute, StringComparison.Ordinal);
            UpdateQueryTabHeader(tab);
            RenderQuery(tab);

            if (!handle.HasMore)
            {
                // 首批就读完：服务端已经释放游标（complete / has_more=false），句柄随丢弃即可。
                ConsoleBridge.WriteDiagnostic($"[query] {tab.State.Title} 首批读完（{tab.State.Rows.Count} 行），游标由服务端释放");
                await DropCompletedCursorAsync(tab).ConfigureAwait(true);
                RenderQuery(tab);
            }
        }
        catch (OperationCanceledException)
        {
            tab.State.MarkCancelled("已取消");
            await ReleaseCursorAsync(tab, "取消").ConfigureAwait(true);
            if (!tab.Closed)
            {
                RenderQuery(tab);
            }
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            tab.State.MarkFailure(failure.Headline, failure.Code);
            RenderQuery(tab);
            ConsoleBridge.WriteDiagnostic($"[query] {tab.State.Title} 执行失败：[{failure.Code}] {failure.Detail}");
        }
        finally
        {
            tab.Cts = null;
            tab.RunPending = false;
            cts.Dispose();
            ApplyQueryStatus();
        }
    }

    /// <summary>取下一批（滚动到底或点「加载更多」；同一时刻最多一批在飞）。</summary>
    private async Task FetchMoreAsync(QueryTab tab, string reason)
    {
        var handle = tab.Handle;
        if (tab.Closed || handle is null || tab.State.Busy || !tab.State.HasMore)
        {
            return;
        }

        tab.State.Fetching = true;
        UpdateRunButton(tab);
        ApplyQueryStatus();

        var cts = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        tab.Cts = cts;
        try
        {
            var batch = await handle.FetchBatchAsync(QueryLimits.BatchSize, cts.Token).ConfigureAwait(true);
            tab.State.AppendBatch(batch);
            UpdateColumnCount(tab);
            ConsoleBridge.WriteDiagnostic(
                $"[query] {tab.State.Title} {reason}：+{batch.Rows.Count} 行，累计 {tab.State.Rows.Count}，has_more={batch.HasMore}");

            if (batch.Complete)
            {
                await DropCompletedCursorAsync(tab).ConfigureAwait(true);
            }

            RenderQuery(tab);
        }
        catch (OperationCanceledException)
        {
            tab.State.MarkCancelled("取下一批时被取消");
            await ReleaseCursorAsync(tab, "取消").ConfigureAwait(true);
            if (!tab.Closed)
            {
                RenderQuery(tab);
            }
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            tab.State.MarkFailure(failure.Headline, failure.Code);
            await ReleaseCursorAsync(tab, "取批失败").ConfigureAwait(true);
            if (!tab.Closed)
            {
                RenderQuery(tab);
            }
        }
        finally
        {
            tab.Cts = null;
            cts.Dispose();
            ApplyQueryStatus();
        }
    }

    /// <summary>滚动到接近末尾时续拉（与服务端 has_more 对齐，不猜）。</summary>
    private void MaybePrefetch(QueryTab tab)
    {
        if (tab.Handle is null || !tab.State.HasMore || tab.State.Busy || tab.Closed)
        {
            return;
        }

        var rowHeight = (double)Metrics.RowHeightData;
        if (rowHeight <= 0)
        {
            return;
        }

        var lastVisible = (int)Math.Floor((tab.BodyScroll.VerticalOffset + tab.BodyScroll.ViewportHeight) / rowHeight);
        if (lastVisible + QueryLimits.PrefetchMargin >= tab.State.Rows.Count)
        {
            _ = FetchMoreAsync(tab, reason: "滚动到底续拉");
        }
    }

    /// <summary>取消：先中断在途 RPC，再 Cancel + Close 游标（幂等：没有执行中的查询时不发任何 RPC）。</summary>
    private async Task CancelQueryAsync(QueryTab tab)
    {
        if (!tab.State.Busy && !tab.RunPending)
        {
            ConsoleBridge.WriteDiagnostic($"[query] {tab.State.Title} 取消被忽略：当前没有执行中的查询（幂等）");
            _notice = "当前没有执行中的查询（重复取消被忽略）";
            ApplyBackendState();
            return;
        }

        var handle = tab.Handle;
        tab.Cts?.Cancel();
        tab.State.MarkCancelled("用户取消");
        RenderQuery(tab);

        if (handle is not null)
        {
            tab.Handle = null;
            try
            {
                using var timeout = new CancellationTokenSource(CursorCloseTimeout);
                await handle.CancelAsync(timeout.Token).ConfigureAwait(true);
            }
            catch (Exception error)
            {
                ConsoleBridge.WriteDiagnostic($"[query] {tab.State.Title} Cancel 失败：{error.GetType().Name}: {error.Message}");
            }

            await CloseHandleAsync(tab, handle, "取消").ConfigureAwait(true);
        }

        if (!tab.Closed)
        {
            tab.State.MarkCursorReleased("取消");
            RenderQuery(tab);
        }

        ApplyQueryStatus();
    }

    // ------------------------------------------------------------------ 游标生命周期

    /// <summary>只读标志（<c>result_meta.read_only</c>；没有结果集时按 false 处理）。</summary>
    private static bool ReadOnlyOf(QueryHandle handle)
    {
        if (handle.Statements.Count == 0)
        {
            return false;
        }

        return handle.Statements[0].ResultMeta is { } meta && meta.ReadOnly;
    }

    /// <summary>读到底：服务端已释放游标，把句柄丢掉（<c>DisposeAsync</c> 是幂等的）。</summary>
    private async Task DropCompletedCursorAsync(QueryTab tab)
    {
        var handle = tab.Handle;
        tab.Handle = null;
        if (handle is null)
        {
            return;
        }

        await handle.DisposeAsync().ConfigureAwait(true);
        ConsoleBridge.WriteDiagnostic($"[query] {tab.State.Title} 游标已由服务端释放（complete）");
    }

    /// <summary>释放本页的游标（Close + Dispose），并把「已释放」写进结果说明。</summary>
    private async Task ReleaseCursorAsync(QueryTab tab, string reason)
    {
        var handle = tab.Handle;
        tab.Handle = null;
        if (handle is null)
        {
            return;
        }

        await CloseHandleAsync(tab, handle, reason).ConfigureAwait(true);
        tab.State.MarkCursorReleased(reason);
    }

    /// <summary>Close 一个句柄：绝不抛异常（失败只记日志，名额可能仍被占，如实写出来）。</summary>
    private async Task CloseHandleAsync(QueryTab tab, QueryHandle handle, string reason)
    {
        var closed = false;
        try
        {
            using var timeout = new CancellationTokenSource(CursorCloseTimeout);
            await handle.CloseAsync(timeout.Token).ConfigureAwait(true);
            closed = true;
        }
        catch (ObjectDisposedException)
        {
            // 页面重建/退出时后端通道已经销毁：游标随进程一起没了，不算失败（否则每次关窗都留一条假告警）。
            closed = true;
            ConsoleBridge.WriteDiagnostic($"[query] {tab.State.Title} Close（{reason}）：通道已释放，视为已关闭");
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic(
                $"[query] {tab.State.Title} Close 失败（{reason}）：{error.GetType().Name}: {error.Message}");
        }
        finally
        {
            // 幂等：即使 Close 失败也走一次 dispose（它自己再试一次，同样不抛）。
            await handle.DisposeAsync().ConfigureAwait(true);
        }

        var failureCode = handle.CloseFailure is null ? "-" : SqarvoFailure.FromException(handle.CloseFailure).Code;
        ConsoleBridge.WriteDiagnostic(
            $"[query] {tab.State.Title} Close（{reason}）：closed={closed || handle.IsClosed} 失败码={failureCode}");
    }

    /// <summary>关页：先取消在途请求，再把游标 Close 掉（不留下占名额的游标）。</summary>
    private async Task ReleaseCursorOnCloseAsync(QueryTab tab)
    {
        var handle = tab.Handle;
        tab.Handle = null;
        if (handle is null)
        {
            return;
        }

        try
        {
            using var timeout = new CancellationTokenSource(CursorCloseTimeout);
            await handle.CancelAsync(timeout.Token).ConfigureAwait(true);
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[query] {tab.State.Title} 关页 Cancel 失败：{error.GetType().Name}: {error.Message}");
        }

        await CloseHandleAsync(tab, handle, "关页").ConfigureAwait(true);
    }

    /// <summary>页面重建 / 窗口关闭：取消全部在途请求并 Close 全部游标（不等结果，不能让退出卡住）。</summary>
    private void CloseAllQueryCursors(string reason)
    {
        foreach (var tab in _queryTabs.Values.ToList())
        {
            tab.Closed = true;
            try
            {
                tab.Cts?.Cancel();
            }
            catch (ObjectDisposedException)
            {
                // 已经释放：没有在途请求可取消。
            }

            if (tab.Handle is { } handle)
            {
                tab.Handle = null;
                _ = CloseHandleAsync(tab, handle, reason);
            }
        }
    }

    // ------------------------------------------------------------------ 渲染（唯一入口）

    /// <summary>把一个查询页按状态整体重画（五态文案 + 表头 + 行区 + 加载更多）。</summary>
    private void RenderQuery(QueryTab tab)
    {
        if (tab.Closed)
        {
            return;
        }

        UpdateRunButton(tab);

        tab.StateText.Text = tab.State.StateText;
        tab.StateText.Visibility = tab.State.StateText.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
        if (ThemeBrush(tab.State.View == QueryViewState.Error ? "SqarvoWarningTextBrush" : "SqarvoTextSecondaryBrush") is { } stateBrush)
        {
            tab.StateText.Foreground = stateBrush;
        }

        AutomationProperties.SetName(tab.StateText, tab.State.StateText);

        tab.MetaText.Text = tab.State.MetaText;
        tab.MetaText.Visibility = tab.State.MetaText.Length == 0 ? Visibility.Collapsed : Visibility.Visible;

        RenderQueryHeader(tab);
        RenderQueryRows(tab);
        tab.HeaderHost.Visibility = tab.ColumnCount == 0 ? Visibility.Collapsed : Visibility.Visible;

        var loadMore = tab.State.HasMore && !tab.State.Busy;
        tab.LoadMore.Visibility = loadMore ? Visibility.Visible : Visibility.Collapsed;
        tab.LoadMore.IsEnabled = loadMore;

        UpdateQueryTabHeader(tab);
        ApplyQueryStatus();
    }

    private void UpdateRunButton(QueryTab tab)
    {
        var busy = tab.State.Busy || tab.RunPending;
        tab.RunButton.Content = busy ? "取消" : "执行";
        ToolTipService.SetToolTip(
            tab.RunButton,
            busy
                ? "取消当前查询（Ctrl+.，可重复点，幂等）"
                : "执行编辑器中的 SQL（Ctrl+Enter）；选中文本时只执行选中部分");
        AutomationProperties.SetName(tab.RunButton, busy ? "取消查询" : "执行 SQL");
    }

    /// <summary>结果表头：行号 + 每列「列名 · 原始类型」（每次整体重建，不指望属性变更自动重画）。</summary>
    private void RenderQueryHeader(QueryTab tab)
    {
        var grid = tab.HeaderGrid;
        grid.Children.Clear();
        grid.ColumnDefinitions.Clear();
        grid.Width = QueryTotalWidth(tab);

        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.RowNumberWidth) });
        for (var column = 0; column < Math.Max(1, tab.ColumnCount); column++)
        {
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.ColumnWidth) });
        }

        var number = AddQueryCell(grid, 0, "行号", secondary: true, right: true, mono: true, header: true);
        AutomationProperties.SetName(number, "行号");

        var count = Math.Max(1, tab.ColumnCount);
        for (var index = 0; index < count; index++)
        {
            var text = index < tab.State.Columns.Count ? tab.State.Columns[index].HeaderText : string.Empty;
            var cell = AddQueryCell(grid, index + 1, text, secondary: true, right: false, mono: false, header: true);
            AutomationProperties.SetName(cell, text.Length == 0 ? $"列 {index + 1}" : text);
        }

        AutomationProperties.SetName(
            grid,
            tab.ColumnCount == 0 ? "查询结果表头（无列）" : $"查询结果表头 {tab.ColumnCount} 列");
    }

    /// <summary>
    /// 结果行区（虚拟化）：只为视口内的行建容器，上下各加一个高度占位，总高度 = 全部行。
    /// </summary>
    /// <remarks>
    /// 每次滚动都**重建**（先 clear 再建）：WinUI 不会因为单元格 Text 变了就重画整行，
    /// 而这里换的是整批容器，重建反而最省心。行数上万时也只存在几十个 Grid。
    /// </remarks>
    private void RenderQueryRows(QueryTab tab)
    {
        var grid = tab.RowsGrid;
        grid.Children.Clear();
        grid.RowDefinitions.Clear();
        grid.ColumnDefinitions.Clear();
        grid.Width = QueryTotalWidth(tab);

        var columns = Math.Max(1, tab.ColumnCount);
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.RowNumberWidth) });
        for (var column = 0; column < columns; column++)
        {
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.ColumnWidth) });
        }

        var rows = tab.State.Rows;
        if (rows.Count == 0)
        {
            return;
        }

        var rowHeight = (double)Metrics.RowHeightData;
        if (rowHeight <= 0)
        {
            return;
        }

        var viewport = tab.BodyScroll.ViewportHeight > 0
            ? tab.BodyScroll.ViewportHeight
            : rowHeight * MinimumRenderedRows;

        var first = (int)Math.Floor(tab.BodyScroll.VerticalOffset / rowHeight) - OverscanRows;
        first = Math.Max(0, Math.Min(first, rows.Count - 1));

        var wanted = (int)Math.Ceiling(viewport / rowHeight) + OverscanRows * 2;
        var last = Math.Min(rows.Count, first + Math.Max(1, wanted));

        var rowIndex = 0;
        if (first > 0)
        {
            grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(first * rowHeight) });
            var spacer = new Border { Height = first * rowHeight, Background = ThemeBrush("SqarvoTransparentBrush") };
            Grid.SetRow(spacer, rowIndex);
            Grid.SetColumnSpan(spacer, columns + 1);
            grid.Children.Add(spacer);
            rowIndex++;
        }

        for (var index = first; index < last; index++)
        {
            grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(rowHeight) });
            var row = NewQueryRow(tab, index, rows[index], columns);
            Grid.SetRow(row, rowIndex++);

            // 必须跨满所有列：行容器自带「行号列 + N 个数据列」的列定义，但不设 ColumnSpan 时它只落在
            // 外层网格的第 0 列（宽度 = RowNumberWidth），数据列被裁成 0 宽——真机表现就是「行号有、
            // 单元格全空」。
            Grid.SetColumnSpan(row, columns + 1);
            grid.Children.Add(row);
        }

        if (last < rows.Count)
        {
            var remaining = (rows.Count - last) * rowHeight;
            grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(remaining) });
            var spacer = new Border { Height = remaining, Background = ThemeBrush("SqarvoTransparentBrush") };
            Grid.SetRow(spacer, rowIndex);
            Grid.SetColumnSpan(spacer, columns + 1);
            grid.Children.Add(spacer);
        }
    }

    private Grid NewQueryRow(QueryTab tab, int index, IReadOnlyList<string> cells, int columns)
    {
        var grid = new Grid { Height = Metrics.RowHeightData };

        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.RowNumberWidth) });
        for (var column = 0; column < columns; column++)
        {
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.ColumnWidth) });
        }

        AddQueryCell(grid, 0, (index + 1).ToString(CultureInfo.InvariantCulture), secondary: true, right: true, mono: true);

        for (var column = 0; column < columns; column++)
        {
            var text = column < cells.Count ? cells[column] : string.Empty;
            var numeric = column < tab.State.Columns.Count && tab.State.Columns[column].Numeric;
            AddQueryCell(
                grid,
                column + 1,
                text,
                secondary: QueryCellText.IsWeak(text),
                right: numeric,
                mono: false,
                weak: QueryCellText.IsWeak(text));
        }

        // 整行可访问名：--report-shell 断言「行号 + 首列」就靠它。
        var preview = cells.Count == 0 ? string.Empty : $" {cells[0]}";
        AutomationProperties.SetName(grid, $"第 {index + 1} 行{preview}");
        return grid;
    }

    private TextBlock AddQueryCell(
        Grid grid,
        int column,
        string text,
        bool secondary,
        bool right,
        bool mono,
        bool weak = false,
        bool header = false)
    {
        var block = new TextBlock
        {
            Text = text,
            FontSize = mono ? Metrics.FontStatus : Metrics.FontDataCell,
            // 列头 title(600) / 行号 ui(400) / 数据 cell(500)：与 Linux 的
            // columnview header（fw_title）与 .sqarvo-cell（fw_cell）同一口径。
            FontWeight = header ? TypeTokens.Title : mono ? TypeTokens.Ui : TypeTokens.Cell,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = right ? HorizontalAlignment.Right : HorizontalAlignment.Left,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextWrapping.NoWrap,
            IsTextSelectionEnabled = false,
        };

        if (mono)
        {
            block.FontFamily = MonoFont();
        }

        if (weak)
        {
            // NULL / 空串：弱色 + 斜体，和「真的有值」的格子一眼能分开（真机上只用二级色看不出差别）。
            block.FontStyle = FontStyle.Italic;
        }

        var brushKey = weak
            ? "SqarvoTextTertiaryBrush"
            : secondary ? "SqarvoTextSecondaryBrush" : "SqarvoTextPrimaryBrush";
        if (ThemeBrush(brushKey) is { } brush)
        {
            block.Foreground = brush;
        }

        Grid.SetColumn(block, column);
        grid.Children.Add(block);
        return block;
    }

    /// <summary>表格总宽（行号列 + 每列固定 <c>column_width</c>；超宽交给横向滚动，不挤压右侧面板）。</summary>
    private double QueryTotalWidth(QueryTab tab) =>
        Metrics.RowNumberWidth + Math.Max(1, tab.ColumnCount) * (double)Metrics.ColumnWidth;

    private void UpdateColumnCount(QueryTab tab)
    {
        var cells = tab.State.Rows.Count > 0 ? tab.State.Rows[0].Length : 0;
        tab.ColumnCount = Math.Max(tab.State.Columns.Count, cells);
    }

    // ------------------------------------------------------------------ 状态栏 / 标签事件

    /// <summary>状态栏行 2：当前是查询页时换成「SQL 摘要 + N 行 / 已取 M 行」，否则保留默认文案。</summary>
    private void ApplyQueryStatus()
    {
        var tab = CurrentQueryTab();
        var busy = tab is not null && (tab.State.Busy || tab.RunPending);

        ButtonRowCancel.IsEnabled = busy;
        ToolTipService.SetToolTip(
            ButtonRowCancel,
            busy ? "取消当前查询（Ctrl+.，幂等）" : "取消：当前没有执行中的查询");

        if (tab is null)
        {
            return;
        }

        SqlSummaryText.Text = tab.State.SqlSummaryLine;
        ToolTipService.SetToolTip(SqlSummaryText, tab.State.SqlSummaryLine);

        LastRefreshText.Text = tab.State.CountsText;
        ToolTipService.SetToolTip(
            LastRefreshText,
            tab.State.MetaText.Length == 0 ? tab.State.CountsText : tab.State.MetaText);
    }

    /// <summary>关闭标签（× / 中键 / Ctrl+W 都走这里）：对象树页不可关。</summary>
    private void CloseQueryTab(QueryTab tab)
    {
        if (tab.Closed)
        {
            return;
        }

        tab.Closed = true;

        try
        {
            tab.Cts?.Cancel();
        }
        catch (ObjectDisposedException)
        {
            // 已经释放。
        }

        _queryTabs.Remove(tab.Item);
        if (WorkspaceTabs.TabItems.Contains(tab.Item))
        {
            WorkspaceTabs.TabItems.Remove(tab.Item);
        }

        if (WorkspaceTabs.TabItems.Count > 0 && WorkspaceTabs.SelectedItem is null)
        {
            WorkspaceTabs.SelectedIndex = 0;
        }

        _activeQueryTab = CurrentQueryTab();
        ConsoleBridge.WriteDiagnostic($"[query] 关闭标签 {tab.State.Title}（已取 {tab.State.Rows.Count} 行）");
        ApplyBackendState();

        // 先把页摘掉再释放游标：关页不该等 RPC，但游标必须 Close（名额不能悬着）。
        _ = ReleaseCursorOnCloseAsync(tab);
    }

    /// <summary>切页：离开查询页时，若那一页还有在途执行，就取消并 Close（不留后台任务）。</summary>
    private void OnWorkspaceTabSelectionChanged(object sender, SelectionChangedEventArgs args)
    {
        var current = CurrentQueryTab();
        var previous = _activeQueryTab;
        _activeQueryTab = current;

        if (previous is not null && !ReferenceEquals(previous, current) && previous.State.Busy)
        {
            ConsoleBridge.WriteDiagnostic($"[query] 离开 {previous.State.Title}：取消在途执行并释放游标");
            _ = CancelQueryAsync(previous);
        }

        ApplyQueryStatus();
    }

    private void OnWorkspaceTabCloseRequested(TabView sender, TabViewTabCloseRequestedEventArgs args)
    {
        if (args.Item is TabViewItem item && _queryTabs.TryGetValue(item, out var tab))
        {
            CloseQueryTab(tab);
        }
    }

    // ------------------------------------------------------------------ 接线（构造函数调用一次）

    /// <summary>
    /// 查询面的接线：标签条的关页 / 切页事件 + 三个窗口级快捷键。
    /// </summary>
    /// <remarks>
    /// 快捷键用 <see cref="UIElement.KeyboardAccelerators"/>（与既有的 Ctrl+N 同一条路径）：不改工具条形状，
    /// 也不新增任何控件。Ctrl+Enter 在编辑器里另有捕获阶段的处理（TextBox 会把 Enter 当换行吃掉，
    /// 加速器在它后面才可能收到），两处都进同一个 <see cref="ToggleRun"/>，不会执行两次——
    /// <see cref="ExecuteQueryAsync"/> 对「已经在执行」是幂等的。
    /// </remarks>
    private void WireQuerySurface()
    {
        WorkspaceTabs.TabCloseRequested += OnWorkspaceTabCloseRequested;
        WorkspaceTabs.SelectionChanged += OnWorkspaceTabSelectionChanged;

        var newTab = new KeyboardAccelerator { Key = VirtualKey.T, Modifiers = VirtualKeyModifiers.Control };
        newTab.Invoked += (_, args) =>
        {
            args.Handled = true;
            CreateQueryTab();
        };

        KeyboardAccelerators.Add(newTab);

        var closeTab = new KeyboardAccelerator { Key = VirtualKey.W, Modifiers = VirtualKeyModifiers.Control };
        closeTab.Invoked += (_, args) =>
        {
            if (CurrentQueryTab() is not { } tab)
            {
                // Objects 页不可关（契约 §4）：快捷键在这里什么都不做，也不弹框。
                return;
            }

            args.Handled = true;
            CloseQueryTab(tab);
        };

        KeyboardAccelerators.Add(closeTab);

        // Ctrl+. 的窗口级副本：编辑器用 PreviewKeyDown 处理（TextBox 会吃掉 Enter/.），
        // 焦点在结果区 / 按钮上时靠这条加速器同样能取消。
        var cancel = new KeyboardAccelerator
        {
            Key = (VirtualKey)OemPeriodVirtualKey,
            Modifiers = VirtualKeyModifiers.Control,
        };
        cancel.Invoked += (sender, args) =>
        {
            if (CurrentQueryTab() is not { } tab)
            {
                return;
            }

            args.Handled = true;
            _ = CancelQueryAsync(tab);
        };

        KeyboardAccelerators.Add(cancel);

        var run = new KeyboardAccelerator { Key = VirtualKey.Enter, Modifiers = VirtualKeyModifiers.Control };
        run.Invoked += (_, args) =>
        {
            if (CurrentQueryTab() is not { } tab)
            {
                return;
            }

            args.Handled = true;
            ToggleRun(tab);
        };

        KeyboardAccelerators.Add(run);
    }

    /// <summary>切密度档重建外壳后，把重建前开着的查询标签按编辑器文本恢复出来。</summary>
    private void RestorePendingQueryTabs()
    {
        var pending = _pendingQuerySql;
        var selected = _pendingSelectedTab;
        _pendingQuerySql = null;
        _pendingSelectedTab = null;

        if (pending is null || pending.Count == 0)
        {
            return;
        }

        foreach (var sql in pending)
        {
            var tab = CreateQueryTab(sql, select: false);
            if (tab is null)
            {
                continue;
            }

            // 重建后什么都没执行过：按「未保存」标注（内容与最近一次执行不同），状态说明保持「尚未执行」。
            tab.LastEditorText = string.Empty;
            tab.State.Dirty = sql.Length > 0;
            UpdateQueryTabHeader(tab);
            RenderQuery(tab);
        }

        if (selected is int index && index >= 0 && index < WorkspaceTabs.TabItems.Count)
        {
            WorkspaceTabs.SelectedIndex = index;
        }

        _activeQueryTab = CurrentQueryTab();
        ApplyBackendState();
        ConsoleBridge.WriteDiagnostic($"[query] 重建后恢复 {_queryTabs.Count} 个查询标签（结果集不恢复）");
    }

    // ------------------------------------------------------------------ QA 探针（--qa-query）

    /// <summary>
    /// <c>--qa-query=&lt;sql&gt;</c>：新建一个查询页 → 执行给定 SQL → 等首批回来 →
    /// 把 <c>[query]</c> 证据行打到 stdout。走的是与用户完全相同的路径（<see cref="CreateQueryTab"/> +
    /// <see cref="ExecuteQueryAsync"/>），不是另一套快路径。
    /// </summary>
    /// <param name="sql">要执行的 SQL。</param>
    /// <param name="cancellationToken">放弃这次探针。</param>
    /// <returns>首批是否拿到了可展示的结果（有行 / 空结果都算成功，Error / Cancelled 算失败）。</returns>
    public async Task<bool> ProbeQueryAsync(string sql, int pages, CancellationToken cancellationToken)
    {
        try
        {
            var tab = CreateQueryTab(sql);
            if (tab is null)
            {
                ConsoleBridge.WriteLine("[query] tab=missing（标签被上限或「未连接」拒绝）");
                return false;
            }

            await ExecuteQueryAsync(tab, reason: "QA 探针").ConfigureAwait(true);
            cancellationToken.ThrowIfCancellationRequested();

            tab.Editor.Focus(FocusState.Programmatic);

            var state = tab.State;

            // 分批续拉：走与「滚动到底」同一条 FetchMoreAsync，验证 has_more/游标/计数都对得上。
            for (var batch = 1; batch < pages && state.HasMore; batch++)
            {
                var before = state.Rows.Count;
                await FetchMoreAsync(tab, reason: $"QA 续拉第 {batch} 批").ConfigureAwait(true);
                state = tab.State;
                ConsoleBridge.WriteLine(
                    $"[query] page={batch + 1} rows={state.Rows.Count} added={state.Rows.Count - before} " +
                    $"has_more={state.HasMore} state={StateName(state.View)}");
            }

            ConsoleBridge.WriteLine(
                $"[query] tab={state.Title} state={StateName(state.View)} rows={state.Rows.Count} " +
                $"columns={state.Columns.Count} statements={state.StatementCount} has_more={state.HasMore} " +
                $"read_only={state.ReadOnlyResult} query_id={state.QueryId}");

            if (state.Columns.Count > 0)
            {
                var columns = state.Columns.Take(12).Select(column => column.HeaderText);
                ConsoleBridge.WriteLine($"[query] columns={string.Join(",", columns)}");
            }

            if (state.Rows.Count > 0)
            {
                var cells = state.Rows[0].Take(8).Select(Quote);
                ConsoleBridge.WriteLine($"[query] first-row={string.Join("|", cells)}");
            }

            if (state.Headline is { Length: > 0 })
            {
                ConsoleBridge.WriteLine($"[query] error={state.Headline} code={state.Code}");
            }

            ConsoleBridge.WriteLine($"[query] status={state.SqlSummaryLine} counts={state.CountsText}");
            ConsoleBridge.WriteLine($"[query] text={state.StateText}");
            ConsoleBridge.WriteLine(
                $"[query] open-cursors={_queryTabs.Values.Count(item => item.Handle is not null)} tabs={_queryTabs.Count} " +
                $"session={ShortSession(state.SessionId)}");

            return state.View is QueryViewState.ReadOnly or QueryViewState.Empty;
        }
        catch (OperationCanceledException)
        {
            ConsoleBridge.WriteLine("[query] cancelled");
            return false;
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteLine($"[query] failed {error.GetType().Name}: {error.Message}");
            return false;
        }
    }

    /// <summary>QA：把所有查询页关掉（走与用户点 × 同一条 <see cref="CloseQueryTab"/>），用于验证 Close 真的发生。</summary>
    /// <returns>关掉的页数。</returns>
    public int CloseAllQueryTabsForQa()
    {
        var tabs = _queryTabs.Values.ToList();
        foreach (var tab in tabs)
        {
            CloseQueryTab(tab);
        }

        return tabs.Count;
    }

    /// <summary>当前查询页数量（<c>--qa-query</c> 与诊断用）。</summary>
    public int QueryTabCount => _queryTabs.Count;

    private static string StateName(QueryViewState state) => state switch
    {
        QueryViewState.Loading => "loading",
        QueryViewState.Empty => "empty",
        QueryViewState.Error => "error",
        QueryViewState.Cancelled => "cancelled",
        _ => "read_only",
    };

    private static string Quote(string text) =>
        text.IndexOf('|') >= 0 || text.IndexOf(' ') >= 0 ? $"\"{text}\"" : text;
}