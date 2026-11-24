"""新行编辑器（data_page.editor）的三态入口：省略默认不勾、提示行高度稳定。"""

import pytest

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.sql

# 双击打开单元格弹窗需要输入合成（坐标点击 / 按键），Wayland 下 atspi_error（见 README
# 「已知限制」1），应用也没有驱动 open_insert_editor 的 QA flag。因此弹窗本身的交互
# 无法在本端自动化；三态初始值与提示行选择的规则由纯函数单测覆盖：
#   frontend/linux/src/table_data.rs::insert_editor_opens_with_omit_unchecked_from_every_mode
#   frontend/linux/src/table_data.rs::insert_editor_initial_text_and_null_reflect_draft
pytestmark = pytest.mark.skip(
    reason="Wayland 禁止输入合成，无法双击打开单元格弹窗；初始态由 table_data 纯函数单测覆盖"
)


def test_insert_editor_omit_unchecked_by_default(app_connected):
    """新增行后双击某列，省略框默认不勾、输入框可用；提示行不随勾选变高。

    注册表组件: data_page.editor、status_bar.row1.ops.add。
    """
    frame = app_connected.frame()

    with step("open_table_and_add_row"):
        conn_row = ops.find_row(frame, "uitest.db")
        expect(conn_row is not None, "连接行存在", ops.tree_rows(frame)[:4])
        if ops.find_row(frame, "main") is None:
            ops.expand_tree_row(conn_row)
        main = A.wait_until(lambda: ops.find_row(frame, "main"), 10, "main 库行出现")
        if ops.find_row(frame, "Tables") is None:
            ops.expand_tree_row(main)
        tables = A.wait_until(lambda: ops.find_row(frame, "Tables"), 10, "Tables 分组出现")
        ops.expand_tree_row(tables)
        widgets = A.wait_until(lambda: ops.find_row(frame, "widgets"), 15, "widgets 表行出现")
        actions = A.action_names(widgets)
        A.click(widgets, actions.index("activate") if "activate" in actions else 0)
        A.wait_until(
            lambda: A.query(frame, role_in=("button",), name="开始事务"),
            15, "表数据页工具条出现")

        adds = A.query(frame, role_in=("button",), name="新增行：在当前表末尾新增一行")
        expect(len(adds) == 1, "状态栏「新增行」按钮在场且唯一", [A.nm(x) for x in adds])
        A.click(adds[0])

    with step("double_click_cell_opens_editor"):
        # 双击只能通过坐标 / 输入合成触发，Wayland 下不可用（已知限制 1）。
        pytest.fail("需要双击打开编辑弹窗，Wayland 无法合成输入")
