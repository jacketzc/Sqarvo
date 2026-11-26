"""S2 layout commands through native controls."""
from contextlib import contextmanager
import ctypes as C
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import subprocess

import pytest
from gi.repository import Atspi

import adapter as A
import ops

pytestmark = pytest.mark.smoke


def layout_button(frame, name):
    return A.find_one(frame, role_in=("toggle button",), name=name)


def pressed(frame, name):
    return "pressed" in A.states(layout_button(frame, name))


def client_size(frame):
    size = Atspi.Component.get_extents(frame, Atspi.CoordType.WINDOW)
    return size.width, size.height


def resize(width, height):
    """Request a client resize through X11 on the runner's private display."""
    with A._xtest_display() as (x11, _, display):
        prop = subprocess.check_output(["xprop", "-root", "_NET_ACTIVE_WINDOW"], text=True)
        window = re.search(r"0x[0-9a-f]+", prop)
        if not window or int(window[0], 16) == 0:
            raise A.UIError("no active client on isolated display")
        x11.XResizeWindow.argtypes = [C.c_void_p, C.c_ulong, C.c_uint, C.c_uint]
        x11.XResizeWindow(display, int(window[0], 16), width, height)



@pytest.mark.parametrize("connected", [False, True])
def test_focus_and_navigation(ui_instance, connected):
    instance = ui_instance("layoutconn" if connected else "layoutbare")
    instance.start(autoconnect=connected, extra_args=("--window-size", "1220x750"))
    frame = instance.frame()
    if connected:
        ops.wait_connected(frame, "uitest.db")
    nav = "导航"
    focus = "专注模式"
    inspector = "信息面板"

    with ops.step("resize_and_restore"):
        A.wait_until(lambda: pressed(frame, inspector), 15, "inspector initially shown")
        resize(840, 600)
        A.wait_until(lambda: not pressed(frame, inspector), 10,
                     f"inspector folded (resize from {client_size(frame)})")
        ops.expect(client_size(frame) == (840, 600),
                   "Compact 窗口可缩至 840×600", client_size(frame))
        menu = A.find_one(frame, role_in=("toggle button",), name="主菜单（文件 / 视图 / MCP / 帮助）")
        ops.expect("showing" in A.states(menu), "最小窗口可访问主菜单", A.states(menu))
        for name in ("连接", "新建查询"):
            button = A.find_one(frame, role_in=("button",), name=name)
            ops.expect("showing" in A.states(button), f"{name}可达", A.states(button))
        ops.expect(pressed(frame, nav), "导航仍可见", pressed(frame, nav))
        A.click(layout_button(frame, inspector))
        A.wait_until(lambda: pressed(frame, inspector) and not pressed(frame, nav),
                     5, "inspector temporarily prioritized")
        resize(1220, 750)
        A.wait_until(lambda: pressed(frame, nav) and pressed(frame, inspector),
                     10, "both panels restored")

    with ops.step("focus_and_exit"):
        A.click(layout_button(frame, focus))
        A.wait_until(lambda: pressed(frame, focus), 5, "focus mode active")
        ops.expect(not pressed(frame, nav) and not pressed(frame, inspector),
                   "专注模式折叠两栏", (pressed(frame, nav), pressed(frame, inspector)))
        A.click(layout_button(frame, nav))
        A.wait_until(lambda: pressed(frame, nav) and pressed(frame, inspector),
                     5, "panels restored upon focus exit")
        ops.expect(not pressed(frame, focus), "导航按钮退出专注模式", pressed(frame, focus))
        A.press_keys("Control_L", "f")
        A.wait_until(lambda: any("focused" in A.states(entry) for entry in
                                 A.query(group(frame, "导航"), role_in=("entry",))),
                     5, "Ctrl+F focuses navigation search")


def group(frame, name):
    return A.find_one(frame, role_in=("grouping",), name=name)


def panel_pointer(frame, *, drag=0, inspector=False):
    """Operate the divider between the accessible Navigation and Workspace groups."""
    nav_region = group(frame, "导航")
    nav = Atspi.Component.get_extents(nav_region, Atspi.CoordType.WINDOW)
    pane = Atspi.Component.get_extents(nav_region.get_parent(), Atspi.CoordType.WINDOW)
    center = Atspi.Component.get_extents(group(frame, "工作区"), Atspi.CoordType.WINDOW)
    left, right = nav, center
    if inspector:
        left = center
        right = Atspi.Component.get_extents(group(frame, "信息面板"), Atspi.CoordType.WINDOW)
    with A._xtest_display() as (x11, xtest, display):
        prop = subprocess.check_output(["xprop", "-root", "_NET_ACTIVE_WINDOW"], text=True)
        window = re.search(r"0x[0-9a-f]+", prop)
        info = subprocess.check_output(["xwininfo", "-id", window[0]], text=True)
        ox, oy = map(int, re.findall(r"Absolute upper-left [XY]:\s*(-?\d+)", info))
        # GTK's WINDOW-relative coordinates start at the contents, while X11's
        # client also includes the symmetric resize border in this runner.
        ox += (client_size(frame)[0] - pane.width) // 2 - pane.x
        if right.x <= left.x + left.width:
            raise A.UIError(f"no divider between visible panels {left.x, left.width} and {right.x}")
        x, y = ox + (left.x + left.width + right.x) // 2, oy + left.y + left.height // 2
        xtest.XTestFakeMotionEvent.argtypes = [C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong]
        xtest.XTestFakeButtonEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        xtest.XTestFakeMotionEvent(display, -1, x, y, 0)
        xtest.XTestFakeButtonEvent(display, 1, True, 0)
        if drag:
            xtest.XTestFakeMotionEvent(display, -1, x + drag // 2, y, 75)
            xtest.XTestFakeMotionEvent(display, -1, x + drag, y, 75)
        xtest.XTestFakeButtonEvent(display, 1, False, 75)
        if not drag:
            xtest.XTestFakeButtonEvent(display, 1, True, 75)
            xtest.XTestFakeButtonEvent(display, 1, False, 75)


def test_handle_preferences_and_reset(ui_instance):
    inst = ui_instance("layoutdrag").start(extra_args=("--window-size", "1300x750"))
    frame = inst.frame()
    A.find_one(frame, role_in=("grouping",), name="导航", timeout=15)
    prefs = inst.home / "config" / "sqarvo" / "Sqarvo" / "window.json"

    def read():
        return json.loads(prefs.read_text()) if prefs.exists() else {}

    with ops.step("drag_and_reclaim"):
        panel_pointer(frame, drag=60)
        try:
            preferred = A.wait_until(lambda: read().get("last_sidebar_width", 0) > 260
                                     and read()["last_sidebar_width"], 5, "drag persisted")
        except A.UIError as err:
            raise A.UIError(f"{err}; current saved preferences: {read()}") from err
        resize(840, 600)
        A.wait_until(lambda: not pressed(frame, "信息面板"), 5, "inspector folded")
        ops.expect(read()["last_sidebar_width"] == preferred, "缩小窗口保留首选宽度", read())
        resize(1300, 750)
        A.wait_until(lambda: pressed(frame, "信息面板"), 5, "inspector restored")
        panel_pointer(frame)
        A.wait_until(lambda: read().get("last_sidebar_width") == 260, 5, "double click reset")
        panel_pointer(frame, drag=-50, inspector=True)
        A.wait_until(lambda: read().get("last_inspector_width", 0) > 280, 5, "inspector drag persisted")
        ops.expect(read()["last_sidebar_width"] == 260, "拖动右栏保留左栏首选宽度", read())
        panel_pointer(frame, inspector=True)
        A.wait_until(lambda: read().get("last_inspector_width") == 280, 5, "inspector double click reset")

    with ops.step("reset_layout"):
        resize(840, 600)
        A.wait_until(lambda: not pressed(frame, "信息面板"), 5, "inspector folded")
        A.click(layout_button(frame, "导航"))
        A.wait_until(lambda: read().get("sidebar_shown") is False, 5, "user preference saved")
        A.click(layout_button(frame, "专注模式"))
        A.window_action(frame, "win.reset-layout")
        A.wait_until(lambda: pressed(frame, "导航") and not pressed(frame, "专注模式"),
                     5, "layout reset")
        A.wait_until(lambda: read().get("sidebar_shown") is True, 5, "reset preferences persisted")
        ops.expect(client_size(frame) == (840, 600),
                   "重置恢复显示偏好并保留窗口尺寸", read())

    with ops.step("restart_restores_normal_geometry_and_maximized"):
        A.click(layout_button(frame, "专注模式"))
        A.wait_until(lambda: pressed(frame, "专注模式"), 5, "focus active before restart")
        A.window_action(frame, "window.toggle-maximized")
        A.wait_until(lambda: read().get("maximized") is True, 10, "maximized state persisted")
        ops.expect((read()["window_width"], read()["window_height"]) == (840, 600),
                   "最大化保留正常窗口尺寸", read())
        ops.close_window(frame)
        A.wait_gone(lambda: A.find_app("sqarvo-desktop"), 15, "native close completed")
        inst.stop()
        inst.start()
        frame = inst.frame()
        A.wait_until(lambda: client_size(frame)[0] > 840, 15, "maximized geometry restored")
        A.find_one(frame, role_in=("grouping",), name="全局状态栏", timeout=15)
        A.window_action(frame, "window.toggle-maximized")
        A.wait_until(lambda: client_size(frame) == (840, 600), 10, "normal geometry restored")
        A.wait_until(lambda: pressed(frame, "导航"), 5, "focus preference not persisted")
        ops.expect(not pressed(frame, "专注模式"), "重启清除专注运行时状态", read())


@pytest.mark.sql
def test_table_layout_preserves_transaction_and_unsaved_edit(ui_instance):
    inst = ui_instance("layouttransaction")
    frame = ops.open_table_for_qa(inst, "widgets")

    def button(name):
        return A.find_one(frame, role_in=("button",), name=name)

    def rpc_count():
        return inst.log_text().count("[qa.rpc] dispatch")

    with ops.step("start_transaction_and_add_row"):
        A.click(button("开始事务"))
        A.find_one(frame, role_in=("button",), name="取消（不开始事务）", timeout=10)
        A.click(A.query(frame, role_in=("button",), name="开始事务")[-1])
        A.find_one(frame, role_in=("button",), name="回滚事务", timeout=15)
        A.click(button("新增行：在当前表末尾新增一行"))
        apply = button("应用：提交所有脏行的变更")
        A.wait_enabled(apply, 10, "unsaved row present")
        before = rpc_count()
        ops.expect(before > 0, "计数器已记录准备阶段 RPC", before)

    with ops.step("layout_during_transaction"):
        resize(840, 600)
        A.wait_until(lambda: not pressed(frame, "信息面板"), 10, "inspector folded")
        A.click(layout_button(frame, "专注模式"))
        A.wait_until(lambda: pressed(frame, "专注模式"), 5, "focus entered")
        A.click(layout_button(frame, "专注模式"))
        A.wait_until(lambda: not pressed(frame, "专注模式"), 5, "focus exited")
        A.window_action(frame, "win.reset-layout")
        ops.expect(client_size(frame) == (840, 600), "有活动事务的数据页支持 Compact 最小窗口", client_size(frame))
        ops.expect(button("回滚事务") is not None, "事务仍有效", ops.labels(frame))
        ops.expect("sensitive" in A.states(button("应用：提交所有脏行的变更")),
                   "未保存行仍在", ops.labels(frame))
        ops.expect(rpc_count() == before, "布局动作零新增数据库 RPC", (before, rpc_count()))
        center = group(frame, "工作区")
        ops.expect(A.find_one(center, role_in=("button",), name=A.nm(apply)) is not None,
                   "行操作挂在中央工作区", A.nm(center))
        for name in ("首页（offset = 0）", "上一页", "下一页", "末页（需要总数）"):
            pager = A.find_one(center, role_in=("button",), name=name)
            ops.expect("showing" in A.states(pager), "分页控件挂在中央工作区", name)
        status = group(frame, "全局状态栏")
        labels = ops.labels(status)
        for prefix in ("SQL:", "Last Refresh:", "MCP："):
            ops.expect(any(label.startswith(prefix) for label in labels),
                       f"全局状态栏保留 {prefix} 信息", labels)
        ops.expect("widgets" in A.nm(frame), "保留活动数据表工作区", A.nm(frame))


def test_layout_preference_survives_first_connection(ui_instance):
    inst = ui_instance("layouttransition")
    legacy = inst.home / "config" / "sqarvo" / "Sqarvo" / "connections.json"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(json.dumps({
        "version": 1,
        "connections": [{
            "name": "LayoutDB", "alias": "LayoutDB", "driver": "sqlite",
            "file_path": str(inst.db), "host": "", "port": 0, "database": "",
            "username": "", "read_only": False, "tls_mode": "disabled",
            "credential_ref": "",
        }],
    }))
    inst.start(extra_args=("--window-size", "1300x750"))
    frame = inst.frame()
    with ops.step("first_connection_preserves_layout"):
        row = ops.find_row(frame, "LayoutDB", timeout=15)
        A.wait_until(lambda: pressed(frame, "信息面板"), 5, "inspector visible on startup")
        A.click(layout_button(frame, "信息面板"))
        A.wait_until(lambda: not pressed(frame, "信息面板"), 5, "inspector actively hidden")
        A.mouse_click(row, horizontal_fraction=0.65)
        A.press_keys("Return")
        ops.wait_connected(frame, "LayoutDB")
        A.wait_until(lambda: "[bench]" in inst.log_text() and "state=Workspace" in inst.log_text(),
                     10, "connected workspace mounted")
        ops.expect(not pressed(frame, "信息面板") and pressed(frame, "导航"),
                   "连接后沿用启动态布局", ops.labels(frame))


@pytest.mark.sql
def test_query_text_survives_layout_at_minimum_size(ui_instance):
    inst = ui_instance("layoutsql").start(autoconnect=True, extra_args=("--window-size", "840x600"))
    frame = inst.frame()
    A.find_one(frame, role_in=("grouping",), name="工作区", timeout=15)
    A.wait_enabled(A.find_one(frame, role_in=("button",), name="新建查询"), 20)
    with ops.step("query_at_minimum"):
        page = ops.open_sql_page(frame)
        editor = ops.editor(page)
        sql = "SELECT name FROM widgets ORDER BY id"
        A.set_text(editor, sql)
        A.click(layout_button(frame, "专注模式"))
        A.wait_until(lambda: pressed(frame, "专注模式"), 5, "focus entered")
        A.window_action(frame, "win.reset-layout")
        A.wait_until(lambda: not pressed(frame, "专注模式"), 5, "focus cleared")
        ops.expect(A.text_value(editor) == sql, "布局操作保留 SQL 文本", A.text_value(editor))
        ops.expect(client_size(frame) == (840, 600), "SQL 工作区可在 840×600 使用", client_size(frame))
        run = A.find_one(page, role_in=("button",), name="执行", timeout=10)
        A.click(run)
        rows = ops.wait_query_done(page, min_rows=3)
        ops.expect(len(rows) == 3, "最小窗口可执行并读到查询结果", rows)


def test_density_changes_keep_panel_preferences(ui_instance):
    inst = ui_instance("layoutdensity")
    prefs = inst.home / "config" / "sqarvo" / "Sqarvo" / "window.json"
    prefs.parent.mkdir(parents=True)
    prefs.write_text(json.dumps({
        "version": 2, "tier": "compact", "theme": "dark", "language": "zh",
        "last_sidebar_width": 320, "last_inspector_width": 350,
    }))
    inst.start()
    frame = inst.frame()
    A.find_one(frame, role_in=("grouping",), name="全局状态栏", timeout=15)
    with ops.step("density_round_trip"):
        for tier, target in (("comfortable", (1050, 750)), ("large", (1260, 900)), ("compact", (840, 600))):
            log_offset = len(inst.log_text())
            A.window_action(frame, f"win.tier-{tier}")

            def tier_configured():
                # The application records geometry after native size changes
                # settle. The window manager can clamp the requested default.
                actual = re.search(
                    r"窗口几何 (\d+)x(\d+).*已记入",
                    inst.log_text()[log_offset:],
                )
                return (
                    actual is not None
                    and client_size(frame) == tuple(map(int, actual.groups()))
                    and json.loads(prefs.read_text()).get("tier") == tier
                )

            A.wait_until(tier_configured, 5, f"{tier} configured and persisted")
            resize(*target)
            A.wait_until(lambda: client_size(frame) == target, 10, f"{tier} native minimum")
            ops.expect("sensitive" in A.states(layout_button(frame, "导航")),
                       "密度切换后布局控件可用", A.states(layout_button(frame, "导航")))
        saved = json.loads(prefs.read_text())
        ops.expect((saved["last_sidebar_width"], saved["last_inspector_width"]) == (320, 350),
                   "切档与窗口收缩不改首选宽度", saved)
        A.window_action(frame, "win.reset-layout")
        A.wait_until(lambda: json.loads(prefs.read_text())["last_sidebar_width"] == 260, 5, "reset persisted")
        saved = json.loads(prefs.read_text())
        ops.expect((saved["tier"], saved["theme"], saved["language"]) == ("compact", "dark", "zh"),
                   "重置布局保留档位、主题和语言", saved)


def test_sidebar_hide_moves_keyboard_focus(ui_instance):
    """shell.layout, shell.toolbar.layout: move focus to the Navigation button."""
    frame = ui_instance("layoutfocus").start().frame()
    A.find_one(frame, role_in=("grouping",), name="导航", timeout=15)
    A.window_action(frame, "win.focus-search")
    A.wait_until(lambda: any("focused" in A.states(entry) for entry in
                             A.query(group(frame, "导航"), role_in=("entry",))),
                 5, "navigation search focused")
    A.window_action(frame, "win.toggle-sidebar")
    A.wait_until(lambda: not pressed(frame, "导航") and
                 "focused" in A.states(layout_button(frame, "导航")), 5, "focus fallback")
    resize(1400, 750)
    A.wait_until(lambda: client_size(frame)[0] == 1400, 5, "window resized")
    ops.expect(not pressed(frame, "导航"), "主动隐藏的导航在扩宽后保持隐藏")


@pytest.mark.sql
def test_bottom_state_follows_active_workspace(ui_instance):
    """status_bar.row1.detail_toggle, status_bar.row1.pager, status_bar.row2.sql."""
    inst = ui_instance("layoutstatus")
    with sqlite3.connect(inst.db) as con:
        con.executemany("INSERT INTO widgets(name, qty) VALUES (?, ?)", [("paging", 1)] * 502)
    frame = ops.open_table_for_qa(inst, "widgets")
    center = group(frame, "工作区")

    def status():
        return ops.labels(group(frame, "全局状态栏"))

    def wait_sql(text):
        A.wait_until(lambda: any(line.startswith("SQL:") and text in line for line in status()),
                     15, f"active SQL status contains {text}")

    def next_page():
        return A.find_one(center, role_in=("button",), name="下一页")

    def detail():
        return A.find_one(center, role_in=("toggle button",), name_contains="字段 / 值")

    def add_row():
        return A.find_one(center, role_in=("button",), name="新增行：在当前表末尾新增一行")

    def inactive_controls():
        ops.expect("sensitive" not in A.states(next_page()), "当前页面分页按钮禁用")
        ops.expect("sensitive" not in A.states(add_row()), "当前页面新增行禁用")
        ops.expect("pressed" not in A.states(detail()), "当前页面详情开关复位")
        ops.expect(not any(s.startswith("Last Refresh:") for s in status()),
                   "不显示其他数据页的最后刷新时间", status())

    with ops.step("table_and_objects"):
        wait_sql("widgets")
        A.wait_enabled(next_page(), 15)
        A.wait_enabled(add_row(), 10)
        data_row = A.find_one(center, role_in=("table row",), name_contains="bolt", timeout=10)
        ops.expect(Atspi.Selection.select_child(data_row.get_parent(), data_row.get_index_in_parent()),
                   "选中数据表中的 bolt 行")
        A.wait_until(lambda: "sensitive" in A.states(detail()), 5, "table detail available")
        A.click(detail())
        A.wait_until(lambda: "pressed" in A.states(detail()), 5, "table detail active")
        A.window_action(frame, "win.tab-1")
        wait_sql("尚未执行")
        inactive_controls()
        A.window_action(frame, "win.tab-2")
        wait_sql("widgets")
        row = ops.find_row(frame, "widgets_empty", timeout=15)
        A.mouse_click(row, horizontal_fraction=0.65)
        A.press_keys("Return")
        wait_sql("widgets_empty")
        A.wait_until(lambda: "sensitive" not in A.states(next_page()), 10, "empty table has no next page")
        A.window_action(frame, "win.tab-1")
        wait_sql("尚未执行")
        inactive_controls()
        A.window_action(frame, "win.tab-2")
        wait_sql("widgets")
        A.wait_enabled(next_page(), 15)

    with ops.step("blank_and_executed_query"):
        page = ops.open_sql_page(frame)
        wait_sql("尚未执行")
        inactive_controls()
        ops.write_sql(page, "SELECT 42")
        ops.execute_sql(page)
        ops.wait_query_done(page, min_rows=1)
        ops.expect(any(s.startswith("SQL:") and "尚未执行" not in s and "widgets" not in s
                       for s in status()), "全局状态跟随当前 SQL 页", status())

    with ops.step("disconnect_and_switch"):
        # The fixture owns this companion process. Check ownership and pin the
        # process before sending a signal on the isolated host.
        endpoint = Path(inst.env["SQARVO_ENDPOINT_FILE"])
        pid = json.loads(endpoint.read_text())["pid"]
        with _backend_pidfd(pid, inst.proc.pid) as fd:
            signal.pidfd_send_signal(fd, signal.SIGTERM)
        wait_sql("连接已断开")
        for tab in ("win.tab-1", "win.tab-2", "win.tab-3", "win.tab-4"):
            A.window_action(frame, tab)
            wait_sql("连接已断开")
            ops.expect("sensitive" not in A.states(next_page()), "失连页分页禁用", tab)


@contextmanager
def _backend_pidfd(pid, owner):
    fd = os.pidfd_open(pid)
    try:
        proc = Path("/proc") / str(pid)
        parent = re.search(r"^PPid:\s*(\d+)$", (proc / "status").read_text(), re.M)
        ops.expect(parent is not None and int(parent[1]) == owner, "后端属于当前隔离夹具")
        yield fd
    finally:
        os.close(fd)
