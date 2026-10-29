"""ACP durable attachment must fail closed and retire its native child.

RPI_TEST_ACP_BRIDGE=/path/rpi-acp-bridge python -m unittest discover \
    -s packages/rpi-acp/tests -p test_durable_attach.py -v
Uses a deterministic RPC peer, without model calls or credentials.
"""
import ctypes
import json
import os
from pathlib import Path
import shlex
import sys
import tempfile
import time
import unittest
import uuid

from restart import Client


def backend():
    if "--session-id" in sys.argv:
        session_id = sys.argv[sys.argv.index("--session-id") + 1]
    else:
        saved = Path(sys.argv[sys.argv.index("--session") + 1])
        session_id = json.loads(saved.read_text().splitlines()[0])["id"]
    scenario = os.environ["RPI_ATTACH_SCENARIO"]
    root = Path(os.environ["RPI_ATTACH_ROOT"])
    (root / f"child-{uuid.uuid4()}").write_text(str(os.getpid()))

    def emit(value):
        print(json.dumps(value), flush=True)

    ready = dict(type="ready", protocol="rpi")
    if scenario != "no-capability":
        ready["supportedCommands"] = ["get_session_branch", "execute_command"]
    emit(ready)
    for line in sys.stdin:
        command = json.loads(line)
        kind = command["type"]
        success, error = True, None
        if kind == "get_session_branch":
            assert scenario != "no-capability", command
            assert command["customTypes"] == ["durable-state"], command
            assert "customType" not in command, command
            data = dict(filtered=True, entries=[])
            if scenario == "missing-filtered":
                del data["filtered"]
            elif scenario == "false-filtered":
                data["filtered"] = False
            elif scenario == "rpc-error":
                success, error = False, "fixture branch RPC failed"
            elif scenario == "corrupt-journal":
                data["entries"] = [dict(id="broken", type="custom", customType="durable-state",
                                        data=dict(format="rpi-durable-journal", version=999))]
        elif kind == "get_state":
            data = dict(sessionId=session_id, leafId=None)
        elif kind == "get_available_models":
            data = dict(models=[])
        elif kind == "get_available_thinking_levels":
            data = dict(levels=[])
        else:
            raise AssertionError(command)
        emit(dict(type="response", id=command["id"], command=kind,
                  success=success, error=error, data=data))


def alive(pid):
    if os.name == "nt":
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.restype = ctypes.c_void_p
        kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        handle = kernel.OpenProcess(0x100000, False, pid)  # SYNCHRONIZE
        if not handle:
            if ctypes.get_last_error() == 87:  # ERROR_INVALID_PARAMETER (exited PID)
                return False
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            return kernel.WaitForSingleObject(handle, 0) == 258  # WAIT_TIMEOUT
        finally:
            kernel.CloseHandle(handle)
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


@unittest.skipUnless(os.environ.get("RPI_TEST_ACP_BRIDGE"), "set RPI_TEST_ACP_BRIDGE")
class DurableAttachTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rpi-acp-attach-")
        self.root = Path(self.temp.name)
        self.store = self.root / "sessions"
        self.session_id = str(uuid.uuid4())
        directory = self.store / self.session_id / "workspace"
        directory.mkdir(parents=True)
        (directory / f"fixture_{self.session_id}.jsonl").write_text(
            json.dumps(dict(kind="header", version=4, id=self.session_id)) + "\n")
        script = Path(__file__).resolve()
        if os.name == "nt":
            self.launcher = self.root / "backend.cmd"
            self.launcher.write_text(f'@echo off\n"{sys.executable}" "{script}" --backend %*\n')
        else:
            self.launcher = self.root / "backend"
            self.launcher.write_text(
                f'#!/bin/sh\nexec {shlex.quote(sys.executable)} {shlex.quote(str(script))} --backend "$@"\n')
            self.launcher.chmod(0o700)

    def tearDown(self):
        self.temp.cleanup()

    def client(self, scenario):
        client = Client([os.environ["RPI_TEST_ACP_BRIDGE"]], dict(
            os.environ, RPI_BIN=str(self.launcher), RPI_ACP_SESSION_DIR=str(self.store),
            RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_ACP_NO_PERMISSION_GATE="1",
            RPI_ATTACH_ROOT=str(self.root), RPI_ATTACH_SCENARIO=scenario))
        response, _ = client.request("initialize", dict(protocolVersion=1))
        self.assertIn("result", response)
        return client

    def params(self, method):
        params = dict(cwd=str(self.root), mcpServers=[])
        if method != "session/new":
            params["sessionId"] = self.session_id
        return params

    def assert_children_exited(self):
        pids = [int(path.read_text()) for path in self.root.glob("child-*")]
        self.assertTrue(pids, "fixture backend never started")
        deadline = time.monotonic() + 5
        while any(alive(pid) for pid in pids) and time.monotonic() < deadline:
            time.sleep(.02)
        self.assertFalse([pid for pid in pids if alive(pid)], "failed session leaked a backend")

    def test_attach_failures_are_session_errors_and_do_not_leak(self):
        for scenario, expected in [
            ("missing-filtered", "get_session_branch: host must return filtered custom entries"),
            ("false-filtered", "get_session_branch: host must return filtered custom entries"),
            ("rpc-error", "get_session_branch: fixture branch RPC failed"),
            ("corrupt-journal", "durable branch replay:"),
        ]:
            client = self.client(scenario)
            try:
                # Repeat restore to prove failed hosts were not registered/reused.
                for method in ["session/new", "session/load", "session/resume", "session/load"]:
                    with self.subTest(scenario=scenario, method=method):
                        before = len(list(self.root.glob("child-*")))
                        response, updates = client.request(method, self.params(method))
                        self.assertEqual(response["error"]["code"], -32603, response)
                        self.assertTrue(response["error"]["message"].startswith(expected), response)
                        self.assertEqual(updates, [])
                        self.assertEqual(len(list(self.root.glob("child-*"))), before + 1)
                        self.assert_children_exited()  # while ACP transport is still open
            finally:
                client.close()

    def test_only_missing_capabilities_allow_non_durable_sessions(self):
        for scenario in ["no-capability", "healthy"]:
            for method in ["session/new", "session/load", "session/resume"]:
                with self.subTest(scenario=scenario, method=method):
                    client = self.client(scenario)
                    try:
                        response, _ = client.request(method, self.params(method))
                        self.assertIn("result", response)
                        session_id = response["result"].get("sessionId", self.session_id)
                        response, _ = client.request("session/durable", dict(
                            sessionId=session_id, operation="status"))
                        if scenario == "healthy":
                            self.assertIn("result", response)
                        else:
                            self.assertIn("durable_unavailable:", response["error"]["message"])
                    finally:
                        client.close()
                    self.assert_children_exited()


if __name__ == "__main__":
    if "--backend" in sys.argv:
        backend()
    else:
        unittest.main()
