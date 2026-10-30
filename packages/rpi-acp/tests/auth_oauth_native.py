#!/usr/bin/env python3
"""Native provider-extension OAuth E2E against a synthetic loopback server.

Not external-provider proof. Requires frozen --bridge, --native and --plugin.
Uses no installed configuration, inherited credentials, or third-party network.
Only native auth.json may contain generated tokens; temporary homes are removed.
"""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import queue
import secrets
import shutil
import subprocess
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlsplit
from urllib.request import build_opener, ProxyHandler

from auth_native import AGENT, REV

PROVIDER = "stub-oauth-test"
OPENER = build_opener(ProxyHandler({}))


class OAuthServer:
    def __init__(self, fail_token=False):
        self.state = secrets.token_hex(20)
        self.code = "local-code-" + secrets.token_hex(24)
        self.access = "local-access-" + secrets.token_hex(24)
        self.refresh = "local-refresh-" + secrets.token_hex(24)
        self.private = "private-payload-" + secrets.token_hex(24)
        self.expires = int(time.time()) + 3600
        self.events = []
        self.authorized = self.used = False
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass  # Never log URLs, request bodies or tokens.

            def reply(self, status, value):
                body = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                url = urlsplit(self.path)
                if url.path == "/begin":
                    owner.events.append("begin")
                    self.reply(200, {"state": owner.state, "url": owner.base +
                                    "/authorize?state=" + owner.state, "private": owner.private})
                elif url.path == "/authorize" and parse_qs(url.query) == {"state": [owner.state]}:
                    owner.events.append("authorize")
                    owner.authorized = True
                    self.reply(200, {"code": owner.code, "state": owner.state})
                else:
                    self.reply(400, {"error": "invalid_state"})

            def do_POST(self):
                n = int(self.headers.get("Content-Length", "0"))
                value = json.loads(self.rfile.read(n)) if 0 < n <= 65536 else {}
                if self.path == "/revoke":
                    owner.events.append("revoke")
                    valid = (owner.used and value == {"credential": {
                        "access": owner.access, "refresh": owner.refresh}})
                    if valid:
                        self.reply(200, {"version": 1, "kind": "success", "payload": {}})
                    else:
                        self.reply(400, {"error": "invalid_credential"})
                    return
                owner.events.append("token")
                valid = (self.path == "/token" and owner.authorized and not owner.used and
                         value == {"grant_type": "authorization_code", "code": owner.code,
                                   "state": owner.state})
                if fail_token or not valid:
                    # Include generated secrets in a private server error: no
                    # provider/host/bridge diagnostic may forward these bytes.
                    self.reply(400, {"error": "invalid_grant", "private": owner.private,
                                     "access_token": owner.access, "refresh_token": owner.refresh})
                else:
                    owner.used = True
                    self.reply(200, {"access_token": owner.access, "refresh_token": owner.refresh,
                                     "expires_at": owner.expires})
        self.http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = "http://127.0.0.1:" + str(self.http.server_port)
        self.thread = threading.Thread(target=self.http.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.http.shutdown()
        self.http.server_close()
        self.thread.join(timeout=5)


class Client:
    def __init__(self, bridge, root, env, version):
        self.version, self.frames, self.errors = version, [], []
        self.q = queue.Queue()
        self.p = subprocess.Popen([str(bridge), "--provider", PROVIDER], cwd=root, env=env,
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  text=True, encoding="utf-8")
        def read():
            try:
                for line in self.p.stdout:
                    frame = json.loads(line)
                    self.frames.append(frame)
                    self.q.put(frame)
            except Exception:
                self.q.put({"reader_failed": True})
        self.reader = threading.Thread(target=read, daemon=True)
        self.err_reader = threading.Thread(target=lambda: self.errors.extend(self.p.stderr), daemon=True)
        self.reader.start()
        self.err_reader.start()

    def send(self, value):
        self.p.stdin.write(json.dumps(dict(jsonrpc="2.0", **value)) + "\n")
        self.p.stdin.flush()

    def recv(self):
        try:
            return self.q.get(timeout=20)
        except queue.Empty:
            raise AssertionError("timed out awaiting ACP frame") from None

    def request(self, method, ident, params):
        self.send(dict(method=method, id=ident, params=params))
        frame = self.recv()
        assert frame.get("id") == ident and "result" in frame, "request failed: " + method
        return frame["result"]

    def respond(self, prompt, action="accept", content=None):
        result = {"action": action}
        if content is not None:
            result["content"] = content
        self.send(dict(id=prompt["id"], result=result))

    def prompt(self, mode):
        frame = self.recv()
        assert frame.get("method") == "elicitation/create", "missing " + mode + " prompt"
        params = frame["params"]
        assert params["mode"] == mode and params["requestId"] == "oauth-parent"
        assert "sessionId" not in params and "toolCallId" not in params
        if mode == "form":
            assert params["requestedSchema"] == {
                "type": "object", "properties": {
                    "code": {"type": "string", "password": True, "minLength": 1},
                    "state": {"type": "string", "minLength": 1}},
                "required": ["code", "state"], "additionalProperties": False}
        return frame

    def finish(self, success, url_prompt):
        completions = []
        while True:
            frame = self.recv()
            if frame.get("method") == "elicitation/complete":
                completions.append(frame)
                assert frame["params"]["requestId"] == "oauth-parent"
                assert frame["params"]["elicitationId"] == url_prompt["params"]["elicitationId"]
            else:
                assert frame.get("id") == "oauth-parent", "unexpected authentication frame"
                assert ("result" in frame) == success, "authentication result mismatch"
                if success:
                    assert frame["result"] == {}
                assert len(completions) == int(success), "URL completion before credentials or missing"
                return

    def close(self):
        if self.p.poll() is None:
            self.p.stdin.close()
            try:
                self.p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.p.kill()
                self.p.wait(timeout=5)
        self.reader.join(timeout=3)
        self.err_reader.join(timeout=3)
        assert self.p.returncode == 0, "bridge did not shut down cleanly"
        assert self.q.empty(), "unexpected frames after shutdown"


def run(bridge, native, plugin, version, case):
    server = OAuthServer(fail_token=case == "token-error")
    try:
        with tempfile.TemporaryDirectory(prefix="acp-oauth-native-") as tmp:
            root = Path(tmp)
            agent = root / "agent"
            extensions = agent / "extensions"
            extensions.mkdir(parents=True)
            shutil.copy2(plugin, extensions / plugin.name)
            env = {k: v for k, v in os.environ.items() if k.upper() in {
                "SYSTEMROOT", "WINDIR", "PATH", "PATHEXT", "TEMP", "TMP", "COMSPEC"}}
            env.update(RPI_BIN=str(native), HOME=tmp, USERPROFILE=tmp,
                       RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(extensions),
                       RPI_ACP_SESSION_DIR=str(root / "sessions"), RPI_OFFLINE="1",
                       RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_STUB_OAUTH_TEST="1",
                       RPI_STUB_OAUTH_ADDRESS=f"127.0.0.1:{server.http.server_port}")
            c = Client(bridge, root, env, version)
            diagnostics = []
            def native_command(*args, ready=None):
                result = subprocess.run([str(native), "auth", *args, "--provider", PROVIDER],
                                        cwd=root, env=env, capture_output=True, text=True, timeout=20)
                diagnostics.extend([result.stdout, result.stderr])
                if ready is not None:
                    value = json.loads(result.stdout.strip().splitlines()[-1])
                    assert value["ready"] is ready and (result.returncode == 0) == ready
                return result

            def disk_check(saved):
                expected = [agent / "auth.json"] if saved else []
                for secret in [server.access, server.refresh]:
                    found = [p for p in root.rglob("*") if p.is_file() and secret.encode() in p.read_bytes()]
                    assert found == expected, "token persisted outside native auth.json or wrong state"
                for secret in [server.code, server.private]:
                    assert not any(secret.encode() in p.read_bytes() for p in root.rglob("*") if p.is_file()), "transient secret persisted"
                auth_path = agent / "auth.json"
                auth = json.loads(auth_path.read_text()) if auth_path.exists() else {}
                if saved:
                    assert auth[PROVIDER] == {"type": "oauth", "access": server.access,
                                              "refresh": server.refresh, "expires": server.expires}, "native credential envelope mismatch"
                else:
                    assert PROVIDER not in auth, "credential unexpectedly persisted"

            def status(ready, ident):
                assert c.request("auth/status", ident, {}) == {"authenticated": ready}
                native_command("check", "--json", ready=ready)
                disk_check(ready)

            try:
                # Registration must be explicitly gated, with the same real DLL.
                disabled = dict(env)
                disabled.pop("RPI_STUB_OAUTH_TEST")
                probe = subprocess.run([str(native), "auth", "describe", "--provider", PROVIDER, "--json"],
                                       cwd=root, env=disabled, capture_output=True, text=True, timeout=20)
                diagnostics.extend([probe.stdout, probe.stderr])
                assert probe.returncode == 0 and json.loads(probe.stdout.strip().splitlines()[-1])["kind"] == "api_key"
                probe = native_command("describe", "--json")
                assert probe.returncode == 0 and json.loads(probe.stdout.strip().splitlines()[-1])["kind"] == "oauth"
                caps = {"elicitation": {"url": {}, "form": {}},
                        "_meta": {"rpi/unstable-session-rfds": REV}}
                params = {"protocolVersion": version,
                          "clientCapabilities" if version == 1 else "capabilities": caps}
                if version == 2:
                    params["info"] = {"name": "native-oauth-test", "version": "1"}
                initialized = c.request("initialize", 1, params)
                key = "id" if version == 1 else "methodId"
                assert any(m.get(key) == AGENT and m["type"] == "agent" for m in initialized["authMethods"])
                status(False, 2)
                c.send(dict(method="authenticate" if version == 1 else "auth/login",
                            id="oauth-parent", params={"methodId": AGENT}))
                url_prompt = c.prompt("url")
                assert url_prompt["params"]["url"] == server.base + "/authorize?state=" + server.state
                assert server.events == ["begin"]
                disk_check(False)
                at_url = case.endswith("-url")
                prompt = url_prompt
                if not at_url:
                    with OPENER.open(url_prompt["params"]["url"], timeout=5) as response:
                        authorization = json.load(response)
                    c.respond(url_prompt)
                    prompt = c.prompt("form")
                    # URL acceptance must not complete login, exchange a token,
                    # persist credentials, or claim authenticated status.
                    assert server.events == ["begin", "authorize"] and not server.used
                    status(False, 3)
                if case.startswith("disconnect"):
                    c.close()
                elif case.startswith("parent-cancel"):
                    c.send(dict(method="$/cancel_request", params={"requestId": "oauth-parent"}))
                    cancelled = c.recv()
                    assert cancelled.get("method") == "$/cancel_request"
                    assert cancelled["params"] == {"requestId": prompt["id"]}
                    result = c.recv()
                    assert result.get("id") == "oauth-parent" and result["error"]["code"] == -32800
                    # A late response must not resume exchange or save a token.
                    c.respond(prompt, content={"code": server.code, "state": server.state} if not at_url else None)
                    status(False, 4)
                else:
                    if case.startswith("decline") or case.startswith("cancel"):
                        c.respond(prompt, "decline" if case.startswith("decline") else "cancel")
                    else:
                        content = dict(authorization)
                        if case == "invalid-state":
                            content["state"] = "invalid-" + server.state
                        if case == "invalid-code":
                            content["code"] = "invalid-" + server.code
                        c.respond(prompt, content=content)
                    success = case == "success"
                    c.finish(success, url_prompt)
                    status(success, 4)
                    if success:
                        assert server.events == ["begin", "authorize", "token"] and server.used
                        c.request("logout" if version == 1 else "auth/logout", 5, {})
                        status(False, 6)
                        assert server.events == ["begin", "authorize", "token", "revoke"], "missing native token revocation"
                if case != "success":
                    assert not server.used, "failed/cancelled flow consumed token"
                    expected = ["begin"] if at_url else ["begin", "authorize"]
                    if case in ("invalid-code", "token-error"):
                        expected.append("token")
                    assert server.events == expected, "unexpected server exchanges"
                    native_command("check", "--json", ready=False)
                    disk_check(False)
            finally:
                # Inspect before independent cleanup, so cleanup cannot hide a
                # persistence bug. Still always remove native credentials/child.
                try:
                    c.close()
                finally:
                    assert native_command("logout").returncode == 0, "native cleanup failed"
                    disk_check(False)
                    after_cleanup = list(server.events)
                    assert native_command("logout").returncode == 0, "repeated native cleanup failed"
                    assert server.events == after_cleanup, "empty logout attempted revocation"
                    # frames contains bridge stdout only, never client send() input:
                    # the typed code must be sent, but must not be echoed publicly.
                    public = json.dumps(c.frames) + "".join(c.errors) + "".join(diagnostics)
                    assert all(secret not in public for secret in [server.code, server.access,
                                                                  server.refresh, server.private]), "secret disclosed"
                    disk_check(False)
    finally:
        server.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("bridge", "native", "plugin"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    bridge, native, plugin = (getattr(args, name).resolve() for name in ("bridge", "native", "plugin"))
    for path in (bridge, native, plugin):
        assert path.is_file(), "frozen binary missing"
    cases = ["success", "decline-url", "cancel-url", "decline-form", "cancel-form",
             "invalid-state", "invalid-code", "token-error", "parent-cancel-url",
             "parent-cancel-form", "disconnect-url", "disconnect-form"]
    for version in (1, 2):
        for case in cases:
            run(bridge, native, plugin, version, case)
            print(f"PASS native OAuth ACP v{version}: {case}", flush=True)
    for name, path in (("bridge", bridge), ("native", native), ("plugin", plugin)):
        print(f"{name}: {path} sha256={hashlib.sha256(path.read_bytes()).hexdigest()}")
    print("PASS local native host/provider extension OAuth network fixture; NOT external provider proof")


if __name__ == "__main__":
    main()
