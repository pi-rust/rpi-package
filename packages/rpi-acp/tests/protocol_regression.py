"""Protocol and lifecycle regression against a real rpi-acp-bridge process.

Build the bridge and the offline native peer first, then run:

    python packages/rpi-acp/tests/protocol_regression.py \
        --bridge target/debug/rpi-acp-bridge.exe --rpc target/debug/examples/fake_rpc.exe

The bridge is a real subprocess; only the native rpi --mode rpc child is a
controlled fixture, so malformed wire input and shutdown races exercise the
actual ACP transport.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time
import unittest


class Bridge:
    def __init__(self, executable, rpc, store, cwd, extra_env=None, read_output=True):
        env = os.environ.copy()
        env.update(
            RPI_BIN=str(rpc),
            RPI_ACP_SESSION_DIR=str(store),
            RPI_ACP_NO_AUTO_REGISTER_ZED="1",
            RPI_ACP_NO_PERMISSION_GATE="1",
        )
        if extra_env:
            env.update({key: str(value) for key, value in extra_env.items()})
        self.process = subprocess.Popen(
            [str(executable)],
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        self.output = queue.Queue()
        self.output_enabled = threading.Event()
        if read_output:
            self.output_enabled.set()
        self.errors = []
        threading.Thread(target=self._read_output, daemon=True).start()
        threading.Thread(target=lambda: self.errors.extend(self.process.stderr), daemon=True).start()
        self.next_id = 0

    def _read_output(self):
        while True:
            self.output_enabled.wait()
            line = self.process.stdout.readline()
            if not line:
                return
            try:
                self.output.put(json.loads(line))
            except json.JSONDecodeError as error:
                self.output.put({"__decode_error": str(error), "raw": line})

    def send(self, message):
        self.process.stdin.write(message if isinstance(message, str) else json.dumps(message))
        self.process.stdin.write("\n")
        self.process.stdin.flush()

    def request(self, method, params=None):
        self.next_id += 1
        request_id = self.next_id
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method,
                   "params": {} if params is None else params})
        messages = []
        deadline = time.monotonic() + 15
        while True:
            message = self.output.get(timeout=max(.01, deadline - time.monotonic()))
            if message.get("id") == request_id:
                return message, messages
            messages.append(message)

    def read_raw(self, timeout=15):
        self.process.stdout.flush()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            # The raw fixture mode deliberately has no stdout reader thread;
            # this synchronous read is only for the initialization handshake.
            line = self.process.stdout.readline()
            if line:
                return json.loads(line)
        raise AssertionError("timed out reading raw bridge output")

    def wait_response(self, request_id, timeout=15):
        messages = []
        deadline = time.monotonic() + timeout
        while True:
            message = self.output.get(timeout=max(.01, deadline - time.monotonic()))
            if message.get("id") == request_id:
                return message, messages
            messages.append(message)

    def wait_pid(self, path, count=1, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if path.exists():
                pids = [int(line) for line in path.read_text().splitlines() if line.strip()]
                if len(pids) >= count:
                    return pids
            time.sleep(.02)
        raise AssertionError(f"fixture did not start {count} children: {path}")

    @staticmethod
    def _kernel32():
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
        kernel32.OpenProcess.restype = ctypes.c_void_p
        kernel32.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        kernel32.WaitForSingleObject.restype = ctypes.c_uint32
        kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
        kernel32.CloseHandle.restype = ctypes.c_int
        return kernel32

    @staticmethod
    def wait_pid_exit(pid, timeout=5):
        if os.name == "nt":
            kernel32 = Bridge._kernel32()
            handle = kernel32.OpenProcess(0x00100000, False, pid)  # SYNCHRONIZE
            if not handle:
                raise AssertionError(f"could not open fixture child {pid}")
            try:
                result = kernel32.WaitForSingleObject(handle, int(timeout * 1000))
            finally:
                kernel32.CloseHandle(handle)
            if result == 0:  # WAIT_OBJECT_0
                return
            raise AssertionError(f"fixture child {pid} is still alive")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except OSError:
                return
            time.sleep(.02)
        raise AssertionError(f"fixture child {pid} is still alive")

    @staticmethod
    def open_pid_handle(pid):
        if os.name != "nt":
            # Keep a real liveness token on POSIX as well. Returning None made
            # wait_handle_exit silently skip the lifecycle assertion.
            return ("posix-pid", pid)
        kernel32 = Bridge._kernel32()
        handle = kernel32.OpenProcess(0x00100000, False, pid)
        if not handle:
            raise AssertionError(f"could not hold fixture child {pid}")
        return kernel32, handle

    @staticmethod
    def wait_handle_exit(handle, timeout=5):
        if handle is None:
            raise AssertionError("missing process liveness token")
        if os.name != "nt":
            kind, pid = handle
            if kind != "posix-pid":
                raise AssertionError(f"invalid POSIX process token: {handle!r}")
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    return
                except PermissionError:
                    pass
                time.sleep(.02)
            raise AssertionError(f"fixture child {pid} is still alive")
        kernel32, value = handle
        try:
            result = kernel32.WaitForSingleObject(value, int(timeout * 1000))
        finally:
            kernel32.CloseHandle(value)
        if result != 0:
            raise AssertionError("fixture child is still alive")

    def pause_output(self):
        self.output_enabled.clear()

    def resume_output(self):
        self.output_enabled.set()

    def assert_no_output(self, seconds=.25):
        try:
            message = self.output.get(timeout=seconds)
        except queue.Empty:
            return
        raise AssertionError(f"unexpected output: {message}")

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            self.process.wait(timeout=15)
        if self.process.returncode != 0:
            raise AssertionError("bridge failed: " + "".join(self.errors))


class ProtocolRegression(unittest.TestCase):
    def test_envelopes_cancel_close_and_resume(self):
        with tempfile.TemporaryDirectory(prefix="rpi-acp-wire-") as directory:
            root = Path(directory)
            bridge = Bridge(self.bridge, self.rpc, root / "sessions", root)
            try:
                bridge.send("not json")
                self.assertEqual(bridge.output.get(timeout=5)["error"]["code"], -32700)

                bridge.send([])
                self.assertEqual(bridge.output.get(timeout=5)["error"]["code"], -32600)

                result, _ = bridge.request("does/not-exist", {})
                self.assertEqual(result["error"]["code"], -32601)
                result, _ = bridge.request("session/new", {"cwd": 7})
                self.assertEqual(result["error"]["code"], -32602)

                # A valid notification, including an unknown method, never
                # creates a JSON-RPC response.
                bridge.send({"jsonrpc": "2.0", "method": "unknown/notification", "params": {}})
                bridge.assert_no_output()

                bridge.send({"jsonrpc": "2.0", "id": None, "method": "authenticate", "params": {}})
                null_response = bridge.output.get(timeout=5)
                self.assertIsNone(null_response.get("id"), null_response)
                self.assertEqual(null_response.get("error", {}).get("code"), -32602, null_response)

                result, _ = bridge.request("initialize", {
                    "protocolVersion": 1,
                    "clientCapabilities": {"elicitation": {"form": {}}},
                })
                self.assertEqual(result["result"]["protocolVersion"], 1)
                self.assertEqual(result["result"]["agentCapabilities"]["sessionCapabilities"]["list"], {})
                logout, _ = bridge.request("logout", {})
                self.assertEqual(logout.get("result"), {}, logout)
                newer, _ = bridge.request("initialize", {
                    "protocolVersion": 2,
                    "clientCapabilities": {"elicitation": {"form": {}}},
                })
                self.assertEqual(newer.get("error", {}).get("code"), -32602, newer)

                result, _ = bridge.request("session/new", {"cwd": str(root), "mcpServers": []})
                session_id = result["result"]["sessionId"]

                # The fixture opens an elicitation (agent id 1) and then a
                # permission request (agent id 2). An unknown client id must
                # not cancel the elicitation. Once permission id 2 is active,
                # finish a client request with that same id and prove that a
                # finished client id still cannot cancel it.
                bridge.send({"jsonrpc": "2.0", "id": 100, "method": "session/prompt",
                             "params": {"sessionId": session_id,
                                        "prompt": [{"type": "text", "text": "cancel me"}]}})
                question = bridge.output.get(timeout=5)
                self.assertIn(question.get("method"),
                              ("elicitation/create", "session/request_permission"), question)
                self.assertEqual(question["id"], 1)
                bridge.send({"jsonrpc": "2.0", "method": "$/cancel_request",
                             "params": {"requestId": 999}})
                if question["method"] == "elicitation/create":
                    answer = {"action": "accept", "content": {"answers": ["Rust"]}}
                else:
                    answer = {"outcome": {"outcome": "selected", "optionId": "0"}}
                bridge.send({"jsonrpc": "2.0", "id": 1, "result": answer})
                permission = None
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    message = bridge.output.get(timeout=.5)
                    if message.get("method") == "session/request_permission":
                        permission = message
                        break
                self.assertIsNotNone(permission)
                self.assertEqual(permission["id"], 2)
                bridge.send({"jsonrpc": "2.0", "id": 2, "method": "authenticate", "params": {}})
                self.assertEqual(bridge.output.get(timeout=5).get("id"), 2)
                bridge.send({"jsonrpc": "2.0", "method": "$/cancel_request",
                             "params": {"requestId": 2}})
                bridge.assert_no_output(.5)

                # The actual client request is still live and can be cancelled.
                bridge.send({"jsonrpc": "2.0", "method": "$/cancel_request",
                             "params": {"requestId": 100}})
                cancelled = []
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    message = bridge.output.get(timeout=.5)
                    if message.get("id") == 100:
                        cancelled.append(message)
                        break
                self.assertEqual(len(cancelled), 1, cancelled)
                self.assertEqual(cancelled[0]["result"]["stopReason"], "cancelled")

                # A second turn leaves a permission request pending. Close must
                # settle the original prompt exactly once before its close reply.
                bridge.send({"jsonrpc": "2.0", "id": 200, "method": "session/prompt",
                             "params": {"sessionId": session_id,
                                        "prompt": [{"type": "text", "text": "close me"}]}})
                question = bridge.output.get(timeout=5)
                self.assertIn(question.get("method"),
                              ("elicitation/create", "session/request_permission"), question)
                if question["method"] == "elicitation/create":
                    answer = {"action": "accept", "content": {"answers": ["Rust"]}}
                else:
                    answer = {"outcome": {"outcome": "selected", "optionId": "0"}}
                bridge.send({"jsonrpc": "2.0", "id": question["id"], "result": answer})
                permission = bridge.output.get(timeout=5)
                self.assertEqual(permission.get("method"), "session/request_permission", permission)
                close, messages = bridge.request("session/close", {"sessionId": session_id})
                prompt_responses = [message for message in messages if message.get("id") == 200]
                self.assertEqual(len(prompt_responses), 1, messages)
                self.assertEqual(prompt_responses[0].get("result", {}).get("stopReason"), "cancelled")
                self.assertEqual(close.get("result"), {}, close)

                # Close retains the native transcript and load reattaches it.
                loaded, _ = bridge.request("session/resume", {
                    "sessionId": session_id, "cwd": str(root), "mcpServers": []})
                self.assertIn("result", loaded, loaded)
                repeated_close, _ = bridge.request("session/close", {"sessionId": session_id})
                self.assertEqual(repeated_close.get("result"), {}, repeated_close)
                deleted, _ = bridge.request("session/delete", {"sessionId": session_id})
                self.assertEqual(deleted.get("result"), {}, deleted)
                repeated, _ = bridge.request("session/delete", {"sessionId": session_id})
                self.assertEqual(repeated.get("result"), {}, repeated)
            finally:
                bridge.close()

    def test_native_client_fs_and_terminal_requests_round_trip(self):
        for method, result in [
            ("fs/read_text_file", {"content": "from client"}),
            ("terminal/create", {"terminalId": "terminal-fixture"}),
        ]:
            with self.subTest(method=method), tempfile.TemporaryDirectory(prefix="rpi-acp-client-") as directory:
                root = Path(directory)
                bridge = Bridge(
                    self.bridge,
                    self.rpc,
                    root / "sessions",
                    root,
                    extra_env={"RPI_TEST_RPC_CLIENT_REQUEST": method},
                )
                try:
                    initialized, _ = bridge.request("initialize", {"protocolVersion": 1})
                    self.assertEqual(initialized["result"]["protocolVersion"], 1)
                    created, _ = bridge.request("session/new", {"cwd": str(root), "mcpServers": []})
                    session_id = created["result"]["sessionId"]
                    bridge.send({"jsonrpc": "2.0", "id": 71, "method": "session/prompt",
                                 "params": {"sessionId": session_id,
                                            "prompt": [{"type": "text", "text": "client request"}]}})
                    request = None
                    messages = []
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        message = bridge.output.get(timeout=.5)
                        messages.append(message)
                        if message.get("method") == method:
                            request = message
                            break
                    self.assertIsNotNone(request, messages)
                    self.assertEqual(request["params"]["sessionId"], session_id)
                    bridge.send({"jsonrpc": "2.0", "id": request["id"], "result": result})
                    response, messages = bridge.wait_response(71)
                    self.assertEqual(response.get("result", {}).get("stopReason"), "end_turn", messages)
                finally:
                    bridge.close()


        with tempfile.TemporaryDirectory(prefix="rpi-acp-list-") as directory:
            root = Path(directory)
            bridge = Bridge(self.bridge, self.rpc, root / "sessions", root)
            try:
                result, _ = bridge.request("initialize", {"protocolVersion": 1})
                self.assertEqual(result["result"]["agentCapabilities"]["sessionCapabilities"]["list"], {})
                created, _ = bridge.request("session/new", {"cwd": str(root), "mcpServers": []})
                session_id = created["result"]["sessionId"]
                listed, _ = bridge.request("session/list", {"cwd": str(root)})
                sessions = listed["result"]["sessions"]
                self.assertEqual([item["sessionId"] for item in sessions], [session_id])
                self.assertEqual(Path(sessions[0]["cwd"]), root)
                self.assertIsInstance(sessions[0]["updatedAt"], str)
                invalid, _ = bridge.request("session/list", {"cwd": "relative"})
                self.assertEqual(invalid["error"]["code"], -32602)
                closed, _ = bridge.request("session/close", {"sessionId": session_id})
                self.assertEqual(closed.get("result"), {}, closed)
                listed_after_close, _ = bridge.request("session/list", {})
                self.assertEqual(len(listed_after_close["result"]["sessions"]), 1)
                deleted, _ = bridge.request("session/delete", {"sessionId": session_id})
                self.assertEqual(deleted.get("result"), {}, deleted)
                listed_after_delete, _ = bridge.request("session/list", {})
                self.assertEqual(listed_after_delete["result"]["sessions"], [])
            finally:
                bridge.close()

    def test_cancel_new_during_slow_ready_kills_uncommitted_child(self):
        with tempfile.TemporaryDirectory(prefix="rpi-acp-cancel-new-") as directory:
            root = Path(directory)
            pid_log = root / "pids.log"
            bridge = Bridge(
                self.bridge,
                self.rpc,
                root / "sessions",
                root,
                extra_env={
                    "RPI_TEST_RPC_READY_DELAY_MS": 10000,
                    "RPI_TEST_RPC_PID_LOG": pid_log,
                },
            )
            try:
                bridge.request("initialize", {"protocolVersion": 1})
                bridge.send({"jsonrpc": "2.0", "id": 901, "method": "session/new",
                             "params": {"cwd": str(root), "mcpServers": []}})
                pid = bridge.wait_pid(pid_log)[0]
                handle = Bridge.open_pid_handle(pid)
                bridge.send({"jsonrpc": "2.0", "method": "$/cancel_request",
                             "params": {"requestId": 901}})
                cancelled, _ = bridge.wait_response(901, timeout=5)
                self.assertEqual(cancelled.get("error", {}).get("code"), -32800, cancelled)
                Bridge.wait_handle_exit(handle)
            finally:
                bridge.close()

    def test_cancel_new_after_child_started_before_state_commit(self):
        with tempfile.TemporaryDirectory(prefix="rpi-acp-cancel-commit-") as directory:
            root = Path(directory)
            pid_log = root / "pids.log"
            bridge = Bridge(
                self.bridge,
                self.rpc,
                root / "sessions",
                root,
                extra_env={
                    "RPI_TEST_RPC_GET_STATE_DELAY_MS": 10000,
                    "RPI_TEST_RPC_PID_LOG": pid_log,
                },
            )
            try:
                bridge.request("initialize", {"protocolVersion": 1})
                bridge.send({"jsonrpc": "2.0", "id": 912, "method": "session/new",
                             "params": {"cwd": str(root), "mcpServers": []}})
                pid = bridge.wait_pid(pid_log)[0]
                handle = Bridge.open_pid_handle(pid)
                bridge.send({"jsonrpc": "2.0", "method": "$/cancel_request",
                             "params": {"requestId": 912}})
                cancelled, _ = bridge.wait_response(912, timeout=5)
                self.assertEqual(cancelled.get("error", {}).get("code"), -32800, cancelled)
                Bridge.wait_handle_exit(handle)
            finally:
                bridge.close()
    def test_logout_cancel_kills_helper_and_multiple_sessions_do_not_block(self):
        with tempfile.TemporaryDirectory(prefix="rpi-acp-owner-") as directory:
            root = Path(directory)
            pid_log = root / "pids.log"
            bridge = Bridge(
                self.bridge,
                self.rpc,
                root / "sessions",
                root,
                extra_env={
                    "RPI_TEST_LOGOUT_HANG_MS": 10000,
                    "RPI_TEST_RPC_PID_LOG": pid_log,
                },
            )
            try:
                bridge.request("initialize", {"protocolVersion": 1})
                bridge.send({"jsonrpc": "2.0", "id": 902, "method": "logout", "params": {}})
                logout_pid = bridge.wait_pid(pid_log)[0]
                logout_handle = Bridge.open_pid_handle(logout_pid)
                bridge.send({"jsonrpc": "2.0", "method": "$/cancel_request",
                             "params": {"requestId": 902}})
                cancelled, _ = bridge.wait_response(902, timeout=5)
                self.assertEqual(cancelled.get("error", {}).get("code"), -32800, cancelled)
                Bridge.wait_handle_exit(logout_handle)

                # One duplicate ID reserves the shared namespace before the
                # second request can launch another child.
                bridge.send({"jsonrpc": "2.0", "id": 903, "method": "session/new",
                             "params": {"cwd": str(root), "mcpServers": []}})
                bridge.send({"jsonrpc": "2.0", "id": 903, "method": "session/new",
                             "params": {"cwd": str(root), "mcpServers": []}})
                first, _ = bridge.wait_response(903)
                second, _ = bridge.wait_response(903)
                responses = [first, second]
                self.assertEqual(sum("result" in value for value in responses), 1, responses)
                self.assertEqual(sum("error" in value for value in responses), 1, responses)
                session_id = next(value["result"]["sessionId"] for value in responses if "result" in value)
                child_pids = bridge.wait_pid(pid_log, count=2)
                self.assertEqual(len(child_pids), 2)

                # Two independent startups proceed concurrently rather than
                # waiting behind a process-wide request lock.
                bridge.send({"jsonrpc": "2.0", "id": 904, "method": "session/new",
                             "params": {"cwd": str(root), "mcpServers": []}})
                bridge.send({"jsonrpc": "2.0", "id": 905, "method": "session/new",
                             "params": {"cwd": str(root), "mcpServers": []}})
                left, messages = bridge.wait_response(904)
                right = next((message for message in messages if message.get("id") == 905), None)
                if right is None:
                    right, _ = bridge.wait_response(905)
                self.assertIn("result", left, left)
                self.assertIn("result", right, right)
                for value in (session_id, left["result"]["sessionId"], right["result"]["sessionId"]):
                    closed, _ = bridge.request("session/close", {"sessionId": value})
                    self.assertEqual(closed.get("result"), {}, closed)
            finally:
                bridge.close()

    def test_eof_cleans_all_pending_new_children(self):
        with tempfile.TemporaryDirectory(prefix="rpi-acp-eof-") as directory:
            root = Path(directory)
            pid_log = root / "pids.log"
            bridge = Bridge(
                self.bridge,
                self.rpc,
                root / "sessions",
                root,
                extra_env={
                    "RPI_TEST_RPC_READY_DELAY_MS": 30000,
                    "RPI_TEST_RPC_PID_LOG": pid_log,
                },
            )
            try:
                bridge.request("initialize", {"protocolVersion": 1})
                for request_id in (907, 908, 909, 910, 911):
                    bridge.send({"jsonrpc": "2.0", "id": request_id, "method": "session/new",
                                 "params": {"cwd": str(root), "mcpServers": []}})
                pids = bridge.wait_pid(pid_log, count=5)
                handles = [Bridge.open_pid_handle(pid) for pid in pids]
                bridge.process.stdin.close()
                bridge.process.wait(timeout=8)
                for handle in handles:
                    Bridge.wait_handle_exit(handle)
            finally:
                bridge.close()
    def test_output_backpressure_closes_bridge_and_child(self):
        with tempfile.TemporaryDirectory(prefix="rpi-acp-backpressure-") as directory:
            root = Path(directory)
            pid_log = root / "pids.log"
            bridge = Bridge(
                self.bridge,
                self.rpc,
                root / "sessions",
                root,
                extra_env={"RPI_TEST_RPC_PID_LOG": pid_log},
                read_output=False,
            )
            try:
                bridge.send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                             "params": {"protocolVersion": 1}})
                initialized = bridge.read_raw()
                self.assertEqual(initialized.get("result", {}).get("protocolVersion"), 1)
                bridge.send({"jsonrpc": "2.0", "id": 2, "method": "session/new",
                             "params": {"cwd": str(root), "mcpServers": []}})
                created = bridge.read_raw()
                self.assertIn("sessionId", created.get("result", {}), created)
                pid = bridge.wait_pid(pid_log)[0]
                handle = Bridge.open_pid_handle(pid)
                # No reader thread has consumed stdout since the handshake;
                # this 128 KiB error response fills the OS pipe and exercises
                # the writer mutex/IO deadline with an idle native child.
                bridge.send({"jsonrpc": "2.0", "id": 906,
                             "method": "x" * (128 * 1024), "params": {}})
                bridge.process.wait(timeout=15)
                Bridge.wait_handle_exit(handle)
                self.assertEqual(bridge.process.returncode, 0)
            finally:
                bridge.close()

    def test_explicit_v2_initialize_and_prompt_lifecycle(self):
        with tempfile.TemporaryDirectory(prefix="rpi-acp-v2-") as directory:
            root = Path(directory)
            bridge = Bridge(self.bridge, self.rpc, root / "sessions", root)
            try:
                initialized, _ = bridge.request("initialize", {
                    "protocolVersion": 2,
                    "info": {"name": "test-client", "version": "1"},
                    "capabilities": {"elicitation": {"form": {}}},
                })
                result = initialized["result"]
                self.assertEqual(result["protocolVersion"], 2)
                self.assertIn("info", result)
                self.assertIn("session", result["capabilities"])
                self.assertNotIn("agentCapabilities", result)
                created, _ = bridge.request("session/new", {
                    "cwd": str(root), "additionalDirectories": [], "mcpServers": []})
                session_id = created["result"]["sessionId"]
                unsupported_mcp, _ = bridge.request("session/new", {
                    "cwd": str(root), "mcpServers": [{
                        "type": "stdio", "name": "tools", "command": "C:/tools/mcp.exe"}]
                })
                self.assertEqual(unsupported_mcp["error"]["code"], -32602, unsupported_mcp)
                unsupported_replay, _ = bridge.request("session/resume", {
                    "sessionId": session_id, "cwd": str(root),
                    "replayFrom": {"type": "start"}, "mcpServers": []})
                self.assertEqual(unsupported_replay["error"]["code"], -32602, unsupported_replay)
                bridge.send({"jsonrpc": "2.0", "id": 77, "method": "session/prompt",
                             "params": {"sessionId": session_id,
                                        "prompt": [{"type": "text", "text": "v2"}]}})
                messages = []
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    message = bridge.output.get(timeout=.5)
                    messages.append(message)
                    if message.get("method") == "elicitation/create":
                        bridge.send({"jsonrpc": "2.0", "id": message["id"],
                                     "result": {"action": "accept",
                                                "content": {"answers": ["Rust"]}}})
                    elif message.get("method") == "session/request_permission":
                        bridge.send({"jsonrpc": "2.0", "id": message["id"],
                                     "result": {"outcome": {"outcome": "selected",
                                                               "optionId": "0"}}})
                    if message.get("id") == 77:
                        response = message
                        break
                else:
                    self.fail(f"timed out waiting for v2 prompt response: {messages}")
                self.assertEqual(response["result"]["messageId"].startswith("user:"), True)
                updates = [m["params"]["update"] for m in messages
                           if m.get("method") == "session/update"]
                self.assertTrue(any(u.get("sessionUpdate") == "user_message" for u in updates), updates)
                self.assertTrue(any(u.get("sessionUpdate") == "state_update" and
                                    u.get("state") == "running" for u in updates), updates)
                self.assertTrue(any(u.get("sessionUpdate") == "state_update" and
                                    u.get("state") == "idle" for u in updates), updates)
                self.assertTrue(all("messageId" in u for u in updates
                                    if u.get("sessionUpdate") in
                                    ("user_message", "agent_message_chunk", "agent_thought_chunk")))
            finally:
                bridge.close()

    def test_native_client_request_round_trip_through_acp(self):
        if self.native is None:
            self.skipTest("--native is required for the real native producer fixture")
        with tempfile.TemporaryDirectory(prefix="rpi-acp-native-client-") as directory:
            root = Path(directory)
            bridge = Bridge(self.bridge, self.rpc, root / "sessions", root,
                            extra_env={"RPI_BIN": str(self.native)})
            try:
                initialized, _ = bridge.request("initialize", {
                    "protocolVersion": 2,
                    "info": {"name": "native-client-fixture", "version": "1"},
                    "capabilities": {},
                })
                self.assertEqual(initialized["result"]["protocolVersion"], 2)
                auth_state, _ = bridge.request("auth/get_state", {})
                self.assertIn("ready", auth_state["result"])
                self.assertEqual(auth_state["result"]["provider"], "anthropic")
                created, _ = bridge.request("session/new", {
                    "cwd": str(root), "additionalDirectories": [], "mcpServers": []})
                session_id = created["result"]["sessionId"]
                bridge.send({"jsonrpc": "2.0", "id": 91, "method": "rpi/client_request",
                             "params": {"sessionId": session_id,
                                        "method": "fs/read_text_file",
                                        "params": {"path": str(root / "README.md")}}})
                request = None
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    message = bridge.output.get(timeout=.5)
                    if message.get("method") == "fs/read_text_file":
                        request = message
                        break
                self.assertIsNotNone(request)
                bridge.send({"jsonrpc": "2.0", "id": request["id"],
                             "result": {"content": "native ACP client round trip"}})
                response, _ = bridge.wait_response(91)
                self.assertEqual(response["result"], {"content": "native ACP client round trip"})
            finally:
                bridge.close()

    @classmethod
    def from_args(cls, bridge, rpc, native=None):
        cls.bridge = bridge
        cls.rpc = rpc
        cls.native = native
        return cls



def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bridge", required=True, type=Path)
    parser.add_argument("--rpc", required=True, type=Path)
    parser.add_argument("--native", type=Path)
    args = parser.parse_args()
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        ProtocolRegression.from_args(args.bridge, args.rpc, args.native))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)


if __name__ == "__main__":
    main()
