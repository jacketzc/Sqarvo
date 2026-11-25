"""日期时间字段的编辑器用 GtkCalendar + 时分秒选择框，而不是纯文本框。

注册表组件: data_page.editor（日期时间选择器：L2 手搭复合件，原生 GtkCalendar
+ 三个原生 GtkSpinButton）。只断言交互、文案与应用自身 QA 诊断，不做样式 / 截图断言（ADR 0029）。

自动化说明：无窗口管理器焦点时 GtkPopover 会立刻被关闭，AT-SPI 看不到弹层控件；
应用自带 QA 日志（``--qa-open-editor`` 走双击同一个编辑入口）是这里唯一可靠的
可观察证据：它记录选择器字段类型、控件数量、写回文本框的初值与覆盖警告。
"""

import re
from pathlib import Path

import pytest

import adapter as A
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.sql

_TREE_ARGS = (
    "--qa-tree-expand", "UITestDB",
    "--qa-tree-expand", "main",
    "--qa-tree-expand", "main",
    "--qa-tree-activate", "events",
)
_ISO_SECONDS = r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}"


def _picker_log(inst: UiInstance) -> str:
    log = Path(inst.home) / "state" / "sqarvo" / "desktop.log"
    A.wait_until(
        lambda: log.is_file() and "日期时间选择器：" in log.read_text(
            encoding="utf-8", errors="replace"),
        15, "日期时间选择器构建日志落地")
    return log.read_text(encoding="utf-8", errors="replace")


def _line_with(text: str, needle: str) -> str:
    return next(line for line in text.splitlines() if needle in line)


def test_valid_datetime_uses_picker_with_canonical_initial_text():
    """合法 DATETIME：建出 1 个日历 + 3 个时间旋钮，文本框初值保持原值。"""
    inst = UiInstance("datetime_picker_valid", db=True)
    try:
        inst.start(autoconnect=True, extra_args=(*_TREE_ARGS, "--qa-open-editor", "0:1"))
        frame = inst.frame()
        with step("table_ready"):
            A.wait_until(
                lambda: A.query(frame, role_in=("button",), name="开始事务"),
                15, "events 表数据页出现")

        with step("picker_built_from_valid_value"):
            line = _line_with(_picker_log(inst), "日期时间选择器：")
            expect("field=Datetime" in line, "字段被识别为 Datetime", line)
            expect("icon=1" in line, "GtkEntry 右侧挂了日历图标", line)
            expect("calendar=1" in line and "spins=3" in line,
                   "建出 1 个日期选择 + 3 个时分秒旋钮", line)
            expect('initial="2026-11-05 10:30:00"' in line,
                   "文本框初值为原日期时间文本", line)
            expect('warning=""' in line, "合法值不显示覆盖警告", line)
    finally:
        inst.dispose()


def test_unparseable_datetime_defaults_to_now_with_visible_warning():
    """无法解析的原值：选择器填当前时间，并点名原值说明应用会覆盖。"""
    inst = UiInstance("datetime_picker_poison", db=True)
    try:
        inst.start(autoconnect=True, extra_args=(*_TREE_ARGS, "--qa-open-editor", "1:1"))
        frame = inst.frame()
        with step("table_ready"):
            A.wait_until(
                lambda: A.query(frame, role_in=("button",), name="开始事务"),
                15, "events 表数据页出现")

        with step("picker_defaults_to_now_and_warns"):
            line = _line_with(_picker_log(inst), "日期时间选择器：")
            match = re.search(r'initial="(' + _ISO_SECONDS + r')"', line)
            expect(match is not None, "初值为当前时间的规范格式", line)
            expect("原值「not-a-date」无法解析为日期时间" in line,
                   "警告点名无法解析的原值", line)
            expect("点「应用」将覆盖原值" in line,
                   "警告明说应用会覆盖", line)
    finally:
        inst.dispose()
