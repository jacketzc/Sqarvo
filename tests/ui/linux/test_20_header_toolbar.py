"""S7 header and toolbar through native window controls and the shared commands."""
import ctypes as C
from itertools import cycle
import json
import re
import sqlite3
import subprocess

import pytest
from gi.repository import Atspi

import adapter as A
import ops
from test_10_layout import client_size, resize

pytestmark = pytest.mark.smoke


def test_native_header_window_controls(ui_instance):
    """shell.header, shell.title, shell.window, shell.menu (A05.5, A14.3)."""
    inst = ui_instance("nativeheader")
    inst.start(extra_args=("--window-size", "840x600", "--lang", "en"))
    frame = inst.frame()
    title = A.find_one(frame, role_in=("label",), name="Sqarvo", timeout=12)
    menu = A.find_one(frame, role_in=("toggle button",), name_contains="Main Menu")
    connect = A.find_one(frame, role_in=("button",), name="Connect")

    def active_client():
        return re.search(r"0x[0-9a-f]+", subprocess.check_output(
            ["xprop", "-root", "_NET_ACTIVE_WINDOW"], text=True))[0]

    window = active_client()

    def maximized():
        return "_NET_WM_STATE_MAXIMIZED_HORZ" in subprocess.check_output(
            ["xprop", "-id", window, "_NET_WM_STATE"], text=True)

    with ops.step("move_and_maximize_using_the_title"):
        origin = A._pointer_origin()
        title_bounds = Atspi.Component.get_extents(title, Atspi.CoordType.WINDOW)
        action_bounds = Atspi.Component.get_extents(connect, Atspi.CoordType.WINDOW)
        x = title_bounds.x + title_bounds.width // 2
        y = title_bounds.y + title_bounds.height // 2
        stride = action_bounds.height
        offsets = cycle((stride, 2 * stride))
        with A.mouse_drag(title, horizontal_fraction=0.5) as drag:
            def moved():
                # Crossing the drag threshold hands control to the WM. Poll with
                # motion near the original title so window buttons remain on-screen.
                drag(surface_point=(x + next(offsets), y + stride))
                return A._pointer_origin() != origin

            A.wait_until(moved, 8, "WM tracking the native title drag")
        A.wait_until(lambda: A._pointer_origin() != origin, 8, "native title drag moved the window")
        A.mouse_click(title, count=2)
        A.wait_until(maximized, 10, "native title double-click maximized")

    with ops.step("native_controls_and_application_menu"):
        def control(label):
            return A.find_one(frame, role_in=("button",), name=label, timeout=10)

        A.click(control("Maximize"))
        A.wait_until(lambda: not maximized(), 10, "restore button")
        A.click(control("Maximize"))
        A.wait_until(maximized, 10, "maximize button")
        A.click(control("Maximize"))
        A.wait_until(lambda: not maximized(), 10, "restore button")
        A.click(menu)
        A.find_one(frame, role_in=("menu",), timeout=8)
        A.press_keys("Escape")
        A.wait_gone(lambda: A.query(frame, role_in=("menu",)), 8, "application menu dismissed")
        A.mouse_click(title, button=3)
        A.find_one(frame, role_in=("menu",), timeout=8)
        A.press_keys("Escape")
        A.wait_gone(lambda: A.query(frame, role_in=("menu",)), 8, "native title menu dismissed")

        A.click(control("Minimize"))
        A.wait_until(lambda: active_client() != window, 8, "minimize button")
        with A._xtest_display() as (x11, _, display):
            x11.XMapRaised.argtypes = [C.c_void_p, C.c_ulong]
            x11.XMapRaised(display, int(window, 16))
        A.wait_until(lambda: "showing" in A.states(frame), 8, "window restored after minimize")
        A.click(control("Close"))
        A.wait_gone(lambda: A.find_app("sqarvo-desktop"), 15, "native close button")


def start_on_named_database(inst, alias):
    with sqlite3.connect(inst.db) as db:
        db.execute("CREATE TABLE inventory(id INTEGER)")
    legacy = inst.home / "config" / "sqarvo" / "Sqarvo" / "connections.json"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(json.dumps({
        "version": 1,
        "connections": [{
            "name": alias, "alias": alias, "driver": "sqlite", "file_path": str(inst.db),
            "host": "", "port": 0, "database": "", "username": "", "read_only": True,
            "tls_mode": "disabled", "credential_ref": "",
        }],
    }), encoding="utf-8")
    inst.start(extra_args=("--window-size", "840x600", "--lang", "zh",
                           "--qa-tree-expand", alias, "--qa-tree-expand", "main"))
    frame = inst.frame()
    connection = ops.find_row(frame, alias, timeout=20)
    name = A.find_one(connection, role_in=("label",), name=alias)
    A.mouse_click(name, count=2)
    ops.wait_connected(frame, alias, timeout=22)
    main = ops.find_row(frame, "main", "库", timeout=20)
    A.mouse_click(A.find_one(main, role_in=("label",), name="main"), button=3)
    A.press_keys("Escape")
    return frame


def test_more_reflow_and_stale_menu(ui_instance):
    """shell.toolbar.tables, shell.toolbar.views, shell.toolbar.more,
    shell.toolbar.target, shell.title (A06.3–6, A14.3).
    """
    inst = ui_instance("headeroverflow")
    # Long enough to compete with the actual measured categories at compact width.
    alias = "warehouse_" * 9 + "east"
    frame = start_on_named_database(inst, alias)

    def more_button():
        return A.find_one(frame, role_in=("toggle button",), name="更多分类", timeout=10)

    def view_items():
        return A.query(frame, role_in=("check menu item", "menu item", "check box"), name="Views")

    target = f"目标：{alias} · main"
    with ops.step("overflow_and_restore_without_rpc"):
        A.find_one(frame, role_in=("label",), name=target, timeout=10)
        A.wait_until(lambda: client_size(frame) == (840, 600), 8, "Compact 840×600")
        more = more_button()
        # Key controls remain reachable through their names even when showing icons only.
        for name in ("连接", "新建查询", "导航", "专注模式", "信息面板"):
            A.find_one(frame, role_in=("button", "toggle button"), name=name)
        before = inst.log_text().count("[qa.rpc] dispatch")
        A.click(more)
        A.wait_until(view_items, 10, "Views available through More")
        ops.expect(len(view_items()) == 1, "More lists an overflow category once")
        A.press_keys("Escape")
        for _ in range(3):
            resize(1400, 750)
            A.find_one(frame, role_in=("button",), name="Views", timeout=10)
            A.wait_gone(lambda: A.query(frame, name="更多分类"), 8, "no empty More")
            resize(840, 600)
            more_button()
        ops.expect(inst.log_text().count("[qa.rpc] dispatch") == before,
                   "reflow and More open/close dispatch zero business RPCs")

    with ops.step("changing_the_target_dismisses_more"):
        A.click(more_button())
        A.wait_until(view_items, 10, "captured menu open")
        A.window_action(frame, "win.clear-selection")
        A.wait_gone(view_items, 8, "old menu dismissed")
        A.wait_gone(lambda: A.query(frame, role_in=("label",), name=target), 8, "target cleared")
        ops.expect(A.nm(frame) == "Sqarvo", "clearing selection does not create an Objects scope")

    with ops.step("empty_supported_views_and_active_item"):
        main = ops.find_row(frame, "main", "库", timeout=10)
        A.mouse_click(A.find_one(main, role_in=("label",), name="main"), button=3)
        A.press_keys("Escape")
        A.find_one(frame, role_in=("label",), name=target, timeout=10)
        A.click(more_button())
        A.click(A.wait_until(view_items, 10, "Views in the captured menu")[0])
        A.wait_until(lambda: A.nm(frame) == f"Views @ {alias} — Sqarvo"
                     or A.find_dialog(inst.app, "Views"), 12, "More dispatched the Views command")
        if dialog := A.find_dialog(inst.app, "Views"):
            A.find_one(dialog, role_in=("label",), name_contains=target)
            A.click(A.find_one(dialog, role_in=("button",), name="继续"))
        A.wait_until(lambda: A.nm(frame) == f"Views @ {alias} — Sqarvo", 12,
                     "the fixed Objects page uses the actual Views scope")
        A.find_one(frame, role_in=("label",), name="SQLite · main", timeout=8)
        A.find_one(frame, role_in=("label",), name_contains="当前范围没有这类对象", timeout=8)
        A.wait_gone(lambda: A.query(frame, role_in=("label",), name=target), 8,
                    "matching visible navigation and workspace scope hide Target")
        before = inst.log_text().count("[qa.rpc] dispatch")
        A.click(A.find_one(frame, role_in=("toggle button",), name="导航"))
        A.wait_gone(lambda: A.query(frame, role_in=("grouping",), name="导航"), 8,
                    "navigation sidebar hidden")
        A.wait_gone(lambda: A.query(frame, role_in=("label",), name=target), 8,
                    "matching target stays hidden with the sidebar closed")
        plus = A.find_one(frame, role_in=("button",), name_contains="新建查询标签")
        A.wait_until(lambda: target in (plus.get_description() or ""), 8,
                     "the full target stays in the New Query tooltip")
        views = A.find_one(frame, role_in=("button",), name="Views", timeout=10)
        A.wait_until(lambda: "selected" in A.states(views), 8,
                     "Views is active when the suppressed Target frees toolbar space")
        ops.expect(inst.log_text().count("[qa.rpc] dispatch") == before,
                   "hiding Navigation, reflow and More do not browse")
        ops.expect(not ops.query_tab_names(frame), "browsing Views creates no ordinary query tabs")


def test_truncated_target_requires_confirmation_for_the_plus(ui_instance):
    """shell.toolbar.new_query, shell.toolbar.target, tabs.strip.new,
    shell.title, tabs.query.connection_picker (A04.2, A05.4–5, A06.5).
    """
    inst = ui_instance("headertarget")
    alias = "warehouse_" * 19 + "west"
    frame = start_on_named_database(inst, alias)
    target = f"目标：{alias} · main"
    A.find_one(frame, role_in=("label",), name=target, timeout=10)
    plus = A.find_one(frame, role_in=("button",), name_contains="新建查询标签")
    A.wait_until(lambda: target in (plus.get_description() or ""), 8, "plus exposes the exact target")
    before = ops.query_tab_names(frame)
    A.click(plus)
    dialog = A.wait_dialog(inst.app, "新建查询")
    A.find_one(dialog, role_in=("label",), name_contains=target)
    ops.expect(ops.query_tab_names(frame) == before, "query waits for target confirmation")
    A.click(A.find_one(dialog, role_in=("button",), name="取消"))
    A.wait_gone(lambda: A.find_dialog(inst.app, "新建查询"), 8, "cancel target confirmation")
    ops.expect(ops.query_tab_names(frame) == before, "cancel leaves the workspace unchanged")
    with ops.step("confirmation_rejects_changed_navigation_target"):
        toolbar = A.find_one(frame, role_in=("grouping",), name="主工具条")
        query = A.find_one(toolbar, role_in=("button",), name="新建查询")
        ops.expect(query.get_description() == plus.get_description(),
                   "toolbar and plus expose the same full target")
        calls = inst.log_text().count("[qa.rpc] dispatch Connect")
        A.click(query)
        dialog = A.wait_dialog(inst.app, "新建查询")
        A.window_action(frame, "win.clear-selection")
        A.wait_gone(lambda: A.query(frame, role_in=("label",), name=target), 8,
                    "navigation changed while confirming")
        A.click(A.find_one(dialog, role_in=("button",), name="继续"))
        A.wait_gone(lambda: A.find_dialog(inst.app, "新建查询"), 8, "expired confirmation dismissed")
        A.find_one(frame, role_in=("label",), name="所选资源已不可用，请重新选择或清除选择。",
                   timeout=8)
        ops.expect(ops.query_tab_names(frame) == before, "expired confirmation creates no query")
        ops.expect(inst.log_text().count("[qa.rpc] dispatch Connect") == calls,
                   "expired confirmation sends no Connect")
        main = ops.find_row(frame, "main", "库", timeout=10)
        A.mouse_click(A.find_one(main, role_in=("label",), name="main"), button=3)
        A.press_keys("Escape")
        A.find_one(frame, role_in=("label",), name=target, timeout=8)

    A.press_keys("Control_L", "t")
    dialog = A.wait_dialog(inst.app, "新建查询")
    A.click(A.find_one(dialog, role_in=("button",), name="继续"))
    ops.active_query_page(frame, "查询 1")
    A.wait_until(lambda: A.nm(frame) == f"查询 1 @ {alias} — Sqarvo", 15,
                 "the Ctrl+T path bound the confirmed profile")
    A.find_one(frame, role_in=("label",), name="SQLite · main", timeout=10)
    header = A.find_one(frame, role_in=("grouping",), name="标题栏")
    description = header.get_description()
    ops.expect(f"查询 1 @ {alias}" in description and "SQLite · main" in description,
               "title exposes the complete title and actual database")
    ops.expect("已连接" in description and "://" not in description,
               "title description exposes safe status without a connection URI")
    for kind in ("Tables", "Views"):
        ops.expect(all("selected" not in A.states(button) for button in
                       A.query(frame, role_in=("button",), name=kind)),
                   "ordinary SQL does not highlight a category")

    resize(840, 600)
    A.wait_until(lambda: client_size(frame) == (840, 600), 8,
                 "the bound long name leaves Compact 840×600 usable")
    log = inst.log_text()
    before = log.count("[qa.rpc] dispatch")
    hovered = log.count("[qa.header] title tooltip requested")
    with A._xtest_display() as (_, xtest, display):
        A._mouse_motion(xtest, display, A._pointer_target(header, 0.5))
    A.wait_until(lambda: inst.log_text().count("[qa.header] title tooltip requested") > hovered,
                 8, "GTK requested the native title tooltip for the pointer")
    ops.expect(inst.log_text().count("[qa.rpc] dispatch") == before,
               "hovering the title dispatches zero business RPCs")
    with ops.step("matching_but_clipped_title_still_confirms"):
        A.wait_gone(lambda: A.query(frame, role_in=("label",), name=target), 8,
                    "matching scope suppresses even the long Target label")
        plus = A.find_one(frame, role_in=("button",), name_contains="新建查询标签")
        A.click(plus)
        dialog = A.wait_dialog(inst.app, "新建查询")
        A.find_one(dialog, role_in=("label",), name_contains=target)
        A.click(A.find_one(dialog, role_in=("button",), name="取消"))
        A.wait_gone(lambda: A.find_dialog(inst.app, "新建查询"), 8, "full-target confirmation closed")
        ops.expect(len(ops.query_tab_names(frame)) == 1, "canceling confirmation keeps the original query")


@pytest.mark.parametrize("language,toolbar_name,names", [
    ("zh", "主工具条", ("连接", "新建查询", "导航", "专注模式", "信息面板")),
    ("en", "Global toolbar", ("Connect", "New Query", "Navigation", "Focus Mode", "Inspector")),
])
def test_toolbar_commands_reachable_across_density(ui_instance, language, toolbar_name, names):
    """shell.toolbar, shell.toolbar.layout, shell.toolbar.new_query (A02.1, A06.2, A14.3)."""
    inst = ui_instance(f"headerdensity{language}")
    inst.start(extra_args=("--window-size", "840x600", "--lang", language))
    frame = inst.frame()
    toolbar = A.find_one(frame, role_in=("grouping",), name=toolbar_name, timeout=12)
    A.wait_enabled(A.find_one(toolbar, role_in=("button",), name=names[1]), 12)
    A.wait_until(lambda: "[qa.rpc] dispatch GetMcpAccessPolicy" in inst.log_text(),
                 10, "startup RPCs settled")
    before = inst.log_text().count("[qa.rpc] dispatch")
    for tier, size in (("compact", (840, 600)), ("comfortable", (1050, 750)),
                       ("large", (1260, 900)), ("compact", (840, 600))):
        offset = len(inst.log_text())
        A.window_action(frame, f"win.tier-{tier}")
        A.wait_until(lambda: f"→ {tier}（" in inst.log_text()[offset:]
                     or f"已经是 {tier} 档" in inst.log_text()[offset:],
                     8, f"{language} {tier} applied")
        resize(*size)
        A.wait_until(lambda: client_size(frame) == size, 10, f"{language} {tier} window usable")
        for name in names:
            button = A.find_one(toolbar, role_in=("button", "toggle button"), name=name)
            ops.expect({"showing", "sensitive"} <= A.states(button), f"{language} {tier}: {name} reachable")
    ops.expect(inst.log_text().count("[qa.rpc] dispatch") == before,
               "density changes and reflow dispatch no business RPCs")
