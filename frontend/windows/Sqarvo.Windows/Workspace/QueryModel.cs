using Sqarvo.Common.V1;
using Sqarvo.Core.Model;
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;

namespace Sqarvo.Windows.Workspace;

/// <summary>
/// 结果区的五态（`ui-behavior-contract.md` §8.3 的「查询标签」列）：加载 / 空 / 错误 / 取消 / 只读。
/// </summary>
/// <remarks>
/// <para>
/// 这五个状态是**互斥**的：任何时刻结果区只显示其中一句。已加载的行在 Error / Cancelled 下**保留**
/// （用户能看到已经拿到的数据，而不是屏幕被一句错误清空）。
/// </para>
/// <para>
/// <see cref="Empty"/> 覆盖三种「没有行可显示」的情形，文案不同但状态相同：还没执行过（尚未执行）、
/// 语句执行成功但没有返回行（空结果）、语句没有结果集只有影响行数（无结果集）。
/// <see cref="ReadOnly"/> 是「已有行」的终态：查询结果网格在 v0.1 不可编辑，所以每一份结果都是只读的，
/// 文案按服务端的 <c>result_meta.read_only</c> 如实区分「服务端声明只读」与「本版不可编辑」。
/// </para>
/// </remarks>
internal enum QueryViewState
{
    /// <summary>正在执行 / 正在取下一批（首批还没回来时行区为空）。</summary>
    Loading = 0,

    /// <summary>没有行可显示：尚未执行 / 空结果 / 无结果集。</summary>
    Empty = 1,

    /// <summary>失败（保留已加载行）。</summary>
    Error = 2,

    /// <summary>已取消（保留已加载行，游标已释放）。</summary>
    Cancelled = 3,

    /// <summary>已有行并且是只读结果（查询结果网格的终态）。</summary>
    ReadOnly = 4,
}

/// <summary>查询面里与档位无关的硬约束。</summary>
/// <remarks>
/// 两个数字都来自契约而不是布局：每批 200 行（`--qa-query` / 滚动续拉的单位）、
/// 标签上限 8 个（与后端 <c>QueryClient.MaxConcurrentQueries</c> 对齐，保证不会撞 RESOURCE_EXHAUSTED）。
/// </remarks>
internal static class QueryLimits
{
    /// <summary>每次 <c>Execute</c> / <c>FetchBatch</c> 请求的行数。</summary>
    public const int BatchSize = 200;

    /// <summary>同时打开的标签上限（Objects 页不占名额）。</summary>
    public const int MaxTabs = 8;

    /// <summary>离末尾不到这么多行时开始预取下一批（滚动到底就续拉）。</summary>
    public const int PrefetchMargin = 20;
}

/// <summary>结果集的一列（表头文案 = 「列名 · 原始类型」，与数据网格同一个口径）。</summary>
/// <param name="Name">列名。</param>
/// <param name="TypeLabel">数据库原始类型名；结果集没有类型信息时为空串（不编造）。</param>
/// <param name="Numeric">数值类列（右对齐；不影响值本身）。</param>
internal sealed record QueryColumn(string Name, string TypeLabel, bool Numeric)
{
    /// <summary>表头文字：有原始类型时用中点分隔，没有就只写列名。</summary>
    public string HeaderText => TypeLabel.Length == 0 ? Name : $"{Name} · {TypeLabel}";

    /// <summary>从协议的列元数据建一列。</summary>
    /// <param name="column">`ResultSetMeta.columns` 里的一项。</param>
    /// <returns>展示用的列。</returns>
    public static QueryColumn From(ColumnMeta column)
    {
        var type = column.Type;
        var label = type?.OriginalTypeName ?? string.Empty;
        var numeric = type is not null && type.Kind is ColumnTypeKind.ColumnTypeInteger
            or ColumnTypeKind.ColumnTypeFloat
            or ColumnTypeKind.ColumnTypeDecimal;
        return new QueryColumn(column.Name, label, numeric);
    }
}

/// <summary>
/// 单元格文本：把 <see cref="ColumnValue"/> 渲染成一行文字。
/// </summary>
/// <remarks>
/// <para>
/// 纯函数，不依赖 WinUI，因此可以在 Linux 上直接断言（NULL ≠ 空字符串 ≠ 二进制占位）。
/// 口径与 Linux 的 <c>backend/query.rs::format_cell</c> 一致：NULL 写 <c>NULL</c>、空文本写 <c>''</c>、
/// 二进制写 <c>0x89 50 …</c> 前缀、被服务端截断的值追加「…（已截断）」。
/// </para>
/// </remarks>
internal static class QueryCellText
{
    /// <summary>NULL 的显示文字（与空字符串区分：空串显示为 <c>''</c>）。</summary>
    public const string Null = "NULL";

    /// <summary>截断后缀。</summary>
    public const string TruncatedSuffix = "…（已截断）";

    /// <summary>渲染一个单元格。</summary>
    /// <param name="value">协议值。</param>
    /// <returns>单行文字；未知类型不猜内容，只写类型名。</returns>
    public static string Format(ColumnValue value)
    {
        ArgumentNullException.ThrowIfNull(value);

        if (value.Kind == ValueKind.Null)
        {
            return Null;
        }

        var text = value.ValueCase switch
        {
            ColumnValue.ValueOneofCase.BoolValue => value.BoolValue ? "true" : "false",
            ColumnValue.ValueOneofCase.Int64Value => value.Int64Value.ToString(CultureInfo.InvariantCulture),
            ColumnValue.ValueOneofCase.Uint64Value => value.Uint64Value.ToString(CultureInfo.InvariantCulture),
            ColumnValue.ValueOneofCase.Float64Value => value.Float64Value.ToString("R", CultureInfo.InvariantCulture),
            ColumnValue.ValueOneofCase.DecimalValue => FormatDecimal(value.DecimalValue),
            ColumnValue.ValueOneofCase.TextValue => value.TextValue.Length == 0 ? "''" : value.TextValue,
            ColumnValue.ValueOneofCase.BytesValue => FormatBytes(value.BytesValue),
            ColumnValue.ValueOneofCase.DateValue => FormatDate(value.DateValue),
            ColumnValue.ValueOneofCase.TimeValue => FormatTime(value.TimeValue),
            ColumnValue.ValueOneofCase.DatetimeValue => FormatDateTime(value.DatetimeValue),
            ColumnValue.ValueOneofCase.TimestampValue => FormatTimestamp(value.TimestampValue),
            ColumnValue.ValueOneofCase.JsonValue => value.JsonValue,
            ColumnValue.ValueOneofCase.UuidValue => value.UuidValue,
            ColumnValue.ValueOneofCase.ArrayValue => FormatArray(value.ArrayValue),
            ColumnValue.ValueOneofCase.UnsupportedValue => value.UnsupportedValue,
            _ => string.Empty,
        };

        if (text.Length == 0)
        {
            // 没有值但也不是 NULL：如实写「无法显示」而不是编一个空串。
            return value.OriginalTypeName.Length == 0 ? "(无法显示)" : $"({value.OriginalTypeName})";
        }

        return value.Truncated ? text + TruncatedSuffix : text;
    }

    /// <summary>这一格是不是弱化显示（NULL / 空字符串——两者都不能被读成「有值」）。</summary>
    /// <param name="text">已渲染的文字。</param>
    /// <returns>NULL 或空字符串时为 <see langword="true"/>。</returns>
    public static bool IsWeak(string text) =>
        string.Equals(text, Null, StringComparison.Ordinal)
        || string.Equals(text, "''", StringComparison.Ordinal);

    private static string FormatDecimal(DecimalValue value)
    {
        var digits = value.UnscaledValue ?? string.Empty;
        if (value.Scale <= 0)
        {
            return digits;
        }

        var negative = digits.StartsWith('-');
        var body = negative ? digits[1..] : digits;
        if (body.Length <= value.Scale)
        {
            body = new string('0', value.Scale - body.Length + 1) + body;
        }

        var cut = body.Length - value.Scale;
        return $"{(negative ? "-" : string.Empty)}{body[..cut]}.{body[cut..]}";
    }

    private static string FormatBytes(global::Google.Protobuf.ByteString value)
    {
        var bytes = value.ToByteArray();
        var shown = Math.Min(bytes.Length, 4);
        var builder = new StringBuilder("0x");
        for (var index = 0; index < shown; index++)
        {
            if (index > 0)
            {
                builder.Append(' ');
            }

            builder.Append(bytes[index].ToString("X2", CultureInfo.InvariantCulture));
        }

        if (bytes.Length > shown)
        {
            builder.Append("…");
        }

        return $"{builder} ({bytes.Length} bytes)";
    }

    private static string FormatDate(DateTimeValue value) =>
        $"{value.Year:D4}-{value.Month:D2}-{value.Day:D2}";

    private static string FormatTime(DateTimeValue value) =>
        $"{value.Hour:D2}:{value.Minute:D2}:{value.Second:D2}";

    private static string FormatDateTime(DateTimeValue value) =>
        $"{FormatDate(value)} {FormatTime(value)}";

    private static string FormatTimestamp(TimestampValue value)
    {
        var moment = DateTimeOffset.FromUnixTimeMilliseconds(value.UnixNanos / 1_000_000)
            .ToOffset(TimeSpan.FromSeconds(value.OffsetSeconds));
        return moment.ToString("yyyy-MM-dd HH:mm:ss", CultureInfo.InvariantCulture);
    }

    private static string FormatArray(ArrayValue value) =>
        "[" + string.Join(", ", value.Elements.Select(Format)) + "]";
}

/// <summary>
/// SQL 文本的展示口径：状态栏摘要、以及「选中部分优先执行」。
/// </summary>
internal static class QuerySql
{
    /// <summary>状态栏行 2 的 SQL 摘要：单行、折叠空白、限长（超出加省略号）。</summary>
    /// <param name="sql">要概括的 SQL（最近一次执行的文本）。</param>
    /// <param name="maxLength">最大字符数。</param>
    /// <returns>摘要；没有执行过时返回「尚未执行」。</returns>
    public static string Summary(string? sql, int maxLength = 80)
    {
        if (string.IsNullOrWhiteSpace(sql))
        {
            return "尚未执行";
        }

        var builder = new StringBuilder();
        var lastWasSpace = false;
        foreach (var character in sql)
        {
            var isSpace = char.IsWhiteSpace(character);
            if (isSpace)
            {
                if (builder.Length > 0 && !lastWasSpace)
                {
                    builder.Append(' ');
                }
            }
            else
            {
                builder.Append(character);
            }

            lastWasSpace = isSpace;

            if (builder.Length > maxLength)
            {
                break;
            }
        }

        var text = builder.ToString().TrimEnd();
        return text.Length > maxLength ? text[..maxLength] + "…" : text;
    }

}

/// <summary>
/// 一个查询标签的状态：编辑器之外的**全部**数据都在这一个对象里（页面只是它的渲染）。
/// </summary>
/// <remarks>
/// 状态转换只有这几条（其余路径都不许直接改 <see cref="View"/>）：
/// <list type="bullet">
/// <item><description><see cref="BeginExecution"/>：Loading（旧行与旧列一起清空）。</description></item>
/// <item><description><see cref="ApplyFirstBatch"/>：Empty（没有结果集 / 没有行）或 ReadOnly（有行）。</description></item>
/// <item><description><see cref="AppendBatch"/>：保持 ReadOnly，只追加行；<see cref="HasMore"/> 跟着服务端走。</description></item>
/// <item><description><see cref="MarkCancelled"/> / <see cref="MarkFailure"/>：Cancelled / Error（行保留）。</description></item>
/// <item><description><see cref="MarkCursorReleased"/>：游标已释放（切页 / 重新执行前的说明，不改变行）。</description></item>
/// </list>
/// </remarks>
internal sealed class QueryTabState
{
    /// <summary>建一个尚未执行过的查询标签状态。</summary>
    /// <param name="number">标签序号（1 起，不复用已关闭的序号）。</param>
    public QueryTabState(int number)
    {
        Number = number;
    }

    /// <summary>标签序号（「查询 N」里的 N）。</summary>
    public int Number { get; }

    /// <summary>标签名「查询 N」（不带 `@ 库`：库与连接在 tooltip 里，与契约 §4 一致）。</summary>
    public string Title => $"查询 {Number}";

    /// <summary>这一页绑定的连接身份键（建页时解析一次，之后不随树选中漂移）。</summary>
    public string ConnectionKey { get; set; } = string.Empty;

    /// <summary>连接名（tooltip 用）。</summary>
    public string ConnectionName { get; set; } = string.Empty;

    /// <summary>当前库名（tooltip 用；空 = 连接没有默认库）。</summary>
    public string CatalogName { get; set; } = string.Empty;

    /// <summary>会话 ID（执行时用它）；连接断开后清空。</summary>
    public string SessionId { get; set; } = string.Empty;

    /// <summary>编辑器内容与「最近一次执行的 SQL」不同（标签名上的 <c>*</c>）。</summary>
    public bool Dirty { get; set; }

    /// <summary>最近一次执行的 SQL 原文（状态栏摘要的来源，不是编辑器当前内容）。</summary>
    public string ExecutedSql { get; private set; } = string.Empty;

    /// <summary>五态。</summary>
    public QueryViewState View { get; private set; } = QueryViewState.Empty;

    /// <summary>已取到的行（单元格已格式化成文字，滚动时不再重算）。</summary>
    public List<string[]> Rows { get; } = new();

    /// <summary>结果集的列（空 = 没有结果集）。</summary>
    public List<QueryColumn> Columns { get; } = new();

    /// <summary>服务端还有下一批。</summary>
    public bool HasMore { get; private set; }

    /// <summary>结果集段数（`Execute` 的 <c>statements</c> 条数；v0.1 恒为 1）。</summary>
    public int StatementCount { get; private set; }

    /// <summary>语句是否返回了结果集（false = UPDATE / DDL，只有影响行数）。</summary>
    public bool HasResultSet { get; private set; }

    /// <summary>服务端声明结果集只读（<c>result_meta.read_only</c>）。</summary>
    public bool ReadOnlyResult { get; private set; }

    /// <summary>无结果集语句的影响行数。</summary>
    public long RowsAffected { get; private set; }

    /// <summary>失败主标题（Error 态）。</summary>
    public string? Headline { get; private set; }

    /// <summary>稳定错误码（Error 态；界面必须显示）。</summary>
    public string? Code { get; private set; }

    /// <summary>补充说明（取消原因 / 游标已释放 / 空结果细节），不覆盖五态本身。</summary>
    public string? Note { get; private set; }

    /// <summary>后端分配的 query_id（诊断与取消用；不含敏感信息）。</summary>
    public string QueryId { get; private set; } = string.Empty;

    /// <summary>`Execute` 在飞（入口因此切取消态）。</summary>
    public bool Executing { get; set; }

    /// <summary>`FetchBatch` 在飞（同一时刻最多一批）。</summary>
    public bool Fetching { get; set; }

    /// <summary>还在跑（执行中或取批中）——取消入口据此判断是否幂等忽略。</summary>
    public bool Busy => Executing || Fetching;

    /// <summary>状态栏行 2 右格：`N 行 / 已取 M 行`（总数未知时 N 写 <c>?</c>，不编造）。</summary>
    public string CountsText => HasMore
        ? $"? 行 / 已取 {Rows.Count} 行（还有更多）"
        : $"{Rows.Count} 行 / 已取 {Rows.Count} 行";

    /// <summary>状态栏行 2 左格：`SQL: <摘要>`。</summary>
    public string SqlSummaryLine => $"SQL: {QuerySql.Summary(ExecutedSql)}";

    /// <summary>结果区上方那一句（五态互斥；行保留在 Error / Cancelled 下）。</summary>
    /// <remarks>取下一批在飞时先说「正在取下一批…」：那是 Loading 的那一半语义，不能只靠按钮变取消暗示。</remarks>
    public string StateText => Fetching && View == QueryViewState.ReadOnly
        ? $"正在取下一批…（已取 {Rows.Count} 行，稍后追加上去）"
        : View switch
    {
        QueryViewState.Loading => "正在执行…（等待首批数据）",
        QueryViewState.Empty => EmptyText(),
        QueryViewState.Error => $"查询失败：{Headline}（{Code}）",
        QueryViewState.Cancelled => $"已取消：保留已取的 {Rows.Count} 行（游标已释放，可重新执行）",
        _ => ReadOnlyText(),
    };

    /// <summary>结果集说明（段数 / 列数 / 查询 id）——与五态分开，始终显示。</summary>
    public string MetaText
    {
        get
        {
            if (StatementCount == 0)
            {
                return string.Empty;
            }

            var segments = StatementCount == 1
                ? "结果集 1 段"
                : $"结果集 {StatementCount} 段（v0.1 只显示第 1 段）";
            var columns = Columns.Count == 0 ? string.Empty : $" · {Columns.Count} 列";
            var query = string.IsNullOrEmpty(QueryId) ? string.Empty : $" · 查询 {QueryId}";
            var note = string.IsNullOrEmpty(Note) ? string.Empty : $" · {Note}";
            return segments + columns + query + note;
        }
    }

    /// <summary>开始一次执行：行与列一起作废（不把上一份结果拼到新结果上）。</summary>
    /// <param name="sql">本次执行的 SQL（选中部分或全文）。</param>
    public void BeginExecution(string sql)
    {
        ExecutedSql = sql;
        Rows.Clear();
        Columns.Clear();
        HasMore = false;
        StatementCount = 0;
        HasResultSet = true;
        ReadOnlyResult = false;
        RowsAffected = 0;
        Headline = null;
        Code = null;
        Note = null;
        QueryId = string.Empty;
        View = QueryViewState.Loading;
        Executing = true;
    }

    /// <summary>首批到达：语句摘要 + 首批行（句柄只是搬运工，结论全在 <see cref="ApplyFirstBatchCore"/>）。</summary>
    /// <param name="handle">刚拿到的句柄（读 statements / 首批行 / has_more / query_id）。</param>
    /// <param name="readOnly">服务端声明的只读标志。</param>
    public void ApplyFirstBatch(Sqarvo.Core.Clients.QueryHandle handle, bool readOnly)
    {
        ArgumentNullException.ThrowIfNull(handle);

        var statement = handle.Statements.Count > 0 ? handle.Statements[0] : null;
        ApplyFirstBatchCore(
            queryId: handle.QueryId,
            statementCount: Math.Max(1, handle.Statements.Count),
            hasResultSet: statement?.HasResultSet ?? false,
            rowsAffected: (long)(statement?.RowsAffected ?? 0),
            readOnly: readOnly,
            columns: statement?.ResultMeta?.Columns,
            rows: handle.InitialRows,
            hasMore: handle.HasMore);
    }

    /// <summary>
    /// 首批的结论写进状态（<c>Execute</c> 与纯逻辑自检共用：不依赖 gRPC 句柄，Linux 上可以直接断言）。
    /// </summary>
    /// <param name="queryId">后端分配的 query_id。</param>
    /// <param name="statementCount">语句条数（v0.1 恒为 1）。</param>
    /// <param name="hasResultSet">语句是否返回结果集。</param>
    /// <param name="rowsAffected">无结果集语句的影响行数。</param>
    /// <param name="readOnly">服务端声明结果集只读。</param>
    /// <param name="columns">列元数据（可为空：没有结果集时）。</param>
    /// <param name="rows">首批行。</param>
    /// <param name="hasMore">服务端还有下一批。</param>
    internal void ApplyFirstBatchCore(
        string queryId,
        int statementCount,
        bool hasResultSet,
        long rowsAffected,
        bool readOnly,
        IEnumerable<ColumnMeta>? columns,
        IReadOnlyList<Row> rows,
        bool hasMore)
    {
        QueryId = queryId;
        StatementCount = Math.Max(1, statementCount);
        HasResultSet = hasResultSet;
        RowsAffected = rowsAffected;
        ReadOnlyResult = readOnly;

        Columns.Clear();
        if (columns is not null)
        {
            foreach (var column in columns)
            {
                Columns.Add(QueryColumn.From(column));
            }
        }

        AppendRows(rows);
        HasMore = hasMore;
        Executing = false;
        ResolveLoadedState();
    }

    /// <summary>后续批次到达：只追加行，并把 has_more 跟到服务端的值。</summary>
    /// <param name="batch">`FetchBatch` 的结果。</param>
    public void AppendBatch(QueryBatch batch)
    {
        ArgumentNullException.ThrowIfNull(batch);

        AppendRows(batch.Rows);
        HasMore = batch.HasMore;
        Fetching = false;
        ResolveLoadedState();
    }

    /// <summary>取消：行保留，状态转 Cancelled。</summary>
    /// <param name="note">取消原因（可为空）。</param>
    public void MarkCancelled(string? note = null)
    {
        Executing = false;
        Fetching = false;
        HasMore = false;
        Note = note;
        View = QueryViewState.Cancelled;
    }

    /// <summary>失败：行保留，错误码与文案都写进状态。</summary>
    /// <param name="headline">主标题。</param>
    /// <param name="code">稳定错误码。</param>
    public void MarkFailure(string headline, string code)
    {
        Executing = false;
        Fetching = false;
        HasMore = false;
        Headline = headline;
        Code = code;
        View = QueryViewState.Error;
    }

    /// <summary>游标已释放（切页 / 重新执行 / 关页）：只加说明，不谎称还有更多。</summary>
    /// <param name="reason">释放原因。</param>
    public void MarkCursorReleased(string reason)
    {
        HasMore = false;
        Note = $"游标已释放（{reason}）";
    }

    private void AppendRows(IReadOnlyList<Row> rows)
    {
        foreach (var row in rows)
        {
            var cells = new string[row.Values.Count];
            for (var index = 0; index < row.Values.Count; index++)
            {
                cells[index] = QueryCellText.Format(row.Values[index]);
            }

            Rows.Add(cells);
        }
    }

    private void ResolveLoadedState()
    {
        if (!HasResultSet)
        {
            // UPDATE / INSERT / DDL：没有结果集，只有影响行数。绝不假装有结果。
            View = QueryViewState.Empty;
            return;
        }

        if (Rows.Count == 0 && !HasMore)
        {
            View = QueryViewState.Empty;
            return;
        }

        // 有行（或服务端还有行）：查询结果网格在 v0.1 不可编辑，进入只读终态。
        View = QueryViewState.ReadOnly;
    }

    private string EmptyText()
    {
        // 一次都没执行过：先判「尚未执行」，否则新标签会被说成「无结果集（影响 0 行）」。
        if (StatementCount == 0 && string.IsNullOrEmpty(ExecutedSql))
        {
            return "尚未执行：输入 SQL 后按 Ctrl+Enter（或点「执行」）";
        }

        if (!HasResultSet)
        {
            return $"空结果：语句执行成功但没有结果集（影响 {RowsAffected} 行）";
        }

        return "空结果：语句执行成功，但没有返回任何行";
    }

    private string ReadOnlyText()
    {
        if (Columns.Count == 0)
        {
            return "只读结果：已取到行，但结果集没有列元数据（无法确定列，不显示表格）";
        }

        return ReadOnlyResult
            ? "只读结果：服务端声明该结果集只读（本版查询结果网格不支持编辑）"
            : "只读结果：查询结果网格在本版不支持编辑（不是数据库权限问题）";
    }
}