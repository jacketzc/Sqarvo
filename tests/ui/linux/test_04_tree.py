"""数据库对象树导航（任务 §6.6）：连接 → 库 → 种类分组 → 表。"""
import pytest

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.sql


def test_tree_navigation(app_connected):
    """数据库对象树导航：连接 → 库 → 种类分组 → 表，含收起往返。

    注册表组件: object_tree.tree, object_tree.tree.row.conn,
    object_tree.tree.row.expander, object_tree.tree.row.catalog,
    object_tree.tree.row.group, object_tree.tree.row.object。
    """
    frame = app_connected.frame()

    with step("expand_connection"):
        conn_row = ops.find_row(frame, "uitest.db")
        expect(conn_row is not None, "连接行存在", ops.tree_rows(frame)[:4])
        if ops.find_row(frame, "main") is None:
            ops.expand_tree_row(conn_row)
        main = A.wait_until(lambda: ops.find_row(frame, "main"), 10,
                            "展开连接后出现 main 库行")
        expect(main is not None, "main 库行出现", None)

    with step("expand_schema"):
        if ops.find_row(frame, "Tables") is None:
            ops.expand_tree_row(main)
        tables = A.wait_until(lambda: ops.find_row(frame, "Tables"), 10,
                              "展开 main 后出现 Tables 分组行")
        expect(tables is not None, "Tables 分组行出现", ops.tree_rows(frame)[:6])

    with step("expand_tables_group"):
        ops.expand_tree_row(tables)
        widgets = A.wait_until(lambda: ops.find_row(frame, "widgets"), 15,
                               "展开 Tables 后出现 widgets 表行")
        expect(widgets is not None, "widgets 表行出现", ops.tree_rows(frame)[:8])

    with step("collapse_round_trip"):
        # 同一行再点一次 = 收起（tree.rs：已连接行点击为展开/收起切换）。
        ops.expand_tree_row(conn_row)
        # 收起后 main 行不再 showing（树逐层实现，子行销毁）。
        gone = A.wait_until(
            lambda: not _row_showing(frame, "uitest.db", "main"), 10,
            "收起连接后子行消失")
        expect(gone, "收起连接后 main 子行不可见", None)
        ops.expand_tree_row(conn_row)
        A.wait_until(lambda: ops.find_row(frame, "main"), 10, "重新展开恢复 main 行")


def _row_showing(frame, *must):
    row = ops.find_row(frame, *must)
    if row is None:
        return False
    return "showing" in A.states(row)
