using Sqarvo.Services.V1;

namespace Sqarvo.Core.Model;

/// <summary>
/// Grid display state of one statement inside the statement switcher row (#190).
/// Mirrors the Linux <c>SlotState</c> (<c>frontend/linux/src/statements.rs</c>).
/// </summary>
public enum StatementSlotState
{
    /// <summary>A result set whose rows are loaded (count + whether more batches remain).</summary>
    Rows,

    /// <summary>A result-set statement whose first batch has not been fetched yet.</summary>
    Unloaded,

    /// <summary>A write statement (no result set); carries the affected row count.</summary>
    Affected,

    /// <summary>
    /// Executed successfully but its result was never delivered: the atomic-failure contract
    /// (ADR 0006) does not keep cursors of earlier statements when a later one fails.
    /// </summary>
    Undelivered,

    /// <summary>The statement that failed.</summary>
    Failed,
}

/// <summary>
/// One entry of the statement switcher row (#190). Mirrors the Linux <c>SwitcherEntry</c>.
/// </summary>
/// <param name="Index">Zero-based statement index inside the script.</param>
/// <param name="Preview">SQL preview of the statement (empty on the failure path).</param>
/// <param name="HasResultSet">Whether the statement produced a result set.</param>
/// <param name="State">Grid display state of the statement.</param>
/// <param name="Loaded">Rows loaded so far (only meaningful for <see cref="StatementSlotState.Rows"/>).</param>
/// <param name="HasMore">Whether more batches remain (only meaningful for <see cref="StatementSlotState.Rows"/>).</param>
/// <param name="RowsAffected">Affected rows (only meaningful for <see cref="StatementSlotState.Affected"/>).</param>
public sealed record StatementSwitcherEntry(
    uint Index,
    string Preview,
    bool HasResultSet,
    StatementSlotState State,
    long Loaded = 0,
    bool HasMore = false,
    long RowsAffected = 0);

/// <summary>
/// Pure rules of the multi-statement switcher row (#190): default selection, entry assembly for the
/// success and the atomic-failure paths, and the entry labels. Kept free of UI types so the rules are
/// unit-testable on any platform; the wording matches the Linux implementation
/// (<c>frontend/linux/src/statements.rs</c>) exactly — cross-platform behaviour is a hard requirement.
/// </summary>
/// <remarks>
/// <para>
/// Failure-path contract limit (ADR 0006): <c>Execute</c> fails atomically — the error response carries
/// no statement summaries and the backend keeps no cursor of the already-executed statements. Entries
/// can therefore only be listed up to the failed statement, earlier entries are labelled
/// 「已执行 · 结果未返回」, and the default selection is the failed statement itself (there is no way to
/// know which of the executed statements was a SELECT). Improving this needs a backend contract change.
/// </para>
/// </remarks>
public static class StatementSwitcher
{
    /// <summary>Label of an executed-but-undelivered entry (failure path).</summary>
    public const string UndeliveredLabel = "已执行 · 结果未返回";

    /// <summary>Placeholder note of an executed-but-undelivered statement (failure path).</summary>
    public const string UndeliveredNote = "该语句已执行成功，但脚本失败时后端未保留其结果游标";

    /// <summary>Whether the script is multi-statement (the switcher row only exists then).</summary>
    /// <param name="statements">Statement summaries of the execution.</param>
    /// <returns><see langword="true"/> when more than one statement was executed.</returns>
    public static bool IsMulti(IReadOnlyList<StatementSummary> statements) => statements.Count > 1;

    /// <summary>
    /// Default selection on success: the last statement that produced a result set;
    /// <see langword="null"/> for an all-write script (the UI then falls back to the first entry).
    /// </summary>
    /// <param name="statements">Statement summaries of the execution.</param>
    /// <returns>The zero-based statement index, or <see langword="null"/>.</returns>
    public static uint? DefaultSelection(IReadOnlyList<StatementSummary> statements)
    {
        for (var i = statements.Count - 1; i >= 0; i--)
        {
            if (statements[i].HasResultSet)
            {
                return statements[i].StatementIndex;
            }
        }

        return null;
    }

    /// <summary>
    /// Builds the switcher entries of a successful multi-statement execution. The first result-set
    /// statement already carries its first batch (it arrived with <c>Execute</c>); the other result-set
    /// statements start <see cref="StatementSlotState.Unloaded"/> and are fetched when selected.
    /// </summary>
    /// <param name="statements">Statement summaries of the execution.</param>
    /// <param name="firstLoaded">Rows of the first batch (belongs to the first result-set statement).</param>
    /// <param name="firstHasMore">Whether that first result set has more batches.</param>
    /// <returns>One entry per statement, in script order.</returns>
    public static List<StatementSwitcherEntry> EntriesFromStatements(
        IReadOnlyList<StatementSummary> statements,
        long firstLoaded,
        bool firstHasMore)
    {
        int? firstResult = null;
        for (var i = 0; i < statements.Count; i++)
        {
            if (statements[i].HasResultSet)
            {
                firstResult = i;
                break;
            }
        }

        var entries = new List<StatementSwitcherEntry>(statements.Count);
        for (var i = 0; i < statements.Count; i++)
        {
            var summary = statements[i];
            if (!summary.HasResultSet)
            {
                entries.Add(new StatementSwitcherEntry(
                    summary.StatementIndex,
                    summary.SqlPreview,
                    HasResultSet: false,
                    StatementSlotState.Affected,
                    RowsAffected: (long)summary.RowsAffected));
            }
            else if (i == firstResult)
            {
                entries.Add(new StatementSwitcherEntry(
                    summary.StatementIndex,
                    summary.SqlPreview,
                    HasResultSet: true,
                    StatementSlotState.Rows,
                    Loaded: firstLoaded,
                    HasMore: firstHasMore));
            }
            else
            {
                entries.Add(new StatementSwitcherEntry(
                    summary.StatementIndex,
                    summary.SqlPreview,
                    HasResultSet: true,
                    StatementSlotState.Unloaded));
            }
        }

        return entries;
    }

    /// <summary>
    /// Builds the switcher entries of a failed multi-statement execution: entries stop at the failed
    /// statement (the atomic-failure response carries no summaries of later statements), earlier
    /// statements are <see cref="StatementSlotState.Undelivered"/>.
    /// </summary>
    /// <param name="failedIndex">Zero-based index of the failed statement (trailing metadata).</param>
    /// <returns>Entries for statements 0..=failedIndex.</returns>
    public static List<StatementSwitcherEntry> EntriesFromFailure(uint failedIndex)
    {
        var entries = new List<StatementSwitcherEntry>((int)failedIndex + 1);
        for (uint i = 0; i <= failedIndex; i++)
        {
            entries.Add(new StatementSwitcherEntry(
                i,
                Preview: string.Empty,
                HasResultSet: false,
                i == failedIndex ? StatementSlotState.Failed : StatementSlotState.Undelivered));
        }

        return entries;
    }

    /// <summary>
    /// Status-line prefix of a failed multi-statement execution: 「前 N-1 条已执行，第 N 条失败」.
    /// The caller appends 「：&lt;错误文案&gt;」.
    /// </summary>
    /// <param name="completed">Number of statements executed successfully before the failure.</param>
    /// <param name="failedIndex">Zero-based index of the failed statement.</param>
    /// <returns>The prefix without the error detail.</returns>
    public static string FailureStatusLine(int completed, uint failedIndex) =>
        $"前 {completed} 条已执行，第 {failedIndex + 1} 条失败";

    /// <summary>
    /// The kind word of an entry: the first keyword of the SQL preview, upper-cased, at most 12
    /// characters; falls back to 「结果集」/「写语句」 when the preview is empty (failure path).
    /// </summary>
    /// <param name="entry">The switcher entry.</param>
    /// <returns>The kind word used in the entry label.</returns>
    public static string KindLabel(StatementSwitcherEntry entry)
    {
        var word = entry.Preview
            .Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
            .FirstOrDefault() ?? string.Empty;
        if (word.Length == 0)
        {
            return entry.HasResultSet ? "结果集" : "写语句";
        }

        // Linux: keyword.chars().take(12).collect::<String>().to_uppercase() — truncate, then upper-case.
        return new string(word.Take(12).ToArray()).ToUpperInvariant();
    }

    /// <summary>
    /// The visible label of an entry: 「序号 · 类型词 · 行数/影响行数/状态」 (wording matches Linux).
    /// </summary>
    /// <param name="entry">The switcher entry.</param>
    /// <returns>The entry label.</returns>
    public static string EntryLabel(StatementSwitcherEntry entry)
    {
        var n = entry.Index + 1;
        var kind = KindLabel(entry);
        return entry.State switch
        {
            StatementSlotState.Rows => entry.HasMore
                ? $"{n} · {kind} · {entry.Loaded} 行…"
                : $"{n} · {kind} · {entry.Loaded} 行",
            StatementSlotState.Unloaded => $"{n} · {kind} · 未加载",
            StatementSlotState.Affected => $"{n} · {kind} · 影响 {entry.RowsAffected} 行",
            StatementSlotState.Undelivered => $"{n} · {UndeliveredLabel}",
            StatementSlotState.Failed => $"{n} · 失败",
            _ => string.Empty,
        };
    }

    /// <summary>
    /// Tooltip of an entry: the full SQL preview (the label only carries the first keyword) plus a
    /// state description. Wording matches Linux <c>entry_tooltip</c>.
    /// </summary>
    /// <param name="entry">The switcher entry.</param>
    /// <returns>「语句 N：&lt;预览&gt;\n&lt;状态说明&gt;」 (no preview line when the preview is empty).</returns>
    public static string EntryTooltip(StatementSwitcherEntry entry)
    {
        var state = entry.State switch
        {
            StatementSlotState.Rows => entry.HasMore
                ? $"已加载 {entry.Loaded} 行，还有后续批次（选中后滚动续拉）"
                : $"已加载 {entry.Loaded} 行",
            StatementSlotState.Unloaded => "尚未加载：选中后拉取该语句的结果",
            StatementSlotState.Affected => $"无结果集，影响 {entry.RowsAffected} 行",
            StatementSlotState.Undelivered => UndeliveredNote,
            StatementSlotState.Failed => "该语句执行失败",
            _ => string.Empty,
        };
        return entry.Preview.Length == 0
            ? $"语句 {entry.Index + 1}：{state}"
            : $"语句 {entry.Index + 1}：{entry.Preview}\n{state}";
    }
}
