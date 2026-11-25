"""Objects 作用域 / 刷新与 Inspector 行数身份的功能回归。

注册表组件: objects_page.table, objects_page.status, inspector.object.info,
inspector.pane_switch.info。只验证交互与状态，不做样式 / 截图断言。
"""
from __future__ import annotations

import sqlite3
import time

import adapter as A
import ops
import pytest
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.sql

EXPAND_ARGS = ("--qa-tree-expand", "main", "--qa-tree-select-group", "tables")


def _start(tag: str, *extra: str, expand: bool = True) -> UiInstance:
    args = (*EXPAND_ARGS, *extra) if expand else extra
    inst = UiInstance(tag).start(autoconnect=True, extra_args=args)
    ops.wait_connected(inst.frame(), "uitest.db", timeout=40)
    return inst


def _submit_count(inst: UiInstance, label: str) -> int:
    return inst.log_text().count(f"[qa.metadata] submit {label} id=")


def _wait_submit(inst: UiInstance, label: str, minimum: int) -> int:
    def probe():
        count = _submit_count(inst, label)
        return count if count >= minimum else None
    return A.wait_until(probe, 20, f"{label} 提交次数达到 {minimum}")


def _objects_grid(frame):
    for table in A.query(frame, role_in=("tree table",)):
        labels = [A.nm(x) for x in A.query(table, role_in=("label",)) if A.nm(x)]
        if "表名" in labels:
            return table
    return None


def _objects_labels(frame) -> list[str]:
    grid = _objects_grid(frame)
    if grid is None:
        return []
    return [A.nm(x) for x in A.query(grid, role_in=("label",)) if A.nm(x)]


def _status_texts(frame) -> list[str]:
    return [A.nm(x) for x in A.query(frame, role_in=("label",)) if A.nm(x)]


def _wait_list_loaded(inst: UiInstance) -> None:
    """等列表真正加载完成，而不是 reload 刚开始清空列表的瞬间。"""
    def probe():
        if _objects_grid(inst.frame()) is None:
            return None
        texts = _status_texts(inst.frame())
        if not any(text.startswith("共 ") for text in texts):
            return None
        if any("正在读取" in text or "读取失败" in text for text in texts):
            return None
        return texts
    A.wait_until(probe, 20, "Objects 列表加载完成（状态为最终计数）")


def _tab_action(frame, action: str) -> None:
    A.window_action(frame, action)


def _sql_tab_roundtrip(inst: UiInstance) -> None:
    """开一个查询标签再切回 Objects（不执行 SQL，避免 SQLite 切库限制）。"""
    ops.open_sql_page(inst.frame())
    _tab_action(inst.frame(), "win.tab-1")
    _wait_list_loaded(inst)


def _external_ddl(inst: UiInstance, statement: str) -> None:
    con = sqlite3.connect(inst.db)
    try:
        con.execute(statement)
        con.commit()
    finally:
        con.close()


def test_objects_list_refreshes_on_reentry_and_f5():
    """外部 DDL 改结构后切回 Objects 见最新列表；DROP 后确认最终加载成功再断言。"""
    inst = _start("obj_reload")
    try:
        with step("initial_list"):
            _wait_list_loaded(inst)
            expect("widgets" in _objects_labels(inst.frame()),
                   "初始列表含 widgets", _objects_labels(inst.frame()))
            before = _submit_count(inst, "objects.nodes")

        with step("create_and_reenter"):
            _external_ddl(inst, "CREATE TABLE fresh_table(id INTEGER)")
            _sql_tab_roundtrip(inst)
            _wait_submit(inst, "objects.nodes", before + 1)
            expect("fresh_table" in _objects_labels(inst.frame()),
                   "切回 Objects 后看到新建表", _objects_labels(inst.frame()))

        with step("rename_and_reenter"):
            count = _submit_count(inst, "objects.nodes")
            _external_ddl(inst, "ALTER TABLE fresh_table RENAME TO renamed_table")
            _sql_tab_roundtrip(inst)
            _wait_submit(inst, "objects.nodes", count + 1)
            labels = _objects_labels(inst.frame())
            expect("renamed_table" in labels and "fresh_table" not in labels,
                   "重命名后新旧表名正确更新", labels)

        with step("drop_and_reenter"):
            count = _submit_count(inst, "objects.nodes")
            _external_ddl(inst, "DROP TABLE renamed_table")
            _sql_tab_roundtrip(inst)
            _wait_submit(inst, "objects.nodes", count + 1)
            _wait_list_loaded(inst)
            labels = _objects_labels(inst.frame())
            expect("renamed_table" not in labels, "DROP 的表消失", labels)
            expect("widgets" in labels and "widgets_empty" in labels,
                   "其它表仍在（列表成功加载而非空态）", labels)

        with step("f5_force_reload"):
            count = _submit_count(inst, "objects.nodes")
            _tab_action(inst.frame(), "win.refresh")
            _wait_submit(inst, "objects.nodes", count + 1)
            _wait_list_loaded(inst)
            expect("widgets" in _objects_labels(inst.frame()),
                   "F5 后列表保持当前作用域", _objects_labels(inst.frame()))
    finally:
        inst.dispose()


def test_objects_row_selection_does_not_reload():
    """走 GtkSingleSelection 的语义选中一行，确认状态变化且列表不重复查询。"""
    inst = _start("obj_select", "--qa-objects-select", "widgets_empty")
    try:
        with step("selected_row"):
            grid = A.wait_until(lambda: _objects_grid(inst.frame()), 20, "Objects 网格")
            def selected():
                for row in A.query(grid, role_in=("table row",)):
                    if "widgets_empty" in A.nm(row) and "selected" in A.states(row):
                        return row
                return None
            A.wait_until(selected, 15, "widgets_empty 行被真正选中")
            before = _submit_count(inst, "objects.nodes")

        with step("no_duplicate_request"):
            time.sleep(0.5)
            expect(_submit_count(inst, "objects.nodes") == before,
                   "行选中不触发 ListNodes", (before, _submit_count(inst, "objects.nodes")))
    finally:
        inst.dispose()


def test_objects_true_empty_state_is_not_restored():
    """选中连接行进入真实空态后，切标签不恢复历史表列表。"""
    inst = _start("obj_empty", "--qa-tree-expand", "main",
                  "--qa-tree-select-connection", "UITestDB", expand=False)
    try:
        with step("true_empty"):
            A.wait_until(lambda: _objects_grid(inst.frame()) is None,
                         15, "Objects 处于真实空态")
            before = _submit_count(inst, "objects.nodes")

        with step("reentry_keeps_empty"):
            ops.open_sql_page(inst.frame())
            _tab_action(inst.frame(), "win.tab-1")
            time.sleep(0.6)
            expect(_objects_grid(inst.frame()) is None,
                   "重新进入 Objects 不恢复历史列表", _status_texts(inst.frame())[-8:])
            expect(_submit_count(inst, "objects.nodes") == before,
                   "空态下重新进入不发送 ListNodes", (before, _submit_count(inst, "objects.nodes")))
    finally:
        inst.dispose()


def test_objects_unsupported_kind_survives_reentry_and_f5():
    """Functions 的「不支持」说明在重入与 F5 后保持，不回退到 Tables 列表。"""
    inst = _start("obj_unsupported")
    try:
        with step("functions"):
            _wait_list_loaded(inst)
            A.click(A.query(inst.frame(), role_in=("button",), name="Functions")[0])
            A.wait_until(
                lambda: any("尚未实现" in text for text in _status_texts(inst.frame())),
                15, "Functions 显示不支持说明")
            before = _submit_count(inst, "objects.nodes")

        with step("reentry_and_f5"):
            ops.open_sql_page(inst.frame())
            _tab_action(inst.frame(), "win.tab-1")
            _tab_action(inst.frame(), "win.refresh")
            time.sleep(0.8)
            expect(_objects_grid(inst.frame()) is None,
                   "不支持页未回退到表列表", _status_texts(inst.frame())[-8:])
            expect(_submit_count(inst, "objects.nodes") == before,
                   "不支持页不重新查列表", (before, _submit_count(inst, "objects.nodes")))
    finally:
        inst.dispose()


def test_loaded_rows_survive_tab_switch_without_metadata_rpc():
    """Objects 页状态事件与重新进入不得清除当前表标签的有效行数。"""
    inst = _start("rows_stable", "--qa-tree-open", "widgets")
    try:
        with step("rows_visible"):
            A.wait_until(
                lambda: any(text == "3 行" for text in _status_texts(inst.frame())),
                15, "表信息页展示已加载 3 行")
            columns = _submit_count(inst, "columns")

        with step("switch_to_objects_and_back"):
            _tab_action(inst.frame(), "win.tab-1")
            time.sleep(0.4)
            _tab_action(inst.frame(), "win.tab-2")
            A.wait_until(
                lambda: any(text == "3 行" for text in _status_texts(inst.frame())),
                15, "回到表标签行数仍为 3 行")
            expect(_submit_count(inst, "columns") >= columns,
                   "回到表标签按无缓存语义重新查询（行数仍有效）",
                   (columns, _submit_count(inst, "columns")))
    finally:
        inst.dispose()


def test_column_info_f5_reloads_that_column_not_another_object():
    """列信息页 F5 重查父表 GetColumns，不发别的对象的 DDL 请求。"""
    inst = _start("column_f5", "--qa-tree-open", "widgets", "--qa-column-info", "id")
    try:
        with step("column_info_open"):
            A.wait_until(lambda: _submit_count(inst, "columns") >= 1, 20,
                         "列信息页已在位")
            columns = _submit_count(inst, "columns")
            ddl = _submit_count(inst, "ddl")

        with step("f5_reloads_column"):
            _tab_action(inst.frame(), "win.refresh")
            _wait_submit(inst, "columns", columns + 1)
            expect(_submit_count(inst, "ddl") == ddl,
                   "列页刷新不应改刷其它对象的 DDL", (ddl, _submit_count(inst, "ddl")))
    finally:
        inst.dispose()
