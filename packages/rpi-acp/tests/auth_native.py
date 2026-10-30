#!/usr/bin/env python3
"""Real native API-key ACP acceptance. No network or external credentials.

Build bridge/native first. --bridge and --native must name frozen binaries.
Only the native auth.json may persist the generated dummy secret; all homes,
config and transcript paths are temporary and removed even on failure.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import secrets
import subprocess
import tempfile
import threading

REV = "1142c83bbfa9407f9f264d1e290ffafcd54eb79e"
AGENT = "rpi-agent-credentials"


def run(bridge, native, version):
    with tempfile.TemporaryDirectory(prefix="acp-auth-native-") as tmp:
        root = Path(tmp)
        agent = root / "agent"
        (agent / "extensions").mkdir(parents=True)
        # Allow only OS/process necessities: never inherit real provider keys,
        # extension paths, native config, or a developer's credential home.
        env = {k: v for k, v in os.environ.items() if k.upper() in {
            "SYSTEMROOT", "WINDIR", "PATH", "PATHEXT", "TEMP", "TMP", "COMSPEC"}}
        env.update(RPI_BIN=str(native), HOME=tmp, USERPROFILE=tmp,
                   RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(agent / "extensions"),
                   RPI_ACP_SESSION_DIR=str(root / "sessions"), RPI_OFFLINE="1",
                   RPI_ACP_NO_AUTO_REGISTER_ZED="1")
        p = subprocess.Popen([str(bridge), "--provider", "openai"], cwd=root, env=env,
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             text=True, encoding="utf-8")
        output, frames, errors = queue.Queue(), [], []
        def read():
            for line in p.stdout:
                frame = json.loads(line)
                frames.append(frame)
                output.put(frame)
        reader = threading.Thread(target=read, daemon=True)
        reader.start()
        err_reader = threading.Thread(target=lambda: errors.extend(p.stderr), daemon=True)
        err_reader.start()
        secret = "acp-dummy-" + secrets.token_hex(24)
        def send(value):
            p.stdin.write(json.dumps(dict(jsonrpc="2.0", **value)) + "\n")
            p.stdin.flush()
        def request(method, ident, params):
            send(dict(method=method, id=ident, params=params))
            frame = output.get(timeout=15)
            assert frame.get("id") == ident and "result" in frame, "request failed: " + method
            return frame["result"]
        def check(ready):
            result = subprocess.run([str(native), "auth", "check", "--provider", "openai", "--json"],
                                    cwd=root, env=env, capture_output=True, text=True, timeout=15)
            assert secret not in result.stdout + result.stderr, "native check disclosed secret"
            value = json.loads(result.stdout.strip().splitlines()[-1])
            assert value["ready"] is ready, "native check state mismatch"
            assert (result.returncode == 0) == ready, "native check exit mismatch"
        def disk_check(expect_secret):
            found = []
            for path in root.rglob("*"):
                if path.is_file() and secret.encode() in path.read_bytes():
                    found.append(path)
            assert found == ([agent / "auth.json"] if expect_secret else []), "secret outside native auth.json"
        try:
            caps = {"elicitation": {"form": {}}, "auth": {"terminal": {}},
                    "_meta": {"rpi/unstable-session-rfds": REV}}
            params = {"protocolVersion": version}
            params["clientCapabilities" if version == 1 else "capabilities"] = caps
            if version == 2:
                params["info"] = {"name": "native-auth-test", "version": "1"}
            result = request("initialize", 1, params)
            methods = result["authMethods"]
            method_key = "id" if version == 1 else "methodId"
            assert any(m.get(method_key) == AGENT and m["type"] == "agent" for m in methods)
            assert any(m["type"] == "terminal" for m in methods), "terminal method lost"
            assert result["agentCapabilities" if version == 1 else "capabilities"]["auth"]["status"] is True
            assert request("auth/status", 2, {}) == {"authenticated": False}
            check(False)
            parent = "native-api-key-parent"
            send(dict(method="authenticate" if version == 1 else "auth/login", id=parent,
                      params={"methodId": AGENT}))
            form = output.get(timeout=15)
            assert form.get("method") == "elicitation/create", "native did not request API key"
            params = form["params"]
            assert params["requestId"] == parent and params["mode"] == "form"
            assert "sessionId" not in params and "toolCallId" not in params
            assert "apiKey" in params["requestedSchema"]["properties"]
            send(dict(id=form["id"], result={"action": "accept", "content": {"apiKey": secret}}))
            done = output.get(timeout=15)
            assert done.get("id") == parent and done.get("result") == {}, "native auth failed"
            check(True)
            assert request("auth/status", 3, {}) == {"authenticated": True}
            disk_check(True)
            request("logout" if version == 1 else "auth/logout", 4, {})
            check(False)
            assert request("auth/status", 5, {}) == {"authenticated": False}
            disk_check(False)
            assert not any(f.get("method") == "elicitation/complete" for f in frames), "form emitted URL completion"
        finally:
            # Independent native cleanup also runs if a protocol assertion fails.
            cleanup = subprocess.run([str(native), "auth", "logout", "--provider", "openai"],
                                     cwd=root, env=env, capture_output=True, text=True, timeout=15)
            if p.poll() is None:
                p.stdin.close()
                try:
                    p.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    p.kill(); p.wait(timeout=5)
            reader.join(timeout=2); err_reader.join(timeout=2)
            assert secret not in json.dumps(frames) + "".join(errors) + cleanup.stdout + cleanup.stderr, "secret disclosed"
            disk_check(False)
        assert p.returncode == 0, "bridge exit failure"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", type=Path, required=True)
    parser.add_argument("--native", type=Path, required=True)
    args = parser.parse_args()
    bridge, native = args.bridge.resolve(), args.native.resolve()
    for version in (1, 2):
        run(bridge, native, version)
        print(f"PASS native API-key ACP v{version}: login, status/check, auth.json-only persistence, redaction, logout")
    for name, path in (("bridge", bridge), ("native", native)):
        print(f"{name}: {path} sha256={hashlib.sha256(path.read_bytes()).hexdigest()}")
    print("OAuth extension begin/exchange: NOT VERIFIED by this API-key test")


if __name__ == "__main__":
    main()
