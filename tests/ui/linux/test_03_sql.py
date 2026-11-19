"""SQL 编辑执行与结果校验（任务 §6.4、§6.5）。"""
import re

import pytest

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.sql

SQL = "SELECT id, name, qty FROM widgets ORDER BY id"


def _run_query(app, sql):
    frame = app.frame()
    with step("open_sql_page"):
        page = ops.open_sql_page(frame)
    with step("type_sql"):
        ops.write_sql(page, sql)
    with step("execute"):
        ops.execute_sql(page)
    return frame, page


def test_execute_sql(app_connected):
    """执行 SELECT：结果网格按行返回，行数正确。

    注册表组件: tabs.query.editor, tabs.query.actions, tabs.query.results.grid。
    """
    frame, page = _run_query(app_connected, SQL)
    with step("verify_result"):
        rows = ops.wait_query_done(page, min_rows=3, timeout=20)
        expect(len(rows) == 3, "3 rows", f"{len(rows)} rows")


def test_query_result_values(app_connected):
    """结果值逐格核对：id/name/qty 与建库夹具一致。

    注册表组件: tabs.query.results.grid。
    """
    frame, page = _run_query(app_connected, SQL)
    with step("verify_values"):
        rows = ops.wait_query_done(page, min_rows=3, timeout=20)
        cells = {c for r in rows for c in r}
        expect({"bolt", "nut", "gear"} <= cells,
               "结果包含 bolt/nut/gear 三行", sorted(cells)[:12])
        expect({"10", "20", "5"} <= cells,
               "qty 值 10/20/5 出现在结果里", sorted(cells)[:12])


def test_error_query_shows_failure_state(app_connected):
    """错误 SQL：进入明确的失败态而不是假装成功（根 AGENTS §3 状态可见）。

    注册表组件: tabs.query.state, tabs.query.actions。
    """
    frame, page = _run_query(app_connected, "SELECT * FROM no_such_table_xyz")
    with step("verify_error_state"):
        def probe():
            texts = (ops.sql_summary(frame) + ops.labels(page)
                     + ops.labels(frame))
            return [t for t in texts
                    if "失败" in t or "错误" in t or "no_such_table" in t.lower()]
        msgs = A.wait_until(probe, 15, "错误提示文案出现在界面")
        expect(bool(msgs), "查询失败时有可见的错误文案", msgs)
        rows = ops.data_rows(page)
        expect(len(rows) == 0, "失败查询不产生结果行", f"{len(rows)} rows")


# ---- #231：结果区定稿版式（消息 / 摘要 / 结果；无独立语句切换控件行） --------
#
# 版式真值：Issue #231 维护者「范围修订（确认 #2）」。行为真值：注册表
# tabs.query.results（默认页规则）、tabs.query.results.views（三页签）、
# tabs.query.results.message（逐语句日志）、tabs.query.results.summary（两列三行）、
# tabs.query.results.result_picker（多结果集折叠）。
# tabs.query.results.statements（语句切换控件）已退役（status=removed）：结果区不再有
# 页签下方的独立控件行，「结果」页签恒为 1 枚，多结果集靠页内下拉折叠。
#
# 只断言交互与功能（可访问性名称、状态流转、行数、文本形态），不做样式断言（ADR 0029）。

MULTI_INSERT = "\n".join(
    f"INSERT INTO widgets (name, qty)\nVALUES ('m{i}', {i});" for i in range(20)
)

# 维护者 4 语句样例的等价脚本：2 条多行 INSERT + 2 条 SELECT（2 个结果集）。
FOUR_STATEMENTS = "\n".join([
    "INSERT INTO widgets (name, qty)\nVALUES ('qa1', 1);",
    "INSERT INTO widgets (name, qty)\nVALUES ('qa2', 2);",
    "SELECT id, name FROM widgets ORDER BY id LIMIT 1;",
    "SELECT id, name FROM widgets ORDER BY id LIMIT 2;",
])

# 全写脚本（没有结果集 → 页签条上不出现「结果」）。
ALL_WRITE = "\n".join([
    "INSERT INTO widgets (name, qty) VALUES ('w1', 1);",
    "INSERT INTO widgets (name, qty) VALUES ('w2', 2);",
])

# 消息页耗时行形态：`> Query Time: 0.004 seconds`（三位小数秒，整数运算出值）。
MESSAGE_TIME_RE = re.compile(r"^> Query Time: \d+\.\d{3} seconds$")
CLOCK_RE = re.compile(r"^\d{2}:\d{2}:\d{2}$")
DURATION_RE = re.compile(r"^\d+\.\d{3} s$")


def _close(frame):
    """用例用完关掉自己开的查询标签（模块共用实例，有会话标签上限 8）。"""
    try:
        ops.close_current_query_tab(frame)
    except Exception:
        pass


def _require_width(acc, what):
    """控件外框宽；AT-SPI 接口拿不到时按 BLOCKED skip（不把缺失当通过）。"""
    width = ops.widget_width(acc)
    if width is None:
        pytest.skip(f"BLOCKED：AT-SPI Component.get_extents 在本环境拿不到{what}宽度")
        raise RuntimeError("unreachable")  # pytest.skip 已抛出；仅为类型收窄
    return int(width)


def test_results_view_tabs_exist_and_switch(app_connected):
    """三枚页签存在、可切；「结果」页回到网格时行数与 SELECT 一致；N=1 无结果集下拉。

    注册表组件: tabs.query.results.views（a11y 组名「结果视图页签」）、
    tabs.query.results.message、tabs.query.results.summary、
    tabs.query.results.result_picker（单个结果集时不出现）。
    """
    frame, page = _run_query(app_connected, SQL)
    with step("tabs_exist"):
        ops.results_tab_group(frame)
        for name in ops.RESULTS_TABS:
            ops.results_tab(page, name)
        # 单语句 SELECT：默认页 =「结果」（tabs.query.results 的 default_tab_for 规则）
        A.wait_until(lambda: ops.active_results_tab(page) == "结果", 15,
                     "单语句 SELECT 默认落「结果」页")
    with step("single_result_set_has_no_picker"):
        combos = [A.nm(c) for c in A.query(page, role_in=("combo box",))]
        expect(not any(n.startswith("结果 ") for n in combos),
               "只有 1 个结果集时没有「结果 N」下拉", combos)
    with step("switch_summary"):
        ops.switch_results_tab(page, "摘要")
        A.wait_until(lambda: "已执行语句" in ops.summary_pairs(page), 15,
                     "摘要页键值对出现在可访问树")
        pairs = ops.summary_pairs(page)
        expect(pairs.get("成功") == "1" and pairs.get("失败") == "0",
               "摘要：成功 1 / 失败 0（单语句 SELECT）", pairs)
    with step("switch_messages"):
        ops.switch_results_tab(page, "消息")
        text = ops.messages_log_text(page)
        lines = [l for l in text.splitlines() if l.strip()]
        expect(lines[0].strip() == SQL,
               "消息页首段是完整 SQL（不再是「1 · SELECT · …」摘要行）", lines[0])
        expect(any(l.strip() == "> OK" for l in lines),
               "查询类语句的结论行是「> OK」", lines[:4])
    with step("switch_back_results"):
        ops.switch_results_tab(page, "结果")
        rows = ops.wait_query_done(page, min_rows=3, timeout=15)
        # AT-SPI 子树唯一枚举：换原生 gtk::Stack 后每个节点恰好 1 份（不再有
        # AdwViewStack 时代的 3 份重复），故按去重后行数断言功能事实。
        distinct = {tuple(r) for r in rows}
        expect(len(distinct) == 3, "回到「结果」页网格去重后 3 行",
               f"{len(distinct)} distinct of {len(rows)}")


def test_multi_insert_message_and_summary(app_connected):
    """20 条多行 INSERT：默认落「消息」；消息 20 段（逐段结论+耗时）；摘要 20 · 20 · 0。

    注册表组件: tabs.query.results（默认页规则）、tabs.query.results.message、
    tabs.query.results.summary、tabs.query.results.views（全写脚本无「结果」页签）。
    """
    frame, page = _run_query(app_connected, MULTI_INSERT)
    try:
        with step("default_tab_messages"):
            A.wait_until(lambda: "selected" in A.states(ops.results_tab(page, "消息")), 30,
                         "全写脚本默认页签=消息")
            expect(ops.active_results_tab(page) == "消息",
                   "默认落「消息」页（default_tab_for：无结果集全写 → messages）",
                   ops.active_results_tab(page))
            tabs = [A.nm(t) for t in A.query(page, role_in=("page tab",))]
            expect(tabs == ["消息", "摘要"],
                   "全写脚本页签只有「消息」「摘要」（没有结果集就没有「结果」页签）",
                   tabs)
        with step("messages_20_blocks"):
            text = ops.messages_log_text(page)
            blocks = text.split("\n\n")
            expect(len(blocks) == 20, "消息页 20 段（语句之间空一行）",
                   f"{len(blocks)} blocks")
            lines = text.splitlines()
            affected = [l.strip() for l in lines if l.strip() == "> Affected rows: 1"]
            times = [l.strip() for l in lines if l.strip().startswith("> Query Time:")]
            expect(len(affected) == 20,
                   "20 条写语句各一行「> Affected rows: 1」", len(affected))
            expect(len(times) == 20 and all(MESSAGE_TIME_RE.match(t) for t in times),
                   "20 行「> Query Time: N.NNN seconds」（三位小数秒）", times[:3])
            expect(not any("> Error:" in l for l in lines),
                   "成功脚本没有「> Error:」行",
                   [l for l in lines if "> Error:" in l][:3])
        with step("summary_counts"):
            ops.switch_results_tab(page, "摘要")
            A.wait_until(lambda: "已执行语句" in ops.summary_pairs(page), 15,
                         "摘要六项出现在可访问树")
            pairs = ops.summary_pairs(page)
            expect(pairs.get("已执行语句") == "20", "已执行语句 = 20", pairs)
            expect(pairs.get("成功") == "20", "成功 = 20", pairs)
            expect(pairs.get("失败") == "0", "失败 = 0", pairs)
            expect(pairs.get("结束时间") not in (None, "—"),
                   "结束时间已写入（终态到达）", pairs)
            expect(pairs.get("总耗时", "").endswith(" s"),
                   "总耗时为「N.NNN s」格式", pairs)
            notes = [t for t in ops.labels(page) if "客户端计时" in t]
            expect(bool(notes), "摘要页底部有计时口径行", notes)
    finally:
        _close(frame)


def test_multi_statement_does_not_widen_window(app_connected):
    """回归条款（#231 缺陷存在的理由）：20 条多行 INSERT 执行不得撑宽窗口。

    注册表组件: tabs.query.results.views（页签在说明行内、不新增行）、
    tabs.query.results.message（长 SQL 在只读日志里换行，宽度不顶窗口）、
    tabs.query.results.statements（v0.2.3.4 退役：结果页内不应再有独立切换控件）。
    """
    frame = app_connected.frame()
    with step("prepare"):
        page = ops.open_sql_page(frame)
        ops.write_sql(page, MULTI_INSERT)
    try:
        before = _require_width(frame, "窗口执行前")
        with step("execute"):
            ops.execute_sql(page)
        with step("run_finished"):
            A.wait_until(lambda: "selected" in A.states(ops.results_tab(page, "消息")), 30,
                         "20 条 INSERT 执行完成（默认页签=消息）")
        width_after = ops.window_width(frame)
        print(f"[231] 窗口外框宽：前 {before} → 后 {width_after}（断言后 ≤ 前 + 1px）")
        with step("window_width_not_increased"):
            after = _require_width(frame, "窗口执行后")
            expect(after <= before + 1,
                   f"执行后窗口宽度不增大（前 {before}px，容差 1px）",
                   f"前 {before} → 后 {after}")
        with step("tabs_within_window"):
            # 页签组是结果区里唯一的语句导航控件（独立切换控件已退役）：
            # 它必须在窗口宽度之内。
            group_width = _require_width(ops.results_tab_group(frame), "页签组")
            expect(group_width <= before,
                   f"页签组宽 {group_width} 在窗口宽 {before} 之内",
                   f"页签组 {group_width} vs 窗口 {before}")
        with step("no_statement_switcher"):
            combos = [A.nm(c) for c in A.query(page, role_in=("combo box",))]
            expect(not any("INSERT" in n for n in combos),
                   "语句切换控件已退役：结果页里没有列出语句的独立下拉",
                   combos)
    finally:
        _close(frame)


# ---- #231 定稿版式：消息页逐语句形态 / 摘要两列 / 全写无「结果」/ 结果集下拉 ----


def test_message_page_format(app_connected):
    """4 语句脚本：消息页逐条语句「完整 SQL + 结论行 + 耗时行」，语句间空一行。

    注册表组件: tabs.query.results.message。
    """
    frame, page = _run_query(app_connected, FOUR_STATEMENTS)
    try:
        with step("default_tab_results"):
            A.wait_until(lambda: ops.active_results_tab(page) == "结果", 20,
                         "有结果集的脚本默认落「结果」页")
        with step("messages_blocks"):
            ops.switch_results_tab(page, "消息")
            A.wait_until(lambda: "> Query" in ops.messages_log_text(page), 20,
                         "消息页出现逐语句结论/耗时行")
            text = ops.messages_log_text(page)
            lines = text.splitlines()
            blocks = text.split("\n\n")
            print(f"[231] 消息页 {len(blocks)} 段 / {len(lines)} 行；首段：\n{blocks[0]}")
            expect(len(blocks) == 4, "4 条语句 → 消息页 4 段（语句之间空一行）",
                   f"{len(blocks)} blocks")
            expect(text.count("\n\n") == 3, "语句段之间正好 3 处空行", text.count("\n\n"))
            affected = [l.strip() for l in lines if l.strip() == "> Affected rows: 1"]
            expect(len(affected) == 2, "两条 INSERT 各一行「> Affected rows: 1」",
                   len(affected))
            oks = [l.strip() for l in lines if l.strip() == "> OK"]
            expect(len(oks) == 2, "两条 SELECT 各一行「> OK」", len(oks))
            times = [l.strip() for l in lines if l.strip().startswith("> Query Time:")]
            expect(len(times) == 4, "4 条语句各一行「> Query Time:」", times)
            expect(all(MESSAGE_TIME_RE.match(t) for t in times),
                   "耗时形如「> Query Time: 0.001 seconds」（三位小数）", times)
        with step("full_sql_text"):
            expect(blocks[0].startswith("INSERT INTO widgets (name, qty)"),
                   "首段是完整 SQL（不是「序号 · 类型词」摘要，也没有「（预览）」前缀）",
                   blocks[0].splitlines()[:2])
            expect("（预览）" not in text,
                   "切分条数与后端一致，没有退回后端预览", blocks[0].splitlines()[:2])
    finally:
        _close(frame)


def test_summary_two_columns(app_connected):
    """4 语句脚本：摘要页两列三行（左 已执行/成功/失败，右 开始/结束/总耗时）。

    注册表组件: tabs.query.results.summary。
    """
    frame, page = _run_query(app_connected, FOUR_STATEMENTS)
    try:
        with step("open_summary"):
            A.wait_until(lambda: ops.results_tab_group(frame) is not None, 20,
                         "结果区出现")
            ops.switch_results_tab(page, "摘要")
            A.wait_until(lambda: "已执行语句" in ops.summary_pairs(page), 20,
                         "摘要六项出现在可访问树")
            pairs = ops.summary_pairs(page)
            print(f"[231] 摘要取值：{pairs}")
            expect(set(pairs) >= set(ops.SUMMARY_KEYS),
                   "六个键齐全（已执行语句/成功/失败/开始时间/结束时间/总耗时）",
                   pairs)
            expect((pairs.get("已执行语句"), pairs.get("成功"), pairs.get("失败"))
                   == ("4", "4", "0"),
                   "计数 4 / 4 / 0", (pairs.get("已执行语句"), pairs.get("成功"),
                                     pairs.get("失败")))
        with step("time_values"):
            expect(CLOCK_RE.match(pairs.get("开始时间", "")),
                   "开始时间是 HH:MM:SS", pairs.get("开始时间"))
            expect(CLOCK_RE.match(pairs.get("结束时间", "")),
                   "结束时间是 HH:MM:SS", pairs.get("结束时间"))
            expect(DURATION_RE.match(pairs.get("总耗时", "")),
                   "总耗时是「N.NNN s」", pairs.get("总耗时"))
        with step("two_columns"):
            cols = ops.summary_columns(page)
            left = {cols.get(k) for k in ("已执行语句", "成功", "失败")}
            right = {cols.get(k) for k in ("开始时间", "结束时间", "总耗时")}
            expect(len(left) == 1 and None not in left,
                   "左列三行（已执行语句/成功/失败）在同一个列容器里", cols)
            expect(len(right) == 1 and None not in right,
                   "右列三行（开始时间/结束时间/总耗时）在同一个列容器里", cols)
            expect(left != right, "计数与时间分成两列（不是一列六行）",
                   (left, right))
    finally:
        _close(frame)


def test_no_results_tab_when_all_writes(app_connected):
    """全写脚本：没有结果集 → 页签只有「消息」「摘要」，默认页「消息」。

    注册表组件: tabs.query.results.views。
    """
    frame, page = _run_query(app_connected, ALL_WRITE)
    try:
        with step("wait_terminal"):
            A.wait_until(lambda: "selected" in A.states(ops.results_tab(page, "消息")), 25,
                         "全写脚本默认页签=消息")
        with step("no_results_tab"):
            tabs = [A.nm(t) for t in A.query(page, role_in=("page tab",))]
            print(f"[231] 全写脚本可见页签：{tabs}")
            expect(tabs == ["消息", "摘要"],
                   "页签只有「消息」「摘要」两枚", tabs)
            expect(ops.active_results_tab(page) == "消息",
                   "默认页是「消息」", ops.active_results_tab(page))
        with step("messages_have_blocks"):
            text = ops.messages_log_text(page)
            expect(text.count("> Affected rows: 1") == 2,
                   "两条写语句各一行「> Affected rows: 1」", text.splitlines())
    finally:
        _close(frame)


def test_result_picker_folds_multiple_result_sets(app_connected):
    """2 个结果集：结果页内一枚下拉「结果 1 / 结果 2」，切到结果 2 后网格行数变。

    注册表组件: tabs.query.results.result_picker（a11y 名称「结果集」、条目
    「结果 {number}」、N<2 不出现、切换换入换出各自网格状态）。
    """
    frame, page = _run_query(app_connected, FOUR_STATEMENTS)
    try:
        with step("picker_present"):
            A.wait_until(lambda: ops.active_results_tab(page) == "结果", 20,
                         "有结果集的脚本默认落「结果」页")
            picker = ops.result_picker(page)
            expect(A.nm(picker).startswith("结果 1"),
                   "结果集下拉当前值是「结果 1」", A.nm(picker))
            labels = ops.result_picker_labels(page)
            expect(labels == ["结果 1", "结果 2"],
                   "下拉条目「结果 1 / 结果 2」（结果集按脚本顺序编号）", labels)
            print(f"[231] 结果集下拉：name={A.nm(picker)!r} 条目={labels}")
        with step("first_result_one_row"):
            rows = ops.wait_query_done(page, min_rows=1, timeout=20)
            expect(len(rows) == 1,
                   "结果 1（LIMIT 1）网格 1 行", f"{len(rows)} rows: {rows[:3]}")
        with step("switch_to_result_two"):
            ops.pick_result(page, "结果 2")
            A.wait_until(lambda: len(ops.data_rows(page)) == 2, 20,
                         "切到结果 2 后网格 2 行")
            rows2 = ops.data_rows(page)
            expect(len(rows2) == 2,
                   "结果 2（LIMIT 2）网格 2 行", f"{len(rows2)} rows: {rows2}")
            expect(A.nm(ops.result_picker(page)).strip() == "结果 2",
                   "下拉选中「结果 2」", A.nm(ops.result_picker(page)))
            print(f"[231] 切到「结果 2」：网格 {len(rows2)} 行 {rows2}")
        with step("switch_back_keeps_state"):
            ops.pick_result(page, "结果 1")
            A.wait_until(lambda: ops.data_rows(page) and len(ops.data_rows(page)) == 1, 20,
                         "切回结果 1 仍只有 1 行（各自网格状态保留）")
            expect([tuple(r) for r in ops.data_rows(page)] == [tuple(rows[0])],
                   "切回后仍是结果 1 自己的那一行", ops.data_rows(page))
    finally:
        _close(frame)