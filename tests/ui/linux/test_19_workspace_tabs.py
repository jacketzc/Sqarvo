"""S6 native workspace tab management on the isolated display."""

import json
import signal
from pathlib import Path

from gi.repository import Atspi

import adapter as A
import ops
from ops import expect, step
from test_10_layout import _backend_pidfd
from test_15_refresh_regressions import _browse_tables, _rpc_counts


def tab_rpc_counts(inst):
    log = inst.log_text()
    return (
        *_rpc_counts(inst), log.count("[db-rpc]"),
        log.count("请求释放"), log.count("原生临时摘页"),
    )


def tab(frame, title):
    widget = A.find_one(frame, role_in=("page tab",), name=title, timeout=8)
    previous = None
    stable = 0

    def can_target_pointer():
        nonlocal previous, stable
        bounds = Atspi.Component.get_extents(widget, Atspi.CoordType.WINDOW)
        position = (bounds.x, bounds.y, bounds.width, bounds.height)
        stable = stable + 1 if previous == position else 0
        previous = position
        return bounds.width > 0 and bounds.height > 0 and stable >= 2

    # The native reorder signal is synchronous, while widget positions animate.
    # Use settled bounds only to target the next real pointer event.
    A.wait_until(can_target_pointer, 8, f"稳定的指针目标 {title}")
    return widget


def expect_order(inst, ids):
    def model_order():
        lines = inst.log_text().splitlines()
        order = next((line.split()[2:] for line in reversed(lines)
                      if line.startswith("[qa.tabs] order ")), [])
        return order == ["tab-objects", *ids]

    A.wait_until(model_order, 8, f"标签模型顺序 {ids}")


def expect_active(frame, title):
    A.wait_until(lambda: A.nm(frame).startswith(title), 8, f"活动工作区 {title}")


def pointer_surface_point(inst, prefix):
    return A.wait_until(
        lambda: next((tuple(map(int, line[len(prefix):].split()))
                      for line in reversed(inst.log_text().splitlines())
                      if line.startswith(prefix)), None),
        8, f"原生控件的指针位置 {prefix}",
    )


def cancel_close(inst):
    dialog = A.wait_dialog(inst.app, "关闭标签？")
    A.click(A.find_one(dialog, role_in=("button",), name="取消"))
    A.wait_gone(lambda: A.find_dialog(inst.app, "关闭标签？"), 8, "关闭确认已取消")


def test_sql_tab_unsaved_indicator_survives_execution(ui_instance):
    """tabs.strip.tab, tabs.strip.tab.query (A12.1)."""
    inst = ui_instance("tabdirty").start(autoconnect=True)
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db")
    page = ops.open_sql_page(frame)
    title = A.nm(page)
    A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 15)
    name = "有未保存的更改"

    def dirty():
        widget = tab(frame, title)
        return name in (widget.get_description() or "")

    expect(not dirty(), "空白查询没有未保存状态")
    ops.write_sql(page, "SELECT 1;")
    A.wait_until(dirty, 8, "编辑后页签可访问描述反映未保存状态")
    ops.execute_sql(page)
    expect(len(ops.wait_query_done(page, min_rows=1)) == 1, "SQL 执行成功")
    expect(dirty(), "执行成功后依然标记为未保存")
    ops.write_sql(page, "")
    A.wait_until(lambda: not dirty(), 8, "清空 SQL 后去掉未保存状态")


def test_mru_restores_objects_and_preserves_background_selection(ui_instance):
    """tabs.strip.tab, tabs.strip.tab.close, objects_page.tab (A11.1, A11.3)."""
    inst = ui_instance("tabmru").start(extra_args=("--lang", "zh"))
    frame = inst.frame()
    for _ in range(3):
        ops.open_sql_page(frame)
    first, second, third = ops.query_tab_names(frame)
    A.press_keys("Alt_L", "1")
    expect_active(frame, "Sqarvo")
    A.press_keys("Alt_L", "2")
    expect_active(frame, first)
    with A.mouse_drag(tab(frame, third)) as move_to:
        move_to(tab(frame, second), horizontal_fraction=0.15)
    expect_order(inst, ["tab-q1", "tab-q3", "tab-q2"])
    expect_active(frame, first)
    A.press_keys("Control_L", "w")
    expect_active(frame, "Sqarvo")
    A.wait_gone(lambda: first in ops.query_tab_names(frame), 8, "活动标签已关闭")
    A.press_keys("Alt_L", "2")
    expect_active(frame, third)
    A.press_keys("Alt_L", "1")
    expect_active(frame, "Sqarvo")
    A.press_keys("Control_L", "Shift_L", "Tab")
    expect_active(frame, second)
    A.mouse_click(tab(frame, third), button=2, horizontal_fraction=0.3)
    A.wait_gone(lambda: third in ops.query_tab_names(frame), 8, "后台关闭")
    expect_active(frame, second)
    A.press_keys("Control_L", "w")
    expect_active(frame, "Sqarvo")
    A.wait_gone(lambda: ops.query_tab_names(frame), 8, "仅剩 Objects")


def test_explicit_browse_is_an_objects_activation(ui_instance):
    """objects_page.tab, tabs.strip.tab.close (A11.1, A11.3)."""
    inst = ui_instance("tabmru_browse").start(autoconnect=True, extra_args=(
        "--qa-tree-expand", "main", "--qa-tree-select-group", "tables",
    ))
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db")
    for _ in range(2):
        page = ops.open_sql_page(frame)
        A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 15)
    expect_active(frame, "查询 2")
    _browse_tables(frame)
    A.find_one(frame, role_in=("label",), name="widgets", timeout=15)
    expect_active(frame, "Tables @")
    A.press_keys("Alt_L", "2")
    expect_active(frame, "查询 1")
    A.press_keys("Control_L", "w")
    expect_active(frame, "Tables @")
    expect("查询 2" in ops.query_tab_names(frame), "其他查询保持存续")


def test_batch_cancel_stops_at_dirty_instance(ui_instance):
    """tabs.strip.menu, tabs.strip.tab.close, tabs.close_dialog (A12.4)."""
    inst = ui_instance("tabbatch").start()
    frame = inst.frame()
    ops.open_sql_page(frame)
    dirty = ops.open_sql_page(frame)
    editor = ops.write_sql(dirty, "SELECT 55;")
    ops.open_sql_page(frame)
    first, second, third = ops.query_tab_names(frame)
    A.mouse_click(tab(frame, third), button=3, horizontal_fraction=0.3)
    menu = A.wait_until(lambda: next((widget for widget in A.walk(frame)
                                     if "tab.close-all" in A.action_names(widget)), None),
                        8, "原生右键菜单已捕获标签")
    A.window_action(menu, "tab.close-all")
    cancel_close(inst)
    A.wait_gone(lambda: first in ops.query_tab_names(frame), 8, "批量已关闭首个实例")
    expect(second in ops.query_tab_names(frame) and third in ops.query_tab_names(frame),
           "取消保留当前及后续目标")
    expect_active(frame, third)
    expect(A.text_value(editor).startswith("SELECT 55;"), "批量取消保留 SQL")


def test_search_lists_all_tabs_and_filters_locally(ui_instance):
    """tabs.strip.list, tabs.strip.new, objects_page.tab (A11.4–5)."""
    inst = ui_instance("tabsearch").start(
        autoconnect=True, extra_args=("--window-size", "840x600", "--lang", "zh"),
    )
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db")
    for _ in range(8):
        page = ops.open_sql_page(frame)
        A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 15)
    A.wait_until(lambda: inst.log_text().count("库上下文：main") >= 8, 15, "所有会话就绪")
    expect("showing" in A.states(tab(frame, "Objects")), "溢出时 Objects 可达")
    plus = A.find_one(frame, role_in=("button",), name_contains="新建查询标签")
    expect("showing" in A.states(plus), "溢出时加号可达")
    before = tab_rpc_counts(inst)

    def scroll_position():
        return next((float(line.split()[2]) for line in reversed(inst.log_text().splitlines())
                     if line.startswith("[qa.tabs] scroll ")), 0.0)

    A.wait_until(lambda: scroll_position() > 0, 8, "新建活动标签滚动到可见区")
    active = tab(frame, "查询 8")
    viewport = active.get_parent()
    while viewport is not None and A.role(viewport) != "scroll pane":
        viewport = viewport.get_parent()
    expect(viewport is not None, "普通标签属于横向滚动区")
    end = scroll_position()
    with A.mouse_drag(active, horizontal_fraction=0.4) as move_to:
        move_to(surface_point=pointer_surface_point(inst, "[qa.tabs] left-scroll-edge tab-q8 "))
        A.wait_until(lambda: scroll_position() < end, 8, "拖拽时到达边缘自动滚动")
        A.press_keys("Escape")
    expect_active(frame, "查询 8")
    expect(tab_rpc_counts(inst) == before, "边缘滚动及取消零新增业务请求")
    scrolled_left = scroll_position()
    A.press_keys("Alt_L", "9")
    A.wait_until(lambda: scroll_position() > scrolled_left, 8, "显式激活定位到当前标签")
    end = scroll_position()
    A.mouse_click(viewport, button=4, count=4)
    A.wait_until(lambda: scroll_position() < end, 8, "真实滚轮水平移动标签")
    expect_active(frame, "查询 8")
    menu = A.find_one(frame, role_in=("toggle button",), name="所有标签", timeout=8)
    A.click(menu)
    search = A.find_one(frame, role_in=("entry", "text"), name="搜索标签…", timeout=8)
    for value, match in (("UITEST.DB", "查询 1"), ("not_a_tab", "没有匹配的标签"),
                         ("", "Objects"), ("查询 2", "查询 2")):
        A.set_text(search, value)
        A.find_one(frame, role_in=("label",), name_contains=match, timeout=8)
        expect(tab_rpc_counts(inst) == before, "标签搜索输入、改词、清空零 RPC")
        expect(scroll_position() < end, "搜索期间手动滚动未拉回活动标签")
    A.press_keys("Return")
    expect_active(frame, "查询 2")


def test_reveal_background_tab_uses_captured_object(ui_instance):
    """tabs.strip.menu, object_tree (A11.7)."""
    inst = ui_instance("tabreveal")
    frame = ops.open_table_for_qa(inst, "widgets")
    query = ops.open_sql_page(frame)
    expect_active(frame, "查询 1")
    A.window_action(frame, "win.clear-selection")
    A.mouse_click(tab(frame, "widgets"), button=3, horizontal_fraction=0.35)
    menu = A.wait_until(lambda: next((widget for widget in A.walk(frame)
                                     if "tab.reveal" in A.action_names(widget)), None),
                        8, "后台表的原生菜单")
    expect_active(frame, "查询 1")
    A.window_action(menu, "tab.reveal")
    A.wait_until(lambda: "selected" in A.states(ops.find_row(frame, "widgets", "表")),
                 15, "定位到捕获的表资源")
    expect_active(frame, "查询 1")
    A.press_keys("Escape")
    A.mouse_click(tab(frame, "widgets"), button=3, horizontal_fraction=0.35)
    menu = A.wait_until(lambda: next((widget for widget in A.walk(frame)
                                     if "tab.copy" in A.action_names(widget)), None),
                        8, "后台表的复制命令")
    A.window_action(menu, "tab.copy")
    A.press_keys("Escape")
    editor = ops.editor(query)
    A.mouse_click(editor)
    A.press_keys("Control_L", "v")
    A.wait_until(lambda: A.text_value(editor) == "widgets", 8, "复制捕获的原始完整名称")
    expect_active(frame, "查询 1")


def test_sort_and_close_with_eight_established_sessions(ui_instance):
    """tabs.limit, tabs.strip.tab, tabs.strip.tab.close (A10.3, A11.2)."""
    inst = ui_instance("tabfull").start(
        autoconnect=True, extra_args=("--window-size", "1440x760", "--lang", "zh"),
    )
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db")
    A.click(A.find_one(frame, role_in=("toggle button",), name="专注模式"))
    for _ in range(8):
        page = ops.open_sql_page(frame)
        A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 15)
    A.wait_until(lambda: inst.log_text().count("库上下文：main") >= 8, 15, "八条会话已就绪")
    A.window_action(frame, "win.tab-2")
    expect_active(frame, "查询 1")
    before = tab_rpc_counts(inst)
    with A.mouse_drag(tab(frame, "查询 2")) as move_to:
        move_to(tab(frame, "查询 1"), horizontal_fraction=0.4)
    expect_order(inst, ["tab-q2", "tab-q1", *(f"tab-q{i}" for i in range(3, 9))])
    expect_active(frame, "查询 1")
    plus = A.find_one(frame, role_in=("button",), name_contains="新建查询标签")
    A.click(plus)
    limit = A.wait_dialog(inst.app, "无法新建标签")
    A.click(A.find_one(limit, role_in=("button",), name="知道了"))
    A.wait_gone(lambda: A.find_dialog(inst.app, "无法新建标签"), 8, "额度提醒已关闭")
    expect(tab_rpc_counts(inst) == before, "排序后八条会话仍占用原额度")
    A.window_action(frame, "win.close-tab")
    A.wait_until(lambda: "标签 tab-q1 的会话已释放" in inst.log_text(), 15, "真实关闭释放一个额度")
    A.click(A.find_one(frame, role_in=("button",), name_contains="新建查询标签"))
    page = ops.active_query_page(frame, "查询 9")
    A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 15)
    A.wait_until(lambda: "标签 tab-q9 的会话已建立" in inst.log_text(), 15, "额度恢复后新增独立查询")


def test_window_local_sort_and_cancellation(ui_instance):
    """tabs, tabs.strip, tabs.strip.tab, tabs.strip.tab.query, objects_page.tab,
    tabs.strip.tab.close (A10.5, A11.1–3, A11.6, A12.2).
    """
    inst = ui_instance("tabtransfer").start(
        autoconnect=True, extra_args=("--window-size", "1200x700", "--lang", "zh"),
    )
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db")
    A.click(A.find_one(frame, role_in=("toggle button",), name="专注模式"))
    editors = {}
    for value in (123, 456, 789):
        page = ops.open_sql_page(frame)
        A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 15)
        editors[A.nm(page)] = (ops.write_sql(page, f"SELECT {value};"), value)
    first, second, third = ops.query_tab_names(frame)
    q1, q2, q3 = ("tab-q1", "tab-q2", "tab-q3")
    A.wait_until(lambda: inst.log_text().count("库上下文：main") >= 3, 15, "会话初始请求已完成")
    before = tab_rpc_counts(inst)
    expect(before[2] > 0, "准备阶段已有业务 RPC", before)

    with step("active_and_background_sort"):
        with A.mouse_drag(tab(frame, third)) as move_to:
            move_to(tab(frame, first), horizontal_fraction=0.15)
        expect_order(inst, [q3, q1, q2])
        expect_active(frame, third)
        with A.mouse_drag(tab(frame, second)) as move_to:
            move_to(tab(frame, first), horizontal_fraction=0.15)
        expect_order(inst, [q3, q2, q1])
        expect_active(frame, third)

    with step("cancel_after_reordering"):
        order = [q3, q2, q1]
        with A.mouse_drag(tab(frame, first)) as move_to:
            move_to(tab(frame, second), horizontal_fraction=0.15)
            expect_order(inst, [q3, q1, q2])
            A.press_keys("Escape")
            expect_order(inst, order)
        expect_active(frame, third)

    with step("cancel_at_window_boundary"):
        for return_to_window in (False, True, True):
            with A.mouse_drag(tab(frame, first)) as move_to:
                move_to(tab(frame, second), horizontal_fraction=0.15)
                expect_order(inst, [q3, q1, q2])
                move_to(frame, vertical_fraction=1.08)
                expect_order(inst, order)
                if return_to_window:
                    move_to(tab(frame, third), horizontal_fraction=0.15)
            expect_order(inst, order)
            expect_active(frame, third)
        expect(len(A.windows(inst.app)) == 1, "无新增窗口")

    with step("fixed_objects_and_background_right_click"):
        objects = A.find_one(frame, role_in=("page tab",), name="Objects", timeout=8)
        with A.mouse_drag(objects) as move_to:
            move_to(tab(frame, first), horizontal_fraction=0.8)
        expect_order(inst, order)
        expect_active(frame, third)
        A.mouse_click(tab(frame, first), button=3, horizontal_fraction=0.3)
        expect_active(frame, third)
        A.press_keys("Escape")
        expect(tab_rpc_counts(inst) == before, "排序和取消零新增业务请求及摘页",
               (before, tab_rpc_counts(inst)))

    with step("activate_execute_and_close_after_cancellation"):
        for title, (editor, value) in editors.items():
            A.mouse_click(tab(frame, title), horizontal_fraction=0.3)
            expect_active(frame, title)
            expect(A.text_value(editor).startswith(f"SELECT {value};"), "原编辑器的 SQL 保留")
            page = ops.active_query_page(frame, title)
            A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 8)
            ops.execute_sql(page)
            expect(len(ops.wait_query_done(page, min_rows=1)) == 1, "会话可继续执行 SQL")
        A.mouse_click(tab(frame, second), button=2, horizontal_fraction=0.3)
        cancel_close(inst)
        expect_active(frame, third)
        for sequence, title in enumerate((first, second, third), 1):
            A.mouse_click(tab(frame, title), horizontal_fraction=0.3)
            expect_active(frame, title)
            if sequence == 1:
                active_tab = tab(frame, title)
                A.find_one(active_tab, role_in=("button",), name="关闭标签")
                A.mouse_click_surface(pointer_surface_point(inst, "[qa.tabs] close-point tab-q1 "))
            else:
                A.press_keys("Control_L", "w")
            dialog = A.wait_dialog(inst.app, "关闭标签？")
            A.click(A.find_one(dialog, role_in=("button",), name="放弃更改并关闭"))
            A.wait_until(lambda: title not in ops.query_tab_names(frame), 8, "确认后销毁原标签")
            A.wait_until(lambda: f"标签 tab-q{sequence} 的会话已释放" in inst.log_text(),
                         15, "关闭收到释放回执")
        expect(not ops.query_tab_names(frame), "所有普通页已关闭")


def test_sort_during_transaction_edits_and_running_query(ui_instance):
    """tabs.strip.tab, tabs.close_dialog (A10.5, A11.2, A12.1–3)."""
    inst = ui_instance("tabsortbusy")
    frame = ops.open_table_for_qa(inst, "widgets")
    A.click(A.find_one(frame, role_in=("toggle button",), name="专注模式"))
    A.click(A.find_one(frame, role_in=("button",), name="开始事务"))
    A.find_one(frame, role_in=("button",), name="取消（不开始事务）", timeout=10)
    A.click(A.query(frame, role_in=("button",), name="开始事务")[-1])
    A.find_one(frame, role_in=("button",), name="回滚事务", timeout=15)
    A.click(A.find_one(frame, role_in=("button",), name="新增行：在当前表末尾新增一行"))
    A.wait_enabled(A.find_one(frame, role_in=("button",), name="应用：提交所有脏行的变更"), 10)
    table_title = A.nm(A.find_one(frame, role_in=("page tab",), name_contains="widgets"))
    page = ops.open_sql_page(frame)
    query_title = A.nm(page)
    A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 15)
    ops.write_sql(page, "SELECT 123;")
    A.wait_until(lambda: "库上下文：main" in inst.log_text(), 15, "会话初始请求已完成")

    with step("sort_while_sql_rpc_is_in_flight"):
        pid = json.loads(Path(inst.env["SQARVO_ENDPOINT_FILE"]).read_text())["pid"]
        with _backend_pidfd(pid, inst.proc.pid) as backend:
            signal.pidfd_send_signal(backend, signal.SIGSTOP)
            try:
                ops.execute_sql(page)
                A.wait_until(lambda: "[db-rpc] ExecuteQuery" in inst.log_text(), 5, "查询已发出")
                before = tab_rpc_counts(inst)
                with A.mouse_drag(tab(frame, table_title)) as move_to:
                    move_to(tab(frame, query_title), horizontal_fraction=0.8)
                expect_order(inst, ["tab-q1", "tab-t1"])
                expect_active(frame, query_title)
                expect(tab_rpc_counts(inst) == before, "排序没有取消查询或改变事务",
                       (before, tab_rpc_counts(inst)))
            finally:
                signal.pidfd_send_signal(backend, signal.SIGCONT)
        expect(len(ops.wait_query_done(page, min_rows=1)) == 1, "在途查询继续完成")
        A.mouse_click(tab(frame, table_title), horizontal_fraction=0.3)
        expect_active(frame, "widgets")
        A.find_one(frame, role_in=("button",), name="回滚事务", timeout=8)
        A.wait_enabled(A.find_one(frame, role_in=("button",), name="应用：提交所有脏行的变更"), 8)
        A.press_keys("Control_L", "w")
        cancel_close(inst)


def test_close_receipt_during_sort_preserves_surviving_order(ui_instance):
    """tabs.strip.tab, tabs.strip.tab.close (A10.3, A11.2, A12.5)."""
    inst = ui_instance("tabsortclose").start(
        autoconnect=True, extra_args=("--window-size", "1200x700", "--lang", "zh"),
    )
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db")
    A.click(A.find_one(frame, role_in=("toggle button",), name="专注模式"))
    for index in range(3):
        page = ops.open_sql_page(frame)
        A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 15)
        if index == 0:
            edited = ops.write_sql(page, "SELECT 1;")
    first, second, third = ops.query_tab_names(frame)
    A.wait_until(lambda: inst.log_text().count("库上下文：main") >= 3, 15, "会话初始请求已完成")
    released = inst.log_text().count("请求释放")

    with step("close_another_instance_while_pointer_is_held"):
        with A.mouse_drag(tab(frame, second)) as move_to:
            move_to(tab(frame, first), horizontal_fraction=0.15)
            expect_order(inst, ["tab-q2", "tab-q1", "tab-q3"])
            expect_active(frame, third)
            A.window_action(frame, "win.close-tab")
            A.wait_until(lambda: "标签 tab-q3 的会话已释放" in inst.log_text(),
                         15, "后台任务回执在拖拽时到达")
            A.wait_until(lambda: third not in ops.query_tab_names(frame), 8, "关闭实例已销毁")
            page = ops.open_sql_page(frame)
            A.wait_enabled(A.find_one(page, role_in=("button",), name="执行"), 15)
            expect(A.nm(page) == "查询 4", "并发新建实例成功")
            move_to(frame, vertical_fraction=1.08)
            expect_order(inst, ["tab-q1", "tab-q2", "tab-q4"])
        expect(third not in ops.query_tab_names(frame), "取消未复活已关闭实例")
        expect(A.text_value(edited).startswith("SELECT 1;"), "原 SQL 编辑器仍在")
        expect(inst.log_text().count("请求释放") == released + 1,
               "只有真实关闭释放会话", inst.log_text().count("请求释放"))
        expect("原生临时摘页" not in inst.log_text(), "排序和关闭没有原生转移")
