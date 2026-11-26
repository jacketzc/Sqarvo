"""SQLite 连接的创建与打开（任务 §6.3）。

创建路径说明：连接经 `--qa-profile` 通道填入**与用户输入相同的控件**并触发「保存并
连接」同一回调（前端 AGENTS §13 的验收辅助通道）。外部 AT-SPI 无法切换 `adw::ComboRow`
驱动下拉（Wayland 禁用合成输入，见 README「已知限制」），本测试覆盖的是这条通道之后
一切用户可观察的行为：保存、注册、状态流转、树行徽标。
"""
import pytest

import ops
from ops import expect, step

pytestmark = pytest.mark.sql


def test_sqlite_connection_open(app_connected):
    """打开后：对象树出现该连接行且徽标为「已连接」；状态栏摘要不再是未连接。

    注册表组件: object_tree.tree.row.conn, status_bar。
    """
    with step("tree_row"):
        frame = app_connected.frame()
        row = ops.find_row(frame, "uitest.db", timeout=0)
        expect(row is not None, "对象树存在指向测试库文件的连接行", ops.tree_rows(frame)[:4])
        texts = ops.tree_row_texts(row)
        expect(any("已连接" in t for t in texts), "连接行徽标为「已连接」", texts)
    with step("status_bar"):
        lines = ops.status_text(frame)
        expect(all("未连接" not in l or "浏览会话" not in l for l in lines),
               "状态栏摘要不再显示「未连接」", lines)
