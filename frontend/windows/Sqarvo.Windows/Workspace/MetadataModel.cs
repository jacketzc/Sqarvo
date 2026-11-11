using Sqarvo.Services.V1;

namespace Sqarvo.Windows.Workspace;

/// <summary>
/// catalog 下的客户端虚拟分组（`workspace-model.md` §4）：服务端层级里没有 `Tables` / `Views`
/// 这一层，分组靠 <c>ListNodesRequest.filter.kinds</c> 逐组拉取，每组有自己的分页 token。
/// </summary>
internal enum ObjectGroup
{
    /// <summary>表（`filter.kinds = [TABLE]`）。</summary>
    Tables = 0,

    /// <summary>视图（`filter.kinds = [VIEW, MATERIALIZED_VIEW]`）。</summary>
    Views = 1,

    /// <summary>函数 / 过程：契约里没有该层的列表拉取，只有入口。</summary>
    Functions = 2,

    /// <summary>已保存的查询：本版没有这一层，只有入口。</summary>
    Queries = 3,
}

/// <summary>
/// 文案与常量：分组名、种类名、空组说明、每页条数。
/// </summary>
/// <remarks>
/// 分组顺序固定 <c>Tables → Views → Functions → Queries</c>，名字是英文（与 Linux 客户端逐字一致，
/// 也是 <c>--report-shell</c> 断言里的节点名）。种类文案见 Linux <c>backend/metadata.rs</c> 的
/// <c>kind_label</c>：同一份词表同时给树与信息面板用，不会出现「图标是表、文字是视图」。
/// </remarks>
internal static class MetadataLabels
{
    /// <summary>每次 <c>ListNodes</c> 请求的条数。档位无关（不是布局度量，不随密度档变）。</summary>
    public const int PageSize = 200;

    /// <summary>分组的固定顺序。</summary>
    public static readonly ObjectGroup[] Groups =
    {
        ObjectGroup.Tables,
        ObjectGroup.Views,
        ObjectGroup.Functions,
        ObjectGroup.Queries,
    };

    /// <summary>分组行文案。</summary>
    public static string GroupLabel(ObjectGroup group) => group switch
    {
        ObjectGroup.Tables => "Tables",
        ObjectGroup.Views => "Views",
        ObjectGroup.Functions => "Functions",
        _ => "Queries",
    };

    /// <summary>该组拉取时用的 `filter.kinds`；<see langword="null"/> = 后端没有这个能力（不发请求）。</summary>
    public static IReadOnlyList<ObjectKind>? KindsOf(ObjectGroup group) => group switch
    {
        ObjectGroup.Tables => new[] { ObjectKind.Table },
        ObjectGroup.Views => new[] { ObjectKind.View, ObjectKind.MaterializedView },
        _ => null,
    };

    /// <summary>空的说明（「确实没有」而不是读取失败）。</summary>
    public static string EmptyNote(ObjectGroup group) => group switch
    {
        ObjectGroup.Tables => "（这一组没有表）",
        ObjectGroup.Views => "（这一组没有视图）",
        ObjectGroup.Functions => "（这一组没有函数）",
        _ => "（还没有保存的查询）",
    };

    /// <summary>后端没有该能力时的说明行；有列表能力的组返回 <see langword="null"/>。</summary>
    public static string? UnsupportedNote(ObjectGroup group) => group switch
    {
        ObjectGroup.Functions => "函数列表尚未实现（点开看说明，不发请求）",
        ObjectGroup.Queries => "已保存的查询尚未实现（点开看说明）",
        _ => null,
    };

    /// <summary>对象种类文案。</summary>
    public static string KindLabel(ObjectKind kind) => kind switch
    {
        ObjectKind.Catalog => "库",
        ObjectKind.Schema => "schema",
        ObjectKind.Table => "表",
        ObjectKind.View => "视图",
        ObjectKind.MaterializedView => "物化视图",
        ObjectKind.Column => "列",
        ObjectKind.Index => "索引",
        ObjectKind.Sequence => "序列",
        ObjectKind.Function => "函数",
        ObjectKind.Procedure => "存储过程",
        ObjectKind.Trigger => "触发器",
        ObjectKind.Type => "类型",
        ObjectKind.Other => "对象",
        _ => "未知",
    };

    /// <summary>对象种类所属的分组；不属于可列分组时返回 <see langword="null"/>。</summary>
    public static ObjectGroup? GroupOf(ObjectKind kind) => kind switch
    {
        ObjectKind.Table => ObjectGroup.Tables,
        ObjectKind.View or ObjectKind.MaterializedView => ObjectGroup.Views,
        _ => null,
    };

    /// <summary>可空性文案（「可空」/「不可空」两态，不合并）。</summary>
    public static string NullabilityLabel(bool nullable) => nullable ? "可空" : "不可空";

    /// <summary>主键标记：复合主键带上主键内顺序（协议里 <c>position</c> 从 1 起）。</summary>
    public static string PrimaryKeyMark(uint position) => position switch
    {
        0 => string.Empty,
        1 => "PK",
        _ => $"PK {position}",
    };
}

/// <summary>树 / 页面里的一个对象（来自 `ListNodes` 的 <c>ObjectNode</c>）。</summary>
internal sealed record ObjectEntry(ObjectRef Reference, string Name, ObjectKind Kind)
{
    public string KindLabel => MetadataLabels.KindLabel(Kind);
}

/// <summary>选中了什么（树与 `Objects` 页共用的唯一一份选中状态）。</summary>
internal enum SelectionKind
{
    /// <summary>没有选中 / 选中了占位行。</summary>
    None = 0,

    /// <summary>连接。</summary>
    Connection = 1,

    /// <summary>库 / catalog。</summary>
    Catalog = 2,

    /// <summary>`Tables` / `Views` / `Functions` / `Queries` 分组。</summary>
    Group = 3,

    /// <summary>表 / 视图。</summary>
    Object = 4,
}

/// <summary>
/// 一份不可变的选中状态：树选中、`Objects` 页选中与信息面板内容**全部**从这里派生。
/// </summary>
/// <remarks>
/// 「当前库 / 当前表」只有这一个来源：`Objects` 页的作用域由 <see cref="ObjectsScope.For"/> 现算，
/// 信息面板由 <see cref="DetailKey"/> 现算，任何地方都不另存一份「当前库」布尔量或字符串
/// （否则树、页面、面板会各说一套）。
/// </remarks>
internal sealed record MetadataSelection(
    SelectionKind Kind,
    string ConnectionKey = "",
    string ConnectionName = "",
    ObjectRef? Catalog = null,
    ObjectGroup Group = ObjectGroup.Tables,
    ObjectEntry? Object = null)
{
    /// <summary>空选中。</summary>
    public static MetadataSelection None { get; } = new(SelectionKind.None);

    /// <summary>选中一个连接。</summary>
    public static MetadataSelection ForConnection(string key, string name) =>
        new(SelectionKind.Connection, key, name);

    /// <summary>选中一个库。</summary>
    public static MetadataSelection ForCatalog(string key, string name, ObjectRef catalog) =>
        new(SelectionKind.Catalog, key, name, catalog);

    /// <summary>选中一个分组。</summary>
    public static MetadataSelection ForGroup(string key, string name, ObjectRef catalog, ObjectGroup group) =>
        new(SelectionKind.Group, key, name, catalog, group);

    /// <summary>选中一个表 / 视图。</summary>
    public static MetadataSelection ForObject(string key, string name, ObjectRef catalog, ObjectEntry entry) =>
        new(SelectionKind.Object, key, name, catalog, MetadataLabels.GroupOf(entry.Kind) ?? ObjectGroup.Tables, entry);

    /// <summary>库名（未选中库时为空）。</summary>
    public string CatalogName => Catalog?.Catalog ?? string.Empty;

    /// <summary>是否选到了可列 / 可看信息的作用域（库、分组、表 / 视图）。</summary>
    public bool HasScope => Kind is SelectionKind.Catalog or SelectionKind.Group or SelectionKind.Object;

    /// <summary>信息面板的「看的是谁」键：同键不重发 `GetColumns` / `GetDdl`。</summary>
    public string DetailKey => Kind switch
    {
        SelectionKind.Object => $"object|{RefKey(Object?.Reference)}",
        SelectionKind.Catalog or SelectionKind.Group => $"catalog|{RefKey(Catalog)}",
        SelectionKind.Connection => $"connection|{ConnectionKey}",
        _ => "none",
    };

    /// <summary>对象引用的稳定键（用来比较两份引用是否指向同一个对象）。</summary>
    internal static string RefKey(ObjectRef? reference) => reference is null
        ? "-"
        : $"{reference.SessionId}|{reference.Catalog}|{reference.Schema}|{reference.Name}|{(int)reference.Kind}";
}

/// <summary>`Objects` 页的一次作用域：父引用 + 种类过滤 + 展示用的分组名。</summary>
internal sealed record ObjectsScope(
    string Signature,
    ObjectRef Parent,
    IReadOnlyList<ObjectKind> Kinds,
    ObjectGroup Group,
    string CatalogName)
{
    /// <summary>
    /// 把选中映射成页面作用域；没有可列的分组（连接、占位行、`Functions` / `Queries`）时返回
    /// <see langword="null"/>，页面显示空态或「尚未实现」说明。
    /// </summary>
    public static ObjectsScope? For(MetadataSelection selection)
    {
        if (selection.Catalog is not { } catalog)
        {
            return null;
        }

        var group = selection.Kind switch
        {
            SelectionKind.Catalog => ObjectGroup.Tables,
            SelectionKind.Group => selection.Group,
            SelectionKind.Object => MetadataLabels.GroupOf(selection.Object?.Kind ?? ObjectKind.Unspecified),
            _ => null,
        };

        if (group is not { } kindGroup || MetadataLabels.KindsOf(kindGroup) is not { } kinds)
        {
            return null;
        }

        // 作用域键只看「库 + 分组」：从库、分组或表选进来都是同一次浏览，不重发 ListNodes。
        return new ObjectsScope(
            $"{MetadataSelection.RefKey(catalog)}|{kindGroup}",
            catalog,
            kinds,
            kindGroup,
            catalog.Catalog);
    }
}

/// <summary>`Objects` 页的状态（一页作用域一份，换作用域整体替换）。</summary>
internal sealed class ObjectsPageState
{
    /// <summary>作用域签名（与 <see cref="MetadataSelection.Signature"/> 同源）。</summary>
    public string Signature { get; init; } = string.Empty;

    /// <summary>作用域分组。</summary>
    public ObjectGroup Group { get; init; }

    /// <summary>库名（只用于文案）。</summary>
    public string CatalogName { get; init; } = string.Empty;

    /// <summary>已加载的行（模型顺序 = 服务端顺序，客户端不重排）。</summary>
    public List<ObjectEntry> Items { get; } = new();

    /// <summary>下一页的 token；空 = 没有下一页（服务端如实返回）。</summary>
    public string NextToken { get; set; } = string.Empty;

    /// <summary>首页是否在飞。</summary>
    public bool Loading { get; set; }

    /// <summary>下一页是否在飞。</summary>
    public bool LoadingMore { get; set; }

    /// <summary>失败主标题（空 = 没有失败）。</summary>
    public string? Headline { get; set; }

    /// <summary>稳定错误码。</summary>
    public string? Code { get; set; }

    /// <summary>还有下一页。</summary>
    public bool HasMore => NextToken.Length > 0;
}

/// <summary>一列（`GetColumns` + `GetPrimaryKey` 合并后的展示模型）。</summary>
internal sealed record ColumnEntry(string Name, string TypeLabel, bool Nullable, uint PrimaryKeyPosition)
{
    /// <summary>可空性文案。</summary>
    public string NullabilityLabel => MetadataLabels.NullabilityLabel(Nullable);

    /// <summary>主键标记（非主键为空串：面板不摆占位）。</summary>
    public string PrimaryKeyLabel => MetadataLabels.PrimaryKeyMark(PrimaryKeyPosition);
}

/// <summary>`GetDdl` 的结果（原文 + 是否原文 + 来源说明）。</summary>
internal sealed record DdlResult(string Text, bool Exact, string SourceDetail);

/// <summary>信息面板里「表 / 视图」这一态的加载状态与内容。</summary>
internal sealed class ObjectDetail
{
    /// <summary>列是否在飞。</summary>
    public bool ColumnsLoading { get; set; }

    /// <summary>列（按 ordinal 顺序）。</summary>
    public List<ColumnEntry> Columns { get; } = new();

    /// <summary>列读取失败的主标题。</summary>
    public string? ColumnsHeadline { get; set; }

    /// <summary>列读取失败的稳定错误码。</summary>
    public string? ColumnsCode { get; set; }

    /// <summary>主键读取失败的主标题（读不到主键时不画任何主键标记，也不猜）。</summary>
    public string? PrimaryKeyHeadline { get; set; }

    /// <summary>主键读取失败的稳定错误码。</summary>
    public string? PrimaryKeyCode { get; set; }

    /// <summary>DDL 是否在飞。</summary>
    public bool DdlLoading { get; set; }

    /// <summary>DDL 结果；未成功时为 <see langword="null"/>。</summary>
    public DdlResult? Ddl { get; set; }

    /// <summary>DDL 读取失败的主标题（失败时面板显示错误，不是空面板）。</summary>
    public string? DdlHeadline { get; set; }

    /// <summary>DDL 读取失败的稳定错误码。</summary>
    public string? DdlCode { get; set; }
}