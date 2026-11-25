"""Objects 重新进入 / F5 刷新与 Inspector 行数隔离的功能回归。

注册表组件: objects_page.table, objects_page.status, inspector.object.info,
inspector.pane_switch.info。只验证交互与状态，不做样式 / 截图断言。
"""
from __future__ import annotations

import time

import adapter as A
import ops
import pytest
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.sql

EXPAND_ARGS = ("--qa-tree-expand", "main", "--qa-tree-select-group", "tables")


def _start(tag: str, *extra: str) -> UiInstance:
    inst = UiInstance(tag).start(autoconnect=True, extra_args=(*EXPAND_ARGS, *extra))
    ops.wait_connected(inst.frame(), "uitest.db", timeout=40)
    return inst


def _submit_count(inst: UiInstance, label: str) -> int:
    return inst.log_text().count(f"[qa.metadata] submit {label} id=")


def _wait_submit(inst: UiInstance, label: str, minimum: int) -> int:
    def probe():
        count = _submit_count(inst, label)
        return count if count >= minimum else None
    return A.wait_until(probe, 20, f"{label} 提交次数达到 {minimum}")


def _objects_labels(frame) -> list[str]:
    # 打开查询标签后会有多个 tree table（结果网格）：按 Objects 的列头定位。
    for table in A.query(frame, role_in=("tree table",)):
        labels = [A.nm(x) for x in A.query(table, role_in=("label",)) if A.nm(x)]
        if "表名" in labels:
            return labels
    return []


def _tab_action(frame, action: str) -> None:
    A.window_action(frame, action)

def _select_connection_row(inst: UiInstance) -> None:
    """选中连接行，避免新建查询标签继承库上下文（SQLite 不支持切库）。"""
    import gi
    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi
    frame = inst.frame()
    tree = A.query(frame, role_in=("list",))[0]
    rows = A.children(tree)
    for index, row in enumerate(rows):
        labels = [A.nm(x) for x in A.query(row, role_in=("label",), max_depth=6)]
        if any("uitest.db" in label for label in labels):
            expect(Atspi.Selection.select_child(tree, index), "选中连接行", False)
            return
    raise AssertionError("树里找不到连接行")


def _sql(inst: UiInstance, statement: str) -> None:
    frame = inst.frame()
    with step(f"sql:{statement[:24]}"):
        page = ops.open_sql_page(frame)
        ops.write_sql(page, statement)
        before = inst.log_text().count("> Affected rows:")
        ops.execute_sql(page)
        A.wait_until(
            lambda: inst.log_text().count("> Affected rows:") > before,
            25, "SQL 执行完成（消息页给出结果）")


def _back_to_objects(frame) -> None:
    _tab_action(frame, "win.tab-1")
    A.wait_until(lambda: any(label == "Objects" for label in ops.labels(frame)),
                 10, "回到 Objects 页")


def test_objects_reloads_on_reentry_and_f5():
    """SQL 页建 / 删 / 改表后切回 Objects 可见；F5 强制重列。"""
    inst = _start("obj_reload")
    try:
        with step("initial_list"):
            A.wait_until(lambda: "widgets" in _objects_labels(inst.frame()), 15,
                         "Objects 初始列表包含 widgets")
            before = _submit_count(inst, "nodes")

        with step("create_and_reenter"):
            _select_connection_row(inst)
            _sql(inst, "CREATE TABLE fresh_table(id INTEGER)")
            _back_to_objects(inst.frame())
            _wait_submit(inst, "nodes", before + 1)
            A.wait_until(lambda: "fresh_table" in _objects_labels(inst.frame()), 15,
                         "切回 Objects 后看到新建的 fresh_table")

        with step("rename_and_reenter"):
            count = _submit_count(inst, "nodes")
            _sql(inst, "ALTER TABLE fresh_table RENAME TO renamed_table")
            _back_to_objects(inst.frame())
            _wait_submit(inst, "nodes", count + 1)
            labels = A.wait_until(
                lambda: _objects_labels(inst.frame()) if "renamed_table" in _objects_labels(inst.frame()) else None,
                15, "重命名后新表名出现")
            expect("fresh_table" not in labels, "旧表名已消失", labels)

        with step("drop_and_reenter"):
            count = _submit_count(inst, "nodes")
            _sql(inst, "DROP TABLE renamed_table")
            _back_to_objects(inst.frame())
            _wait_submit(inst, "nodes", count + 1)
            A.wait_until(
                lambda: "renamed_table" not in _objects_labels(inst.frame()), 15,
                "DROP 后表从列表消失")

        with step("f5_force_reload"):
            count = _submit_count(inst, "nodes")
            _tab_action(inst.frame(), "win.refresh")
            _wait_submit(inst, "nodes", count + 1)
            expect("widgets" in _objects_labels(inst.frame()),
                   "F5 后列表仍展示当前作用域", _objects_labels(inst.frame()))
    finally:
        inst.dispose()


def test_objects_selection_does_not_reload():
    """同一作用域内选中一行不重复发 ListNodes。"""
    inst = _start("obj_select")
    try:
        with step("load_list"):
            A.wait_until(lambda: "widgets" in _objects_labels(inst.frame()), 15,
                         "Objects 列表就绪")
            before = _submit_count(inst, "nodes")
        with step("select_row"):
            _tab_action(inst.frame(), "win.tab-1")
            time.sleep(0.2)
            # 用 Objects 页的语义单选（不切换作用域）。
            A.wait_until(
                lambda: any("widgets" in label for label in _objects_labels(inst.frame())),
                10, "widgets 行在位")
            after = _submit_count(inst, "nodes")
            expect(after == before, "选中行不触发列表重新查询", (before, after))
    finally:
        inst.dispose()


def _pane_button(frame, name: str):
    buttons = A.query(frame, role_in=("toggle button",), name=name)
    expect(bool(buttons), f"子页按钮 {name} 存在", len(buttons))
    return buttons[0]


def test_loaded_rows_do_not_leak_between_tables():
    """A 有数据标签行数，B 没有数据标签时表信息页不继承 A 的行数。"""
    inst = _start("rows_leak", "--qa-tree-open", "widgets")
    try:
        with step("a_has_loaded_rows"):
            A.wait_until(
                lambda: any(label == "3 行" for label in [A.nm(x) for x in A.query(inst.frame(), role_in=("label",))]),
                15, "A 表信息页展示已加载 3 行")
        with step("switch_to_b_info"):
            # B（widgets_empty）没有打开数据标签，树预览默认 DDL；切到表信息页。
            _select_tree_row = None
            frame = inst.frame()
            lst = A.query(frame, role_in=("list",))[0]
            for index, row in enumerate(A.children(lst)):
                labels = [A.nm(x) for x in A.query(row, role_in=("label",), max_depth=6)]
                if any(name == "widgets_empty" for name in labels):
                    import gi
                    gi.require_version("Atspi", "2.0")
                    from gi.repository import Atspi
                    expect(Atspi.Selection.select_child(lst, index), "选中 widgets_empty", False)
                    break
            A.click(_pane_button(inst.frame(), "表信息"))
            A.wait_until(
                lambda: "widgets_empty" in [A.nm(x) for x in A.query(inst.frame(), role_in=("label",))],
                15, "B 表信息页已加载")
            labels = [A.nm(x) for x in A.query(inst.frame(), role_in=("label",))]
            expect(not any(label == "3 行" for label in labels),
                   "B 的表信息页不展示 A 的已加载行数", labels[-12:])
    finally:
        inst.dispose()


def test_column_info_f5_reloads_that_column_not_another_object():
    """列信息页 F5 重查父表 GetColumns，不发别的对象的 DDL 请求。"""
    inst = _start("column_f5", "--qa-tree-open", "widgets", "--qa-column-info", "id")
    try:
        with step("column_info_open"):
            A.wait_until(lambda: _submit_count(inst, "columns") >= 1, 20,
                         "列信息页已在位（父表 GetColumns 已读到）")
            columns = _submit_count(inst, "columns")
            ddl = _submit_count(inst, "ddl")

        with step("f5_reloads_column"):
            _tab_action(inst.frame(), "win.refresh")
            _wait_submit(inst, "columns", columns + 1)
            expect(_submit_count(inst, "ddl") == ddl,
                   "列页刷新不应改刷其它对象的 DDL", (ddl, _submit_count(inst, "ddl")))
    finally:
        inst.dispose()
