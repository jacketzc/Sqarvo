"""Targeted regressions for the Linux isolation runner.

Run with /usr/bin/python3. Add --host to exercise real scopes and SIGKILL.
"""

from contextlib import ExitStack
import fcntl
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch


ROOT = Path(__file__).resolve().parents[3]
RUNNER = ROOT / "scripts/run-ui-tests-isolated.sh"
CLEANUP = runpy.run_path(str(ROOT / "scripts/cleanup-ui-runs.py"))
CGROUP_PARENT = Path("/sys/fs/cgroup/test-user/app.slice")
NAME = "sqarvo-ui.ABCDE123"


class RunCleanupTests(unittest.TestCase):
    def setUp(self):
        self.parent = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.fd = os.open(self.parent, os.O_DIRECTORY)
        self.addCleanup(os.close, self.fd)
        self.run = self.parent / NAME
        self.run.mkdir(mode=0o700)
        self.marker = self.run / CLEANUP["LEASE"]
        self.marker.write_text(f"{NAME}.scope\n{CGROUP_PARENT}\n")
        self.marker.chmod(0o600)

    def remove(self, inspect):
        return CLEANUP["remove_run"](self.fd, NAME, CGROUP_PARENT, inspect)

    def test_live_lease_prevents_removal_without_consulting_systemd(self):
        fd = os.open(self.run, os.O_RDONLY)
        self.addCleanup(os.close, fd)
        fcntl.flock(fd, fcntl.LOCK_EX)
        self.assertFalse(self.remove(lambda *_: self.fail("checked a live instance")))
        self.assertTrue(self.run.is_dir())

    def test_normal_cleanup_reuses_held_lease(self):
        fd = os.open(self.run, os.O_RDONLY)
        self.addCleanup(os.close, fd)
        fcntl.flock(fd, fcntl.LOCK_EX)
        removed = CLEANUP["remove_run"](self.fd, NAME, CGROUP_PARENT, lambda *_: True, lease_fd=fd)
        self.assertTrue(removed)
        self.assertFalse(self.run.exists())

    def test_scope_must_be_finished(self):
        with self.assertRaisesRegex(ValueError, "scope may still have processes"):
            self.remove(lambda *_: False)
        self.assertTrue(self.run.is_dir())

    def test_finished_scope_allows_cleanup_without_following_symlinks(self):
        target = self.parent / "unrelated"
        target.mkdir()
        sentinel = target / "keep"
        sentinel.touch()
        (self.run / "link").symlink_to(target, target_is_directory=True)

        def finished(scope, cgroup):
            self.assertEqual(scope, f"{NAME}.scope")
            self.assertEqual(cgroup, CGROUP_PARENT / scope)
            return True

        self.assertTrue(self.remove(finished))
        self.assertFalse(self.run.exists())
        self.assertTrue(sentinel.is_file())

    def test_top_level_symlink_is_rejected(self):
        other = self.run.rename(self.parent / "other")
        self.run.symlink_to(other, target_is_directory=True)
        with self.assertRaises(OSError):
            self.remove(lambda *_: self.fail("followed a symlink"))
        self.assertTrue(other.is_dir())

    def test_unowned_or_non_private_directory_is_preserved(self):
        with patch("os.geteuid", return_value=os.geteuid() + 1):
            with self.assertRaisesRegex(ValueError, "owner or permissions"):
                self.remove(lambda *_: self.fail("accepted another owner's directory"))
        self.run.chmod(0o755)
        with self.assertRaisesRegex(ValueError, "owner or permissions"):
            self.remove(lambda *_: self.fail("accepted a non-private directory"))
        self.assertTrue(self.run.is_dir())

    def test_missing_or_foreign_marker_preserves_directory(self):
        self.marker.unlink()
        with self.assertRaises(FileNotFoundError):
            self.remove(lambda *_: self.fail("accepted an unmarked directory"))
        self.marker.write_text(f"{NAME}.scope\n/a/different/cgroup\n")
        self.marker.chmod(0o600)
        with self.assertRaisesRegex(ValueError, "invalid runner lease marker"):
            self.remove(lambda *_: self.fail("accepted another cgroup tree"))
        self.assertTrue(self.run.is_dir())

    def test_directory_replacement_is_rejected(self):
        def replace(*_):
            self.run.rename(self.parent / "old")
            self.run.mkdir(mode=0o700)
            return True

        with self.assertRaisesRegex(ValueError, "directory changed"):
            self.remove(replace)
        self.assertTrue(self.run.is_dir())


class IoWeightTests(unittest.TestCase):
    def check_weight(self, mode, controllers=None, weight=None, *, slice_controllers="cpu memory"):
        with tempfile.TemporaryDirectory() as tmp:
            parent = Path(tmp)
            (parent / "cgroup.controllers").write_text(slice_controllers + "\n")
            cgroup = parent / "test.scope"
            cgroup.mkdir()
            if controllers is not None:
                (cgroup / "cgroup.controllers").write_text(controllers + "\n")
            if weight is not None:
                (cgroup / "io.weight").write_text(f"default {weight}\n")
            return subprocess.run(
                ["bash", "-c", 'source "$1"; verify_io_weight "$2" "$3"',
                 "runner-regression", str(RUNNER), str(cgroup), mode],
                text=True, capture_output=True, check=False,
            )

    def test_auto_applies_20_or_warns_only_if_controller_is_missing(self):
        delegated = self.check_weight("auto", "cpu io memory", 20)
        self.assertEqual(delegated.returncode, 0, delegated.stderr)
        self.assertIn("io.weight=default 20", delegated.stderr)
        unsupported = self.check_weight("auto", "cpu memory")
        self.assertEqual(unsupported.returncode, 0, unsupported.stderr)
        self.assertIn("controller unavailable", unsupported.stderr)
        inactive = self.check_weight("auto", "cpu memory", slice_controllers="cpu io memory")
        self.assertEqual(inactive.returncode, 3)
        self.assertIn("inactive in the scope", inactive.stderr)
        for controllers, weight in [("cpu io memory", None), ("cpu io memory", 100), (None, None)]:
            with self.subTest(controllers=controllers, weight=weight):
                self.assertEqual(self.check_weight("auto", controllers, weight).returncode, 3)

    def test_off_skips_readback_and_numeric_requires_exact_weight(self):
        self.assertEqual(self.check_weight("off").returncode, 0)
        self.assertEqual(self.check_weight("42", "cpu io memory", 42).returncode, 0)
        self.assertEqual(self.check_weight("42", "cpu io memory", 20).returncode, 3)
        self.assertEqual(self.check_weight("42", "cpu memory").returncode, 3)

    def test_missing_executable_shows_install_hint(self):
        with tempfile.TemporaryDirectory() as tmp:
            for tool in ("dirname", "systemctl", "systemd-run"):
                (Path(tmp) / tool).symlink_to(Path("/usr/bin") / tool)
            result = subprocess.run(["/bin/bash", str(RUNNER)], env=dict(os.environ, PATH=tmp),
                                    text=True, capture_output=True, check=False)
        self.assertEqual(result.returncode, 3)
        self.assertIn("missing executable: Xvfb", result.stderr)
        self.assertIn("apt-get install --no-install-recommends xvfb", result.stderr)


class ScopeStateTests(unittest.TestCase):
    def test_requires_inactive_scope_and_absent_cgroup(self):
        group = Mock()
        group.relative_to.return_value = Path("test-user/app.slice") / f"{NAME}.scope"
        scope = CLEANUP["scope_finished"]
        expected = "/" + str(group.relative_to.return_value)
        with patch("subprocess.run") as command:
            command.return_value.stdout = (
                f"LoadState=loaded\nActiveState=active\nControlGroup={expected}\n"
            )
            self.assertFalse(scope(f"{NAME}.scope", group))
            group.lstat.assert_not_called()
            command.return_value.stdout = f"LoadState=loaded\nActiveState=failed\nControlGroup={expected}\n"
            self.assertFalse(scope(f"{NAME}.scope", group))
            group.lstat.side_effect = FileNotFoundError
            self.assertTrue(scope(f"{NAME}.scope", group))
            command.return_value.stdout = "LoadState=not-found\nActiveState=inactive\nControlGroup=\n"
            self.assertTrue(scope(f"{NAME}.scope", group))


class HostScopeTests(unittest.TestCase):
    def invoke(self, *args, **overrides):
        env = dict(os.environ)
        env.pop("SQARVO_UI_IO_WEIGHT", None)
        env.update(overrides)
        return subprocess.run([RUNNER, "--gtk-tests", "/usr/bin/true", *args], cwd=ROOT,
                              env=env, text=True, capture_output=True, timeout=30)

    def test_real_io_modes(self):
        auto = self.invoke()
        self.assertEqual(auto.returncode, 0, auto.stderr + auto.stdout)
        off = self.invoke(SQARVO_UI_IO_WEIGHT="off")
        self.assertEqual(off.returncode, 0, off.stderr)
        self.assertIn("IOWeight=off", off.stderr)
        self.assertNotIn("controller unavailable", off.stdout + off.stderr)
        strict = self.invoke(SQARVO_UI_IO_WEIGHT="37")
        if "controller unavailable" in auto.stdout:
            self.assertEqual(strict.returncode, 3)
            self.assertIn("unavailable for explicit IOWeight=37", strict.stderr)
        else:
            self.assertEqual(strict.returncode, 0, strict.stderr)

    def test_sigkill_run_is_preserved_until_scope_ends(self):
        env = dict(os.environ, SQARVO_UI_TIMEOUT="14")
        env.pop("SQARVO_UI_IO_WEIGHT", None)
        with ExitStack() as stack:
            out = stack.enter_context(tempfile.TemporaryFile(mode="w+t"))
            err = stack.enter_context(tempfile.TemporaryFile(mode="w+t"))
            process = subprocess.Popen(
                [RUNNER, "--gtk-tests", "/usr/bin/python3", "-c",
                 'import os,time; print("READY", os.environ["SQARVO_UI_RUN_DIR"], '
                 'os.environ["SQARVO_UI_CGROUP"], flush=True); time.sleep(60)'],
                env=env, cwd=ROOT, stdout=out, stderr=err, start_new_session=True,
            )
            stack.callback(process.wait, timeout=20)
            deadline = time.monotonic() + 12
            ready = None
            while time.monotonic() < deadline:
                out.seek(0)
                ready = next((line for line in out if line.startswith("READY ")), None)
                if ready or process.poll() is not None:
                    break
                time.sleep(0.05)
            err.seek(0)
            self.assertIsNotNone(ready, err.read())
            _, run_name, group_name = ready.split()
            run = Path(run_name)
            group = Path(group_name)
            process.send_signal(signal.SIGKILL)
            self.assertEqual(process.wait(timeout=5), -signal.SIGKILL)
            concurrent = self.invoke()
            self.assertEqual(concurrent.returncode, 0, concurrent.stderr)
            self.assertTrue(group.is_dir(), "probe scope ended before the concurrent check")
            self.assertTrue(run.is_dir(), "removed a running instance after its caller died")

            deadline = time.monotonic() + 25
            while group.exists() and time.monotonic() < deadline:
                time.sleep(0.2)
            self.assertFalse(group.exists(), "probe scope failed to stop")
            subsequent = self.invoke()
            self.assertEqual(subsequent.returncode, 0, subsequent.stderr)
            self.assertIn(f"Removed stale UI run: {run}", subsequent.stderr)
            self.assertFalse(run.exists(), "stale directory survived scope exit")


if __name__ == "__main__":
    host = "--host" in sys.argv
    if host:
        sys.argv.remove("--host")
    cases = [RunCleanupTests, IoWeightTests, ScopeStateTests]
    if host:
        cases.append(HostScopeTests)
    suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(case) for case in cases)
    sys.exit(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
