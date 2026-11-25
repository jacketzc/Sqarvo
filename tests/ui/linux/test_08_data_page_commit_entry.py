"""表数据页提交入口收敛（v0.2.3.4）：工具条不再有「提交」，状态栏「应用」是唯一入口。"""
import pytest

import adapter as A
import ops
from ops import expect, step

pytestmark = pytest.mark.sql


def test_commit_entry_is_status_bar_apply_only(ui_instance):
    """打开表后工具条没有「提交」按钮，状态栏「应用」在场且无脏行时禁用。

    注册表组件: data_page.toolbar.commit（removed）、status_bar.row1.ops.apply、
    data_page.toolbar.begin_txn。只断言交互与可访问性名称 / 状态，不做样式或截图断言。
    """
    with step("open_table"):
        frame = ops.open_table_for_qa(ui_instance("commit_entry"), "widgets")

    with step("no_toolbar_commit_button"):
        commits = A.query(frame, role_in=("button",), name="提交")
        expect(not commits, "工具条没有名为「提交」的按钮", [A.nm(x) for x in commits])

    with step("status_bar_apply_is_single_entry"):
        applies = A.query(frame, role_in=("button",),
                          name="应用：提交所有脏行的变更")
        expect(len(applies) == 1, "状态栏「应用」按钮在场且唯一",
               [A.nm(x) for x in applies])
        # 刚打开表、没有脏行：新谓词 commit_available() 应给出 false。
        expect("sensitive" not in A.states(applies[0]),
               "没有脏行时「应用」禁用", A.states(applies[0]))
