"""可复用 UI 操作层：把 Sqarvo 的常见用户流程封装成确定性步骤。

只依赖 adapter（AT-SPI 语义选择器），不依赖 pytest。
测试通过 `step()` 记录当前步骤，失败时报告器输出 `Step:` 行。
控件名一律取自 docs/ui-specification/components/*.toml 的 a11y.name / l10n 文案。
"""
from __future__ import annotations

import contextlib

from adapter import (
    UIError,
    action_names,
    click,
    find_one,
    query,
    states,
    text_value,
    wait_dialog,
    wait_gone,
    wait_until,
    walk,
    window_action,
    nm,
    role,
)
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

# 当前步骤（失败诊断用）。conftest 的报告器读取它。
CURRENT = {"step": None}


class StepFailure(AssertionError):
    def __init__(self, step, expected, actual):
        super().__init__(f"Step: {step}\nExpected: {expected}\nActual: {actual}")
        self.step, self.expected, self.actual = step, expected, actual


@contextlib.contextmanager
def step(name):
    CURRENT["step"] = name
    try:
        yield
    finally:
        CURRENT["step"] = name


def expect(cond, expected, actual=None):
    if not cond:
        raise StepFailure(CURRENT["step"], expected,
                          "<unavailable>" if actual is None else actual)


# ---------- 全局 ----------

def labels(frame):
    return [nm(x) for x in query(frame, role_in=("label",))]


def frame_of(app, timeout: float = 20):
    return wait_until(lambda: _frame(app), timeout, "main frame 'Sqarvo*'")


def _frame(app):
    from adapter import windows
    for w in windows(app):
        if role(w) == "frame" and nm(w).startswith("Sqarvo"):
            return w
    return None


def status_text(frame):
    """状态栏摘要（含「未连接/已连接 · 驱动 N · 浏览会话 M」）。"""
    return [l for l in labels(frame) if "驱动" in l]


# ---------- 连接对话框 ----------

def open_connection_dialog(app, frame):
    """点主工具条「连接」→ 返回新建连接对话框。"""
    btn = find_one(frame, role_in=("button",), name="连接", timeout=15)
    expect("sensitive" in states(btn), "「连接」按钮可用", "sensitive" in states(btn))
    click(btn)
    return wait_dialog(app, "新建连接", timeout=15)


def close_connection_dialog_via_cancel(dlg):
    cancel = find_one(dlg, role_in=("button",), name="取消")
    click(cancel)
    wait_gone(lambda: find_one_or_none(dlg), 10, "连接对话框关闭")


def find_one_or_none(dlg):
    try:
        return dlg if "showing" in states(dlg) else None
    except Exception:
        return None


def dialog_driver_value(dlg):
    """驱动下拉当前值：ComboRow 的已实现列表项内嵌 label 文本。"""
    combo = find_one(dlg, role_in=("combo box",), name="驱动")
    for l in query(combo, role_in=("label",)):
        if "（" in nm(l) and "）" in nm(l):
            return nm(l)
    return ""


# ---------- 对象树 ----------

def tree_rows(frame):
    """侧栏对象树当前可见行（每行的全部 label 文本列表）。"""
    rows = []
    for li in query(frame, role_in=("list item",)):
        texts = [nm(x) for x in query(li, role_in=("label",), max_depth=8) if nm(x)]
        rows.append(texts)
    return rows


def find_row(frame, *must_labels, timeout=0):
    def probe():
        for li in query(frame, role_in=("list item",)):
            texts = [nm(x) for x in query(li, role_in=("label",), max_depth=8)]
            if all(any(m in t for t in texts) for m in must_labels):
                return li
        return None
    if timeout > 0:
        return wait_until(probe, timeout, f"tree row {must_labels}")
    return probe()


def expand_tree_row(row):
    """点行内展开按钮（与用户点三角箭头同一路径）。返回动作名。"""
    btn = find_one(row, role_in=("button",))
    return click(btn)


# ---------- SQL 页 ----------

def query_tab_names(frame):
    return [nm(t) for t in query(frame, role_in=("page tab",))
            if nm(t).startswith("查询")]


def active_query_page(frame, title: str):
    """标题为「查询 N」的页面板（每个标签一个；名与标签条一致，语义锚点）。"""
    def probe():
        hits = [p for p in walk(frame)
                if role(p) == "panel" and nm(p) == title
                and any(role(x) == "button" and nm(x) == "执行"
                        for x in query(p, role_in=("button",)))]
        return hits[0] if hits else None
    return wait_until(probe, 15, f"query page panel '{title}'")


def open_sql_page(frame):
    """点工具条「新建查询」开新标签，返回该页面板容器。"""
    before = query_tab_names(frame)
    btn = find_one(frame, role_in=("button",), name="新建查询", timeout=10)
    expect("sensitive" in states(btn), "已连接时「新建查询」可用",
           "sensitive" in states(btn))
    click(btn)
    wait_until(lambda: len(query_tab_names(frame)) > len(before), 15,
               "新查询标签出现")
    return active_query_page(frame, query_tab_names(frame)[-1])


def editor(page):
    """页内 SQL 编辑器：editable 的多行 text 节点。"""
    def probe():
        for a in query(page, role_in=("text",)):
            if "editable" in states(a):
                return a
        return None
    return wait_until(probe, 15, "SQL editor (editable text)")


def write_sql(page, sql):
    ed = editor(page)
    Atspi.EditableText.set_text_contents(ed, sql)
    expect(text_value(ed) == sql, "SQL 写入编辑器", repr(text_value(ed)[:80]))
    return ed


def execute_sql(page):
    """点页内「执行」按钮；成功返回后由调用方等待结果。"""
    btn = find_one(page, role_in=("button",), name="执行", timeout=10)
    expect("sensitive" in states(btn), "「执行」按钮可用", "sensitive" in states(btn))
    click(btn)


def data_rows(page):
    """结果网格数据行：table row 名首段是行号；返回单元格 label 列表。"""
    out = []
    for r in query(page, role_in=("table row",)):
        name = nm(r)
        if name and name.split(" ")[0].isdigit():
            cells = [nm(c) for c in query(r, role_in=("table cell",)) if nm(c)]
            out.append(cells)
    return out


def wait_query_done(page, min_rows=1, timeout=20):
    rows = wait_until(
        lambda: data_rows(page) if len(data_rows(page)) >= min_rows else None,
        timeout, f"result grid with >= {min_rows} data rows")
    return rows


def sql_summary(frame):
    """窗口状态栏「SQL: …」摘要（活动页专属表面）。"""
    return [nm(l) for l in query(frame, role_in=("label",)) if nm(l).startswith("SQL:")]


# ---------- 连接状态 / 窗口 ----------

def wait_connected(frame, conn_hint: str, timeout=30):
    def probe():
        rows = tree_rows(frame)
        return any(any(conn_hint in t for t in r) and any("已连接" in t for t in r)
                   for r in rows)
    wait_until(probe, timeout, f"tree shows connected row containing '{conn_hint}'")


def close_window(frame):
    window_action(frame, "window.close")


def open_query_page_via_action(frame):
    """Ctrl+T 同一个 GAction（win.new-tab 是「新建查询标签」快捷键的动作本体）。"""
    window_action(frame, "win.new-tab")


# ---------- 查询结果区三页签（#231，组件 tabs.query.results.views/.message/.summary） ----------
# 选择器取自注册表 a11y.name；隐藏页不进 AT-SPI 树（实现口径见 tabs.query.results.views
# impl_notes），读页内容前必须先切到该页。

RESULTS_TABS = ("消息", "摘要", "结果")
SUMMARY_KEYS = ("已执行语句", "成功", "失败", "开始时间", "结束时间", "总耗时")


def results_tab_group(frame, timeout=25):
    """页签组容器（page tab list，name='结果视图页签'）；出现即结果区可见。"""
    return find_one(frame, role_in=("page tab list",), name="结果视图页签",
                    timeout=timeout)


def results_tab(page, name, timeout=25):
    """结果区一枚页签（page tab；selected 态 = 当前页）。"""
    return find_one(page, role_in=("page tab",), name=name, timeout=timeout)


def switch_results_tab(page, name):
    """点击切到指定页签，并等 selected 生效（只读可访问性状态，不做样式断言）。"""
    click(results_tab(page, name))
    wait_until(lambda: "selected" in states(results_tab(page, name)), 15,
               f"results tab '{name}' selected")


def active_results_tab(page):
    """当前页签名（三枚都没 selected 时返回空串）。"""
    for t in query(page, role_in=("page tab",)):
        if "selected" in states(t):
            return nm(t)
    return ""


def messages_log_text(page, timeout=20):
    """消息页日志全文：只读多行 text 节点（编辑器带 editable，据此区分）。"""
    def probe():
        for a in query(page, role_in=("text",)):
            st = states(a)
            if "read-only" in st and "multi-line" in st and "editable" not in st:
                value = text_value(a)
                if value:
                    return value
        return None
    return wait_until(probe, timeout, "messages page log text (read-only multi-line text)")


def summary_pairs(page):
    """摘要页键值对 {键: 值}（值取同一行盒里另一个 label；需先切到摘要页）。"""
    pairs = {}
    for a in query(page, role_in=("label",)):
        key = nm(a)
        if key in SUMMARY_KEYS:
            row = a.get_parent()
            others = [nm(x) for x in query(row, role_in=("label",)) if nm(x) != key]
            if others:
                pairs.setdefault(key, others[0])
    return pairs


def window_width(frame):
    """窗口外框宽（AT-SPI Component.get_extents）。接口不可用 / 值无效时返回
    None——调用方按 BLOCKED（skip + 原因）处理，不得当成通过。"""
    try:
        ext = Atspi.Component.get_extents(frame, Atspi.CoordType.SCREEN)
    except Exception:
        return None
    return ext.width if ext.width > 0 else None


def widget_width(acc):
    """控件外框宽（同上，None = 拿不到）。"""
    try:
        ext = Atspi.Component.get_extents(acc, Atspi.CoordType.SCREEN)
    except Exception:
        return None
    return ext.width if ext.width > 0 else None
