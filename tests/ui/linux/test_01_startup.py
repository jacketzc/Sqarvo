"""启动态与主窗口可访问性（任务 §6.1、§6.2）。"""
import pytest

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.smoke


def test_startup_readiness(app_bare):
    """应用启动：AT-SPI 可见 + 主窗口出现 + 后端握手完成的状栏摘要。

    注册表组件: startup, startup.workspace, status_bar。
    """
    with step("startup_ready"):
        expect(app_bare.app is not None, "应用出现在 AT-SPI 总线", app_bare.app)
        frame = app_bare.frame()
        expect(A.nm(frame).startswith("Sqarvo"), "主窗口标题以 Sqarvo 开头", A.nm(frame))
        lines = A.wait_until(lambda: [l for l in ops.status_text(frame) if "驱动" in l],
                             30, "状态栏驱动摘要（后端握手完成）")
        expect(len(lines) >= 1, "状态栏给出「… 驱动 N …」摘要行", lines)
        navigation = A.find_one(frame, role_in=("grouping",), name="导航", timeout=10)
        A.find_one(navigation, role_in=("label",), name="连接")
        A.find_one(navigation, role_in=("label",),
                   name="用工具条的「连接」保存一条连接。保存后可在导航中找到它。", timeout=10)


def test_main_window_accessibility(app_bare):
    """主窗口可访问性与未连接时的查询入口。

    注册表组件: shell, startup.workspace, shell.toolbar.new_query, tabs.strip.new。
    """
    frame = app_bare.frame()
    connect = A.find_one(frame, role_in=("button",), name="连接", timeout=20)
    expect("sensitive" in A.states(connect), "「连接」按钮可用", A.states(connect))
    new_query = A.find_one(frame, role_in=("button",), name="新建查询", timeout=10)
    expect("sensitive" in A.states(new_query), "可创建未绑定查询")
    for name in ("Tables", "Views"):
        button = A.find_one(frame, role_in=("button",), name=name, timeout=10)
        expect("sensitive" not in A.states(button), f"「{name}」需要数据库目标")
    for name in ("Functions", "Queries"):
        expect(not A.query(frame, role_in=("button",), name=name), f"隐藏未实现的 {name}")


def test_connection_dialog_open_and_cancel(app_bare):
    """点「连接」打开新建连接对话框：驱动默认值与按钮齐备；「取消」可关闭。

    注册表组件: dialogs.connection, dialogs.connection.driver,
    dialogs.connection.actions.cancel。
    """
    with step("open_dialog"):
        frame = app_bare.frame()
        dlg = ops.open_connection_dialog(app_bare.app, frame)
        expect("新建连接" in A.nm(dlg), "对话框标题为「新建连接」", A.nm(dlg))
    with step("dialog_controls"):
        combo = A.find_one(dlg, role_in=("combo box",), name="驱动")
        expect(combo is not None, "存在「驱动」下拉", None)
        value = ops.dialog_driver_value(dlg)
        expect("MariaDB" in value or "mariadb" in value,
               "驱动默认预选 MariaDB（dialog.rs 默认值）", value)
        for name in ("测试连接", "保存并连接", "取消"):
            b = A.find_one(dlg, role_in=("button",), name=name)
            expect("sensitive" in A.states(b), f"「{name}」按钮可用", A.states(b))
    with step("cancel_closes"):
        ops.close_connection_dialog_via_cancel(dlg)
        expect(A.find_dialog(app_bare.app, "新建连接") is None,
               "取消后对话框关闭", A.find_dialog(app_bare.app, "新建连接"))
