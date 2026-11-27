"""新增行单元格编辑器的默认状态与取消。"""

from contextlib import closing
import sqlite3

import pytest
from gi.repository import Atspi, GLib

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.sql


def test_insert_editor_omit_unchecked_by_default(ui_instance):
    """data_page.editor、status_bar.row1.ops.add：双击打开、输入可用、取消保留草稿。"""
    inst = ui_instance("insert_editor")
    with closing(sqlite3.connect(inst.db)) as db, db:
        db.execute("CREATE TABLE insert_probe (id INTEGER PRIMARY KEY, note TEXT DEFAULT 'probe_default')")

    with step("open_table_and_add_row"):
        frame = ops.open_table_for_qa(inst, "insert_probe")
        add = A.find_one(frame, role_in=("button",), name="新增行：在当前表末尾新增一行")
        apply = A.find_one(frame, role_in=("button",), name="应用：提交所有脏行的变更")
        A.wait_enabled(add, 10)
        A.click(add)
        A.wait_enabled(apply, 10)

    def draft_cell():
        return A.find_one(frame, role_in=("table cell",), name="默认：'probe_default'", timeout=10)

    omit_name = "省略该列（交给数据库默认值 / 自增）"

    def open_editor():
        previous = None

        def settled_cell():
            nonlocal previous
            cell = draft_cell()
            bounds = Atspi.Component.get_extents(cell, Atspi.CoordType.WINDOW)
            location = (bounds.x, bounds.y, bounds.width, bounds.height)
            settled = (location == previous and "active" in A.states(frame)
                       and "showing" in A.states(cell) and bounds.width and bounds.height)
            previous = location
            return cell if settled else None

        # GtkPopover 挂在单元格标签下，GTK 通过焦点事件暴露输入框。
        focused = []
        event_name = "object:state-changed:focused"
        listener = Atspi.EventListener.new(
            lambda event: focused.append(event.source) if event.detail1 else None
        )
        expect(listener.register(event_name), "订阅 AT-SPI 焦点事件")
        context = GLib.MainContext.default()

        def focused_entry():
            while context.pending():
                context.iteration(False)
            return next((source for source in reversed(focused)
                         if A.role(source) in ("entry", "text")
                         and "focused" in A.states(source)), None)

        try:
            # 从原生单元格内侧发送双击，确保命中挂有编辑手势的标签。
            A.mouse_click(A.wait_until(settled_cell, 8, "单元格指针目标已稳定"),
                          count=2, vertical_fraction=0.8)
            field = A.wait_until(focused_entry, 8, "新行编辑器的输入框取得焦点")
        finally:
            listener.deregister(event_name)

        editor = field.get_parent()
        A.find_one(editor, role_in=("label",), name_contains="新行 · note · ", timeout=8)
        omit = A.find_one(editor, role_in=("check box",), name=omit_name, timeout=8)
        expect("checked" not in A.states(omit), "新行编辑器默认未勾选省略", A.states(omit))
        expect({"sensitive", "editable"} <= A.states(field), "输入框可用且可编辑", A.states(field))
        expect(A.text_value(field) == "", "省略草稿从空输入开始", A.text_value(field))
        return editor, field

    def cancel_editor(editor):
        A.click(A.find_one(editor, role_in=("button",), name="取消"))
        A.wait_gone(lambda: any("showing" in A.states(check) for check in
                                A.query(editor, role_in=("check box",), name=omit_name)),
                    8, "取消后编辑器收起")

    with step("double_click_cell_opens_editor"):
        editor, field = open_editor()
        A.set_text(field, "discard this edit")
        expect(A.text_value(field) == "discard this edit", "输入框接受文本", A.text_value(field))

    with step("cancel_preserves_the_unsubmitted_row"):
        cancel_editor(editor)
        draft_cell()
        A.wait_enabled(apply, 8)
        editor, _ = open_editor()
        cancel_editor(editor)
        with closing(sqlite3.connect(f"{inst.db.as_uri()}?mode=ro", uri=True)) as db:
            rows = db.execute("SELECT note FROM insert_probe").fetchall()
        expect(not rows, "取消编辑保留界面草稿且没有写库", rows)
