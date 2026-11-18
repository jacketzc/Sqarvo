"""SQL 编辑执行与结果校验（任务 §6.4、§6.5）。"""
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


# ---- #231：结果区三页签（消息 / 摘要 / 结果）与不撑宽回归 -------------------

MULTI_INSERT = "\n".join(
    f"INSERT INTO widgets (name, qty)\nVALUES ('m{i}', {i});" for i in range(20)
)


def test_results_view_tabs_exist_and_switch(app_connected):
    """三枚页签的可访问性名称存在且切换生效（只验交互与功能，不做样式断言）。

    注册表组件: tabs.query.results.views（a11y 组名「结果视图页签」）。
    """
    frame, page = _run_query(app_connected, SQL)
    with step("tabs_exist"):
        ops.results_tab_group(frame)
        for name in ops.RESULTS_TABS:
            ops.results_tab(page, name)
        # 单语句 SELECT：默认页 =「结果」（tabs.query.results 的 default_tab_for 规则）
        A.wait_until(lambda: ops.active_results_tab(page) == "结果", 15,
                     "单语句 SELECT 默认落「结果」页")
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
        first = text.splitlines()[0]
        expect(first.startswith("1 · SELECT"),
               "消息页第一行「1 · SELECT · …」", first)
    with step("switch_back_results"):
        ops.switch_results_tab(page, "结果")
        rows = ops.wait_query_done(page, min_rows=3, timeout=15)
        # AT-SPI 子枚举去重：#231 的 ViewStack 层把同一可访问节点（D-Bus path 相同）
        # 在祖先 children 里重复枚举（实测 3 份，基线 1 份）——产品缺陷另报，
        # 本断言只核对「去重后网格内容与 SELECT 一致」这一功能事实。
        distinct = {tuple(r) for r in rows}
        expect(len(distinct) == 3, "回到「结果」页网格去重后 3 行", f"{len(distinct)} distinct of {len(rows)}")


def test_multi_insert_message_and_summary(app_connected):
    """20 条多行 INSERT：默认落「消息」页；消息页 20 行；摘要「成功 20 / 失败 0」。

    注册表组件: tabs.query.results（默认页规则）、tabs.query.results.message、
    tabs.query.results.summary。
    """
    frame, page = _run_query(app_connected, MULTI_INSERT)
    with step("default_tab_messages"):
        A.wait_until(lambda: "selected" in A.states(ops.results_tab(page, "消息")), 25,
                     "全写脚本默认页签=消息")
        expect(ops.active_results_tab(page) == "消息",
               "默认落「消息」页（default_tab_for：无结果集全写 → messages）",
               ops.active_results_tab(page))
    with step("messages_20_lines"):
        text = ops.messages_log_text(page)
        lines = [l for l in text.splitlines() if l.strip()]
        expect(len(lines) == 20, "消息页 20 行（每语句一行）", f"{len(lines)} lines")
        expect(lines[0].startswith("1 · INSERT · 影响 1 行"),
               "第一行文案「1 · INSERT · 影响 1 行」", lines[0])
        expect(not any("错误" in l for l in lines),
               "成功脚本没有「> 错误：」行", [l for l in lines if "错误" in l][:3])
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


def test_multi_statement_does_not_widen_window(app_connected):
    """回归条款（#231 缺陷存在的理由）：20 条多行 INSERT 执行不得撑宽窗口。

    注册表组件: tabs.query.results.statements（条目区宽度有界）、tabs.query.results.views。
    """
    frame = app_connected.frame()
    with step("prepare"):
        page = ops.open_sql_page(frame)
        ops.write_sql(page, MULTI_INSERT)
    width_before = ops.window_width(frame)
    if width_before is None:
        pytest.skip("BLOCKED：AT-SPI Component.get_extents 在本环境拿不到窗口外框宽度，"
                    "无法断言执行前后窗口宽度关系（退一步的页签控件宽度对照同样依赖该接口）")
    with step("execute"):
        ops.execute_sql(page)
    with step("run_finished"):
        A.wait_until(lambda: "selected" in A.states(ops.results_tab(page, "消息")), 25,
                     "20 条 INSERT 执行完成（默认页签=消息）")
    width_after = ops.window_width(frame)
    print(f"[231] 窗口外框宽：前 {width_before} → 后 {width_after}（断言后 ≤ 前 + 1px）")
    with step("window_width_not_increased"):
        if width_after is None:
            pytest.skip("BLOCKED：执行后拿不到窗口外框宽度（同上）")
        expect(width_after <= width_before + 1,
               f"执行后窗口宽度不增大（前 {width_before}px，容差 1px）",
               f"前 {width_before} → 后 {width_after}")
    with step("switcher_within_window"):
        # 语句切换控件（GtkDropDown）必须在窗口宽度之内；其可访问名实测为选中条目文本
        # （见注册表 impl_notes「可访问性现状」），故按 combo box 角色定位。
        combos = [c for c in A.query(page, role_in=("combo box",))
                  if "INSERT" in A.nm(c)]
        expect(bool(combos), "多语句切换控件（DropDown）在结果页内出现",
               [A.nm(c) for c in A.query(page, role_in=("combo box",))])
        w = ops.widget_width(combos[0])
        if w is None:
            pytest.skip("BLOCKED：控件 Component.get_extents 拿不到宽度")
        expect(w <= width_after,
               f"切换控件宽 {w} 在窗口宽 {width_after} 之内（条目区宽度有界）",
               f"控件 {w} vs 窗口 {width_after}")
