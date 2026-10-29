"""Protocol and lifecycle regression against a real rpi-acp-bridge process.

Build the bridge and the offline native peer first, then run:

    python packages/rpi-acp/tests/protocol_regression.py \
        --bridge target/debug/rpi-acp-bridge.exe --rpc target/debug/examples/fake_rpc.exe

The bridge is a real subprocess; only the native rpi --mode rpc child is a
controlled fixture, so malformed wire input and shutdown races exercise the
actual ACP transport.
"""
import argparse
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
    def __init__(self, executable, rpc, store, cwd):
        env = os.environ.copy()
        env.update(
            RPI_BIN=str(rpc),
            RPI_ACP_SESSION_DIR=str(store),
            RPI_ACP_NO_AUTO_REGISTER_ZED="1",
            RPI_ACP_NO_PERMISSION_GATE="1",
        )
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
        self.errors = []
        threading.Thread(target=self._read_output, daemon=True).start()
        threading.Thread(target=lambda: self.errors.extend(self.process.stderr), daemon=True).start()
        self.next_id = 0

    def _read_output(self):
        for line in self.process.stdout:
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

    def assert_no_output(self, seconds=.25):
        with self.assert_raises_empty():
            self.output.get(timeout=seconds)

    class assert_raises_empty:
        def __enter__(self):
            return self
        def __exit__(self, exception_type, exception, traceback):
            return exception_type is queue.Empty

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

                result, _ = bridge.request("initialize", {
                    "protocolVersion": 1,
                    "clientCapabilities": {"elicitation": {"form": {}}},
                })
                self.assertEqual(result["result"]["protocolVersion"], 1)
                logout, messages = bridge.request("logout", {})
                self.assertEqual(logout.get("result"), {}, logout)
                self.assertFalse(any("__decode_error" in message for message in messages), messages)
                self.assertEqual(result["result"]["agentCapabilities"]["sessionCapabilities"]["close"], {})
                result, _ = bridge.request("initialize", {"protocolVersion": 2})
                self.assertEqual(result["error"]["code"], -32602)

                result, _ = bridge.request("session/new", {"cwd": str(root), "mcpServers": []})
                session_id = result["result"]["sessionId"]

                # The fixture opens a permission/question dialog. Sending a
                # JSON-RPC cancellation must settle the native run; close then
                # waits for that prompt task before stopping the child.
                bridge.send({"jsonrpc": "2.0", "id": 99, "method": "session/prompt",
                             "params": {"sessionId": session_id,
                                        "prompt": [{"type": "text", "text": "cancel me"}]}})
                def wait_for_active_permission():
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        message = bridge.output.get(timeout=.5)
                        if message.get("method") == "elicitation/create":
                            bridge.send({"jsonrpc": "2.0", "method": "$/cancel_request",
                                         "params": {"requestId": message["id"]}})
                        elif message.get("method") == "session/request_permission":
                            return message["id"]
                    self.fail("native fixture did not open a permission request")

                # First turn: cancel an agent->client elicitation request, then
                # cancel the client->agent prompt request itself.
                wait_for_active_permission()
                bridge.send({"jsonrpc": "2.0", "method": "$/cancel_request", "params": {"requestId": 99}})
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if bridge.output.get(timeout=.5).get("id") == 99:
                        break
                else:
                    self.fail("prompt request did not settle after $/cancel_request")

                # Second turn: leave a permission dialog active and close the
                # session. Close must cancel the native dialog, await the
                # prompt task, and only then stop the child.
                bridge.send({"jsonrpc": "2.0", "id": 100, "method": "session/prompt",
                             "params": {"sessionId": session_id,
                                        "prompt": [{"type": "text", "text": "close me"}]}})
                wait_for_active_permission()
                close, _ = bridge.request("session/close", {"sessionId": session_id})
                self.assertEqual(close.get("result"), {}, close)

                # Close retains the native transcript and load reattaches it.
                loaded, _ = bridge.request("session/resume", {
                    "sessionId": session_id, "cwd": str(root), "mcpServers": []})
                self.assertIn("result", loaded, loaded)
                deleted, _ = bridge.request("session/delete", {"sessionId": session_id})
                self.assertEqual(deleted.get("result"), {}, deleted)
                repeated, _ = bridge.request("session/delete", {"sessionId": session_id})
                self.assertEqual(repeated.get("result"), {}, repeated)
            finally:
                bridge.close()

    @classmethod
    def from_args(cls, bridge, rpc):
        cls.bridge = bridge
        cls.rpc = rpc
        return cls


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bridge", required=True, type=Path)
    parser.add_argument("--rpc", required=True, type=Path)
    args = parser.parse_args()
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        ProtocolRegression.from_args(args.bridge, args.rpc))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)


if __name__ == "__main__":
    main()
