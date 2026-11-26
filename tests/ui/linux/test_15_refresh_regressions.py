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
    ops.click_tree_until(
        lambda: A.find_one(ops.find_row(frame, exact_name="Tables", timeout=15),
                           role_in=("label",), name="Tables"),
        lambda: A.query(frame, role_in=("grouping",), name="Objects"),
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


def test_objects_list_refreshes_on_reentry_and_f5(ui_instance):
    """外部 DDL 改结构后切回 Objects 见最新列表；DROP 后确认最终加载成功再断言。"""
    inst = _start(ui_instance, "obj_reload")
    with step("initial_list"):
        _wait_list_loaded(inst)
        expect("widgets" in _objects_labels(inst.frame()),
               "初始列表含 widgets", _objects_labels(inst.frame()))
        before = _submit_count(inst, "objects.nodes")

    with step("create_and_reenter"):
        _external_ddl(inst, "CREATE TABLE fresh_table(id INTEGER)")
        _sql_tab_roundtrip(inst)
        _expect_submit(inst, "objects.nodes", before + 1)
        expect("fresh_table" in _objects_labels(inst.frame()),
               "切回 Objects 后看到新建表", _objects_labels(inst.frame()))

    with step("rename_and_reenter"):
        count = _submit_count(inst, "objects.nodes")
        _external_ddl(inst, "ALTER TABLE fresh_table RENAME TO renamed_table")
        _sql_tab_roundtrip(inst)
        _expect_submit(inst, "objects.nodes", count + 1)
        labels = _objects_labels(inst.frame())
        expect("renamed_table" in labels and "fresh_table" not in labels,
               "重命名后新旧表名正确更新", labels)

    with step("drop_and_reenter"):
        count = _submit_count(inst, "objects.nodes")
        _external_ddl(inst, "DROP TABLE renamed_table")
        _sql_tab_roundtrip(inst)
        _expect_submit(inst, "objects.nodes", count + 1)
        _wait_list_loaded(inst)
        labels = _objects_labels(inst.frame())
        expect("renamed_table" not in labels, "DROP 的表消失", labels)
        expect("widgets" in labels and "widgets_empty" in labels,
               "其它表仍在（列表成功加载而非空态）", labels)

    with step("f5_force_reload"):
        count = _submit_count(inst, "objects.nodes")
        _tab_action(inst.frame(), "win.refresh")
        _wait_list_loaded(inst)
        _expect_submit(inst, "objects.nodes", count + 1)
        expect("widgets" in _objects_labels(inst.frame()),
               "F5 后列表保持当前作用域", _objects_labels(inst.frame()))


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
        _expect_submit(inst, "objects.nodes", before["objects.nodes"] + 1)
        _expect_submit(inst, "nodes", before["nodes"])


@pytest.mark.parametrize("kind", ["Functions", "Queries"])
def test_objects_unsupported_kind_survives_reentry_and_f5(ui_instance, kind):
    """shell.toolbar.functions, shell.toolbar.queries: 重入和 F5 后保留未实现说明。"""
    inst = _start(ui_instance, f"obj_unsupported_{kind.lower()}")
    with step("unsupported_category"):
        _wait_list_loaded(inst)
        A.click(A.query(inst.frame(), role_in=("button",), name=kind)[0])
        A.wait_until(
            lambda: any("尚未实现" in text for text in _status_texts(_objects_page(inst.frame()))),
            15, f"{kind} 显示不支持说明")
        before = _submit_count(inst, "objects.nodes")

    frame = inst.frame()
    with step("reentry_keeps_unsupported"):
        ops.open_sql_page(frame)
        _tab_action(frame, "win.tab-1")
        A.wait_until(lambda: any("尚未实现" in text for text in _status_texts(_objects_page(frame))),
                     15, "重新进入后保留不支持状态")
        expect(_objects_grid(frame) is None, "重新进入仍无对象列表")
        _expect_submit(inst, "objects.nodes", before)

    with step("f5_keeps_unsupported"):
        _tab_action(frame, "win.refresh")
        A.wait_until(lambda: any("尚未实现" in text for text in _status_texts(_objects_page(frame))),
                     15, "F5 后保留不支持状态")
        expect(_objects_grid(frame) is None, "F5 后仍无对象列表")
        _expect_submit(inst, "objects.nodes", before)


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
        tab = A.find_one(frame, role_in=("page tab",), name="widgets @ main", timeout=15)
        close = A.find_one(tab, role_in=("button",), name="Close Tab")
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
