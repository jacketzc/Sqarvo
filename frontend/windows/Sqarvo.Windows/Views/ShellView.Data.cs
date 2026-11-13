using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Sqarvo.Common.V1;
using Sqarvo.Core.Clients;
using Sqarvo.Services.V1;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Theming;
using Sqarvo.Windows.Workspace;
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;
using Windows.UI.Text;

namespace Sqarvo.Windows.Views;

/// <summary>
/// <see cref="ShellView"/> 的表数据面（#173）：数据标签、offset 分页、单元格暂存、提交 / 撤销、事务状态。
/// </summary>
/// <remarks>
/// <para>
/// **只加内容，不改外壳**（#170 的教训见 <c>docs/planning/windows-frontend-parity-plan.md</c> §5.2）：
/// 数据页整体在代码里用**已经在用的那几种控件**搭（Grid / Border / Rectangle / TextBlock / TextBox /
/// ScrollViewer / Button / TabViewItem），`ShellView.xaml` 一行没改；工具条、菜单行、标签条与状态栏的
/// 外形一个像素都不动，状态栏只是换文案（行 2 右侧 = 事务状态）。
/// </para>
/// <para>
/// **规则都在 <see cref="DataPage"/> / <see cref="DataStatusText"/> 里**：这一页算不算脏、这一格为什么
/// 不能编辑、提交要不要二次确认、状态栏写什么，全部是 <c>Workspace/DataModel.cs</c> 的纯函数（本机
/// <c>win-data-probe</c> 直接断言）。本文件只负责「按结论建控件 / 发 RPC / 取消在途请求」。
/// </para>
/// <para>
/// **不指望属性变更自动重画**（WinUI 3 实测）：表头与行区每次状态变化整块重建；行容器落在
/// 「行号列 + N 个数据列」的 Grid 里，一律 <c>Grid.SetColumnSpan(row, N + 1)</c>，否则只占行号列宽、
/// 数据格被裁成 0 宽（真机症状：行号有、单元格全空）。
/// </para>
/// <para>
/// 生命周期（<see cref="OpenTableDataAsync(ObjectPayload)"/> → <see cref="CloseDataTab"/>）：
/// 打开 = `GetPrimaryKey` + `BrowseTable(offset 0)`；第一次暂存才 `Begin`（状态栏从「自动提交」变
/// 「未提交 N 行」）；`Ctrl+S` = `ApplyRowChanges`（带 `transaction_id`）→ 全部成功再 `Commit`；
/// `Ctrl+Z` = 丢弃暂存 + `Rollback`；关页 / 离开 / 关窗先取消在途请求，有未提交改动时拦一次
/// （非模态提示行），关页时回滚事务。本页**复用这条连接已有的数据库会话**（不另建会话）。
/// </para>
/// </remarks>
public sealed partial class ShellView
{
    /// <summary>事务 / 提交这类短 RPC 的独立超时（与查询游标的 Close 同一取舍：不能卡住界面）。</summary>
    private static readonly TimeSpan DataTxnTimeout = TimeSpan.FromSeconds(5);

    /// <summary>数据页上限（同时开这么多表格已经足够；与查询标签各自计数）。</summary>
    private const int MaxDataTabs = 8;

    /// <summary>精确计数的预算（毫秒）：超时只放弃统计，照常返回本页数据。</summary>
    private const uint TotalBudgetMillis = 2000;

    private readonly Dictionary<TabViewItem, DataTab> _dataTabs = new();

    /// <summary>当前选中的数据页（<see langword="null"/> = 停在 Objects / 查询页）。</summary>
    private DataTab? _activeDataTab;

    /// <summary>关窗守卫是否已经拦过一次（第二次请求放行，避免用户被自己的改动锁死在窗口里）。</summary>
    private bool _closeGuardArmed;

    /// <summary>一个数据页的控件引用 + 事务 / 请求生命周期（只有这一份状态）。</summary>
    private sealed class DataTab
    {
        public TabViewItem Item { get; set; } = new();

        public string ConnectionKey { get; init; } = string.Empty;

        public ObjectRef Target { get; init; } = new();

        public string TableName { get; init; } = string.Empty;

        public string CatalogName { get; init; } = string.Empty;

        /// <summary>主键定义（打开时读一次；后续页复用）。</summary>
        public IReadOnlyList<PrimaryKeyColumn> PrimaryKey { get; init; } = Array.Empty<PrimaryKeyColumn>();

        /// <summary>这一页的数据（建页时就有一份空状态，界面才画得出「正在读取…」）。</summary>
        public DataPage Page { get; set; } = new(string.Empty, string.Empty, new ObjectRef(), string.Empty, string.Empty);

        public Grid Root { get; set; } = new();

        public TextBlock StateText { get; set; } = new();

        public TextBlock BannerText { get; set; } = new();

        public TextBlock MetaText { get; set; } = new();

        public Grid HeaderHost { get; set; } = new();

        public ScrollViewer HeaderScroll { get; set; } = new();

        public Grid HeaderGrid { get; set; } = new();

        public ScrollViewer BodyScroll { get; set; } = new();

        public Grid RowsGrid { get; set; } = new();

        public Button Prev { get; set; } = new();

        public Button Next { get; set; } = new();

        public Button LoadMore { get; set; } = new();

        public Button Refresh { get; set; } = new();

        /// <summary>选中格（F2 从这里进入编辑；-1 = 没有选中）。</summary>
        public int SelectedRow { get; set; } = -1;

        public int SelectedColumn { get; set; } = -1;

        /// <summary>正在编辑的单元格（<see langword="null"/> = 没有编辑态）。</summary>
        public DataEdit? Edit { get; set; }

        /// <summary>编辑态对应的输入框（重画后重新挂上）。</summary>
        public TextBox? EditBox { get; set; }

        /// <summary>读页 / 提交 / Begin 的取消源（各自独立：提交不能被翻页取消，反之亦然）。</summary>
        public CancellationTokenSource? LoadCts { get; set; }

        public CancellationTokenSource? SubmitCts { get; set; }

        public CancellationTokenSource? TxnCts { get; set; }

        /// <summary>读取在飞。</summary>
        public bool Loading { get; set; }

        /// <summary>提交在飞（同一时刻最多一批）。</summary>
        public bool Submitting { get; set; }

        /// <summary>Begin 在飞（防止一次暂存触发两次 Begin）。</summary>
        public bool Beginning { get; set; }

        /// <summary>在飞的 Begin（提交前必须等它结束，否则会一边 Begin 一边自动提交）。</summary>
        public Task BeginTask { get; set; } = Task.CompletedTask;

        /// <summary>页已关掉（回调不许再碰它的控件）。</summary>
        public bool Closed { get; set; }

        /// <summary>显式事务 id（空 = 自动提交）。</summary>
        public string TransactionId { get; set; } = string.Empty;

        /// <summary>事务阶段（决定状态栏行 2 右侧文案）。</summary>
        public DataTxnPhase Phase { get; set; } = DataTxnPhase.AutoCommit;

        /// <summary>事务降级说明（例如驱动不支持显式事务 / 主键读不到）。</summary>
        public string? TxnNote { get; set; }

        /// <summary>「超过风险阈值」已经确认过一次。</summary>
        public bool RiskArmed { get; set; }

        /// <summary>关页已经拦过一次（第二次真的放弃）。</summary>
        public bool CloseConfirmed { get; set; }

        /// <summary>最近一次提交的逐行失败（页内错误列表）。</summary>
        public IReadOnlyList<DataRowFailure> Failures { get; set; } = Array.Empty<DataRowFailure>();

        /// <summary>表头与行区横向滚动同步的重入闸。</summary>
        public bool SyncingScroll { get; set; }

        /// <summary>
        /// 每个数据列的**当前宽度**（下标 = 列序号）：表头、行区与每一行都从这一份状态取
        /// <see cref="GridLength"/>，所以表头与行永远同宽。默认值是 column_width（上限）。
        /// 它挂在 DataTab 上：翻页 / 刷新 / 重新读取都会重画但不清空它；换表是新的 DataTab，天然重置。
        /// </summary>
        public List<double> ColumnWidths { get; } = new();

        /// <summary>被用户拖过 / 双击自适应过的列（「用户宽度」：重画不覆写，只换表才重置）。</summary>
        public HashSet<int> UserSizedColumns { get; } = new();

        /// <summary>正在拖的列序号（-1 = 没有在拖）。</summary>
        public int ResizeColumn { get; set; } = -1;

        /// <summary>按下时指针相对把手的 X（拖宽手势的起点）。</summary>
        public double ResizeStartX { get; set; }

        /// <summary>按下时该列的宽度（拖宽手势的起始宽度）。</summary>
        public double ResizeStartWidth { get; set; }

        /// <summary>标签名（`表名 @ 库`，有未提交改动时带 `*`）。</summary>
        public string Title
        {
            get
            {
                var catalog = CatalogName.Length == 0 ? "默认" : CatalogName;
                var dirty = Page.HasChanges ? " *" : string.Empty;
                return $"{TableName} @ {catalog}{dirty}";
            }
        }
    }

    /// <summary>编辑态：哪一行、哪一列、输入框里的草稿（重画后不丢）。</summary>
    private sealed class DataEdit
    {
        public int Row { get; init; }

        public int Column { get; init; }

        public string Draft { get; set; } = string.Empty;

        /// <summary>已经提交 / 取消（输入框的 LostFocus 迟到时不要重复暂存）。</summary>
        public bool Closed { get; set; }
    }

    /// <summary>
    /// 列宽拖拽把手：命中间件本身是一块带透明背景的 Panel，构造时把指针光标设成 SizeWestEast。
    /// WinUI 3 的 <c>Border</c> 是 sealed，不能派生；<c>ProtectedCursor</c> 又是 protected 属性，
    /// 所以从非密封的 <c>Panel</c> 派生一个最小的命中宿主来设它。这里不引入新控件类型、不改外壳形态
    /// （#170 的教训：外壳控件形态一动就可能秒崩）。
    /// </summary>
    private sealed class ColumnResizeGrip : Panel
    {
        public ColumnResizeGrip()
        {
            ProtectedCursor = InputSystemCursor.Create(InputSystemCursorShape.SizeWestEast);
        }
    }

    // ------------------------------------------------------------------ 接线（构造函数调用一次）

    /// <summary>数据面的接线点：标签事件、树入口、行操作按钮、三个窗口级快捷键。</summary>
    /// <remarks>
    /// 全部走**已有的**控件：`ObjectTree` 的双击 / 右键 / 回车、状态栏行操作组、`WorkspaceTabs` 的
    /// 关页与切页、<see cref="UIElement.KeyboardAccelerators"/>。不新增任何控件、不改 XAML。
    /// </remarks>
    private void WireDataSurface()
    {
        WorkspaceTabs.TabCloseRequested += OnDataTabCloseRequested;
        WorkspaceTabs.SelectionChanged += OnDataTabSelectionChanged;

        ObjectTree.DoubleTapped += OnTreeDoubleTappedForData;
        ObjectTree.RightTapped += OnTreeRightTapped;
        ObjectTree.KeyDown += OnTreeKeyDownForData;

        ButtonRowApply.Click += (_, _) =>
        {
            if (CurrentDataTab() is { } tab)
            {
                _ = SubmitDataAsync(tab);
            }
        };

        ButtonRowRevert.Click += (_, _) =>
        {
            if (CurrentDataTab() is { } tab)
            {
                _ = RevertDataAsync(tab);
            }
        };

        // 状态栏「−」与数据行右键菜单共用同一条删除方法（只标记、不写库）。
        ButtonRowDelete.Click += (_, _) =>
        {
            if (CurrentDataTab() is { } tab)
            {
                DeleteSelectedRows(tab);
            }
        };

        ButtonRowRefresh.Click += (_, _) =>
        {
            if (CurrentDataTab() is { } tab)
            {
                _ = RefreshDataAsync(tab);
            }
        };

        ButtonRowCancel.Click += (_, _) =>
        {
            if (CurrentDataTab() is { } tab)
            {
                CancelDataRequests(tab, "用户取消");
                _notice = "已取消表数据请求（已取到的行保留在页面上）";
                ApplyDataStatus();
            }
        };

        var submit = new KeyboardAccelerator { Key = VirtualKey.S, Modifiers = VirtualKeyModifiers.Control };
        submit.Invoked += (sender, args) =>
        {
            if (CurrentDataTab() is not { } tab)
            {
                return;
            }

            args.Handled = true;
            _ = SubmitDataAsync(tab);
        };
        KeyboardAccelerators.Add(submit);

        var revert = new KeyboardAccelerator { Key = VirtualKey.Z, Modifiers = VirtualKeyModifiers.Control };
        revert.Invoked += (sender, args) =>
        {
            if (CurrentDataTab() is not { } tab)
            {
                return;
            }

            args.Handled = true;
            _ = RevertDataAsync(tab);
        };
        KeyboardAccelerators.Add(revert);

        // F5（ui-behavior-contract.md §8「刷新当前表 / 对象树」）：当前标签是表数据页就刷那张表
        // （脏行守卫照旧），否则重建整棵对象树。状态栏不提供第二个刷新入口，所以这里是唯一快捷键。
        var refresh = new KeyboardAccelerator { Key = VirtualKey.F5 };
        refresh.Invoked += (sender, args) =>
        {
            args.Handled = true;

            if (CurrentDataTab() is { } tab)
            {
                if (tab.Refresh.IsEnabled)
                {
                    _ = RefreshDataAsync(tab);
                }
                else
                {
                    _notice = "有未提交的更改：先 Ctrl+S 提交或 Ctrl+Z 放弃，再刷新";
                    ApplyDataStatus();
                }

                return;
            }

            // 不在表标签上：整棵树重建（作废已连接的层，回到连接层重读）。文案按契约写在状态行行 2 左格。
            SqlSummaryText.Text = "正在重新读取对象树…";
            ToolTipService.SetToolTip(SqlSummaryText, "F5：当前不是表数据页，所以重建整棵对象树");
            RebuildTree();
        };
        KeyboardAccelerators.Add(refresh);

        var edit = new KeyboardAccelerator { Key = VirtualKey.F2 };
        edit.Invoked += (sender, args) =>
        {
            if (CurrentDataTab() is not { } tab)
            {
                return;
            }

            args.Handled = true;
            BeginEditOnSelection(tab);
        };
        KeyboardAccelerators.Add(edit);
    }

    /// <summary>当前选中的数据页（Objects 页 / 查询页时为 <see langword="null"/>）。</summary>
    private DataTab? CurrentDataTab() =>
        WorkspaceTabs.SelectedItem is TabViewItem item && _dataTabs.TryGetValue(item, out var tab) ? tab : null;

    /// <summary>已打开的数据页数量（诊断 / QA 用）。</summary>
    public int DataTabCount => _dataTabs.Count;

    // ------------------------------------------------------------------ 树的入口（双击 / 回车 / 右键）

    /// <summary>双击树上的表 / 视图：打开数据页（契约 §2.2：双击打开，单击只选中）。</summary>
    private void OnTreeDoubleTappedForData(object sender, DoubleTappedRoutedEventArgs args)
    {
        if (NodeAt(args.OriginalSource as DependencyObject)?.Content is not TreeRow { Payload: ObjectPayload target })
        {
            return;
        }

        if (!IsBrowsable(target.Entry.Kind))
        {
            return;
        }

        args.Handled = true;
        _ = OpenTableDataAsync(target);
    }

    /// <summary>回车打开选中的表 / 视图（与双击同一条路径）。</summary>
    private void OnTreeKeyDownForData(object sender, KeyRoutedEventArgs args)
    {
        if (args.Key != VirtualKey.Enter
            || ObjectTree.SelectedNode?.Content is not TreeRow { Payload: ObjectPayload target }
            || !IsBrowsable(target.Entry.Kind))
        {
            return;
        }

        args.Handled = true;
        _ = OpenTableDataAsync(target);
    }

    /// <summary>右键表 / 视图：「浏览数据」（与双击同一条路径，不是另一套加载）。</summary>
    private void OnTreeRightTapped(object sender, RightTappedRoutedEventArgs args)
    {
        if (NodeAt(args.OriginalSource as DependencyObject) is not { } node
            || node.Content is not TreeRow { Payload: ObjectPayload target }
            || !IsBrowsable(target.Entry.Kind))
        {
            return;
        }

        if (!ReferenceEquals(ObjectTree.SelectedNode, node))
        {
            ObjectTree.SelectedNode = node;
        }

        var flyout = new MenuFlyout();
        var browse = new MenuFlyoutItem
        {
            Text = "浏览数据",
            FontSize = Metrics.FontUi,
            FontWeight = TypeTokens.Ui,
        };
        browse.Click += async (_, _) => await OpenTableDataAsync(target);
        flyout.Items.Add(browse);

        var caption = new MenuFlyoutItem
        {
            Text = $"{target.Entry.Name} · {target.Entry.KindLabel}",
            FontSize = Metrics.FontUi,
            FontWeight = TypeTokens.Ui,
            IsEnabled = false,
        };
        flyout.Items.Add(caption);

        try
        {
            flyout.ShowAt(ObjectTree, args.GetPosition(ObjectTree));
        }
        catch (Exception error)
        {
            // 右键菜单是外观补偿：拿不到坐标也不该把界面带崩（双击仍然能打开）。
            ConsoleBridge.WriteDiagnostic($"[data] 右键菜单打开失败：{error.GetType().Name}: {error.Message}");
            return;
        }

        args.Handled = true;
    }

    private static bool IsBrowsable(ObjectKind kind) =>
        kind is ObjectKind.Table or ObjectKind.View or ObjectKind.MaterializedView;

    /// <summary>从被点的元素往上找到 `TreeViewItem`，再映射回节点。</summary>
    private TreeViewNode? NodeAt(DependencyObject? element)
    {
        while (element is not null)
        {
            if (element is TreeViewItem item)
            {
                return ObjectTree.NodeFromContainer(item);
            }

            element = VisualTreeHelper.GetParent(element);
        }

        return null;
    }

    // ------------------------------------------------------------------ 打开 / 读页

    /// <summary>打开（或复用）一张表的数据页：`GetPrimaryKey` 一次 + `BrowseTable(offset 0, 200 行)`。</summary>
    /// <param name="target">树里的对象行（连接键 + 目录引用 + 对象条目）。</param>
    /// <returns>打开（或复用）的数据页；被上限 / 「未连接」拒绝时为 <see langword="null"/>。</returns>
    private async Task<DataTab?> OpenTableDataAsync(ObjectPayload target)
    {
        ArgumentNullException.ThrowIfNull(target);

        var existing = _dataTabs.Values.FirstOrDefault(tab =>
            !tab.Closed
            && string.Equals(tab.ConnectionKey, target.ConnectionKey, StringComparison.Ordinal)
            && string.Equals(tab.Target.Catalog, target.Entry.Reference.Catalog, StringComparison.Ordinal)
            && string.Equals(tab.Target.Name, target.Entry.Reference.Name, StringComparison.Ordinal));
        if (existing is not null)
        {
            // 同一张表复用已有标签（契约 §4）：只切过去，不重载、不新建会话。
            WorkspaceTabs.SelectedItem = existing.Item;
            _notice = $"{existing.TableName} 已经打开了（切到已有数据页，未重新读取）";
            ApplyBackendState();
            return existing;
        }

        if (_dataTabs.Count >= MaxDataTabs)
        {
            _notice = $"无法打开表数据：数据页上限 {MaxDataTabs} 个（当前 {_dataTabs.Count} 个）：请先关闭一个数据页";
            ApplyBackendState();
            return null;
        }

        var sessionId = _session?.OpenResult(target.ConnectionKey)?.SessionId;
        if (string.IsNullOrEmpty(sessionId))
        {
            _notice = $"无法打开 {target.Entry.Name}：这条连接还没有浏览会话（先在左侧连接，再双击表）";
            ApplyBackendState();
            return null;
        }

        var metadata = _session?.Metadata;
        if (metadata is null)
        {
            _notice = "无法打开表数据：本地后端尚未就绪";
            ApplyBackendState();
            return null;
        }

        // 对象引用原样回传（服务端给的那一份），只补 session_id。
        var reference = new ObjectRef
        {
            SessionId = sessionId,
            Catalog = target.Entry.Reference.Catalog,
            Schema = target.Entry.Reference.Schema,
            Name = target.Entry.Reference.Name,
            Kind = target.Entry.Reference.Kind,
        };

        // 主键定义：行身份的来源。读不到就按只读处理（绝不当成可编辑）。
        IReadOnlyList<PrimaryKeyColumn> primaryKey;
        string? primaryKeyNote = null;
        using (var pkCts = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token))
        {
            try
            {
                primaryKey = await metadata.GetPrimaryKeyAsync(reference, pkCts.Token).ConfigureAwait(true);
            }
            catch (OperationCanceledException)
            {
                _notice = $"{target.Entry.Name}：读取主键被取消";
                ApplyBackendState();
                return null;
            }
            catch (Exception error)
            {
                var failure = SqarvoFailure.FromException(error);
                primaryKey = Array.Empty<PrimaryKeyColumn>();
                primaryKeyNote = $"主键定义读取失败（{failure.Code}）：本页按只读处理";
            }
        }

        var catalog = target.Catalog.Catalog;
        var tab = new DataTab
        {
            ConnectionKey = target.ConnectionKey,
            Target = reference,
            TableName = target.Entry.Name,
            CatalogName = catalog,
            PrimaryKey = primaryKey,
            TxnNote = primaryKeyNote,
            Page = new DataPage(target.ConnectionKey, sessionId, reference, target.Entry.Name, catalog)
            {
                Offset = 0,
                PageSize = DataLimits.PageSize,
                Loading = true,
            },
        };

        BuildDataPage(tab);
        _dataTabs[tab.Item] = tab;
        WorkspaceTabs.TabItems.Add(tab.Item);
        WorkspaceTabs.SelectedItem = tab.Item;
        _activeDataTab = tab;

        ConsoleBridge.WriteDiagnostic(
            $"[data] 打开 {tab.TableName}（库 {(catalog.Length == 0 ? "默认" : catalog)}，主键列 {primaryKey.Count}）");

        await LoadDataPageAsync(tab, offset: 0, append: false, reason: "首页").ConfigureAwait(true);
        return tab;
    }

    /// <summary>给一个数据页建控件树（空壳；内容由 <see cref="RenderDataPage"/> 填）。</summary>
    private void BuildDataPage(DataTab tab)
    {
        var root = new Grid
        {
            Padding = new Thickness(Metrics.GridPaddingStart, 0, Metrics.GridPaddingEnd, 0),
        };
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });

        // ---- 动作行：上一页 / 下一页 / 加载更多 / 刷新 + 快捷键提示 ----
        var actions = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = Metrics.GridPaddingEnd,
            VerticalAlignment = VerticalAlignment.Center,
        };

        var captured = tab;
        tab.Prev = NewDataButton("‹ 上一页");
        tab.Prev.Click += (_, _) => _ = StepPageAsync(captured, forward: false);
        AutomationProperties.SetName(tab.Prev, "上一页");
        actions.Children.Add(tab.Prev);

        tab.Next = NewDataButton("下一页 ›");
        tab.Next.Click += (_, _) => _ = StepPageAsync(captured, forward: true);
        AutomationProperties.SetName(tab.Next, "下一页");
        actions.Children.Add(tab.Next);

        tab.LoadMore = NewDataButton(LoadMoreText);
        tab.LoadMore.Click += (_, _) => _ = FetchMoreDataAsync(captured);
        AutomationProperties.SetName(tab.LoadMore, "表数据加载更多");
        actions.Children.Add(tab.LoadMore);

        tab.Refresh = NewDataButton("刷新");
        tab.Refresh.Click += (_, _) => _ = RefreshDataAsync(captured);
        AutomationProperties.SetName(tab.Refresh, "刷新表数据");
        actions.Children.Add(tab.Refresh);

        var hint = new TextBlock
        {
            Text = "双击单元格或 F2 编辑 · Enter 暂存 · Esc 取消 · Ctrl+S 提交 · Ctrl+Z 撤销",
            FontSize = Metrics.FontStatus,
            FontWeight = TypeTokens.Ui,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            Margin = new Thickness(Metrics.InspectorGap, 0, 0, 0),
        };
        if (ThemeBrush("SqarvoTextSecondaryBrush") is { } hintBrush)
        {
            hint.Foreground = hintBrush;
        }

        actions.Children.Add(hint);
        Grid.SetRow(actions, 0);
        root.Children.Add(actions);

        // ---- 状态行（五态）----
        tab.StateText = new TextBlock
        {
            FontSize = Metrics.FontStatus,
            FontWeight = TypeTokens.Ui,
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(0, 0, 0, Metrics.GridPaddingEnd),
            Visibility = Visibility.Collapsed,
        };
        AutomationProperties.SetName(tab.StateText, "表数据状态");
        Grid.SetRow(tab.StateText, 1);
        root.Children.Add(tab.StateText);

        // ---- 常驻横幅：只读原因（永远可见，不只靠禁用暗示）----
        tab.BannerText = new TextBlock
        {
            FontSize = Metrics.FontStatus,
            FontWeight = TypeTokens.Ui,
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(0, 0, 0, Metrics.GridPaddingEnd),
            Visibility = Visibility.Collapsed,
        };
        if (ThemeBrush("SqarvoWarningTextBrush") is { } bannerBrush)
        {
            tab.BannerText.Foreground = bannerBrush;
        }

        AutomationProperties.SetName(tab.BannerText, "表数据横幅");
        Grid.SetRow(tab.BannerText, 2);
        root.Children.Add(tab.BannerText);

        // ---- 说明行 + 表头 + 行区（同一个容器：表头与行区之间不插文字，列不会错位）----
        tab.MetaText = new TextBlock
        {
            FontSize = Metrics.FontPanel,
            FontWeight = TypeTokens.Ui,
            TextTrimming = TextTrimming.CharacterEllipsis,
            Margin = new Thickness(0, 0, 0, Metrics.GridPaddingEnd),
        };
        if (ThemeBrush("SqarvoTextSecondaryBrush") is { } metaBrush)
        {
            tab.MetaText.Foreground = metaBrush;
        }

        AutomationProperties.SetName(tab.MetaText, "表数据说明");

        tab.HeaderHost = new Grid();
        tab.HeaderHost.RowDefinitions.Add(new RowDefinition { Height = new GridLength(Metrics.ColumnHeaderMinHeight) });
        tab.HeaderHost.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

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
        tab.HeaderHost.Children.Add(tab.HeaderScroll);

        var divider = new Rectangle
        {
            Height = Metrics.BorderWidthPxNeverScaled,
            Fill = ThemeBrush("SqarvoDividerBrush"),
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        Grid.SetRow(divider, 1);
        tab.HeaderHost.Children.Add(divider);

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
        AutomationProperties.SetName(tab.BodyScroll, "表数据网格");
        tab.BodyScroll.ViewChanged += (_, _) => SyncDataScroll(captured, fromBody: true);
        tab.HeaderScroll.ViewChanged += (_, _) => SyncDataScroll(captured, fromBody: false);

        var tableHost = new Grid();
        tableHost.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        tableHost.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        tableHost.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        Grid.SetRow(tab.MetaText, 0);
        tableHost.Children.Add(tab.MetaText);
        Grid.SetRow(tab.HeaderHost, 1);
        tableHost.Children.Add(tab.HeaderHost);
        Grid.SetRow(tab.BodyScroll, 2);
        tableHost.Children.Add(tab.BodyScroll);
        Grid.SetRow(tableHost, 3);
        root.Children.Add(tableHost);

        tab.Root = root;

        var item = new TabViewItem
        {
            Header = tab.Title,
            IsClosable = true,
            Content = root,
            // 标签字号取度量（font_ui）：与查询标签、Objects 固定页同一档，
            // 否则表数据标签吃框架默认 14，同类标签并排会一大一小。
            FontSize = Metrics.FontUi,
            FontWeight = TypeTokens.Ui,
        };
        AutomationProperties.SetName(item, tab.Title);
        ToolTipService.SetToolTip(item, $"{tab.Title} · 表数据标签（Ctrl+W 关闭；Ctrl+S 提交 / Ctrl+Z 撤销）");
        tab.Item = item;

        RenderDataPage(tab);
    }

    private Button NewDataButton(string text) => new()
    {
        Content = text,
        Height = Metrics.SecondaryMinHeight,
        Padding = new Thickness(Metrics.InspectorGap, 0, Metrics.InspectorGap, 0),
        FontSize = Metrics.FontStatus,
        FontWeight = TypeTokens.Ui,
    };

    /// <summary>读一页（`BrowseTable`）。首页替换、续拉追加；任何失败都保留已取到的行。</summary>
    /// <param name="tab">目标页。</param>
    /// <param name="offset">起始 offset。</param>
    /// <param name="append">是否追加（「加载更多」）。</param>
    /// <param name="reason">诊断用的原因。</param>
    private async Task LoadDataPageAsync(DataTab tab, long offset, bool append, string reason)
    {
        if (tab.Closed)
        {
            return;
        }

        var tableData = _session?.TableData;
        if (tableData is null)
        {
            _notice = "无法读取表数据：本地后端尚未就绪";
            ApplyDataStatus();
            return;
        }

        // 有未提交的更改时不允许换页 / 刷新：换页会把整页数据替换掉，改动的行就定位不到了。
        if (!append && tab.Page.HasChanges)
        {
            var dirty = tab.Page.ChangeCount;
            _notice = $"有 {dirty} 条未提交的更改：先 Ctrl+S 提交或 Ctrl+Z 撤销再翻页 / 刷新（翻页会替换整页数据）";
            ApplyDataStatus();
            return;
        }

        if (!append)
        {
            // 换页 = 整页替换（暂存已在上面的守卫里挡住，这里换成新状态对象）。
            tab.Page = new DataPage(tab.ConnectionKey, tab.Target.SessionId, tab.Target, tab.TableName, tab.CatalogName)
            {
                Offset = Math.Max(0, offset),
                PageSize = DataLimits.PageSize,
            };
        }

        var page = tab.Page;
        CancelLoad(tab);
        var cts = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        tab.LoadCts = cts;
        tab.Loading = true;
        tab.Failures = Array.Empty<DataRowFailure>();
        page.Loading = true;
        page.Headline = null;
        page.Code = null;

        RenderDataPage(tab);
        ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} {reason}：offset {offset}{(append ? "（追加）" : string.Empty)}");

        try
        {
            var response = await tableData.BrowseTableAsync(
                tab.Target,
                pageSize: DataLimits.PageSize,
                offset: (ulong)Math.Max(0, offset),
                pageToken: null,
                includeTotal: true,
                sort: null,
                filter: null,
                totalTimeout: TimeSpan.FromMilliseconds(TotalBudgetMillis),
                cancellationToken: cts.Token).ConfigureAwait(true);

            if (tab.Closed || cts.IsCancellationRequested)
            {
                return;
            }

            page.Apply(response, tab.PrimaryKey, append);
            if (tab.TxnNote is { Length: > 0 } note && page.Note is null)
            {
                page.Note = note;
            }

            ConsoleBridge.WriteDiagnostic(
                $"[data] {tab.TableName} {reason}：rows={page.Rows.Count} has_more={page.HasMore} " +
                $"read_only={page.ReadOnly} total_kind={page.TotalKind} offset={page.Offset}");
        }
        catch (OperationCanceledException)
        {
            page.Loading = false;
            if (!tab.Closed)
            {
                _notice = $"{tab.TableName}：读取已取消（已取到的行保留）";
            }
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);

            // 失败**不丢已取到的行**：只写失败态，行区照旧渲染。
            page.Loading = false;
            page.Headline = failure.Headline;
            page.Code = failure.Code;
            ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} {reason} 失败：[{failure.Code}] {failure.Detail}");
        }
        finally
        {
            tab.LoadCts = null;
            tab.Loading = false;
            cts.Dispose();
            if (!tab.Closed)
            {
                RenderDataPage(tab);
            }
        }
    }

    /// <summary>上一页 / 下一页（offset 分页；页码只读显示，不提供跳转）。</summary>
    private async Task StepPageAsync(DataTab tab, bool forward)
    {
        if (tab.Loading || tab.Submitting)
        {
            return;
        }

        var page = tab.Page;
        var target = forward ? page.Offset + page.PageSize : page.Offset - page.PageSize;
        if (target < 0)
        {
            _notice = "已经是第一页";
            ApplyDataStatus();
            return;
        }

        if (forward && !page.HasMore)
        {
            _notice = "已经是最后一页（服务端 has_more=false）：没有更多数据";
            ApplyDataStatus();
            return;
        }

        await LoadDataPageAsync(tab, target, append: false, reason: forward ? "下一页" : "上一页").ConfigureAwait(true);
    }

    /// <summary>「加载更多」：不换页，从已取行数处续拉（append）。</summary>
    private async Task FetchMoreDataAsync(DataTab tab)
    {
        if (tab.Loading || tab.Submitting)
        {
            return;
        }

        var page = tab.Page;
        if (!page.HasMore)
        {
            _notice = "没有更多数据（服务端 has_more=false）";
            ApplyDataStatus();
            return;
        }

        await LoadDataPageAsync(tab, page.NextOffset, append: true, reason: "加载更多").ConfigureAwait(true);
    }

    /// <summary>刷新当前页（清空暂存；有未提交更改时被守卫拒绝）。</summary>
    private async Task RefreshDataAsync(DataTab tab)
    {
        if (tab.Loading || tab.Submitting)
        {
            return;
        }

        if (tab.Page.HasChanges)
        {
            _notice = $"有 {tab.Page.ChangeCount} 条未提交的更改：先 Ctrl+S 提交或 Ctrl+Z 撤销再刷新";
            ApplyDataStatus();
            return;
        }

        CancelEdit(tab);
        tab.Failures = Array.Empty<DataRowFailure>();
        await LoadDataPageAsync(tab, tab.Page.Offset, append: false, reason: "刷新").ConfigureAwait(true);
    }

    // ------------------------------------------------------------------ 单元格编辑（本地暂存）

    /// <summary>选中一格（单击只选中，不改数据）。</summary>
    private void SelectCell(DataTab tab, int row, int column)
    {
        var page = tab.Page;
        if (row < 0 || row >= page.Rows.Count || column < 0 || column >= page.Columns.Count)
        {
            return;
        }

        tab.SelectedRow = row;
        tab.SelectedColumn = column;
        RenderDataRows(tab);
        // 状态栏「−」（status_bar.row1.ops.delete）的可用性是在 ApplyDataStatus 里算的，
        // 只重画行不刷新状态栏的话，选中行之后按钮仍是置灰的（实测 UIA enabled=False 永远点不动）。
        ApplyDataStatus();
    }

    /// <summary>F2：从选中格进入编辑态。</summary>
    private void BeginEditOnSelection(DataTab tab)
    {
        if (tab.SelectedRow < 0 || tab.SelectedColumn < 0)
        {
            _notice = "先单击选中一个单元格再按 F2（或双击单元格直接编辑）";
            ApplyDataStatus();
            return;
        }

        BeginCellEdit(tab, tab.SelectedRow, tab.SelectedColumn);
    }

    /// <summary>进入编辑态（双击 / F2 同一条路径）。被拒绝时给具体原因，不静默。</summary>
    private void BeginCellEdit(DataTab tab, int rowIndex, int columnIndex)
    {
        var page = tab.Page;
        if (tab.Loading || tab.Submitting || rowIndex < 0 || rowIndex >= page.Rows.Count)
        {
            return;
        }

        var target = page.Rows[rowIndex];
        if (DataStatusText.EditBlockReason(page, target, columnIndex) is { } reason)
        {
            _notice = $"{tab.TableName} 第 {target.Number} 行：{reason}";
            ApplyDataStatus();
            return;
        }

        tab.SelectedRow = rowIndex;
        tab.SelectedColumn = columnIndex;
        var current = target.Value(columnIndex);
        tab.Edit = new DataEdit
        {
            Row = rowIndex,
            Column = columnIndex,
            Draft = DataCellText.IsNull(current) ? string.Empty : DataCellText.Display(current),
        };

        RenderDataPage(tab);
    }

    /// <summary>Esc：取消编辑（不改任何值）。</summary>
    private void CancelEdit(DataTab tab)
    {
        if (tab.Edit is { } edit)
        {
            edit.Closed = true;
        }

        tab.Edit = null;
        tab.EditBox = null;
        if (!tab.Closed)
        {
            RenderDataRows(tab);
        }
    }

    /// <summary>Enter：把输入框的值提交到**本地暂存**（不落库）。</summary>
    /// <returns>是否真的暂存了一条编辑。</returns>
    private bool CommitCellEdit(DataTab tab)
    {
        if (tab.Edit is not { } edit)
        {
            return true;
        }

        var text = tab.EditBox?.Text ?? edit.Draft;
        edit.Closed = true;
        tab.Edit = null;
        tab.EditBox = null;

        var staged = StageCellValue(tab, edit.Row, edit.Column, text);
        if (!tab.Closed)
        {
            RenderDataRows(tab);
        }

        return staged;
    }

    /// <summary>解析并暂存一个单元格（用户路径与 QA 探针共用这一条）。</summary>
    /// <param name="tab">目标页。</param>
    /// <param name="rowIndex">页内行下标。</param>
    /// <param name="columnIndex">列序。</param>
    /// <param name="text">输入框里的文字。</param>
    /// <returns>是否暂存成功（失败原因写在状态栏）。</returns>
    private bool StageCellValue(DataTab tab, int rowIndex, int columnIndex, string text)
    {
        var page = tab.Page;
        if (rowIndex < 0 || rowIndex >= page.Rows.Count || columnIndex < 0 || columnIndex >= page.Columns.Count)
        {
            return false;
        }

        var row = page.Rows[rowIndex];
        if (DataStatusText.EditBlockReason(page, row, columnIndex) is { } blocked)
        {
            _notice = $"{tab.TableName} 第 {row.Number} 行：{blocked}";
            ApplyDataStatus();
            return false;
        }

        if (!DataInput.TryParse(page.Columns[columnIndex], text, out var value, out var error))
        {
            _notice = $"{page.Columns[columnIndex].Name}：{error}";
            ApplyDataStatus();
            return false;
        }

        row.Stage(columnIndex, value);
        tab.Failures = Array.Empty<DataRowFailure>();
        tab.RiskArmed = false;

        if (row.Dirty)
        {
            // 第一次暂存才开事务：没有改动就没有事务（状态栏保持「自动提交」）。
            _ = EnsureTransactionAsync(tab);
        }
        else if (!page.HasChanges)
        {
            // 改回原值 = 没有改动：顺手把已经开的事务回滚掉，别留一个空事务。
            _ = RollbackIfIdleAsync(tab);
        }

        _notice = row.Dirty
            ? $"{page.Columns[columnIndex].Name} 第 {row.Number} 行：已暂存（Ctrl+S 提交到数据库，Ctrl+Z 撤销）"
            : $"{page.Columns[columnIndex].Name} 第 {row.Number} 行：已改回原值，这一行不再算脏";
        RenderDataPage(tab);
        return true;
    }

    // ------------------------------------------------------------------ 事务

    /// <summary>
    /// 第一次暂存时开显式事务；驱动不支持就如实退回自动提交（不静默假装）。
    /// </summary>
    /// <returns>这一次 Begin 的任务（已经在飞时返回同一个任务，提交前可以直接 await）。</returns>
    private Task EnsureTransactionAsync(DataTab tab)
    {
        if (tab.Closed || tab.TransactionId.Length > 0)
        {
            return Task.CompletedTask;
        }

        if (tab.Beginning)
        {
            // 上一次暂存已经发起 Begin：复用同一个任务，避免两次 Begin 各开一个事务。
            return tab.BeginTask;
        }

        tab.BeginTask = BeginTransactionCoreAsync(tab);
        return tab.BeginTask;
    }

    /// <summary>真正发 `Begin` 的那一段（只由 <see cref="EnsureTransactionAsync"/> 调用一次）。</summary>
    private async Task BeginTransactionCoreAsync(DataTab tab)
    {
        var transactions = _session?.Transactions;
        var page = tab.Page;
        if (transactions is null || page.ReadOnly)
        {
            return;
        }

        tab.Beginning = true;
        CancelTxn(tab);
        var cts = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        tab.TxnCts = cts;
        try
        {
            var info = await transactions
                .BeginTransactionAsync(page.SessionId, IsolationLevel.Unspecified, readOnly: false, cts.Token)
                .ConfigureAwait(true);

            if (tab.Closed)
            {
                // 页已经关了：这个事务没人用，立刻回滚（不在后端留一个悬着的事务）。
                _ = RollbackQuietAsync(info.TransactionId, "页已关闭");
                return;
            }

            tab.TransactionId = info.TransactionId;
            tab.Phase = DataTxnPhase.Open;
            tab.TxnNote = null;
            ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} Begin 成功（事务 {ShortId(info.TransactionId)}）");
        }
        catch (OperationCanceledException)
        {
            // 关页 / 退出：正常路径。
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            tab.TransactionId = string.Empty;
            tab.Phase = DataTxnPhase.AutoCommit;
            tab.TxnNote = $"显式事务不可用（{failure.Code}）：提交按自动提交执行（一批一个隐式事务）";
            ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} Begin 失败：[{failure.Code}] {failure.Detail}");
        }
        finally
        {
            tab.Beginning = false;
            tab.TxnCts = null;
            cts.Dispose();
            if (!tab.Closed)
            {
                RenderDataPage(tab);
            }
        }
    }

    /// <summary>改动全被改回原值时回滚空事务（有改动就什么都不做）。</summary>
    private async Task RollbackIfIdleAsync(DataTab tab)
    {
        if (!tab.Page.HasChanges && tab.TransactionId.Length > 0 && !tab.Submitting && !tab.Beginning)
        {
            var id = tab.TransactionId;
            tab.TransactionId = string.Empty;
            await RollbackAsync(tab, id, "改动已全部改回原值").ConfigureAwait(true);
        }
    }

    private async Task RollbackAsync(DataTab tab, string transactionId, string reason)
    {
        var transactions = _session?.Transactions;
        if (transactions is null || transactionId.Length == 0)
        {
            return;
        }

        try
        {
            using var timeout = new CancellationTokenSource(DataTxnTimeout);
            await transactions.RollbackAsync(transactionId, timeout.Token).ConfigureAwait(true);
            ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} Rollback 成功（{reason}，事务 {ShortId(transactionId)}）");
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} Rollback 失败（{reason}）：[{failure.Code}] {failure.Detail}");
        }
        finally
        {
            tab.Phase = DataTxnPhase.RolledBack;
        }
    }

    /// <summary>关页 / 放弃时的「静默回滚」：不抛、不阻塞退出路径（失败只记日志）。</summary>
    private async Task RollbackQuietAsync(string transactionId, string reason)
    {
        var transactions = _session?.Transactions;
        if (transactions is null || transactionId.Length == 0)
        {
            return;
        }

        try
        {
            using var timeout = new CancellationTokenSource(DataTxnTimeout);
            await transactions.RollbackAsync(transactionId, timeout.Token).ConfigureAwait(true);
            ConsoleBridge.WriteDiagnostic($"[data] Rollback 成功（{reason}，事务 {ShortId(transactionId)}）");
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[data] Rollback 失败（{reason}）：{error.GetType().Name}: {error.Message}");
        }
    }

    // ------------------------------------------------------------------ 提交 / 撤销

    /// <summary>`Ctrl+S` / 「应用」：先关编辑态，再走风险守卫，然后 `ApplyRowChanges` → `Commit`。</summary>
    private async Task SubmitDataAsync(DataTab tab)
    {
        if (tab.Closed)
        {
            return;
        }

        if (tab.Submitting)
        {
            _notice = "提交已经在进行中（重复的 Ctrl+S 被忽略）";
            ApplyDataStatus();
            return;
        }

        // 编辑态里的值先落进暂存（与 Enter 同一条路径），否则用户会以为「刚改的那一格也一起提交了」。
        if (tab.Edit is not null && !CommitCellEdit(tab))
        {
            return;
        }

        var page = tab.Page;
        var plan = DataSubmitPlan.Build(page, tab.RiskArmed);
        if (!plan.Allowed)
        {
            tab.RiskArmed = plan.NeedsRiskConfirmation && !tab.RiskArmed;
            _notice = plan.Message;
            ApplyDataStatus();
            return;
        }

        var changes = DataChangePlan.Build(page);
        if (changes.Updates.Count == 0 && changes.Deletes.Count == 0)
        {
            _notice = "没有待提交的更改";
            ApplyDataStatus();
            return;
        }

        var tableData = _session?.TableData;
        if (tableData is null)
        {
            _notice = "无法提交：本地后端尚未就绪";
            ApplyDataStatus();
            return;
        }

        // 事务：还没开就现在开（已经有一次 Begin 在飞就等它，绝不一边 Begin 一边自动提交）。
        if (tab.TransactionId.Length == 0)
        {
            await EnsureTransactionAsync(tab).ConfigureAwait(true);
        }

        tab.RiskArmed = false;
        tab.Submitting = true;
        tab.Phase = DataTxnPhase.Committing;
        tab.Failures = Array.Empty<DataRowFailure>();
        var queryId = $"query_{Guid.NewGuid():D}";

        CancelSubmit(tab);
        var cts = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        tab.SubmitCts = cts;
        RenderDataPage(tab);

        var request = new ApplyRowChangesRequest
        {
            SessionId = page.SessionId,
            Object = page.Target,
            TransactionId = tab.TransactionId,
            QueryId = queryId,
            AcknowledgeRisk = plan.AcknowledgeRisk,
        };
        request.Updates.AddRange(changes.Updates);
        request.Deletes.AddRange(changes.Deletes);

        ConsoleBridge.WriteDiagnostic(
            $"[data] {tab.TableName} 提交：rows={changes.Rows.Count} changes={changes.ChangeCount} " +
            $"updates={changes.Updates.Count} deletes={changes.Deletes.Count} " +
            $"txn={(tab.TransactionId.Length == 0 ? "自动提交" : ShortId(tab.TransactionId))} ack={plan.AcknowledgeRisk}");

        try
        {
            var response = await tableData.ApplyRowChangesAsync(request, cts.Token).ConfigureAwait(true);
            if (tab.Closed)
            {
                return;
            }

            var report = DataApplyReport.Apply(changes, response);
            tab.Failures = report.Failures;

            if (report.Cancelled)
            {
                tab.Phase = tab.TransactionId.Length > 0 ? DataTxnPhase.Open : DataTxnPhase.AutoCommit;
                _notice = DataStatusText.SubmitSummary(report, (int)report.Deferred);
            }
            else if (report.Failures.Count == 0)
            {
                if (tab.TransactionId.Length > 0)
                {
                    var committed = await CommitTransactionAsync(tab).ConfigureAwait(true);
                    if (committed)
                    {
                        _notice = CommittedNotice(tab, page, report, "（事务已提交）");
                    }
                    else
                    {
                        _notice = $"已写入 {report.Applied} 行，但事务提交失败：改动仍在事务里（Ctrl+Z 可回滚）";
                    }
                }
                else
                {
                    tab.Phase = DataTxnPhase.AutoCommit;
                    _notice = CommittedNotice(tab, page, report, "（自动提交：本批一个隐式事务）");
                }
            }
            else
            {
                // 有失败：事务保持打开（可重试），失败的逐行原因在页内列表里。
                tab.Phase = tab.TransactionId.Length > 0 ? DataTxnPhase.Open : DataTxnPhase.AutoCommit;
                _notice = DataStatusText.SubmitSummary(report, (int)report.Deferred);
            }

            ConsoleBridge.WriteDiagnostic(
                $"[data] {tab.TableName} 提交结果：applied={report.Applied} failures={report.Failures.Count} " +
                $"cancelled={report.Cancelled} clamped={report.Clamped} deferred={report.Deferred} " +
                $"dirty={page.DirtyRowCount} txn={(tab.TransactionId.Length == 0 ? "-" : ShortId(tab.TransactionId))}");
        }
        catch (OperationCanceledException)
        {
            // 取消 = 整批回滚（ADR 0018）：界面保留脏行与编辑文本，可再次提交。
            tab.Phase = tab.TransactionId.Length > 0 ? DataTxnPhase.Open : DataTxnPhase.AutoCommit;
            _notice = "提交已取消：整批回滚，改动仍保留在页面上（可再次 Ctrl+S）";
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            tab.Phase = tab.TransactionId.Length > 0 ? DataTxnPhase.Open : DataTxnPhase.AutoCommit;
            page.Headline = failure.Headline;
            page.Code = failure.Code;
            _notice = $"提交失败：{failure.Headline}（{failure.Code}）· 改动仍保留，可重试或 Ctrl+Z 撤销";
            ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} 提交失败：[{failure.Code}] {failure.Detail}");
        }
        finally
        {
            tab.SubmitCts = null;
            tab.Submitting = false;
            cts.Dispose();
            if (!tab.Closed)
            {
                RenderDataPage(tab);
            }
        }
    }

    /// <summary>
    /// 提交成功后的收尾：只把「已执行」的删除行移出页面；被单批上限 deferred、还没执行的删除仍带标记留在
    /// 页面上，并且续传提示优先于「已提交：删除 N 行」（否则用户会以为都删完了）。
    /// </summary>
    private string CommittedNotice(DataTab tab, DataPage page, DataApplyReport report, string suffix)
    {
        var removed = page.RemoveExecutedDeletes();
        ClampSelectionAfterDelete(tab, page);

        if (report.Clamped && report.Deferred > 0)
        {
            return DataStatusText.SubmitSummary(report, (int)report.Deferred) + suffix;
        }

        return removed > 0
            ? DataStatusText.DeleteCommittedText(removed) + suffix
            : $"已提交 {report.Applied} 行{suffix}";
    }

    /// <summary>删除行移出页面后把选区夹回有效范围（删掉最后一行时退回「无选中」）。</summary>
    private static void ClampSelectionAfterDelete(DataTab tab, DataPage page)
    {
        if (page.Rows.Count == 0)
        {
            tab.SelectedRow = -1;
            tab.SelectedColumn = -1;
            return;
        }

        if (tab.SelectedRow >= page.Rows.Count)
        {
            tab.SelectedRow = page.Rows.Count - 1;
        }
    }

    /// <summary>提交事务；失败时保留事务并给出说明（不谎称已提交）。</summary>
    private async Task<bool> CommitTransactionAsync(DataTab tab)
    {
        var transactions = _session?.Transactions;
        var id = tab.TransactionId;
        if (transactions is null || id.Length == 0)
        {
            return true;
        }

        try
        {
            using var timeout = new CancellationTokenSource(DataTxnTimeout);
            await transactions.CommitAsync(id, timeout.Token).ConfigureAwait(true);
            tab.TransactionId = string.Empty;
            tab.Phase = DataTxnPhase.AutoCommit;
            ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} Commit 成功（事务 {ShortId(id)}）");
            return true;
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} Commit 失败：[{failure.Code}] {failure.Detail}");
            tab.TxnNote = $"事务提交失败（{failure.Code}）：改动仍在事务里，Ctrl+Z 可回滚";
            return false;
        }
    }

    /// <summary>`Ctrl+Z` / 「撤销」：丢弃全部本地暂存，并回滚显式事务。</summary>
    private async Task RevertDataAsync(DataTab tab)
    {
        if (tab.Closed)
        {
            return;
        }

        if (tab.Submitting)
        {
            _notice = "提交在进行中：等它结束后再撤销（或点状态栏「取消」中断这一批）";
            ApplyDataStatus();
            return;
        }

        CancelEdit(tab);
        var page = tab.Page;
        var changes = page.ChangeCount + page.MarkedDeleteCount;
        if (changes == 0 && tab.TransactionId.Length == 0)
        {
            _notice = "没有待撤销的更改";
            ApplyDataStatus();
            return;
        }

        page.DiscardAllEdits();
        tab.Failures = Array.Empty<DataRowFailure>();
        tab.RiskArmed = false;

        if (tab.TransactionId.Length > 0)
        {
            var id = tab.TransactionId;
            tab.TransactionId = string.Empty;
            await RollbackAsync(tab, id, "撤销").ConfigureAwait(true);
            _notice = $"已放弃 {changes} 条未提交的更改（事务已回滚）";
        }
        else
        {
            tab.Phase = DataTxnPhase.RolledBack;
            _notice = $"已放弃 {changes} 条未提交的更改";
        }

        RenderDataPage(tab);
    }

    // ------------------------------------------------------------------ 关页 / 取消

    private void OnDataTabCloseRequested(TabView sender, TabViewTabCloseRequestedEventArgs args)
    {
        if (args.Item is TabViewItem item && _dataTabs.TryGetValue(item, out var tab))
        {
            CloseDataTab(tab, discard: false);
        }
    }

    /// <summary>关页：先取消在途请求；有未提交改动时**拦一次**（非模态提示行），第二次才放弃并回滚。</summary>
    private void CloseDataTab(DataTab tab, bool discard)
    {
        if (tab.Closed)
        {
            return;
        }

        var page = tab.Page;
        var pending = page.HasChanges || tab.TransactionId.Length > 0;
        if (pending && !discard && !tab.CloseConfirmed)
        {
            tab.CloseConfirmed = true;
            _notice = $"{tab.TableName} 还有 {page.DirtyRowCount} 行未提交：Ctrl+S 提交 / Ctrl+Z 撤销后再关闭；再点一次 × 将放弃更改并关闭";
            ApplyBackendState();
            return;
        }

        tab.Closed = true;
        CancelDataRequests(tab, "关页");

        var transactionId = tab.TransactionId;
        tab.TransactionId = string.Empty;
        if (transactionId.Length > 0)
        {
            _ = RollbackQuietAsync(transactionId, "关页");
        }

        _dataTabs.Remove(tab.Item);
        if (WorkspaceTabs.TabItems.Contains(tab.Item))
        {
            WorkspaceTabs.TabItems.Remove(tab.Item);
        }

        if (WorkspaceTabs.TabItems.Count > 0 && WorkspaceTabs.SelectedItem is null)
        {
            WorkspaceTabs.SelectedIndex = 0;
        }

        _activeDataTab = CurrentDataTab();
        ConsoleBridge.WriteDiagnostic(
            $"[data] 关闭标签 {tab.TableName}（已取 {page.Rows.Count} 行，未提交 {page.DirtyRowCount} 行，事务已回滚）");
        ApplyBackendState();
    }

    /// <summary>页面重建 / 窗口关闭：取消全部在途请求并回滚事务（不等结果，退出不能被卡住）。</summary>
    private void CloseAllDataTabsForUnload(string reason)
    {
        foreach (var tab in _dataTabs.Values.ToList())
        {
            tab.Closed = true;
            CancelDataRequests(tab, reason);

            var transactionId = tab.TransactionId;
            tab.TransactionId = string.Empty;
            if (transactionId.Length > 0)
            {
                _ = RollbackQuietAsync(transactionId, reason);
            }
        }
    }

    /// <summary>取消这一页所有在途请求（翻页 / 提交 / Begin）。</summary>
    private void CancelDataRequests(DataTab tab, string reason)
    {
        CancelLoad(tab);
        CancelSubmit(tab);
        CancelTxn(tab);
        ConsoleBridge.WriteDiagnostic($"[data] {tab.TableName} 取消在途请求（{reason}）");
    }

    private static void CancelLoad(DataTab tab)
    {
        try
        {
            tab.LoadCts?.Cancel();
        }
        catch (ObjectDisposedException)
        {
            // 已经释放：没有在途请求。
        }
    }

    private static void CancelSubmit(DataTab tab)
    {
        try
        {
            tab.SubmitCts?.Cancel();
        }
        catch (ObjectDisposedException)
        {
            // 同上。
        }
    }

    private static void CancelTxn(DataTab tab)
    {
        try
        {
            tab.TxnCts?.Cancel();
        }
        catch (ObjectDisposedException)
        {
            // 同上。
        }
    }

    /// <summary>切页：离开数据页时提醒还有未提交改动（页不会销毁，改动仍在）。</summary>
    private void OnDataTabSelectionChanged(object sender, SelectionChangedEventArgs args)
    {
        var current = CurrentDataTab();
        var previous = _activeDataTab;
        _activeDataTab = current;

        if (previous is not null
            && !ReferenceEquals(previous, current)
            && !previous.Closed
            && previous.Page.HasChanges)
        {
            _notice = $"{previous.TableName} 还有 {previous.Page.DirtyRowCount} 行未提交：改动仍在本页（Ctrl+S 提交 / Ctrl+Z 撤销）";
            ConsoleBridge.WriteDiagnostic($"[data] 离开 {previous.TableName}：保留 {previous.Page.DirtyRowCount} 行未提交改动");
        }

        ApplyBackendState();
    }

    /// <summary>
    /// 关窗守卫：任一数据页有未提交改动 / 打开的事务时拦一次并给出提示（非模态，不弹对话框）。
    /// </summary>
    /// <returns><see langword="true"/> = 这次关窗被拦下（调用方不要关窗）。</returns>
    /// <remarks>
    /// 第二次请求放行：先回滚所有事务再让窗口关（否则用户会被自己的改动锁死在窗口里，
    /// 而「再点一次关闭」已经是明确的放弃意图）。
    /// </remarks>
    public bool TryBlockWindowClose()
    {
        // 只按「真的有未提交改动」判定：已回滚/已提交的事务（Phase=RolledBack 但还没被用户看见）
        // 曾经把关窗拦下来过一次——界面写「还有 0 行未提交」却关不掉，是自相矛盾的提示。
        // 开着但没有改动的事务不需要拦：关窗时下面照样回滚它。
        var pending = _dataTabs.Values
            .Where(tab => !tab.Closed && (tab.Page.HasChanges || tab.Page.DirtyRowCount > 0))
            .ToList();

        if (pending.Count == 0 && _dataTabs.Count > 0)
        {
            var shapes = string.Join(
                "；",
                _dataTabs.Values.Select(tab =>
                    $"{tab.TableName} dirty={tab.Page.DirtyRowCount} changes={tab.Page.ChangeCount} " +
                    $"txn={(tab.TransactionId.Length > 0 ? "开" : "空")} phase={tab.Phase}"));
            ConsoleBridge.WriteDiagnostic($"[data] 关窗：没有未提交改动，直接放行（{shapes}）");
        }

        if (pending.Count == 0)
        {
            return false;
        }

        if (_closeGuardArmed)
        {
            foreach (var tab in pending)
            {
                var transactionId = tab.TransactionId;
                tab.TransactionId = string.Empty;
                if (transactionId.Length > 0)
                {
                    _ = RollbackQuietAsync(transactionId, "关窗（第二次请求，放弃更改）");
                }
            }

            ConsoleBridge.WriteDiagnostic($"[data] 关窗：第二次请求，放弃 {pending.Count} 个数据页的改动并回滚事务");
            return false;
        }

        _closeGuardArmed = true;
        var first = pending[0];
        WorkspaceTabs.SelectedItem = first.Item;
        _notice = $"{first.TableName} 还有 {first.Page.DirtyRowCount} 行未提交：Ctrl+S 提交 / Ctrl+Z 撤销后再退出；再点一次关闭将放弃更改并退出";
        ApplyBackendState();
        ConsoleBridge.WriteDiagnostic($"[data] 关窗被拦下：{pending.Count} 个数据页有未提交改动");
        return true;
    }

    // ------------------------------------------------------------------ 渲染（唯一入口）

    /// <summary>把一个数据页按状态整体重画（状态行 + 横幅 + 说明 + 表头 + 行区 + 分页按钮）。</summary>
    private void RenderDataPage(DataTab tab)
    {
        if (tab.Closed)
        {
            return;
        }

        // 表头与所有行共用同一份列宽状态：重画前把它对齐到当前列数（已有的用户宽度一律保留）。
        EnsureColumnWidths(tab);

        var page = tab.Page;

        var state = DataStatusText.StateText(page);
        if (page.ReadOnly && state == DataStatusText.ReadOnlyReasonText(page.ReadOnlyReason))
        {
            // 只读原因由下面的常驻横幅负责，不在状态行里再说一遍。
            state = string.Empty;
        }

        tab.StateText.Text = state;
        tab.StateText.Visibility = state.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
        var stateKey = page.Headline is { Length: > 0 } ? "SqarvoWarningTextBrush" : "SqarvoTextSecondaryBrush";
        if (ThemeBrush(stateKey) is { } stateBrush)
        {
            tab.StateText.Foreground = stateBrush;
        }

        AutomationProperties.SetName(tab.StateText, state);

        // 只读原因常驻显示（不只出现在状态行里，也不只靠按钮禁用暗示）。
        var banner = page.ReadOnly ? DataStatusText.ReadOnlyReasonText(page.ReadOnlyReason) : string.Empty;
        if (page.ReadOnly && tab.TxnNote is { Length: > 0 } note)
        {
            banner = $"{banner} · {note}";
        }

        tab.BannerText.Text = banner;
        tab.BannerText.Visibility = banner.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
        AutomationProperties.SetName(tab.BannerText, banner);

        var meta = DataStatusText.MetaText(page);
        if (tab.Failures.Count > 0)
        {
            // 逐行失败原因（稳定错误码 + 可读文案），排在说明行下面，不盖掉数据。
            meta = string.Join(
                "\n",
                new[] { meta }.Concat(tab.Failures.Select(failure => $"[{failure.Code}] {failure.Message}")));
        }

        tab.MetaText.Text = meta;
        tab.MetaText.TextWrapping = tab.Failures.Count > 0 ? TextWrapping.Wrap : TextWrapping.NoWrap;
        tab.MetaText.Visibility = meta.Length == 0 ? Visibility.Collapsed : Visibility.Visible;

        RenderDataHeader(tab);
        RenderDataRows(tab);

        tab.Prev.IsEnabled = !tab.Loading && !tab.Submitting && page.Offset > 0;
        tab.Next.IsEnabled = !tab.Loading && !tab.Submitting && page.HasMore;
        tab.LoadMore.IsEnabled = !tab.Loading && !tab.Submitting && page.HasMore;
        tab.LoadMore.Visibility = page.HasMore || page.Loading ? Visibility.Visible : Visibility.Collapsed;
        tab.Refresh.IsEnabled = !tab.Loading && !tab.Submitting && !page.HasChanges;

        ToolTipService.SetToolTip(
            tab.Prev,
            tab.Prev.IsEnabled ? "上一页（offset 分页）" : page.Offset <= 0 ? "已经是第一页" : "读取 / 提交进行中：稍候");
        ToolTipService.SetToolTip(
            tab.Next,
            tab.Next.IsEnabled ? "下一页（offset 分页）" : !page.HasMore ? "已经是最后一页（has_more=false）" : "读取 / 提交进行中：稍候");
        ToolTipService.SetToolTip(
            tab.Refresh,
            tab.Refresh.IsEnabled ? "重读当前页（清空本地暂存）" : page.HasChanges ? "有未提交的更改：先 Ctrl+S 或 Ctrl+Z" : "读取 / 提交进行中：稍候");
        ToolTipService.SetToolTip(
            tab.LoadMore,
            tab.LoadMore.IsEnabled ? "不换页，直接在表尾续拉 200 行（offset = 已取行数）" : "没有更多数据（has_more=false）");

        UpdateDataTabHeader(tab);

        // 编辑态：重建后把焦点放回输入框（重画不能把光标弄丢）。
        if (tab.Edit is not null && tab.EditBox is { } box)
        {
            try
            {
                box.Focus(FocusState.Programmatic);
                box.SelectionStart = box.Text.Length;
            }
            catch (Exception error)
            {
                ConsoleBridge.WriteDiagnostic($"[data] 编辑框聚焦失败：{error.GetType().Name}: {error.Message}");
            }
        }

        ApplyDataStatus();
    }

    /// <summary>标签名与 tooltip（库、已取行数、未提交行数、事务都在 tooltip 里）。</summary>
    private void UpdateDataTabHeader(DataTab tab)
    {
        var page = tab.Page;
        tab.Item.Header = tab.Title;
        var txn = tab.TransactionId.Length == 0 ? "无显式事务" : $"事务 {ShortId(tab.TransactionId)}";
        var tip = $"{tab.TableName} @ {(tab.CatalogName.Length == 0 ? "默认" : tab.CatalogName)} · 已取 {page.Rows.Count} 行" +
                  $"{(page.HasChanges ? $" · 未提交 {page.DirtyRowCount} 行" : string.Empty)} · {txn} · Ctrl+W 关闭";
        ToolTipService.SetToolTip(tab.Item, tip);
        AutomationProperties.SetName(tab.Item, page.HasChanges ? $"{tab.TableName}（有未提交的更改）" : tab.TableName);
    }

    /// <summary>表头：行号 + 每列「列名 · 类型（· PK）」+ 列右缘的拖宽把手（每次整体重建）。</summary>
    private void RenderDataHeader(DataTab tab)
    {
        var page = tab.Page;
        var grid = tab.HeaderGrid;
        grid.Children.Clear();
        grid.ColumnDefinitions.Clear();
        grid.Width = DataTotalWidth(tab);

        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.RowNumberWidth) });
        for (var column = 0; column < page.Columns.Count; column++)
        {
            // 表头与行区、每一行都读 tab.ColumnWidths 这一份共享状态，永远同宽。
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(tab.ColumnWidths[column]) });
        }

        var number = AddDataCell(grid, 0, "行号", secondary: true, right: true, mono: true, nullCell: false, header: true);
        AutomationProperties.SetName(number, "行号");

        for (var index = 0; index < page.Columns.Count; index++)
        {
            var column = page.Columns[index];
            var cell = AddDataCell(grid, index + 1, column.HeaderText, secondary: true, right: false, mono: false, nullCell: false, header: true);
            if (column.PrimaryKey && ThemeBrush("SqarvoAccentTextBrush") is { } accent)
            {
                // 主键列标记：用强调色（主题 token，不是颜色字面量）。
                cell.Foreground = accent;
            }

            AutomationProperties.SetName(cell, column.HeaderText);

            // 列边界的拖宽把手：命中热区取 metrics.column_resize_hit，指针变 SizeWestEast；
            // 拖动改该列宽（受上下限约束），双击按本页内容自适应。
            var grip = new ColumnResizeGrip
            {
                Width = Metrics.ColumnResizeHit,
                Background = ThemeBrush("SqarvoTransparentBrush"),
                HorizontalAlignment = HorizontalAlignment.Right,
                VerticalAlignment = VerticalAlignment.Stretch,
            };
            Grid.SetColumn(grip, index + 1);
            AutomationProperties.SetName(grip, $"调整 {column.HeaderText} 列宽");
            ToolTipService.SetToolTip(grip, "拖动改列宽 · 双击按本页内容自适应");
            WireColumnResize(grip, tab, index);
            grid.Children.Add(grip);
        }

        AutomationProperties.SetName(
            grid,
            page.Columns.Count == 0 ? "表数据表头（无列）" : $"表数据表头 {page.Columns.Count} 列");

        tab.HeaderHost.Visibility = page.Columns.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>行区：行号列 + 数据列；脏行左侧标记、选中底色、NULL 弱色斜体、编辑态输入框。</summary>
    private void RenderDataRows(DataTab tab)
    {
        var page = tab.Page;
        var grid = tab.RowsGrid;
        grid.Children.Clear();
        grid.RowDefinitions.Clear();
        grid.ColumnDefinitions.Clear();
        grid.Width = DataTotalWidth(tab);

        var columns = page.Columns.Count;
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.RowNumberWidth) });
        for (var column = 0; column < columns; column++)
        {
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(tab.ColumnWidths[column]) });
        }

        tab.EditBox = null;
        if (columns == 0)
        {
            return;
        }

        for (var index = 0; index < page.Rows.Count; index++)
        {
            var container = NewDataRow(tab, page, index, page.Rows[index], columns);
            grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(Metrics.RowHeightData) });
            Grid.SetRow(container, index);
            // 必须跨满所有列：行容器自带「行号列 + N 个数据列」的列定义，但不设 ColumnSpan 时它只落在
            // 外层网格的第 0 列（宽度 = RowNumberWidth），数据列被裁成 0 宽——真机症状「行号有、单元格全空」。
            Grid.SetColumnSpan(container, columns + 1);
            grid.Children.Add(container);
        }
    }

    private Grid NewDataRow(DataTab tab, DataPage page, int index, DataRow row, int columns)
    {
        var selected = index == tab.SelectedRow;
        var grid = new Grid { Height = Metrics.RowHeightData };

        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.RowNumberWidth) });
        for (var column = 0; column < columns; column++)
        {
            // 行容器自带列定义，也读同一份共享状态：表头与每一行始终同宽。
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(tab.ColumnWidths[column]) });
        }

        // 底色：选中 / 脏行都能看出来（脏行另有行号列的 ● 标记）。
        Brush? background = selected
            ? ThemeBrush("SqarvoSelectionBrush")
            : row.Dirty
                ? ThemeBrush("SqarvoCardBorderBrush")
                : ThemeBrush("SqarvoTransparentBrush");
        if (background is not null)
        {
            var backdrop = new Border { Background = background };
            Grid.SetColumnSpan(backdrop, columns + 1);
            grid.Children.Add(backdrop);
        }

        var numberText = row.Removed
            ? $"✕{row.Number}"
            : row.Dirty
                ? $"●{row.Number}"
                : row.Number.ToString(CultureInfo.InvariantCulture);
        var number = AddDataCell(grid, 0, numberText, secondary: !row.Dirty, right: true, mono: true, nullCell: false);
        if (row.Dirty && ThemeBrush("SqarvoAccentTextBrush") is { } accent)
        {
            number.Foreground = accent;
        }

        ToolTipService.SetToolTip(
            number,
            row.MarkedForDeletion
                ? $"第 {row.Number} 行 · 已标记删除（提交前可撤销）"
                : row.Removed
                    ? $"第 {row.Number} 行 · 已被其它会话删除（刷新可看到最新数据）"
                    : row.Dirty
                        ? $"第 {row.Number} 行 · 有未提交的更改（{row.Edits.Count} 列）"
                        : $"第 {row.Number} 行");

        for (var column = 0; column < columns; column++)
        {
            var meta = page.Columns[column];
            var value = row.Value(column);
            var isNull = DataCellText.IsNull(value);

            if (tab.Edit is { Closed: false } edit && edit.Row == index && edit.Column == column)
            {
                var box = new TextBox
                {
                    Text = edit.Draft,
                    FontSize = Metrics.FontDataCell,
                    FontWeight = TypeTokens.Cell,
                    FontFamily = MonoFont(),
                    PlaceholderText = isNull ? "NULL" : string.Empty,
                    IsSpellCheckEnabled = false,
                    VerticalAlignment = VerticalAlignment.Center,
                };
                AutomationProperties.SetName(box, $"编辑 {meta.Name} · {meta.TypeLabel}");
                ToolTipService.SetToolTip(box, "Enter 暂存（不落库）· Esc 取消 · 输入 NULL 表示 SQL NULL");

                var capturedEdit = edit;
                box.TextChanged += (_, _) => capturedEdit.Draft = box.Text ?? string.Empty;
                box.PreviewKeyDown += (_, args) =>
                {
                    if (args.Key == VirtualKey.Enter)
                    {
                        args.Handled = true;
                        CommitCellEdit(tab);
                    }
                    else if (args.Key == VirtualKey.Escape)
                    {
                        args.Handled = true;
                        CancelEdit(tab);
                        _notice = "已取消编辑（值没有改变）";
                        ApplyDataStatus();
                    }
                };
                box.LostFocus += (_, _) =>
                {
                    // 点到别处 = 暂存（不是落库）；已经提交 / 取消过的输入框迟到事件直接忽略。
                    if (!capturedEdit.Closed && ReferenceEquals(tab.Edit, capturedEdit))
                    {
                        CommitCellEdit(tab);
                    }
                };

                tab.EditBox = box;
                Grid.SetColumn(box, column + 1);
                grid.Children.Add(box);
                continue;
            }

            var cell = AddDataCell(
                grid,
                column + 1,
                DataCellText.Display(value),
                secondary: isNull,
                right: meta.Numeric,
                mono: false,
                nullCell: isNull);

            var rowIndex = index;
            var columnIndex = column;
            cell.Tapped += (_, args) =>
            {
                args.Handled = true;
                SelectCell(tab, rowIndex, columnIndex);
            };
            cell.DoubleTapped += (_, args) =>
            {
                args.Handled = true;
                BeginCellEdit(tab, rowIndex, columnIndex);
            };

            var blocked = DataStatusText.EditBlockReason(page, row, column);
            ToolTipService.SetToolTip(
                cell,
                blocked is { Length: > 0 }
                    ? $"{meta.Name}：{blocked}"
                    : $"{meta.HeaderText}（双击或 F2 编辑；空串 = 空文本，NULL 是空值）");
        }

        AutomationProperties.SetName(
            grid,
            $"{numberText} {(columns > 0 ? DataCellText.Display(row.Value(0)) : string.Empty)}".Trim());

        // 选中行底色是品牌中档：脏行强调色 / 次要色在紫底上都会糊，统一换配对前景。
        if (selected)
        {
            ApplySelectionForeground(grid);
        }

        if (row.Removed)
        {
            // 标记删除 / 已被其它会话删除：整行删除线 + 压暗（与 Linux 的
            // .sqarvo-row-deleted 同一视觉：0.55 透明度）。背景板不动，只压文字。
            foreach (var block in grid.Children.OfType<TextBlock>())
            {
                block.TextDecorations = TextDecorations.Strikethrough;
                block.Opacity = 0.55;
            }
        }

        // 行右键菜单挂在整行容器上：单元格文字、列分隔处、行分隔线、末列右侧空白
        // 都落在同一行 Grid 里；表格下方空白不属于任何行，不挂手势、不响应。
        var menuTab = tab;
        var menuRow = index;
        grid.RightTapped += (_, args) => OnDataRowRightTapped(menuTab, menuRow, args);

        return grid;
    }

    private TextBlock AddDataCell(
        Grid grid,
        int column,
        string text,
        bool secondary,
        bool right,
        bool mono,
        bool nullCell,
        bool header = false)
    {
        var block = new TextBlock
        {
            Text = text,
            // 列头 12（font_ui + 600）、行号 11（font_status）、数据格 13（font_data_cell + 500）。
            FontSize = header ? Metrics.FontUi : mono ? Metrics.FontStatus : Metrics.FontDataCell,
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

        if (nullCell)
        {
            // NULL 用弱色斜体（与空串区分：空串是空白，什么都不显示）。
            block.FontStyle = FontStyle.Italic;
        }

        var brushKey = nullCell
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

    /// <summary>表格总宽（行号列 + 每个数据列的当前宽度；超宽交给横向滚动）。</summary>
    private static double DataTotalWidth(DataTab tab)
    {
        EnsureColumnWidths(tab);
        var total = (double)Metrics.RowNumberWidth;
        if (tab.ColumnWidths.Count == 0)
        {
            // 还没有列时也给一列宽，表头 / 行区不塌成 0（与原来的 Math.Max(1, …) 同义）。
            return total + Metrics.ColumnWidth;
        }

        foreach (var width in tab.ColumnWidths)
        {
            total += width;
        }

        return total;
    }

    /// <summary>
    /// 把共享列宽状态对齐到当前列数：新出现的列补默认 column_width，已有列（含用户宽度）一律保留。
    /// 翻页 / 刷新 / 重新读取只是重画、不清这份状态；换表是新的 DataTab，天然重置（改动确认单 #2）。
    /// </summary>
    private static void EnsureColumnWidths(DataTab tab)
    {
        var count = tab.Page.Columns.Count;
        if (count == 0)
        {
            // 换页 / 刷新刚换了 DataPage，列还没回来（这一帧只是 Loading 重画）。此时按 count=0 裁剪
            // 会把用户的列宽与 UserSizedColumns 一起清空 —— 等响应填好列再画就全退回默认了。
            return;
        }

        while (tab.ColumnWidths.Count < count)
        {
            tab.ColumnWidths.Add(Metrics.ColumnWidth);
        }

        if (tab.ColumnWidths.Count > count)
        {
            tab.ColumnWidths.RemoveRange(count, tab.ColumnWidths.Count - count);
        }

        tab.UserSizedColumns.RemoveWhere(index => index >= count);
    }

    /// <summary>列宽约束：下限 metrics.column_min_width、上限 metrics.column_width（拖拽与自适应共用）。</summary>
    private static double ClampColumnWidth(double width) =>
        Math.Clamp(width, Metrics.ColumnMinWidth, Metrics.ColumnWidth);

    /// <summary>把某个 Grid 里第 column 个数据列的宽度设成给定值（第 0 列是行号列，跳过）。</summary>
    private static void SetColumnWidth(Grid grid, int column, double width)
    {
        var index = column + 1;
        if (index < grid.ColumnDefinitions.Count)
        {
            grid.ColumnDefinitions[index].Width = new GridLength(width);
        }
    }

    /// <summary>
    /// 拖动中只改各 Grid 里那一列的 GridLength，不重建表头：重建会销毁正在捕获指针的把手，
    /// 拖拽会当场断掉。表头、行区与每一行都改，保证拖动过程中也始终同宽。
    /// </summary>
    private static void ApplyColumnWidthLive(DataTab tab, int column, double width)
    {
        tab.ColumnWidths[column] = width;
        tab.UserSizedColumns.Add(column);

        SetColumnWidth(tab.HeaderGrid, column, width);
        SetColumnWidth(tab.RowsGrid, column, width);
        foreach (var child in tab.RowsGrid.Children)
        {
            if (child is Grid row)
            {
                SetColumnWidth(row, column, width);
            }
        }

        var total = DataTotalWidth(tab);
        tab.HeaderGrid.Width = total;
        tab.RowsGrid.Width = total;
    }

    /// <summary>给列右缘的把手接上拖动手势与双击自适应。</summary>
    private void WireColumnResize(ColumnResizeGrip grip, DataTab tab, int column)
    {
        grip.PointerPressed += (_, args) =>
        {
            if (tab.Closed)
            {
                return;
            }

            EnsureColumnWidths(tab);
            if (column >= tab.ColumnWidths.Count)
            {
                return;
            }

            var point = args.GetCurrentPoint(tab.RowsGrid);
            if (!point.Properties.IsLeftButtonPressed)
            {
                return;
            }

            tab.ResizeColumn = column;
            // 参照系必须是不随列宽移动的元素：把手自己贴在列右缘、跟着列一起右移，用 grip 取点等于拿
            // 一把会动的尺子量位移（W ← 2·W0 + d − W_cur，拖起来来回抖、默认列宽还已到上限）。
            // RowsGrid 的左缘在拖动期间固定，横向滚动也不会在按下 ~ 松开之间发生。
            tab.ResizeStartX = point.Position.X;
            tab.ResizeStartWidth = tab.ColumnWidths[column];
            if (!grip.CapturePointer(args.Pointer))
            {
                tab.ResizeColumn = -1;
                return;
            }

            args.Handled = true;
        };

        grip.PointerMoved += (_, args) =>
        {
            if (tab.Closed || tab.ResizeColumn != column || column >= tab.ColumnWidths.Count)
            {
                return;
            }

            var point = args.GetCurrentPoint(tab.RowsGrid);
            if (!point.Properties.IsLeftButtonPressed)
            {
                // 按键早松了但没收到 PointerReleased（捕获丢了）：按取消处理，别让用户没按键地改列宽。
                tab.ResizeColumn = -1;
                return;
            }

            var width = ClampColumnWidth(tab.ResizeStartWidth + (point.Position.X - tab.ResizeStartX));
            if (width != tab.ColumnWidths[column])
            {
                ApplyColumnWidthLive(tab, column, width);
            }

            args.Handled = true;
        };

        grip.PointerReleased += (_, args) =>
        {
            if (tab.ResizeColumn != column)
            {
                return;
            }

            tab.ResizeColumn = -1;
            grip.ReleasePointerCapture(args.Pointer);
            args.Handled = true;
        };

        grip.PointerCaptureLost += (_, _) =>
        {
            if (tab.ResizeColumn == column)
            {
                tab.ResizeColumn = -1;
            }
        };

        grip.DoubleTapped += (_, args) =>
        {
            args.Handled = true;
            AutoFitColumn(tab, column);
        };
    }

    /// <summary>双击把手：按表头文案与已加载值里最长文字 + 单元格内距自适应，受上下限约束，记为用户宽度。</summary>
    private void AutoFitColumn(DataTab tab, int column)
    {
        if (tab.Closed)
        {
            return;
        }

        EnsureColumnWidths(tab);
        if (column < 0 || column >= tab.ColumnWidths.Count)
        {
            return;
        }

        var measured = MeasureAutoFitWidth(tab, column);
        if (measured <= 0)
        {
            // 量不到（游离 TextBlock 没展开模板时可能返回 0）：保持当前宽度，不要把整列压到下限 64。
            ConsoleBridge.WriteDiagnostic(
                $"[colwidth] 自适应放弃 column={column}：量不到内容宽度（保持 " +
                $"{tab.ColumnWidths[column].ToString("0.##", CultureInfo.InvariantCulture)}）");
            return;
        }

        var width = ClampColumnWidth(measured + 2 * (double)Metrics.ColumnCellPadding);
        tab.ColumnWidths[column] = width;
        tab.UserSizedColumns.Add(column);
        RenderDataPage(tab);
        ConsoleBridge.WriteDiagnostic(
            $"[colwidth] 自适应 column={column} width={width.ToString("0.##", CultureInfo.InvariantCulture)} " +
            $"min={Metrics.ColumnMinWidth} max={Metrics.ColumnWidth}");
    }

    /// <summary>最长文字（表头 + 已加载值）的自然宽度；量不到时返回 0（由调用方决定保持原宽）。</summary>
    private static double MeasureAutoFitWidth(DataTab tab, int column)
    {
        // 表头用 font_ui + title(600)、数据格用 font_data_cell + cell(500)（AddDataCell 的口径），
        // 测量必须两边分开，否则表头会量得偏窄 / 偏宽。
        var longest = MeasureTextWidth(tab.Page.Columns[column].HeaderText, Metrics.FontUi, TypeTokens.Title);
        foreach (var row in tab.Page.Rows)
        {
            var text = DataCellText.Display(row.Value(column));
            if (text.Length > 0)
            {
                longest = Math.Max(longest,
                    MeasureTextWidth(text, Metrics.FontDataCell, TypeTokens.Cell));
            }
        }

        return longest;
    }

    /// <summary>用游离 TextBlock 量一段单行文字的自然宽度（字号 / 字重由调用方按格类型给）。</summary>
    private static double MeasureTextWidth(string text, double fontSize, FontWeight fontWeight)
    {
        if (string.IsNullOrEmpty(text))
        {
            return 0;
        }

        try
        {
            var probe = new TextBlock
            {
                Text = text,
                FontSize = fontSize,
                FontWeight = fontWeight,
                TextWrapping = TextWrapping.NoWrap,
                TextTrimming = TextTrimming.None,
            };
            probe.Measure(new global::Windows.Foundation.Size(double.PositiveInfinity, double.PositiveInfinity));
            return probe.DesiredSize.Width;
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[colwidth] 文本测量失败：{error.GetType().Name}: {error.Message}");
            return 0;
        }
    }

    /// <summary>
    /// QA 钩子（<c>--qa-column-width=&lt;列序号&gt;:&lt;宽度&gt;</c>）：**只在诊断路径**把某列设成用户宽度，
    /// 用于截图 / --qa-layout-audit 取证。拖拽动作本身无法用脚本模拟，这个开关等价于「用户把该列拖到某宽度」，
    /// 走的是与拖拽完全相同的共享列宽状态与重画路径；正常用户路径不会调用它。
    /// </summary>
    public void ApplyQaColumnWidth(int index, double width)
    {
        var tab = CurrentDataTab() ?? _activeDataTab;
        if (tab is null || tab.Closed)
        {
            ConsoleBridge.WriteLine("[colwidth] no-active-tab（没有已打开的数据页，QA 宽度未设置）");
            return;
        }

        EnsureColumnWidths(tab);
        if (index < 0 || index >= tab.ColumnWidths.Count)
        {
            ConsoleBridge.WriteLine($"[colwidth] out-of-range index={index} columns={tab.ColumnWidths.Count}");
            return;
        }

        var applied = ClampColumnWidth(width);
        tab.ColumnWidths[index] = applied;
        tab.UserSizedColumns.Add(index);
        RenderDataPage(tab);
        ConsoleBridge.WriteLine(
            $"[colwidth] index={index} requested={width.ToString("0.##", CultureInfo.InvariantCulture)} " +
            $"applied={applied.ToString("0.##", CultureInfo.InvariantCulture)} " +
            $"min={Metrics.ColumnMinWidth} max={Metrics.ColumnWidth} user_sized={tab.UserSizedColumns.Count}");
    }

    /// <summary>
    /// 布局审计（<c>--qa-layout-audit</c>）用：当前激活数据页每个数据列的宽度（共享列宽状态）。
    /// 表头与所有行都从这份状态取绝对值 GridLength，因此它既是设定值、也是实际渲染宽度。
    /// </summary>
    public IReadOnlyList<double>? ActiveDataColumnWidths()
    {
        var tab = CurrentDataTab() ?? _activeDataTab;
        if (tab is null || tab.Closed)
        {
            return null;
        }

        EnsureColumnWidths(tab);
        return tab.ColumnWidths.ToArray();
    }

    /// <summary>表头与行区横向滚动互相同步（纵向各管各的）。</summary>
    private void SyncDataScroll(DataTab tab, bool fromBody)
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
            ConsoleBridge.WriteDiagnostic($"[data] 横向滚动同步失败：{error.GetType().Name}: {error.Message}");
        }
        finally
        {
            tab.SyncingScroll = false;
        }
    }

    /// <summary>状态栏行 2 与行操作按钮：当前是数据页时按这一页的真实状态写。</summary>
    private void ApplyDataStatus()
    {
        var tab = CurrentDataTab();
        if (tab is null)
        {
            // 回到 Objects / 查询页：分页摘要回到默认（行 2 的文案由查询 / 后端路径自己写）。
            PagerText.Text = "第 ? / ? 页";
            TransactionText.Text = "自动提交";
            ButtonRowApply.IsEnabled = false;
            ButtonRowRevert.IsEnabled = false;
            ButtonRowRefresh.IsEnabled = false;

            // 「取消」在查询页由 ApplyQueryStatus 负责（它可能刚把它设成可点）：只有停在 Objects 页时才收手。
            if (CurrentQueryTab() is null)
            {
                ButtonRowCancel.IsEnabled = false;
                ToolTipService.SetToolTip(ButtonRowCancel, "取消：当前没有执行中的查询");
            }

            return;
        }

        var page = tab.Page;
        var submitting = tab.Submitting;
        var label = DataStatusText.TransactionLabel(page.ReadOnly, tab.Phase, submitting, page.DirtyRowCount);
        TransactionText.Text = label;
        ToolTipService.SetToolTip(
            TransactionText,
            tab.TransactionId.Length == 0
                ? $"事务：{label}（没有显式事务：提交时按自动提交执行）"
                : $"事务：{label} · 事务 {ShortId(tab.TransactionId)}");

        // 状态栏行 2 左格：最近的提示（编辑被拒原因 / 提交结果 / 守卫说明）优先，
        // 没有提示时才是这一页的摘要——否则数据页会把 _notice 盖掉，用户看不到任何反馈。
        var summary = DataStatusText.Summary(page);
        SqlSummaryText.Text = _notice is { Length: > 0 } notice ? notice : summary;
        ToolTipService.SetToolTip(
            SqlSummaryText,
            summary
            + (DataStatusText.OrderingLabel(page.Ordering) is { Length: > 0 } ordering ? $" · {ordering}" : string.Empty)
            + (tab.Failures.Count > 0 ? $" · 最近一次提交有 {tab.Failures.Count} 行失败（见页内说明）" : string.Empty));

        PagerText.Text = DataStatusText.PageLabel(page);
        AutomationProperties.SetName(PagerText, $"分页：{DataStatusText.PageLabel(page)}");

        var canApply = !page.ReadOnly && page.HasChanges && !submitting && !tab.Loading;
        ButtonRowApply.IsEnabled = canApply;
        ToolTipService.SetToolTip(
            ButtonRowApply,
            canApply
                ? $"应用：提交 {page.ChangeCount + page.MarkedDeleteCount} 条变更（Ctrl+S）"
                : page.ReadOnly
                    ? DataStatusText.ReadOnlyReasonText(page.ReadOnlyReason)
                    : submitting
                        ? "提交进行中"
                        : "应用：没有待提交的更改（双击单元格或用 F2 修改后按 Enter 暂存）");

        var canRevert = page.HasChanges && !submitting;
        ButtonRowRevert.IsEnabled = canRevert;
        ToolTipService.SetToolTip(
            ButtonRowRevert,
            canRevert ? "撤销：放弃全部未提交的更改并回滚事务（Ctrl+Z）" : "撤销：没有待撤销的更改");

        ButtonRowRefresh.IsEnabled = !tab.Loading && !submitting && !page.HasChanges;
        ToolTipService.SetToolTip(
            ButtonRowRefresh,
            ButtonRowRefresh.IsEnabled ? "刷新：重读当前页（清空本地暂存）" : "刷新：有未提交的更改或请求进行中");

        ButtonRowCancel.IsEnabled = tab.Loading || submitting;
        ToolTipService.SetToolTip(
            ButtonRowCancel,
            ButtonRowCancel.IsEnabled ? "取消正在进行的表数据请求（已取到的行保留）" : "取消：当前没有进行中的表数据请求");

        // 新增行仍不在本片；删除行走「标记 → 提交」整片（右键菜单 / 状态栏「−」同一方法）。
        ButtonRowAdd.IsEnabled = false;
        ToolTipService.SetToolTip(ButtonRowAdd, "新增行不在本片（#173 只做单元格编辑与提交）：请用 SQL");
        var canDelete = CanDeleteRows(tab, out var deleteReason);
        ButtonRowDelete.IsEnabled = canDelete;
        ToolTipService.SetToolTip(ButtonRowDelete, deleteReason);
    }

    /// <summary>「删除行」可用性：可编辑、没有在途请求、有选中行（复用只读原因做 tooltip）。</summary>
    private static bool CanDeleteRows(DataTab tab, out string reason)
    {
        var page = tab.Page;
        if (page.ReadOnly)
        {
            reason = DataStatusText.ReadOnlyReasonText(page.ReadOnlyReason);
            return false;
        }

        if (tab.Loading || tab.Submitting)
        {
            reason = "正在读取 / 提交：稍候再删除行";
            return false;
        }

        if (tab.SelectedRow < 0 || tab.SelectedRow >= page.Rows.Count)
        {
            reason = "先选中一行再删除";
            return false;
        }

        // 拿不到主键的行无法定位（DELETE 要按主键），标了也提交不了：直接不给标。
        if (!page.Rows[tab.SelectedRow].Locatable)
        {
            reason = "这一行没有主键值，无法定位：不能删除（请用 SQL）";
            return false;
        }

        reason = "删除行：标记选中行为待删除（提交前可 Ctrl+Z 撤销）";
        return true;
    }

    /// <summary>标记删除（右键菜单 / 状态栏「−」共用）：只改本地行状态，不写库。</summary>
    private void DeleteSelectedRows(DataTab tab)
    {
        if (!CanDeleteRows(tab, out var reason))
        {
            _notice = reason;
            ApplyDataStatus();
            return;
        }

        var page = tab.Page;
        var row = page.Rows[tab.SelectedRow];
        if (row.MarkedForDeletion)
        {
            _notice = $"第 {row.Number} 行已经标记删除（Ctrl+Z 可撤销）";
            ApplyDataStatus();
            return;
        }

        row.MarkedForDeletion = true;
        row.Removed = true; // ✕N + 删除线 + 压暗
        tab.Failures = Array.Empty<DataRowFailure>();
        tab.RiskArmed = false;
        _ = EnsureTransactionAsync(tab);

        var marked = page.MarkedDeleteCount;
        _notice = marked == 1
            ? DataStatusText.MarkDeletedText(row.Number)
            : DataStatusText.MarkDeletedBatchText(marked);
        RenderDataPage(tab);
    }

    /// <summary>数据行右键：先选中该行，再弹「删除行」/「复制为 ▸ 复制为 INSERT」。</summary>
    private void OnDataRowRightTapped(DataTab tab, int rowIndex, RightTappedRoutedEventArgs args)
    {
        var page = tab.Page;
        if (rowIndex < 0 || rowIndex >= page.Rows.Count)
        {
            return;
        }

        // 若已选中这一行就保留选中集（本片是单选）；否则先选中它，菜单动作永远作用于当前选中集。
        // 列光标保持不动：右键不该把用户正在编辑的那一列弹回首列。
        if (tab.SelectedRow != rowIndex)
        {
            tab.SelectedRow = rowIndex;
            if (tab.SelectedColumn < 0 || tab.SelectedColumn >= page.Columns.Count)
            {
                tab.SelectedColumn = 0;
            }

            RenderDataRows(tab);
            ApplyDataStatus();
        }

        var canDelete = CanDeleteRows(tab, out var reason);
        var flyout = new MenuFlyout();

        var delete = new MenuFlyoutItem
        {
            Text = "删除行",
            FontSize = Metrics.FontUi,
            FontWeight = TypeTokens.Ui,
            IsEnabled = canDelete,
        };
        if (!canDelete)
        {
            ToolTipService.SetToolTip(delete, reason);
        }

        delete.Click += (_, _) => DeleteSelectedRows(tab);
        flyout.Items.Add(delete);

        // 二级菜单：「复制为 ▸ 复制为 INSERT」始终可用（只读连接也能复制数据）。
        var copy = new MenuFlyoutSubItem
        {
            Text = "复制为",
            FontSize = Metrics.FontUi,
            FontWeight = TypeTokens.Ui,
        };
        var insert = new MenuFlyoutItem
        {
            Text = "复制为 INSERT",
            FontSize = Metrics.FontUi,
            FontWeight = TypeTokens.Ui,
        };
        insert.Click += (_, _) => CopySelectedAsInsert(tab);
        copy.Items.Add(insert);
        flyout.Items.Add(copy);

        try
        {
            // RenderDataRows 重建了行容器，锚点必须用不会被重建的 BodyScroll。
            flyout.ShowAt(tab.BodyScroll, args.GetPosition(tab.BodyScroll));
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteDiagnostic($"[data] 行右键菜单打开失败：{error.GetType().Name}: {error.Message}");
            return;
        }

        args.Handled = true;
    }

    /// <summary>「复制为 INSERT」：选中行当前值合成一条多值 INSERT，写进系统剪贴板。</summary>
    private void CopySelectedAsInsert(DataTab tab)
    {
        var page = tab.Page;
        if (tab.SelectedRow < 0 || tab.SelectedRow >= page.Rows.Count)
        {
            _notice = "复制为 INSERT 失败：先选中一行";
            ApplyDataStatus();
            return;
        }

        var sql = DataInsertSql.Build(page, new[] { page.Rows[tab.SelectedRow] });
        if (sql.Length == 0)
        {
            _notice = "复制为 INSERT 失败：这一页没有可复制的列";
            ApplyDataStatus();
            return;
        }

        try
        {
            var package = new DataPackage();
            package.SetText(sql);
            Clipboard.SetContent(package);
            // 不 Flush 的话，应用退出后剪贴板内容可能在被粘贴前失效（WinUI 的已知行为）。
            Clipboard.Flush();
            _notice = "已复制 1 行为 INSERT 语句";
        }
        catch (Exception error)
        {
            _notice = $"复制为 INSERT 失败：{error.GetType().Name}: {error.Message}";
            ConsoleBridge.WriteDiagnostic($"[data] 复制为 INSERT 失败：{error.GetType().Name}: {error.Message}");
        }

        ApplyDataStatus();
    }

    private static string ShortId(string value) => value.Length <= 8 ? value : value[..8];

    // ------------------------------------------------------------------ QA 探针（--qa-data）

    /// <summary>QA 解析出来的表引用（展示名 + 库名 + 服务端给的对象引用）。</summary>
    private sealed record TableRef(string DisplayName, string CatalogName, ObjectRef Reference);

    /// <summary>
    /// <c>--qa-data=&lt;库.表&gt;</c>：走**与用户完全相同**的路径开数据页 → 翻一页 → 改一格并提交 →
    /// 还原 → 再改一格并撤销，把 <c>[data]</c> 证据行打到 stdout。
    /// </summary>
    /// <param name="connectionKey">已连接的连接身份键。</param>
    /// <param name="catalogName">库名（<see langword="null"/> = 第一个可见库）。</param>
    /// <param name="table">表名（区分大小写不敏感）。</param>
    /// <param name="cancellationToken">放弃这次探针。</param>
    /// <returns>整条链路是否跑通（数据页打开 + 提交 + 回滚都有真实结果）。</returns>
    /// <remarks>
    /// 探针**会写开发库**：先写入一个短标记再还原，最后一步只做本地回滚（不写库），因此跑完数据与初始一致。
    /// 只对开发库使用（<c>SQARVO_QA_PASSWORD</c> 指向的测试实例），不要指向生产库。
    /// </remarks>
    public async Task<bool> ProbeDataAsync(
        string connectionKey,
        string? catalogName,
        string table,
        CancellationToken cancellationToken)
    {
        try
        {
            var metadata = _session?.Metadata;
            var sessionId = _session?.OpenResult(connectionKey)?.SessionId;
            if (metadata is null || string.IsNullOrEmpty(sessionId))
            {
                ConsoleBridge.WriteLine("[data] backend/session=missing（后端未就绪或连接没有会话）");
                return false;
            }

            ConsoleBridge.WriteLine("[data] 注意：--qa-data 会先写入再还原开发库里的一个单元格（不要指向生产库）");

            var resolved = await ResolveTableRefAsync(metadata, sessionId, catalogName, table, cancellationToken)
                .ConfigureAwait(true);
            if (resolved is not { } tableRef)
            {
                ConsoleBridge.WriteLine($"[data] table={table} missing（在 {(catalogName ?? "第一个可见库")} 里没找到这张表）");
                return false;
            }

            ConsoleBridge.WriteLine(
                $"[data] table={tableRef.DisplayName} catalog={(tableRef.CatalogName.Length == 0 ? "默认" : tableRef.CatalogName)} " +
                $"kind={MetadataLabels.KindLabel(tableRef.Reference.Kind)} session={ShortId(sessionId)}");

            var catalogRef = new ObjectRef
            {
                SessionId = sessionId,
                Catalog = tableRef.CatalogName,
                Kind = ObjectKind.Catalog,
            };
            var target = new ObjectPayload(
                connectionKey,
                catalogRef,
                new ObjectEntry(tableRef.Reference, tableRef.DisplayName, tableRef.Reference.Kind));

            var tab = await OpenTableDataAsync(target).ConfigureAwait(true);
            if (tab is null)
            {
                ConsoleBridge.WriteLine("[data] open=failed（数据页被上限 / 「未连接」拒绝）");
                return false;
            }

            cancellationToken.ThrowIfCancellationRequested();
            var page = tab.Page;
            ConsoleBridge.WriteLine(
                $"[data] open page={page.PageNumber} offset={page.Offset} rows={page.Rows.Count} columns={page.Columns.Count} " +
                $"has_more={page.HasMore} read_only={page.ReadOnly} read_only_reason={page.ReadOnlyReason} " +
                $"total_kind={page.TotalKind} page_size={page.PageSize} transaction={TransactionLabel(tab)}");

            if (page.Headline is { Length: > 0 } headline)
            {
                ConsoleBridge.WriteLine($"[data] open error={headline} code={page.Code}");
                return false;
            }

            // 翻一页（offset 分页，下一页 offset = 当前 offset + page_size）。
            if (page.HasMore)
            {
                await StepPageAsync(tab, forward: true).ConfigureAwait(true);
                ConsoleBridge.WriteLine(
                    $"[data] page={tab.Page.PageNumber} offset={tab.Page.Offset} rows={tab.Page.Rows.Count} " +
                    $"has_more={tab.Page.HasMore} state={DescribeState(tab.Page)}");

                // 回到第一页再改：证据里「第 1 行」的坐标与第 1 页一致，便于人工复核。
                await StepPageAsync(tab, forward: false).ConfigureAwait(true);
            }
            else
            {
                ConsoleBridge.WriteLine("[data] page=1（服务端 has_more=false：没有第二页可翻，翻页证据只剩这一行）");
            }

            if (tab.Page.ReadOnly)
            {
                ConsoleBridge.WriteLine($"[data] edit=skipped reason={DataStatusText.ReadOnlyReasonText(tab.Page.ReadOnlyReason)}");
                ConsoleBridge.WriteLine($"[data] summary tabs={_dataTabs.Count} rows={tab.Page.Rows.Count} dirty_rows=0 transaction={TransactionLabel(tab)}");
                return true;
            }

            if (PickProbeCell(tab) is not { } cell)
            {
                ConsoleBridge.WriteLine("[data] edit=skipped reason=没有可编辑的文本 / 数值列（本版只就地编辑文本、整数、小数、布尔）");
                ConsoleBridge.WriteLine($"[data] summary tabs={_dataTabs.Count} rows={tab.Page.Rows.Count} dirty_rows=0 transaction={TransactionLabel(tab)}");
                return true;
            }

            var original = cell.Original;
            const string marker = "qa173";

            if (!StageCellValue(tab, cell.Row, cell.Column, marker))
            {
                ConsoleBridge.WriteLine("[data] edit=failed（值没有暂存，原因见状态栏）");
                return false;
            }

            ConsoleBridge.WriteLine(
                $"[data] edit row={cell.Row + 1} column={cell.ColumnName} kind={cell.KindLabel} " +
                $"from={QuoteText(original)} to={QuoteText(marker)} staged_changes={tab.Page.ChangeCount} " +
                $"dirty_rows={tab.Page.DirtyRowCount} transaction={TransactionLabel(tab)}");

            var dirtyBefore = tab.Page.DirtyRowCount;
            await SubmitDataAsync(tab).ConfigureAwait(true);
            ConsoleBridge.WriteLine(
                $"[data] submit applied={Math.Max(0, dirtyBefore - tab.Page.DirtyRowCount)} failures={tab.Failures.Count} " +
                $"dirty_rows={tab.Page.DirtyRowCount} transaction={TransactionLabel(tab)} " +
                $"code={(tab.Failures.Count > 0 ? tab.Failures[0].Code : "-")}");

            if (tab.Failures.Count > 0 || tab.Page.HasChanges)
            {
                ConsoleBridge.WriteLine("[data] submit=incomplete（失败原因已写进页内说明；不继续做还原）");
                return false;
            }

            // 回读验证：重新读这一页，确认数据库里真的是新值。
            await RefreshDataAsync(tab).ConfigureAwait(true);
            var afterWrite = CellText(tab, cell.Row, cell.Column);
            ConsoleBridge.WriteLine(
                $"[data] verify row={cell.Row + 1} column={cell.ColumnName} value={QuoteText(afterWrite)} match={afterWrite == marker}");

            // 还原：把原值写回去（探针退出时数据库与进入时一致）。
            if (!StageCellValue(tab, cell.Row, cell.Column, original))
            {
                ConsoleBridge.WriteLine("[data] restore=staged-failed（原文无法写回，请人工检查这一格）");
                return false;
            }

            await SubmitDataAsync(tab).ConfigureAwait(true);
            await RefreshDataAsync(tab).ConfigureAwait(true);
            var restored = CellText(tab, cell.Row, cell.Column);
            ConsoleBridge.WriteLine(
                $"[data] restore row={cell.Row + 1} column={cell.ColumnName} value={QuoteText(restored)} match={restored == original} " +
                $"dirty_rows={tab.Page.DirtyRowCount} transaction={TransactionLabel(tab)}");

            // 再改一格并**撤销**：验证 Rollback 真的发生、并且没有写库。
            if (!StageCellValue(tab, cell.Row, cell.Column, marker))
            {
                ConsoleBridge.WriteLine("[data] revert=staged-failed");
                return false;
            }

            var stagedForRevert = tab.Page.ChangeCount;
            await RevertDataAsync(tab).ConfigureAwait(true);
            ConsoleBridge.WriteLine(
                $"[data] revert staged={stagedForRevert} transaction={TransactionLabel(tab)} dirty_rows={tab.Page.DirtyRowCount}");

            await RefreshDataAsync(tab).ConfigureAwait(true);
            var afterRevert = CellText(tab, cell.Row, cell.Column);
            ConsoleBridge.WriteLine(
                $"[data] verify-after-revert value={QuoteText(afterRevert)} match={afterRevert == original} " +
                $"(撤销没有写库，数据仍是原值)");

            ConsoleBridge.WriteLine(
                $"[data] summary tabs={_dataTabs.Count} rows={tab.Page.Rows.Count} dirty_rows={tab.Page.DirtyRowCount} " +
                $"transaction={TransactionLabel(tab)} page={DataStatusText.PageLabel(tab.Page)}");
            return true;
        }
        catch (OperationCanceledException)
        {
            ConsoleBridge.WriteLine("[data] cancelled");
            return false;
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteLine($"[data] failed {error.GetType().Name}: {error.Message}");
            return false;
        }
    }

    /// <summary>QA：把所有数据页关掉（走与用户点 × 同一条 <see cref="CloseDataTab"/>，回滚事务）。</summary>
    /// <returns>关掉的页数。</returns>
    public int CloseAllDataTabsForQa()
    {
        var tabs = _dataTabs.Values.ToList();
        foreach (var tab in tabs)
        {
            CloseDataTab(tab, discard: true);
        }

        return tabs.Count;
    }

    /// <summary>把「库.表」解析成服务端给的对象引用（走 `ListNodes`，不自己拼名字当引用）。</summary>
    private async Task<TableRef?> ResolveTableRefAsync(
        MetadataClient metadata,
        string sessionId,
        string? catalogName,
        string table,
        CancellationToken cancellationToken)
    {
        var catalogs = await metadata
            .ListNodesAsync(
                new ObjectRef { SessionId = sessionId, Kind = ObjectKind.Catalog },
                200,
                null,
                null,
                cancellationToken)
            .ConfigureAwait(true);

        ObjectRef? catalog = null;
        var catalogLabel = string.Empty;
        foreach (var node in catalogs.Nodes)
        {
            if (node.Reference is not { } reference)
            {
                continue;
            }

            var label = string.IsNullOrWhiteSpace(node.DisplayName) ? reference.Catalog : node.DisplayName;
            if (catalogName is null
                || string.Equals(reference.Catalog, catalogName, StringComparison.OrdinalIgnoreCase)
                || string.Equals(label, catalogName, StringComparison.OrdinalIgnoreCase))
            {
                catalog = reference;
                catalogLabel = label;
                break;
            }
        }

        if (catalog is null)
        {
            return null;
        }

        var parent = new ObjectRef
        {
            SessionId = sessionId,
            Catalog = catalog.Catalog,
            Schema = catalog.Schema,
            Name = catalog.Name,
            Kind = catalog.Kind,
        };

        var filter = new NodeFilter();
        filter.Kinds.Add(ObjectKind.Table);
        filter.Kinds.Add(ObjectKind.View);
        filter.Kinds.Add(ObjectKind.MaterializedView);

        string? pageToken = null;
        for (var page = 0; page < 10; page++)
        {
            var response = await metadata
                .ListNodesAsync(parent, 200, pageToken, filter, cancellationToken)
                .ConfigureAwait(true);

            foreach (var node in response.Nodes)
            {
                if (node.Reference is not { } reference)
                {
                    continue;
                }

                var label = string.IsNullOrWhiteSpace(node.DisplayName) ? reference.Name : node.DisplayName;
                if (string.Equals(label, table, StringComparison.OrdinalIgnoreCase))
                {
                    return new TableRef(label, catalogLabel, reference);
                }
            }

            pageToken = response.Page?.NextPageToken;
            if (string.IsNullOrEmpty(pageToken))
            {
                break;
            }
        }

        return null;
    }

    /// <summary>QA 选中的一格（页内下标 + 列名 + 类型 + 原值文字）。</summary>
    private sealed record ProbeCell(int Row, int Column, string ColumnName, string KindLabel, string Original);

    /// <summary>
    /// QA 选一格可编辑的：优先文本列（写短标记不会踩类型 / 长度），没有就退到第一格可编辑的数值列。
    /// </summary>
    private ProbeCell? PickProbeCell(DataTab tab)
    {
        var page = tab.Page;
        if (page.Rows.Count == 0)
        {
            return null;
        }

        for (var column = 0; column < page.Columns.Count; column++)
        {
            var meta = page.Columns[column];
            if (meta.Kind != ColumnTypeKind.ColumnTypeText)
            {
                continue;
            }

            for (var row = 0; row < page.Rows.Count; row++)
            {
                if (DataStatusText.EditBlockReason(page, page.Rows[row], column) is null)
                {
                    return new ProbeCell(row, column, meta.Name, meta.TypeLabel, DataCellText.Display(page.Rows[row].Value(column)));
                }
            }
        }

        for (var column = 0; column < page.Columns.Count; column++)
        {
            var meta = page.Columns[column];
            if (!DataInput.SupportsInlineEdit(meta.Kind))
            {
                continue;
            }

            for (var row = 0; row < page.Rows.Count; row++)
            {
                if (DataStatusText.EditBlockReason(page, page.Rows[row], column) is null)
                {
                    return new ProbeCell(row, column, meta.Name, meta.TypeLabel, DataCellText.Display(page.Rows[row].Value(column)));
                }
            }
        }

        return null;
    }

    private static string CellText(DataTab tab, int row, int column) =>
        row < tab.Page.Rows.Count && column < tab.Page.Columns.Count
            ? DataCellText.Display(tab.Page.Rows[row].Value(column))
            : string.Empty;

    private static string TransactionLabel(DataTab tab) => DataStatusText.TransactionLabel(
        tab.Page.ReadOnly,
        tab.Phase,
        tab.Submitting,
        tab.Page.DirtyRowCount);

    private static string DescribeState(DataPage page) => page.Loading
        ? "loading"
        : page.Headline is { Length: > 0 }
            ? "error"
            : page.Rows.Count == 0
                ? "empty"
                : "loaded";

    private static string QuoteText(string text) =>
        text.IndexOf('"') >= 0 || text.IndexOf(' ') >= 0 || text.Length == 0 ? $"\"{text}\"" : text;
}