"""Offline ACP v2 insertion, failure, cancellation and correlation wire tests.

    cargo build -p rpi-acp --bin rpi-acp-bridge --example prompt_correctness_rpc
    python packages/rpi-acp/tests/prompt_correctness.py \
        --bridge target/debug/rpi-acp-bridge --rpc target/debug/examples/prompt_correctness_rpc

On Windows use .exe paths. Only the native child is fake; ACP is a real process.
Settlement is explicitly gated, never inferred from a timeout or prompt acceptance.
"""
import argparse
from contextlib import contextmanager
import json
from pathlib import Path
import queue
import tempfile
import time
import unittest

from protocol_regression import Bridge


class PromptCorrectness(unittest.TestCase):
    @contextmanager
    def peer(self):
        with tempfile.TemporaryDirectory(prefix="rpi-acp-prompt-") as directory:
            self.root = Path(directory)
            self.log = self.root / "native.jsonl"
            self.messages = []
            bridge = Bridge(self.bridge, self.rpc, self.root / "sessions", self.root,
                            extra_env={"RPI_PROMPT_CORRECTNESS_LOG": self.log})
            self.peer_bridge = bridge
            try:
                initialized, _ = bridge.request("initialize", {
                    "protocolVersion": 2,
                    "info": {"name": "prompt-correctness", "version": "1"},
                    "capabilities": {},
                })
                self.assertEqual(initialized["result"]["protocolVersion"], 2)
                created, _ = bridge.request("session/new", {
                    "cwd": str(self.root), "additionalDirectories": [], "mcpServers": []})
                self.sid = created["result"]["sessionId"]
                yield bridge
            finally:
                bridge.close()

    def send(self, request_id, rpc_method, **params):
        self.peer_bridge.send({"jsonrpc": "2.0", "id": request_id,
                               "method": rpc_method, "params": {"sessionId": self.sid, **params}})

    def wait(self, predicate, timeout=8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                message = self.peer_bridge.output.get(timeout=max(.01, deadline - time.monotonic()))
            except queue.Empty:
                break
            self.messages.append(message)
            self.assertNotIn("__decode_error", message)
            if predicate(message):
                return message
        self.fail(f"timed out; wire={self.messages!r}; stderr={self.peer_bridge.errors!r}")

    @staticmethod
    def update(message):
        return message.get("params", {}).get("update", {}) if message.get("method") == "session/update" else {}

    def idle(self):
        return self.wait(lambda m: self.update(m).get("state") == "idle")

    def drain(self, seconds=.2):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                self.messages.append(self.peer_bridge.output.get(timeout=max(.01, deadline - time.monotonic())))
            except queue.Empty:
                return

    def assert_once(self, request_id):
        self.drain()
        responses = [m for m in self.messages if m.get("id") == request_id and
                     ("result" in m or "error" in m)]
        self.assertEqual(len(responses), 1, responses)
        return responses[0]

    def prompt(self, scenario):
        self.send(100, "session/prompt", prompt=[{"type": "text", "text": scenario}])
        response = self.wait(lambda m: m.get("id") == 100)
        self.assertEqual(response.get("result", {}).get("messageId"),
                         "native-persisted-user-001", response)
        self.assertFalse(any(self.update(m).get("state") == "idle" for m in self.messages))
        users = [self.update(m) for m in self.messages
                 if self.update(m).get("sessionUpdate") == "user_message"]
        self.assertEqual(len(users), 1, self.messages)
        self.assertEqual(users[0]["messageId"], "native-persisted-user-001")
        self.assertEqual(users[0]["content"], [{"type": "text", "text": scenario}])
        histories = list((self.root / "sessions").rglob("correctness_*.jsonl"))
        self.assertEqual(len(histories), 1)
        persisted = [json.loads(line) for line in histories[0].read_text().splitlines()]
        self.assertEqual(persisted[-1]["message"]["id"], response["result"]["messageId"])

    def signal_settlement(self):
        self.send(200, "rpi/client_request", method="fs/read_text_file",
                  params={"path": str(self.root / "settle")})

    def test_insertion_acknowledged_before_settlement_and_duplicate_is_ignored(self):
        for scenario in ("success", "duplicate"):
            with self.subTest(scenario=scenario), self.peer():
                self.prompt(scenario)
                self.drain()
                self.assertFalse(any(self.update(m).get("state") == "idle" for m in self.messages))
                self.signal_settlement()
                self.assertEqual(self.update(self.idle())["stopReason"], "end_turn")
                self.assertIn("result", self.assert_once(100))
                self.assertEqual(sum(self.update(m).get("sessionUpdate") == "user_message"
                                     for m in self.messages), 1)

    def test_pre_insertion_rejection_returns_one_error(self):
        with self.peer():
            self.send(100, "session/prompt", prompt=[{"type": "text", "text": "reject"}])
            response = self.wait(lambda m: m.get("id") == 100)
            self.assertIn("error", response)
            self.assert_once(100)
            self.assertFalse(any(self.update(m).get("sessionUpdate") == "user_message"
                                 for m in self.messages), self.messages)

    def test_post_insertion_failure_does_not_replace_success_response(self):
        with self.peer():
            self.prompt("post-failure")
            self.signal_settlement()
            self.assertEqual(self.update(self.idle())["stopReason"], "error")
            self.assertIn("result", self.assert_once(100))

    def test_cancel_forwards_cleanup_and_rejects_ordinary_requests(self):
        with self.peer() as bridge:
            self.prompt("cancel")
            # create may arrive before the prompt response: search the retained wire too.
            create = next((m for m in self.messages if m.get("method") == "terminal/create"), None)
            if create is None:
                create = self.wait(lambda m: m.get("method") == "terminal/create")
            bridge.send({"jsonrpc": "2.0", "id": create["id"],
                         "result": {"terminalId": "fixture-terminal"}})
            self.wait(lambda m: m.get("method") == "terminal/wait_for_exit")
            bridge.send({"jsonrpc": "2.0", "method": "session/cancel",
                         "params": {"sessionId": self.sid}})
            kill = self.wait(lambda m: m.get("method") == "terminal/kill")
            self.drain()
            self.assertFalse(any(self.update(m).get("state") == "idle" for m in self.messages))
            self.assertFalse(any(m.get("method") == "terminal/release" for m in self.messages))
            bridge.send({"jsonrpc": "2.0", "id": kill["id"], "result": {}})
            release = self.wait(lambda m: m.get("method") == "terminal/release")
            self.drain()
            self.assertFalse(any(self.update(m).get("state") == "idle" for m in self.messages))
            bridge.send({"jsonrpc": "2.0", "id": release["id"], "result": {}})
            self.assertEqual(self.update(self.idle())["stopReason"], "cancelled")
            self.assertIn("result", self.assert_once(100))
            commands = [json.loads(line) for line in self.log.read_text().splitlines()]
            self.assertGreaterEqual(sum(c["type"] == "abort" for c in commands), 1)
            answers = [c for c in commands if c["type"] == "client_response"]
            for request_id in ("wait", "forbidden-fs", "forbidden-create"):
                rejected = [c for c in answers if c["requestId"] == request_id]
                self.assertEqual(len(rejected), 1, answers)
                self.assertTrue(rejected[0].get("cancelled") or "error" in rejected[0])
            for request_id in ("kill", "release"):
                self.assertEqual(len([c for c in answers if c["requestId"] == request_id
                                      and c.get("result") == {}]), 1, answers)
            self.assertEqual(sum(m.get("method") == "terminal/create" for m in self.messages), 1)
            self.assertFalse(any(m.get("method", "").startswith("fs/") for m in self.messages))

    def test_concurrent_same_method_diagnostics_keep_correlation(self):
        with self.peer() as bridge:
            for request_id, tag in ((301, "alpha"), (302, "beta")):
                self.send(request_id, "rpi/client_request", method="fs/read_text_file",
                          params={"path": str(self.root / tag)})
            forwarded = []
            while len(forwarded) < 2:
                forwarded.append(self.wait(lambda m: m.get("method") == "fs/read_text_file"))
            self.assertEqual({Path(m["params"]["path"]).name for m in forwarded}, {"alpha", "beta"})
            # Reverse answers so FIFO matching cannot accidentally pass.
            for message in reversed(forwarded):
                bridge.send({"jsonrpc": "2.0", "id": message["id"],
                             "result": {"content": Path(message["params"]["path"]).name}})
            while not all(any(m.get("id") == i for m in self.messages) for i in (301, 302)):
                self.wait(lambda m: m.get("id") in (301, 302))
            for request_id, tag in ((301, "alpha"), (302, "beta")):
                self.assertEqual(self.assert_once(request_id)["result"], {"content": tag})
            self.assertEqual(sum(m.get("method") == "fs/read_text_file" for m in self.messages), 2)
            commands = [json.loads(line) for line in self.log.read_text().splitlines()]
            diagnostics = [c for c in commands if c["type"] == "client_request"]
            self.assertEqual(len(diagnostics), 2)
            correlations = [c.get("correlationId") for c in diagnostics]
            self.assertTrue(all(isinstance(c, str) and c for c in correlations), diagnostics)
            self.assertEqual(len(set(correlations)), 2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", type=Path, required=True)
    parser.add_argument("--rpc", type=Path, required=True)
    args = parser.parse_args()
    PromptCorrectness.bridge = args.bridge.resolve()
    PromptCorrectness.rpc = args.rpc.resolve()
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(PromptCorrectness)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)


if __name__ == "__main__":
    main()
