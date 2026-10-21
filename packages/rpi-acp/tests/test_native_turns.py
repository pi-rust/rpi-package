"""Real-host turn/extension regression using a local model fixture, no API calls.

Set RPI_TEST_HOST, RPI_TEST_PLUGIN (ACP), and RPI_TEST_GOAL_PLUGIN to enable.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import tempfile
import threading
import unittest

from restart import Client


@unittest.skipUnless(all(os.environ.get(name) for name in (
    "RPI_TEST_HOST", "RPI_TEST_PLUGIN", "RPI_TEST_GOAL_PLUGIN",
)), "set native host, ACP and goal plugin paths")
class NativeTurnTests(unittest.TestCase):
    def test_goal_observer_does_not_block_completion_or_tool_continuation(self):
        cancel_started = threading.Event()
        release_cancel = threading.Event()

        class Model(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                last = body["messages"][-1]
                if last.get("content") == "cancel":
                    cancel_started.set()
                    release_cancel.wait(timeout=10)
                tool = last["role"] == "user" and last.get("content") == "tools"
                if tool:
                    delta = {"content": "Before read.", "tool_calls": [{
                        "index": 0, "id": "native-read", "type": "function",
                        "function": {"name": "read", "arguments": json.dumps({"path": "fixture.txt"})},
                    }]}
                else:
                    delta = {"content": "After read." if last["role"] == "tool" else "Reply."}
                events = [
                    {"choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": None}]},
                    {"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                    {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if tool else "stop"}]},
                ]
                data = ("".join("data: " + json.dumps(e) + "\n\n" for e in events)
                        + "data: [DONE]\n\n").encode()
                try:
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    pass  # The cancelled native request closes its connection.

        server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
        serving = threading.Thread(target=server.serve_forever, daemon=True)
        serving.start()
        try:
            with tempfile.TemporaryDirectory(prefix="rpi-acp-native-turns-") as directory:
                root = Path(directory)
                agent = root / "agent"
                extensions = agent / "extensions"
                extensions.mkdir(parents=True)
                shutil.copy2(os.environ["RPI_TEST_GOAL_PLUGIN"], extensions / Path(os.environ["RPI_TEST_GOAL_PLUGIN"]).name)
                (root / "fixture.txt").write_text("local fixture", encoding="utf-8")
                (agent / "models.json").write_text(json.dumps({"providers": {"native-fixture": {
                    "api": "openai-completions", "baseUrl": f"http://127.0.0.1:{server.server_port}/v1",
                    "apiKey": "offline-fixture", "models": [{"id": "fixture", "name": "fixture",
                        "reasoning": False, "input": ["text"], "contextWindow": 8192, "maxTokens": 1024}],
                }}}), encoding="utf-8")
                (agent / "settings.json").write_text(json.dumps({
                    "defaultProvider": "native-fixture", "defaultModel": "fixture", "defaultThinkingLevel": "off",
                }), encoding="utf-8")
                env = {key: value for key, value in os.environ.items()
                       if not key.endswith(("_API_KEY", "_AUTH_TOKEN"))
                       and key not in ("RPI_EXTENSIONS_DIR", "RPI_PROVIDER", "RPI_MODEL")}
                env.update(RPI_BIN=os.environ["RPI_TEST_HOST"], RPI_CODING_AGENT_DIR=str(agent),
                           RPI_ACP_SESSION_DIR=str(root / "sessions"), RPI_ACP_NO_PERMISSION_GATE="1",
                           RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_OFFLINE="1")
                client = Client([os.environ["RPI_TEST_HOST"], "--acp", "--no-session", "--local-only",
                                 "--extension", os.environ["RPI_TEST_PLUGIN"]], env)
                try:
                    result, _ = client.request("initialize", {"protocolVersion": 1})
                    self.assertIn("result", result)
                    result, _ = client.request("session/new", {"cwd": str(root), "mcpServers": []})
                    self.assertIn("result", result)
                    sid = result["result"]["sessionId"]
                    for prompt, expected in [("first", "Reply."), ("second", "Reply."),
                                             ("tools", "Before read.After read."), ("after tools", "Reply.")]:
                        with self.subTest(prompt=prompt):
                            result, messages = client.request("session/prompt", {
                                "sessionId": sid, "prompt": [{"type": "text", "text": prompt}],
                            })
                            self.assertEqual(result.get("result"), {"stopReason": "end_turn"}, result)
                            updates = [m["params"]["update"] for m in messages if m.get("method") == "session/update"]
                            text = "".join(u["content"]["text"] for u in updates
                                           if u["sessionUpdate"] == "agent_message_chunk")
                            self.assertEqual(text, expected)
                            if prompt == "tools":
                                self.assertTrue(any(u["sessionUpdate"] == "tool_call" for u in updates))
                                self.assertTrue(any(u["sessionUpdate"] == "tool_call_update"
                                                    and u.get("status") == "completed" for u in updates))
                    cancelled = []
                    errors = []

                    def prompt_until_cancelled():
                        try:
                            cancelled.append(client.request("session/prompt", {
                                "sessionId": sid, "prompt": [{"type": "text", "text": "cancel"}],
                            }))
                        except BaseException as error:
                            errors.append(error)

                    waiting = threading.Thread(target=prompt_until_cancelled, daemon=True)
                    waiting.start()
                    try:
                        self.assertTrue(cancel_started.wait(timeout=10), "native prompt did not reach the local model")
                        client.process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "session/cancel",
                                                               "params": {"sessionId": sid}}) + "\n")
                        client.process.stdin.flush()
                        waiting.join(timeout=10)
                        self.assertFalse(waiting.is_alive(), "native cancellation did not settle ACP")
                        self.assertEqual(errors, [])
                        self.assertEqual(cancelled[0][0].get("result"), {"stopReason": "cancelled"})
                    finally:
                        release_cancel.set()
                    result, _ = client.request("session/prompt", {
                        "sessionId": sid, "prompt": [{"type": "text", "text": "after cancellation"}],
                    })
                    self.assertEqual(result.get("result"), {"stopReason": "end_turn"}, result)
                finally:
                    client.close()
        finally:
            release_cancel.set()
            server.shutdown()
            server.server_close()
            serving.join(timeout=5)
