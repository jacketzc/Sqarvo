"""隔离 X11 中验证主动断开菜单及正常退出的会话回收。"""
import pytest

import adapter as A
import ops
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.smoke


def test_window_close_disposes_session():
    """已连接实例经「关闭」按钮退出：清理路径完整、重启后为未连接。

    注册表组件: shell.header, object_tree.tree.row.conn, startup。
    """
    inst = UiInstance("disc").start(autoconnect=True)
    reopened = None
    try:
        frame = inst.frame()
        ops.wait_connected(frame, "uitest.db", timeout=40)
        with step("close_window"):
            # 此按钮来自 GTK 原生窗口装饰，采用桌面会话的语言。
            close_btn = A.wait_until(
                lambda: next((button for button in A.query(frame, role_in=("button",))
                              if A.nm(button) in {"Close", "关闭"}), None),
                15, "原生窗口关闭按钮")
            A.click(close_btn)
        with step("clean_exit"):
            rc = inst.proc.wait(timeout=20)
            expect(rc == 0, "退出码 0（正常清理路径）", f"rc={rc}")
            inst.proc = None  # 已自行退出
        inst.stop()  # 只回收进程，保留隔离目录（连接配置要留给重启）

        # 重启（保留隔离配置目录）：连接已保存、但未自动连接。
        reopened = UiInstance("disc", fresh=False, db=False).start(autoconnect=False)
        frame = reopened.frame()
        with step("reopened_disconnected"):
            # 启动态连接行显示数据库文件路径；名称在 tooltip。按路径定位。
            row = A.wait_until(lambda: reopened_frame_has_row(frame), 30,
                               "重启后对象树仍列出保存的连接")
            texts = ops.tree_row_texts(row)
            expect("未连接" in texts, "重启后连接行显示未连接", texts)
    finally:
        if reopened is not None:
            reopened.dispose()
        inst.dispose()


def reopened_frame_has_row(frame):
    return ops.find_row(frame, "uitest.db")


def test_menu_driven_active_disconnect(app_connected):
    """object_tree.ctx_menu.disconnect: 真实右键菜单和确认框关闭浏览会话。"""
    frame = app_connected.frame()
    row = ops.find_row(frame, "uitest.db", "已连接", timeout=10)
    A.mouse_click(A.find_one(row, role_in=("label",), name="uitest.db"), button=3)
    menu = A.wait_until(
        lambda: next((menu for menu in A.query(frame, role_in=("menu",))
                      if "ctx.disconnect" in A.action_names(menu)), None),
        10, "资源右键菜单")
    A.window_action(menu, "ctx.disconnect")
    dialog = A.wait_dialog(app_connected.app, "断开连接？")
    A.click(A.find_one(dialog, role_in=("button",), name="断开连接"))
    ops.find_row(frame, "uitest.db", "未连接", timeout=15)
    expect(ops.find_row(frame, "main") is None, "主动断开移除该会话的树层")
    with step("double_click_reconnects"):
        row = ops.find_row(frame, "uitest.db", "未连接", timeout=15)
        A.mouse_click(row, count=2)
        ops.wait_connected(frame, "uitest.db", timeout=20)
