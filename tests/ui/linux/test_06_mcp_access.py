"""MCP 策略菜单、风险确认与持久状态的 AT-SPI 功能覆盖。"""

import json

import pytest

import adapter as A
import ops
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.smoke

DISABLED = "MCP：访问关闭"
READABLE = "MCP：仅允许读取"
WRITABLE = "MCP：只读已关闭。当前 MCP 版本尚未提供数据库写入工具。"
NO_WRITE_TOOLS = "当前 MCP 版本尚未提供数据库写入工具。"


@pytest.fixture
def app_with_profile():
    inst = UiInstance("mcp_policy")
    try:
        inst.start(autoconnect=True)
        ops.wait_connected(inst.frame(), str(inst.db), timeout=30)
        yield inst
    finally:
        inst.dispose()


def status(frame, caption):
    return A.find_one(frame, role_in=("label",), name=caption, timeout=15)


def snapshot(inst):
    path = inst.home / "config" / "sqarvo" / "connections.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    return data["revision"], data["mcp"]


def respond(dialog, name):
    A.click(A.find_one(dialog, role_in=("button",), name=name, timeout=10))


def confirm_scope(dialog, *, write):
    captions = [A.nm(label) for label in A.query(dialog, role_in=("label",))]
    expect(any("当前 1 个连接档案" in caption for caption in captions),
           "确认框说明当前 1 个连接档案", captions)
    if write:
        expect(any(NO_WRITE_TOOLS in caption and "关闭只读模式不会自动增加写入能力。" in caption
                   for caption in captions),
               "确认框说明当前版本的写入能力范围", captions)


def test_mcp_access_menu_and_live_status(app_with_profile):
    """注册表组件: shell.menu, workspace.manage.menu.mcp_access,
    workspace.status.mcp_indicator, dialog.mcp_enable_confirm。
    """
    inst = app_with_profile
    frame = inst.frame()
    with step("access_menu"):
        status(frame, DISABLED)
        A.find_one(frame, role_in=("toggle button",), name="主菜单（文件 / 视图 / 帮助）")
        # GTK exposes unnamed AT-SPI menu items. The window action is the same
        # action bound to the native menu, whose labels are checked by Rust tests.
        actions = A.action_names(frame)
        expect("win.mcp-access" in actions, "MCP 数据访问动作可用", actions)
        expect("win.mcp-read-only" not in actions, "全局关闭时 MCP 只读动作禁用", actions)
        original = snapshot(inst)
        A.window_action(frame, "win.mcp-access")

    with step("cancel_enable"):
        dialog = A.wait_dialog(inst.app, "启用 MCP 数据访问？")
        confirm_scope(dialog, write=False)
        respond(dialog, "取消")
        A.wait_gone(lambda: A.find_dialog(inst.app, "启用 MCP 数据访问？"), 10, "启用确认框关闭")
        status(frame, DISABLED)
        expect(snapshot(inst) == original, "取消启用时策略与 revision 不变", snapshot(inst))

    with step("enable_and_revoke"):
        A.window_action(frame, "win.mcp-access")
        respond(A.wait_dialog(inst.app, "启用 MCP 数据访问？"), "启用 MCP 数据访问")
        status(frame, READABLE)
        expect(snapshot(inst) == (original[0] + 1, {"enabled": True, "read_only": True}),
               "启用后以只读方式访问且 revision 推进一次", snapshot(inst))
        A.window_action(frame, "win.mcp-access")
        status(frame, DISABLED)
        expect(snapshot(inst) == (original[0] + 2, {"enabled": False, "read_only": True}),
               "撤销访问恢复安全默认值", snapshot(inst))


def test_mcp_read_only_confirmation(app_with_profile):
    """注册表组件: workspace.manage.menu.mcp_readonly, workspace.status.mcp_indicator,
    dialog.mcp_readonly_disable_confirm。
    """
    inst = app_with_profile
    frame = inst.frame()
    with step("enable_access"):
        status(frame, DISABLED)
        A.window_action(frame, "win.mcp-access")
        respond(A.wait_dialog(inst.app, "启用 MCP 数据访问？"), "启用 MCP 数据访问")
        status(frame, READABLE)
        original = snapshot(inst)

    with step("cancel_read_only_change"):
        A.window_action(frame, "win.mcp-read-only")
        dialog = A.wait_dialog(inst.app, "关闭 MCP 只读模式？")
        confirm_scope(dialog, write=True)
        respond(dialog, "取消")
        A.wait_gone(lambda: A.find_dialog(inst.app, "关闭 MCP 只读模式？"), 10, "只读确认框关闭")
        status(frame, READABLE)
        expect(snapshot(inst) == original, "取消关闭只读时策略与 revision 不变", snapshot(inst))

    with step("disable_and_restore_read_only"):
        A.window_action(frame, "win.mcp-read-only")
        respond(A.wait_dialog(inst.app, "关闭 MCP 只读模式？"), "关闭 MCP 只读")
        warning = status(frame, WRITABLE)
        expect("showing" in A.states(warning), "只读关闭时状态提示可见", A.states(warning))
        expect(snapshot(inst) == (original[0] + 1, {"enabled": True, "read_only": False}),
               "关闭只读经 gRPC 提交并回读", snapshot(inst))
        A.window_action(frame, "win.mcp-read-only")
        status(frame, READABLE)
        expect(snapshot(inst) == (original[0] + 2, {"enabled": True, "read_only": True}),
               "恢复只读时访问继续启用", snapshot(inst))


def test_malformed_profile_store_shows_error_without_reset():
    """注册表组件: object_tree.tree, workspace.status.mcp_indicator。"""
    inst = UiInstance("broken_profiles")
    try:
        document = inst.home / "config" / "sqarvo" / "connections.json"
        document.parent.mkdir()
        document.write_text("{broken document", encoding="utf-8")
        inst.start()
        frame = inst.frame()
        with step("stored_profiles_unavailable"):
            A.find_one(frame, role_in=("label",), name_contains="无法加载共享连接档案", timeout=15)
            status(frame, "MCP：无法读取状态")
            expect(not ops.find_row(frame, "uitest.db"), "档案不可读时对象树保持空", ops.tree_rows(frame))
            expect(document.read_text(encoding="utf-8") == "{broken document",
                   "档案加载失败不会重置文件")
    finally:
        inst.dispose()


def test_edit_profile_preserves_id_and_increments_revision():
    """注册表组件: object_tree.ctx_menu.edit_connection, dialogs.connection.fields.alias,
    dialogs.connection.actions.save。
    """
    inst = UiInstance("edit_profile")
    try:
        legacy = inst.home / "config" / "sqarvo" / "Sqarvo" / "connections.json"
        legacy.parent.mkdir(parents=True)
        legacy.write_text(json.dumps({"version": 1, "connections": [{
            "name": "UT Profile", "alias": "Before", "driver": "sqlite", "host": "",
            "port": 0, "database": "", "username": "", "file_path": str(inst.db),
            "read_only": False, "tls_mode": "disabled", "credential_ref": "",
        }]}), encoding="utf-8")
        inst.start(extra_args=("--qa-startup-menu", "Before", "--qa-startup-menu-pick", "编辑连接…"))
        frame = inst.frame()
        store = legacy.parent.parent / "connections.json"
        with step("edit_and_save_profile"):
            dialog = A.wait_dialog(inst.app, "编辑连接")
            previous = json.loads(store.read_text(encoding="utf-8"))["profiles"][0]
            alias = A.find_one(dialog, role_in=("text", "entry"), name="别名（可选）", timeout=10)
            A.set_text(alias, "After")
            respond(dialog, "保存")
            A.wait_gone(lambda: A.find_dialog(inst.app, "编辑连接"), 10, "编辑连接对话框关闭")
            row = ops.find_row(frame, "After", timeout=10)
            labels = [A.nm(label) for label in A.query(row, role_in=("label",))]
            expect(not any("已连接" in label for label in labels), "保存编辑不会建立会话", labels)
            profiles = json.loads(store.read_text(encoding="utf-8"))["profiles"]
            expect(len(profiles) == 1, "编辑同一档案不会新增条目", profiles)
            updated = profiles[0]
            expect(updated["profile_id"] == previous["profile_id"], "保留原 UUID", updated["profile_id"])
            expect(updated["revision"] == previous["revision"] + 1,
                   "修改推进档案 revision", updated["revision"])
    finally:
        inst.dispose()
