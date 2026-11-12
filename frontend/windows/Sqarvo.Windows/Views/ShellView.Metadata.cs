using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Sqarvo.Services.V1;
using Sqarvo.Windows.Connections;
using Sqarvo.Windows.Diagnostics;
using Sqarvo.Windows.Theming;
using Sqarvo.Windows.Workspace;
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Windows.System;
using Windows.UI.Text;

namespace Sqarvo.Windows.Views;

/// <summary>
/// <see cref="ShellView"/> 的元数据面（#171）：树的层与分页、`Objects` 页三列表格、信息面板三态、
/// <c>--qa-metadata</c> 探针。
/// </summary>
/// <remarks>
/// 拆成 partial 文件只是为了「一个文件一件事」：外壳与连接面在 <c>ShellView.xaml.cs</c>，
/// 元数据面的渲染与加载在这里。所有状态仍然是 <see cref="ShellView"/> 的私有字段（唯一一份），
/// 没有第二个「当前库 / 当前表」。
/// </remarks>
public sealed partial class ShellView
{
    // ---- 树的层：连接层（库）+ 分组层（对象） -------------------------------------

    /// <summary>连接层：列出该连接可见的库（`parent.kind = CATALOG` 且不带名字）。</summary>
    private Task EnsureCatalogsAsync(TreeViewNode node)
    {
        var load = LoadFor(node);
        if (load.Loading)
        {
            return load.Task;
        }

        if (load.Loaded)
        {
            return Task.CompletedTask;
        }

        load.Task = LoadTreePageAsync(node, load, firstPage: true);
        return load.Task;
    }

    /// <summary>分组层：按 `filter.kinds` 拉这一组的第一页。</summary>
    private Task EnsureGroupLoadedAsync(TreeViewNode node)
    {
        var load = LoadFor(node);
        if (load.Loading)
        {
            return load.Task;
        }

        if (load.Loaded)
        {
            return Task.CompletedTask;
        }

        load.Task = LoadTreePageAsync(node, load, firstPage: true);
        return load.Task;
    }

    private Task LoadNodeFromScratchAsync(TreeViewNode node)
        => node.Content is ConnectionNode ? EnsureCatalogsAsync(node) : EnsureGroupLoadedAsync(node);

    private Task LoadNextTreePageAsync(TreeViewNode node)
    {
        var load = LoadFor(node);
        if (load.Loading || load.NextToken.Length == 0)
        {
            return Task.CompletedTask;
        }

        load.Task = LoadTreePageAsync(node, load, firstPage: false);
        return load.Task;
    }

    /// <summary>拉树上的这一层一页；结果就地渲染成子节点（加载 / 空 / 失败三态都可见）。</summary>
    private async Task LoadTreePageAsync(TreeViewNode node, NodeLoad load, bool firstPage)
    {
        if (firstPage)
        {
            load.Items.Clear();
            load.NextToken = string.Empty;
            load.Canceled = false;
        }

        var metadata = _session?.Metadata;
        if (metadata is null)
        {
            load.Loading = false;
            load.Loaded = false;
            load.ErrorHeadline = "本地后端尚未就绪";
            load.ErrorCode = "backend.not_acquired";
            RenderNode(node);
            return;
        }

        if (!TryBuildTreeRequest(node, out var parent, out var filter, out var reason))
        {
            load.Loading = false;
            load.Loaded = false;
            load.ErrorHeadline = reason;
            load.ErrorCode = "metadata.request_unavailable";
            RenderNode(node);
            return;
        }

        load.Loading = true;
        load.Loaded = false;
        load.ErrorHeadline = null;
        load.ErrorCode = null;
        RenderNode(node);

        load.Cts?.Dispose();
        load.Cts = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        var token = load.Cts.Token;
        var canceled = false;

        try
        {
            var response = await metadata.ListNodesAsync(
                parent,
                MetadataLabels.PageSize,
                firstPage ? null : load.NextToken,
                filter,
                token);

            foreach (var item in response.Nodes)
            {
                if (item.Reference is not { } reference)
                {
                    // 没有引用的节点无法再回传，也无法展开：跳过而不是猜一个名字。
                    continue;
                }

                var name = string.IsNullOrWhiteSpace(item.DisplayName) ? reference.Name : item.DisplayName;
                load.Items.Add(new ObjectEntry(reference, name, reference.Kind));
            }

            load.NextToken = response.Page?.NextPageToken ?? string.Empty;
        }
        catch (OperationCanceledException)
        {
            // 收起 / 切页 / 退出：不是错误，也不写错误行；保留已读内容。
            canceled = true;
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            load.ErrorHeadline = failure.Headline;
            load.ErrorCode = failure.Code;
        }
        finally
        {
            load.Canceled = canceled;
            load.Loading = false;
            load.Loaded = !canceled || load.Items.Count > 0;
            RenderNode(node);
        }
    }

    /// <summary>把一行（连接 / 分组）翻译成 <c>ListNodesRequest</c> 的父引用与过滤条件。</summary>
    private bool TryBuildTreeRequest(
        TreeViewNode node,
        out ObjectRef parent,
        out NodeFilter? filter,
        out string reason)
    {
        parent = new ObjectRef();
        filter = null;
        reason = "这一行不对应任何可列出的层";

        string? connectionKey = node.Content switch
        {
            ConnectionNode connection => connection.Profile.Key,
            TreeRow { Payload: GroupPayload group } => group.ConnectionKey,
            _ => null,
        };

        if (connectionKey is null)
        {
            return false;
        }

        var sessionId = _session?.OpenResult(connectionKey)?.SessionId;
        if (string.IsNullOrEmpty(sessionId))
        {
            reason = "这条连接还没有浏览会话：先连接（双击连接行）再展开";
            return false;
        }

        switch (node.Content)
        {
            case ConnectionNode:
                // 目录层：kind = CATALOG 且不带任何名字（契约里的「列出全部库」）。
                parent = new ObjectRef { SessionId = sessionId, Kind = ObjectKind.Catalog };
                return true;

            case TreeRow { Payload: GroupPayload group }:
                if (MetadataLabels.KindsOf(group.Group) is not { } kinds)
                {
                    reason = $"{MetadataLabels.GroupLabel(group.Group)} 没有后端列表能力";
                    return false;
                }

                // 目录引用原样回传（服务端填的 catalog/schema/name），session_id 用本次会话的。
                parent = new ObjectRef
                {
                    SessionId = sessionId,
                    Catalog = group.Catalog.Catalog,
                    Schema = group.Catalog.Schema,
                    Name = group.Catalog.Name,
                    Kind = group.Catalog.Kind,
                };
                filter = new NodeFilter();
                filter.Kinds.AddRange(kinds);
                return true;

            default:
                return false;
        }
    }

    private void RenderNode(TreeViewNode node)
    {
        switch (node.Content)
        {
            case ConnectionNode connection:
                RenderCatalogLayer(node, connection.Profile);
                break;

            case TreeRow { Kind: TreeRowKind.Group, Payload: GroupPayload group }:
                RenderGroupLayer(node, group);
                break;
        }
    }

    /// <summary>连接层的子节点：库 → （建四个分组行，0 次 RPC）。</summary>
    private void RenderCatalogLayer(TreeViewNode node, ConnectionProfile profile)
    {
        var load = LoadFor(node);
        var children = new List<TreeViewNode>();

        if (!load.Loaded || load.Loading)
        {
            children.Add(PlaceholderNode());
        }

        foreach (var entry in load.Items)
        {
            children.Add(CatalogNode(profile.Key, entry));
        }

        if (load.ErrorHeadline is { } headline)
        {
            children.Add(ErrorNode($"读取失败：{headline}（{load.ErrorCode}）"));
        }

        if (!load.Loading && load.NextToken.Length > 0)
        {
            children.Add(LoadMoreNode());
        }

        if (load.Loaded && !load.Loading && load.Items.Count == 0 && load.ErrorHeadline is null)
        {
            children.Add(NoteNode("（这条连接里没有可见的库）"));
        }

        SetChildren(node, children);
    }

    /// <summary>分组层的子节点：对象 + 续拉 / 空 / 失败三态。</summary>
    private void RenderGroupLayer(TreeViewNode node, GroupPayload group)
    {
        var load = LoadFor(node);
        var children = new List<TreeViewNode>();

        if (!load.Loaded || load.Loading)
        {
            children.Add(PlaceholderNode());
        }

        foreach (var entry in load.Items)
        {
            children.Add(ObjectNode(group, entry));
        }

        if (load.ErrorHeadline is { } headline)
        {
            children.Add(ErrorNode($"读取失败：{headline}（{load.ErrorCode}）"));
        }

        if (!load.Loading && load.NextToken.Length > 0)
        {
            children.Add(LoadMoreNode());
        }

        if (load.Loaded && !load.Loading && load.Items.Count == 0 && load.ErrorHeadline is null)
        {
            children.Add(NoteNode(MetadataLabels.EmptyNote(group.Group)));
        }

        SetChildren(node, children);
    }

    private TreeViewNode CatalogNode(string connectionKey, ObjectEntry entry)
    {
        var row = new TreeRow(TreeRowKind.Catalog, entry.Name, new CatalogPayload(connectionKey, entry.Reference));
        var node = new TreeViewNode { Content = row };

        // 展开库本身 0 次 RPC：四个分组行在这里就建好，箭头因此始终可见。
        foreach (var group in MetadataLabels.Groups)
        {
            AddChild(node, GroupNode(connectionKey, entry.Reference, group));
        }

        return node;
    }

    private TreeViewNode GroupNode(string connectionKey, ObjectRef catalog, ObjectGroup group)
    {
        var row = new TreeRow(TreeRowKind.Group, MetadataLabels.GroupLabel(group), new GroupPayload(connectionKey, catalog, group));
        var node = new TreeViewNode { Content = row };

        if (MetadataLabels.UnsupportedNote(group) is { } note)
        {
            // 没有列表能力的组：只写一行说明，不发请求、不摆假数据。
            AddChild(node, NoteNode(note));
        }
        else
        {
            AddChild(node, PlaceholderNode());
        }

        return node;
    }

    private TreeViewNode ObjectNode(GroupPayload group, ObjectEntry entry)
    {
        var row = new TreeRow(TreeRowKind.Object, entry.Name, new ObjectPayload(group.ConnectionKey, group.Catalog, entry));
        return new TreeViewNode { Content = row };
    }

    private TreeViewNode PlaceholderNode() => new() { Content = new TreeRow(TreeRowKind.Loading, LoadingPlaceholder) };

    private TreeViewNode LoadMoreNode() => new() { Content = new TreeRow(TreeRowKind.LoadMore, LoadMoreText) };

    private TreeViewNode ErrorNode(string text) => new() { Content = new TreeRow(TreeRowKind.Error, text) };

    private TreeViewNode NoteNode(string text) => new() { Content = new TreeRow(TreeRowKind.Note, text) };

    private void AddChild(TreeViewNode parent, TreeViewNode child)
    {
        if (child.Content is TreeRow row)
        {
            _rowOwners[row] = parent;
        }

        parent.Children.Add(child);
    }

    private void SetChildren(TreeViewNode node, IReadOnlyList<TreeViewNode> children)
    {
        if (node.Children.Count > 0)
        {
            var existing = new List<TreeViewNode>(node.Children);
            node.Children.Clear();
            foreach (var child in existing)
            {
                if (child.Content is TreeRow row)
                {
                    _rowOwners.Remove(row);
                }
            }
        }

        foreach (var child in children)
        {
            AddChild(node, child);
        }
    }

    private NodeLoad LoadFor(TreeViewNode node)
    {
        if (_loads.TryGetValue(node, out var existing))
        {
            return existing;
        }

        var load = new NodeLoad();
        _loads[node] = load;
        return load;
    }

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

    // ------------------------------------------------------------------ 选中（唯一入口）

    private MetadataSelection SelectionFromTree()
    {
        var content = ObjectTree.SelectedNode?.Content;
        return content switch
        {
            ConnectionNode connection => MetadataSelection.ForConnection(connection.Profile.Key, connection.Profile.Name),
            TreeRow { Kind: TreeRowKind.Catalog, Payload: CatalogPayload catalog } =>
                MetadataSelection.ForCatalog(catalog.ConnectionKey, ConnectionNameOf(catalog.ConnectionKey), catalog.Reference),
            TreeRow { Kind: TreeRowKind.Group, Payload: GroupPayload group } =>
                MetadataSelection.ForGroup(group.ConnectionKey, ConnectionNameOf(group.ConnectionKey), group.Catalog, group.Group),
            TreeRow { Kind: TreeRowKind.Object, Payload: ObjectPayload target } =>
                MetadataSelection.ForObject(target.ConnectionKey, ConnectionNameOf(target.ConnectionKey), target.Catalog, target.Entry),
            _ => MetadataSelection.None,
        };
    }

    /// <summary>
    /// 换选中：先落这一份选中状态，再按它重算页面与面板（单向：状态 → 内容）。
    /// </summary>
    /// <param name="selection">新的选中。</param>
    /// <param name="highlightRow">页面里要保留的行高亮（树选中时传 -1，清掉上一次浏览的高亮）。</param>
    private void Select(MetadataSelection selection, int highlightRow = -1)
    {
        _selection = selection;

        var scope = ObjectsScope.For(selection);
        var pageKey = scope?.Signature ?? "none";
        _objectsRow = highlightRow;

        if (_objects is null || !string.Equals(_objects.Signature, pageKey, StringComparison.Ordinal))
        {
            CancelPage();
            _objects = null;
            _pageTask = LoadFirstPageAsync(scope);
        }
        else
        {
            RenderObjectsPage();
        }

        if (!string.Equals(_detailKey, selection.DetailKey, StringComparison.Ordinal))
        {
            _detailKey = selection.DetailKey;
            _detailTask = LoadInspectorAsync(selection);
        }
        else
        {
            RenderInspector();
        }

        ApplyWorkspaceVisibility();
    }

    private string ConnectionNameOf(string key) =>
        _profiles.FirstOrDefault(profile => string.Equals(profile.Key, key, StringComparison.Ordinal))?.Name ?? key;

    // ------------------------------------------------------------------ Objects 页

    /// <summary>作用域换了一页：建一份新的页面状态并拉第一页。</summary>
    private async Task LoadFirstPageAsync(ObjectsScope? scope)
    {
        if (scope is null)
        {
            _objects = null;
            RenderObjectsPage();
            return;
        }

        var state = new ObjectsPageState
        {
            Signature = scope.Signature,
            Group = scope.Group,
            CatalogName = scope.CatalogName,
            Loading = true,
        };

        _objects = state;
        RenderObjectsPage();
        await LoadPageAsync(scope, state, firstPage: true);
    }

    private async Task LoadPageAsync(ObjectsScope scope, ObjectsPageState state, bool firstPage)
    {
        var metadata = _session?.Metadata;
        if (metadata is null)
        {
            state.Loading = false;
            state.LoadingMore = false;
            state.Headline = "本地后端尚未就绪";
            state.Code = "backend.not_acquired";
            if (ReferenceEquals(_objects, state))
            {
                RenderObjectsPage();
            }

            return;
        }

        CancelPage();
        _pageCts = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        var token = _pageCts.Token;

        var filter = new NodeFilter();
        filter.Kinds.AddRange(scope.Kinds);

        try
        {
            var response = await metadata.ListNodesAsync(
                scope.Parent,
                MetadataLabels.PageSize,
                firstPage ? null : state.NextToken,
                filter,
                token);

            if (!ReferenceEquals(_objects, state))
            {
                // 作用域已经换了：这一页的结果作废（不往新作用域里拼旧数据）。
                return;
            }

            foreach (var item in response.Nodes)
            {
                if (item.Reference is not { } reference)
                {
                    continue;
                }

                var name = string.IsNullOrWhiteSpace(item.DisplayName) ? reference.Name : item.DisplayName;
                state.Items.Add(new ObjectEntry(reference, name, reference.Kind));
            }

            state.NextToken = response.Page?.NextPageToken ?? string.Empty;
            state.Headline = null;
            state.Code = null;
        }
        catch (OperationCanceledException)
        {
            // 切页 / 换作用域 / 退出：保留已加载行，不写错误。
        }
        catch (Exception error)
        {
            if (ReferenceEquals(_objects, state))
            {
                var failure = SqarvoFailure.FromException(error);
                state.Headline = failure.Headline;
                state.Code = failure.Code;
            }
        }
        finally
        {
            if (ReferenceEquals(_objects, state))
            {
                state.Loading = false;
                state.LoadingMore = false;
                RenderObjectsPage();
            }
        }
    }

    private void CancelPage()
    {
        try
        {
            _pageCts?.Cancel();
        }
        catch (ObjectDisposedException)
        {
            // 已经释放：没有在途请求可取消。
        }

        _pageCts?.Dispose();
        _pageCts = null;
    }

    private void CancelDetail()
    {
        try
        {
            _detailCts?.Cancel();
        }
        catch (ObjectDisposedException)
        {
            // 同上。
        }

        _detailCts?.Dispose();
        _detailCts = null;
    }

    /// <summary>重建 Objects 页（表头 + 行 + 状态 + 「加载更多…」），全部从状态现算。</summary>
    private void RenderObjectsPage()
    {
        ObjectsTable.Children.Clear();
        ObjectsTable.RowDefinitions.Clear();
        ObjectsTable.ColumnDefinitions.Clear();

        var scope = ObjectsScope.For(_selection);
        var state = _objects;
        var hasScope = scope is not null && state is not null
            && string.Equals(state.Signature, scope.Signature, StringComparison.Ordinal);

        ObjectsStatus.Text = ObjectsStatusText(scope, state);
        ObjectsStatus.Visibility = ObjectsStatus.Text.Length == 0 ? Visibility.Collapsed : Visibility.Visible;

        // 状态行也登记可访问名：--report-shell 能直接读到「共 N 个对象」这类真实计数。
        AutomationProperties.SetName(
            ObjectsStatus,
            ObjectsStatus.Text.Length == 0 ? "对象列表状态" : ObjectsStatus.Text);

        if (scope is not null)
        {
            // 表头先立着：作用域刚换、第一页还没回来时行区为空，状态行说明「正在读取…」。
            AddObjectsHeaderRow(scope);

            if (hasScope)
            {
                for (var index = 0; index < state!.Items.Count; index++)
                {
                    AddObjectsRow(index, state.Items[index]);
                }
            }
        }

        var canLoadMore = hasScope && state!.HasMore && !state.Loading && !state.LoadingMore;
        ObjectsLoadMore.Visibility = canLoadMore ? Visibility.Visible : Visibility.Collapsed;
        ObjectsLoadMore.IsEnabled = canLoadMore;
    }

    private string ObjectsStatusText(ObjectsScope? scope, ObjectsPageState? state)
    {
        if (scope is null)
        {
            // Functions / Queries：如实说明没有列表能力（不摆假数据）。
            return _selection.Kind == SelectionKind.Group
                ? MetadataLabels.UnsupportedNote(_selection.Group) ?? string.Empty
                : string.Empty;
        }

        if (state is null || !string.Equals(state.Signature, scope.Signature, StringComparison.Ordinal))
        {
            return "正在读取…";
        }

        if (state.Headline is { Length: > 0 } headline)
        {
            return $"读取失败：{headline}（{state.Code}）";
        }

        if (state.Loading)
        {
            return "正在读取…";
        }

        if (state.Items.Count == 0)
        {
            return MetadataLabels.EmptyNote(scope.Group);
        }

        // 文案与 Linux 的 Objects 页状态行一致：「共 N 个对象」/「共 N 个对象（还有更多）」。
        return state.HasMore
            ? $"共 {state.Items.Count} 个对象（还有更多）"
            : $"共 {state.Items.Count} 个对象";
    }

    private void AddObjectsHeaderRow(ObjectsScope scope)
    {
        var grid = NewObjectsRow(Metrics.ColumnHeaderMinHeight, background: null);

        // 表头三个单元格各给一个可访问名（= 列头文字），--report-shell 才能断言列序。
        var numberHeader = AddCell(grid, 0, "行号", secondary: true, rightAligned: true, fontSize: Metrics.FontStatus, mono: true, header: true);
        var nameHeader = AddCell(grid, 1, "表名", secondary: true, rightAligned: false, fontSize: Metrics.FontStatus, mono: false, header: true);
        var kindHeader = AddCell(grid, 2, "类型", secondary: true, rightAligned: false, fontSize: Metrics.FontStatus, mono: false, header: true);
        AutomationProperties.SetName(numberHeader, "行号");
        AutomationProperties.SetName(nameHeader, "表名");
        AutomationProperties.SetName(kindHeader, "类型");

        AutomationProperties.SetName(
            grid,
            $"对象列表表头 {scope.CatalogName} · {MetadataLabels.GroupLabel(scope.Group)}");

        Grid.SetRow(grid, 0);
        ObjectsTable.RowDefinitions.Add(new RowDefinition { Height = new GridLength(Metrics.ColumnHeaderMinHeight) });
        ObjectsTable.Children.Add(grid);

        var divider = new Rectangle
        {
            Height = Metrics.BorderWidthPxNeverScaled,
            Fill = ThemeBrush("SqarvoDividerBrush"),
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };

        Grid.SetRow(divider, 1);
        ObjectsTable.RowDefinitions.Add(new RowDefinition { Height = new GridLength(Metrics.BorderWidthPxNeverScaled) });
        ObjectsTable.Children.Add(divider);
    }

    private void AddObjectsRow(int index, ObjectEntry entry)
    {
        var selected = index == _objectsRow;
        var grid = NewObjectsRow(
            Metrics.RowHeightData,
            selected ? ThemeBrush("SqarvoSelectionBrush") : ThemeBrush("SqarvoTransparentBrush"));

        AddCell(grid, 0, (index + 1).ToString(CultureInfo.InvariantCulture), secondary: true, rightAligned: true, fontSize: Metrics.FontStatus, mono: true);
        AddCell(grid, 1, entry.Name, secondary: false, rightAligned: false, fontSize: Metrics.FontUi, mono: false);
        AddCell(grid, 2, entry.KindLabel, secondary: true, rightAligned: false, fontSize: Metrics.FontStatus, mono: false);

        // 通栏可访问名：--report-shell 断言「行号 名称 类型」就靠它（只有一个 TextBlock 时读出的是纯名字）。
        AutomationProperties.SetName(grid, $"{index + 1} {entry.Name} {entry.KindLabel}");

        var row = index;
        grid.Tapped += (_, args) =>
        {
            args.Handled = true;
            SelectObjectRow(row, announce: false);
        };
        grid.DoubleTapped += (_, args) =>
        {
            args.Handled = true;
            SelectObjectRow(row, announce: true);
        };

        var rowIndex = ObjectsTable.RowDefinitions.Count;
        ObjectsTable.RowDefinitions.Add(new RowDefinition { Height = new GridLength(Metrics.RowHeightData) });
        Grid.SetRow(grid, rowIndex);
        ObjectsTable.Children.Add(grid);
    }

    /// <summary>一行三列的行容器（行号 / 名称 / 类型；列宽全部来自度量）。</summary>
    private Grid NewObjectsRow(double height, Brush? background)
    {
        var grid = new Grid { Height = height };

        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.RowNumberWidth) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Metrics.ColumnWidth) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        if (background is not null)
        {
            var backdrop = new Border { Background = background };
            Grid.SetColumnSpan(backdrop, 3);
            grid.Children.Add(backdrop);
        }

        return grid;
    }

    private TextBlock AddCell(
        Grid grid,
        int column,
        string text,
        bool secondary,
        bool rightAligned,
        int fontSize,
        bool mono,
        bool header = false)
    {
        var block = new TextBlock
        {
            Text = text,
            FontSize = fontSize,
            // 列头 title(600) / 行号 ui(400) / 数据 cell(500)：与数据网格同一口径
            // （Linux 的 Objects 与数据网格共用一套 header / .sqarvo-cell 规则）。
            FontWeight = header ? TypeTokens.Title : mono ? TypeTokens.Ui : TypeTokens.Cell,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = rightAligned ? HorizontalAlignment.Right : HorizontalAlignment.Left,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };

        if (mono)
        {
            block.FontFamily = MonoFont();
        }

        if (ThemeBrush(secondary ? "SqarvoTextSecondaryBrush" : "SqarvoTextPrimaryBrush") is { } brush)
        {
            block.Foreground = brush;
        }

        Grid.SetColumn(block, column);
        grid.Children.Add(block);
        return block;
    }

    /// <summary>选中页面里的一行：同步信息面板，但**不反写树的选中**（否则会形成回环）。</summary>
    private void SelectObjectRow(int index, bool announce)
    {
        var scope = ObjectsScope.For(_selection);
        var state = _objects;
        if (scope is null || state is null || index < 0 || index >= state.Items.Count)
        {
            return;
        }

        var entry = state.Items[index];
        Select(
            MetadataSelection.ForObject(_selection.ConnectionKey, _selection.ConnectionName, scope.Parent, entry),
            highlightRow: index);

        ObjectsListScroll.Focus(FocusState.Programmatic);

        if (announce)
        {
            // 这一层只同步信息面板：数据页的入口是树上的双击 / 回车 / 右键「浏览数据」（#173）。
            _notice = $"{entry.Name}（{entry.KindLabel}）：信息面板已打开；双击左侧树里的这一项或右键「浏览数据」打开数据页";
            ApplyBackendState();
        }
    }

    private void OnObjectsKeyDown(object sender, KeyRoutedEventArgs args)
    {
        var state = _objects;
        if (state is null || state.Items.Count == 0)
        {
            return;
        }

        switch (args.Key)
        {
            case VirtualKey.Up:
                SelectObjectRow(Math.Max(0, _objectsRow - 1), announce: false);
                args.Handled = true;
                break;

            case VirtualKey.Down:
                SelectObjectRow(Math.Min(state.Items.Count - 1, _objectsRow + 1), announce: false);
                args.Handled = true;
                break;

            case VirtualKey.Enter:
                SelectObjectRow(_objectsRow < 0 ? 0 : _objectsRow, announce: true);
                args.Handled = true;
                break;
        }
    }

    private void OnObjectsLoadMoreClick(object sender, RoutedEventArgs args)
    {
        var scope = ObjectsScope.For(_selection);
        var state = _objects;
        if (scope is null || state is null || state.LoadingMore || !state.HasMore)
        {
            return;
        }

        state.LoadingMore = true;
        RenderObjectsPage();
        _pageTask = LoadPageAsync(scope, state, firstPage: false);
    }

    // ------------------------------------------------------------------ 信息面板

    private void RenderInspector()
    {
        InspectorBody.Children.Clear();

        switch (_selection.Kind)
        {
            case SelectionKind.Connection:
                BuildConnectionInspector();
                break;

            case SelectionKind.Catalog:
            case SelectionKind.Group:
                BuildCatalogInspector();
                break;

            case SelectionKind.Object:
                BuildObjectInspector();
                break;

            default:
                // 没有选中对象：内容区留白（规范 §5，不写「没有选中对象」这类否定句）。
                break;
        }
    }

    private void ShowInspector(StackPanel content)
    {
        var scroll = new ScrollViewer
        {
            Content = content,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollMode = ScrollMode.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
        };

        InspectorBody.Children.Clear();
        InspectorBody.Children.Add(scroll);
    }

    private void BuildConnectionInspector()
    {
        var profile = _profiles.FirstOrDefault(item => string.Equals(item.Key, _selection.ConnectionKey, StringComparison.Ordinal));
        var panel = NewPanel();

        if (profile is null)
        {
            panel.Children.Add(Line("连接已不在配置里", size: Metrics.FontTitle, weight: TypeTokens.Heading));
            panel.Children.Add(Line("connections.json 里已经没有这一条；错误码 config.invalid", secondary: true));
            ShowInspector(panel);
            return;
        }

        panel.Children.Add(Line(profile.Name, size: Metrics.FontTitle, weight: TypeTokens.Heading));
        panel.Children.Add(Line(profile.Driver, secondary: true));
        panel.Children.Add(Divider());

        AddField(panel, "驱动", profile.Driver);
        AddField(panel, "地址", profile.IsFileDriver ? profile.FilePath : $"{profile.Host}:{profile.Port.ToString(CultureInfo.InvariantCulture)}");
        AddField(panel, "默认库", profile.Database);
        AddField(panel, "用户", profile.Username);
        AddField(panel, "TLS", profile.TlsMode);
        AddField(panel, "凭据引用", profile.CredentialRef);

        if (_session?.OpenResult(profile.Key) is { } open)
        {
            AddField(panel, "服务端版本", open.ServerVersion);
            AddField(panel, "会话", ShortSession(open.SessionId));
        }

        ShowInspector(panel);
    }

    private void BuildCatalogInspector()
    {
        var catalog = _selection.Catalog;
        var panel = NewPanel();
        panel.Children.Add(Line(string.IsNullOrEmpty(_selection.CatalogName) ? "（未命名库）" : _selection.CatalogName, size: Metrics.FontTitle, weight: TypeTokens.Heading));
        panel.Children.Add(Line("库", secondary: true));
        panel.Children.Add(Divider());

        // 计数只来自**已展开**的分组层：没展开过就不出现，不主动全量统计、不写 0 冒充。
        if (TryLoadedGroupCount(_selection.ConnectionKey, catalog, ObjectGroup.Tables, out var tables))
        {
            AddField(panel, "表", $"{tables} 个");
        }

        if (TryLoadedGroupCount(_selection.ConnectionKey, catalog, ObjectGroup.Views, out var views))
        {
            AddField(panel, "视图", $"{views} 个");
        }

        if (_selection.Kind == SelectionKind.Group)
        {
            AddField(panel, "当前分组", MetadataLabels.GroupLabel(_selection.Group));
        }

        ShowInspector(panel);
    }

    /// <summary>
    /// 某个库下某一组「已展开并读到数据」的条数（信息面板的计数只从这里来）。
    /// </summary>
    /// <returns>这一组已经读过至少一页时为 <see langword="true"/>（计数可以是 0：确实没有对象）。</returns>
    private bool TryLoadedGroupCount(string connectionKey, ObjectRef? catalog, ObjectGroup group, out int count)
    {
        count = 0;
        if (catalog is null || !_nodes.TryGetValue(connectionKey, out var connection))
        {
            return false;
        }

        foreach (var catalogNode in connection.Children)
        {
            if (catalogNode.Content is not TreeRow { Payload: CatalogPayload payload }
                || !SameReference(payload.Reference, catalog))
            {
                continue;
            }

            foreach (var groupNode in catalogNode.Children)
            {
                if (groupNode.Content is TreeRow { Payload: GroupPayload groupPayload }
                    && groupPayload.Group == group
                    && _loads.TryGetValue(groupNode, out var load)
                    && load.Loaded)
                {
                    count = load.Items.Count;
                    return true;
                }
            }
        }

        return false;
    }

    /// <summary>两个引用是否指向同一个对象（不比较 session：会话换掉之后引用仍应匹配）。</summary>
    private static bool SameReference(ObjectRef left, ObjectRef right) =>
        string.Equals(left.Catalog, right.Catalog, StringComparison.Ordinal)
        && string.Equals(left.Schema, right.Schema, StringComparison.Ordinal)
        && string.Equals(left.Name, right.Name, StringComparison.Ordinal)
        && left.Kind == right.Kind;

    private void BuildObjectInspector()
    {
        if (_selection.Object is not { } entry)
        {
            return;
        }

        var panel = NewPanel();
        panel.Children.Add(Line(entry.Name, size: Metrics.FontTitle, weight: TypeTokens.Heading));
        panel.Children.Add(Line(entry.KindLabel, secondary: true));

        // [DDL] / [表信息]：两个独立按钮（不是 ToggleButton，也不是分段容器——#170 的回退教训）。
        var switchBar = new StackPanel { Orientation = Orientation.Horizontal, Spacing = Metrics.GridPaddingEnd };
        switchBar.Children.Add(PaneButton("DDL", _ddlPane, () =>
        {
            _ddlPane = true;
            RenderInspector();
        }));
        switchBar.Children.Add(PaneButton("表信息", !_ddlPane, () =>
        {
            _ddlPane = false;
            RenderInspector();
        }));
        panel.Children.Add(switchBar);
        panel.Children.Add(Divider());

        if (_ddlPane)
        {
            AddDdlPane(panel);
        }
        else
        {
            AddInfoPane(panel);
        }

        ShowInspector(panel);
    }

    private Button PaneButton(string text, bool active, Action onClick)
    {
        var button = new Button
        {
            Content = text,
            FontSize = Metrics.FontPanel,
            FontWeight = TypeTokens.Ui,
            Height = Metrics.SecondaryMinHeight,
            Padding = new Thickness(Metrics.GridPaddingEnd, 0, Metrics.GridPaddingEnd, 0),
            VerticalAlignment = VerticalAlignment.Center,
        };

        if (ThemeBrush(active ? "SqarvoAccentTextBrush" : "SqarvoTextPrimaryBrush") is { } foreground)
        {
            button.Foreground = foreground;
        }

        if (ThemeBrush(active ? "SqarvoSelectionBrush" : "SqarvoTransparentBrush") is { } background)
        {
            button.Background = background;
        }

        AutomationProperties.SetName(button, text);
        button.Click += (_, _) => onClick();
        return button;
    }

    private void AddDdlPane(StackPanel panel)
    {
        var detail = _detail;

        if (detail is null || detail.DdlLoading)
        {
            panel.Children.Add(Line("正在读取 DDL…", secondary: true));
            return;
        }

        if (detail.DdlHeadline is { Length: > 0 } headline)
        {
            // 失败必须显示错误（中文标题 + 稳定错误码），不是一块空面板。
            panel.Children.Add(Line($"DDL 读取失败：{headline}（{detail.DdlCode}）", warning: true));
            return;
        }

        if (detail.Ddl is not { } ddl)
        {
            panel.Children.Add(Line("DDL 尚未读取", secondary: true));
            return;
        }

        if (!ddl.Exact)
        {
            panel.Children.Add(Line("⚠ 这份 DDL 由后端拼装，不是数据库原文；信息可能有损失。", warning: true));
            if (ddl.SourceDetail.Length > 0)
            {
                panel.Children.Add(Line(ddl.SourceDetail, secondary: true));
            }
        }

        if (string.IsNullOrWhiteSpace(ddl.Text))
        {
            panel.Children.Add(Line("（后端返回的 DDL 正文为空）", secondary: true));
            return;
        }

        var text = new TextBlock
        {
            Text = ddl.Text,
            FontFamily = MonoFont(),
            FontSize = Metrics.FontPanel,
            FontWeight = TypeTokens.Ui,
            TextWrapping = TextWrapping.NoWrap,
            IsTextSelectionEnabled = true,
        };

        if (ThemeBrush("SqarvoTextPrimaryBrush") is { } brush)
        {
            text.Foreground = brush;
        }

        var scroll = new ScrollViewer
        {
            Content = text,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollMode = ScrollMode.Auto,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
        };

        AutomationProperties.SetName(text, "DDL 正文");
        panel.Children.Add(scroll);
    }

    private void AddInfoPane(StackPanel panel)
    {
        var detail = _detail;

        if (detail is null || detail.ColumnsLoading)
        {
            panel.Children.Add(Line("正在读取列…", secondary: true));
            return;
        }

        if (detail.ColumnsHeadline is { Length: > 0 } headline)
        {
            panel.Children.Add(Line($"列读取失败：{headline}（{detail.ColumnsCode}）", warning: true));
            return;
        }

        if (detail.PrimaryKeyHeadline is { Length: > 0 } primaryHeadline)
        {
            // 主键读不到时不画任何主键标记（不猜），但要说清为什么没有标记。
            panel.Children.Add(Line($"主键读取失败：{primaryHeadline}（{detail.PrimaryKeyCode}）", warning: true));
        }

        if (detail.Columns.Count == 0)
        {
            panel.Children.Add(Line("（这张表没有列）", secondary: true));
            return;
        }

        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var row = 0;
        AddGridRow(grid, row, "列名", "类型", "可空", "主键", header: true);
        row++;

        foreach (var column in detail.Columns)
        {
            AddGridRow(grid, row, column.Name, column.TypeLabel, column.NullabilityLabel, column.PrimaryKeyLabel, header: false);
            row++;
        }

        AutomationProperties.SetName(grid, $"列信息 {_selection.Object?.Name}");
        panel.Children.Add(grid);
    }

    /// <summary>在列信息网格里加一行（表头与数据行同一套列，字段拿不到时留空而不是写占位）。</summary>
    private void AddGridRow(Grid grid, int row, string name, string type, string nullable, string primaryKey, bool header)
    {
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        var nameCell = AddInspectorCell(grid, row, 0, name, header);
        AddInspectorCell(grid, row, 1, type, header);
        AddInspectorCell(grid, row, 2, nullable, header);
        AddInspectorCell(grid, row, 3, primaryKey, header);

        if (!header)
        {
            // 整行可访问名：--report-shell 断言「列名 类型 可空 主键」就靠它（单元格本身不带名字）。
            var rowText = string.Join(" ", new[] { name, type, nullable, primaryKey }.Where(part => part.Length > 0));
            AutomationProperties.SetName(nameCell, rowText);
        }
    }

    private TextBlock AddInspectorCell(Grid grid, int row, int column, string text, bool header)
    {
        var block = new TextBlock
        {
            Text = text,
            FontSize = Metrics.FontPanel,
            FontWeight = header ? TypeTokens.Title : TypeTokens.Ui,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };

        if (ThemeBrush(header || text.Length == 0 ? "SqarvoTextSecondaryBrush" : "SqarvoTextPrimaryBrush") is { } brush)
        {
            block.Foreground = brush;
        }

        Grid.SetRow(block, row);
        Grid.SetColumn(block, column);
        grid.Children.Add(block);
        return block;
    }

    private void AddField(StackPanel panel, string label, string? value)
    {
        if (string.IsNullOrWhiteSpace(value))
        {
            // 拿不到的字段不出现（不写 — / N/A 冒充）。
            return;
        }

        panel.Children.Add(Line($"{label}：{value}", secondary: true));
    }

    private StackPanel NewPanel() => new()
    {
        Spacing = Metrics.GridPaddingEnd,
        Padding = new Thickness(0, 0, Metrics.GridPaddingEnd, 0),
    };

    /// <summary>
    /// 信息面板里的一行文字。
    /// </summary>
    /// <remarks>
    /// 字重默认 <c>ui</c>(400)：信息面板是次要信息，不跟数据网格抢注意力（Linux 的
    /// <c>.sqarvo-panel-text</c> 同一口径）。面板标题（卡片 / 分节 / 对象名）显式传
    /// <see cref="TypeTokens.Heading"/>。
    /// </remarks>
    private TextBlock Line(string text, bool secondary = false, bool warning = false, double? size = null, FontWeight? weight = null)
    {
        var block = new TextBlock
        {
            Text = text,
            FontSize = size ?? Metrics.FontPanel,
            FontWeight = weight ?? TypeTokens.Ui,
            TextWrapping = TextWrapping.Wrap,
            IsTextSelectionEnabled = true,
        };

        var key = warning ? "SqarvoWarningTextBrush" : secondary ? "SqarvoTextSecondaryBrush" : "SqarvoTextPrimaryBrush";
        if (ThemeBrush(key) is { } brush)
        {
            block.Foreground = brush;
        }

        return block;
    }

    private Rectangle Divider() => new()
    {
        Height = Metrics.BorderWidthPxNeverScaled,
        Fill = ThemeBrush("SqarvoDividerBrush"),
        HorizontalAlignment = HorizontalAlignment.Stretch,
    };

    private static string ShortSession(string sessionId) =>
        string.IsNullOrEmpty(sessionId)
            ? string.Empty
            : sessionId.Length <= 8 ? sessionId : $"{sessionId[..8]}…";

    // ---- 信息面板的数据（列 / 主键 / DDL） ---------------------------------------

    private async Task LoadInspectorAsync(MetadataSelection selection)
    {
        CancelDetail();

        if (selection.Kind != SelectionKind.Object || selection.Object is not { } entry)
        {
            _detail = null;
            RenderInspector();
            return;
        }

        var detail = new ObjectDetail { ColumnsLoading = true, DdlLoading = true };
        _detail = detail;
        RenderInspector();

        var metadata = _session?.Metadata;
        if (metadata is null)
        {
            detail.ColumnsLoading = false;
            detail.DdlLoading = false;
            detail.ColumnsHeadline = "本地后端尚未就绪";
            detail.ColumnsCode = "backend.not_acquired";
            detail.DdlHeadline = "本地后端尚未就绪";
            detail.DdlCode = "backend.not_acquired";
            RenderInspector();
            return;
        }

        _detailCts = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        var token = _detailCts.Token;

        await Task.WhenAll(
            LoadColumnsAsync(metadata, entry.Reference, detail, token),
            LoadDdlAsync(metadata, entry.Reference, detail, token));
    }

    private async Task LoadColumnsAsync(
        Sqarvo.Core.Clients.MetadataClient metadata,
        ObjectRef target,
        ObjectDetail detail,
        CancellationToken token)
    {
        try
        {
            var columns = await metadata.GetColumnsAsync(target, token);

            // 主键单独一个 RPC（协议的选择：布尔标志表达不了复合主键顺序）。
            var primaryKey = new Dictionary<string, uint>(StringComparer.Ordinal);
            try
            {
                foreach (var column in await metadata.GetPrimaryKeyAsync(target, token))
                {
                    primaryKey[column.Name] = column.Position;
                }
            }
            catch (OperationCanceledException)
            {
                throw;
            }
            catch (Exception error)
            {
                var failure = SqarvoFailure.FromException(error);
                detail.PrimaryKeyHeadline = failure.Headline;
                detail.PrimaryKeyCode = failure.Code;
            }

            foreach (var column in columns.OrderBy(item => item.Ordinal))
            {
                var type = column.Type?.OriginalTypeName ?? string.Empty;
                primaryKey.TryGetValue(column.Name, out var position);
                detail.Columns.Add(new ColumnEntry(column.Name, type, column.Nullable, position));
            }
        }
        catch (OperationCanceledException)
        {
            // 换选中 / 退出：正常路径。
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            detail.ColumnsHeadline = failure.Headline;
            detail.ColumnsCode = failure.Code;
        }
        finally
        {
            detail.ColumnsLoading = false;
            if (ReferenceEquals(_detail, detail))
            {
                RenderInspector();
            }
        }
    }

    private async Task LoadDdlAsync(
        Sqarvo.Core.Clients.MetadataClient metadata,
        ObjectRef target,
        ObjectDetail detail,
        CancellationToken token)
    {
        try
        {
            var response = await metadata.GetDdlAsync(target, token);
            detail.Ddl = new DdlResult(response.Ddl ?? string.Empty, response.Exact, response.SourceDetail ?? string.Empty);
        }
        catch (OperationCanceledException)
        {
            // 同上。
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            detail.DdlHeadline = failure.Headline;
            detail.DdlCode = failure.Code;
        }
        finally
        {
            detail.DdlLoading = false;
            if (ReferenceEquals(_detail, detail))
            {
                RenderInspector();
            }
        }
    }

    // ------------------------------------------------------------------ QA 探针（--qa-metadata）

    /// <summary>
    /// <c>--qa-metadata</c>：连接 → 展开库 → 展开 <c>Tables</c> → 选分组 → 选第一张表，
    /// 把每一层的真实计数与列名打到 <c>[metadata]</c> 行，供验收脚本判读。
    /// </summary>
    /// <param name="identity">连接身份键（<see cref="ConnectionProfile.Key"/>）。</param>
    /// <param name="catalogName">要展开的库名；<see langword="null"/> 时取第一个可见库。</param>
    /// <param name="cancellationToken">放弃这次探针。</param>
    /// <returns>整条链路是否都拿到了数据。</returns>
    /// <remarks>
    /// 只走与界面完全相同的加载路径（<see cref="EnsureCatalogsAsync"/> / <see cref="EnsureGroupLoadedAsync"/> /
    /// <see cref="Select"/>），因此它验证的就是用户点开树时会发生的事——不是另一套快路径。
    /// </remarks>
    public async Task<bool> ProbeMetadataAsync(string identity, string? catalogName, CancellationToken cancellationToken)
    {
        try
        {
            if (!await WaitForConnectionNodeAsync(identity, cancellationToken).ConfigureAwait(true))
            {
                ConsoleBridge.WriteLine($"[metadata] connection-node=missing key={identity}");
                return false;
            }

            var connectionNode = _nodes[identity];
            connectionNode.IsExpanded = true;
            await EnsureCatalogsAsync(connectionNode).ConfigureAwait(true);

            var catalogs = new List<(TreeViewNode Node, CatalogPayload Payload)>();
            foreach (var child in connectionNode.Children)
            {
                if (child.Content is TreeRow { Payload: CatalogPayload payload })
                {
                    catalogs.Add((child, payload));
                }
            }

            ConsoleBridge.WriteLine(
                $"[metadata] catalogs={catalogs.Count} names={string.Join(",", catalogs.Select(item => item.Payload.Reference.Catalog))}");

            if (catalogs.Count == 0)
            {
                return false;
            }

            var chosen = catalogName is { Length: > 0 }
                ? catalogs.FirstOrDefault(item => string.Equals(item.Payload.Reference.Catalog, catalogName, StringComparison.OrdinalIgnoreCase))
                : catalogs[0];

            if (chosen.Node is null)
            {
                chosen = catalogs[0];
            }

            chosen.Node.IsExpanded = true;

            // 选库：Objects 页列出该库的表，信息面板显示库。
            Select(MetadataSelection.ForCatalog(identity, ConnectionNameOf(identity), chosen.Payload.Reference));
            await Task.WhenAll(_pageTask, _detailTask).ConfigureAwait(true);
            ConsoleBridge.WriteLine(
                $"[metadata] catalog={chosen.Payload.Reference.Catalog} page-items={_objects?.Items.Count ?? 0} page-error={_objects?.Code ?? "-"}");

            TreeViewNode? tablesGroup = null;
            foreach (var child in chosen.Node.Children)
            {
                if (child.Content is TreeRow { Payload: GroupPayload payload } && payload.Group == ObjectGroup.Tables)
                {
                    tablesGroup = child;
                    break;
                }
            }

            if (tablesGroup is null)
            {
                ConsoleBridge.WriteLine("[metadata] tables-group=missing");
                return false;
            }

            tablesGroup.IsExpanded = true;
            await EnsureGroupLoadedAsync(tablesGroup).ConfigureAwait(true);
            var treeTables = _loads.TryGetValue(tablesGroup, out var groupLoad) ? groupLoad.Items.Count : -1;
            ConsoleBridge.WriteLine($"[metadata] tree-tables={treeTables} tree-error={groupLoad?.ErrorCode ?? "-"}");

            ObjectTree.SelectedNode = tablesGroup;
            Select(MetadataSelection.ForGroup(identity, ConnectionNameOf(identity), chosen.Payload.Reference, ObjectGroup.Tables));
            await Task.WhenAll(_pageTask, _detailTask).ConfigureAwait(true);

            TreeViewNode? firstTable = null;
            ObjectEntry? firstEntry = null;
            foreach (var child in tablesGroup.Children)
            {
                if (child.Content is TreeRow { Payload: ObjectPayload payload })
                {
                    firstTable = child;
                    firstEntry = payload.Entry;
                    break;
                }
            }

            if (firstTable is null || firstEntry is null)
            {
                ConsoleBridge.WriteLine($"[metadata] first-table=missing page-items={_objects?.Items.Count ?? 0}");
                return false;
            }

            ObjectTree.SelectedNode = firstTable;
            Select(MetadataSelection.ForObject(identity, ConnectionNameOf(identity), chosen.Payload.Reference, firstEntry));
            await Task.WhenAll(_pageTask, _detailTask).ConfigureAwait(true);

            var detail = _detail;
            ConsoleBridge.WriteLine(
                $"[metadata] table={firstEntry.Name} columns={detail?.Columns.Count ?? 0} " +
                $"pk-error={detail?.PrimaryKeyCode ?? "-"} ddl-len={detail?.Ddl?.Text.Length ?? -1} " +
                $"ddl-exact={detail?.Ddl?.Exact.ToString() ?? "-"} ddl-error={detail?.DdlCode ?? "-"}");

            if (detail is { Columns.Count: > 0 })
            {
                var columns = detail.Columns
                    .Take(8)
                    .Select(column => $"{column.Name}:{column.TypeLabel}{(column.PrimaryKeyPosition > 0 ? ":PK" : string.Empty)}");
                ConsoleBridge.WriteLine($"[metadata] columns={string.Join(",", columns)}");

                // 让随后的 --report-shell 看到「表信息」页（列名 / 类型 / 可空 / 主键标记都在控件树里）；
                // DDL 正文已由上面的 ddl-len / ddl-exact 断言，不必再切一次。
                _ddlPane = false;
                RenderInspector();
                ConsoleBridge.WriteLine("[metadata] inspector-pane=表信息");
            }

            return detail is { Columns.Count: > 0 };
        }
        catch (OperationCanceledException)
        {
            ConsoleBridge.WriteLine("[metadata] cancelled");
            return false;
        }
        catch (Exception error)
        {
            ConsoleBridge.WriteLine($"[metadata] failed {error.GetType().Name}: {error.Message}");
            return false;
        }
    }

    private async Task<bool> WaitForConnectionNodeAsync(string identity, CancellationToken cancellationToken)
    {
        var deadline = DateTimeOffset.UtcNow.AddSeconds(5);
        while (DateTimeOffset.UtcNow < deadline)
        {
            if (_nodes.ContainsKey(identity))
            {
                return true;
            }

            await Task.Delay(100, cancellationToken).ConfigureAwait(true);
        }

        return _nodes.ContainsKey(identity);
    }

    // ------------------------------------------------------------------ 主题 token / 字体

    /// <summary>从 <see cref="Application.Resources"/> 取一个主题 brush（<c>ThemeResource</c> 的代码等价物）。</summary>
    /// <remarks>
    /// 取不到就返回 <see langword="null"/>，调用方保持控件默认色——主题资源缺失不该让界面崩。
    /// </remarks>
    private static Brush? ThemeBrush(string key)
    {
        try
        {
            return Application.Current is { } application && application.Resources is { } resources
                ? resources[key] as Brush
                : null;
        }
        catch (Exception)
        {
            return null;
        }
    }

    /// <summary>DDL 的等宽字体（代码正文按字符宽度读，不按比例字体）。</summary>
    private static FontFamily MonoFont() => _monoFont ??= new FontFamily("Consolas");

    private static FontFamily? _monoFont;

    /// <summary>树里的连接节点内容：文本即 <see cref="ToString"/>，徽标随连接状态变。</summary>
    private sealed record ConnectionNode(ConnectionProfile Profile, string Badge)
    {
        public override string ToString() => $"{Profile.Name} · {Badge}";
    }

    // ------------------------------------------------------------------ 树行的内容模型

    /// <summary>树行的种类（决定点击语义与渲染分支）。</summary>
    private enum TreeRowKind
    {
        /// <summary>库 / catalog（展开 = 显示四个分组行，0 次 RPC）。</summary>
        Catalog = 0,

        /// <summary>`Tables` / `Views` / `Functions` / `Queries` 分组。</summary>
        Group = 1,

        /// <summary>表 / 视图（叶子；表下的四组子层不在本片范围）。</summary>
        Object = 2,

        /// <summary>加载中占位行。</summary>
        Loading = 3,

        /// <summary>说明行（空分组 / 未实现 / 没有可见库）。</summary>
        Note = 4,

        /// <summary>失败行（点一下重试）。</summary>
        Error = 5,

        /// <summary>还有下一页时的续拉行。</summary>
        LoadMore = 6,
    }

    /// <summary>
    /// 树行内容：只有一个 <see cref="ToString"/> 文本 + 行种类 + 归属数据。
    /// </summary>
    /// <remarks>
    /// 刻意**不用** <c>DataTemplate</c>：对象树的行模板是 #170 崩溃排查的重点隔离项
    /// （<c>SQARVO_SAFE_TREE</c> 探针），这里保持「Content 对象 + 默认模板」这条稳定路径。
    /// </remarks>
    private sealed class TreeRow
    {
        public TreeRow(TreeRowKind kind, string text, object? payload = null)
        {
            Kind = kind;
            Text = text;
            Payload = payload;
        }

        public TreeRowKind Kind { get; }

        public string Text { get; }

        public object? Payload { get; }

        public override string ToString() => Text;
    }

    /// <summary>库行的数据：连接键 + 服务端给的目录引用（原样回传，不自己拼）。</summary>
    private sealed record CatalogPayload(string ConnectionKey, ObjectRef Reference);

    /// <summary>分组行的数据：连接键 + 目录引用 + 分组。</summary>
    private sealed record GroupPayload(string ConnectionKey, ObjectRef Catalog, ObjectGroup Group);

    /// <summary>对象行的数据：连接键 + 目录引用 + 对象条目。</summary>
    private sealed record ObjectPayload(string ConnectionKey, ObjectRef Catalog, ObjectEntry Entry);

    /// <summary>一层（连接层或分组层）的分页与加载状态。</summary>
    private sealed class NodeLoad
    {
        /// <summary>这一层是否已经成功读过第一页（展开不再重发）。</summary>
        public bool Loaded { get; set; }

        /// <summary>是否有请求在飞。</summary>
        public bool Loading { get; set; }

        /// <summary>被取消过（收起 / 切页）：再展开要重试。</summary>
        public bool Canceled { get; set; }

        /// <summary>下一页 token（空 = 没有下一页）。</summary>
        public string NextToken { get; set; } = string.Empty;

        /// <summary>已读到的行。</summary>
        public List<ObjectEntry> Items { get; } = new();

        /// <summary>这一层最近一次失败的标题 / 码（空 = 没有失败）。</summary>
        public string? ErrorHeadline { get; set; }

        /// <summary>这一层最近一次失败的稳定错误码。</summary>
        public string? ErrorCode { get; set; }

        /// <summary>在途请求的取消源。</summary>
        public CancellationTokenSource? Cts { get; set; }

        /// <summary>在途请求本身（重复展开时直接 await 同一个任务）。</summary>
        public Task Task { get; set; } = Task.CompletedTask;
    }
}
