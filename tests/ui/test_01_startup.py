"""启动态与主窗口可访问性（任务 §6.1、§6.2）。"""
import pytest

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.smoke


def test_startup_readiness(app_bare):
    """应用启动：AT-SPI 可见 + 主窗口出现 + 后端握手完成的状栏摘要。"""
    with step("startup_ready"):
        expect(app_bare.app is not None, "应用出现在 AT-SPI 总线", app_bare.app)
        frame = app_bare.frame()
        expect(A.nm(frame).startswith("Sqarvo"), "主窗口标题以 Sqarvo 开头", A.nm(frame))
        lines = A.wait_until(lambda: [l for l in ops.status_text(frame) if "驱动" in l],
                             30, "状态栏驱动摘要（后端握手完成）")
        expect(len(lines) >= 1, "状态栏给出「… 驱动 N …」摘要行", lines)


def test_main_window_accessibility(app_bare):
    """主窗口可访问性：核心控件语义在位；未连接时「新建查询」按状态级不可用呈现（禁用+原因）。"""
    with step("window_controls"):
        frame = app_bare.frame()
        btns = {A.nm(b): b for b in A.query(frame, role_in=("button",))}
        expect("连接" in btns, "存在「连接」按钮", sorted(btns)[:8])
        expect("sensitive" in A.states(btns["连接"]), "「连接」按钮可用", A.states(btns["连接"]))
        for name in ("Tables", "Views", "Functions", "Queries"):
            expect(name in btns, f"存在「{name}」按钮", sorted(btns)[:8])
    with step("disabled_reason"):
        # 未连接：新建查询入口可见、禁用、带原因文案（根 §8 / 前端 §8.1 状态级不可用）。
        # 工具条按钮名保持「新建查询」，原因在 tooltip（accessible description）；
        # 标签条按钮直接把原因写进可访问名。任一形式成立即可，但必须有一种。
        disabled = [b for b in A.query(frame, role_in=("button",))
                    if "查询" in A.nm(b) and "sensitive" not in A.states(b)]
        expect(bool(disabled), "未连接时存在被禁用的「新建查询」入口",
               [A.nm(b) for b in A.query(frame, role_in=("button",)) if "查询" in A.nm(b)])
        reasons = [A.nm(b) + " " + A.description(b) for b in disabled]
        expect(any("先连接" in r for r in reasons),
               "禁用态给出原因（可访问名或 tooltip 含「先连接」）", reasons)


def test_connection_dialog_open_and_cancel(app_bare):
    """点「连接」打开新建连接对话框：驱动默认值与按钮齐备；「取消」可关闭。"""
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
