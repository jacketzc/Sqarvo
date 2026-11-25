"""pytest 夹具 + 结构化报告器（UI 功能测试专用）。

- 每个实例一套隔离的 XDG 目录（config/data/state）+ 独立 SQLite 文件，绝不触碰
  用户 `~/.config/sqarvo` 或真实数据库。
- 实例经 `SQARVO_QA_APP_ID` 与用户正在运行的窗口共存（前端 AGENTS §13 验收通道）。
- `--qa-exit-after-ms` 看门狗兜底：即使本进程被 Ctrl-C 掐死，应用也会自杀退出。
- 输出契约（根 AGENTS §13 / 任务 §7）：成功只打摘要；失败打步骤级诊断并落盘
  完整可访问性子树到 tests/ui/logs/，常规输出不含树、日志、截图。
"""
from __future__ import annotations

import json
import os
import shutil
import signal
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

import pytest

HERE = Path(__file__).parent
UIROOT = HERE
RESULTS_DIR = UIROOT / "results"
LOGS_DIR = UIROOT / "logs"

import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

import adapter as A  # noqa: E402
import ops  # noqa: E402

WATCHDOG_MS = int(os.environ.get("SQARVO_UI_WATCHDOG_MS", "420000"))
READY_TIMEOUT = float(os.environ.get("SQARVO_UI_READY_TIMEOUT", "40"))


def _bin_path() -> Path:
    env = os.environ.get("SQARVO_UI_BIN")
    if env:
        return Path(env)
    repo = UIROOT.parents[2] / "frontend/linux/target/release/sqarvo-desktop"
    if repo.exists():
        return repo
    local = Path.home() / ".local/bin/sqarvo-desktop"
    if local.exists():
        return local
    pytest.exit("UI TEST: FAIL\nError: 找不到 sqarvo-desktop（构建它或设 SQARVO_UI_BIN）",
                returncode=3)
    raise SystemExit(3)  # pytest.exit 正常会抛异常，这行只是类型完备


BIN = _bin_path()

# ---------- 环境可行性门 ----------

def _atspi_usable():
    try:
        Atspi.init()
        d = Atspi.get_desktop(0)
        return d.get_child_count() >= 0
    except Exception as e:  # pragma: no cover
        return f"{e!r}"


USABLE = _atspi_usable()


def pytest_configure(config):
    if USABLE is not True:
        pytest.exit(f"UI TEST: FAIL\nError: AT-SPI 不可用（{USABLE}）：需要运行中的 GNOME/Wayland 会话",
                    returncode=3)
    # 防呆：总线的 sqarvo 应用根唯一（残留进程会干扰定位）。
    stale = [c for i in range(Atspi.get_desktop(0).get_child_count())
             if (c := Atspi.get_desktop(0).get_child_at_index(i)) is not None
             and "sqarvo" in (c.get_name() or "").lower()]
    if stale:
        pytest.exit("UI TEST: FAIL\nError: AT-SPI 上有残留 sqarvo 实例，请先退出它",
                    returncode=3)


# ---------- 实例生命周期 ----------

_INSTANCES: list = []


class UiInstance:
    """一个 sqarvo-desktop 进程 + 它的隔离环境（XDG 目录、SQLite 档案）。"""

    def __init__(self, tag: str, fresh: bool = True, db: bool = True):
        self.tag = tag
        self.home = UIROOT / ".work" / tag
        if fresh:
            shutil.rmtree(self.home, ignore_errors=True)
        for sub in ("config", "data", "state"):
            (self.home / sub).mkdir(parents=True, exist_ok=True)
        self.db = self.home / "data" / "uitest.db"
        if db:
            _make_sqlite_db(self.db)
        self.profile = None
        self.proc = None
        self.app = None

    def start(self, autoconnect=False, *, extra_args=()):
        args = [str(BIN), *extra_args]
        if autoconnect:
            self.profile = self.home / "profile.json"
            self.profile.write_text(json.dumps(
                {"driver": "sqlite", "path": str(self.db), "name": "UITestDB"}),
                encoding="utf-8")
            args += ["--qa-profile", str(self.profile)]
        args += ["--qa-exit-after-ms", str(WATCHDOG_MS)]
        env = dict(os.environ)
        env.update({
            "XDG_CONFIG_HOME": str(self.home / "config"),
            "XDG_DATA_HOME": str(self.home / "data"),
            "XDG_STATE_HOME": str(self.home / "state"),
            "SQARVO_QA_APP_ID": f"io.sqarvo.Sqarvo.UITest.{os.getpid()}.{self.tag}",
        })
        self.env = env
        _INSTANCES.append(self)
        self.proc = subprocess.Popen(args, env=env, start_new_session=True,
                                     stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL)
        self.app = A.wait_app("sqarvo-desktop", timeout=READY_TIMEOUT)
        return self

    def frame(self):
        return ops.frame_of(self.app, timeout=READY_TIMEOUT)

    def app_alive(self):
        return self.proc is not None and self.proc.poll() is None

    def stop(self, grace=10.0):
        if self.proc is None:
            return
        if self.proc.poll() is None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except ProcessLookupError:
                return
            try:
                self.proc.wait(timeout=grace)
            except subprocess.TimeoutExpired:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
                self.proc.wait(timeout=5)
        # 后端由桌面进程 PDEATHSIG 清理；这里只确认没有孤儿。
        time.sleep(0.3)
        for i in range(Atspi.get_desktop(0).get_child_count()):
            c = Atspi.get_desktop(0).get_child_at_index(i)
            if c is not None and "sqarvo-desktop" in (c.get_name() or ""):
                raise RuntimeError(f"实例 {self.tag} 退出后 AT-SPI 根仍在（残留进程）")

    def dispose(self):
        self.stop()
        if self in _INSTANCES:
            _INSTANCES.remove(self)
        shutil.rmtree(self.home, ignore_errors=True)


def _make_sqlite_db(path: Path):
    if path.exists():
        path.unlink()
    con = sqlite3.connect(path)
    con.execute("CREATE TABLE widgets(id INTEGER PRIMARY KEY, name TEXT NOT NULL, qty INTEGER NOT NULL)")
    con.executemany("INSERT INTO widgets(name, qty) VALUES(?,?)",
                    [("bolt", 10), ("nut", 20), ("gear", 5)])
    # 空表：数据页必须照常绘制表头，且能直接新增行（不进占位页）。
    con.execute("CREATE TABLE widgets_empty(id INTEGER PRIMARY KEY, name TEXT NOT NULL, qty INTEGER NOT NULL)")
    # 日期时间选择器：合法 DATETIME 与无法解析的原值各一行（列序 id=0, when_ts=1）。
    con.execute("CREATE TABLE events(id INTEGER PRIMARY KEY, when_ts DATETIME NOT NULL)")
    con.executemany("INSERT INTO events(when_ts) VALUES(?)",
                    [("2026-11-05 10:30:00",), ("not-a-date",)])
    con.commit()
    con.close()


@pytest.fixture(scope="module")
def app_bare():
    """未连接实例：启动 / 可访问性 / 对话框类测试共用。"""
    inst = UiInstance("bare").start(autoconnect=False)
    yield inst
    inst.dispose()


@pytest.fixture(scope="module")
def app_connected():
    """SQLite 已连接实例：连接状态 / SQL / 结果 / 树导航测试共用。"""
    inst = UiInstance("conn").start(autoconnect=True)
    ops.wait_connected(inst.frame(), "uitest.db", timeout=READY_TIMEOUT)
    yield inst
    inst.dispose()


# ---------- 失败诊断 ----------

_FAILURE_CTX = {}

_REPORT = {"tests": [], "started": time.time()}
_DURATIONS = {}


def pytest_runtest_logreport(report):
    _DURATIONS[report.nodeid] = _DURATIONS.get(report.nodeid, 0.0) + report.duration
    if report.when == "call" or (report.when == "setup" and report.outcome != "passed"):
        entry = {
            "test": report.nodeid.split("::")[-1],
            "outcome": report.outcome,
            "duration_s": round(_DURATIONS[report.nodeid], 2),
            "step": ops.CURRENT.get("step"),
        }
        if report.outcome == "failed":
            msg = str(report.longrepr.reprcrash.message) if hasattr(report.longrepr, "reprcrash") else str(report.longrepr)
            entry["error"] = msg
            for line in msg.splitlines():
                if line.startswith("Expected: "):
                    entry["expected"] = line[10:]
                elif line.startswith("Actual: "):
                    entry["actual"] = line[8:]
        _REPORT["tests"].append(entry)


def pytest_exception_interact(node, call, report):
    """失败瞬间抓活体诊断（进程还开着才有意义）。"""
    try:
        inst = next((i for i in reversed(_INSTANCES) if i.app_alive()), None)
        if inst is None:
            return
        frame = inst.frame()
        dump = A.dump_tree(frame)
        LOGS_DIR.mkdir(exist_ok=True)
        path = LOGS_DIR / f"{report.nodeid.replace('::', '_')}.txt"
        path.write_text(
            f"step={ops.CURRENT.get('step')}\n--- accessible tree ---\n{dump}\n",
            encoding="utf-8")
        _FAILURE_CTX[report.nodeid] = str(path)
    except Exception as e:  # pragma: no cover
        print(f"[diagnostics] 采集失败：{e}")


def pytest_terminal_summary(terminalreporter, *args, **kwargs):
    tests = _REPORT["tests"]
    passed = sum(1 for t in tests if t["outcome"] == "passed")
    failed = sum(1 for t in tests if t["outcome"] == "failed")
    skipped = terminalreporter.stats.get("skipped", [])
    dur = time.time() - _REPORT["started"]
    lines = []
    verdict = "PASS" if failed == 0 else "FAIL"
    lines.append(f"UI TEST: {verdict}")
    lines.append(f"Passed: {passed}")
    lines.append(f"Failed: {failed}")
    if skipped:
        lines.append(f"Skipped: {len(skipped)}")
    lines.append(f"Duration: {dur:.1f}s")
    for t in tests:
        if t["outcome"] == "failed":
            lines.append("----")
            lines.append(f"Test: {t['test']}")
            if t.get("step"):
                lines.append(f"Step: {t['step']}")
            if t.get("expected"):
                lines.append(f"Expected: {t['expected']}")
            if t.get("actual"):
                lines.append(f"Actual: {t['actual']}")
            lines.append(f"Error: {t.get('error', '').splitlines()[0][:200]}")
    for nodeid, path in _FAILURE_CTX.items():
        lines.append(f"Diagnostic: {path}")
    text = "\n".join(lines)
    terminalreporter.write("\n" + text + "\n")
    RESULTS_DIR.mkdir(exist_ok=True)
    _REPORT.update({"verdict": verdict, "passed": passed, "failed": failed,
                    "duration_s": round(dur, 2),
                    "report": str(RESULTS_DIR / "report.json")})
    (RESULTS_DIR / "report.json").write_text(
        json.dumps(_REPORT, ensure_ascii=False, indent=1), encoding="utf-8")
    (RESULTS_DIR / "summary.txt").write_text(text + "\n", encoding="utf-8")
