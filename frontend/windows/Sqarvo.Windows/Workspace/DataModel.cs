using Sqarvo.Common.V1;
using Sqarvo.Services.V1;
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;

namespace Sqarvo.Windows.Workspace;

// 表数据面的**纯逻辑**（#173）：一页数据、行身份、本地暂存、提交计划、只读 / 冲突判定、状态文案。
//
// 这里刻意没有任何 WinUI 类型：本仓库的主要开发与 CI 在 Linux，WinUI 的 XAML 无法在本机编译，
// 所以凡是「能断言的规则」都放在这个文件里，用 `~/…/win-data-probe` 在本机直接跑（ALL PASS）。
// 渲染（Grid / TextBlock / TextBox）只在 `Views/ShellView.Data.cs`，它调用这里的函数取结论，
// 不自己算「这行算不算脏」「为什么不能编辑」。

/// <summary>表数据面里与档位无关的硬约束（数字来自协议与契约，不是布局）。</summary>
internal static class DataLimits
{
    /// <summary>每页行数（`BrowseTable.page_size`）。</summary>
    public const int PageSize = 200;

    /// <summary>服务端风险阈值（`DEFAULT_RISK_CONFIRMATION_ROWS`）：超过就必须带 `acknowledge_risk`。</summary>
    public const int RiskConfirmationRows = 100;
}

/// <summary>一列（`GetColumns` / `BrowseTable.columns` + `GetPrimaryKey` 合并后的展示模型）。</summary>
internal sealed record DataColumn(
    string Name,
    string TypeLabel,
    ColumnTypeKind Kind,
    bool Nullable,
    uint PrimaryKeyPosition)
{
    /// <summary>是不是主键的一部分（主键列不可编辑：它负责定位行）。</summary>
    public bool PrimaryKey => PrimaryKeyPosition > 0;

    /// <summary>数值类列（右对齐；不影响值本身）。</summary>
    public bool Numeric => Kind is ColumnTypeKind.ColumnTypeInteger
        or ColumnTypeKind.ColumnTypeFloat
        or ColumnTypeKind.ColumnTypeDecimal;

    /// <summary>表头文字：`列名 · 原始类型`，主键列再标 `PK` / `PK 2`。</summary>
    public string HeaderText
    {
        get
        {
            var label = TypeLabel.Length == 0 ? Name : $"{Name} · {TypeLabel}";
            return PrimaryKeyPosition switch
            {
                0 => label,
                1 => $"{label} · PK",
                _ => $"{label} · PK {PrimaryKeyPosition}",
            };
        }
    }

    /// <summary>由协议列元数据 + 主键内顺序建一列。</summary>
    /// <param name="column">`BrowseTable.columns` 里的一项。</param>
    /// <param name="primaryKeyPosition">主键内顺序（0 = 不是主键）。</param>
    /// <returns>展示用列。</returns>
    public static DataColumn From(ColumnMeta column, uint primaryKeyPosition)
    {
        ArgumentNullException.ThrowIfNull(column);
        var type = column.Type;
        return new DataColumn(
            column.Name,
            type?.OriginalTypeName ?? string.Empty,
            type?.Kind ?? ColumnTypeKind.ColumnTypeUnspecified,
            column.Nullable,
            primaryKeyPosition);
    }
}

/// <summary>单元格文字的**展示口径**（NULL 与空串必须可区分）。</summary>
/// <remarks>
/// 与查询结果的差别是有意的：查询结果把空文本渲染成 <c>''</c>（证明「有值但为空」），
/// 表数据网格里空串**保留空白**（用户要给空串就留空），NULL 用弱色斜体的 <c>NULL</c> 标出。
/// 其余类型沿用 <see cref="QueryCellText"/> 的同一套格式化，不另立一份（同程序集、同 Workspace 命名空间；
/// 纯逻辑探针 `win-data-probe` 把 `QueryModel.cs` 一起链进来，因此这条复用在本机也是真的编译过的）。
/// </remarks>
internal static class DataCellText
{
    /// <summary>NULL 的显示文字（弱色 + 斜体由渲染层加，模型只给文字）。</summary>
    public const string Null = "NULL";

    /// <summary>一格的显示文字。</summary>
    /// <param name="value">服务端值（可为 <see langword="null"/>：行缺列）。</param>
    /// <returns>NULL → <c>NULL</c>；空文本 → 空串；其余同查询结果网格。</returns>
    public static string Display(ColumnValue? value)
    {
        if (value is null || value.Kind == ValueKind.Null)
        {
            return Null;
        }

        return value.ValueCase == ColumnValue.ValueOneofCase.TextValue
            ? value.TextValue
            : QueryCellText.Format(value);
    }

    /// <summary>这一格是不是 NULL。</summary>
    /// <param name="value">服务端值。</param>
    /// <returns>NULL（或缺失）时为 <see langword="true"/>。</returns>
    public static bool IsNull(ColumnValue? value) => value is null || value.Kind == ValueKind.Null;

    /// <summary>这一格是不是空文本（与 NULL 相反的边界：它**是**一个值）。</summary>
    /// <param name="value">服务端值。</param>
    /// <returns>空文本时为 <see langword="true"/>。</returns>
    public static bool IsEmptyText(ColumnValue? value) =>
        value is not null
        && value.ValueCase == ColumnValue.ValueOneofCase.TextValue
        && value.TextValue.Length == 0;
}

/// <summary>输入框文字 → 强类型值（编辑单元格时用）。</summary>
/// <remarks>
/// 协议只接受强类型值，绝不做字符串拼接：这里按列类型解析，解析失败给可读原因而不是发一个坏请求。
/// 约定：输入 <c>NULL</c>（大小写不敏感）表示写 SQL NULL；空文本在文本列表示**空字符串**（不是 NULL）。
/// </remarks>
internal static class DataInput
{
    /// <summary>本版支持就地编辑的类型（其余类型值得用 SQL 改，不假装支持）。</summary>
    /// <param name="kind">列的逻辑类型。</param>
    /// <returns>可编辑时为 <see langword="true"/>。</returns>
    public static bool SupportsInlineEdit(ColumnTypeKind kind) => kind is
        ColumnTypeKind.ColumnTypeBoolean
        or ColumnTypeKind.ColumnTypeInteger
        or ColumnTypeKind.ColumnTypeFloat
        or ColumnTypeKind.ColumnTypeDecimal
        or ColumnTypeKind.ColumnTypeText
        or ColumnTypeKind.ColumnTypeJson
        or ColumnTypeKind.ColumnTypeUuid;

    /// <summary>解析一格的输入文字。</summary>
    /// <param name="column">目标列。</param>
    /// <param name="text">输入框里的文字。</param>
    /// <param name="value">解析成功时的强类型值。</param>
    /// <param name="error">失败原因（可直接展示）。</param>
    /// <returns>成功时为 <see langword="true"/>。</returns>
    public static bool TryParse(DataColumn column, string text, out ColumnValue value, out string? error)
    {
        ArgumentNullException.ThrowIfNull(column);
        ArgumentNullException.ThrowIfNull(text);

        value = new ColumnValue { OriginalTypeName = column.TypeLabel };
        error = null;

        if (string.Equals(text.Trim(), DataCellText.Null, StringComparison.OrdinalIgnoreCase))
        {
            if (!column.Nullable)
            {
                error = $"该列不允许 NULL（NOT NULL）：请输入具体值（要写空文本就留空并直接回车）";
                return false;
            }

            value.Kind = ValueKind.Null;
            return true;
        }

        switch (column.Kind)
        {
            case ColumnTypeKind.ColumnTypeBoolean:
                if (bool.TryParse(text.Trim(), out var flag))
                {
                    value.Kind = ValueKind.Bool;
                    value.BoolValue = flag;
                    return true;
                }

                if (text.Trim() is "1" or "0")
                {
                    value.Kind = ValueKind.Bool;
                    value.BoolValue = text.Trim() == "1";
                    return true;
                }

                error = "布尔列只接受 true / false / 1 / 0";
                return false;

            case ColumnTypeKind.ColumnTypeInteger:
                if (long.TryParse(text.Trim(), NumberStyles.Integer, CultureInfo.InvariantCulture, out var integer))
                {
                    value.Kind = ValueKind.Int64;
                    value.Int64Value = integer;
                    return true;
                }

                error = "整数列只接受十进制整数（不带小数与千分位）";
                return false;

            case ColumnTypeKind.ColumnTypeFloat:
                if (double.TryParse(text.Trim(), NumberStyles.Float, CultureInfo.InvariantCulture, out var real))
                {
                    value.Kind = ValueKind.Float64;
                    value.Float64Value = real;
                    return true;
                }

                error = "浮点列只接受数字（小数点用 .，不要用千分位）";
                return false;

            case ColumnTypeKind.ColumnTypeDecimal:
                return TryParseDecimal(text.Trim(), value, out error);

            case ColumnTypeKind.ColumnTypeText:
                value.Kind = ValueKind.Text;
                value.TextValue = text;
                return true;

            case ColumnTypeKind.ColumnTypeJson:
                value.Kind = ValueKind.Json;
                value.JsonValue = text;
                return true;

            case ColumnTypeKind.ColumnTypeUuid:
                value.Kind = ValueKind.Uuid;
                value.UuidValue = text.Trim();
                return true;

            default:
                error = $"本版不支持就地编辑 {column.TypeLabel} 类型：请用 SQL 修改";
                return false;
        }
    }

    private static bool TryParseDecimal(string text, ColumnValue value, out string? error)
    {
        error = null;
        var negative = text.StartsWith('-');
        var body = negative ? text[1..] : text;
        var dot = body.IndexOf('.');
        var digits = dot < 0 ? body : body.Remove(dot, 1);
        var scale = dot < 0 ? 0 : body.Length - dot - 1;

        if (digits.Length == 0 || !digits.All(char.IsAsciiDigit))
        {
            error = "小数列只接受数字（例如 12.50）";
            return false;
        }

        value.Kind = ValueKind.Decimal;
        value.DecimalValue = new DecimalValue
        {
            UnscaledValue = (negative ? "-" : string.Empty) + digits,
            Scale = scale,
        };
        return true;
    }
}

/// <summary>一行：服务端原值 + 本地暂存值 + 定位这一行的主键值。</summary>
internal sealed class DataRow
{
    private readonly ColumnValue[] _values;

    /// <summary>建一行。</summary>
    /// <param name="ordinal">页内下标（0 起）。</param>
    /// <param name="number">展示行号（1 起，= offset + 下标 + 1）。</param>
    /// <param name="values">服务端值，顺序与列一致。</param>
    /// <param name="key">主键值（空 = 这一行无法定位，只读）。</param>
    public DataRow(int ordinal, long number, IReadOnlyList<ColumnValue> values, IReadOnlyList<PrimaryKeyValue> key)
    {
        ArgumentNullException.ThrowIfNull(values);
        ArgumentNullException.ThrowIfNull(key);

        Ordinal = ordinal;
        Number = number;
        _values = new ColumnValue[values.Count];
        for (var index = 0; index < values.Count; index++)
        {
            _values[index] = values[index];
        }

        Key = key.ToArray();
    }

    /// <summary>页内下标（0 起）。</summary>
    public int Ordinal { get; }

    /// <summary>展示行号（= offset + 下标 + 1，不是主键）。</summary>
    public long Number { get; }

    /// <summary>定位这一行的主键值（提交时原样回传；空 = 不可定位）。</summary>
    public IReadOnlyList<PrimaryKeyValue> Key { get; }

    /// <summary>本地暂存：列序 → 新值（只放真正改过的列）。</summary>
    public Dictionary<int, ColumnValue> Edits { get; } = new();

    /// <summary>删除态（行号写「✕N」、整行删除线 + 压暗）：用户标记删除或服务端报告行已不存在。</summary>
    public bool Removed { get; set; }

    /// <summary>用户标记删除（右键 / 状态栏「−」）：提交时按主键生成 DELETE，Ctrl+Z 可复位。</summary>
    public bool MarkedForDeletion { get; set; }

    /// <summary>
    /// 删除已经在服务端（当前事务或自动提交批次）执行成功。置位的行退出提交集合 —— 否则下一次
    /// Ctrl+S 会把同一条 DELETE 再发一遍，服务端回 ROW_NOT_FOUND，行被误标成「已被其它会话删除」。
    /// 提交成功由 <see cref="DataPage.RemoveExecutedDeletes"/> 整行移出；事务回滚 / Ctrl+Z 复位。
    /// </summary>
    public bool DeleteExecuted { get; set; }

    /// <summary>有未提交的更改。</summary>
    public bool Dirty => Edits.Count > 0;

    /// <summary>这一行能被主键定位（否则整页只读）。</summary>
    public bool Locatable => Key.Count > 0;

    /// <summary>服务端原值（提交成功后会被当前值替换）。</summary>
    /// <param name="column">列序。</param>
    /// <returns>原值；缺列时 <see langword="null"/>。</returns>
    public ColumnValue? Original(int column) => column >= 0 && column < _values.Length ? _values[column] : null;

    /// <summary>当前显示值（有暂存就用暂存）。</summary>
    /// <param name="column">列序。</param>
    /// <returns>当前值；缺列时 <see langword="null"/>。</returns>
    public ColumnValue? Value(int column) =>
        Edits.TryGetValue(column, out var edited) ? edited : Original(column);

    /// <summary>暂存一格。新值与原值语义相等时把这条编辑**删掉**（撤销回原值不算脏）。</summary>
    /// <param name="column">列序。</param>
    /// <param name="value">新值。</param>
    public void Stage(int column, ColumnValue value)
    {
        ArgumentNullException.ThrowIfNull(value);

        if (DataValues.Same(Original(column), value))
        {
            Edits.Remove(column);
            return;
        }

        Edits[column] = value;
    }

    /// <summary>丢弃这一行的暂存。</summary>
    public void DiscardEdits() => Edits.Clear();

    /// <summary>提交成功：原值换成当前值、清空暂存（之后这一行不再算脏）。</summary>
    public void AcceptEdits()
    {
        foreach (var (column, value) in Edits)
        {
            if (column >= 0 && column < _values.Length)
            {
                _values[column] = value;
            }
        }

        Edits.Clear();
    }
}

/// <summary>值的语义相等（忽略传输细节 <c>original_type_name</c> / <c>truncated</c>）。</summary>
internal static class DataValues
{
    /// <summary>两个值是否语义相等。</summary>
    /// <param name="left">左值。</param>
    /// <param name="right">右值。</param>
    /// <returns>kind 与 oneof 内容都相同时为 <see langword="true"/>。</returns>
    public static bool Same(ColumnValue? left, ColumnValue? right)
    {
        if (left is null || right is null)
        {
            return left is null && right is null;
        }

        if (left.Kind != right.Kind || left.ValueCase != right.ValueCase)
        {
            return false;
        }

        return left.ValueCase switch
        {
            ColumnValue.ValueOneofCase.None => true,
            ColumnValue.ValueOneofCase.BoolValue => left.BoolValue == right.BoolValue,
            ColumnValue.ValueOneofCase.Int64Value => left.Int64Value == right.Int64Value,
            ColumnValue.ValueOneofCase.Uint64Value => left.Uint64Value == right.Uint64Value,
            ColumnValue.ValueOneofCase.Float64Value => left.Float64Value.Equals(right.Float64Value),
            ColumnValue.ValueOneofCase.DecimalValue =>
                left.DecimalValue?.UnscaledValue == right.DecimalValue?.UnscaledValue
                && left.DecimalValue?.Scale == right.DecimalValue?.Scale,
            ColumnValue.ValueOneofCase.TextValue => left.TextValue == right.TextValue,
            ColumnValue.ValueOneofCase.BytesValue => left.BytesValue.Equals(right.BytesValue),
            ColumnValue.ValueOneofCase.JsonValue => left.JsonValue == right.JsonValue,
            ColumnValue.ValueOneofCase.UuidValue => left.UuidValue == right.UuidValue,
            ColumnValue.ValueOneofCase.UnsupportedValue => left.UnsupportedValue == right.UnsupportedValue,
            // 日期 / 时间 / 时间戳 / 数组：按渲染后的文字比较即可（它们没有第三种表示）。
            _ => QueryCellText.Format(left) == QueryCellText.Format(right),
        };
    }
}

/// <summary>一页表数据的状态（一页一份；换 offset 整体替换，续拉就地追加）。</summary>
internal sealed class DataPage
{
    /// <summary>建一页。</summary>
    /// <param name="connectionKey">连接身份键。</param>
    /// <param name="sessionId">数据库会话。</param>
    /// <param name="target">对象引用（原样回传给服务端）。</param>
    /// <param name="tableName">对象名（展示用）。</param>
    /// <param name="catalogName">库名（可为空：不编造库名）。</param>
    public DataPage(string connectionKey, string sessionId, ObjectRef target, string tableName, string catalogName)
    {
        ArgumentNullException.ThrowIfNull(target);
        ConnectionKey = connectionKey;
        SessionId = sessionId;
        Target = target;
        TableName = tableName;
        CatalogName = catalogName;
    }

    /// <summary>连接身份键（重连后要重新建页）。</summary>
    public string ConnectionKey { get; }

    /// <summary>数据库会话 id。</summary>
    public string SessionId { get; }

    /// <summary>对象引用。</summary>
    public ObjectRef Target { get; }

    /// <summary>对象名。</summary>
    public string TableName { get; }

    /// <summary>库名（空 = 连接没有默认库，界面写「默认」）。</summary>
    public string CatalogName { get; }

    /// <summary>列（首页读到时建一次；续拉的列必须与它一致）。</summary>
    public List<DataColumn> Columns { get; } = new();

    /// <summary>已取到的行（首页 200 行；「加载更多」在尾部追加）。</summary>
    public List<DataRow> Rows { get; } = new();

    /// <summary>本页第一行的 offset（「加载更多」不改它；翻页才换）。</summary>
    public long Offset { get; set; }

    /// <summary>每页行数。</summary>
    public int PageSize { get; set; } = DataLimits.PageSize;

    /// <summary>服务端还有更多数据。</summary>
    public bool HasMore { get; set; }

    /// <summary>服务端声明本页只读。</summary>
    public bool ReadOnly { get; set; }

    /// <summary>只读原因（可编辑时为 Unspecified）。</summary>
    public ReadOnlyReason ReadOnlyReason { get; set; }

    /// <summary>排序保证（决定「翻页是否稳定」的文案）。</summary>
    public RowOrder Ordering { get; set; }

    /// <summary>总数三态。</summary>
    public TotalKind TotalKind { get; set; }

    /// <summary>总数（仅在 <see cref="TotalKind"/> 不是 Unknown 时有意义）。</summary>
    public ulong TotalRows { get; set; }

    /// <summary>请求了总数但没统计完（超时 / 被中断）。</summary>
    public bool TotalSkipped { get; set; }

    /// <summary>读取在飞。</summary>
    public bool Loading { get; set; }

    /// <summary>失败主标题（空 = 没有失败）。</summary>
    public string? Headline { get; set; }

    /// <summary>稳定错误码。</summary>
    public string? Code { get; set; }

    /// <summary>补充说明（只读降级原因 / 事务降级原因等），不覆盖五态。</summary>
    public string? Note { get; set; }

    /// <summary>页码（1 起；offset 分页，所以页码是算出来的，不是服务端给的）。</summary>
    public int PageNumber => (int)(Offset / Math.Max(1, PageSize)) + 1;

    /// <summary>总页数（**只在精确总数时**给得出来；否则 <see langword="null"/>，界面写 `?`）。</summary>
    /// <remarks>精确的 0 行也算一页（空表），否则页码会写成「第 1 / 0 页」。</remarks>
    public int? TotalPagesExact => TotalKind == TotalKind.Exact && PageSize > 0
        ? Math.Max(1, (int)((TotalRows + (ulong)PageSize - 1) / (ulong)PageSize))
        : null;

    /// <summary>下一次「加载更多」的 offset。</summary>
    /// <remarks>
    /// 用「已从服务端取到的行数」而不是 <c>Rows.Count</c>：提交成功后已执行的删除行会被移出页面，
    /// 用 <c>Rows.Count</c> 会让 offset 回退，接着「加载更多」就把已经显示过的行再拉一遍。
    /// </remarks>
    public long NextOffset => Offset + LoadedRowCount;

    /// <summary>已经从服务端取到并追加进页面的行数（删除不移除计数，换页清零）。</summary>
    public int LoadedRowCount { get; private set; }

    /// <summary>有未提交更改的行数（编辑 + 标记删除）。</summary>
    public int DirtyRowCount => Rows.Count(row => row.Dirty || row.MarkedForDeletion);

    /// <summary>标记删除、待提交的行数。</summary>
    public int MarkedDeleteCount => Rows.Count(row => row.MarkedForDeletion);

    /// <summary>待提交的变更条数（= 各脏行的改动列数之和；删除行数另见 MarkedDeleteCount）。</summary>
    public int ChangeCount => Rows.Sum(row => row.Edits.Count);

    /// <summary>有没有未提交的更改（编辑或标记删除）。</summary>
    public bool HasChanges => ChangeCount > 0 || MarkedDeleteCount > 0;

    /// <summary>`库.表`（库名未知时只写表名）。</summary>
    public string TableLabel => CatalogName.Length == 0 ? TableName : $"{CatalogName}.{TableName}";

    /// <summary>把一页浏览结果写进状态（首页替换、续拉追加）。</summary>
    /// <param name="response">`BrowseTable` 响应。</param>
    /// <param name="primaryKey">`GetPrimaryKey` 的列（按主键顺序；空 = 无主键）。</param>
    /// <param name="append"><see langword="true"/> = 续拉（追加行、保留已有暂存）。</param>
    /// <exception cref="InvalidOperationException">续拉的列与服务端列不一致（不混两套列）。</exception>
    public void Apply(BrowseTableResponse response, IReadOnlyList<PrimaryKeyColumn> primaryKey, bool append)
    {
        ArgumentNullException.ThrowIfNull(response);
        ArgumentNullException.ThrowIfNull(primaryKey);

        var positions = new Dictionary<string, uint>(StringComparer.Ordinal);
        foreach (var column in primaryKey)
        {
            positions[column.Name] = column.Position;
        }

        if (!append)
        {
            Columns.Clear();
            Rows.Clear();
            LoadedRowCount = 0;
        }

        var incoming = new List<DataColumn>(response.Columns.Count);
        foreach (var column in response.Columns)
        {
            incoming.Add(DataColumn.From(column, positions.TryGetValue(column.Name, out var position) ? position : 0));
        }

        if (append && Columns.Count > 0)
        {
            // 续拉页的列必须与首页一致：列变了说明服务端换了一套列（不该发生），
            // 混进去会让「列序 → 值」错位，宁可失败也不显示错的数据。
            if (incoming.Count != Columns.Count
                || incoming.Where((column, index) => !string.Equals(column.Name, Columns[index].Name, StringComparison.Ordinal)).Any())
            {
                throw new InvalidOperationException("续拉页的列与首页不一致：已保留已取到的行，请刷新这一页");
            }
        }
        else if (Columns.Count == 0)
        {
            Columns.AddRange(incoming);
        }

        var keyIndexes = KeyIndexes(Columns);
        var pkCount = keyIndexes.Count;
        foreach (var row in response.Rows)
        {
            var values = row.Values;
            var key = new List<PrimaryKeyValue>(pkCount);
            if (pkCount > 0)
            {
                var complete = true;
                foreach (var index in keyIndexes)
                {
                    if (index < 0 || index >= values.Count)
                    {
                        complete = false;
                        break;
                    }

                    var column = Columns[index];
                    if (column.PrimaryKeyPosition == 0)
                    {
                        complete = false;
                        break;
                    }

                    key.Add(new PrimaryKeyValue { Column = column.Name, Value = values[index] });
                }

                if (!complete)
                {
                    key.Clear();
                }
            }

            var number = Offset + LoadedRowCount + 1;
            Rows.Add(new DataRow(Rows.Count, number, values, key));
            LoadedRowCount++;
        }

        HasMore = response.HasMore;
        ReadOnly = response.ReadOnly;
        ReadOnlyReason = response.ReadOnlyReason;
        Ordering = response.Ordering;
        TotalKind = response.TotalKind;
        TotalRows = response.TotalRows;
        TotalSkipped = response.TotalSkipped;

        // 服务端说可编辑、但本地拿不到主键（GetPrimaryKey 失败或没有主键列）：按只读处理。
        // 没有主键就没有行身份，「可编辑」只会在提交时变成 `table_data.no_primary_key`。
        if (!ReadOnly && keyIndexes.Count == 0)
        {
            ReadOnly = true;
            ReadOnlyReason = Sqarvo.Services.V1.ReadOnlyReason.NoPrimaryKey;
            Note = "读不到主键定义：本页按只读处理（没有主键就无法定位行）";
        }

        Loading = false;
        Headline = null;
        Code = null;
    }

    /// <summary>这一页的主键列下标（按主键内顺序）。</summary>
    /// <param name="columns">列。</param>
    /// <returns>列下标列表；无主键时为空。</returns>
    public static List<int> KeyIndexes(IReadOnlyList<DataColumn> columns)
    {
        ArgumentNullException.ThrowIfNull(columns);

        var keyed = new List<(uint Position, int Index)>();
        for (var index = 0; index < columns.Count; index++)
        {
            if (columns[index].PrimaryKeyPosition > 0)
            {
                keyed.Add((columns[index].PrimaryKeyPosition, index));
            }
        }

        return keyed.OrderBy(item => item.Position).Select(item => item.Index).ToList();
    }

    /// <summary>丢弃全部本地暂存与删除标记（Ctrl+Z：把 Removed 一起复位）。</summary>
    public void DiscardAllEdits()
    {
        foreach (var row in Rows)
        {
            row.DiscardEdits();
            // 自己标删 / 已执行删除的行回到原样；服务端报告「行已不存在」的 Removed 不动。
            if (row.MarkedForDeletion || row.DeleteExecuted)
            {
                row.MarkedForDeletion = false;
                row.DeleteExecuted = false;
                row.Removed = false;
            }
        }
    }

    /// <summary>提交成功后把已执行删除的行移出页面（被单批上限 deferred、还没执行的行留在页面上）。</summary>
    /// <returns>移除的行数。</returns>
    public int RemoveExecutedDeletes() => Rows.RemoveAll(row => row.DeleteExecuted);
}

/// <summary>一次提交要发给服务端的行变更（纯值，可直接断言）。</summary>
internal sealed record DataChangePlan(
    IReadOnlyList<RowUpdate> Updates,
    IReadOnlyList<RowDelete> Deletes,
    IReadOnlyList<DataRow> Rows,
    int ChangeCount,
    int DeleteCount,
    int UpdateRowCount)
{
    /// <summary>按当前页面状态生成变更列表（脏行 → `RowUpdate`；标记删除 → `RowDelete`）。</summary>
    /// <remarks>
    /// `Rows` 的顺序即 `apply_row_changes` 响应里 `change_index` 的下标：协议规定
    /// updates 在前、deletes 在后，在同一个请求（同一个显式事务）里顺序执行。
    /// 删除只传完整主键值，`DELETE FROM … WHERE …` 由后端驱动用 prepared statement 绑定参数生成，
    /// 前端不拼 SQL 字符串。
    /// </remarks>
    /// <param name="page">数据页。</param>
    /// <returns>提交计划。</returns>
    public static DataChangePlan Build(DataPage page)
    {
        ArgumentNullException.ThrowIfNull(page);

        var updates = new List<RowUpdate>();
        var deletes = new List<RowDelete>();
        var rows = new List<DataRow>();
        var changes = 0;

        // updates 在前：脏行（跳过标记删除 / 已删除的行）。
        foreach (var row in page.Rows)
        {
            if (!row.Dirty || row.Removed || row.MarkedForDeletion)
            {
                continue;
            }

            var update = new RowUpdate { ExpectedRowsAffected = 1 };
            var key = new RowKey();
            key.Values.AddRange(row.Key);
            update.Key = key;

            // 列序升序：同一次提交的请求字节稳定，便于比对与复现。
            foreach (var column in row.Edits.Keys.OrderBy(index => index))
            {
                if (column < 0 || column >= page.Columns.Count)
                {
                    continue;
                }

                update.Changes.Add(new ColumnChange
                {
                    Column = page.Columns[column].Name,
                    Value = row.Edits[column],
                });
            }

            if (update.Changes.Count == 0)
            {
                continue;
            }

            changes += update.Changes.Count;
            updates.Add(update);
            rows.Add(row);
        }

        var updateRowCount = rows.Count;

        // deletes 在后：多列主键全列、强类型值，走协议已有的参数化路径。
        foreach (var row in page.Rows)
        {
            if (!row.MarkedForDeletion || !row.Locatable)
            {
                continue;
            }

            var delete = new RowDelete { ExpectedRowsAffected = 1 };
            var key = new RowKey();
            key.Values.AddRange(row.Key);
            delete.Key = key;
            deletes.Add(delete);
            rows.Add(row);
            changes++;
        }

        return new DataChangePlan(updates, deletes, rows, changes, deletes.Count, updateRowCount);
    }
}

/// <summary>提交前的守卫结论（阈值确认 = 非模态的「再按一次」）。</summary>
internal sealed record DataSubmitPlan(
    bool Allowed,
    bool NeedsRiskConfirmation,
    bool AcknowledgeRisk,
    int RowCount,
    int ChangeCount,
    string Message)
{
    /// <summary>算这一批能不能发。</summary>
    /// <param name="page">数据页。</param>
    /// <param name="riskArmed">用户已经就「超过阈值」确认过一次。</param>
    /// <returns>守卫结论（<see cref="Message"/> 可直接展示）。</returns>
    public static DataSubmitPlan Build(DataPage page, bool riskArmed)
    {
        ArgumentNullException.ThrowIfNull(page);

        if (page.ReadOnly)
        {
            return new DataSubmitPlan(false, false, false, 0, 0, DataStatusText.ReadOnlyReasonText(page.ReadOnlyReason));
        }

        var rows = page.DirtyRowCount;
        var deletes = page.MarkedDeleteCount;
        var changes = page.ChangeCount + deletes;
        if (changes == 0)
        {
            return new DataSubmitPlan(false, false, false, 0, 0, "没有待提交的更改：双击单元格或按 F2 修改后按 Enter 暂存，或右键标记删除，再按 Ctrl+S 提交");
        }

        if (rows > DataLimits.RiskConfirmationRows && !riskArmed)
        {
            // 一次改这么多行是不可逆的高风险动作：本版不弹对话框，改成「再按一次确认」。
            var deletePart = deletes > 0 ? $"（其中删除 {deletes} 行）" : string.Empty;
            return new DataSubmitPlan(
                false,
                true,
                false,
                rows,
                changes,
                $"本次提交 {rows} 行{deletePart}，超过风险阈值 {DataLimits.RiskConfirmationRows} 行：再按一次 Ctrl+S（或再点「应用」）确认提交");
        }

        // ui-behavior-contract §8.2：变更里**含任何删除**都必须确认（删除单独警告）。本端没有模态对话框，
        // 与超阈值同一套「再按一次」语义，但确认文案单列删除行数。
        if (deletes > 0 && !riskArmed)
        {
            return new DataSubmitPlan(
                false,
                true,
                false,
                rows,
                changes,
                $"本次提交包含删除 {deletes} 行（删除不可撤销）：再按一次 Ctrl+S（或再点「应用」）确认提交");
        }

        return new DataSubmitPlan(
            true,
            false,
            rows > DataLimits.RiskConfirmationRows,
            rows,
            changes,
            $"正在提交 {rows} 行 / {changes} 条变更…");
    }
}

/// <summary>单行的提交失败（冲突 / 行已删除 / 其它）。</summary>
internal sealed record DataRowFailure(long RowNumber, string Code, string Message, bool RemovesRow);

/// <summary>一次提交的结论（应用到页面状态的是这里，不是 UI）。</summary>
internal sealed record DataApplyReport(
    bool Cancelled,
    bool Clamped,
    uint Deferred,
    int Applied,
    IReadOnlyList<DataRowFailure> Failures)
{
    /// <summary>整批都成功（没有失败、没有被取消）。</summary>
    public bool Ok => !Cancelled && Failures.Count == 0;

    /// <summary>把 <c>ApplyRowChanges</c> 的结果写回页面：成功行变干净，失败行保留（可重试）。</summary>
    /// <param name="plan">发出的计划（下标 → 行）。</param>
    /// <param name="response">服务端响应。</param>
    /// <returns>结论。</returns>
    public static DataApplyReport Apply(DataChangePlan plan, ApplyRowChangesResponse response)
    {
        ArgumentNullException.ThrowIfNull(plan);
        ArgumentNullException.ThrowIfNull(response);

        if (response.Cancelled)
        {
            // 取消是正常结果：整批已回滚，界面**保留**脏行与编辑文本（不采信任何部分结果）。
            return new DataApplyReport(true, response.Clamped, response.DeferredChanges, 0, Array.Empty<DataRowFailure>());
        }

        var failures = new List<DataRowFailure>();
        var applied = 0;
        var seen = new HashSet<int>();

        foreach (var result in response.Results)
        {
            var index = (int)result.ChangeIndex;
            if (index < 0 || index >= plan.Rows.Count)
            {
                continue;
            }

            seen.Add(index);
            var row = plan.Rows[index];
            var isDelete = index >= plan.UpdateRowCount;
            switch (result.Outcome)
            {
                case ChangeOutcome.Applied:
                    // 更新成功落回原值；删除成功则退出提交集合（保留 ✕N / 删除线，等事务提交后整行移出）——
                    // 不移除标记的话，部分失败那一轮之后的下一次 Ctrl+S 会重复发同一条 DELETE。
                    if (isDelete)
                    {
                        row.MarkedForDeletion = false;
                        row.DeleteExecuted = true;
                        row.Removed = true;
                    }
                    else
                    {
                        row.AcceptEdits();
                    }
                    applied++;
                    break;

                case ChangeOutcome.RowNotFound:
                    // 主键被别的会话删掉了：再重试也不会成功，行标成「已不存在」并退出提交集合。
                    row.DiscardEdits();
                    row.Removed = true;
                    if (isDelete)
                    {
                        // 已经被别的会话删掉了：别再把它当待删除行重试。
                        row.MarkedForDeletion = false;
                    }
                    failures.Add(DataConflicts.RowNotFound(row, result));
                    break;

                case ChangeOutcome.Conflict:
                case ChangeOutcome.Unspecified:
                    failures.Add(DataConflicts.Mismatch(row, result));
                    break;
            }
        }

        foreach (var index in Enumerable.Range(0, plan.Rows.Count))
        {
            if (!seen.Contains(index))
            {
                // 服务端没给这一行的结果（例如被钳制没执行到）：按「未执行」处理，保持脏、可重试。
                var row = plan.Rows[index];
                failures.Add(new DataRowFailure(
                    row.Number,
                    "table_data.not_applied",
                    $"第 {row.Number} 行本批没有执行（服务端未返回结果）：改动保留，可再次提交",
                    RemovesRow: false));
            }
        }

        return new DataApplyReport(false, response.Clamped, response.DeferredChanges, applied, failures);
    }
}

/// <summary>冲突与「行已删除」的固定文案（稳定错误码 + 可读原因）。</summary>
internal static class DataConflicts
{
    /// <summary>主键被删（`CHANGE_OUTCOME_ROW_NOT_FOUND`）：单独一句，不说成「影响行数不符」。</summary>
    /// <param name="row">目标行。</param>
    /// <param name="result">服务端逐行结果。</param>
    /// <returns>失败描述。</returns>
    public static DataRowFailure RowNotFound(DataRow row, RowChangeResult result) => new(
        row.Number,
        "table_data.row_deleted",
        $"第 {row.Number} 行已被其它会话删除（主键不再存在）{(result.Message.Length > 0 ? $"：{result.Message}" : string.Empty)}；本次改动已丢弃，刷新可看到最新数据",
        RemovesRow: true);

    /// <summary>乐观并发冲突（`CHANGE_OUTCOME_CONFLICT`）：影响行数与期望不符。</summary>
    /// <param name="row">目标行。</param>
    /// <param name="result">服务端逐行结果。</param>
    /// <returns>失败描述。</returns>
    public static DataRowFailure Mismatch(DataRow row, RowChangeResult result) => new(
        row.Number,
        "table_data.rows_affected_mismatch",
        $"第 {row.Number} 行影响行数与期望不符（乐观并发冲突，服务端影响 {result.RowsAffected} 行）{(result.Message.Length > 0 ? $"：{result.Message}" : string.Empty)}；改动保留，可重试或撤销",
        RemovesRow: false);
}

/// <summary>事务阶段（决定状态栏第 2 行右侧的文字）。</summary>
internal enum DataTxnPhase
{
    /// <summary>没有显式事务（本页没有改动，或提交后已提交）。</summary>
    AutoCommit = 0,

    /// <summary>显式事务已开（第一次暂存时 Begin）。</summary>
    Open = 1,

    /// <summary>提交 RPC 在飞。</summary>
    Committing = 2,

    /// <summary>刚回滚过（下一次改动会重新 Begin）。</summary>
    RolledBack = 3,
}

/// <summary>表数据面的全部固定文案（纯函数：文案是可断言的规则，不是渲染细节）。</summary>
internal static class DataStatusText
{
    /// <summary>只读原因的可展示说明（必须比「不可编辑」多给原因）。</summary>
    /// <param name="reason">服务端的只读原因。</param>
    /// <returns>一句话。</returns>
    public static string ReadOnlyReasonText(ReadOnlyReason reason) => reason switch
    {
        Sqarvo.Services.V1.ReadOnlyReason.NoPrimaryKey =>
            "只读：该表没有主键（无稳定排序、无法定位行，编辑被拒绝）",
        Sqarvo.Services.V1.ReadOnlyReason.DriverUnsupported =>
            "只读：驱动未声明行编辑能力",
        Sqarvo.Services.V1.ReadOnlyReason.ConnectionReadOnly =>
            "只读：连接以只读方式打开",
        _ => "只读：服务端声明本页不可编辑",
    };

    /// <summary>状态栏行 2 右侧：事务状态（与 #173 的五种文案一一对应）。</summary>
    /// <param name="readOnly">本页只读。</param>
    /// <param name="phase">事务阶段。</param>
    /// <param name="submitting">提交 RPC 在飞。</param>
    /// <param name="dirtyRows">未提交的行数。</param>
    /// <returns>`只读` / `自动提交` / `未提交 N 行` / `提交中…` / `已回滚` 之一。</returns>
    public static string TransactionLabel(bool readOnly, DataTxnPhase phase, bool submitting, int dirtyRows)
    {
        if (submitting || phase == DataTxnPhase.Committing)
        {
            return "提交中…";
        }

        if (readOnly && dirtyRows == 0)
        {
            return "只读";
        }

        if (dirtyRows > 0)
        {
            return $"未提交 {dirtyRows} 行";
        }

        return phase == DataTxnPhase.RolledBack ? "已回滚" : "自动提交";
    }

    /// <summary>页码文案：总页数只有精确总数时才知道，否则写 `?`（不编造）。</summary>
    /// <param name="page">数据页。</param>
    /// <returns>例如 `第 2 页 / 共 ? 页`。</returns>
    public static string PageLabel(DataPage page)
    {
        ArgumentNullException.ThrowIfNull(page);
        var total = page.TotalPagesExact;
        return total is { } known ? $"第 {page.PageNumber} / {known} 页" : $"第 {page.PageNumber} 页 / 共 ? 页";
    }

    /// <summary>总数文案（三态必须可区分：精确 / 估计 / 未知）。</summary>
    /// <param name="page">数据页。</param>
    /// <returns>`共 N 行` / `约 N 行` / `共 ? 行`；跳过统计时说明原因。</returns>
    public static string TotalText(DataPage page)
    {
        ArgumentNullException.ThrowIfNull(page);
        return page.TotalKind switch
        {
            TotalKind.Exact => $"共 {page.TotalRows} 行",
            TotalKind.Estimate => $"约 {page.TotalRows} 行",
            _ => page.TotalSkipped ? "共 ? 行（未统计：超出计数预算）" : "共 ? 行",
        };
    }

    /// <summary>已取行数 + 总数。</summary>
    /// <param name="page">数据页。</param>
    /// <returns>例如 `已取 400 行 · 共 ? 行`。</returns>
    public static string CountsText(DataPage page)
    {
        ArgumentNullException.ThrowIfNull(page);
        return $"已取 {page.Rows.Count} 行 · {TotalText(page)}";
    }

    /// <summary>状态栏行 2 左侧：这一页在看什么。</summary>
    /// <param name="page">数据页。</param>
    /// <returns>例如 `表 customers · 第 1 页 / 共 ? 页 · 已取 200 行`。</returns>
    public static string Summary(DataPage page)
    {
        ArgumentNullException.ThrowIfNull(page);
        return $"表 {page.TableLabel} · {PageLabel(page)} · {CountsText(page)}";
    }

    /// <summary>排序保证文案（没有排序信息时为空串：不摆一句「未知排序」占位）。</summary>
    /// <param name="ordering">服务端给的排序保证。</param>
    /// <returns>一句话或空串。</returns>
    public static string OrderingLabel(RowOrder ordering) => ordering switch
    {
        RowOrder.PrimaryKeyAscending => "排序：主键升序（翻页稳定）",
        RowOrder.Unstable => "排序：不稳定（无主键）",
        RowOrder.RequestedSort => "排序：请求的列（重复值翻页可能重复或漏行）",
        _ => string.Empty,
    };

    /// <summary>失败文案：稳定错误码必须一起显示。</summary>
    /// <param name="headline">后端给的主标题。</param>
    /// <param name="code">稳定错误码。</param>
    /// <param name="keptRows">失败时页面上保留的行数。</param>
    /// <returns>可直接展示的一句。</returns>
    public static string FailureText(string headline, string code, int keptRows) =>
        $"读取失败：{headline}（{code}）" + (keptRows > 0 ? $" · 已取的 {keptRows} 行仍保留在页面上" : string.Empty);

    /// <summary>页内状态行（五态互斥：失败 &gt; 读取中 &gt; 只读 &gt; 空 &gt; 正常时为空）。</summary>
    /// <param name="page">数据页。</param>
    /// <returns>一句说明；正常有数据时为空串（不占位）。</returns>
    public static string StateText(DataPage page)
    {
        ArgumentNullException.ThrowIfNull(page);

        if (page.Headline is { Length: > 0 } headline)
        {
            return FailureText(headline, page.Code ?? "unknown", page.Rows.Count);
        }

        if (page.Loading)
        {
            return page.Rows.Count == 0
                ? $"正在读取表数据…（offset {page.Offset}，每页 {page.PageSize} 行）"
                : $"正在读取…（已取 {page.Rows.Count} 行，完成后替换这一页）";
        }

        if (page.ReadOnly)
        {
            return ReadOnlyReasonText(page.ReadOnlyReason);
        }

        if (page.Rows.Count == 0)
        {
            return page.Offset > 0
                ? "本页没有数据：offset 超出总数（服务端如实返回空行），回上一页或刷新再看"
                : "空表：该表存在，但当前条件下没有任何行";
        }

        return string.Empty;
    }

    /// <summary>页内说明行（列数 / 页码 / 计数 / 排序 / 只读降级说明）。</summary>
    /// <param name="page">数据页。</param>
    /// <returns>一句说明；没有列时为「正在读取列…」。</returns>
    public static string MetaText(DataPage page)
    {
        ArgumentNullException.ThrowIfNull(page);

        if (page.Columns.Count == 0)
        {
            return page.Loading ? "正在读取列…" : string.Empty;
        }

        var parts = new List<string>
        {
            $"{page.Columns.Count} 列",
            PageLabel(page),
            CountsText(page),
            page.HasMore ? "还有更多" : "已到本页末尾",
        };

        if (OrderingLabel(page.Ordering) is { Length: > 0 } ordering)
        {
            parts.Add(ordering);
        }

        if (page.Note is { Length: > 0 } note)
        {
            parts.Add(note);
        }

        return string.Join(" · ", parts);
    }

    /// <summary>提交结论的汇总文字（成功 / 部分失败 / 取消）。</summary>
    /// <param name="report">提交结论。</param>
    /// <param name="deferred">是否被单批上限钳制。</param>
    /// <returns>可直接展示的一句。</returns>
    public static string SubmitSummary(DataApplyReport report, int deferred)
    {
        ArgumentNullException.ThrowIfNull(report);

        if (report.Cancelled)
        {
            return "提交已取消：整批回滚，改动仍保留在页面上（可再次提交）";
        }

        var text = report.Failures.Count == 0
            ? $"已提交 {report.Applied} 行"
            : $"提交完成：成功 {report.Applied} 行，失败 {report.Failures.Count} 行（见下方逐行原因）";

        if (report.Clamped && deferred > 0)
        {
            text += $"；单批上限 500 行：本次只执行了前 {report.Applied} 条，还有 {deferred} 条未提交——再按一次 Ctrl+S 续传";
        }

        return text;
    }

    /// <summary>标记删除（单行）：状态栏行 2 文案。</summary>
    public static string MarkDeletedText(long rowNumber) =>
        $"第 {rowNumber} 行已标记删除（提交前可撤销）";

    /// <summary>标记删除（批量）：状态栏行 2 文案。</summary>
    public static string MarkDeletedBatchText(int count) =>
        $"已标记删除 {count} 行（批量提交前可撤销）";

    /// <summary>提交成功：状态栏行 2 文案。</summary>
    public static string DeleteCommittedText(int count) => $"已提交：删除 {count} 行";

    /// <summary>编辑被拒绝时的原因（逐条不同，不写统一一句「不可编辑」）。</summary>
    /// <param name="page">数据页。</param>
    /// <param name="row">目标行。</param>
    /// <param name="column">目标列。</param>
    /// <returns><see langword="null"/> = 可以编辑；否则是可直接展示的原因。</returns>
    public static string? EditBlockReason(DataPage page, DataRow row, int column)
    {
        ArgumentNullException.ThrowIfNull(page);
        ArgumentNullException.ThrowIfNull(row);

        if (column < 0 || column >= page.Columns.Count)
        {
            return "这一列不存在";
        }

        if (page.ReadOnly)
        {
            return ReadOnlyReasonText(page.ReadOnlyReason);
        }

        var target = page.Columns[column];
        if (target.PrimaryKey)
        {
            // 主键负责定位行：改主键等于改行身份（后续页 / 已暂存的行都会指向旧值）。
            return $"主键列 {target.Name} 不可编辑：主键用于定位行，改主键会改变行身份（请用 SQL 修改）";
        }

        if (!row.Locatable)
        {
            // 「非主键且不可定位」：这一列本身不是主键，行又拿不到主键值，提交时服务端一定会拒绝。
            return $"列 {target.Name} 不是主键，行只能靠主键定位：这一行缺少主键值 / 表没有主键，因此只读";
        }

        if (row.Original(column) is { Truncated: true })
        {
            return $"列 {target.Name} 的值被服务端截断（只收到前缀），禁止编辑：提交截断值会写坏数据";
        }

        if (!DataInput.SupportsInlineEdit(target.Kind))
        {
            return $"列 {target.Name} 的类型 {target.TypeLabel} 不在本版就地编辑范围（二进制 / 日期时间等请用 SQL 修改）";
        }

        return null;
    }
}

/// <summary>
/// 「复制为 INSERT」的写出口径（#1）：把选中行的当前值（含未提交编辑）合成一条多值 INSERT。
/// </summary>
/// <remarks>
/// 与 Linux 端 <c>frontend/linux/src/export.rs</c> 的 <c>multi_row_insert</c> / <c>sql_literal</c>
/// 同一口径：标识符双引号、字符串单引号（<c>'</c> 转义成 <c>''</c>）、NULL 写 <c>NULL</c>、
/// 数字 / Decimal 原文、BLOB / 非有限浮点 / 截断值写 NULL 并在末尾附 sqarvo 注释。
/// 值格式化复用 <see cref="QueryCellText.Format"/>（同一程序集的既有值 → 文本口径）。
/// 生成的是**供粘贴执行的文本**，不经过数据库写入路径，不拼进任何 gRPC 请求。
/// </remarks>
internal static class DataInsertSql
{
    /// <summary>合成一条多值 INSERT 语句。</summary>
    /// <param name="page">数据页（取表名 / 库名 / 列清单）。</param>
    /// <param name="rows">选中行（取当前值）。</param>
    /// <returns>以 LF 结尾的 INSERT 语句；没有列或没有行时返回空串。</returns>
    public static string Build(DataPage page, IReadOnlyList<DataRow> rows)
    {
        ArgumentNullException.ThrowIfNull(page);
        ArgumentNullException.ThrowIfNull(rows);

        if (page.Columns.Count == 0 || rows.Count == 0)
        {
            return string.Empty;
        }

        var table = page.CatalogName.Length == 0
            ? Quote(page.TableName)
            : Quote(page.CatalogName) + "." + Quote(page.TableName);
        var columns = string.Join(", ", page.Columns.Select(column => Quote(column.Name)));

        var text = new StringBuilder();
        text.Append("INSERT INTO ").Append(table).Append(" (").Append(columns).Append(") VALUES\n");
        var nulled = 0;
        for (var index = 0; index < rows.Count; index++)
        {
            var values = new List<string>(page.Columns.Count);
            for (var column = 0; column < page.Columns.Count; column++)
            {
                values.Add(Literal(rows[index].Value(column), ref nulled));
            }

            text.Append('(').Append(string.Join(", ", values)).Append(')');
            text.Append(index + 1 < rows.Count ? ",\n" : ";\n");
        }

        if (nulled > 0)
        {
            text.Append("-- sqarvo: ").Append(nulled)
                .Append(" 个 BLOB / 非有限浮点 / 截断值写成 NULL（客户端不导出原始字节）\n");
        }

        return text.ToString();
    }

    /// <summary>标识符引号（双引号内嵌双引号转义）。</summary>
    private static string Quote(string identifier) => "\"" + identifier.Replace("\"", "\"\"") + "\"";

    /// <summary>单个值的 SQL 字面量。</summary>
    private static string Literal(ColumnValue? value, ref int nulled)
    {
        if (value is null || value.Kind == ValueKind.Null || value.Truncated)
        {
            if (value is not null && value.Truncated)
            {
                nulled++;
            }

            return "NULL";
        }

        switch (value.ValueCase)
        {
            case ColumnValue.ValueOneofCase.BoolValue:
                // TRUE / FALSE 在 PostgreSQL 是唯一可用写法，MySQL / MariaDB / SQLite 都把它们当 1 / 0。
                return value.BoolValue ? "TRUE" : "FALSE";
            case ColumnValue.ValueOneofCase.Int64Value:
                return value.Int64Value.ToString(CultureInfo.InvariantCulture);
            case ColumnValue.ValueOneofCase.Uint64Value:
                return value.Uint64Value.ToString(CultureInfo.InvariantCulture);
            case ColumnValue.ValueOneofCase.Float64Value:
                if (double.IsFinite(value.Float64Value))
                {
                    return value.Float64Value.ToString("R", CultureInfo.InvariantCulture);
                }

                nulled++;
                return "NULL";
            case ColumnValue.ValueOneofCase.DecimalValue:
                return QueryCellText.Format(value);
            case ColumnValue.ValueOneofCase.BytesValue:
                nulled++;
                return "NULL";
            case ColumnValue.ValueOneofCase.TextValue:
                return "'" + value.TextValue.Replace("'", "''") + "'";
            default:
                // 日期 / 时间 / JSON / UUID / 数组等：复用查询结果的值格式化口径，加单引号。
                return "'" + QueryCellText.Format(value).Replace("'", "''") + "'";
        }
    }
}