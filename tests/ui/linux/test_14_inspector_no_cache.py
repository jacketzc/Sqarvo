"""Inspector 无缓存按需查询的请求与展示行为（功能验收，无截图/样式断言）。

注册表组件: inspector, inspector.object, inspector.pane_switch,
inspector.pane_switch.ddl, inspector.pane_switch.info, inspector.object.info。

验证真实请求而不是缓存命中信息：统计 MetadataHub 在真实提交边界打印的 `[qa.metadata] submit <kind> id=` 行，
并用 AT-SPI 读取面板上的列文案。每个实例独立配置 / 数据库 / 日志。
"""
from __future__ import annotations

import os
import sqlite3
import time

import pytest

import adapter as A
import ops
import gi
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.sql

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

KINDS = ("ddl", "columns", "primary_key", "inspector.nodes")
EXPAND_ARGS = ("--qa-tree-expand", "main", "--qa-tree-expand", "Tables")


def _start(ui_instance, tag: str, *extra: str) -> UiInstance:
    inst = ui_instance(tag).start(autoconnect=True, extra_args=(*EXPAND_ARGS, *extra))
    ops.wait_connected(inst.frame(), "uitest.db", timeout=40)
    return inst


def _counts(inst: UiInstance) -> dict[str, int]:
    text = inst.log_text()
    return {kind: text.count(f"[qa.metadata] submit {kind} id=") for kind in KINDS}


def _wait_counts(inst: UiInstance, **minimum: int) -> dict[str, int]:
    def probe():
        counts = _counts(inst)
        return counts if all(counts[k] >= v for k, v in minimum.items()) else None
    return A.wait_until(probe, 25, f"请求计数达到 {minimum}")


def _select_tree_row(inst: UiInstance, name: str) -> None:
    """走对象树的单选模型（与用户点击行同一语义）。"""
    frame = inst.frame()
    def target():
        for row in A.query(frame, role_in=("list item",)):
            if A.query(row, role_in=("label",), name=name, max_depth=6):
                return row
        return None

    row = A.wait_until(target, 15, f"树行 {name}")
    A.mouse_click(A.find_one(row, role_in=("label",), name=name))


def _inspector(frame):
    return A.find_one(frame, role_in=("grouping",), name="信息面板", timeout=15)


def _pane_button(frame, name: str):
    buttons = A.query(_inspector(frame), role_in=("toggle button",), name=name)
    expect(bool(buttons), f"面板切换按钮 {name} 存在", len(buttons))
    return buttons[0]


def _labels(frame) -> list[str]:
    return [A.nm(x) for x in A.query(_inspector(frame), role_in=("label",)) if A.nm(x)]


def _clipboard_text():
    """Read the real clipboard from a separate client of the runner's private X11 display."""
    with A._xtest_display():
        gi.require_version("Gdk", "4.0")
        gi.require_version("Gtk", "4.0")
        from gi.repository import Gdk, Gio, GLib, Gtk

        expect(Gtk.init_check(), "初始化隔离 X11 的 GTK 剪贴板客户端")
        display = Gdk.Display.get_default()
        expect(display is not None and display.get_name() == os.environ["DISPLAY"],
               "连接隔离的剪贴板显示服务")
        loop = GLib.MainLoop()
        cancel = Gio.Cancellable()
        received = []

        def finish(clipboard, result):
            try:
                received.append(clipboard.read_text_finish(result))
            except GLib.Error:
                received.append(None)
            loop.quit()

        def timeout(*_):
            cancel.cancel()
            loop.quit()
            return GLib.SOURCE_REMOVE

        timer = GLib.timeout_source_new(5000)
        timer.set_callback(timeout)
        timer.attach()
        try:
            display.get_clipboard().read_text_async(cancel, finish)
            loop.run()
            return received[0] if received else ""
        finally:
            timer.destroy()


def test_table_info_pages_retry_and_refresh(ui_instance):
    """inspector.object.info, inspector.object.info.more, inspector.object.info.refresh, inspector.object.info.children,
    inspector.object.info.children.load_more, inspector.object.info.children.retry."""
    inst = ui_instance("inspector_child_pages")
    with sqlite3.connect(inst.db) as db:
        for index in range(501):
            db.execute(f"CREATE INDEX idx_s4_{index:04d} ON widgets(qty)")
        db.execute("CREATE TRIGGER trg_s4_tail AFTER INSERT ON widgets BEGIN SELECT 1; END")
    inst.start(autoconnect=True, extra_args=(*EXPAND_ARGS, "--qa-tree-select", "widgets", "--lang", "zh"))
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db", timeout=40)
    _wait_counts(inst, ddl=1)
    A.click(_pane_button(frame, "表信息"))
    panel = _inspector(frame)

    def button(name):
        return A.find_one(panel, role_in=("button",), name=name, timeout=15)

    def refresh_command():
        A.click(A.find_one(panel, role_in=("toggle button",), name="表信息更多操作", timeout=10))
        menu = A.find_one(panel, role_in=("menu",), name="表信息更多操作", timeout=10)
        return A.find_one(menu, role_in=("menu item",), name="刷新表信息", timeout=10)

    def has_name(name):
        return any(name in label for label in _labels(frame))

    with step("first_page_then_explicit_retry"):
        more = button("加载更多索引和触发器")
        A.wait_until(lambda: has_name("idx_s4_0000"), 15, "第一批索引已显示")
        expect(not has_name("idx_s4_0500") and not has_name("trg_s4_tail"), "后续页尚未读取")
        before = _counts(inst)
        with sqlite3.connect(inst.db) as lock:
            lock.execute("BEGIN EXCLUSIVE")
            A.click(more)
            retry = button("重试读取索引和触发器")
            expect(has_name("idx_s4_0000"), "读取失败保留第一批索引")
            expect(has_name("读取失败"), "显示失败状态")
            expect(_counts(inst)["inspector.nodes"] == before["inspector.nodes"] + 1,
                   "一次手动分页只提交一个请求")
        A.click(retry)
        A.wait_until(lambda: has_name("idx_s4_0500"), 15, "重试后可见后续索引")
        A.find_one(panel, role_in=("button",), name="查看触发器 DDL", timeout=10)
        expect(_counts(inst)["inspector.nodes"] == before["inspector.nodes"] + 2,
               "手动重试只提交一个请求，后续触发器已可操作")

    with step("refresh_replaces_the_current_details"):
        with sqlite3.connect(inst.db) as db:
            db.execute("DROP INDEX idx_s4_0000")
            db.execute("DROP TRIGGER trg_s4_tail")
            db.execute("CREATE TRIGGER trg_s4_refreshed AFTER INSERT ON widgets BEGIN SELECT 2; END")
            db.execute("ALTER TABLE widgets ADD COLUMN refreshed_s4 TEXT")
        before = _counts(inst)
        expect(not A.query(panel, role_in=("button",), name="刷新表信息"),
               "表信息区域没有常驻刷新按钮")
        rpc_before = len(inst.log_text())
        A.click(refresh_command())
        A.wait_until(lambda: has_name("refreshed_s4 · TEXT") and has_name("idx_s4_0500")
                     and not has_name("idx_s4_0000"), 15, "按当前表的新定义刷新列和索引")
        after = _counts(inst)
        expect(after == {**before, "columns": before["columns"] + 1,
                         "primary_key": before["primary_key"] + 1,
                         "inspector.nodes": before["inspector.nodes"] + 1},
               "显式刷新只读取表信息", (before, after))
        calls = [line.removeprefix("[qa.rpc] dispatch ") for line in
                 inst.log_text()[rpc_before:].splitlines() if line.startswith("[qa.rpc] dispatch ")]
        expect(sorted(calls) == ["GetColumns", "GetPrimaryKey", "ListNodes"],
               "显式刷新只发送当前表信息的三个业务请求", calls)
        A.click(button("加载更多索引和触发器"))
        A.find_one(panel, role_in=("label",), name="trg_s4_refreshed", timeout=15)
        expect(not has_name("trg_s4_tail"), "刷新后的分页只显示当前触发器")

    with step("late_page_cannot_replace_another_table"):
        A.click(refresh_command())
        more = button("加载更多索引和触发器")
        before = _counts(inst)["inspector.nodes"]
        with sqlite3.connect(inst.db) as lock:
            lock.execute("BEGIN EXCLUSIVE")
            A.click(more)
            _wait_counts(inst, **{"inspector.nodes": before + 1})
            _select_tree_row(inst, "widgets_empty")
            A.wait_until(lambda: "widgets_empty" in _labels(frame), 10, "切到另一个表")
        A.wait_until(lambda: "无触发器" in _labels(frame), 15, "新表的元数据已返回")
        expect(not has_name("idx_s4_0500") and not has_name("trg_s4_refreshed"),
               "旧分页的数据没有写入新表")

    with step("changing_the_target_dismisses_refresh_menu"):
        refresh_command()
        before = _counts(inst)
        A.window_action(frame, "win.clear-selection")
        A.wait_gone(lambda: A.query(panel, role_in=("menu item",), name="刷新表信息"), 8,
                    "清除预览目标后关闭旧刷新菜单")
        expect(_counts(inst) == before, "失效的菜单不发送元数据请求")


def test_trigger_ddl_view_copy_and_object_isolation(ui_instance):
    """inspector.object.info.children, inspector.object.info.children.view_ddl,
    inspector.object.info.children.trigger_picker,
    inspector.object.info.trigger_ddl, inspector.object.info.trigger_ddl.copy,
    inspector.object.info.trigger_ddl.back, inspector.object.info.trigger_ddl.retry,
    object_tree.tree.row.object."""
    first = "CREATE TRIGGER trg_s4_a AFTER INSERT ON widgets BEGIN SELECT 17; END"
    second = "CREATE TRIGGER trg_s4_b AFTER INSERT ON widgets_empty BEGIN SELECT 42; END"
    inst = ui_instance("inspector_trigger_ddl")
    with sqlite3.connect(inst.db) as db:
        db.execute("CREATE TRIGGER trg_s4_0 AFTER INSERT ON widgets BEGIN SELECT 8; END")
        db.execute(first)
        db.execute(second)
    inst.start(autoconnect=True, extra_args=(*EXPAND_ARGS, "--qa-tree-select", "widgets", "--lang", "zh"))
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db", timeout=40)
    _wait_counts(inst, ddl=1)
    navigation = A.find_one(frame, role_in=("grouping",), name="导航")
    panel = _inspector(frame)

    def button(name):
        return A.find_one(panel, role_in=("button",), name=name, timeout=15)

    def texts():
        return [A.text_value(control) for control in A.query(panel, role_in=("text",))]

    def rpc_count():
        return inst.log_text().count("[qa.rpc] dispatch")

    with step("table_stays_a_leaf_and_trigger_ddl_is_explicit"):
        row = ops.find_row(navigation, exact_name="widgets", timeout=10)
        expect(not A.query(row, role_in=("button",), name="展开或折叠"),
               "表保持叶节点")
        A.click(_pane_button(frame, "表信息"))
        view = button("查看触发器 DDL")
        before = _counts(inst)
        picker = A.find_one(panel, role_in=("combo box",), timeout=10)
        A.click(A.find_one(picker, role_in=("toggle button",)))
        A.wait_until(lambda: "expanded" in A.states(picker), 10, "展开触发器选择")
        option = A.wait_until(lambda: next(
            (item for item in A.query(frame, role_in=("list item",))
             if A.query(item, role_in=("label",), name="trg_s4_a", max_depth=3)), None),
            10, "触发器下拉包含非首个条目")
        listing = option.get_parent()
        expect(Atspi.Selection.select_child(listing, option.get_index_in_parent()),
               "从原生下拉选择触发器 trg_s4_a")
        popup = listing.get_parent()
        while popup is not None and "default.activate" not in A.action_names(popup):
            popup = popup.get_parent()
        expect(popup is not None, "原生下拉提供提交选择的操作")
        A.window_action(popup, "default.activate")
        A.wait_until(lambda: "expanded" not in A.states(picker)
                     and bool(A.query(picker, role_in=("label",), name="trg_s4_a")),
                     10, "提交非首个触发器的选择")
        A.click(view)
        A.wait_until(lambda: first in texts(), 15, "显示 widgets 触发器的完整 SQLite 原文")
        expect(_counts(inst) == {**before, "ddl": before["ddl"] + 1},
               "查看触发器只提交一个 GetDdl", (_counts(inst), before))

    with step("copy_and_passive_actions_have_no_rpc"):
        before = rpc_count()
        A.click(button("复制触发器 DDL"))
        copied = A.wait_until(_clipboard_text, 10, "系统剪贴板提供文本")
        expect(copied == first, "系统剪贴板包含选中触发器的完整原文", copied)
        A.window_action(frame, "win.focus-search")
        entry = A.find_one(navigation, role_in=("entry",), name="搜索资源…")
        A.set_text(entry, "s4_no_match")
        A.find_one(navigation, role_in=("label",), name="已加载的资源中没有匹配项", timeout=10)
        A.set_text(entry, "")
        A.wait_gone(lambda: A.query(navigation, role_in=("label",),
                                   name="已加载的资源中没有匹配项"), 10, "清词恢复普通导航")
        A.window_action(frame, "win.toggle-sidebar")
        A.window_action(frame, "win.toggle-sidebar")
        focus = A.find_one(frame, role_in=("toggle button",), name="专注模式")
        A.click(focus)
        A.wait_until(lambda: "pressed" in A.states(focus), 10, "进入专注模式")
        A.click(focus)
        A.wait_until(lambda: "pressed" not in A.states(focus), 10, "退出专注模式")
        A.wait_until(lambda: first in texts(), 10, "恢复布局后仍是原触发器")
        expect(rpc_count() == before, "复制、导航过滤和布局变化新增零业务 RPC",
               (before, rpc_count()))

    with step("late_trigger_response_cannot_replace_another_object"):
        before = _counts(inst)
        A.click(button("返回表信息"))
        view = button("查看触发器 DDL")
        expect(_counts(inst) == before, "返回表信息复用已读分页，新增零请求")
        with sqlite3.connect(inst.db) as lock:
            lock.execute("BEGIN EXCLUSIVE")
            before = _counts(inst)["ddl"]
            A.click(view)
            _wait_counts(inst, ddl=before + 1)
            _select_tree_row(inst, "widgets_empty")
            A.wait_until(lambda: "widgets_empty" in _labels(frame), 10, "切到另一个表")
        A.click(button("查看触发器 DDL"))
        A.wait_until(lambda: second in texts(), 15, "只显示新表触发器的原文")
        expect(first not in texts(), "旧表触发器的迟到响应未覆盖新目标")

    with step("retry_failed_trigger_ddl"):
        A.click(button("返回表信息"))
        with sqlite3.connect(inst.db) as lock:
            lock.execute("BEGIN EXCLUSIVE")
            A.click(button("查看触发器 DDL"))
            retry = button("重试读取触发器 DDL")
            expect(first not in texts() and second not in texts(), "失败时旧正文已经清除")
        before = _counts(inst)
        A.click(retry)
        A.wait_until(lambda: second in texts(), 15, "手动重试后恢复同一触发器正文")
        expect(_counts(inst) == {**before, "ddl": before["ddl"] + 1}, "重试只发一个 GetDdl")


def test_first_visit_is_on_demand_and_pane_switches_requery(ui_instance):
    """首次选中只查 DDL；切到表信息与切回 DDL 各查一次目标子页。"""
    inst = _start(ui_instance, "insp_demand", "--qa-tree-select", "widgets")
    with step("first_visit_only_ddl"):
        counts = _wait_counts(inst, ddl=1)
        expect(counts == {"ddl": 1, "columns": 0, "primary_key": 0, "inspector.nodes": 0},
               "首次选中 widgets 只请求 GetDdl", counts)

    with step("switch_to_info"):
        A.click(_pane_button(inst.frame(), "表信息"))
        counts = _wait_counts(inst, **{"columns": 1, "primary_key": 1, "inspector.nodes": 1})
        expect(counts == {"ddl": 1, "columns": 1, "primary_key": 1, "inspector.nodes": 1},
               "切到表信息只补查列 / 主键 / 索引", counts)

    with step("switch_back_to_ddl"):
        A.click(_pane_button(inst.frame(), "DDL"))
        counts = _wait_counts(inst, ddl=2)
        expect(counts == {"ddl": 2, "columns": 1, "primary_key": 1, "inspector.nodes": 1},
               "切回 DDL 重新发 GetDdl，不重发表信息请求", counts)

    with step("duplicate_click_on_active_pane"):
        A.wait_until(lambda: "CREATE TABLE widgets" in _ddl_text(inst.frame()), 15, "DDL 已加载")
        before = _counts(inst)
        A.click(_pane_button(inst.frame(), "DDL"))
        expect(_counts(inst) == before, "重复点已选中的子页不重复请求",
               (before, _counts(inst)))

    with step("reenter_info"):
        A.click(_pane_button(inst.frame(), "表信息"))
        A.wait_until(lambda: "qty · INTEGER" in _labels(inst.frame()), 15, "重新进入表信息页显示 qty")
        counts = _counts(inst)
        expect(counts == {"ddl": 2, "columns": 2, "primary_key": 2, "inspector.nodes": 2},
               "重新进入表信息只提交各一个元数据请求", counts)


def test_revisit_refresh_and_idle_do_not_reuse_history(ui_instance):
    """A → B → A 每次重查；主动刷新即使对象不变也重查；闲置不产生轮询。"""
    inst = _start(ui_instance, "insp_revisit", "--qa-tree-select", "widgets")
    _wait_counts(inst, ddl=1)

    with step("a_to_b_to_a"):
        _select_tree_row(inst, "widgets_empty")
        counts = _wait_counts(inst, ddl=2)
        expect(counts["ddl"] == 2, "切到 B 重新查一次", counts)
        _select_tree_row(inst, "widgets")
        counts = _wait_counts(inst, ddl=3)
        expect(counts["ddl"] == 3, "回到 A 再查一次（不复用历史结果）", counts)

    with step("explicit_refresh"):
        A.window_action(inst.frame(), "win.refresh")
        counts = _wait_counts(inst, ddl=4)
        expect(counts["ddl"] == 4, "F5 主动刷新重查当前对象", counts)

    with step("idle_has_no_background_requests"):
        before = _counts(inst)
        time.sleep(2.5)
        expect(_counts(inst) == before, "没有用户操作时不应有元数据查询",
               (before, _counts(inst)))


def _ddl_text(frame) -> str:
    for acc in A.walk(_inspector(frame), max_depth=40):
        text = A.text_value(acc)
        if "CREATE TABLE" in text:
            return text
    return ""


def test_fast_a_b_a_switch_keeps_the_last_request(ui_instance):
    """快速 A → B → A 后，只有最后一次 A 的正文留在面板上。"""
    inst = _start(ui_instance, "insp_fast", "--qa-tree-select", "widgets")
    A.wait_until(lambda: "CREATE TABLE" in _ddl_text(inst.frame()), 15, "初始 DDL 正文")
    with step("fast_switch"):
        _select_tree_row(inst, "widgets_empty")
        _select_tree_row(inst, "widgets")
        counts = _wait_counts(inst, ddl=3)
        expect(counts["ddl"] == 3, "A → B → A 共三次 GetDdl", counts)
        A.wait_until(
            lambda: "CREATE TABLE widgets" in _ddl_text(inst.frame())
            and "widgets_empty" not in _ddl_text(inst.frame()),
            15, "最后展示的是最后一次 A 的 DDL")


def test_disconnect_clears_display_and_invalidates_requests(ui_instance):
    """断开浏览会话后，旧对象的正文不再展示，在途响应也不会回填。"""
    inst = _start(ui_instance, "insp_disc", "--qa-tree-select", "widgets",
                  "--qa-tree-disconnect", "UITestDB")
    with step("disconnect"):
        A.wait_until(lambda: "浏览会话已断开" in inst.log_text(), 20,
                     "浏览会话已断开")
        expect("CREATE TABLE" not in _ddl_text(inst.frame()),
               "断连后 DDL 正文已清空", _ddl_text(inst.frame())[:80])

    with step("no_requests_after_disconnect"):
        before = _counts(inst)
        time.sleep(1.5)
        expect(_counts(inst) == before, "断连后不应继续发起元数据请求",
               (before, _counts(inst)))


def test_external_ddl_change_is_visible_on_next_visit(ui_instance):
    """外部 ALTER TABLE 加列后，主动刷新按新会话状态重新查列并展示。"""
    inst = _start(ui_instance, "insp_ddl_change", "--qa-tree-select", "widgets")
    with step("load_columns"):
        _wait_counts(inst, ddl=1)
        A.click(_pane_button(inst.frame(), "表信息"))
        _wait_counts(inst, columns=1)
        A.wait_until(lambda: "qty · INTEGER" in _labels(inst.frame()),
                     15, "表信息页出现原有列 qty")
        before = _counts(inst)

    with step("alter_table_externally"):
        con = sqlite3.connect(inst.db)
        try:
            con.execute("ALTER TABLE widgets ADD COLUMN new_marker TEXT")
            con.commit()
        finally:
            con.close()

    with step("refresh_and_read_new_column"):
        A.window_action(inst.frame(), "win.refresh")
        _wait_counts(inst, columns=before["columns"] + 1)
        A.wait_until(lambda: "new_marker · TEXT" in _labels(inst.frame()),
                     15, "表信息页出现新增列 new_marker")
        expect("qty · INTEGER" in _labels(inst.frame()), "原有列仍在展示", _labels(inst.frame()))
        expected = {kind: count + (kind != "ddl") for kind, count in before.items()}
        expect(_counts(inst) == expected, "F5 各提交一个表信息请求", _counts(inst))
