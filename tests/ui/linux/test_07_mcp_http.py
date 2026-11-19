"""GTK → management gRPC → real local MCP → SQLite, with an isolated keyring."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import http.client
import json
import os
from queue import Empty, SimpleQueue
from pathlib import Path
import shutil
import socket
import subprocess
from threading import Thread

import pytest

import adapter as A
from conftest import UiInstance
import ops
from ops import expect, step


def read_token():
    # The suite itself runs on the private session bus. Read directly into memory.
    import gi

    gi.require_version("Secret", "1")
    from gi.repository import Secret

    token = Secret.password_lookup_sync(None, {
        "application": "io.sqarvo.Sqarvo",
        "profile": "mcp-http",
        "reference": "sqarvo:conn/c13ba28f-5b41-4d45-b28b-fb7397db84d4",
    }, None)
    expect(bool(token), "隔离密钥环中存在 MCP HTTP 凭据")
    return token


class HttpClient:
    def __init__(self, port, token):
        self.port = port
        self.token = token
        self.session = None
        self.sequence = 0

    def exchange(self, method, body=None, *, headers=None, path="/mcp"):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        values = {
            "Accept": "application/json, text/event-stream",
            "Content-Type": "application/json",
        }
        if self.token:
            values["Authorization"] = f"Bearer {self.token}"
        if self.session:
            values.update({"Mcp-Session-Id": self.session, "MCP-Protocol-Version": "2025-11-25"})
        values.update(headers or {})
        try:
            connection.request(method, path, json.dumps(body) if body else None, values)
            response = connection.getresponse()
            data = None
            if method == "POST" and response.status == 200:
                if response.headers.get_content_type() == "text/event-stream":
                    for line in response:
                        if line.startswith(b"data:") and line[5:].strip():
                            data = json.loads(line[5:])
                            break
                else:
                    data = json.load(response)
            return response.status, response.headers, data
        finally:
            connection.close()

    def initialize(self):
        status, headers, body = self.exchange("POST", {
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                       "clientInfo": {"name": "sqarvo-http-test", "version": "1"}},
        })
        assert status == 200 and body["result"]["protocolVersion"] == "2025-11-25"
        self.session = headers["Mcp-Session-Id"]
        assert self.session
        assert self.exchange("POST", {"jsonrpc": "2.0", "method": "notifications/initialized"})[0] == 202

    def request(self, method, params=None):
        self.sequence += 1
        return self.exchange("POST", {
            "jsonrpc": "2.0", "id": self.sequence, "method": method, "params": params or {},
        })


def codex_mcp(binary, port, env, tool_call=None):
    # Exercise the actual credential launcher and Codex MCP client without a
    # model request. The client configuration and ephemeral thread are isolated.
    codex_home = Path(env["XDG_CONFIG_HOME"]) / "codex"
    codex_home.mkdir(exist_ok=True)
    env = dict(env, CODEX_HOME=str(codex_home))
    with subprocess.Popen(
        [binary, "http-auth", "launch", "codex", "--app-server", "--port", str(port)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, env=env, cwd=env["XDG_DATA_HOME"],
    ) as process:
        messages = SimpleQueue()

        def read_messages():
            for line in process.stdout:
                messages.put(json.loads(line))
            messages.put(None)

        reader = Thread(target=read_messages, daemon=True)
        reader.start()
        next_id = 0

        def invoke(method, params=None):
            nonlocal next_id
            request = {"method": method}
            if params is not None:
                next_id += 1
                request.update(id=next_id, params=params)
            process.stdin.write(json.dumps(request) + "\n")
            process.stdin.flush()
            if params is None:
                return None
            while True:
                try:
                    reply = messages.get(timeout=15)
                except Empty:
                    pytest.fail(f"Codex app-server did not answer {method}")
                assert reply is not None, "Codex app-server stopped"
                if reply.get("id") == next_id:
                    assert "result" in reply, reply
                    return reply["result"]

        invoke("initialize", {"clientInfo": {"name": "sqarvo-ui-test", "version": "1"}})
        invoke("initialized")
        servers = invoke("mcpServerStatus/list", {})["data"]
        entry = next(item for item in servers if item["name"] == "sqarvo")
        assert entry["authStatus"] == "bearerToken"
        assert entry["toolsError"] is None
        result = None
        if tool_call is not None:
            thread = invoke("thread/start", {"ephemeral": True, "sandbox": "read-only"})["thread"]
            result = invoke("mcpServer/tool/call", {
                "threadId": thread["id"],
                "server": "sqarvo",
                "tool": tool_call["name"],
                "arguments": tool_call["arguments"],
            })
        process.stdin.close()
        process.wait(timeout=12)
        reader.join(timeout=2)
    return set(entry["tools"]), result


@pytest.mark.smoke
def test_managed_mcp_http():
    """注册表组件: shell.menu.mcp, workspace.mcp_management.service/agent。"""
    if os.environ.get("SQARVO_UI_PRIVATE_MCP_KEYRING") != "1":
        pytest.skip("requires an isolated, unlocked Secret Service test session")
    mcp = os.environ.get("SQARVO_MCP_BIN")
    if not mcp or not shutil.which("codex"):
        pytest.skip("requires the MCP companion and Codex CLI")
    inst = UiInstance("mcp_http")
    try:
        inst.start(autoconnect=True)
        frame = inst.frame()
        ops.wait_connected(frame, str(inst.db), timeout=30)
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]
        config = inst.home / "config" / "sqarvo" / "mcp-http-port"

        def open_panel():
            A.window_action(frame, "win.mcp-management")
            return A.wait_dialog(inst.app, "MCP 管理")

        def press(container, name):
            A.click(A.find_one(container, role_in=("button",), name=name, timeout=10))

        with step("start_managed_http"):
            panel = open_panel()
            A.find_one(panel, role_in=("label",), name="HTTP 服务已停止", timeout=10)
            save = A.find_one(panel, role_in=("button",), name="保存端口", timeout=10)
            copy_url = A.find_one(panel, role_in=("button",), name="复制 MCP 地址", timeout=10)
            expect("sensitive" not in A.states(save), "未修改端口时禁止保存")
            expect("sensitive" not in A.states(copy_url), "停止时禁止复制实际监听地址")
            entry = A.find_one(panel, role_in=("text", "entry"), name="HTTP 端口", timeout=10)
            A.set_text(entry, str(port))
            press(panel, "保存端口")
            A.find_one(panel, role_in=("label",), name=f"http://127.0.0.1:{port}/mcp", timeout=10)
            expect(config.read_text(encoding="ascii").strip() == str(port), "后端保存实际端口")
            press(panel, "启动 HTTP 服务")
            A.find_one(panel, role_in=("label",), name="HTTP 服务正在运行", timeout=15)

        with step("authenticate_and_discover"):
            token = read_token()
            client = HttpClient(port, token)
            anonymous = HttpClient(port, None)
            invalid = HttpClient(port, "0" * 64)
            expect(anonymous.request("tools/list")[0] == 401, "缺失令牌返回 401")
            expect(invalid.request("tools/list")[0] == 401, "错误令牌返回 401")
            expect(client.exchange("POST", headers={"Host": "invalid.example"})[0] == 403,
                   "拒绝不匹配的 Host")
            expect(client.exchange("POST", headers={"Origin": "https://invalid.example"})[0] == 403,
                   "拒绝其他 Origin")
            other = HttpClient(port, token)
            with ThreadPoolExecutor(max_workers=2) as pool:
                handshakes = [pool.submit(peer.initialize) for peer in (client, other)]
                for handshake in handshakes:
                    handshake.result(timeout=10)
            expect(client.session != other.session, "并发客户端使用独立会话")
            expect(other.exchange("DELETE")[0] == 202, "客户端可以关闭自己的会话")
            expect(other.request("tools/list")[0] == 404, "已关闭的会话不可复用")
            expect(client.exchange("GET")[0] == 200, "GET SSE 建立并断开")
            tools = client.request("tools/list")[2]["result"]["tools"]
            expect(len(tools) == 8, "八个现有工具")
            expect(codex_mcp(mcp, port, inst.env)[0] == {tool["name"] for tool in tools},
                   "真实 Codex 经本机凭据启动器读取同一工具清单")
            resources = client.request("resources/list")[2]["result"]["resources"]
            templates = client.request("resources/templates/list")[2]["result"]["resourceTemplates"]
            expect(len(resources) + len(templates) == 4, "四类现有资源")

        with step("copy_agent_configuration"):
            for name, confirmation in [
                ("复制 MCP 地址", "已复制 MCP 地址"),
                ("复制提示词（http方式）", "已复制 Agent 配置提示词"),
                ("复制提示词（stdio方式）", "已复制 stdio 配置提示词"),
            ]:
                press(panel, name)
                A.find_one(panel, role_in=("label",), name=confirmation, timeout=10)

        with step("live_database_authorization"):
            def policy_toggle():
                return next(
                    (node for node in A.query(panel, role_in=("toggle button", "switch"), name="MCP 数据访问") if A.action_names(node)),
                    None,
                )

            A.click(A.wait_until(policy_toggle, 10, "管理页的 MCP 数据访问开关"))
            press(A.wait_dialog(inst.app, "启用 MCP 数据访问？"), "启用 MCP 数据访问")
            A.find_one(panel, role_in=("label",), name="MCP：仅允许读取", timeout=10)
            readonly = A.find_one(panel, role_in=("toggle button", "switch"), name="MCP 只读", timeout=10)
            A.click(readonly)
            press(A.wait_dialog(inst.app, "关闭 MCP 只读模式？"), "取消")
            A.find_one(panel, role_in=("label",), name="MCP：仅允许读取", timeout=10)
            request = {"name": "execute_readonly_query",
                       "arguments": {"connection_alias": "uitestdb", "sql": "SELECT 42 AS value"}}
            listed = client.request("tools/call", {"name": "list_connections", "arguments": {}})[2]
            request["arguments"]["connection_alias"] = listed["result"]["structuredContent"]["connections"][0]["alias"]
            result = client.request("tools/call", request)[2]["result"]
            expect(not result.get("isError") and "42" in json.dumps(result), "通过 MCP 查询 SQLite")
            _, codex_result = codex_mcp(mcp, port, inst.env, request)
            expect(not codex_result.get("isError") and "42" in json.dumps(codex_result),
                   "真实 Codex 客户端通过 HTTP 调用已授权的 SQLite 工具")
            resource = client.request("resources/read", {"uri": "database://connections"})[2]
            expect("result" in resource and resource["result"]["contents"], "通过 HTTP 读取共享连接资源")
            request["arguments"]["sql"] = "DELETE FROM widgets"
            result = client.request("tools/call", request)[2]["result"]
            expect(result["structuredContent"]["error"]["code"] == "mcp.readonly_violation", "HTTP 延用只读策略")
            request["arguments"]["sql"] = "SELECT 42"
            A.click(A.wait_until(policy_toggle, 10, "管理页的 MCP 数据访问开关"))
            A.find_one(panel, role_in=("label",), name="MCP：访问关闭", timeout=10)
            result = client.request("tools/call", request)[2]["result"]
            expect(result["structuredContent"]["error"]["code"] == "mcp.connection_denied", "无需重启即撤销访问")

        with step("rotate_revoke_and_stop"):
            press(panel, "访问令牌管理")
            press(A.wait_dialog(inst.app, "访问令牌管理"), "重新生成访问令牌")
            press(A.wait_dialog(inst.app, "重新生成访问令牌？"), "取消")
            expect(client.request("tools/list")[0] == 200, "取消后原令牌仍有效")
            press(panel, "访问令牌管理")
            press(A.wait_dialog(inst.app, "访问令牌管理"), "重新生成访问令牌")
            press(A.wait_dialog(inst.app, "重新生成访问令牌？"), "重新生成访问令牌")
            A.find_one(panel, role_in=("label",), name="认证凭据已更新。请重新启动已配置的客户端。", timeout=10)
            expect(client.request("tools/list")[0] == 401, "旧令牌立即被拒绝")
            replacement = HttpClient(port, read_token())
            replacement.initialize()
            press(panel, "访问令牌管理")
            press(A.wait_dialog(inst.app, "访问令牌管理"), "撤销访问令牌")
            press(A.wait_dialog(inst.app, "撤销访问令牌？"), "撤销访问令牌")
            A.wait_until(lambda: replacement.request("tools/list")[0] == 401, 10, "撤销后拒绝认证")
            stop = A.find_one(panel, role_in=("button",), name="停止 HTTP 服务", timeout=10)
            A.wait_enabled(stop, 10)
            A.click(stop)
            A.find_one(panel, role_in=("label",), name="HTTP 服务已停止", timeout=10)
            with socket.socket() as released:
                released.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                released.bind(("127.0.0.1", port))

        with step("shutdown_owned_process_only"):
            press(panel, "启动 HTTP 服务")
            A.find_one(panel, role_in=("label",), name="HTTP 服务正在运行", timeout=15)
            client = HttpClient(port, read_token())
            client.initialize()
            with socket.socket() as reserve:
                reserve.bind(("127.0.0.1", 0))
                manual_port = reserve.getsockname()[1]
            with subprocess.Popen(
                [mcp, "serve", "--port", str(manual_port)], env=inst.env,
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            ) as manual, closing(http.client.HTTPConnection("127.0.0.1", port, timeout=10)) as active:
                try:
                    def manual_ready():
                        try:
                            return HttpClient(manual_port, None).request("tools/list")[0] == 401
                        except ConnectionRefusedError:
                            return False

                    A.wait_until(manual_ready, 10, "独立 CLI 服务在运行")
                    active.request("GET", "/mcp", headers={
                        "Accept": "text/event-stream", "Mcp-Session-Id": client.session,
                        "MCP-Protocol-Version": "2025-11-25", "Authorization": f"Bearer {client.token}",
                    })
                    stream = active.getresponse()
                    expect(stream.status == 200, "退出前保持一个活动 SSE 流")
                    press(panel, "关闭")
                    A.wait_gone(lambda: A.find_dialog(inst.app, "MCP 管理"), 10, "管理页关闭")
                    inst.proc.terminate()
                    expect(inst.proc.wait(timeout=20) == 0, "GTK 正常退出")
                    # A blocked shutdown would leave the SSE stream open until the socket timeout.
                    stream.read()
                    def managed_port_released():
                        with socket.socket() as probe:
                            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                            try:
                                probe.bind(("127.0.0.1", port))
                            except OSError:
                                return False
                        return True

                    A.wait_until(managed_port_released, 8, "GUI 退出后托管端口释放")
                    expect(manual_ready(), "独立 CLI 服务不随 GUI 退出")
                finally:
                    manual.terminate()
                    manual.wait(timeout=8)
    finally:
        inst.dispose()
