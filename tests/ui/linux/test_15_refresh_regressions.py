"""Objects 作用域 / 刷新与 Inspector 行数身份的功能回归。

注册表组件: objects_page.table, objects_page.status, inspector.object.info,
inspector.pane_switch.info。只验证交互与状态，不做样式 / 截图断言。
"""
from __future__ import annotations

import json
import sqlite3

import adapter as A
import ops
import pytest
from gi.repository import Atspi
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.sql

EXPAND_ARGS = ("--qa-tree-expand", "main")


def _start(ui_instance, tag: str, *extra: str) -> UiInstance:
    actions = extra or ("--qa-tree-select-group", "tables")
    inst = ui_instance(tag).start(autoconnect=True, extra_args=(*EXPAND_ARGS, *actions))
    ops.wait_connected(inst.frame(), "uitest.db", timeout=40)
    return inst


def _submit_count(inst: UiInstance, label: str) -> int:
    return inst.log_text().count(f"[qa.metadata] submit {label} id=")


def _expect_submit(inst: UiInstance, label: str, expected: int) -> None:
    actual = _submit_count(inst, label)
    expect(actual == expected, f"{label} 提交次数 = {expected}", actual)


def _metadata_counts(inst: UiInstance) -> dict[str, int]:
    return {kind: _submit_count(inst, kind)
            for kind in ("objects.nodes", "nodes", "ddl", "columns", "primary_key",
                         "inspector.nodes")}


def _select_row(container, row) -> None:
    index = row.get_index_in_parent()
    expect(index >= 0 and Atspi.Selection.select_child(container, index),
           f"选中 {A.nm(row)}", index)


def _select_tree_row(frame, *labels) -> None:
    row = ops.find_row(frame, *labels, timeout=15)
    label = next(label for label in A.query(row, role_in=("label",)) if labels[0] in A.nm(label))
    A.mouse_click(label)
    A.wait_until(lambda: "selected" in A.states(row), 15, f"树行 {labels} 已选中")


def _browse_tables(frame):
    def label():
        return A.find_one(ops.find_row(frame, exact_name="Tables", timeout=15),
                          role_in=("label",), name="Tables")

    # Exercise the command even when the fixed page is already active or empty.
    A.mouse_click(label(), horizontal_fraction=0.2)
    ops.click_tree_until(
        label,
        lambda: A.query(frame, role_in=("grouping",), name="Objects")
        and "selected" in A.states(ops.find_row(frame, exact_name="Tables")),
        what="分类正文单击进入固定 Objects")


def _inspector(frame):
    return A.find_one(frame, role_in=("grouping",), name="信息面板", timeout=15)


def _objects_page(frame):
    return A.find_one(frame, role_in=("grouping",), name="Objects", timeout=15)


def _objects_grid(frame):
    for table in A.query(_objects_page(frame), role_in=("tree table",)):
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
        texts = _status_texts(_objects_page(inst.frame()))
        if not any("个已加载对象" in text for text in texts):
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


def test_objects_reentry_reuses_list_and_f5_refreshes_external_ddl(ui_instance):
    """objects_page.tab, objects_page.table: reentry reuses rows; F5 sees external DDL."""
    inst = _start(ui_instance, "obj_reload")
    _wait_list_loaded(inst)
    frame = inst.frame()
    before = _submit_count(inst, "objects.nodes")
    with step("ordinary_reentry"):
        _sql_tab_roundtrip(inst)
        _expect_submit(inst, "objects.nodes", before)
        expect("widgets" in _objects_labels(frame), "普通重入保留已有对象")

    for sql, present, absent in (
        ("CREATE TABLE fresh_table(id INTEGER)", "fresh_table", None),
        ("ALTER TABLE fresh_table RENAME TO renamed_table", "renamed_table", "fresh_table"),
        ("DROP TABLE renamed_table", None, "renamed_table"),
    ):
        with step(sql):
            _external_ddl(inst, sql)
            before = _submit_count(inst, "objects.nodes")
            _tab_action(frame, "win.refresh")
            _wait_list_loaded(inst)
            labels = _objects_labels(frame)
            expect("widgets" in labels and (present is None or present in labels)
                   and (absent is None or absent not in labels),
                   "显式刷新反映外部 DDL 并保留其它表", labels)
            _expect_submit(inst, "objects.nodes", before + 1)


def test_sql_editor_ddl_refreshes_objects_on_reentry(ui_instance):
    """tabs.query.editor, tabs.query.actions, objects_page.table: 提交 DDL 后重入。"""
    inst = _start(ui_instance, "obj_editor_ddl")
    frame = inst.frame()
    _wait_list_loaded(inst)
    original = {"events", "widgets", "widgets_empty"}
    expect(original <= set(_objects_labels(frame)), "原表已加载", _objects_labels(frame))

    # SQLite 不支持指定请求库。先以连接行为上下文建立 SQL 标签，再恢复 Objects 作用域。
    _select_tree_row(frame, "uitest.db", "已连接")
    ops.open_sql_page(frame)
    _browse_tables(frame)
    _wait_list_loaded(inst)

    for sql, present, absent in (
        ("CREATE TABLE editor_ddl(id INTEGER)", "editor_ddl", None),
        ("ALTER TABLE editor_ddl RENAME TO editor_renamed", "editor_renamed", "editor_ddl"),
        ("DROP TABLE editor_renamed", None, "editor_renamed"),
    ):
        with step(sql):
            _tab_action(frame, "win.tab-2")
            page = ops.active_query_page(frame, ops.query_tab_names(frame)[-1])
            ops.write_sql(page, sql)
            A.wait_until(lambda: "sensitive" in A.states(A.find_one(
                page, role_in=("button",), name="执行")), 15, "SQL 会话就绪")
            before = _metadata_counts(inst)
            ops.execute_sql(page)
            ops.switch_results_tab(page, "消息")
            A.wait_until(lambda: sql in ops.messages_log_text(page)
                         and "> Query Time:" in ops.messages_log_text(page),
                         20, "当前 DDL 的执行结果")
            ops.switch_results_tab(page, "摘要")
            summary = ops.summary_pairs(page)
            expect(summary.get("成功") == "1" and summary.get("失败") == "0",
                   "当前 DDL 成功提交", summary)
            with sqlite3.connect(inst.db.as_uri() + "?mode=ro", uri=True) as con:
                tables = {row[0] for row in con.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'table'")}
            expect((present is None or present in tables)
                   and (absent is None or absent not in tables),
                   "独立连接读到已提交的 DDL", tables)

            _tab_action(frame, "win.tab-1")
            _wait_list_loaded(inst)
            labels = _objects_labels(frame)
            expect(original <= set(labels) and (present is None or present in labels)
                   and (absent is None or absent not in labels),
                   "重入后列表反映 DDL 且保留原表", labels)
            _expect_submit(inst, "objects.nodes", before["objects.nodes"] + 1)
            _expect_submit(inst, "nodes", before["nodes"])

    for action, exists in (("COMMIT", True), ("ROLLBACK", False)):
        name = f"txn_{action.lower()}_ddl"
        for sql, marker, visible in ((f"BEGIN; CREATE TABLE {name}(id INTEGER);", name, False),
                                     (f"{action};", action, exists)):
            with step(sql):
                _tab_action(frame, "win.tab-2")
                page = ops.active_query_page(frame, ops.query_tab_names(frame)[-1])
                ops.write_sql(page, sql)
                before = _submit_count(inst, "objects.nodes")
                ops.execute_sql(page)
                ops.switch_results_tab(page, "消息")
                A.wait_until(lambda: marker in ops.messages_log_text(page)
                             and "> Query Time:" in ops.messages_log_text(page),
                             20, "当前事务 SQL 已完成")
                ops.switch_results_tab(page, "摘要")
                expect(ops.summary_pairs(page).get("失败") == "0", "事务 SQL 无错误")
                with sqlite3.connect(inst.db.as_uri() + "?mode=ro", uri=True) as con:
                    found = con.execute(
                        "SELECT 1 FROM sqlite_master WHERE name = ?", (name,)).fetchone()
                expect(bool(found) == visible, "独立浏览会话只看到已提交结构", found)
                _tab_action(frame, "win.tab-1")
                _wait_list_loaded(inst)
                _expect_submit(inst, "objects.nodes", before + 1)
                expect((name in _objects_labels(frame)) == visible,
                       "Objects 与事务后的真实结构一致", _objects_labels(frame))

    with step("completed_ddl_is_visible_after_later_script_statement_fails"):
        _tab_action(frame, "win.tab-2")
        page = ops.active_query_page(frame, ops.query_tab_names(frame)[-1])
        ops.write_sql(page, "CREATE TABLE partially_applied(id INTEGER); SELECT * FROM missing_table;")
        before = _metadata_counts(inst)
        ops.execute_sql(page)
        ops.switch_results_tab(page, "摘要")
        A.wait_until(lambda: ops.summary_pairs(page).get("失败") == "1",
                     20, "脚本后续语句失败")
        with sqlite3.connect(inst.db.as_uri() + "?mode=ro", uri=True) as con:
            expect(con.execute("SELECT name FROM sqlite_master WHERE name='partially_applied'")
                   .fetchone() == ("partially_applied",), "失败之前的 DDL 已提交")
        _tab_action(frame, "win.tab-1")
        A.find_one(_objects_page(frame), role_in=("label",), name="partially_applied", timeout=15)
        _wait_list_loaded(inst)
        _expect_submit(inst, "objects.nodes", before["objects.nodes"] + 1)
        _expect_submit(inst, "nodes", before["nodes"])


    with step("invalidated_first_page_failure_remains_pending"):
        _tab_action(frame, "win.tab-2")
        page = ops.active_query_page(frame, ops.query_tab_names(frame)[-1])
        sql = "CREATE TABLE pending_refresh_ddl(id INTEGER)"
        ops.write_sql(page, sql)
        ops.execute_sql(page)
        ops.switch_results_tab(page, "消息")
        A.wait_until(lambda: sql in ops.messages_log_text(page)
                     and "> Query Time:" in ops.messages_log_text(page),
                     20, "应用内 DDL 已完成")
        before = _submit_count(inst, "objects.nodes")
        with sqlite3.connect(inst.db) as lock:
            lock.execute("BEGIN EXCLUSIVE")
            _tab_action(frame, "win.tab-1")
            objects = _objects_page(frame)
            A.find_one(objects, role_in=("button",), name="重试", timeout=20)
            _expect_submit(inst, "objects.nodes", before + 1)
            expect(original <= set(_objects_labels(frame)), "首页刷新失败保留原对象")
            rpc = _rpc_counts(inst)
            A.set_text(_objects_filter(frame), "PENDING_REFRESH")
            A.find_one(objects, role_in=("label",),
                       name="已加载的对象中没有匹配项", timeout=10)
            _expect_filter_rpc(inst, rpc, "待刷新首页失败后输入")
        _tab_action(frame, "win.tab-2")
        _tab_action(frame, "win.tab-1")
        _wait_list_loaded(inst)
        _expect_submit(inst, "objects.nodes", before + 2)
        A.find_one(_objects_page(frame), role_in=("label",), name="pending_refresh_ddl", timeout=10)
        expect(A.text_value(_objects_filter(frame)) == "PENDING_REFRESH", "刷新成功仍保留词")
        _tab_action(frame, "win.tab-2")
        _tab_action(frame, "win.tab-1")
        _expect_submit(inst, "objects.nodes", before + 2)


def test_objects_row_selection_does_not_reload(ui_instance):
    """objects_page.table.row: 单选一行，确认 Inspector 跟随且列表不重查。"""
    inst = _start(ui_instance, "obj_select")
    _wait_list_loaded(inst)
    frame = inst.frame()
    before = _metadata_counts(inst)
    with step("select_objects_row"):
        grid = _objects_grid(frame)
        row = next(row for row in A.query(grid, role_in=("table row",))
                   if "widgets_empty" in A.nm(row))
        _select_row(row.get_parent(), row)
        A.wait_until(lambda: "selected" in A.states(row), 15, "Objects 行已选中")
        panel = _inspector(frame)
        A.wait_until(lambda: "widgets_empty" in _status_texts(panel)
                     and any("CREATE TABLE widgets_empty" in A.text_value(control)
                             for control in A.query(panel, role_in=("text",))),
                     15, "Inspector 展示 widgets_empty 的 DDL")
        _expect_submit(inst, "objects.nodes", before["objects.nodes"])
        _expect_submit(inst, "nodes", before["nodes"])
        _expect_submit(inst, "ddl", before["ddl"] + 1)


def _objects_filter(frame):
    return A.find_one(_objects_page(frame), role_in=("entry", "text"), name="筛选对象…")


def _rpc_counts(inst):
    log = inst.log_text()
    return (log.count("[qa.rpc] dispatch"), log.count("[qa.metadata] submit"))


def _expect_filter_rpc(inst, before, phase):
    delta = tuple(after - old for after, old in zip(_rpc_counts(inst), before))
    expect(delta == (0, 0), f"{phase} RPC 与元数据提交增量 = (0, 0)", delta)
    print(f"Objects {phase}: dispatch={delta[0]} metadata={delta[1]}")


def test_objects_filter_keeps_selection_and_scope_local(ui_instance):
    """objects_page, objects_page.filter, objects_page.tab, objects_page.table.row,
    objects_page.table.row.menu, objects_page.table.row.menu.open,
    object_tree.search.box, tabs.strip.list: A08.1, A09.1–4, A09.6, A11.5, A13.5.
    """
    inst = _start(ui_instance, "obj_filter")
    frame = inst.frame()
    _wait_list_loaded(inst)
    entry = _objects_filter(frame)
    original = _objects_labels(frame)
    panel = _inspector(frame)
    navigation = A.find_one(frame, role_in=("grouping",), name="导航")
    selected_tree = ops.find_row(navigation, exact_name="Tables", timeout=10)
    expect("selected" in A.states(selected_tree), "保留当前导航分类")

    def row(name):
        return next((row for row in A.query(_objects_grid(frame), role_in=("table row",))
                     if name in A.nm(row).split()), None)

    with step("input_then_shift_selected_row_without_rpc"):
        before = _rpc_counts(inst)
        A.press_keys("Control_L", "Shift_L", "f")
        A.wait_until(lambda: "focused" in A.states(entry), 5, "Ctrl+Shift+F 聚焦检索")
        A.set_text(entry, "WiDgEtS")
        A.find_one(_objects_page(frame), role_in=("label",), name="显示 2 / 3 个已加载对象", timeout=10)
        _expect_filter_rpc(inst, before, "输入")

        selected = A.wait_until(lambda: row("widgets_empty"), 10, "可选择已过滤对象")
        _select_row(selected.get_parent(), selected)
        A.wait_until(lambda: "widgets_empty" in _status_texts(panel)
                     and any("CREATE TABLE widgets_empty" in A.text_value(control)
                             for control in A.query(panel, role_in=("text",))),
                     15, "主动选择显示准确 DDL")
        before = _rpc_counts(inst)
        A.set_text(entry, "_EmPtY")
        A.find_one(_objects_page(frame), role_in=("label",), name="显示 1 / 3 个已加载对象", timeout=10)
        expect("selected" in A.states(A.wait_until(lambda: row("widgets_empty"), 10, "保留对象")),
               "位置改变后仍选中原对象")
        _expect_filter_rpc(inst, before, "改词与程序化选中位置变化")

        before = _rpc_counts(inst)
        A.set_text(entry, "events")
        remaining = A.wait_until(lambda: row("events"), 10, "当前唯一匹配")
        expect("selected" not in A.states(remaining), "过滤掉原对象后不自动选另一对象")
        _expect_filter_rpc(inst, before, "过滤掉选中对象")
        expect("selected" in A.states(selected_tree), "过滤没有改变树选中")

        before = _rpc_counts(inst)
        A.press_keys("Return")
        expect(A.query(frame, role_in=("grouping",), name="Objects"),
               "检索框 Enter 保持当前工作区")
        _expect_filter_rpc(inst, before, "检索框 Enter")

        before = _rpc_counts(inst)
        A.press_keys("Escape")
        A.wait_until(lambda: _objects_labels(frame) == original, 10, "Esc 恢复原排序")
        expect(A.text_value(entry) == "", "Esc 清空关键词")
        _expect_filter_rpc(inst, before, "Esc 清词")

    with step("three_local_searches_keep_independent_terms"):
        nav_entry = A.find_one(navigation, role_in=("entry",), name="搜索资源…")
        tab_list = A.find_one(frame, role_in=("toggle button",), name="所有标签")
        before = _rpc_counts(inst)
        title = A.nm(frame)
        A.set_text(nav_entry, "only_for_navigation")
        A.find_one(navigation, role_in=("label",), name="已加载的资源中没有匹配项", timeout=10)
        A.set_text(entry, "_EMPTY")
        A.wait_until(lambda: row("widgets_empty") and not row("widgets"), 10, "Objects 独立检索")
        A.click(tab_list)
        search = A.find_one(frame, role_in=("entry", "text"), name="搜索标签…", timeout=8)
        A.set_text(search, "only_for_tabs")
        A.find_one(frame, role_in=("label",), name="没有匹配的标签", timeout=8)
        expect(A.text_value(nav_entry) == "only_for_navigation" and A.text_value(entry) == "_EMPTY",
               "标签过滤不改导航或 Objects 检索词")
        A.set_text(search, "")
        matches = A.find_one(frame, role_in=("list",), name="所有标签", timeout=8)
        A.find_one(matches, role_in=("label",), name_contains="Objects", timeout=8)
        A.press_keys("Return")
        A.wait_gone(lambda: A.query(frame, role_in=("entry", "text"), name="搜索标签…"),
                    8, "标签列表已关闭")
        A.set_text(nav_entry, "")
        A.wait_until(lambda: "selected" in A.states(ops.find_row(navigation, exact_name="Tables")),
                     10, "清除导航筛选后原选择仍在")
        expect(A.text_value(entry) == "_EMPTY" and A.nm(frame) == title,
               "导航清词保留 Objects 范围和检索词")
        _expect_filter_rpc(inst, before, "三个本地搜索入口联合操作")

    with step("keyboard_enters_list_without_opening"):
        A.press_keys("Control_L", "f")
        nav_entry = A.find_one(navigation, role_in=("entry",), name="搜索资源…")
        A.wait_until(lambda: "focused" in A.states(nav_entry), 5, "Ctrl+F 聚焦导航")
        A.press_keys("Control_L", "Shift_L", "f")
        A.wait_until(lambda: "focused" in A.states(entry), 5, "Ctrl+Shift+F 聚焦 Objects")
        A.set_text(entry, "_EMPTY")
        A.wait_until(lambda: row("widgets_empty") and not row("widgets"), 10, "唯一匹配行")
        title = A.nm(frame)
        tabs = len(A.query(frame, role_in=("page tab",)))
        before = _submit_count(inst, "objects.nodes")
        A.press_keys("Down")
        A.wait_until(lambda: any("focused" in A.states(node) for node in A.walk(_objects_grid(frame))),
                     10, "方向键把键盘焦点移入可见对象列表")
        expect(A.nm(frame) == title and len(A.query(frame, role_in=("page tab",))) == tabs,
               "方向键未激活对象")
        _expect_submit(inst, "objects.nodes", before)

    with step("reentry_layout_refresh_and_filtered_activation"):
        A.set_text(entry, "_EMPTY")
        A.wait_until(lambda: row("widgets_empty") and not row("widgets"), 10, "保留词场景")
        before = _submit_count(inst, "objects.nodes")
        _sql_tab_roundtrip(inst)
        _expect_submit(inst, "objects.nodes", before)
        expect(A.text_value(_objects_filter(frame)) == "_EMPTY", "重入保留关键词")
        tabs_before = len(A.query(frame, role_in=("page tab",)))
        _browse_tables(frame)
        _expect_submit(inst, "objects.nodes", before)
        expect(len(A.query(frame, role_in=("page tab",))) == tabs_before
               and len(A.query(frame, role_in=("grouping",), name="Objects")) == 1,
               "重复分类沿用唯一固定页，不创建工作区")
        for button in ("导航", "专注模式", "专注模式", "导航"):
            A.click(A.find_one(frame, role_in=("button", "toggle button"), name=button))
            expect(A.text_value(_objects_filter(frame)) == "_EMPTY", "布局切换保留关键词")
        _tab_action(frame, "win.refresh")
        _wait_list_loaded(inst)
        _expect_submit(inst, "objects.nodes", before + 1)
        expect(A.text_value(_objects_filter(frame)) == "_EMPTY", "刷新保留关键词")
        A.press_keys("Control_L", "w")
        expect(A.text_value(_objects_filter(frame)) == "_EMPTY", "固定页不可关闭")

        cell = A.find_one(_objects_grid(frame), role_in=("label",), name="widgets_empty")
        A.mouse_click(cell, count=2)
        A.wait_until(lambda: "widgets_empty @" in A.nm(frame), 15, "双击打开匹配的对象")
        _tab_action(frame, "win.tab-1")
        cell = A.find_one(_objects_grid(frame), role_in=("label",), name="widgets_empty")
        A.mouse_click(cell, button=3)
        menu = A.wait_until(lambda: next(
            (m for m in A.query(frame, role_in=("menu",))
             if "ctx.open-object" in A.action_names(m)), None), 10, "匹配行的对象菜单")
        A.set_text(_objects_filter(frame), "events")
        A.wait_until(lambda: row("events") and not row("widgets_empty"), 10, "菜单打开期间改词")
        A.window_action(menu, "ctx.open-object")
        A.wait_until(lambda: "widgets_empty @" in A.nm(frame), 15, "右键使用打开菜单时的资源")


def test_objects_filter_manual_pagination_and_retry(ui_instance):
    """objects_page.filter, objects_page.more, objects_page.status: A09.3–5."""
    inst = ui_instance("objects_filter_pages")
    with sqlite3.connect(inst.db) as con:
        for index in range(510):
            con.execute(f"CREATE TABLE a_s5_{index:04d}(id INTEGER)")
        con.execute("CREATE TABLE zz_s5_last(id INTEGER)")
    inst.start(autoconnect=True, extra_args=(*EXPAND_ARGS, "--qa-tree-select-group", "tables"))
    ops.wait_connected(inst.frame(), "uitest.db", timeout=40)
    frame = inst.frame()
    _wait_list_loaded(inst)
    entry = _objects_filter(frame)
    page = _objects_page(frame)
    with step("no_match_can_page_explicitly"):
        before = _rpc_counts(inst)
        A.set_text(entry, "ZZ_S5_LAST")
        A.find_one(page, role_in=("label",), name="已加载的对象中没有匹配项", timeout=10)
        A.find_one(page, role_in=("label",), name="显示 0 / 500 个已加载对象（还有更多）", timeout=10)
        _expect_filter_rpc(inst, before, "第一页无匹配")
        more = A.find_one(page, role_in=("button",), name="加载更多…")

    with step("pagination_failure_preserves_loaded_rows_and_retry"):
        lock = sqlite3.connect(inst.db)
        try:
            lock.execute("BEGIN EXCLUSIVE")
            A.click(more)
            A.find_one(page, role_in=("button",), name="重试", timeout=20)
            expect(_submit_count(inst, "objects.nodes") == 2, "分页失败仅发一次请求")
            before = _rpc_counts(inst)
            A.set_text(entry, "a_s5_0000")
            A.find_one(page, role_in=("label",), name="a_s5_0000")
            A.find_one(page, role_in=("label",), name="显示 1 / 500 个已加载对象（还有更多）", timeout=10)
            A.set_text(entry, "ZZ_S5_LAST")
            A.find_one(page, role_in=("label",), name="已加载的对象中没有匹配项", timeout=10)
            _expect_filter_rpc(inst, before, "分页失败后过滤")
        finally:
            lock.close()

        before = _rpc_counts(inst)
        A.click(A.find_one(page, role_in=("button",), name="重试"))
        A.find_one(page, role_in=("label",), name="zz_s5_last", timeout=20)
        A.find_one(page, role_in=("label",), name="显示 1 / 514 个已加载对象", timeout=10)
        delta = tuple(after - old for after, old in zip(_rpc_counts(inst), before))
        expect(delta == (1, 1), "重试与应用过滤只发绑定的分页请求", delta)
        print(f"Objects 主动重试: dispatch={delta[0]} metadata={delta[1]}")


def test_objects_filter_is_not_restored_at_startup(ui_instance):
    """objects_page.filter, objects_page.tab, objects_page.empty: the term is in-memory only."""
    inst = _start(ui_instance, "objects_filter_restart")
    frame = inst.frame()
    _wait_list_loaded(inst)
    entry = _objects_filter(frame)
    A.set_text(entry, "not_persisted")
    A.find_one(_objects_page(frame), role_in=("label",),
               name="已加载的对象中没有匹配项", timeout=10)
    with step("restart_same_profile_and_browse"):
        ops.close_window(frame)
        expect(inst.proc.wait(timeout=20) == 0, "正常关闭应用")
        inst.stop()
        inst.start()
        frame = inst.frame()
        expect(A.text_value(_objects_filter(frame)) == "", "同一配置重启不恢复检索词")
        saved = ops.find_row(frame, "uitest.db", "未连接", timeout=15)
        A.mouse_click(saved, count=2)
        ops.wait_connected(frame, "uitest.db", timeout=20)
        if ops.find_row(frame, exact_name="main") is None:
            ops.expand_tree_row(ops.find_row(frame, "uitest.db", "已连接", timeout=10))
        main = ops.find_row(frame, exact_name="main", timeout=10)
        if ops.find_row(frame, exact_name="Tables") is None:
            ops.expand_tree_row(main)
        _browse_tables(frame)
        _wait_list_loaded(inst)
        expect(A.text_value(_objects_filter(frame)) == "", "同一范围重新浏览仍未过滤")
        expect("widgets" in _objects_labels(frame), "新进程显示未过滤的列表")


def test_objects_scope_survives_connection_selection(ui_instance):
    """objects_page, objects_page.tab, object_tree.tree: 只选连接保留 Objects 范围。"""
    inst = _start(ui_instance, "obj_empty")
    frame = inst.frame()
    with step("select_connection"):
        _wait_list_loaded(inst)
        before = _metadata_counts(inst)
        _select_tree_row(frame, "uitest.db", "已连接")
        expect("widgets" in _objects_labels(frame), "连接选择保留已有范围")
        _expect_submit(inst, "objects.nodes", before["objects.nodes"])

    with step("reentry_keeps_scope"):
        ops.open_sql_page(frame)
        _tab_action(frame, "win.tab-1")
        _wait_list_loaded(inst)
        expect("widgets" in _objects_labels(frame), "重新进入 Objects 恢复已有范围")
        _expect_submit(inst, "objects.nodes", before["objects.nodes"])
        _expect_submit(inst, "nodes", before["nodes"])


@pytest.mark.parametrize("kind", ["Functions", "Queries"])
def test_objects_unimplemented_kind_hidden_after_refresh(ui_instance, kind):
    """shell.toolbar.functions, shell.toolbar.queries: 未实现入口在刷新后仍隐藏。"""
    inst = _start(ui_instance, f"obj_unsupported_{kind.lower()}")
    frame = inst.frame()
    _wait_list_loaded(inst)
    for action in (None, "win.refresh"):
        if action:
            _tab_action(frame, action)
        expect(not A.query(frame, role_in=("button", "menu item"), name=kind),
               f"隐藏未实现的 {kind}")


def test_loaded_rows_survive_tab_switch(ui_instance):
    """Objects 页状态事件与重新进入不得清除当前表标签的有效行数。"""
    inst = _start(ui_instance, "rows_stable", "--qa-tree-open", "widgets")
    with step("rows_visible"):
        A.wait_until(
            lambda: any(text == "3 行" for text in _status_texts(_inspector(inst.frame()))),
            15, "表信息页展示已加载 3 行")
        columns = _submit_count(inst, "columns")

    with step("switch_to_objects_and_back"):
        frame = inst.frame()
        _browse_tables(frame)
        _wait_list_loaded(inst)
        _tab_action(frame, "win.tab-2")
        A.wait_until(
            lambda: any(text == "3 行" for text in _status_texts(_inspector(inst.frame()))),
            15, "回到表标签行数仍为 3 行")
        _expect_submit(inst, "columns", columns + 1)


def test_column_info_f5_reloads_that_column_not_another_object(ui_instance):
    """列信息页 F5 重查父表 GetColumns，不发别的对象的 DDL 请求。"""
    inst = _start(ui_instance, "column_f5", "--qa-data-table", "widgets", "--qa-column-info", "id")
    frame = inst.frame()
    panel = _inspector(frame)
    with step("column_info_open"):
        A.wait_until(lambda: {"id", "原始类型", "INTEGER"} <= set(_status_texts(panel)),
                     20, "列信息页显示 id 的原始类型")
        before = _metadata_counts(inst)
        _external_ddl(inst, "ALTER TABLE widgets RENAME COLUMN id TO old_id")
        _external_ddl(inst, "ALTER TABLE widgets ADD COLUMN id TEXT")

    with step("f5_reloads_column"):
        _tab_action(frame, "win.refresh")
        A.wait_until(lambda: {"id", "原始类型", "TEXT"} <= set(_status_texts(panel)),
                     20, "列信息刷新后显示同名列的新类型")
        after = _metadata_counts(inst)
        expect(after == {**before, "columns": before["columns"] + 1},
               "列页刷新只发送一次 GetColumns", (before, after))


def test_loaded_rows_do_not_leak_between_tables(ui_instance):
    """inspector.object.info: A 真正翻页时，临时预览的 B 不接收 A 的行数。"""
    inst = ui_instance("rows_identity")
    with sqlite3.connect(inst.db) as con:
        con.executemany("INSERT INTO widgets(name, qty) VALUES (?, ?)",
                        [("paging", 1)] * 502)
    inst.start(autoconnect=True, extra_args=(*EXPAND_ARGS, "--qa-tree-open", "widgets"))
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db", timeout=40)
    panel = _inspector(frame)
    A.wait_until(lambda: "500 行" in _status_texts(panel), 20, "A 的第一页")
    _select_tree_row(frame, "widgets_empty")
    info = A.find_one(panel, role_in=("toggle button",), name="表信息", timeout=15)
    if "pressed" not in A.states(info):
        A.click(info)
    A.wait_until(lambda: {"widgets_empty", "qty · INTEGER"} <= set(_status_texts(panel)),
                 15, "表标签仍活动，Inspector 预览 B 的表信息")
    expect("已加载行数" not in _status_texts(panel), "B 未打开数据标签",
           _status_texts(panel))
    before = _metadata_counts(inst)

    with step("paginate_a_while_previewing_b"):
        A.click(A.find_one(frame, role_in=("button",), name="下一页", timeout=15))
        A.wait_until(lambda: "[data] 数据页已就绪：offset=500，5 行" in inst.log_text(),
                     20, "A 的第二页实际返回 5 行")
        labels = _status_texts(panel)
        expect("widgets_empty" in labels and "已加载行数" not in labels,
               "B 仍无行数", labels)
        expect(_metadata_counts(inst) == before, "A 的分页不提交元数据请求",
               (_metadata_counts(inst), before))


def test_closing_background_data_tab_clears_only_its_rows(ui_instance):
    """inspector.object.info: 关闭后台表数据标签只重算该对象已加载行数。"""
    inst = _start(ui_instance, "rows_detach", "--qa-tree-open", "widgets")
    frame = inst.frame()
    panel = _inspector(frame)
    A.wait_until(lambda: "3 行" in _status_texts(panel), 20, "A 的数据页已加载")
    A.mouse_click(A.find_one(ops.find_row(frame, "Tables", timeout=15), role_in=("label",), name="Tables"))
    _wait_list_loaded(inst)
    _select_tree_row(frame, "widgets", "表")
    info = A.find_one(panel, role_in=("toggle button",), name="表信息", timeout=15)
    if "pressed" not in A.states(info):
        A.click(info)
    A.wait_until(lambda: {"widgets", "qty · INTEGER", "3 行"} <= set(_status_texts(panel)),
                 15, "Objects 预览 A 仍显示其后台数据页行数")
    before = _metadata_counts(inst)

    with step("close_background_tab"):
        tab = A.find_one(frame, role_in=("page tab",), name="widgets", timeout=15)
        close = A.find_one(tab, role_in=("button",), name="关闭标签")
        A.click(close)
        A.wait_until(lambda: "[tabs] 标签 tab-t1 已移除" in inst.log_text(), 15, "后台数据页关闭")
        labels = _status_texts(panel)
        expect({"widgets", "qty · INTEGER"} <= set(labels) and "3 行" not in labels,
               "A 仍展示表信息，已加载行数退场", labels)
        expect(_metadata_counts(inst) == before, "关闭后台数据页不提交元数据请求",
               (_metadata_counts(inst), before))


@pytest.mark.parametrize("disconnect", ["UITestDB", "Peer"])
def test_query_catalog_tracks_rebind_and_disconnect(ui_instance, disconnect):
    """inspector.catalog, tabs.query.connection_picker, tabs.strip.tab.query, shell.title:
    同名库重绑、失败回滚和断连归属。"""
    inst = ui_instance(f"catalog_{disconnect}")
    peer_db = inst.db.with_name("peer.db")
    with sqlite3.connect(peer_db) as con:
        con.execute("CREATE TABLE peer_only(id INTEGER)")
    legacy = inst.home / "config" / "sqarvo" / "Sqarvo" / "connections.json"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(json.dumps({
        "version": 1,
        "connections": [
            {"name": label, "alias": label, "driver": "sqlite", "file_path": str(path),
             "host": "", "port": 0, "database": "", "username": "", "read_only": True,
             "tls_mode": "disabled", "credential_ref": ""}
            for label, path in (("Peer", peer_db), ("Unavailable", inst.db.with_name("missing.db")))
        ],
    }), encoding="utf-8")
    script = inst.home / "tabs.txt"
    script.write_text("\n".join([
        "expand main",
        "expand Peer",
        "select-node widgets",
        "query-tab",
        "expect-inspector facts=表=3 个",
        "connection 1 Peer",
        "expect-connection 1 Peer",
        "connection 1 Unavailable",
        "expect-connection 1 Peer",
        "expect-inspector page=catalog",
        "expect-inspector facts=名称=main",
        f"disconnect {disconnect}",
    ]), encoding="utf-8")
    inst.start(autoconnect=True, extra_args=("--qa-tabs-file", str(script)))
    with step("rebind_and_roll_back"):
        frame = inst.frame()
        ops.wait_connected(frame, "uitest.db", timeout=40)
        A.wait_until(lambda: "断言 OK：信息面板 facts=" in inst.log_text(), 15,
                     "原连接 main 的已展开表计数为 3")
        A.wait_until(lambda: "重绑成功：Peer" in inst.log_text(), 15, "SQL 标签重绑 Peer")
        A.wait_until(lambda: "重绑失败（保留 Peer）" in inst.log_text(), 15,
                     "不可用目标失败，标签恢复 Peer")
        A.wait_until(lambda: any("断言 OK：信息面板 facts=" in line and "名称=main" in line and "（期望 名称=main）" in line
                                 for line in inst.log_text().splitlines()), 15,
                     "重绑失败后保留 Peer 的 main")
        fact_line = next(line for line in inst.log_text().splitlines()
                         if "断言 OK：信息面板 facts=" in line and "名称=main" in line and "（期望 名称=main）" in line)
        expect("3 个" not in fact_line, "Peer 的 main 没有借用原连接的表数", fact_line)

    with step("disconnect_" + disconnect):
        A.wait_until(lambda: f"连接「{disconnect}」的浏览会话已断开" in inst.log_text(),
                     15, f"{disconnect} 的浏览会话已断开")
        target = "uitest.db" if disconnect == "UITestDB" else disconnect
        def disconnected_row():
            row = ops.find_row(frame, target)
            return row if row is not None and not any(
                label in {"已连接", "断开中…"} for label in ops.tree_row_texts(row)) else None
        A.wait_until(disconnected_row, 15, "对象树连接行完成断开")
        A.wait_gone(lambda: A.find_dialog(inst.app, "断开连接？"), 15, "断连确认框关闭")
        labels = _status_texts(_inspector(frame))
        expect(("main" in labels) == (disconnect == "UITestDB"),
               "只在当前展示所属连接断开时清除库视图", labels)


def test_navigation_selection_survives_projection_without_requests(ui_instance):
    """object_tree.tree, object_tree.tree.row.expander, objects_page.table, inspector.object:
    折叠和恢复已加载路径时保留逻辑选择，程序化恢复不重复预览或浏览。
    """
    inst = _start(ui_instance, "navigation_projection")
    frame = inst.frame()
    _wait_list_loaded(inst)
    if ops.find_row(frame, "widgets", "表") is None:
        ops.expand_tree_row(ops.find_row(frame, "Tables", timeout=15))
    _select_tree_row(frame, "widgets", "表")
    panel = _inspector(frame)
    A.wait_until(lambda: any("CREATE TABLE widgets" in A.text_value(control)
                             for control in A.query(panel, role_in=("text",))),
                 15, "已完成一次用户主动的预览")
    before = _metadata_counts(inst)

    with step("collapse_then_restore"):
        ops.expand_tree_row(ops.find_row(frame, "uitest.db", "已连接"))
        A.wait_gone(lambda: ops.find_row(frame, "widgets", "表"), 10, "目标随折叠隐藏")
        ops.expand_tree_row(ops.find_row(frame, "uitest.db", "已连接"))
        main = ops.find_row(frame, "main", timeout=15)
        if ops.find_row(frame, "Tables") is None:
            ops.expand_tree_row(main)
        tables = ops.find_row(frame, "Tables", timeout=15)
        if ops.find_row(frame, "widgets", "表") is None:
            ops.expand_tree_row(tables)
        A.wait_until(lambda: "selected" in A.states(
            ops.find_row(frame, "widgets", "表", timeout=5)),
            10, "同一资源恢复为可见选择")
        expect(_metadata_counts(inst) == before, "恢复逻辑选择不会请求元数据",
               (_metadata_counts(inst), before))

    with step("focus_layout_preserves_selection_and_scope"):
        rows = _objects_labels(frame)
        title = A.nm(frame)
        rpc_before = inst.log_text().count("[qa.rpc] dispatch")
        expect(rpc_before > 0, "准备阶段已记录 RPC", rpc_before)
        focus = A.find_one(frame, role_in=("toggle button",), name="专注模式")
        A.click(focus)
        A.wait_until(lambda: "pressed" in A.states(focus), 5, "专注模式已进入")
        expect(_objects_labels(frame) == rows, "折叠两侧栏不变更 Objects 范围", _objects_labels(frame))
        A.click(focus)
        A.wait_until(lambda: "pressed" not in A.states(focus), 5, "专注模式已退出")
        A.wait_until(lambda: "selected" in A.states(
            ops.find_row(frame, "widgets", "表", timeout=5)),
            10, "同一资源仍是逻辑选择")
        expect(A.nm(frame) == title and _objects_labels(frame) == rows,
               "工作区标题与 Objects 范围保持", (A.nm(frame), _objects_labels(frame)))
        expect(inst.log_text().count("[qa.rpc] dispatch") == rpc_before,
               "布局不重新浏览 Objects 或刷新 Inspector",
               (rpc_before, inst.log_text().count("[qa.rpc] dispatch")))
