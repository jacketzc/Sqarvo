"""空表：照常有数据地绘制网格（只是 0 行），且「新增行」直接可用、不报 GTK 断言。

回归：空表曾走独立占位页，草稿行被挡住；且回到顶部无条件 scroll_to(0) 触发
gtk_column_view_scroll_to 的 `pos < n_items` 断言。
"""

from pathlib import Path

import pytest

import adapter as A
import ops
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.sql


def test_empty_table_shows_grid_and_accepts_add_row():
    """空表不进占位页、0 数据行；点状态栏「新增行」后待提交变更出现、无 GTK 断言。

    注册表组件: data_page.grid（0 行也绘制网格）、data_page.placeholder（不再接管空表）、
    status_bar.row1.ops.add。只断言交互、状态流转与应用自身诊断，不做样式/截图断言（ADR 0029）。

    对象树行的 AT-SPI 激活在本环境不可用（README 已知限制 1），故用应用自带的
    `--qa-tree-activate` 走与双击 / 回车同一个 `activate` 信号；后续按钮点击仍是真实 AT-SPI。
    """
    inst = UiInstance("empty_table")
    try:
        inst.start(autoconnect=True, extra_args=(
            "--qa-tree-expand", "UITestDB",
            "--qa-tree-expand", "main",
            "--qa-tree-expand", "main",
            "--qa-tree-activate", "widgets_empty",
        ))
        frame = inst.frame()

        with step("open_empty_table"):
            A.wait_until(
                lambda: A.query(frame, role_in=("button",), name="开始事务"),
                15, "表数据页工具条出现（「开始事务」在场）")

        with step("no_placeholder_no_data_rows"):
            labels = ops.labels(frame)
            expect(not [t for t in labels if "空表" in t or "该表存在" in t],
                   "空表不再显示占位页文案", [t for t in labels if "空表" in t][:4])
            rows = ops.data_rows(frame)
            expect(len(rows) == 0, "空表没有数据行", f"{len(rows)} rows")

        with step("add_row_on_empty_table"):
            adds = A.query(frame, role_in=("button",), name="新增行：在当前表末尾新增一行")
            expect(len(adds) == 1, "状态栏「新增行」按钮在场且唯一", [A.nm(x) for x in adds])
            expect("sensitive" in A.states(adds[0]), "空表时「新增行」可用", A.states(adds[0]))
            A.click(adds[0])
            applies = A.query(frame, role_in=("button",), name="应用：提交所有脏行的变更")
            A.wait_until(lambda: [b for b in applies if "sensitive" in A.states(b)],
                         10, "新增行后「应用」变为可用（草稿行已进入待提交）")

        with step("no_gtk_column_view_assertion"):
            log = Path(inst.home) / "state" / "sqarvo" / "desktop.log"
            A.wait_until(
                lambda: log.is_file() and "数据页已就绪：offset=0，0 行" in log.read_text(
                    encoding="utf-8", errors="replace"),
                10, "空表数据页就绪日志落地")
            text = log.read_text(encoding="utf-8", errors="replace")
            criticals = [line for line in text.splitlines()
                         if "gtk_column_view_scroll_to" in line or "Gtk-CRITICAL" in line]
            expect(not criticals, "空表打开与新增行不触发 GTK 断言", criticals[:4])
    finally:
        inst.dispose()
