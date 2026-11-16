using Sqarvo.Core.Model;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Tests;

/// <summary>
/// #190 语句切换行的纯规则测试：与 Linux <c>frontend/linux/src/statements.rs</c> 的 7 个单测
/// 逐条同语义（默认选择 / 全写脚本 / 首批与未加载 / 失败截断 / 首条失败 / 单语句门槛 /
/// 未返回与类型词兜底），另加 tooltip 与类型词截断的口径核对。
/// </summary>
public sealed class StatementSwitcherTests
{
    private static StatementSummary Statement(uint index, bool hasResultSet) => new()
    {
        StatementIndex = index,
        SqlPreview = hasResultSet ? $"SELECT {index}" : $"UPDATE t{index}",
        RowsAffected = hasResultSet ? 0ul : 5ul,
        HasResultSet = hasResultSet,
    };

    [Fact]
    public void DefaultIsLastSelect()
    {
        // SELECT + UPDATE + SELECT：落最后一条 SELECT。
        var script = new[] { Statement(0, true), Statement(1, false), Statement(2, true) };
        Assert.Equal(2u, StatementSwitcher.DefaultSelection(script));
        // 单条 SELECT。
        Assert.Equal(0u, StatementSwitcher.DefaultSelection(new[] { Statement(0, true) }));
        // SELECT 在中间、后面全是写语句：仍落那条 SELECT。
        script = new[] { Statement(0, false), Statement(1, true), Statement(2, false) };
        Assert.Equal(1u, StatementSwitcher.DefaultSelection(script));
    }

    [Fact]
    public void AllWritesHaveNoDefaultSelect()
    {
        var script = new[] { Statement(0, false), Statement(1, false) };
        Assert.Null(StatementSwitcher.DefaultSelection(script));
        Assert.Null(StatementSwitcher.DefaultSelection(Array.Empty<StatementSummary>()));
    }

    [Fact]
    public void EntriesMarkFirstBatchAndUnloaded()
    {
        var script = new[] { Statement(0, true), Statement(1, false), Statement(2, true) };
        var entries = StatementSwitcher.EntriesFromStatements(script, firstLoaded: 3, firstHasMore: true);

        Assert.Equal(StatementSlotState.Rows, entries[0].State);
        Assert.Equal(3, entries[0].Loaded);
        Assert.True(entries[0].HasMore);
        Assert.Equal(StatementSlotState.Affected, entries[1].State);
        Assert.Equal(5, entries[1].RowsAffected);
        Assert.Equal(StatementSlotState.Unloaded, entries[2].State);

        Assert.Equal("1 · SELECT · 3 行…", StatementSwitcher.EntryLabel(entries[0]));
        Assert.Equal("2 · UPDATE · 影响 5 行", StatementSwitcher.EntryLabel(entries[1]));
        Assert.Equal("3 · SELECT · 未加载", StatementSwitcher.EntryLabel(entries[2]));

        // 首批读完（has_more=false）时不带省略号。
        var done = StatementSwitcher.EntriesFromStatements(script, firstLoaded: 3, firstHasMore: false);
        Assert.Equal("1 · SELECT · 3 行", StatementSwitcher.EntryLabel(done[0]));
    }

    [Fact]
    public void FailureEntriesStopAtFailedStatement()
    {
        // 第 3 条（下标 2）失败，前 2 条已执行。
        var entries = StatementSwitcher.EntriesFromFailure(failedIndex: 2);

        Assert.Equal(3, entries.Count);
        Assert.Equal(StatementSlotState.Undelivered, entries[0].State);
        Assert.Equal(StatementSlotState.Undelivered, entries[1].State);
        Assert.Equal(StatementSlotState.Failed, entries[2].State);
        Assert.Equal("3 · 失败", StatementSwitcher.EntryLabel(entries[2]));
        Assert.Equal("前 2 条已执行，第 3 条失败", StatementSwitcher.FailureStatusLine(completed: 2, failedIndex: 2));
    }

    [Fact]
    public void FailureOnFirstStatementListsOnlyIt()
    {
        var entries = StatementSwitcher.EntriesFromFailure(failedIndex: 0);

        Assert.Single(entries);
        Assert.Equal(StatementSlotState.Failed, entries[0].State);
        Assert.Equal("前 0 条已执行，第 1 条失败", StatementSwitcher.FailureStatusLine(completed: 0, failedIndex: 0));
    }

    [Fact]
    public void SingleStatementNeverEntersMultiPath()
    {
        // 单语句（含空摘要）一律走现状：不显示切换行。
        Assert.False(StatementSwitcher.IsMulti(Array.Empty<StatementSummary>()));
        Assert.False(StatementSwitcher.IsMulti(new[] { Statement(0, true) }));
        Assert.False(StatementSwitcher.IsMulti(new[] { Statement(0, false) }));
        Assert.True(StatementSwitcher.IsMulti(new[] { Statement(0, true), Statement(1, false) }));
    }

    [Fact]
    public void UndeliveredAndKindFallbackLabels()
    {
        var entries = StatementSwitcher.EntriesFromFailure(failedIndex: 1);

        Assert.Equal("1 · 已执行 · 结果未返回", StatementSwitcher.EntryLabel(entries[0]));
        // 没有预览时按「结果集 / 写语句」兜底。
        var entry = entries[0] with { HasResultSet = true };
        Assert.Equal("结果集", StatementSwitcher.KindLabel(entry));
        entry = entry with { HasResultSet = false };
        Assert.Equal("写语句", StatementSwitcher.KindLabel(entry));
    }

    [Fact]
    public void KindLabelTruncatesToTwelveCharactersAndUpperCases()
    {
        var entry = new StatementSwitcherEntry(0, "insertorignore into t values (1)", true, StatementSlotState.Unloaded);
        Assert.Equal("INSERTORIGNO", StatementSwitcher.KindLabel(entry));

        // 空白分隔与 Linux split_whitespace 同口径：多个空格 / 制表符都算分隔。
        entry = entry with { Preview = " \t select\t1 " };
        Assert.Equal("SELECT", StatementSwitcher.KindLabel(entry));
    }

    [Fact]
    public void TooltipCarriesFullPreviewAndStateLine()
    {
        var script = new[] { Statement(0, true), Statement(1, false), Statement(2, true) };
        var entries = StatementSwitcher.EntriesFromStatements(script, firstLoaded: 3, firstHasMore: true);

        Assert.Equal("语句 1：SELECT 0\n已加载 3 行，还有后续批次（选中后滚动续拉）", StatementSwitcher.EntryTooltip(entries[0]));
        Assert.Equal("语句 2：UPDATE t1\n无结果集，影响 5 行", StatementSwitcher.EntryTooltip(entries[1]));
        Assert.Equal("语句 3：SELECT 2\n尚未加载：选中后拉取该语句的结果", StatementSwitcher.EntryTooltip(entries[2]));

        var done = StatementSwitcher.EntriesFromStatements(script, firstLoaded: 3, firstHasMore: false);
        Assert.Equal("语句 1：SELECT 0\n已加载 3 行", StatementSwitcher.EntryTooltip(done[0]));

        // 失败场景没有预览：tooltip 只有状态说明。
        var failed = StatementSwitcher.EntriesFromFailure(failedIndex: 1);
        Assert.Equal($"语句 1：{StatementSwitcher.UndeliveredNote}", StatementSwitcher.EntryTooltip(failed[0]));
        Assert.Equal("语句 2：该语句执行失败", StatementSwitcher.EntryTooltip(failed[1]));
    }
}
