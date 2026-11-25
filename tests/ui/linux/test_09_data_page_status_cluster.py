"""状态簇（data_page.toolbar.status_cluster）的提示收纳：普通提示只进圆点 tooltip。"""

import pytest

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.sql


def test_insert_hint_goes_into_status_cluster_tooltip(ui_instance):
    """新增行的普通提示不占可见过程消息位，只写进状态圆点的 tooltip。

    注册表组件: data_page.toolbar.status_cluster（note_action 收进圆点「最近一次操作」）。
    只断言交互、可访问性名称与文案是否存在，不做样式/截图断言（ADR 0029）。
    """
    with step("open_table"):
        frame = ops.open_table_for_qa(ui_instance("insert_status_tip"), "widgets")

    with step("add_row_via_status_bar"):
        adds = A.query(frame, role_in=("button",), name="新增行：在当前表末尾新增一行")
        expect(len(adds) == 1, "状态栏「新增行」按钮在场且唯一", [A.nm(x) for x in adds])
        A.click(adds[0])

    with step("hint_collected_into_status_dot"):
        # 圆点的可访问名就是 tooltip（sync_status_dot 用同一句更新 Label）。
        A.wait_until(
            lambda: [nm for nm in (A.nm(x) for x in A.query(frame, role_in=("button",)))
                     if "最近一次操作" in nm and "新行：" in nm],
            10, "状态圆点 tooltip 出现「最近一次操作：新行：…」")
        # 可见标签里不再显示这条提示——它已收进状态簇的圆点。
        visible = [text for text in ops.labels(frame) if "双击单元格填值" in text]
        expect(not visible, "可见过程消息位不再显示新增行提示", visible)
