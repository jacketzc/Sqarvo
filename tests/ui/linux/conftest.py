"""pytest 夹具 + 结构化报告器（UI 功能测试专用）。

- 统一入口先建立显示、D-Bus 与 scope 隔离，夹具再为各应用实例分配独立数据和端点。
- 失败时将语义树和应用文本日志写入本次运行的诊断目录。
"""
from __future__ import annotations

import json
import os
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path

import pytest

scope = os.environ.get("SQARVO_UI_SCOPE", "")
run_dir = os.environ.get("SQARVO_UI_RUN_DIR", "")
WORK_DIR = Path(run_dir) / "work"
RESULTS_DIR = Path(os.environ.get("SQARVO_UI_ARTIFACTS", ""))
LOGS_DIR = RESULTS_DIR / "logs"

import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

import adapter as A  # noqa: E402
import ops  # noqa: E402

WATCHDOG_MS = int(os.environ.get("SQARVO_UI_WATCHDOG_MS", "420000"))
READY_TIMEOUT = float(os.environ.get("SQARVO_UI_READY_TIMEOUT", "40"))


# ---------- 环境可行性门 ----------

def _atspi_usable():
    try:
        Atspi.init()
        d = Atspi.get_desktop(0)
        return d.get_child_count() >= 0
    except Exception as e:  # pragma: no cover
        return f"{e!r}"


def pytest_configure(config):
    if not (
        scope and run_dir
        and Path("/proc/self/cgroup").read_text().strip().endswith(f"/{scope}")
        and os.environ.get("DBUS_SESSION_BUS_ADDRESS", "").startswith(f"unix:path={run_dir}/session,")
        and os.environ.get("XAUTHORITY") == f"{run_dir}/Xauthority"
        and os.environ.get("GDK_BACKEND") == "x11"
        and "WAYLAND_DISPLAY" not in os.environ
    ):
        pytest.exit("UI TEST: BLOCKED\nError: 请通过 scripts/run-ui-tests-isolated.sh 运行",
                    returncode=3)
    usable = _atspi_usable()
    if usable is not True:
        pytest.exit(f"UI TEST: BLOCKED\nError: 隔离 AT-SPI 不可用（{usable}）",
                    returncode=3)
    # 防呆：总线的 sqarvo 应用根唯一（残留进程会干扰定位）。
    stale = [c for i in range(Atspi.get_desktop(0).get_child_count())
             if (c := Atspi.get_desktop(0).get_child_at_index(i)) is not None
             and "sqarvo" in (c.get_name() or "").lower()]
    if stale:
        pytest.exit("UI TEST: BLOCKED\nError: 本次隔离 AT-SPI 会话已有 Sqarvo 实例",
                    returncode=3)


# ---------- 实例生命周期 ----------

_INSTANCES: list = []


class UiInstance:
    """一个 sqarvo-desktop 进程 + 它的隔离环境（XDG 目录、SQLite 档案）。"""

    def __init__(self, tag: str, fresh: bool = True, db: bool = True):
        self.tag = tag
        self.home = WORK_DIR / tag
        if fresh:
            shutil.rmtree(self.home, ignore_errors=True)
        for sub in ("config", "data", "state", "cache"):
            (self.home / sub).mkdir(parents=True, exist_ok=True)
        self.db = self.home / "data" / "uitest.db"
        if db:
            _make_sqlite_db(self.db)
        # 请求日志：只在本实例的隔离目录里，供请求计数断言读取（不含截图/视觉断言）。
        self.log_path = self.home / "app.log"
        self._log_handle = None
        self.profile = None
        self.proc = None
        self.app = None
        self.runtime = None

    def start(self, autoconnect=False, *, extra_args=()):
        args = [os.environ["SQARVO_UI_BIN"], *extra_args]
        if autoconnect:
            self.profile = self.home / "profile.json"
            self.profile.write_text(json.dumps(
                {"driver": "sqlite", "path": str(self.db), "name": "UITestDB"}),
                encoding="utf-8")
            args += ["--qa-profile", str(self.profile)]
        args += ["--qa-exit-after-ms", str(WATCHDOG_MS)]
        # Keep the backend socket private and short enough for AF_UNIX.
        self.runtime = tempfile.TemporaryDirectory(prefix="ipc-", dir=run_dir)
        endpoint = Path(self.runtime.name) / "sqarvo" / "backend-endpoint.json"
        env = dict(os.environ)
        env.update({
            "XDG_CONFIG_HOME": str(self.home / "config"),
            "XDG_DATA_HOME": str(self.home / "data"),
            "XDG_STATE_HOME": str(self.home / "state"),
            "XDG_CACHE_HOME": str(self.home / "cache"),
            # Share the runtime with the companion and MCP CLI. The companion
            # supervisor derives this same directory from SQARVO_ENDPOINT_FILE.
            "XDG_RUNTIME_DIR": self.runtime.name,
            "SQARVO_ENDPOINT_FILE": str(endpoint),
            "SQARVO_QA_APP_ID": f"io.sqarvo.Sqarvo.UITest.P{os.getpid()}.{self.tag}",
        })
        self.env = env
        _INSTANCES.append(self)
        self._log_handle = self.log_path.open("wb")
        self.proc = subprocess.Popen(args, cwd=self.home, env=env, start_new_session=True,
                                     stdout=subprocess.DEVNULL,
                                     stderr=self._log_handle)
        self.app = A.wait_app("sqarvo-desktop", timeout=READY_TIMEOUT)
        return self

    def log_text(self):
        """应用 stderr 快照（请求计数断言只数稳定的 `[qa.metadata] submit …` 行）。"""
        if not self.log_path.exists():
            return ""
        return self.log_path.read_text(encoding="utf-8", errors="replace")

    def frame(self):
        return ops.frame_of(self.app, timeout=READY_TIMEOUT)

    def app_alive(self):
        return self.proc is not None and self.proc.poll() is None

    def stop(self, grace=10.0):
        if self.proc is not None and self.proc.poll() is None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.proc.wait(timeout=grace)
            except subprocess.TimeoutExpired:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
                self.proc.wait(timeout=5)
        if self._log_handle is not None:
            self._log_handle.close()
            self._log_handle = None
        # 后端由桌面进程 PDEATHSIG 清理；等待本实例从可访问性总线注销。
        if self.app is not None:
            A.wait_gone(lambda: A.find_app("sqarvo-desktop"), 5,
                        f"实例 {self.tag} 的 AT-SPI 根退出")

    def dispose(self):
        self.stop()
        if self.runtime is not None:
            self.runtime.cleanup()
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


@pytest.fixture
def ui_instance(request):
    """保留运行中的失败实例，等报告器采集语义树后再清理。"""
    ops.CURRENT["step"] = None

    def create(tag):
        inst = UiInstance(tag)
        request.addfinalizer(inst.dispose)
        return inst
    return create


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
    if report.when == "call" or report.failed or (report.when == "setup" and report.skipped):
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
        LOGS_DIR.mkdir(exist_ok=True)
        name = report.nodeid.replace("::", "_")
        if inst.log_path.is_file():
            shutil.copyfile(inst.log_path, LOGS_DIR / f"{name}.log")
        frame = inst.frame()
        dump = A.dump_tree(frame)
        path = LOGS_DIR / f"{name}.txt"
        path.write_text(
            f"step={ops.CURRENT.get('step')}\n--- accessible tree ---\n{dump}\n",
            encoding="utf-8")
        _FAILURE_CTX[report.nodeid] = str(path)
    except Exception as e:  # pragma: no cover
        print(f"[diagnostics] 采集失败：{e}")


def pytest_terminal_summary(terminalreporter, exitstatus):
    tests = _REPORT["tests"]
    passed = sum(1 for t in tests if t["outcome"] == "passed")
    failed = sum(1 for t in tests if t["outcome"] == "failed")
    skipped = terminalreporter.stats.get("skipped", [])
    dur = time.time() - _REPORT["started"]
    lines = []
    verdict = "PASS" if exitstatus == 0 else "FAIL"
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
