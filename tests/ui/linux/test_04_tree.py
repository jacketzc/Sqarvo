"""数据库对象树导航（任务 §6.6）：连接 → 库 → 种类分组 → 表。"""
import pytest
from gi.repository import Atspi

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.sql


def test_tree_navigation(ui_instance):
    """数据库对象树导航：连接 → 库 → 种类分组 → 表，含收起往返。

    注册表组件: object_tree.tree, object_tree.tree.row.conn,
    object_tree.tree.row.expander, object_tree.tree.row.catalog,
    object_tree.tree.row.group, object_tree.tree.row.object。
    """
    inst = ui_instance("navigation_round_trip").start(autoconnect=True)
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db", timeout=40)

    with step("expand_connection"):
        conn_row = ops.find_row(frame, "uitest.db")
        expect(conn_row is not None, "连接行存在", ops.tree_rows(frame)[:4])
        if ops.find_row(frame, "main") is None:
            ops.expand_tree_row(conn_row)
        main = A.wait_until(lambda: ops.find_row(frame, "main"), 10,
                            "展开连接后出现 main 库行")
        expect(main is not None, "main 库行出现", None)

    with step("expand_schema"):
        if ops.find_row(frame, "Tables") is None:
            ops.expand_tree_row(main)
        tables = A.wait_until(lambda: ops.find_row(frame, "Tables"), 10,
                              "展开 main 后出现 Tables 分组行")
        expect(tables is not None, "Tables 分组行出现", ops.tree_rows(frame)[:6])

    with step("expand_tables_group"):
        ops.expand_tree_row(tables)
        widgets = A.wait_until(lambda: ops.find_row(frame, "widgets"), 15,
                               "展开 Tables 后出现 widgets 表行")
        expect(widgets is not None, "widgets 表行出现", ops.tree_rows(frame)[:8])

    with step("collapse_round_trip"):
        # 同一行再点一次 = 收起（tree.rs：已连接行点击为展开/收起切换）。
        ops.expand_tree_row(conn_row)
        # 收起后 main 行不再 showing（树逐层实现，子行销毁）。
        gone = A.wait_until(
            lambda: not _row_showing(frame, "uitest.db", "main"), 10,
            "收起连接后子行消失")
        expect(gone, "收起连接后 main 子行不可见", None)
        ops.expand_tree_row(conn_row)
        A.wait_until(lambda: ops.find_row(frame, "main"), 10, "重新展开恢复 main 行")


def _row_showing(frame, *must):
    row = ops.find_row(frame, *must)
    if row is None:
        return False
    return "showing" in A.states(row)


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

    with step("explicit_browse_uses_fixed_objects"):
        ops.open_sql_page(frame)
        active = A.nm(frame)
        if ops.find_row(navigation, "main") is None:
            ops.expand_tree_row(row("uitest.db"))
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
        ops.expand_tree_row(row("uitest.db"))
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
        A.set_text(entry, "widgets")
        before = inst.log_text().count("[qa.metadata] submit ")
        A.click(A.find_one(navigation, role_in=("button",), name="搜索已连接的数据库"))
        A.wait_until(lambda: inst.log_text().count("[qa.metadata] submit ") > before,
                     10, "显式远程按钮派发 MetadataHub 请求")
        A.set_text(entry, "no_matching_loaded_resource")
        A.find_one(navigation, role_in=("label",), name="已加载的资源中没有匹配项", timeout=10)
        expect(ops.find_row(navigation, "widgets") is None, "远程结果未覆盖本地筛选")

    with step("reveal_catalog_and_clear_selection"):
        A.window_action(frame, "win.tab-2")
        A.wait_until(lambda: A.nm(frame) == active, 10, "定位前活动工作区是查询")
        A.set_text(entry, "main")
        A.click(A.find_one(navigation, role_in=("button",), name="搜索已连接的数据库"))
        A.find_one(navigation, role_in=("button",), name="返回本地筛选", timeout=10)
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
