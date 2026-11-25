#!/usr/bin/python3
"""Remove finished Linux UI runs after checking their lease and systemd scope."""

import fcntl
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
from contextlib import contextmanager


RUN_NAME = re.compile(r"sqarvo-ui\.[A-Za-z0-9]{8}")
LEASE = ".sqarvo-ui-lease"
DIRECTORY_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW


def scope_finished(scope, cgroup):
    result = subprocess.run(
        ["systemctl", "--user", "show", "--property=LoadState,ActiveState,ControlGroup",
         "--", scope],
        capture_output=True, text=True, timeout=5, check=True,
    )
    properties = dict(line.split("=", 1) for line in result.stdout.splitlines())
    expected = "/" + str(cgroup.relative_to("/sys/fs/cgroup"))
    if (
        properties.get("LoadState") not in {"loaded", "not-found"}
        or properties.get("ActiveState") not in {"inactive", "failed"}
        or properties.get("ControlGroup") not in {"", expected}
    ):
        return False
    try:
        # An absent cgroup cannot contain processes, even if its former PID was reused.
        cgroup.lstat()
    except FileNotFoundError:
        return True
    return False


def remove_run(parent, name, cgroup_parent, inspect_scope=scope_finished, *, lease_fd=None):
    scope = name + ".scope"
    fd = os.open(name, DIRECTORY_FLAGS, dir_fd=parent) if lease_fd is None else os.dup(lease_fd)
    with closing_fd(fd) as directory:
        identity = os.fstat(directory)
        if identity.st_uid != os.geteuid() or stat.S_IMODE(identity.st_mode) != 0o700:
            raise ValueError("unexpected directory owner or permissions")
        try:
            fcntl.flock(directory, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False

        with closing_fd(os.open(
            LEASE, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory,
        )) as marker:
            metadata = os.fstat(marker)
            if (
                not stat.S_ISREG(metadata.st_mode)
                or metadata.st_uid != os.geteuid()
                or metadata.st_nlink != 1
                or stat.S_IMODE(metadata.st_mode) != 0o600
                or os.read(marker, 4096) != f"{scope}\n{cgroup_parent}\n".encode()
            ):
                raise ValueError("invalid runner lease marker")

        if not inspect_scope(scope, cgroup_parent / scope):
            raise ValueError("scope may still have processes")
        current = os.stat(name, dir_fd=parent, follow_symlinks=False)
        if not os.path.samestat(identity, current):
            raise ValueError("directory changed while inspecting it")
        shutil.rmtree(name, dir_fd=parent)
        return True


@contextmanager
def closing_fd(fd):
    try:
        yield fd
    finally:
        os.close(fd)


def main(cgroup_parent, target=None, lease_fd=None):
    if not shutil.rmtree.avoids_symlink_attacks:
        print("Warning: retaining UI run directories; safe rmtree unavailable", file=sys.stderr)
        return 1
    with closing_fd(os.open("/tmp", DIRECTORY_FLAGS)) as parent:
        names = [target] if target else os.listdir(parent)
        for name in names:
            if not RUN_NAME.fullmatch(name):
                if target:
                    return 1
                continue
            try:
                removed = remove_run(parent, name, Path(cgroup_parent),
                                     lease_fd=int(lease_fd) if lease_fd is not None else None)
            except FileNotFoundError as error:
                # Separate concurrent removal from a directory with no reliable marker.
                try:
                    os.stat(name, dir_fd=parent, follow_symlinks=False)
                except FileNotFoundError:
                    continue
                print(f"Warning: retaining /tmp/{name}: {error}", file=sys.stderr)
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                print(f"Warning: retaining /tmp/{name}: {error}", file=sys.stderr)
            else:
                if removed:
                    if not target:
                        print(f"Removed stale UI run: /tmp/{name}", file=sys.stderr)
                    continue
                if target:
                    print(f"Warning: retaining /tmp/{name}: runner lease still held", file=sys.stderr)
            if target:
                return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
