using Sqarvo.Common.V1;
using Sqarvo.Core.Model;
using Sqarvo.Services.V1;
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;

namespace Sqarvo.Windows.Workspace;

/// <summary>
/// 结果区的状态（`ui-behavior-contract.md` §8.3 的「查询标签」列）：加载 / 空 / 错误 / 取消 / 只读，
/// 另加 #190 多语句的两个占位态：无结果集（写语句摘要）/ 结果未返回（原子失败里已执行的语句）。
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

    /// <summary>#190 多语句：写语句执行成功但没有结果集，占位页显示影响行数摘要。</summary>
    NoResultSet = 5,

    /// <summary>#190 多语句原子失败（ADR 0006）：该语句已执行成功，但后端未保留其结果游标。</summary>
    Undelivered = 6,
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
/// #190 多语句：一条语句的独立网格状态（与 Linux `StatementSlot` 同口径）。
/// 行列表与顶层 <see cref="QueryTabState.Rows"/> 共享同一实例——切换语句只换引用，不复制行，
/// 已加载的行在切换后原样保留。
/// </summary>
internal sealed class StatementSlot
{
    /// <summary>语句摘要（失败路径为 null：原子失败响应不带任何摘要）。</summary>
    public StatementSummary? Summary { get; init; }

    /// <summary>该语句已加载的行。</summary>
    public List<string[]> Rows { get; } = new();

    /// <summary>该语句的列（无结果集 / 未加载时为空）。</summary>
    public List<QueryColumn> Columns { get; } = new();

    /// <summary>该语句自己的续拉游标（逐语句 fetch(statement_index, page_token)）。</summary>
    public string PageToken { get; set; } = string.Empty;

    /// <summary>该语句服务端还有下一批。</summary>
    public bool HasMore { get; set; }

    /// <summary>首批是否已取回（未取回的 SELECT 选中后才拉首批）。</summary>
    public bool Fetched { get; set; }

    /// <summary>该语句的网格状态。</summary>
    public QueryViewState State { get; set; } = QueryViewState.Empty;

    /// <summary>占位页说明（无结果集摘要 / 结果未返回 / 拉取中）。</summary>
    public string Note { get; set; } = string.Empty;

    /// <summary>服务端声明该结果集只读。</summary>
    public bool ReadOnly { get; set; }
}

/// <summary>
/// #190 多语句执行的状态：逐语句槽位 + 选中项；失败路径只列到失败语句（ADR 0006 原子失败契约）。
/// </summary>
internal sealed class QueryMultiState
{
    public QueryMultiState(List<StatementSlot> slots, int selected, uint? failedIndex, int completed, string failureLine)
    {
        Slots = slots;
        Selected = selected;
        FailedIndex = failedIndex;
        Completed = completed;
        FailureLine = failureLine;
    }

    /// <summary>按脚本顺序的语句槽位（失败路径长度 = failed_index + 1）。</summary>
    public List<StatementSlot> Slots { get; }

    /// <summary>当前选中的语句（0 基；成功默认 = 最后一条 SELECT，全写 = 0，失败 = 失败条目）。</summary>
    public int Selected { get; set; }

    /// <summary>失败语句序号（null = 成功路径）。</summary>
    public uint? FailedIndex { get; }

    /// <summary>失败前已成功执行的条数。</summary>
    public int Completed { get; }

    /// <summary>失败状态行「前 N 条已执行，第 M 条失败：&lt;错误&gt;」。</summary>
    public string FailureLine { get; }

    /// <summary>是否原子失败路径。</summary>
    public bool IsFailure => FailedIndex is not null;
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
/// <item><description><see cref="ResetResults"/>：关闭结果区（#189）——清网格态、回到隐藏，SQL 与脏标记保留。</description></item>
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

    /// <summary>已取到的行（单元格已格式化成文字，滚动时不再重算）。多语句时指向选中槽位的行列表。</summary>
    public List<string[]> Rows { get; private set; } = new();

    /// <summary>结果集的列（空 = 没有结果集）。多语句时指向选中槽位的列列表。</summary>
    public List<QueryColumn> Columns { get; private set; } = new();

    /// <summary>服务端还有下一批。</summary>
    public bool HasMore { get; private set; }

    /// <summary>是否已收到过任何 Execute 响应（成功 / 失败 / 取消 / 无结果集，#189 结果区显隐的驱动）。</summary>
    /// <remarks>
    /// 新标签为 false（结果区隐藏、编辑器占满）；首批 / 失败 / 取消任一到达即 true 并保持
    /// （幂等显示：重新执行不会把已显示的结果区再藏回去），只有 <see cref="ResetResults"/>
    /// （关闭结果区）清回 false。与 Linux 的 show_results 由 show_started / show_batch /
    /// mark_cancelled / show_failure / show_limited 驱动一一对应。
    /// </remarks>
    public bool HasExecuteResponse { get; private set; }

    /// <summary>结果区里的未提交脏行数（#189 关闭前丢弃确认的判据）。</summary>
    /// <remarks>
    /// 本版查询结果网格只读（<see cref="QueryViewState.ReadOnly"/> 恒为只读终态，没有网格内编辑），
    /// 因此恒为 0；判据保留以与 Linux 对齐——将来引入结果集编辑时确认弹窗自动生效。
    /// </remarks>
    public int DirtyRowCount => 0;

    /// <summary>结果集段数（`Execute` 的 <c>statements</c> 条数；单语句为 1，多语句为条数，#190）。</summary>
    public int StatementCount { get; private set; }

    /// <summary>#190 多语句状态（null = 单语句：不显示切换行，行为与既有版本完全一致）。</summary>
    public QueryMultiState? Multi { get; private set; }

    /// <summary>
    /// #190 全部语句槽都无后续批次 = 整个查询交付完毕（#187 的 complete 语义）。
    /// 多语句里首结果集读完不算完：其余 SELECT 槽还等着续拉。
    /// </summary>
    public bool MultiAllComplete
    {
        get
        {
            if (Multi is not { } multi)
            {
                return false;
            }

            foreach (var slot in multi.Slots)
            {
                if (slot.HasMore)
                {
                    return false;
                }
            }

            return true;
        }
    }

    /// <summary>
    /// #190 首批装配后：默认选中的 SELECT 若不是随 Execute 首批到达的那条，
    /// 置加载态并返回 true，调用方随即发起该语句的首批 fetch（镜像 Linux
    /// apply_selected_slot 的「选中即首拉」）。
    /// </summary>
    /// <returns>是否需要调用方发起首批拉取。</returns>
    public bool BeginDefaultSlotFetch()
    {
        if (Multi is not { IsFailure: false } multi)
        {
            return false;
        }

        var slot = multi.Slots[multi.Selected];
        if (slot.Fetched || !slot.HasMore)
        {
            return false;
        }

        slot.State = QueryViewState.Loading;
        slot.Note = "正在拉取该语句的结果…";
        View = QueryViewState.Loading;
        Note = slot.Note;
        return true;
    }

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
    public string CountsText => View switch
    {
        QueryViewState.NoResultSet => $"影响 {RowsAffected} 行",
        QueryViewState.Undelivered => "已执行 · 结果未返回",
        _ => HasMore
            ? $"? 行 / 已取 {Rows.Count} 行（还有更多）"
            : $"{Rows.Count} 行 / 已取 {Rows.Count} 行",
    };

    /// <summary>状态栏行 2 左格：`SQL: <摘要>`。</summary>
    public string SqlSummaryLine => $"SQL: {QuerySql.Summary(ExecutedSql)}";

    /// <summary>结果区上方那一句（五态互斥；行保留在 Error / Cancelled 下）。</summary>
    /// <remarks>取下一批在飞时先说「正在取下一批…」：那是 Loading 的那一半语义，不能只靠按钮变取消暗示。</remarks>
    public string StateText => Fetching && View == QueryViewState.ReadOnly
        ? $"正在取下一批…（已取 {Rows.Count} 行，稍后追加上去）"
        : View switch
    {
        QueryViewState.Loading => Executing ? "正在执行…（等待首批数据）" : "正在拉取该语句的结果…",
        QueryViewState.Empty => EmptyText(),
        QueryViewState.Error => Multi is { IsFailure: true } multi ? multi.FailureLine : $"查询失败：{Headline}（{Code}）",
        QueryViewState.Cancelled => $"已取消：保留已取的 {Rows.Count} 行（游标已释放，可重新执行）",
        QueryViewState.NoResultSet => "已执行 · 无结果集",
        QueryViewState.Undelivered => "已执行 · 结果未返回",
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

            // #190：多语句每段都可切换查看，退役「只显示第 1 段」的旧说明。
            var segments = $"结果集 {StatementCount} 段";
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
        Rows = new List<string[]>();
        Columns = new List<QueryColumn>();
        Multi = null;
        HasMore = false;
        StatementCount = 0;
        HasResultSet = true;
        ReadOnlyResult = false;
        RowsAffected = 0;
        Headline = null;
        Code = null;
        Note = null;
        QueryId = string.Empty;
        _resultsClosed = false;
        View = QueryViewState.Loading;
        Executing = true;
    }

    /// <summary>首批到达：语句摘要 + 首批行（句柄只是搬运工，结论全在 <see cref="ApplyFirstBatchCore"/>）。</summary>
    /// <param name="handle">刚拿到的句柄（读 statements / 首批行 / has_more / query_id）。</param>
    /// <param name="readOnly">服务端声明的只读标志。</param>
    public void ApplyFirstBatch(Sqarvo.Core.Clients.QueryHandle handle, bool readOnly)
    {
        ArgumentNullException.ThrowIfNull(handle);

        if (StatementSwitcher.IsMulti(handle.Statements))
        {
            ApplyMultiFirstBatch(handle);
            return;
        }

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
        HasExecuteResponse = true;
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

    /// <summary>
    /// #190 多语句首批：逐语句建槽位。首批行属于**第一条** SELECT（随 Execute 到达）；
    /// 其余 SELECT 未加载、选中后才拉首批；写语句直接进「无结果集」占位态。
    /// 默认选中 = 已成功语句中最后一条 SELECT；全写脚本选第 1 条（与 Linux default_selection 同口径）。
    /// </summary>
    private void ApplyMultiFirstBatch(Sqarvo.Core.Clients.QueryHandle handle)
    {
        QueryId = handle.QueryId;
        HasExecuteResponse = true;
        StatementCount = handle.Statements.Count;
        Executing = false;
        Headline = null;
        Code = null;

        int? firstResult = null;
        for (var i = 0; i < handle.Statements.Count; i++)
        {
            if (handle.Statements[i].HasResultSet)
            {
                firstResult = i;
                break;
            }
        }

        var slots = new List<StatementSlot>(handle.Statements.Count);
        for (var i = 0; i < handle.Statements.Count; i++)
        {
            var summary = handle.Statements[i];
            var slot = new StatementSlot { Summary = summary };
            if (!summary.HasResultSet)
            {
                slot.State = QueryViewState.NoResultSet;
                slot.Fetched = true;
                slot.Note = $"语句 {i + 1} 影响 {(long)summary.RowsAffected} 行（无结果集）";
            }
            else if (i == firstResult)
            {
                if (summary.ResultMeta is not null)
                {
                    foreach (var column in summary.ResultMeta.Columns)
                    {
                        slot.Columns.Add(QueryColumn.From(column));
                    }

                    slot.ReadOnly = summary.ResultMeta.ReadOnly;
                }

                AppendRows(slot.Rows, handle.InitialRows);
                slot.PageToken = handle.PageToken;
                slot.HasMore = handle.HasMore;
                slot.Fetched = true;
                slot.State = slot.Rows.Count == 0 && !slot.HasMore
                    ? QueryViewState.Empty
                    : QueryViewState.ReadOnly;
            }
            else
            {
                // 未加载的 SELECT：选中后拉首批（HasMore 先置 true 驱动那一次 fetch）。
                slot.HasMore = true;
            }

            slots.Add(slot);
        }

        var selected = 0;
        if (StatementSwitcher.DefaultSelection(handle.Statements) is uint wanted)
        {
            for (var i = 0; i < handle.Statements.Count; i++)
            {
                if (handle.Statements[i].StatementIndex == wanted)
                {
                    selected = i;
                    break;
                }
            }
        }

        Multi = new QueryMultiState(slots, selected, failedIndex: null, completed: 0, failureLine: string.Empty);
        LoadSlot(slots[selected]);
    }

    /// <summary>把选中槽位的网格状态装载到顶层（行/列共享引用，切换不复制、不丢行）。</summary>
    private void LoadSlot(StatementSlot slot)
    {
        Rows = slot.Rows;
        Columns = slot.Columns;
        HasResultSet = slot.Summary?.HasResultSet ?? false;
        RowsAffected = (long)(slot.Summary?.RowsAffected ?? 0);
        ReadOnlyResult = slot.ReadOnly;
        HasMore = slot.HasMore;
        Note = string.IsNullOrEmpty(slot.Note) ? null : slot.Note;
        View = slot.State;
    }

    /// <summary>把顶层网格状态写回当前选中槽位（切换 / 取消 / 释放游标前调用，与 Linux sync_selected_slot 同口径）。</summary>
    private void SyncSelectedSlot()
    {
        if (Multi is not { } multi)
        {
            return;
        }

        var slot = multi.Slots[multi.Selected];
        slot.State = View;
        slot.HasMore = HasMore;
        slot.Note = Note ?? string.Empty;
    }

    /// <summary>
    /// #190 切换选中语句：写回旧槽位 → 装载新槽位。未加载的 SELECT 返回 true，
    /// 调用方随即发起该语句的首批 fetch（statement_index + 空 page_token）。
    /// </summary>
    /// <param name="index">目标语句（0 基）。</param>
    /// <returns>是否需要调用方发起首批拉取。</returns>
    public bool SelectStatement(int index)
    {
        if (Multi is not { } multi || index < 0 || index >= multi.Slots.Count || index == multi.Selected)
        {
            return false;
        }

        SyncSelectedSlot();
        multi.Selected = index;
        var slot = multi.Slots[index];
        LoadSlot(slot);

        if (!multi.IsFailure && !slot.Fetched && slot.HasMore)
        {
            slot.State = QueryViewState.Loading;
            slot.Note = "正在拉取该语句的结果…";
            View = QueryViewState.Loading;
            Note = slot.Note;
            return true;
        }

        return false;
    }

    /// <summary>#190 切换行条目（从槽位实时派生：行数 / 续拉状态随批次更新；失败路径直接用 Core 规则）。</summary>
    public IReadOnlyList<StatementSwitcherEntry> SwitcherEntries()
    {
        if (Multi is not { } multi)
        {
            return Array.Empty<StatementSwitcherEntry>();
        }

        if (multi.IsFailure)
        {
            return StatementSwitcher.EntriesFromFailure(multi.FailedIndex!.Value);
        }

        var entries = new List<StatementSwitcherEntry>(multi.Slots.Count);
        foreach (var slot in multi.Slots)
        {
            var summary = slot.Summary!;
            if (!summary.HasResultSet)
            {
                entries.Add(new StatementSwitcherEntry(
                    summary.StatementIndex, summary.SqlPreview, HasResultSet: false,
                    StatementSlotState.Affected, RowsAffected: (long)summary.RowsAffected));
            }
            else if (!slot.Fetched)
            {
                entries.Add(new StatementSwitcherEntry(
                    summary.StatementIndex, summary.SqlPreview, HasResultSet: true,
                    StatementSlotState.Unloaded));
            }
            else
            {
                entries.Add(new StatementSwitcherEntry(
                    summary.StatementIndex, summary.SqlPreview, HasResultSet: true,
                    StatementSlotState.Rows, Loaded: slot.Rows.Count, HasMore: slot.HasMore));
            }
        }

        return entries;
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

        if (Multi is { IsFailure: false } multi)
        {
            // #190：批次写回选中槽位（行列表本就共享引用），游标逐语句独立。
            var slot = multi.Slots[multi.Selected];
            slot.PageToken = batch.PageToken;
            slot.HasMore = batch.HasMore;
            slot.Fetched = true;
            slot.State = View;
            slot.Note = string.Empty;
            Note = null;
        }
    }

    /// <summary>取消：行保留，状态转 Cancelled。</summary>
    /// <param name="note">取消原因（可为空）。</param>
    public void MarkCancelled(string? note = null)
    {
        Executing = false;
        Fetching = false;
        HasMore = false;
        Note = note;
        HasExecuteResponse = true;
        View = QueryViewState.Cancelled;

        if (Multi is { IsFailure: false } multi)
        {
            var slot = multi.Slots[multi.Selected];
            slot.State = QueryViewState.Cancelled;
            slot.HasMore = false;
            slot.Note = note ?? string.Empty;
        }
    }

    /// <summary>失败：行保留，错误码与文案都写进状态。</summary>
    /// <param name="headline">主标题。</param>
    /// <param name="code">稳定错误码。</param>
    /// <param name="failedIndex">#190 多语句脚本里失败语句的 0 基序号（trailing metadata；单语句为 null）。</param>
    /// <param name="completed">失败前已成功执行的条数（trailing metadata；缺失时按 failedIndex 推定）。</param>
    /// <param name="detail">错误详情（拼进失败状态行；空则用 headline）。</param>
    public void MarkFailure(string headline, string code, int? failedIndex = null, int? completed = null, string? detail = null)
    {
        Executing = false;
        Fetching = false;
        HasMore = false;
        Headline = headline;
        Code = code;
        HasExecuteResponse = true;

        if (failedIndex is int failed && failed >= 0)
        {
            // #190 多语句原子失败（ADR 0006）：不返回部分结果、metadata 无语句类型——
            // 切换行只列 0..=failed_index；已执行条目「已执行 · 结果未返回」；默认选中失败条目本身；
            // 状态行「前 N 条已执行，第 M 条失败：<错误>」。与 Linux show_multi_failure 逐条一致。
            var done = completed ?? failed;
            var line = StatementSwitcher.FailureStatusLine(done, (uint)failed)
                + "：" + (string.IsNullOrEmpty(detail) ? headline : detail);
            var slots = new List<StatementSlot>(failed + 1);
            for (var i = 0; i <= failed; i++)
            {
                slots.Add(new StatementSlot
                {
                    State = i == failed ? QueryViewState.Error : QueryViewState.Undelivered,
                    Fetched = true,
                    Note = i == failed ? line : StatementSwitcher.UndeliveredNote,
                });
            }

            Multi = new QueryMultiState(slots, failed, (uint)failed, done, line);
            Rows = slots[failed].Rows;
            Columns = slots[failed].Columns;
            HasResultSet = false;
            RowsAffected = 0;
            ReadOnlyResult = false;
            Note = null;
            View = QueryViewState.Error;
            return;
        }

        if (Multi is { IsFailure: false } existing)
        {
            // 多语句续拉失败：只把当前选中条目标失败，切换行与其余条目保留（Linux sync_selected_slot 同口径）。
            var slot = existing.Slots[existing.Selected];
            slot.State = QueryViewState.Error;
            slot.HasMore = false;
            slot.Fetched = true;
            slot.Note = $"{headline}（{code}）";
            Note = null;
            View = QueryViewState.Error;
            return;
        }

        View = QueryViewState.Error;
    }

    /// <summary>游标已释放（切页 / 重新执行 / 关页）：只加说明，不谎称还有更多。</summary>
    /// <param name="reason">释放原因。</param>
    public void MarkCursorReleased(string reason)
    {
        HasMore = false;
        Note = $"游标已释放（{reason}）";

        if (Multi is { } multi)
        {
            var slot = multi.Slots[multi.Selected];
            slot.HasMore = false;
            slot.Note = Note;
        }
    }

    /// <summary>关闭结果区（#189）：清网格态并回到「隐藏」，编辑器占满整页。</summary>
    /// <remarks>
    /// 游标由调用方先走 CloseQuery 幂等释放（完成 / 关闭后句柄已置空，重复调用不再发 RPC）；
    /// SQL 文本、<see cref="ExecutedSql"/> 与 <see cref="Dirty"/> 保留——关闭结果区不清编辑器，
    /// 重新执行仍走同一会话。与 Linux 的 close_results（reset + hide）同口径。
    /// </remarks>
    public void ResetResults()
    {
        Rows = new List<string[]>();
        Columns = new List<QueryColumn>();
        Multi = null;
        HasMore = false;
        StatementCount = 0;
        HasResultSet = false;
        RowsAffected = 0;
        Headline = null;
        Code = null;
        Note = null;
        QueryId = string.Empty;
        HasExecuteResponse = false;
        _resultsClosed = true;
        View = QueryViewState.Empty;
    }

    /// <summary>结果区是否被用户关闭过（空态文案用；重新执行即清）。</summary>
    private bool _resultsClosed;

    private void AppendRows(IReadOnlyList<Row> rows) => AppendRows(Rows, rows);

    private static void AppendRows(List<string[]> target, IReadOnlyList<Row> rows)
    {
        foreach (var row in rows)
        {
            var cells = new string[row.Values.Count];
            for (var index = 0; index < row.Values.Count; index++)
            {
                cells[index] = QueryCellText.Format(row.Values[index]);
            }

            target.Add(cells);
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
        // 关闭结果区后的防御分支：正常路径下此时结果区已隐藏，这句不会被看到。
        if (_resultsClosed)
        {
            return "结果区已关闭：执行查询后会再次显示";
        }

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