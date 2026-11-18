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
