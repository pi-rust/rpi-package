"""Integration check with a real rpi backend, without making model calls.

python tests/restart.py --bridge /path/rpi-acp-bridge --rpi /path/rpi
Seeds a native user-message fixture only while the backend is stopped, then
checks that two fresh ACP processes restore it. Uses an isolated temp store.
"""
import argparse
import json
import os
import queue
import subprocess
import tempfile
import threading
import time
import uuid
from pathlib import Path


def fixture_environment(root):
    """Give native frontends a deterministic model without user configuration."""
    agent = root / "agent"
    extensions = agent / "extensions"
    extensions.mkdir(parents=True, exist_ok=True)
    (agent / "models.json").write_text(json.dumps({"providers": {"acp-fixture": {
        "api": "openai-completions", "baseUrl": "http://127.0.0.1:1/v1", "apiKey": "offline-fixture",
        "models": [{"id": "fixture", "name": "fixture", "reasoning": False,
                    "input": ["text"], "contextWindow": 8192, "maxTokens": 1024}],
    }}}), encoding="utf-8")
    (agent / "settings.json").write_text(json.dumps({"defaultProvider": "acp-fixture",
        "defaultModel": "fixture", "defaultThinkingLevel": "off"}), encoding="utf-8")
    return dict(RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(extensions),
                RPI_PROVIDER="acp-fixture", RPI_MODEL="fixture", RPI_OFFLINE="1")


class Client:
    def __init__(self, command, env):
        self.output = queue.Queue()
        self.process = subprocess.Popen(command, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding="utf-8")
        self.errors = []
        self.readers = [
            threading.Thread(target=self.read, args=(self.process.stdout,), daemon=True),
            threading.Thread(target=lambda: self.errors.extend(self.process.stderr), daemon=True),
        ]
        for reader in self.readers:
            reader.start()
        self.next_id = 0

    def read(self, stream):
        for line in stream:
            self.output.put(json.loads(line))

    def request(self, method, params):
        self.next_id += 1
        request_id = self.next_id
        self.process.stdin.write(json.dumps(dict(jsonrpc="2.0", id=request_id,
                                                method=method, params=params)) + "\n")
        self.process.stdin.flush()
        updates = []
        deadline = time.monotonic() + 25
        while True:
            try:
                message = self.output.get(timeout=max(.01, deadline-time.monotonic()))
            except queue.Empty:
                raise AssertionError("ACP response timed out: " + "".join(self.errors))
            if message.get("id") == request_id:
                return message, updates
            updates.append(message)

    def close(self):
        self.process.stdin.close()
        assert self.process.wait(timeout=10) == 0, "".join(self.errors)
        # A Windows .cmd backend can outlive the wrapper briefly. Wait for its
        # inherited pipe handles to close before removing the temporary cwd.
        for reader in self.readers:
            reader.join(timeout=10)
            assert not reader.is_alive(), "ACP subprocess did not close its output pipes"
        self.process.stdout.close()
        self.process.stderr.close()


def check(command, env, cwd, store):
    first = Client(command, env)
    try:
        result, _ = first.request("initialize", {"protocolVersion": 1})
        assert result["result"]["agentCapabilities"]["loadSession"] is True
        result, _ = first.request("session/new", {"cwd": cwd, "mcpServers": []})
        assert "error" not in result, (result, "".join(first.errors))
        config_options = result["result"].get("configOptions", [])
        model = next((option for option in config_options if option.get("id") == "model"), None)
        assert model is not None, (result, "".join(first.errors))
        values = []
        for option in model["options"]:
            values.extend(item["value"] for item in option.get("options", [option]))
        assert "acp-fixture/fixture" in values, (result, "".join(first.errors))
        session_id = result["result"]["sessionId"]
        uuid.UUID(session_id)
        switched, _ = first.request("session/set_config_option", {
            "sessionId": session_id, "configId": "model", "value": "acp-fixture/fixture"
        })
        assert "error" not in switched, (switched, "".join(first.errors))
    finally:
        first.close()

    files = list((store / session_id).rglob("*.jsonl"))
    assert len(files) == 1, files
    path = files[0]
    entries = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
    seq = max((entry.get("seq", 0) for entry in entries), default=0) + 1
    leaf = None
    for entry in entries:
        if entry.get("kind") == "entry" and entry.get("lane") == "main":
            leaf = entry["id"]
        elif entry.get("kind") == "lane" and entry.get("lane") == "main":
            leaf = entry.get("leafId")
    message = dict(kind="entry", lane="main", type="message", id=str(uuid.uuid4()),
                   seq=seq, parentId=leaf, timestamp=int(time.time()*1000),
                   message=dict(kind="user", role="user", content="ACP_RESTART_HISTORY", timestamp=0))
    with path.open("a", encoding="utf-8") as output:
        output.write(json.dumps(message) + "\n")

    for method in ("session/load", "session/resume"):
        client = Client(command, env)
        try:
            client.request("initialize", {"protocolVersion": 1})
            result, updates = client.request(method, {"sessionId": session_id, "cwd": cwd, "mcpServers": []})
            # load/resume also return the session's configOptions (model + thinking).
            assert "configOptions" in result.get("result", {}), (result, "".join(client.errors))
            if method == "session/load":
                assert any(item.get("params", {}).get("update", {}).get("content", {}).get("text")
                           == "ACP_RESTART_HISTORY" for item in updates), updates
            else:
                assert updates == [], updates
            result, _ = client.request("session/load", {"sessionId": "rpi-old-1", "cwd": cwd})
            assert "error" in result, result
            result, _ = client.request("session/load", {"sessionId": str(uuid.uuid4()), "cwd": cwd})
            assert "error" in result, result
        finally:
            client.close()
    print("PASS: native session survives process exit; load replays history; resume skips replay; missing sessions fail")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--bridge", required=True)
    parser.add_argument("--rpi", required=True)
    parser.add_argument("--extension", help="Also test the rpi --acp extension frontend")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="rpi-acp-restart-") as directory:
        root = Path(directory)
        env = os.environ.copy()
        env.update(fixture_environment(root))
        env.update(RPI_BIN=args.rpi, RPI_ACP_SESSION_DIR=str(root / "sessions"),
                   RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_ACP_NO_PERMISSION_GATE="1")
        check([args.bridge], env, str(root), root / "sessions")
        if args.extension:
            check([args.rpi, "--acp", "--no-session", "--local-only", "--extension", args.extension],
                  env, str(root), root / "sessions")
