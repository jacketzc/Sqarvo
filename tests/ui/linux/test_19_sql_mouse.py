"""Physical SQL context-menu regression with a populated workspace."""
import pytest

import adapter as A
import ops
from test_03_sql import SQL, _execute_selection, _run_query

pytestmark = pytest.mark.sql


def test_execute_selection_after_existing_query_tabs(ui_instance):
    """注册表组件: tabs.query.actions, tabs.query.editor。"""
    inst = ui_instance("populated_query_tabs").start(autoconnect=True)
    frame = inst.wait_autoconnected()
    with ops.step("prepare_existing_query_tabs"):
        for _ in range(2):
            _, page = _run_query(inst, SQL)
            ops.wait_query_done(page, min_rows=3)
        _, page = _run_query(inst, "SELECT * FROM no_such_table_xyz")
        A.wait_until(lambda: any("失败" in text for text in ops.sql_summary(frame) + ops.labels(page)),
                     15, "第三个查询标签显示错误")
        ops.expect(len(ops.query_tab_names(frame)) == 3, "三个前置查询标签保持打开")
    _execute_selection(inst, "mouse-menu")
