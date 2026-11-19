#!/usr/bin/env python3
"""Linux E2E for GTK -> backend gRPC -> live MCP authorization."""

import argparse
import json
import os
from contextlib import closing
from pathlib import Path
import select
import signal
import sqlite3
import subprocess
import tempfile
import time
import uuid


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def seed_profiles(root, config_root):
    database = root / "gtk.sqlite"
    with closing(sqlite3.connect(database)) as connection, connection:
        connection.execute("CREATE TABLE access_probe (value INTEGER)")
        connection.execute("INSERT INTO access_probe VALUES (42)")
    desktop_file = config_root / "Sqarvo" / "connections.json"
    desktop_file.parent.mkdir(parents=True)
    desktop_file.write_text(
        json.dumps({
            "version": 1,
            "connections": [{
                "name": "gtk-qa",
                "alias": "gtk-qa",
                "driver": "sqlite",
                "host": "",
                "port": 0,
                "database": "",
                "username": "",
                "credential_ref": "",
                "file_path": str(database),
                "read_only": True,
                "tls_mode": "disabled",
            }],
        }),
        encoding="utf-8",
    )
    (config_root / "mcp.json").write_text(
        json.dumps({
            "connections": [{
                "alias": "legacy-mcp-qa",
                "driver_id": "sqlite",
                "host": str(root / "legacy.sqlite"),
                "properties": {"read_only": "true"},
                "enabled": True,
            }],
        }),
        encoding="utf-8",
    )
    return database


class McpClient:
    def __init__(self, child):
        self.child = child
        self.remaining = bytearray()
        self.next_id = 0

    def send(self, method, params, *, notify=False):
        data = {"jsonrpc": "2.0", "method": method, "params": params}
        if not notify:
            self.next_id += 1
            data["id"] = self.next_id
        self.child.stdin.write((json.dumps(data) + "\n").encode())
        self.child.stdin.flush()

    def request(self, method, params):
        self.send(method, params)
        deadline = time.monotonic() + 15
        while b"\n" not in self.remaining:
            timeout = max(0, deadline - time.monotonic())
            ready, _, _ = select.select([self.child.stdout], [], [], timeout)
            check(ready, f"Timed out waiting for MCP {method}")
            chunk = os.read(self.child.stdout.fileno(), 65536)
            check(chunk, f"MCP exited during {method}: {self.child.poll()}")
            self.remaining.extend(chunk)
        line, _, self.remaining = self.remaining.partition(b"\n")
        result = json.loads(line)
        check(result.get("jsonrpc") == "2.0" and result.get("id") == self.next_id, result)
        return result

    def call(self, name, arguments):
        reply = self.request("tools/call", {"name": name, "arguments": arguments})
        check("error" not in reply and "result" in reply, reply)
        return reply["result"]


def run_gtk(desktop, environment, action, indicator):
    command = [
        "dbus-run-session", "--", desktop, "--qa-exit-after-ms", "6000",
        "--qa-mcp-access", action, "--qa-mcp-indicator", indicator,
    ]
    with subprocess.Popen(
        command,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
        text=True,
    ) as child:
        try:
            output, _ = child.communicate(timeout=22)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            output, _ = child.communicate()
            raise AssertionError(f"GTK {action} timed out\n{output}") from None
        check(child.returncode == 0, output)

    observations = [
        "MCP 菜单动作可用",
        "MCP 动作方向",
        "MCP 策略 gRPC 回读与修订",
        "MCP 状态指示文本与可访问名称",
    ]
    if action == "on":
        observations.append("MCP 确认框标题与当前档案数")
    for observation in observations:
        marker = f"[qa] 断言通过：{observation}"
        check(marker in output, f"Missing {marker}\n{output}")
    check("[qa] 断言失败" not in output, output)
    print(f"GTK {action}: " + "; ".join(observations))


def assert_denied(result):
    check(result.get("isError") is True, result)
    check(result["structuredContent"]["error"]["code"] == "mcp.connection_denied", result)


def open_database(pid, database):
    for descriptor in Path(f"/proc/{pid}/fd").iterdir():
        try:
            target = descriptor.readlink()
        except FileNotFoundError:
            continue
        if target == database:
            return True
    return False


def exercise(args, root):
    fake_home = root / "home"
    fake_home.mkdir()
    environment = os.environ.copy()
    environment.pop("SQARVO_ENDPOINT_FILE", None)
    for key, name in [
        ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
        ("XDG_RUNTIME_DIR", "run"), ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ]:
        directory = fake_home / name
        directory.mkdir(mode=0o700)
        environment[key] = str(directory)
    environment["HOME"] = str(fake_home)
    environment["SQARVO_BACKEND_BIN"] = str(args.backend)
    environment["SQARVO_QA_APP_ID"] = f"io.sqarvo.Sqarvo.M10QA{uuid.uuid4().hex}"
    environment["GDK_BACKEND"] = "x11"

    config = Path(environment["XDG_CONFIG_HOME"]) / "sqarvo"
    database = seed_profiles(root, config)
    with tempfile.TemporaryFile(mode="w+b") as diagnostics:
        child = subprocess.Popen(
            [args.mcp], env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=diagnostics,
        )
        try:
            client = McpClient(child)
            initialized = client.request("initialize", {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "sqarvo-gtk-mcp-e2e", "version": "1"},
            })
            check(initialized["result"]["protocolVersion"] == "2025-11-25", initialized)
            client.send("notifications/initialized", {}, notify=True)

            assert_denied(client.call("list_connections", {}))
            check((config / "Sqarvo" / "connections.json.bak").is_file(), "missing GTK backup")
            check((config / "mcp.json.bak").is_file(), "missing MCP backup")
            check(json.loads((config / "mcp.json").read_text(encoding="utf-8")) == {},
                  "MCP legacy profiles survived in service config")
            profile_path = config / "connections.json"
            migrated = json.loads(profile_path.read_text(encoding="utf-8"))
            default_policy = {"enabled": False, "read_only": True}
            check(migrated["mcp"] == default_policy, migrated["mcp"])
            check(len(migrated["profiles"]) == 2 and len(migrated["migration_state"]) == 2,
                  "expected exactly one import from each source")
            print("GTK and MCP legacy files migrated; legacy enabled=true grants no access")

            run_gtk(args.desktop, environment, "on", "readonly")
            listing = client.call("list_connections", {})
            check(not listing.get("isError"), listing)
            aliases = {item["alias"] for item in listing["structuredContent"]["connections"]}
            check(aliases == {"gtk-qa", "legacy-mcp-qa"}, aliases)
            query = {
                "connection_alias": "gtk-qa", "database": "", "sql": "SELECT value FROM access_probe",
            }
            result = client.call("execute_readonly_query", query)
            check(not result.get("isError") and result["structuredContent"]["rows"] == [[42]], result)
            check(open_database(child.pid, database), "MCP did not hold a SQLite file descriptor")
            print(f"MCP PID {child.pid} sees both profiles and queried [[42]]; SQLite FD open")

            run_gtk(args.desktop, environment, "off", "disabled")
            assert_denied(client.call("execute_readonly_query", query))
            check(child.poll() is None, "MCP exited on revocation")
            check(not open_database(child.pid, database), "MCP kept the SQLite FD after revocation")
            assert_denied(client.call("list_connections", {}))
            resource = client.request("resources/read", {"uri": "database://connections"})
            check(resource["error"]["code"] == -32002, resource)
            check(resource["error"]["data"]["error"]["code"] == "mcp.connection_denied", resource)
            print(f"Same MCP PID {child.pid} denied query, discovery and resource read; SQLite FD closed")

            revoked = json.loads(profile_path.read_text(encoding="utf-8"))
            check(revoked["mcp"] == default_policy, revoked["mcp"])
            check(revoked["revision"] == migrated["revision"] + 2, "expected exactly two policy writes")
            check(revoked["profiles"] == migrated["profiles"], "a GTK restart changed the migrated profiles")
            check(revoked["migration_state"] == migrated["migration_state"], "migration was not idempotent")
            print("Two GTK restarts preserved migrated profiles and source fingerprints")

            child.stdin.close()
            check(child.wait(timeout=10) == 0, "MCP did not shut down successfully at EOF")
        except Exception:
            diagnostics.seek(0)
            print(diagnostics.read().decode(errors="replace"))
            raise
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            child.stdin.close()
            child.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for binary in ("desktop", "backend", "mcp"):
        parser.add_argument(f"--{binary}", required=True, type=lambda path: Path(path).resolve())
    args = parser.parse_args()
    if not os.environ.get("DISPLAY"):
        parser.error("an X11 display is required; run with xvfb-run -a on a headless host")
    for path in vars(args).values():
        if not path.is_file() or not os.access(path, os.X_OK):
            parser.error(f"not an executable file: {path}")
    with tempfile.TemporaryDirectory(prefix="sqarvo-m10-", dir="/tmp") as temp:
        exercise(args, Path(temp))


if __name__ == "__main__":
    main()
