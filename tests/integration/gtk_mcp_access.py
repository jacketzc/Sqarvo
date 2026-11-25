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
import sys
import tempfile
import time
import uuid


def check(condition, message):
    if not condition:
        raise AssertionError(message)


DEFAULT_POLICY = {"enabled": False, "read_only": True}


def seed_database(database):
    with closing(sqlite3.connect(database)) as connection, connection:
        connection.execute("CREATE TABLE access_probe (value INTEGER)")
        connection.execute("INSERT INTO access_probe VALUES (42)")


def seed_profiles(database, config_root):
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
                "host": str(database.with_name("legacy.sqlite")),
                "properties": {"read_only": "true"},
                "enabled": True,
            }],
        }),
        encoding="utf-8",
    )

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


def run_gtk_qa(desktop, environment, *args):
    command = [desktop, "--qa-exit-after-ms", "6000", *args]
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
            raise AssertionError(f"GTK {args} timed out\n{output}") from None
        check(child.returncode == 0, output)
    check("[qa] 断言失败" not in output, output)
    return output


def run_gtk(desktop, environment, action, indicator):
    output = run_gtk_qa(desktop, environment, "--qa-mcp-access", action,
                        "--qa-mcp-indicator", indicator)
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


def snapshot(profile_path):
    return json.loads(profile_path.read_text(encoding="utf-8"))


def assert_read(client, query):
    result = client.call("execute_readonly_query", query)
    check(not result.get("isError") and result["structuredContent"]["rows"] == [[42]], result)


def exercise(args, root, *, migrated_profiles):
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
    profile_path = config / "connections.json"
    database = root / "gtk.sqlite"
    seed_database(database)
    if migrated_profiles:
        seed_profiles(database, config)
    else:
        qa_profile = root / "profile.json"
        qa_profile.write_text(json.dumps({
            "driver": "sqlite", "name": "gtk-qa", "path": str(database), "read_only": True,
        }), encoding="utf-8")
        run_gtk_qa(args.desktop, environment, "--qa-profile", str(qa_profile))
        saved, = snapshot(profile_path)["profiles"]
        check(saved["revision"] == 1 and saved["file_path"] == str(database), saved)
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
            initial = snapshot(profile_path)
            check(initial["mcp"] == DEFAULT_POLICY, initial["mcp"])
            if migrated_profiles:
                check((config / "Sqarvo" / "connections.json.bak").is_file(), "missing GTK backup")
                check((config / "mcp.json.bak").is_file(), "missing MCP backup")
                check(snapshot(config / "mcp.json") == {}, "legacy MCP profiles survived in service config")
                check(len(initial["profiles"]) == 2 and len(initial["migration_state"]) == 2,
                      "expected exactly one import from each source")
                print("GTK and MCP legacy files migrated; legacy enabled=true grants no access")

            run_gtk(args.desktop, environment, "on", "readonly")
            listing = client.call("list_connections", {})
            check(not listing.get("isError"), listing)
            aliases = {item["alias"] for item in listing["structuredContent"]["connections"]}
            if migrated_profiles:
                check(aliases == {"gtk-qa", "legacy-mcp-qa"}, aliases)
            else:
                check(len(aliases) == 1, aliases)
            query = {
                "connection_alias": "gtk-qa" if migrated_profiles else saved["profile_id"],
                "database": "", "sql": "SELECT value FROM access_probe",
            }
            assert_read(client, query)
            check(open_database(child.pid, database), "MCP did not hold a SQLite file descriptor")
            print(f"MCP PID {child.pid} sees {len(aliases)} profile(s) and queried [[42]]; SQLite FD open")

            expected_writes = 2
            if not migrated_profiles:
                run_gtk_qa(args.desktop, environment, "--qa-mcp-readonly", "off",
                           "--qa-mcp-indicator", "writable")
                check(snapshot(profile_path)["mcp"] == {"enabled": True, "read_only": False},
                      "GTK did not disable read-only")
                rejected = client.call("execute_readonly_query", {**query, "sql": "DELETE FROM access_probe"})
                check(rejected.get("isError") and rejected["structuredContent"]["error"]["code"]
                      == "mcp.readonly_violation", rejected)
                run_gtk_qa(args.desktop, environment, "--qa-mcp-readonly", "on",
                           "--qa-mcp-indicator", "readonly")
                check(snapshot(profile_path)["mcp"] == {"enabled": True, "read_only": True},
                      "GTK did not restore read-only")
                expected_writes += 2
                print("GTK read-only toggles preserved the V1 SQL policy")

            run_gtk(args.desktop, environment, "off", "disabled")
            assert_denied(client.call("execute_readonly_query", query))
            check(child.poll() is None, "MCP exited on revocation")
            check(not open_database(child.pid, database), "MCP kept the SQLite FD after revocation")
            assert_denied(client.call("list_connections", {}))
            resource = client.request("resources/read", {"uri": "database://connections"})
            check(resource["error"]["code"] == -32002, resource)
            check(resource["error"]["data"]["error"]["code"] == "mcp.connection_denied", resource)
            print(f"Same MCP PID {child.pid} denied query, discovery and resource read; SQLite FD closed")

            revoked = snapshot(profile_path)
            check(revoked["mcp"] == DEFAULT_POLICY, revoked["mcp"])
            check(revoked["revision"] == initial["revision"] + expected_writes, "unexpected policy writes")
            check(revoked["profiles"] == initial["profiles"], "a GTK restart changed the profiles")
            check(revoked["migration_state"] == initial["migration_state"], "migration was not idempotent")
            if not migrated_profiles:
                audit_path = Path(environment["XDG_DATA_HOME"]) / "sqarvo/mcp/audit.jsonl"
                records = [json.loads(line) for line in audit_path.read_text(encoding="utf-8").splitlines()]
                check(any(record["tool_name"] == "execute_readonly_query" and
                          record["error_subcode"] == "mcp.connection_denied" for record in records),
                      "missing query denial audit record")

                run_gtk(args.desktop, environment, "on", "readonly")
                assert_read(client, query)
                check(open_database(child.pid, database), "expected an existing SQLite connection")
                run_gtk_qa(args.desktop, environment, "--qa-delete-connection", saved["name"])
                check(snapshot(profile_path)["profiles"] == [], "GTK did not delete the saved profile")
                deleted = client.call("execute_readonly_query", query)
                check(deleted.get("isError") and deleted["structuredContent"]["error"]["code"]
                      == "mcp.connection_not_found", deleted)
                check(child.poll() is None and not open_database(child.pid, database),
                      "MCP exited or kept the deleted profile's SQLite FD")
                print(f"Same MCP PID {child.pid} denied the profile deleted through GTK")

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
    scope = os.environ.get("SQARVO_UI_SCOPE", "")
    run = os.environ.get("SQARVO_UI_RUN_DIR", "")
    if not (
        scope and run
        and Path("/proc/self/cgroup").read_text().strip().endswith(f"/{scope}")
        and os.environ.get("DBUS_SESSION_BUS_ADDRESS", "").startswith(f"unix:path={run}/session,")
        and os.environ.get("XAUTHORITY") == f"{run}/Xauthority"
        and os.environ.get("GDK_BACKEND") == "x11"
        and "WAYLAND_DISPLAY" not in os.environ
    ):
        print("UI TEST: BLOCKED\nError: run via scripts/run-ui-tests-isolated.sh --gtk-tests", file=sys.stderr)
        return 3
    for path in vars(args).values():
        if not path.is_file() or not os.access(path, os.X_OK):
            parser.error(f"not an executable file: {path}")
    for migrated in (True, False):
        with tempfile.TemporaryDirectory(prefix="sqarvo-m10-") as temp:
            exercise(args, Path(temp), migrated_profiles=migrated)


if __name__ == "__main__":
    sys.exit(main())
