"""连接断开与会话可回收（任务 §6.7；根 AGENTS §10 生命周期）。

产品里「断开连接」的入口是对象树行的右键手势菜单。Wayland 会话禁止任何输入合成
（AT-SPI GenerateKeyboardEvent / Component.click 均返回 atspi_error，实测），
外部自动化无法触发手势菜单 —— 这是环境限制，如实报告（任务 §10），不伪造通过。

本模块覆盖真实可观察的断开语义：关窗 → 前端正常清理（退出码 0、后端无孤儿）→
重新启动后连接仍在（已保存）但处于「未连接」态，证明会话被回收、状态可恢复。
"""
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
            close_btn = A.find_one(frame, role_in=("button",), name="关闭")
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
            texts = [A.nm(x) for x in A.query(row, role_in=("label",), max_depth=8)]
            expect(not any("已连接" in t for t in texts),
                   "重启后连接行不是已连接态", texts)
    finally:
        if reopened is not None:
            reopened.dispose()
        else:
            inst.dispose()


def reopened_frame_has_row(frame):
    return ops.find_row(frame, "uitest.db")


def test_menu_driven_active_disconnect_unreachable():
    """已知限制：主动「断开连接」只有手势右键菜单入口，Wayland 禁合成输入。

    注册表组件: object_tree.ctx_menu.disconnect。

    应用自身的 `--qa-startup-menu <名> --qa-startup-menu-pick 断开连接` 通道可覆盖
    （#144 验收脚本口径），但它作用于「启动态未连接行」，菜单里没有「断开连接」项，
    因此不能等价替代。等应用侧提供非手势入口（或注册表新增按钮）后补真测试。
    """
    pytest.skip("环境限制：断开连接入口为手势右键菜单，Wayland 禁止输入合成（见 README 已知限制）")
