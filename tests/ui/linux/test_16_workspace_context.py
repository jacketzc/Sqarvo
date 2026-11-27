"""S1 context commands use the selected profile and actual workspace independently."""
import json
import sqlite3

import pytest
from gi.repository import Atspi

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.sql


def test_tab_names_distinguish_different_workspace_types(ui_instance):
    """tabs.strip.tab.table, tabs.strip.tab.query (A05.3, A10.4)."""
    inst = ui_instance("workspace_tab_types")
    with sqlite3.connect(inst.db) as connection:
        connection.execute('CREATE TABLE "查询 1" (value INTEGER)')
    frame = ops.open_table_for_qa(inst, "查询 1")
    A.find_one(frame, role_in=("page tab",), name="查询 1", timeout=10)

    A.press_keys("Control_L", "t")
    for title in ("表数据 · 查询 1", "SQL 查询 · 查询 1"):
        A.find_one(frame, role_in=("page tab",), name=title, timeout=10)
    query = ops.active_query_page(frame, "SQL 查询 · 查询 1")
    A.wait_enabled(A.find_one(query, role_in=("button",), name="执行"), 15)
    A.wait_until(lambda: A.nm(frame) == "查询 1 @ uitest.db — Sqarvo", 8,
                 "窗口标题继续使用工作区原始名称")
    A.press_keys("Control_L", "w")
    A.wait_gone(lambda: A.query(frame, role_in=("page tab",), name="SQL 查询 · 查询 1"),
                8, "关闭独立查询实例")
    A.find_one(frame, role_in=("page tab",), name="表数据 · 查询 1", timeout=8)


def test_long_tab_name_remains_accessible_with_tab_commands(ui_instance):
    """shell.title, shell.toolbar.target, tabs.strip.tab.table, tabs.strip.list (A05.4–5)."""
    inst = ui_instance("workspace_tab_long_name")
    long_name = "customer_order_reconciliation_" + "warehouse_" * 12 + "west"
    with sqlite3.connect(inst.db) as connection:
        connection.execute(f'CREATE TABLE "{long_name}" (value INTEGER)')
    inst.start(autoconnect=True, extra_args=(
        "--qa-tree-expand", "UITestDB", "--qa-tree-expand", "main", "--qa-tree-expand", "main",
        "--qa-tree-activate", long_name, "--window-size", "840x600", "--lang", "zh",
    ))
    frame = inst.frame()
    page = A.find_one(frame, role_in=("page tab",), name=long_name, timeout=15)
    A.wait_until(lambda: long_name in (page.get_description() or ""), 8,
                 "标签提供完整名称及可访问描述")
    A.wait_until(lambda: A.nm(frame) == f"{long_name} @ uitest.db — Sqarvo", 10,
                 "长工作区名称保留原生窗口标题")

    A.click(A.find_one(frame, role_in=("toggle button",), name="所有标签"))
    A.find_one(frame, role_in=("label",), name_contains=long_name, timeout=8)
    A.press_keys("Escape")
    A.click(A.find_one(frame, role_in=("button",), name_contains="新建查询标签"))
    dialog = A.wait_dialog(inst.app, "新建查询")
    A.find_one(dialog, role_in=("label",), name_contains="目标：uitest.db · main")
    expect(not ops.query_tab_names(frame), "标题截断时先确认完整目标")
    A.click(A.find_one(dialog, role_in=("button",), name="继续"))
    ops.active_query_page(frame, "查询 1")


def test_navigation_target_and_clear_selection(ui_instance):
    """object_tree.tree, object_tree.tree.row.conn, shell.menu.clear_selection,
    shell.menu.new_tab, shell.toolbar.new_query, shell.toolbar.tables, shell.toolbar.views,
    shell.title, shell.toolbar.target, tabs.strip.tab.query, object_tree.ctx_menu, object_tree.ctx_menu.disconnect,
    object_tree.ctx_menu.delete_connection, object_tree.hint,
    objects_page.table, objects_page.filter, objects_page.table.row, objects_page.empty,
    objects_page.table.row.menu, objects_page.table.row.menu.open,
    inspector.connection, tabs.strip.tab.table, object_tree.tree.row.object,
    object_tree.ctx_menu.open, object_tree.ctx_menu.edit_connection.confirm.
    """
    inst = ui_instance("workspace_context")
    peer = inst.db.with_name("peer.db")
    for path, value in ((inst.db, "A"), (peer, "B")):
        with sqlite3.connect(path) as con:
            con.execute("CREATE TABLE orders(owner TEXT)")
            con.execute("INSERT INTO orders VALUES (?)", (value,))
            if path == peer:
                con.execute("CREATE TABLE peer_only (id INTEGER)")
                con.execute("CREATE VIEW peer_view AS SELECT owner FROM orders")
    legacy = inst.home / "config" / "sqarvo" / "Sqarvo" / "connections.json"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(json.dumps({
        "version": 1,
        "connections": [{
            "name": "Peer", "alias": "Peer", "driver": "sqlite", "file_path": str(peer),
            "host": "", "port": 0, "database": "", "username": "", "read_only": True,
            "tls_mode": "disabled", "credential_ref": "",
        }],
    }), encoding="utf-8")
    script = inst.home / "connect-peer.txt"
    script.write_text("expand Peer\n", encoding="utf-8")
    inst.start(autoconnect=True, extra_args=("--qa-tabs-file", str(script)))
    frame = inst.frame()
    ops.wait_connected(frame, "uitest.db", timeout=40)
    ops.wait_connected(frame, "Peer", timeout=40)

    def select_row(*labels):
        row = ops.find_row(frame, *labels, timeout=15)
        label = A.find_one(row, role_in=("label",), name=labels[0])
        if labels[0] == "main":
            A.mouse_click(label, button=3)
            A.press_keys("Escape")
        else:
            A.mouse_click(label)
        A.wait_until(lambda: "selected" in A.states(row), 10, f"{labels} 已选中")
        return row

    def browse(kind, expected):
        A.click(A.find_one(frame, role_in=("button",), name=kind))
        objects = A.find_one(frame, role_in=("grouping",), name="Objects", timeout=10)
        A.find_one(objects, role_in=("label",), name=expected, timeout=15)

    def capture_tree_menu(action, *labels):
        def label():
            row = ops.find_row(frame, *labels, exact_name=labels[0])
            return next(iter(A.query(row, role_in=("label",), name=labels[0])), None) if row else None

        return ops.click_tree_until(label, lambda: next(
            (menu for menu in A.query(frame, role_in=("menu",))
             if action in A.action_names(menu)), None),
            button=3, what=f"资源菜单动作 {action}")

    def tree_menu(action, *labels):
        A.window_action(capture_tree_menu(action, *labels), action)

    def query_owner(page, owner, suffix=""):
        execute = A.find_one(page, role_in=("button",), name="执行")
        A.wait_until(lambda: "sensitive" in A.states(execute), 15, "独立查询会话就绪")
        ops.write_sql(page, f"SELECT owner || '{suffix}' FROM orders")
        ops.execute_sql(page)
        A.wait_until(lambda: any(owner + suffix in row for row in ops.data_rows(page)),
                     15, f"同名表返回连接 {owner} 的数据")

    with step("active_a_navigation_b"):
        primary = select_row("uitest.db", "已连接")
        first = ops.open_sql_page(frame)
        query_owner(first, "A")
        title_a = A.nm(frame)
        select_row("Peer", "已连接")
        panel = A.find_one(frame, role_in=("grouping",), name="信息面板", timeout=10)
        A.find_one(panel, role_in=("label",), name="Peer", timeout=10)
        expect(A.nm(frame) == title_a, "选中 B 时窗口标题仍属于 A", A.nm(frame))
        A.find_one(frame, role_in=("label",), name="目标：Peer", timeout=10)
        tables = A.find_one(frame, role_in=("button",), name="Tables")
        expect("sensitive" not in A.states(tables), "只选择连接时分类不可用")
        expect(A.nm(frame) == title_a, "只选择连接时不借用 A 的库", A.nm(frame))

        ops.activate_connection_row(primary)
        A.wait_until(lambda: sum("main" in labels for labels in ops.tree_rows(frame)) == 1,
                     10, "折叠 A 后只显示 B 的 main")
        select_row("main", "库")
        browse("Tables", "peer_only")
        browse("Views", "peer_view")
        expect("Peer" in A.nm(frame), "Objects 与导航使用同一完整目标", A.nm(frame))

        A.window_action(frame, "win.tab-2")
        A.click(A.find_one(frame, role_in=("button",), name="全部折叠"))
        A.wait_gone(lambda: ops.find_row(frame, "main", "库"), 10, "B 的 main 被折叠")
        search = A.find_one(frame, role_in=("entry",), name="搜索资源…")
        A.set_text(search, "no_loaded_peer_match")
        A.find_one(frame, role_in=("label",), name="已加载的资源中没有匹配项", timeout=10)
        second = ops.open_sql_page(frame)
        query_owner(second, "B")
        expect("Peer" in A.nm(frame), "隐藏并过滤后新标签仍实际绑定 B", A.nm(frame))
        A.wait_gone(lambda: A.query(frame, role_in=("label",), name="目标：Peer · main"), 8,
                    "同一个有效数据库在导航折叠和过滤后也不重复提示")
        A.set_text(search, "")
        A.window_action(frame, "win.tab-2")
        A.find_one(frame, role_in=("label",), name="目标：Peer · main", timeout=8)

    with step("table_workspace_reuse_by_profile"):
        def open_orders(label):
            ops.activate_connection_row(ops.find_row(frame, label, "已连接", timeout=10))
            ops.expand_tree_row(ops.find_row(frame, "main", "库", timeout=10))
            ops.expand_tree_row(ops.find_row(frame, "Tables", timeout=10))
            tree_menu("ctx.open-object", "orders", "表")
            A.wait_until(lambda: A.nm(frame) == f"orders @ {label} — Sqarvo",
                         10, f"数据工作区属于 {label}")

        open_orders("uitest.db")
        A.find_one(frame, role_in=("page tab",), name="orders", timeout=10)
        tree_menu("ctx.open-object", "orders", "表")
        A.wait_until(lambda: "复用已有表标签" in inst.log_text(), 10, "重复打开复用工作区")
        expect(inst.log_text().count("新建表标签") == 1, "同一资源只建立一个工作区")
        ops.activate_connection_row(ops.find_row(frame, "uitest.db", "已连接"))
        A.wait_gone(lambda: ops.find_row(frame, "main", "库"), 10, "A 的库已经隐藏")
        open_orders("Peer")
        expect(inst.log_text().count("新建表标签") == 2, "同名表按档案区分为两个工作区")
        for title in ("orders @ uitest.db", "orders @ Peer"):
            A.find_one(frame, role_in=("page tab",), name=title, timeout=10)
        A.window_action(frame, "win.tab-2")

    with step("objects_filter_never_crosses_profile_or_category"):
        def objects():
            return A.find_one(frame, role_in=("grouping",), name="Objects", timeout=10)

        def filter_entry():
            return A.find_one(objects(), role_in=("entry", "text"), name="筛选对象…", timeout=10)

        def filtered_order():
            A.set_text(filter_entry(), "OrDeRs")
            return A.find_one(objects(), role_in=("label",), name="orders", timeout=10)

        select_row("main", "库")
        connect_calls = inst.log_text().count("[qa.rpc] dispatch Connect")
        browse("Tables", "peer_only")
        expect(inst.log_text().count("[qa.rpc] dispatch Connect") == connect_calls,
               "Objects 使用既有浏览会话")
        A.mouse_click(filtered_order(), count=2)
        A.wait_until(lambda: A.nm(frame) == "orders @ Peer — Sqarvo", 10,
                     "过滤后双击使用 B 的同名对象")
        A.window_action(frame, "win.tab-1")
        expect(A.text_value(filter_entry()) == "OrDeRs", "返回 B 保留检索词")
        browse("Views", "peer_view")
        expect(A.text_value(filter_entry()) == "", "同连接分类切换清词")
        browse("Tables", "peer_only")
        filtered_order()
        with step("ddl_in_a_does_not_invalidate_b"):
            before = inst.log_text().count("[qa.metadata] submit objects.nodes ")
            A.window_action(frame, "win.tab-2")
            sql = "CREATE TABLE primary_only_ddl(id INTEGER)"
            ops.write_sql(first, sql)
            ops.execute_sql(first)
            ops.switch_results_tab(first, "消息")
            A.wait_until(lambda: sql in ops.messages_log_text(first)
                         and "> Query Time:" in ops.messages_log_text(first),
                         20, "A 的 DDL 已完成")
            with sqlite3.connect(inst.db.as_uri() + "?mode=ro", uri=True) as con:
                expect(con.execute("SELECT 1 FROM sqlite_master WHERE name='primary_only_ddl'")
                       .fetchone() == (1,), "独立连接确认 A 的结构已提交")
            A.window_action(frame, "win.tab-1")
            expect(A.text_value(filter_entry()) == "OrDeRs", "B 保留检索词")
            A.find_one(objects(), role_in=("label",), name="orders", timeout=10)
            expect(inst.log_text().count("[qa.metadata] submit objects.nodes ") == before,
                   "A 的 DDL 不触发 B 的列表刷新")
        ops.activate_connection_row(ops.find_row(frame, "Peer", "已连接", timeout=10))
        A.wait_gone(lambda: ops.find_row(frame, "main", "库"), 10, "折叠 B 以定位 A")
        ops.activate_connection_row(ops.find_row(frame, "uitest.db", "已连接", timeout=10))
        select_row("main", "库")
        browse("Tables", "widgets")
        expect(A.text_value(filter_entry()) == "", "同名数据库和分类换连接清词")
        expect(not A.query(objects(), role_in=("label",), name="peer_only"),
               "B 的独有对象不出现在 A")
        A.mouse_click(filtered_order(), button=3)
        menu = A.wait_until(lambda: next(
            (menu for menu in A.query(objects(), role_in=("menu",))
             if "ctx.open-object" in A.action_names(menu)), None), 10, "Objects 右键菜单")
        A.window_action(menu, "ctx.open-object")
        A.wait_until(lambda: A.nm(frame) == "orders @ uitest.db — Sqarvo", 10,
                     "过滤后右键使用 A 的同名对象")
        expect(inst.log_text().count("新建表标签") == 2, "两个精确身份分别复用已有工作区")
        ops.activate_connection_row(ops.find_row(frame, "uitest.db", "已连接", timeout=10))
        A.wait_gone(lambda: ops.find_row(frame, "main", "库"), 10, "恢复 B 树视图前折叠 A")
        ops.activate_connection_row(ops.find_row(frame, "Peer", "已连接", timeout=10))
        main = select_row("main", "库")
        browse("Tables", "peer_only")
        if ops.find_row(frame, exact_name="Tables") is None:
            ops.expand_tree_row(main)
        if ops.find_row(frame, "orders", "表") is None:
            ops.expand_tree_row(ops.find_row(frame, exact_name="Tables", timeout=10))
        ops.find_row(frame, "orders", "表", timeout=10)
        filtered_order()
        A.window_action(frame, "win.tab-2")

    with step("menu_keeps_its_target_and_rejects_renamed_row"):
        menu = capture_tree_menu("ctx.open-object", "orders", "表")
        primary = ops.find_row(frame, "uitest.db", "已连接", timeout=10)
        expect(Atspi.Selection.select_child(primary.get_parent(), primary.get_index_in_parent()),
               "菜单打开期间改变 GTK 可见选择")
        A.window_action(menu, "ctx.open-object")
        A.wait_until(lambda: A.nm(frame) == "orders @ Peer — Sqarvo",
                     10, "改选 A 后菜单仍打开原先捕获的 B")

        A.window_action(frame, "win.tab-2")
        menu = capture_tree_menu("ctx.open-object", "orders", "表")
        with sqlite3.connect(peer) as con:
            con.execute("ALTER TABLE orders RENAME TO renamed_orders")
        A.window_action(frame, "win.refresh")
        A.wait_gone(lambda: ops.find_row(frame, "表", exact_name="orders"), 10, "旧资源行已移除")
        A.window_action(menu, "ctx.open-object")
        A.find_one(frame, role_in=("label",), name="所选资源已不可用，请重新选择或清除选择。",
                   timeout=10)
        if ops.find_row(frame, "Tables") is None:
            ops.expand_tree_row(ops.find_row(frame, "main", "库", timeout=10))
        if ops.find_row(frame, "renamed_orders") is None:
            ops.expand_tree_row(ops.find_row(frame, "Tables", timeout=10))
        ops.find_row(frame, exact_name="renamed_orders", timeout=10)
        unavailable = "所选资源已不可用，请重新选择或清除选择。"
        refusals = inst.log_text().count(unavailable)
        before = ops.query_tab_names(frame)
        ops.open_query_page_via_action(frame)
        A.wait_until(lambda: inst.log_text().count(unavailable) > refusals, 10,
                     "已删除的逻辑目标仍拦截新建查询")
        expect(ops.query_tab_names(frame) == before, "旧对象失效后没有改投 A")
        tree_menu("ctx.open-object", "renamed_orders", "表")
        A.wait_until(lambda: A.nm(frame) == "renamed_orders @ Peer — Sqarvo",
                     10, "新名称使用 B 的独立资源身份")
        expect(inst.log_text().count("新建表标签") == 3, "重命名后的资源不会复用旧表标签")
        with sqlite3.connect(peer) as con:
            con.execute("ALTER TABLE renamed_orders RENAME TO orders")
        A.window_action(frame, "win.tab-2")

    with step("disconnect_does_not_fall_back"):
        tree_menu("ctx.disconnect", "Peer", "已连接")
        dialog = A.wait_until(lambda: A.find_dialog(inst.app, "断开连接？"),
                              10, "主动断开确认")
        A.click(A.find_one(dialog, role_in=("button",), name="断开连接"))
        A.wait_until(lambda: "连接「Peer」的浏览会话已断开" in inst.log_text(),
                     15, "B 的浏览会话已断开")
        before_objects = inst.log_text().count("[qa.metadata] submit objects.nodes ")
        A.window_action(frame, "win.tab-1")
        A.find_one(objects(), role_in=("label",), name="没有选中对象", timeout=10)
        expect(A.text_value(filter_entry()) == "", "范围失效清空检索词")
        expect(inst.log_text().count("[qa.metadata] submit objects.nodes ") == before_objects,
               "B 的范围失效后不改投 A")
        A.window_action(frame, "win.tab-2")
        before = ops.query_tab_names(frame)
        ops.open_query_page_via_action(frame)
        A.find_one(frame, role_in=("label",), name="所选资源已不可用，请重新选择或清除选择。",
                   timeout=15)
        expect(ops.query_tab_names(frame) == before, "失效的 B 不会创建绑定 A 的查询")
        A.window_action(frame, "win.tab-3")
        query_owner(second, "B", "-after-disconnect")
        A.window_action(frame, "win.tab-2")

    with step("alias_updates_the_same_inspector_preview"):
        select_row("uitest.db", "已连接")
        select_row("Peer")
        panel = A.find_one(frame, role_in=("grouping",), name="信息面板", timeout=10)
        A.find_one(panel, role_in=("label",), name="Peer", timeout=10)
        metadata = inst.log_text().count("[qa.metadata] submit ")
        tree_menu("ctx.edit-connection", "Peer")
        edit = A.wait_dialog(inst.app, "编辑连接")
        alias = A.find_one(edit, role_in=("text", "entry"), name="别名（可选）")
        A.set_text(alias, "Peer-renamed")
        A.click(A.find_one(edit, role_in=("button",), name="保存"))
        ops.find_row(frame, "Peer-renamed", timeout=10)
        A.find_one(panel, role_in=("label",), name="Peer-renamed", timeout=10)
        A.find_one(frame, role_in=("page tab",), name="orders @ Peer-renamed", timeout=10)
        expect(A.nm(frame) == title_a, "B 改名不改变 A 查询归属")
        expect(inst.log_text().count("[qa.metadata] submit ") == metadata,
               "Inspector 同步连接名没有新增元数据请求")

    with step("closing_a_collision_keeps_the_surviving_qualifier"):
        A.window_action(frame, "win.tab-5")
        A.wait_until(lambda: A.nm(frame) == "orders @ Peer-renamed — Sqarvo", 10,
                     "关闭第二个同名实例")
        A.window_action(frame, "win.close-tab")
        A.wait_gone(lambda: A.query(frame, role_in=("page tab",), name="orders @ Peer-renamed"),
                    10, "同名实例已关闭")
        A.find_one(frame, role_in=("page tab",), name="orders @ uitest.db", timeout=10)
        A.window_action(frame, "win.tab-2")

    with step("deleted_profile_still_does_not_fall_back"):
        tree_menu("ctx.delete-connection", "Peer-renamed")
        dialog = A.wait_until(lambda: A.find_dialog(inst.app, "删除连接？"),
                              10, "删除隔离档案确认")
        A.click(A.find_one(dialog, role_in=("button",), name="删除连接"))
        A.wait_gone(lambda: ops.find_row(frame, "Peer-renamed"), 15, "删除后的档案已从树移除")
        ops.open_query_page_via_action(frame)
        A.find_one(frame, role_in=("label",), name="所选资源已不可用，请重新选择或清除选择。",
                   timeout=15)
        expect(ops.query_tab_names(frame) == before, "已删除的 B 不会创建绑定 A 的查询")

    with step("clear_selection_uses_actual_workspace"):
        A.window_action(frame, "win.clear-selection")
        third = ops.open_sql_page(frame)
        query_owner(third, "A")

    with step("edit_confirmation_keeps_the_connection_target"):
        tree_menu("ctx.edit-connection", "uitest.db", "已连接")
        dialog = A.wait_until(lambda: A.find_dialog(inst.app, "编辑连接？"),
                              10, "编辑已连接档案的确认框")
        A.find_one(dialog, role_in=("label",), name_contains="未保存 SQL")
        A.click(A.find_one(dialog, role_in=("button",), name="取消"))
        A.wait_gone(lambda: A.find_dialog(inst.app, "编辑连接？"), 10, "取消编辑确认")
        expect(A.text_value(ops.editor(third)).startswith("SELECT"), "取消保留已绑定 SQL")
        tree_menu("ctx.edit-connection", "uitest.db", "已连接")
        dialog = A.wait_dialog(inst.app, "编辑连接？")
        A.click(A.find_one(dialog, role_in=("button",), name="关闭并编辑"))
        A.wait_until(lambda: A.find_dialog(inst.app, "编辑连接"),
                     10, "有效的原连接可进入编辑")
