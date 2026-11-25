"""Inspector 无缓存按需查询的请求与展示行为（功能验收，无截图/样式断言）。

注册表组件: inspector, inspector.object, inspector.pane_switch,
inspector.pane_switch.ddl, inspector.pane_switch.info, inspector.object.info。

验证真实请求而不是缓存命中信息：统计 MetadataHub 在真实提交边界打印的 `[qa.metadata] submit <kind> id=` 行，
并用 AT-SPI 读取面板上的列文案。每个实例独立配置 / 数据库 / 日志。
"""
from __future__ import annotations

import sqlite3
import time

import pytest

import adapter as A
import ops
import gi
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.sql

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

KINDS = ("ddl", "columns", "primary_key", "inspector.nodes")
EXPAND_ARGS = ("--qa-tree-expand", "main", "--qa-tree-expand", "Tables")


def _start(tag: str, *extra: str) -> UiInstance:
    inst = UiInstance(tag).start(autoconnect=True, extra_args=(*EXPAND_ARGS, *extra))
    ops.wait_connected(inst.frame(), "uitest.db", timeout=40)
    return inst


def _counts(inst: UiInstance) -> dict[str, int]:
    text = inst.log_text()
    return {kind: text.count(f"[qa.metadata] submit {kind} id=") for kind in KINDS}


def _wait_counts(inst: UiInstance, **minimum: int) -> dict[str, int]:
    def probe():
        counts = _counts(inst)
        return counts if all(counts[k] >= v for k, v in minimum.items()) else None
    return A.wait_until(probe, 25, f"请求计数达到 {minimum}")


def _select_tree_row(inst: UiInstance, name: str) -> None:
    """走对象树的单选模型（与用户点击行同一语义）。"""
    frame = inst.frame()
    lists = A.query(frame, role_in=("list",))
    expect(bool(lists), "对象树列表存在", len(lists))
    rows = A.children(lists[0])
    for index, row in enumerate(rows):
        labels = [A.nm(x) for x in A.query(row, role_in=("label",), max_depth=6)]
        if any(name in label for label in labels):
            expect(Atspi.Selection.select_child(lists[0], index),
                   f"选中树行 {name}", False)
            return
    raise AssertionError(f"树里找不到行 {name}：{ops.tree_rows(frame)}")


def _pane_button(frame, name: str):
    buttons = A.query(frame, role_in=("toggle button",), name=name)
    expect(bool(buttons), f"面板切换按钮 {name} 存在", len(buttons))
    return buttons[0]


def _labels(frame) -> list[str]:
    return [A.nm(x) for x in A.query(frame, role_in=("label",)) if A.nm(x)]


def test_first_visit_is_on_demand_and_pane_switches_requery():
    """首次选中只查 DDL；切到表信息与切回 DDL 各查一次目标子页。"""
    inst = _start("insp_demand", "--qa-tree-select", "widgets")
    try:
        with step("first_visit_only_ddl"):
            counts = _wait_counts(inst, ddl=1)
            expect(counts == {"ddl": 1, "columns": 0, "primary_key": 0, "inspector.nodes": 0},
                   "首次选中 widgets 只请求 GetDdl", counts)

        with step("switch_to_info"):
            A.click(_pane_button(inst.frame(), "表信息"))
            counts = _wait_counts(inst, **{"columns": 1, "primary_key": 1, "inspector.nodes": 1})
            expect(counts == {"ddl": 1, "columns": 1, "primary_key": 1, "inspector.nodes": 1},
                   "切到表信息只补查列 / 主键 / 索引", counts)

        with step("switch_back_to_ddl"):
            A.click(_pane_button(inst.frame(), "DDL"))
            counts = _wait_counts(inst, ddl=2)
            expect(counts == {"ddl": 2, "columns": 1, "primary_key": 1, "inspector.nodes": 1},
                   "切回 DDL 重新发 GetDdl，不重发表信息请求", counts)

        with step("duplicate_click_on_active_pane"):
            before = _counts(inst)
            A.click(_pane_button(inst.frame(), "DDL"))
            time.sleep(0.6)
            expect(_counts(inst) == before, "重复点已选中的子页不重复请求",
                   (before, _counts(inst)))
    finally:
        inst.dispose()


def test_revisit_refresh_and_idle_do_not_reuse_history():
    """A → B → A 每次重查；主动刷新即使对象不变也重查；闲置不产生轮询。"""
    inst = _start("insp_revisit", "--qa-tree-select", "widgets")
    try:
        _wait_counts(inst, ddl=1)

        with step("a_to_b_to_a"):
            _select_tree_row(inst, "widgets_empty")
            counts = _wait_counts(inst, ddl=2)
            expect(counts["ddl"] == 2, "切到 B 重新查一次", counts)
            _select_tree_row(inst, "widgets")
            counts = _wait_counts(inst, ddl=3)
            expect(counts["ddl"] == 3, "回到 A 再查一次（不复用历史结果）", counts)

        with step("explicit_refresh"):
            A.window_action(inst.frame(), "win.refresh")
            counts = _wait_counts(inst, ddl=4)
            expect(counts["ddl"] == 4, "F5 主动刷新重查当前对象", counts)

        with step("idle_has_no_background_requests"):
            before = _counts(inst)
            time.sleep(2.5)
            expect(_counts(inst) == before, "没有用户操作时不应有元数据查询",
                   (before, _counts(inst)))
    finally:
        inst.dispose()


def _ddl_text(frame) -> str:
    for acc in A.walk(frame, max_depth=40):
        text = A.text_value(acc)
        if "CREATE TABLE" in text:
            return text
    return ""


def test_fast_a_b_a_switch_keeps_the_last_request():
    """快速 A → B → A 后，只有最后一次 A 的正文留在面板上。"""
    inst = _start("insp_fast", "--qa-tree-select", "widgets")
    try:
        A.wait_until(lambda: "CREATE TABLE" in _ddl_text(inst.frame()), 15, "初始 DDL 正文")
        with step("fast_switch"):
            _select_tree_row(inst, "widgets_empty")
            _select_tree_row(inst, "widgets")
            counts = _wait_counts(inst, ddl=3)
            expect(counts["ddl"] == 3, "A → B → A 共三次 GetDdl", counts)
            A.wait_until(
                lambda: "CREATE TABLE widgets" in _ddl_text(inst.frame())
                and "widgets_empty" not in _ddl_text(inst.frame()),
                15, "最后展示的是最后一次 A 的 DDL")
    finally:
        inst.dispose()


def test_disconnect_clears_display_and_invalidates_requests():
    """断开浏览会话后，旧对象的正文不再展示，在途响应也不会回填。"""
    inst = _start("insp_disc", "--qa-tree-select", "widgets",
                  "--qa-tree-disconnect", "UITestDB")
    try:
        with step("disconnect"):
            A.wait_until(lambda: "浏览会话已断开" in inst.log_text(), 20,
                         "浏览会话已断开")
            expect("CREATE TABLE" not in _ddl_text(inst.frame()),
                   "断连后 DDL 正文已清空", _ddl_text(inst.frame())[:80])

        with step("no_requests_after_disconnect"):
            before = _counts(inst)
            time.sleep(1.5)
            expect(_counts(inst) == before, "断连后不应继续发起元数据请求",
                   (before, _counts(inst)))
    finally:
        inst.dispose()


def test_external_ddl_change_is_visible_on_next_visit():
    """外部 ALTER TABLE 加列后，主动刷新按新会话状态重新查列并展示。"""
    inst = _start("insp_ddl_change", "--qa-tree-select", "widgets")
    try:
        with step("load_columns"):
            _wait_counts(inst, ddl=1)
            A.click(_pane_button(inst.frame(), "表信息"))
            _wait_counts(inst, columns=1)
            A.wait_until(lambda: any("qty" in label for label in _labels(inst.frame())),
                         15, "表信息页出现原有列 qty")
            before = _counts(inst)

        with step("alter_table_externally"):
            con = sqlite3.connect(inst.db)
            try:
                con.execute("ALTER TABLE widgets ADD COLUMN new_marker TEXT")
                con.commit()
            finally:
                con.close()

        with step("refresh_and_read_new_column"):
            A.window_action(inst.frame(), "win.refresh")
            _wait_counts(inst, columns=before["columns"] + 1)
            A.wait_until(lambda: any("new_marker" in label for label in _labels(inst.frame())),
                         15, "表信息页出现新增列 new_marker")
            expect(any("qty" in label for label in _labels(inst.frame())),
                   "原有列仍在展示", _labels(inst.frame())[:12])
    finally:
        inst.dispose()
