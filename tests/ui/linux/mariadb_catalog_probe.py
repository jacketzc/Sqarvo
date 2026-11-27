"""Run explicitly against the disposable two-catalog MariaDB fixture on port 55306."""
import json

import adapter as A
import ops


def test_mariadb_catalog_and_permission_refusal_preserve_sql_target(ui_instance, monkeypatch):
    """shell.title, shell.toolbar.target, tabs.query.database_picker, tabs.query.state, objects_page.table."""
    monkeypatch.setenv("SQARVO_S8_FIXTURE_PASSWORD", "sqarvo_ui_scope_test_only")
    inst = ui_instance("s8_mariadb_permissions")
    profile = inst.home / "profile.json"
    profile.write_text(json.dumps({
        "driver": "mariadb", "host": "127.0.0.1", "port": 55306,
        "username": "sqarvo_ui_scope", "tls_mode": "disabled", "read_only": True,
    }))
    inst.start(extra_args=("--qa-profile", str(profile),
                            "--qa-password-env", "SQARVO_S8_FIXTURE_PASSWORD",
                            "--lang", "zh", "--window-size", "1440x800"))
    frame = inst.frame()
    ops.wait_connected(frame, "127.0.0.1:55306", timeout=35)
    nav = A.find_one(frame, role_in=("grouping",), name="导航")
    database = ops.find_row(nav, "库", exact_name="sqarvo_test_b", timeout=12)
    A.mouse_click(A.find_one(database, role_in=("label",), name="sqarvo_test_b"))
    objects = A.find_one(frame, role_in=("grouping",), name="Objects", timeout=10)
    A.find_one(objects, role_in=("label",), name="s6_orders", timeout=15)
    toolbar = A.find_one(frame, role_in=("grouping", "tool bar"), name="主工具条")
    for category in ("Tables", "Views"):
        A.find_one(toolbar, role_in=("button", "toggle button"), name=category, timeout=8)
    for unavailable in ("Functions", "Queries"):
        assert not A.query(toolbar, role_in=("button", "toggle button"), name=unavailable)

    page = ops.open_sql_page(frame)
    A.find_one(frame, role_in=("label",), name="MariaDB · sqarvo_test_b", timeout=12)
    title = A.nm(frame)
    ops.write_sql(page, "SELECT id FROM sqarvo_s8_denied.denied")
    ops.execute_sql(page)

    def permission_error():
        message = ops.messages_log_text(page)
        return any(line.startswith("> Error:") and "[mariadb.permission_denied]" in line
                   for line in message.splitlines())

    A.wait_until(permission_error, 12, "真实 MariaDB 权限拒绝")
    assert not ops.data_rows(page)
    assert A.nm(frame) == title

    ops.write_sql(page, "SELECT DATABASE(), owner FROM s6_orders")
    ops.execute_sql(page)
    assert any({"sqarvo_test_b", "peer-fixture"} <= set(row)
               for row in ops.wait_query_done(page))
    assert A.nm(frame) == title
    A.find_one(frame, role_in=("label",), name="MariaDB · sqarvo_test_b")

    # The fixture grants visibility of the database name but denies access to it.
    denied = ops.find_row(nav, "库", exact_name="sqarvo_s8_denied", timeout=12)
    A.mouse_click(A.find_one(denied, role_in=("label",), name="sqarvo_s8_denied"))
    before = inst.log_text().count("[db-rpc] ExecuteQuery")
    blocked = ops.open_sql_page(frame)
    A.wait_until(lambda: "标签 tab-q2 的会话失效" in inst.log_text(),
                 15, "目标库权限拒绝时关闭标签会话")
    execute = A.find_one(blocked, role_in=("button",), name="执行")
    assert "sensitive" not in A.states(execute)
    ops.write_sql(blocked, "SELECT DATABASE()")
    A.press_keys("Control_L", "Return")
    A.wait_until(lambda: "标签 tab-q2 的会话已释放" in inst.log_text(),
                 15, "等候拒绝会话的真实释放回执")
    A.wait_until(lambda: A.nm(frame) == "查询 2 — Sqarvo",
                 10, "目标库被拒绝后保留未绑定草稿")
    assert inst.log_text().count("[db-rpc] ExecuteQuery") == before
    assert A.text_value(ops.editor(blocked)) == "SELECT DATABASE()"
