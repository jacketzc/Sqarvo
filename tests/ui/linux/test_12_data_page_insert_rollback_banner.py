"""插入失败提示只进状态簇的过程消息，不在工具条与编辑器之间新增元素。

回归：新增行提交时若 NOT NULL 字段留空导致整批回滚，曾在页首横幅复述整句、带
「重启 Sqarvo」按钮，还弹出逐行错误列表——这些都在工具条与编辑器之间凭空加了
元素，且重启后端解决不了普通校验失败。现改为：全文（点名字段 + 整批回滚 +
草稿保留 + 错误码）都写在过程消息里；24 字上限只做视觉省略，悬停该文本给全文。

注册表组件: data_page.toolbar.status_cluster（过程消息承载全文，窄时限宽 + tooltip）、
data_page.banner（普通失败不提供「重启 Sqarvo」，仅会话丢失 / 断连提供）、tabs.strip.tab。
只断言交互、状态流转、文案与控件可见性，不做样式 / 截图断言（ADR 0029）。

对象树行的 AT-SPI 激活在本环境不可用（README 已知限制 1），故用应用自带的
`--qa-tree-activate` 走与双击 / 回车同一个 `activate` 信号；提交按钮点击是真实 AT-SPI。
"""

from pathlib import Path

import pytest

import adapter as A
import ops
from conftest import UiInstance
from ops import expect, step

pytestmark = pytest.mark.sql


def test_insert_failure_hint_is_in_status_cluster_process_message():
    """对空表新增一行（NOT NULL 列全留空）提交 → 过程消息点名字段，且不误导重启。"""
    inst = UiInstance("insert_failure_hint", db=True)
    try:
        inst.start(autoconnect=True, extra_args=(
            "--qa-tree-expand", "UITestDB",
            "--qa-tree-expand", "main",
            "--qa-tree-expand", "main",
            "--qa-tree-activate", "widgets_empty",
        ))
        frame = inst.frame()

        with step("open_table_and_add_row"):
            A.wait_until(
                lambda: A.query(frame, role_in=("button",), name="开始事务"),
                15, "表数据页工具条出现（「开始事务」在场）")
            adds = A.query(frame, role_in=("button",),
                           name="新增行：在当前表末尾新增一行")
            expect(len(adds) == 1, "状态栏「新增行」按钮在场且唯一",
                   [A.nm(x) for x in adds])
            A.click(adds[0])
            tab = A.find_one(frame, role_in=("page tab",), name="widgets_empty")
            A.wait_until(lambda: "有未保存的更改" in (tab.get_description() or ""),
                         8, "新增行草稿反映在标签的未保存状态")

        with step("submit_row_that_violates_not_null"):
            applies = A.query(frame, role_in=("button",),
                              name="应用：提交所有脏行的变更")
            A.wait_until(lambda: [b for b in applies if "sensitive" in A.states(b)],
                         10, "新增行后「应用」变为可用")
            A.click(applies[0])

        # 服务端插入失败 → 整批回滚日志落地；这是结果落定的可靠信号。
        log = Path(inst.home) / "state" / "sqarvo" / "desktop.log"
        with step("process_message_carries_full_hint"):
            A.wait_until(
                lambda: log.is_file() and "插入整批回滚：库里 0 行变化" in log.read_text(
                    encoding="utf-8", errors="replace"),
                15, "插入整批回滚日志落地")
            A.wait_until(
                lambda: [t for t in ops.labels(frame) if "不能为空" in t],
                10, "过程消息显示「列 X 不能为空」")
            hints = [t for t in ops.labels(frame) if "插入失败" in t]
            expect(any("列 name 不能为空" in t for t in hints),
                   "过程消息点名 NOT NULL 字段 name", hints[:4])
            # 全文都在过程消息里（窄时视觉省略，悬停同一处给全文）：点名字段 + 整批回滚 +
            # 草稿保留 + 错误码都在同一条文案。
            expect(any("整批已回滚" in t and "草稿保留可重试" in t for t in hints),
                   "过程消息含完整原因（整批回滚 / 草稿保留）", hints[:4])
            expect("有未保存的更改" in (tab.get_description() or ""),
                   "提交失败后标签继续报告未保存状态")

        with step("no_new_element_between_toolbar_and_editor"):
            # 不新增页首横幅 / 逐行错误列表：工具条与编辑器之间不冒出新元素。
            expect(not [t for t in ops.labels(frame) if "第 1 行：" in t],
                   "不显示逐行错误列表（提示只在过程消息）",
                   [t for t in ops.labels(frame) if "第 1 行：" in t][:4])
            restarts = A.query(frame, role_in=("button",), name="重启 Sqarvo")
            expect(not restarts, "普通插入失败不提供「重启 Sqarvo」",
                   [A.nm(x) for x in restarts])
    finally:
        inst.dispose()
