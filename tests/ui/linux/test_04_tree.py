"""数据库对象树导航（任务 §6.6）：连接 → 库 → 种类分组 → 表。"""
import pytest
from gi.repository import Atspi

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.sql


@pytest.mark.parametrize("tier", ("compact", "comfortable", "large"))
def test_tree_navigation(ui_instance, tier):
    """object_tree.tree, object_tree.tree.row, object_tree.tree.row.conn,
    object_tree.tree.row.expander, object_tree.tree.row.catalog,
    object_tree.tree.row.group, object_tree.tree.row.object, object_tree.header.collapse_all.
    """
    import json
    import shutil

    inst = ui_instance(f"navigation_{tier}")
    peer = inst.db.with_name("peer.db")
    shutil.copyfile(inst.db, peer)
    legacy = inst.home / "config" / "sqarvo" / "Sqarvo" / "connections.json"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(json.dumps({"version": 1, "connections": [
        {"name": name, "alias": name, "driver": "sqlite", "file_path": str(path),
         "host": "", "port": 0, "database": "", "username": "", "read_only": True,
         "tls_mode": "disabled", "credential_ref": ""}
        for name, path in (("Primary", inst.db), ("Peer", peer))
    ]}), encoding="utf-8")
    inst.start(extra_args=("--lang", "zh", "--tier", tier))
    frame = inst.frame()
    navigation = A.find_one(frame, role_in=("grouping",), name="导航", timeout=15)

    def resource(name):
        return ops.find_row(navigation, exact_name=name, timeout=10)

    def click_resource(name, count=1):
        row = resource(name)
        A.mouse_click(A.find_one(row, role_in=("label",), name=name), count=count)

    def root_state(name, state):
        row = resource(name)
        A.wait_until(lambda: state in A.description(row), 8, f"{name} 的可访问描述为 {state}")
        expect(not A.query(row, role_in=("button",), name="展开或折叠"),
               "顶层行不提供独立箭头操作")
        return row

    def business_count():
        return inst.log_text().count("[qa.rpc] dispatch")

    with step("offline_selection_then_double_click_connects_only_the_target"):
        root_state("Primary", "未连接")
        root_state("Peer", "未连接")
        click_resource("Primary")
        primary = resource("Primary")
        A.wait_until(lambda: "selected" in A.states(primary), 8, "单击选中离线档案")
        A.press_keys("Right")
        root_state("Primary", "未连接")
        expect("main" not in [text for row in ops.tree_rows(navigation) for text in row],
               "单击和右键方向键保持离线目录隐藏")
        expect("[qa.rpc] dispatch Connect" not in inst.log_text(), "单击和方向键不连接")
        click_resource("Peer")
        click_resource("Primary", count=2)
        ops.wait_connected(frame, "Primary", timeout=20)
        root_state("Primary", "已展开")
        root_state("Peer", "未连接")
        resource("main")

    with step("cached_round_trip_by_double_click_arrows_and_enter"):
        before = business_count()
        click_resource("Peer")
        click_resource("Primary", count=2)
        A.wait_gone(lambda: ops.find_row(navigation, exact_name="main"), 8, "双击收起已连接档案")
        root_state("Primary", "已折叠")
        click_resource("Peer")
        click_resource("Primary", count=2)
        resource("main")
        A.press_keys("Left")
        A.wait_gone(lambda: ops.find_row(navigation, exact_name="main"), 8, "左方向键收起")
        A.press_keys("Right")
        resource("main")
        root_state("Primary", "已展开")
        ops.activate_connection_row(resource("Primary"))
        A.wait_gone(lambda: ops.find_row(navigation, exact_name="main"), 8, "Enter 收起")
        ops.activate_connection_row(resource("Primary"))
        resource("main")
        expect(business_count() == before, "反复开合有效缓存不发请求")

    with step("collapse_all_keeps_selection_and_connection"):
        primary = resource("Primary")
        expect("selected" in A.states(primary), "连接仍是当前逻辑选择")
        before = business_count()
        A.click(A.find_one(navigation, role_in=("button",), name="全部折叠"))
        root_state("Primary", "已折叠")
        expect("selected" in A.states(resource("Primary")), "全部折叠保留逻辑选择")
        ops.wait_connected(frame, "Primary")
        expect(business_count() == before, "全部折叠不请求数据或断开")

    with step("database_arrow_and_object_activation"):
        ops.activate_connection_row(resource("Primary"))
        before_browse = inst.log_text().count("[qa.metadata] submit objects.nodes ")
        ops.expand_tree_row(resource("main"))
        resource("Tables")
        expect(inst.log_text().count("[qa.metadata] submit objects.nodes ") == before_browse,
               "数据库独立箭头不浏览 Objects")
        click_resource("main")
        A.wait_until(lambda: "Primary" in A.nm(frame), 10, "数据库单击进入固定 Objects")
        ops.expand_tree_row(resource("Tables"))

        def widgets_label():
            if row := ops.find_row(navigation, exact_name="widgets"):
                return A.find_one(row, role_in=("label",), name="widgets")

        ops.click_tree_until(widgets_label, lambda: A.nm(frame) == "widgets @ Primary — Sqarvo",
                             count=2, timeout=15, what="具体对象双击打开正确工作区")


def test_navigation_pointer_and_filter(ui_instance):
    """object_tree, object_tree.tree.row, object_tree.search.box, object_tree.search.remote,
    object_tree.search.status, object_tree.search.results, object_tree.ctx_menu,
    object_tree.header, object_tree.header.collapse_all, object_tree.header.more,
    object_tree.header.more.clear_selection, objects_page.
    """
    inst = ui_instance("navigation_filter").start(autoconnect=True, extra_args=("--lang", "zh"))
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db", timeout=40)
    navigation = A.find_one(frame, role_in=("grouping",), name="导航")
    entry = A.find_one(navigation, role_in=("text", "entry"), name="搜索资源…")

    def row(name):
        return ops.find_row(navigation, name, timeout=10)

    def click_name(name, **kwargs):
        A.mouse_click(A.find_one(row(name), role_in=("label",), name=name), **kwargs)

    def more_item(name):
        A.click(A.find_one(navigation, role_in=("toggle button",), name="更多"))
        menu = A.find_one(frame, role_in=("menu",), name="更多", timeout=10)
        return A.find_one(menu, role_in=("menu item",), name=name, timeout=10)

    with step("explicit_browse_uses_fixed_objects"):
        ops.open_sql_page(frame)
        active = A.nm(frame)
        if ops.find_row(navigation, "main") is None:
            ops.activate_connection_row(row("uitest.db"))
        ops.expand_tree_row(row("main"))
        expect(A.nm(frame) == active, "独立箭头不切换工作区", A.nm(frame))
        click_name("main")
        A.wait_until(lambda: A.nm(frame) != active, 10, "数据库单击切换 Objects")
        A.find_one(frame, role_in=("grouping",), name="Objects", timeout=10)
        A.window_action(frame, "win.tab-2")
        click_name("Views", button=3)
        expect(A.nm(frame) == active, "右键只选择资源", A.nm(frame))
        A.press_keys("Escape")
        before_browse = inst.log_text().count("[qa.metadata] submit objects.nodes ")
        click_name("Views", count=2)
        A.wait_until(lambda: A.nm(frame) != active, 10, "分类双击进入 Objects")
        expect(inst.log_text().count("[qa.metadata] submit objects.nodes ") == before_browse + 1,
               "分类双击只请求一次 Objects")
        expect(len(ops.query_tab_names(frame)) == 1, "使用现有固定 Objects")

    with step("keyboard_and_programmatic_selection"):
        A.window_action(frame, "win.tab-2")
        click_name("uitest.db")
        before = inst.log_text().count("[qa.rpc] dispatch")
        tables = row("Tables")
        expect(Atspi.Selection.select_child(tables.get_parent(), tables.get_index_in_parent()),
               "程序化选中分类")
        expect(A.nm(frame) == active, "程序化选择保持查询标签", A.nm(frame))
        click_name("uitest.db")
        A.press_keys("Down")
        A.wait_until(lambda: "selected" in A.states(row("main")), 10, "方向键选择数据库")
        expect(A.nm(frame) == active and inst.log_text().count("[qa.rpc] dispatch") == before,
               "方向键和程序化选择零 RPC")
        A.press_keys("Return")
        A.wait_until(lambda: A.nm(frame) != active, 10, "数据库 Enter 主动浏览")

    with step("collapsed_cache_and_search_are_local"):
        ops.expand_tree_row(row("Tables"))
        row("widgets")
        ops.expand_tree_row(row("Tables"))
        A.wait_gone(lambda: ops.find_row(navigation, "widgets"), 10, "正常树的分组已折叠")
        before = inst.log_text().count("[qa.rpc] dispatch")
        expect(before > 0, "确认已启用业务 RPC 计数")
        A.set_text(entry, "WiDgEt")
        row("widgets")
        A.click(A.find_one(navigation, role_in=("button",), name="全部折叠"))
        A.wait_gone(lambda: ops.find_row(navigation, "widgets"), 10, "搜索内收起路径")
        A.set_text(entry, "WiDgE")
        expect(ops.find_row(navigation, "widgets") is None, "改词保留搜索中的折叠")
        A.set_text(entry, "no_matching_loaded_resource")
        A.find_one(navigation, role_in=("label",), name="已加载的资源中没有匹配项", timeout=10)
        A.set_text(entry, "view")
        ops.activate_connection_row(row("uitest.db"))
        ops.expand_tree_row(row("main"))
        click_name("Views", button=3)
        A.wait_until(lambda: "selected" in A.states(row("Views")), 10, "搜索中明确选择 Views")
        A.press_keys("Escape")
        A.set_text(entry, "")
        row("Tables")
        expect(ops.find_row(navigation, "widgets") is None, "清词恢复普通树折叠")
        expect("selected" in A.states(row("Views")), "清词保留搜索期间用户替换的选择")
        expect(inst.log_text().count("[qa.rpc] dispatch") == before,
               "输入、改词及清词没有新增业务 RPC")

    with step("explicit_remote_dispatch"):
        expect(not A.query(navigation, role_in=("button",), name="搜索已连接的数据库"),
               "导航没有常驻远程搜索按钮")
        expect("sensitive" not in A.states(more_item("搜索已连接的数据库")),
               "空关键词时远程搜索不可用")
        A.press_keys("Escape")
        A.press_keys("Control_L", "f")
        A.set_text(entry, "widgets")
        before = inst.log_text().count("[qa.metadata] submit ")
        A.press_keys("Return")
        expect(inst.log_text().count("[qa.metadata] submit ") == before,
               "搜索框 Enter 保持本地筛选")
        remote = more_item("搜索已连接的数据库")
        A.wait_enabled(remote, 10)
        A.press_keys("Down")
        A.wait_until(lambda: "focused" in A.states(remote), 5, "更多菜单键盘聚焦远程搜索")
        A.press_keys("Return")
        A.wait_until(lambda: inst.log_text().count("[qa.metadata] submit ") > before,
                     10, "更多菜单显式派发 MetadataHub 请求")
        A.set_text(entry, "no_matching_loaded_resource")
        A.find_one(navigation, role_in=("label",), name="已加载的资源中没有匹配项", timeout=10)
        expect(ops.find_row(navigation, "widgets") is None, "远程结果未覆盖本地筛选")

    with step("reveal_catalog_and_clear_selection"):
        A.window_action(frame, "win.tab-2")
        A.wait_until(lambda: A.nm(frame) == active, 10, "定位前活动工作区是查询")
        A.set_text(entry, "main")
        A.click(more_item("搜索已连接的数据库"))
        A.click(more_item("返回本地筛选"))
        expect(A.text_value(entry) == "main", "返回本地筛选保留关键词")
        A.click(more_item("搜索已连接的数据库"))
        results = A.find_one(navigation, role_in=("list",), name="远程搜索结果", timeout=10)
        def remote_main():
            if result := ops.find_row(results, exact_name="main"):
                return A.find_one(result, role_in=("label",), name="main")

        ops.click_tree_until(remote_main, lambda: A.text_value(entry) == "" and
                             "selected" in A.states(row("main")),
                             timeout=15, what="远程库结果 Reveal 后清词并选中库")
        expect(A.nm(frame) == active, "Reveal 保持活动查询工作区", A.nm(frame))
        A.set_text(entry, "does_not_match")
        A.find_one(navigation, role_in=("label",),
                   name="已选资源当前不可见，可从更多菜单清除选择。", timeout=10)
        A.click(A.find_one(navigation, role_in=("toggle button",), name="更多"))
        menu = A.find_one(frame, role_in=("menu",), name="更多", timeout=10)
        A.wait_until(lambda: any("focused" in A.states(item)
                                 for item in A.query(menu, role_in=("menu item",))),
                     10, "更多菜单聚焦清除选择项")
        A.press_keys("Return")
        A.wait_gone(lambda: A.query(navigation, role_in=("label",),
                                   name="已选资源当前不可见，可从更多菜单清除选择。"),
                    10, "清除隐藏的逻辑选择")


def test_filter_can_page_without_matches(ui_instance):
    """object_tree.tree.row.placeholder, object_tree.search.status: 手动分页后重新过滤。"""
    import sqlite3

    inst = ui_instance("navigation_pagination")
    with sqlite3.connect(inst.db) as db:
        for index in range(510):
            db.execute(f"CREATE TABLE a_s4_{index:04d}(id INTEGER)")
        db.execute("CREATE TABLE zz_s4_last(id INTEGER)")
    inst.start(autoconnect=True, extra_args=("--lang", "zh"))
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db", timeout=40)
    navigation = A.find_one(frame, role_in=("grouping",), name="导航")
    entry = A.find_one(navigation, role_in=("entry",), name="搜索资源…")
    main = ops.find_row(navigation, "main", timeout=10)
    ops.expand_tree_row(main)
    tables = ops.find_row(navigation, "Tables", timeout=10)
    ops.expand_tree_row(tables)
    A.wait_until(lambda: "has_more=true" in inst.log_text(), 10, "首批表数据已返回")
    before = inst.log_text().count("[qa.rpc] dispatch")
    A.set_text(entry, "zz_s4_last")
    A.find_one(navigation, role_in=("label",), name="已加载的资源中没有匹配项", timeout=10)
    more = ops.find_row(navigation, "加载更多", timeout=10)
    expect(inst.log_text().count("[qa.rpc] dispatch") == before, "无匹配时未自动分页")
    A.click(next(button for button in A.query(more, role_in=("button",))
                 if A.nm(button).startswith("加载更多")))
    ops.find_row(navigation, "zz_s4_last", timeout=10)
    expect(inst.log_text().count("[qa.rpc] dispatch") == before + 1,
           "只对用户主动分页新增一个 RPC", inst.log_text().count("[qa.rpc] dispatch") - before)


def test_local_refresh_error_can_retry_while_filtered(ui_instance):
    """object_tree.ctx_menu.refresh_group, object_tree.tree.row.placeholder,
    object_tree.tree.row.group, object_tree.search.status: 保留旧数据并显式重试。
    """
    import sqlite3

    inst = ui_instance("navigation_retry").start(autoconnect=True, extra_args=("--lang", "zh"))
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db", timeout=40)
    navigation = A.find_one(frame, role_in=("grouping",), name="导航")
    entry = A.find_one(navigation, role_in=("entry",), name="搜索资源…")
    ops.expand_tree_row(ops.find_row(navigation, exact_name="main", timeout=10))
    expect(ops.find_row(navigation, exact_name="Functions") is None
           and ops.find_row(navigation, exact_name="Queries") is None, "导航隐藏未实现分类")
    views = ops.find_row(navigation, exact_name="Views", timeout=10)
    ops.expand_tree_row(views)
    ops.find_row(navigation, "这一组没有视图", timeout=10)
    tables = ops.find_row(navigation, exact_name="Tables", timeout=10)
    ops.expand_tree_row(tables)
    ops.find_row(navigation, exact_name="widgets", timeout=10)

    with step("failed_refresh_keeps_cached_rows_and_exposes_retry"):
        lock = sqlite3.connect(inst.db)
        try:
            lock.execute("BEGIN EXCLUSIVE")
            menu = ops.click_tree_until(
                lambda: A.find_one(ops.find_row(navigation, exact_name="Tables", timeout=10),
                                   role_in=("label",), name="Tables"),
                lambda: next((menu for menu in A.query(frame, role_in=("menu",))
                              if "ctx.refresh-group" in A.action_names(menu)), None),
                button=3, what="分类刷新菜单")
            A.window_action(menu, "ctx.refresh-group")
            A.wait_until(lambda: any(A.nm(node).startswith("重试：") for node in
                                    A.query(navigation, role_in=("button",))),
                         15, "SQLite 锁导致真实读取失败")
            ops.find_row(navigation, exact_name="widgets", timeout=10)
            before = inst.log_text().count("[qa.rpc] dispatch")
            A.set_text(entry, "zz_after_retry")
            A.find_one(navigation, role_in=("label",), name="已加载的资源中没有匹配项", timeout=10)
            expect(inst.log_text().count("[qa.rpc] dispatch") == before,
                   "过滤时保留 Retry，未自动重试")
        finally:
            lock.close()

    with step("manual_retry_refilters_new_data"):
        with sqlite3.connect(inst.db) as db:
            db.execute("CREATE TABLE zz_after_retry(id INTEGER)")
        retry = next(button for button in A.query(navigation, role_in=("button",))
                     if A.nm(button).startswith("重试："))
        A.click(retry)
        ops.find_row(navigation, exact_name="zz_after_retry", timeout=10)
        expect(inst.log_text().count("[qa.rpc] dispatch") == before + 1,
               "手动重试只读一次、重新过滤成功")
