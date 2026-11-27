"""AT-SPI 自动化适配层（Linux / GTK4）。

只做一件事：把「按语义找控件、操作控件、读控件」封装成确定性 API。
- 选择器一律基于 可访问角色(role) + 可访问名(name)，禁止坐标点击与截图识别。
- 所有等待都是「显式条件 + 超时轮询」，不存在裸 sleep。
- 依赖：/usr/bin/python3 的系统 gi + Atspi 2.0（venv --system-site-packages）。

命名约定：窗口/对话框/按钮的 name 与 docs/ui-specification/components/*.toml
的 a11y.name 一致（那是三端行为真值），测试里不发明第二套名字。
"""
from __future__ import annotations

import ctypes as C
import os
import re
import subprocess
import time
from contextlib import contextmanager
from pathlib import Path
from fnmatch import fnmatchcase

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

# 轮询步长：D-Bus 往返本身毫秒级，50ms 足够灵敏又不刷屏。
_POLL = 0.05


class UIError(RuntimeError):
    """自动化操作失败（找不到控件 / 条件超时 / 接口不支持）。"""


# ---------- 基础读取 ----------

def role(acc) -> str:
    try:
        return acc.get_role_name() or ""
    except Exception:
        return ""


def nm(acc) -> str:
    try:
        return (acc.get_name() or "").replace("\n", " ").strip()
    except Exception:
        return ""


def description(acc) -> str:
    try:
        return acc.get_description() or ""
    except Exception:
        return ""


def states(acc) -> set[str]:
    try:
        ss = acc.get_state_set()
        return {s.value_nick for s in ss.get_states()}
    except Exception:
        return set()


def text_value(acc) -> str:
    """读 Text 接口全文（entry/label/text-view 的当前文本）。"""
    try:
        n = Atspi.Text.get_character_count(acc)
        return Atspi.Text.get_text(acc, 0, n) or ""
    except Exception:
        return ""


def action_names(acc) -> list[str]:
    try:
        return [
            Atspi.Action.get_action_name(acc, i) or ""
            for i in range(Atspi.Action.get_n_actions(acc))
        ]
    except Exception:
        return []


# ---------- 树遍历 ----------

def children(acc) -> list:
    out = []
    try:
        n = acc.get_child_count()
    except Exception:
        return out
    for i in range(n):
        try:
            c = acc.get_child_at_index(i)
        except Exception:
            c = None
        if c is not None:
            out.append(c)
    return out


def walk(acc, depth=0, max_depth=40):
    yield acc
    if depth >= max_depth:
        return
    for c in children(acc):
        yield from walk(c, depth + 1, max_depth)


def query(root, *, role_in=None, name=None, name_contains=None,
          name_glob=None, max_depth=40) -> list:
    """按角色/名字筛选子树节点。name 为精确匹配（注册表口径）。"""
    hits = []
    for a in walk(root, max_depth=max_depth):
        if role_in and role(a) not in role_in:
            continue
        n = nm(a)
        if name is not None and n != name:
            continue
        if name_contains is not None and name_contains not in n:
            continue
        if name_glob is not None and not fnmatchcase(n, name_glob):
            continue
        hits.append(a)
    return hits


# ---------- 等待 ----------

def wait_until(fn, timeout: float, what: str):
    """轮询 fn() 直到返回真值；超时抛 UIError(what)。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        val = fn()
        if val:
            return val
        time.sleep(_POLL)
    raise UIError(f"timeout waiting for: {what}")


def desktop() -> Atspi.Accessible:
    return Atspi.get_desktop(0)


def find_app(name_contains: str) -> object:
    """按应用名（不区分大小写包含匹配）找 AT-SPI 应用根。"""
    d = desktop()
    for i in range(d.get_child_count()):
        c = d.get_child_at_index(i)
        if c is not None and name_contains.lower() in (c.get_name() or "").lower():
            return c
    return None


def wait_app(name_contains: str, timeout: float = 30.0):
    return wait_until(lambda: find_app(name_contains), timeout,
                      f"app matching '{name_contains}' on AT-SPI bus")


def windows(app) -> list:
    """应用下的顶层窗口（frame/window/dialog）。"""
    return [a for a in children(app) if role(a) in ("frame", "window", "dialog")]


def find_window(app, name_glob: str):
    for w in windows(app):
        if fnmatchcase(nm(w), name_glob):
            return w
    return None


def wait_window(app, name_glob: str, timeout: float = 15.0):
    return wait_until(lambda: find_window(app, name_glob), timeout,
                      f"window matching '{name_glob}'")


def find_dialog(app, name: str):
    """AdwDialog 是窗口内 role=dialog 的后代节点，不是顶层窗口。"""
    for w in windows(app):
        for d in query(w, role_in=("dialog", "alert"), name=name):
            if "showing" in states(d):
                return d
    return None


def wait_dialog(app, name: str, timeout: float = 15.0):
    return wait_until(lambda: find_dialog(app, name), timeout,
                      f"dialog '{name}'")


def wait_gone(fn, timeout: float, what: str):
    """等待某条件消失（如对话框关闭）。fn 返回假值即成功。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not fn():
            return
        time.sleep(_POLL)
    raise UIError(f"timeout waiting for disappearance: {what}")


# ---------- 操作 ----------

def click(acc, action_index: int = 0):
    """语义点击：调用控件的 Action#n（通常是 'click'/'press'）。"""
    names = action_names(acc)
    if not names:
        raise UIError(f"no AT-SPI action on {role(acc)}|{nm(acc)}")
    idx = action_index
    if action_index == 0 and len(names) > 1 and "click" in [x.lower() for x in names]:
        idx = [x.lower() for x in names].index("click")
    ok = Atspi.Action.do_action(acc, idx)
    if not ok:
        raise UIError(f"do_action failed on {role(acc)}|{nm(acc)} ({names[idx]})")
    return names[idx]


@contextmanager
def _xtest_display():
    """Open only the display provisioned by the isolated UI runner."""
    run = os.environ.get("SQARVO_UI_RUN_DIR")
    scope = os.environ.get("SQARVO_UI_SCOPE")
    display_name = os.environ.get("DISPLAY")
    if not (
        run and scope and os.environ.get("XAUTHORITY") == f"{run}/Xauthority"
        and Path("/proc/self/cgroup").read_text().strip().endswith(f"/{scope}")
        and display_name == f":{(Path(run) / 'display').read_text().strip()}"
    ):
        raise UIError("XTEST input requires the isolated UI runner")
    x11 = C.CDLL("libX11.so.6")
    xtest = C.CDLL("libXtst.so.6")
    x11.XOpenDisplay.argtypes = [C.c_char_p]
    x11.XOpenDisplay.restype = C.c_void_p
    x11.XCloseDisplay.argtypes = [C.c_void_p]
    x11.XSync.argtypes = [C.c_void_p, C.c_int]
    display = x11.XOpenDisplay(display_name.encode())
    if not display:
        raise UIError("cannot open the isolated X display")
    try:
        yield x11, xtest, display
    finally:
        x11.XSync(display, False)
        x11.XCloseDisplay(display)


def press_keys(*names: str):
    """Send a key chord to the focused control on the runner's private X server."""
    with _xtest_display() as (x11, xtest, display):
        x11.XStringToKeysym.argtypes = [C.c_char_p]
        x11.XStringToKeysym.restype = C.c_ulong
        x11.XKeysymToKeycode.argtypes = [C.c_void_p, C.c_ulong]
        x11.XKeysymToKeycode.restype = C.c_ubyte
        xtest.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        codes = [x11.XKeysymToKeycode(display, x11.XStringToKeysym(name.encode()))
                 for name in names]
        if not all(codes):
            raise UIError(f"unmapped key chord: {names}")
        for code in codes:
            xtest.XTestFakeKeyEvent(display, code, True, 0)
        for code in reversed(codes):
            xtest.XTestFakeKeyEvent(display, code, False, 0)


def _pointer_origin():
    env = dict(os.environ, LC_ALL="C")

    def active_window():
        prop = subprocess.check_output(
            ["xprop", "-root", "_NET_ACTIVE_WINDOW"], env=env, text=True,
        )
        match = re.search(r"0x[0-9a-f]+", prop)
        return match[0] if match and int(match[0], 16) else None

    window = wait_until(active_window, 8, "active X11 window")
    geometry = subprocess.check_output(["xwininfo", "-id", window], env=env, text=True)
    origin = re.findall(r"Absolute upper-left [XY]:\s*(-?\d+)", geometry)
    if len(origin) != 2:
        raise UIError("cannot read active X11 window origin")
    return int(origin[0]), int(origin[1])


def _pointer_target(acc, horizontal_fraction, vertical_fraction=0.5, client_origin=None,
                    *, within=None):
    def visible_bounds():
        bounds = Atspi.Component.get_extents(acc, Atspi.CoordType.WINDOW)
        if bounds.width <= 0 or bounds.height <= 0 or "showing" not in states(acc):
            return None
        if within is not None:
            # GTK can expose a reopened menu item at its former editor coordinates.
            popup = Atspi.Component.get_extents(within, Atspi.CoordType.WINDOW)
            if ("showing" not in states(within)
                    or not (popup.x <= bounds.x and bounds.x + bounds.width <= popup.x + popup.width
                            and popup.y <= bounds.y and bounds.y + bounds.height <= popup.y + popup.height)):
                return None
        return bounds

    target = f"{role(acc)}|{nm(acc)}"
    bounds = (visible_bounds() if within is None else
              wait_until(visible_bounds, 5, f"{target} allocated inside {role(within)}|{nm(within)}"))
    if bounds is None:
        raise UIError(f"no visible bounds for {target}")
    # GTK4 reports (0, 0) for SCREEN coordinates. Offset the window-relative
    # AT-SPI bounds by the X11 client's origin on the private display.
    x, y = client_origin if client_origin is not None else _pointer_origin()
    return (x + bounds.x + int(bounds.width * horizontal_fraction),
            y + bounds.y + int(bounds.height * vertical_fraction))


def _mouse_motion(xtest, display, target):
    xtest.XTestFakeMotionEvent.argtypes = [
        C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong,
    ]
    if not xtest.XTestFakeMotionEvent(display, -1, *target, 0):
        raise UIError("XTEST pointer motion failed")


def _mouse_button(xtest, display, button, pressed):
    xtest.XTestFakeButtonEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
    if not xtest.XTestFakeButtonEvent(display, button, pressed, 0):
        raise UIError(f"XTEST pointer button {button} failed")


def mouse_click(acc, *, button: int = 1, horizontal_fraction: float = 0.5,
                vertical_fraction: float = 0.5, count: int = 1, within=None):
    """Click the AT-SPI bounds of a semantically located control."""
    with _xtest_display() as (_, xtest, display):
        target = _pointer_target(acc, horizontal_fraction, vertical_fraction, within=within)
        _mouse_motion(xtest, display, target)
        for _ in range(count):
            _mouse_button(xtest, display, button, True)
            _mouse_button(xtest, display, button, False)


def mouse_click_surface(point):
    """Click a live native control point in client-surface coordinates."""
    with _xtest_display() as (_, xtest, display):
        x, y = _pointer_origin()
        _mouse_motion(xtest, display, (x + point[0], y + point[1]))
        _mouse_button(xtest, display, 1, True)
        _mouse_button(xtest, display, 1, False)


@contextmanager
def mouse_drag(source, *, horizontal_fraction=0.25):
    """Hold the primary button and move relative to accessible controls."""
    with _xtest_display() as (x11, xtest, display):
        client_origin = _pointer_origin()
        origin = _pointer_target(source, horizontal_fraction, client_origin=client_origin)
        _mouse_motion(xtest, display, origin)
        _mouse_button(xtest, display, 1, True)
        x11.XSync(display, False)

        def move_to(acc=None, horizontal_fraction=0.5, vertical_fraction=0.5, *,
                    surface_point=None, offset=None):
            if offset is not None:
                target = (origin[0] + offset[0], origin[1] + offset[1])
            elif surface_point is not None:
                target = (client_origin[0] + surface_point[0], client_origin[1] + surface_point[1])
            else:
                target = origin if acc is None else _pointer_target(
                    acc, horizontal_fraction, vertical_fraction, client_origin
                )
            _mouse_motion(xtest, display, target)
            x11.XSync(display, False)

        try:
            yield move_to
        finally:
            _mouse_button(xtest, display, 1, False)


def set_text(acc, text: str):
    """EditableText 写入（entry / text-view）。"""
    if not Atspi.EditableText.set_text_contents(acc, text):
        raise UIError(f"set_text_contents failed on {role(acc)}|{nm(acc)}")


def wait_enabled(acc, timeout: float, what: str = ""):
    """GTK 的敏感态在 AT-SPI 里是 `sensitive` 状态（不是 enabled）。"""
    wait_until(lambda: "sensitive" in states(acc), timeout,
               f"sensitive {what or nm(acc)}")


def window_action(frame, action: str):
    """触发窗口级 GAction（frame 以 `win.*`/`window.*` 名义暴露动作）。"""
    acts = action_names(frame)
    if action not in acts:
        raise UIError(f"window action '{action}' not exposed; has {sorted(acts)[:8]}…")
    if not Atspi.Action.do_action(frame, acts.index(action)):
        raise UIError(f"window action '{action}' do_action returned false")


def find_one(root, *, timeout: float = 0.0, **kw):
    """query 的单结果便利封装；找不到且 timeout>0 则等待。"""
    if timeout > 0:
        return wait_until(lambda: (query(root, **kw) or [None])[0], timeout,
                          f"control {kw}")
    hits = query(root, **kw)
    if not hits:
        raise UIError(f"control not found: {kw}")
    return hits[0]


# ---------- 诊断 ----------

def dump_tree(acc, budget: int = 2000) -> str:
    """完整子树文本（仅在失败诊断/文件落盘时用，绝不进常规输出）。"""
    lines = []
    for a in walk(acc):
        if budget <= 0:
            lines.append("... (truncated)")
            break
        budget -= 1
        lines.append(
            f"{role(a)}|{nm(a)}|states={sorted(states(a))}"
            f"|actions={action_names(a)}"
        )
    return "\n".join(lines)
