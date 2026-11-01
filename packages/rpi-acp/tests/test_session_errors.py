"""Session failures must not use ACP's reserved authentication error code."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
import uuid

from restart import Client


@unittest.skipUnless(os.environ.get("RPI_TEST_ACP_BRIDGE"), "set RPI_TEST_ACP_BRIDGE")
class SessionErrorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rpi-acp-session-errors-")
        self.root = Path(self.temp.name)
        self.store = self.root / "sessions"
        self.client = Client([os.environ["RPI_TEST_ACP_BRIDGE"]], dict(
            os.environ, RPI_ACP_SESSION_DIR=str(self.store), RPI_BIN=sys.executable))
        response, _ = self.client.request("initialize", dict(protocolVersion=1))
        self.assertIn("result", response)

    def tearDown(self):
        self.client.close()
        self.temp.cleanup()

    def assert_session_error(self, method, **params):
        response, _ = self.client.request(method, dict(cwd=str(self.root), mcpServers=[], **params))
        self.assertEqual(response["error"]["code"], -32603, response)
        return response["error"]["message"]

    def test_old_and_missing_sessions_do_not_trigger_login(self):
        for method in ["session/load", "session/resume"]:
            with self.subTest(method=method):
                message = self.assert_session_error(method, sessionId="rpi-26968-1")
                self.assertIn("start a new conversation", message)
                message = self.assert_session_error(method, sessionId=str(uuid.uuid4()))
                self.assertIn("no saved ACP session", message)

    def test_invalid_saved_history_does_not_trigger_login(self):
        session_id = str(uuid.uuid4())
        directory = self.store / session_id / "workspace"
        directory.mkdir(parents=True)
        (directory / f"fixture_{session_id}.jsonl").write_text(json.dumps({"kind": "invalid"}) + "\n")
        for method in ["session/load", "session/resume"]:
            with self.subTest(method=method):
                message = self.assert_session_error(method, sessionId=session_id)
                self.assertIn("invalid saved session header", message)

    def test_invalid_working_directories_do_not_trigger_login(self):
        regular_file = self.root / "not-a-directory"
        regular_file.write_text("fixture", encoding="utf-8")
        for cwd in [str(self.root / "missing"), "relative-directory", str(regular_file)]:
            with self.subTest(cwd=cwd):
                response, _ = self.client.request("session/new", dict(cwd=cwd, mcpServers=[]))
                # Explicit MCP parameters validate cwd before session creation:
                # invalid parameters are -32602, never an authentication error.
                self.assertEqual(response["error"]["code"], -32602, response)
                self.assertIn("cwd", response["error"]["message"])


if __name__ == "__main__":
    unittest.main()
