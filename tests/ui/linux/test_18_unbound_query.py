"""S3 session and draft lifecycle over the isolated GTK application."""

import json
import signal
from pathlib import Path

import adapter as A
import ops
from ops import expect, step
from test_10_layout import _backend_pidfd


def test_unbound_queries_and_close_guards(ui_instance):
    """startup.workspace, shell.window, shell.menu, shell.menu.quit, shell.menu.new_tab,
    shell.backend_failure, shell.toolbar,
    shell.toolbar.new_query, tabs.strip.new, tabs.strip.tab.query,
    tabs.strip.tab.close, tabs.query.actions, tabs.query.connection_picker,
    tabs.query.state, tabs.close_dialog, tabs.close_dialog.cancel,
    tabs.close_dialog.discard。
    """
    inst = ui_instance("s3draft").start()
    frame = inst.frame()
    with step("ready_without_connection"):
        page = ops.open_sql_page(frame)
        ops.write_sql(page, "SELECT 123;")
        run = A.find_one(page, role_in=("button",), name="执行")
        expect("sensitive" not in A.states(run), "未绑定时禁用执行", A.states(run))
        actions = A.action_names(frame)
        expect(not {"win.execute", "win.begin-txn", "win.commit-txn", "win.rollback-txn", "win.export-csv"}
               .intersection(actions), "未绑定时执行、事务和导出动作不可用", actions)
        A.press_keys("Control_L", "t")
        A.wait_until(lambda: len(ops.query_tab_names(frame)) == 2, 10, "Ctrl+T 创建第二个草稿")
        plus = A.find_one(frame, role_in=("button",), name_contains="新建查询标签")
        A.click(plus)
        A.wait_until(lambda: len(ops.query_tab_names(frame)) == 3, 10, "加号创建第三个草稿")
        ops.close_current_query_tab(frame)
        ops.close_current_query_tab(frame)
        expect(A.text_value(ops.editor(page)).startswith("SELECT 123;"), "草稿文字保留")
        expect("[db-rpc] Connect" not in inst.log_text(), "未申请查询标签会话")
        expect("[db-rpc] ExecuteQuery" not in inst.log_text(), "未执行查询")
    with step("cancel_close_tab"):
        A.window_action(frame, "win.close-tab")
        dialog = A.wait_until(lambda: A.find_dialog(inst.app, "关闭标签？"), 10, "关闭标签确认")
        A.click(A.find_one(dialog, role_in=("button",), name="取消"))
        A.wait_gone(lambda: A.find_dialog(inst.app, "关闭标签？"), 10, "标签关闭确认已退出")
        expect(A.text_value(ops.editor(page)).startswith("SELECT 123;"), "取消关闭后 SQL 保留")
    with step("cancel_close_window"):
        ops.close_window(frame)
        dialog = A.wait_until(lambda: A.find_dialog(inst.app, "有未提交的更改"), 10, "退出确认")
        A.click(A.find_one(dialog, role_in=("button",), name="取消"))
        expect(A.text_value(ops.editor(page)).startswith("SELECT 123;"), "取消关窗后 SQL 保留")
        A.wait_gone(lambda: A.find_dialog(inst.app, "有未提交的更改"), 10, "关窗确认已退出")
    with step("cancel_application_quit"):
        A.press_keys("Control_L", "q")
        dialog = A.wait_dialog(inst.app, "有未提交的更改")
        A.click(A.find_one(dialog, role_in=("button",), name="取消"))
        A.wait_gone(lambda: A.find_dialog(inst.app, "有未提交的更改"), 10, "退出确认已取消")
        expect(A.text_value(ops.editor(page)).startswith("SELECT 123;"), "取消退出后 SQL 保留")
    with step("cancel_language_restart"):
        A.window_action(frame, "win.lang-en")
        restart = A.wait_dialog(inst.app, "切换语言需要重启")
        A.click(A.find_one(restart, role_in=("button",), name="重启"))
        discard = A.wait_dialog(inst.app, "有未提交的更改")
        A.click(A.find_one(discard, role_in=("button",), name="取消"))
        expect(A.text_value(ops.editor(page)).startswith("SELECT 123;"), "取消换语言重启保留编辑器")
        expect(inst.app_alive(), "继续使用同一进程")

    with step("recover_local_service_without_losing_draft"):
        A.wait_gone(lambda: A.find_dialog(inst.app, "有未提交的更改"), 10, "重启守卫已退出")
        pid = json.loads(Path(inst.env["SQARVO_ENDPOINT_FILE"]).read_text())["pid"]
        editor = ops.editor(page)
        with _backend_pidfd(pid, inst.proc.pid) as backend:
            signal.pidfd_send_signal(backend, signal.SIGTERM)
        recover = A.find_one(page, role_in=("button",), name="重启 Sqarvo", timeout=20)
        A.click(recover)
        dialog = A.wait_dialog(inst.app, "Sqarvo 需要重启")
        A.click(A.find_one(dialog, role_in=("button",), name="重启 Sqarvo"))
        A.wait_until(lambda: inst.log_text().count("state=Connected") == 2, 25, "本地服务重新握手")
        expect(A.text_value(editor).startswith("SELECT 123;"), "恢复后保留同一编辑器及 SQL")
        expect("[db-rpc] Connect" not in inst.log_text(), "恢复未自动绑定草稿")


def test_first_connection_preserves_drafts_and_explicit_binding(ui_instance):
    """startup.workspace, tabs.strip.tab.query, tabs.query.connection_picker,
    tabs.query, tabs.query.actions, tabs.query.state, shell.layout, tabs.limit,
    tabs.query.connection_picker.cancel_binding。
    """
    inst = ui_instance("s3bind")
    legacy = inst.home / "config" / "sqarvo" / "Sqarvo" / "connections.json"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(json.dumps({
        "version": 1,
        "connections": [{
            "name": "DraftSQLite", "alias": "DraftSQLite", "driver": "sqlite",
            "file_path": str(inst.db), "host": "", "port": 0, "database": "",
            "username": "", "read_only": True, "tls_mode": "disabled", "credential_ref": "",
        }, {
            "name": "UnavailableSQLite", "alias": "UnavailableSQLite", "driver": "sqlite",
            "file_path": str(inst.db.parent / "missing" / "database.db"), "host": "", "port": 0,
            "database": "", "username": "", "read_only": True,
            "tls_mode": "disabled", "credential_ref": "",
        }, {
            "name": "NeedsCredentials", "alias": "NeedsCredentials", "driver": "mariadb",
            "file_path": "", "host": "127.0.0.1", "port": 3306, "database": "",
            "username": "synthetic-user", "read_only": True, "tls_mode": "disabled",
            "credential_ref": "",
        }],
    }), encoding="utf-8")
    inst.start()
    frame = inst.frame()

    def choose_connection(page, name):
        picker = A.find_one(page, role_in=("combo box",), timeout=10)
        A.click(A.find_one(picker, role_in=("toggle button",)))
        A.wait_until(
            lambda: any("showing" in A.states(label)
                        for label in A.query(picker, role_in=("label",), name=name)),
            10, f"连接选项 {name}",
        )
        # The native popup owns its own surface. Navigate the visible list by
        # keyboard so the harness does not have to infer its screen origin.
        labels = ("DraftSQLite", "UnavailableSQLite", "NeedsCredentials")
        entries = list(dict.fromkeys(
            A.nm(label) for label in A.query(picker, role_in=("label",))
            if "showing" in A.states(label) and A.nm(label) in labels
        ))
        A.press_keys("Home")
        for _ in range(entries.index(name) + 1):
            A.press_keys("Down")
        A.press_keys("Return")

    def ready(page):
        return "sensitive" in A.states(A.find_one(page, role_in=("button",), name="执行"))

    with step("two_drafts_before_first_connection"):
        ops.find_row(frame, "DraftSQLite", timeout=20)
        first = ops.open_sql_page(frame)
        first_editor = ops.editor(first)
        ops.write_sql(first, "SELECT 123;")
        second = ops.open_sql_page(frame)
        second_editor = ops.editor(second)
        ops.write_sql(second, "SELECT 456;")
        order = ops.query_tab_names(frame)
        expect(inst.log_text().count("[db-rpc] Connect") == 0, "两个草稿产生零 Connect")
        expect(inst.log_text().count("[db-rpc] ExecuteQuery") == 0, "两个草稿产生零 ExecuteQuery")

    with step("first_browse_connection_keeps_same_workspace"):
        row = ops.find_row(frame, "DraftSQLite", timeout=10)
        A.mouse_click(A.find_one(row, name="DraftSQLite"), count=2)
        ops.wait_connected(frame, "DraftSQLite")
        expect(ops.query_tab_names(frame) == order, "首次连接保留原标签顺序")
        expect(A.text_value(second_editor).startswith("SELECT 456;"), "当前编辑器实例保留")
        expect(A.nm(frame).startswith(order[-1]), "首次连接保留活动草稿", A.nm(frame))
        expect(not ready(second), "首次连接没有隐式绑定")
        A.window_action(frame, "win.tab-2")
        A.wait_until(lambda: "showing" in A.states(first_editor), 10, "原第一草稿编辑器")
        expect(A.text_value(first_editor).startswith("SELECT 123;"), "第一草稿保持原内容")
        expect(inst.log_text().count("[db-rpc] Connect") == 1, "首次连接只建立浏览会话")

    with step("cancel_credentials_for_exact_profile"):
        before = inst.log_text().count("[db-rpc] Connect")
        choose_connection(first, "NeedsCredentials")
        dialog = A.wait_dialog(inst.app, "编辑连接")
        A.find_one(dialog, role_in=("text", "entry"), name="别名（可选）")
        A.click(A.find_one(dialog, role_in=("button",), name="取消"))
        A.wait_gone(lambda: A.find_dialog(inst.app, "编辑连接"), 10, "凭据对话框已取消")
        expect(inst.log_text().count("[db-rpc] Connect") == before, "缺少凭据时未发送 Connect")
        expect(A.text_value(first_editor).startswith("SELECT 123;"), "取消凭据对话框保留 SQL")

    with step("explicit_binding_failure_and_success"):
        choose_connection(first, "UnavailableSQLite")
        A.wait_until(lambda: "重绑失败" in inst.log_text(), 15, "SQLite 连接失败后退回草稿")
        expect(A.text_value(first_editor).startswith("SELECT 123;"), "绑定失败保留 SQL")
        expect(not ready(first), "绑定失败保持不可执行")
        choose_connection(first, "DraftSQLite")
        A.wait_until(lambda: ready(first), 15, "显式绑定成立")
        ops.execute_sql(first)
        A.wait_until(lambda: any("123" in row for row in ops.data_rows(first)), 15, "SQLite 实际结果 123")
        expect(inst.log_text().count("[db-rpc] ExecuteQuery") == 1, "仅绑定后提交一次执行")
        A.window_action(frame, "win.close-tab")
        dialog = A.wait_dialog(inst.app, "关闭标签？")
        A.find_one(dialog, role_in=("label",), name_contains="未保存 SQL")
        A.click(A.find_one(dialog, role_in=("button",), name="取消"))
        A.wait_gone(lambda: A.find_dialog(inst.app, "关闭标签？"), 10, "执行后关闭的确认已取消")

    with step("eight_sessions_still_allow_unbound_query"):
        for _ in range(7):
            page = ops.open_sql_page(frame)
            A.wait_until(lambda: ready(page), 15, "标签独立会话就绪")
        A.window_action(frame, "win.tab-3")
        A.window_action(frame, "win.clear-selection")
        draft = ops.open_sql_page(frame)
        expect(not ready(draft), "八条会话占满后仍创建草稿")
        before = inst.log_text().count("[db-rpc] Connect")
        choose_connection(draft, "DraftSQLite")
        A.wait_dialog(inst.app, "无法新建标签")
        expect(inst.log_text().count("[db-rpc] Connect") == before, "额度拒绝先于 Connect")

    with step("full_capacity_rebind_restores_original_session"):
        limit = A.wait_dialog(inst.app, "无法新建标签")
        A.click(A.find_one(limit, role_in=("button",), name="知道了"))
        A.wait_gone(lambda: A.find_dialog(inst.app, "无法新建标签"), 10, "额度提示已关闭")
        A.window_action(frame, "win.tab-2")
        before = inst.log_text().count("[db-rpc] Connect")
        choose_connection(first, "UnavailableSQLite")
        A.wait_until(lambda: inst.log_text().count("[db-rpc] Connect") == before + 2 and ready(first),
                     15, "满额切换失败后恢复原会话")
        log = inst.log_text()
        expect(log.rindex("标签 tab-q1 的会话已释放") <
               log.rindex("标签 tab-q1 建立会话：UnavailableSQLite"),
               "旧会话确认释放后才申请新会话")
        expect(A.text_value(first_editor).startswith("SELECT 123;"), "恢复原会话保留草稿")

    with step("closing_and_opening_compete_for_a_slot"):
        A.window_action(frame, "win.tab-9")
        pid = json.loads(Path(inst.env["SQARVO_ENDPOINT_FILE"]).read_text())["pid"]
        with _backend_pidfd(pid, inst.proc.pid) as backend:
            signal.pidfd_send_signal(backend, signal.SIGSTOP)
            try:
                ops.close_current_query_tab(frame)
                A.window_action(frame, "win.tab-3")
                choose_connection(second, "DraftSQLite")
                limit = A.wait_dialog(inst.app, "无法新建标签")
                A.click(A.find_one(limit, role_in=("button",), name="知道了"))
                A.wait_gone(lambda: A.find_dialog(inst.app, "无法新建标签"), 10, "额度提示已关闭")
            finally:
                signal.pidfd_send_signal(backend, signal.SIGCONT)
        A.wait_until(lambda: "标签 tab-q8 的会话已释放" in inst.log_text(), 15, "真实 Disconnect 回执")
        expect(A.text_value(second_editor).startswith("SELECT 456;"), "关闭另一标签保留当前草稿")

    with step("cancel_opening_and_release_late_success"):
        with _backend_pidfd(pid, inst.proc.pid) as backend:
            released = inst.log_text().count("的会话已释放")
            signal.pidfd_send_signal(backend, signal.SIGSTOP)
            try:
                choose_connection(second, "DraftSQLite")
                A.find_one(second, role_in=("button",), name="取消绑定", timeout=3)
                A.window_action(frame, "win.tab-1")
                A.window_action(frame, "win.tab-prev")
                before = inst.log_text().count("[db-rpc] Connect")
                choose_connection(draft, "DraftSQLite")
                limit = A.wait_dialog(inst.app, "无法新建标签")
                expect(inst.log_text().count("[db-rpc] Connect") == before, "申请中的最后一份额度不能重复使用")
                A.click(A.find_one(limit, role_in=("button",), name="知道了"))
                A.wait_gone(lambda: A.find_dialog(inst.app, "无法新建标签"), 10, "额度提示已关闭")
                A.window_action(frame, "win.tab-3")
                cancel = A.wait_until(lambda: next(
                    (button for button in A.query(second, role_in=("button",), name="取消绑定")
                     if "showing" in A.states(button)), None),
                    2, "当前草稿的取消绑定按钮可见")
                A.click(cancel)
                A.wait_until(lambda: "请求取消绑定" in inst.log_text(), 2, "取消命令已派发")
                expect(not ready(second), "已取消的草稿不能执行")
                A.window_action(frame, "win.tab-1")
            finally:
                signal.pidfd_send_signal(backend, signal.SIGCONT)
            A.wait_until(lambda: inst.log_text().count("的会话已释放") > released, 15,
                         "取消后的迟到会话实际释放")
        expect("释放迟到的会话" in inst.log_text(), "后台确实收到了迟到的成功响应")
        expect(A.nm(frame) == "Sqarvo", "迟到响应未抢占 Objects", A.nm(frame))
        A.window_action(frame, "win.tab-3")
        expect(A.text_value(second_editor).startswith("SELECT 456;"), "取消绑定保留 SQL")
        expect(not ready(second), "迟到结果没有启用草稿执行")

    with step("close_opening_and_release_late_success"):
        A.window_action(frame, "win.tab-1")
        A.window_action(frame, "win.tab-prev")
        with _backend_pidfd(pid, inst.proc.pid) as backend:
            released = inst.log_text().count("的会话已释放")
            signal.pidfd_send_signal(backend, signal.SIGSTOP)
            try:
                choose_connection(draft, "DraftSQLite")
                A.find_one(draft, role_in=("button",), name="取消绑定", timeout=3)
                ops.close_current_query_tab(frame)
            finally:
                signal.pidfd_send_signal(backend, signal.SIGCONT)
            A.wait_until(lambda: inst.log_text().count("的会话已释放") > released, 15,
                         "关闭后迟到会话确实释放")
        expect("查询 10" not in ops.query_tab_names(frame), "迟到响应没有复活关闭的标签")


def test_reconnect_from_objects_only_startup(ui_instance):
    """shell.error, shell.error.retry, startup.workspace, objects_page.tab,
    shell.toolbar.connect, shell.toolbar.new_query, shell.toolbar.layout, shell.title.
    """
    inst = ui_instance("s3retry").start()
    frame = inst.frame()
    A.find_one(frame, role_in=("grouping",), name="Objects", timeout=15)
    focus = A.find_one(frame, role_in=("toggle button",), name="专注模式")
    A.click(focus)
    A.wait_until(lambda: "pressed" in A.states(focus), 10, "专注模式启用")
    pid = json.loads(Path(inst.env["SQARVO_ENDPOINT_FILE"]).read_text())["pid"]
    with _backend_pidfd(pid, inst.proc.pid) as backend:
        signal.pidfd_send_signal(backend, signal.SIGTERM)
    retry = A.find_one(frame, role_in=("button",), name="重新连接", timeout=20)
    toolbar = A.find_one(frame, role_in=("grouping",), name="主工具条")
    for name in ("连接", "新建查询"):
        expect("sensitive" not in A.states(A.find_one(toolbar, role_in=("button",), name=name)),
               f"后端失效时 {name} 不可操作")
    expect(A.nm(frame) == "Sqarvo", "后端失效且没有活动绑定时保留应用标题")
    A.click(retry)
    A.wait_until(lambda: inst.log_text().count("state=Connected") == 2, 25, "重新握手")
    A.find_one(frame, role_in=("grouping",), name="Objects", timeout=10)
    focus = A.find_one(frame, role_in=("toggle button",), name="专注模式")
    expect("pressed" in A.states(focus), "重新挂载保留运行期专注模式")
    expect(len(A.query(frame, role_in=("grouping",), name="Objects")) == 1
           and not ops.query_tab_names(frame), "重新握手后仍只有一个 Objects")


def test_forced_service_exit_releases_capacity_and_allows_explicit_retry(ui_instance):
    """shell.backend_failure, tabs.limit, tabs.query.connection_picker, tabs.query.state."""
    inst = ui_instance("s3reap")
    legacy = inst.home / "config" / "sqarvo" / "Sqarvo" / "connections.json"
    legacy.parent.mkdir(parents=True)
    legacy.write_text(json.dumps({
        "version": 1,
        "connections": [{
            "name": "RecoverySQLite", "alias": "RecoverySQLite", "driver": "sqlite",
            "file_path": str(inst.db), "host": "", "port": 0, "database": "",
            "username": "", "read_only": True, "tls_mode": "disabled", "credential_ref": "",
        }],
    }), encoding="utf-8")
    inst.start()
    frame = inst.frame()
    row = ops.find_row(frame, "RecoverySQLite", timeout=15)
    A.mouse_click(A.find_one(row, name="RecoverySQLite"), count=2)
    ops.wait_connected(frame, "RecoverySQLite")
    page = ops.open_sql_page(frame)
    run = A.find_one(page, role_in=("button",), name="执行")
    A.wait_enabled(run, 10)
    editor = ops.editor(page)
    ops.write_sql(page, "SELECT 789;")
    for _ in range(7):
        other = ops.open_sql_page(frame)
        A.wait_enabled(A.find_one(other, role_in=("button",), name="执行"), 10)
    A.window_action(frame, "win.tab-2")
    A.click(A.find_one(frame, role_in=("toggle button",), name="导航"))
    A.wait_gone(lambda: A.query(frame, role_in=("grouping",), name="导航"), 8,
                "服务失效前隐藏导航")
    pid = json.loads(Path(inst.env["SQARVO_ENDPOINT_FILE"]).read_text())["pid"]
    with _backend_pidfd(pid, inst.proc.pid) as backend:
        signal.pidfd_send_signal(backend, signal.SIGSTOP)
        try:
            A.wait_until(lambda: "已强制回收" in inst.log_text(), 20, "强制回收已确认")
        finally:
            try:
                signal.pidfd_send_signal(backend, signal.SIGCONT)
            except ProcessLookupError:
                pass
    recover = A.find_one(page, role_in=("button",), name="重启 Sqarvo", timeout=10)
    A.find_one(page, role_in=("label",), name_contains="已断开", timeout=10)
    expect("sensitive" not in A.states(run), "导航隐藏时失连原因仍可见，旧会话禁用执行")
    A.wait_until(lambda: A.nm(frame) == "查询 1 @ RecoverySQLite — Sqarvo",
                 10, "后端失效后存续查询保留实际绑定标题")
    A.find_one(frame, role_in=("label",), name="SQLite · main")
    A.click(recover)
    dialog = A.wait_dialog(inst.app, "Sqarvo 需要重启")
    A.click(A.find_one(dialog, role_in=("button",), name="重启 Sqarvo"))
    reconnect = A.find_one(page, role_in=("button",), name="重新连接", timeout=20)
    before = inst.log_text().count("[db-rpc] Connect")
    A.click(reconnect)
    A.wait_enabled(run, 15)
    expect(inst.log_text().count("[db-rpc] Connect") == before + 1, "旧额度已清，同档案仅申请一条会话")
    expect("[db-rpc] ExecuteQuery" not in inst.log_text(), "恢复没有自动执行 SQL")
    expect(A.text_value(editor).startswith("SELECT 789;"), "恢复保留编辑器及 SQL")
