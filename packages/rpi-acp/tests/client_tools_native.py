#!/usr/bin/env python3
"""Real native -> ACP v1 client-tool acceptance, with a loopback OpenAI model.

Usage: python client_tools_native.py ACP_BRIDGE NATIVE_RPI [--artifacts DIRECTORY]
Build candidates separately; this fixture never installs/builds binaries or kills
unrelated processes. Uses only Python's standard library. Artifacts (including
native transcripts) are retained on success and failure. No provider credentials
or network services other than the fixture's loopback server are needed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import tempfile
import uuid


def require(condition, detail):
    if not condition:
        raise AssertionError(detail)


def dump(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False), encoding="utf-8")


def text(value):
    """Extract actual strings, not JSON-escaped newline/Unicode representations."""
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        return "\n".join(text(item) for item in value.values())
    if isinstance(value, list):
        return "\n".join(text(item) for item in value)
    return str(value)


class Fixture:
    def __init__(self, root):
        self.root = root
        self.workspace = root / "workspace"
        self.workspace.mkdir()
        self.read_path = self.workspace / "unsaved.txt"
        self.write_path = self.workspace / "client-created.txt"
        self.read_path.write_text("LOCAL_DECOY_NOT_CLIENT_CONTENT", encoding="utf-8")
        self.read_content = "CLIENT_ONLY_UNSAVED_" + uuid.uuid4().hex + "\nsecond line 🦀\n"
        self.write_content = "CLIENT_WRITE_" + uuid.uuid4().hex + "\n"
        self.terminal_content = "CLIENT_TERMINAL_ONLY_" + uuid.uuid4().hex
        self.error_content = "CLIENT_TERMINAL_WAIT_ERROR_" + uuid.uuid4().hex
        self.buffers = {str(self.read_path): self.read_content}
        self.captures = []
        self.requests = []
        self.frames = []
        self.model_errors = []
        self.session_id = None
        self.terminals = {}
        self.steps = [
            ("read", {"path": str(self.read_path)}, self.read_content),
            ("write", {"path": str(self.write_path), "content": self.write_content}, "Successfully wrote"),
            ("read", {"path": str(self.write_path)}, self.write_content),
            ("bash", {"command": "echo fixture-client-success", "timeout": 10}, self.terminal_content),
            ("bash", {"command": "echo fixture-client-error", "timeout": 10}, self.error_content),
        ]

    def model(self, body):
        stage = len(self.captures)
        self.captures.append(body)
        require(stage <= len(self.steps), "unexpected extra provider request")
        if stage:
            previous = [m for m in body["messages"] if m.get("role") == "tool"
                        and m.get("tool_call_id") == f"client-tool-{stage - 1}"]
            require(len(previous) == 1, f"missing real tool result for stage {stage}: {previous}")
            require(self.steps[stage - 1][2] in text(previous[0]),
                    f"client result absent in subsequent model input at stage {stage}: {previous}")
            require("LOCAL_DECOY_NOT_CLIENT_CONTENT" not in text(previous[0]), "local read leaked")
        if stage == len(self.steps):
            return {"content": "All client tool results received."}, "stop"
        name, arguments, _ = self.steps[stage]
        advertised = {t["function"]["name"] for t in body.get("tools", [])}
        require(name in advertised, f"actual native tool {name} not advertised: {advertised}")
        return {"tool_calls": [{"index": 0, "id": f"client-tool-{stage}", "type": "function",
                                "function": {"name": name, "arguments": json.dumps(arguments)}}]}, "tool_calls"

    def respond(self, frame):
        method, params = frame["method"], frame["params"]
        require(params.get("sessionId") == self.session_id, f"wrong ACP session: {frame}")
        self.requests.append(frame)
        if method.startswith("fs/"):
            path = params["path"]
            require(Path(path).is_absolute(), f"nonabsolute client path: {path}")
            if method == "fs/read_text_file":
                require(path in self.buffers, f"unexpected client read {path}")
                return {"content": self.buffers[path]}, None
            if method == "fs/write_text_file":
                require(path == str(self.write_path), f"unexpected client write {path}")
                require(params["content"] == self.write_content, "client write content differs")
                self.buffers[path] = params["content"]
                return {}, None
        if method == "terminal/create":
            require(Path(params["cwd"]).resolve() == self.workspace.resolve(), f"terminal cwd: {params}")
            require(params["command"] == "bash", f"unexpected terminal command: {params}")
            arguments = params.get("args", [])
            require(arguments in (["-c", "echo fixture-client-success"],
                                  ["-c", "echo fixture-client-error"]), f"terminal args: {params}")
            tid = "fixture-terminal-" + str(len(self.terminals))
            self.terminals[tid] = {"error": arguments[-1].endswith("error"), "methods": [method]}
            return {"terminalId": tid}, None
        if method.startswith("terminal/"):
            tid = params["terminalId"]
            require(tid in self.terminals, f"unknown terminal: {frame}")
            terminal = self.terminals[tid]
            terminal["methods"].append(method)
            if method == "terminal/wait_for_exit":
                if terminal["error"]:
                    return None, {"code": -32603, "message": self.error_content}
                return {"exitCode": 0}, None
            if method == "terminal/output":
                require(not terminal["error"], "output requested after wait failure")
                return {"output": self.terminal_content, "truncated": False,
                        "exitStatus": {"exitCode": 0}}, None
            if method in ("terminal/kill", "terminal/release"):
                return {}, None
        raise AssertionError(f"unexpected agent request (no diagnostic substitute): {frame}")


class Client:
    def __init__(self, bridge, env, fixture):
        self.fixture = fixture
        self.stderr_file = (fixture.root / "acp-stderr.log").open("w", encoding="utf-8")
        self.p = subprocess.Popen([str(bridge)], cwd=fixture.workspace, env=env,
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=self.stderr_file, text=True, encoding="utf-8")
        self.q = queue.Queue()
        self.seq = 0
        def reader():
            for line in self.p.stdout:
                try:
                    self.q.put(json.loads(line))
                except ValueError:
                    self.q.put({"invalid_stdout": line})
            self.q.put({"eof": True})
        self.reader = threading.Thread(target=reader, daemon=True)
        self.reader.start()

    def send(self, value):
        self.p.stdin.write(json.dumps(value) + "\n")
        self.p.stdin.flush()

    def request(self, method, params, timeout=90):
        self.seq += 1
        request_id = self.seq
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + timeout
        while True:
            frame = self.q.get(timeout=max(0.01, deadline - time.monotonic()))
            self.fixture.frames.append(frame)
            require(not frame.get("eof") and "invalid_stdout" not in frame, f"invalid/closed ACP: {frame}")
            if frame.get("method") and "id" in frame:
                result, error = self.fixture.respond(frame)
                self.send({"jsonrpc": "2.0", "id": frame["id"],
                           **({"error": error} if error else {"result": result})})
            elif frame.get("id") == request_id:
                require("result" in frame, f"ACP {method} failed: {frame}")
                return frame["result"]
            require(time.monotonic() < deadline, f"ACP {method} timed out")

    def close(self):
        try:
            if self.p.poll() is None:
                self.p.stdin.close()
                try:
                    self.p.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    # Only the exact bridge process created by this fixture.
                    self.p.kill()
                    self.p.wait(timeout=10)
                    raise AssertionError("fixture ACP bridge did not shut down on EOF")
            require(self.p.returncode == 0, f"bridge exit {self.p.returncode}; see acp-stderr.log")
        finally:
            self.stderr_file.close()


def run(bridge, native, f):
    class Model(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            try:
                require(self.path == "/v1/chat/completions", f"unexpected model endpoint {self.path}")
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                delta, finish = f.model(body)
                events = [{"choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": None}]},
                          {"id": "client-fixture", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                          {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}]
                data = ("".join("data: " + json.dumps(e) + "\n\n" for e in events) + "data: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except Exception:
                f.model_errors.append(traceback.format_exc())
                self.send_error(500, "fixture assertion failed")

    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        agent = f.root / "agent"
        (agent / "extensions").mkdir(parents=True)
        dump(agent / "models.json", {"providers": {"acceptance-local": {
            "api": "openai-completions", "baseUrl": f"http://127.0.0.1:{server.server_port}/v1",
            "apiKey": "local-only", "models": [{"id": "fixture", "name": "fixture", "reasoning": False,
            "input": ["text"], "contextWindow": 1000000, "maxTokens": 1024}]}}})
        dump(agent / "settings.json", {"defaultProvider": "acceptance-local", "defaultModel": "fixture",
                                       "defaultThinkingLevel": "off"})
        env = {k: v for k, v in os.environ.items() if not k.endswith(("_API_KEY", "_AUTH_TOKEN"))
               and not k.startswith("RPI_")}
        env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(agent / "extensions"),
                   RPI_BIN=str(native), RPI_ACP_SESSION_DIR=str(f.root / "sessions"),
                   RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_ACP_NO_PERMISSION_GATE="1",
                   RPI_OFFLINE="1", HOME=str(f.root), USERPROFILE=str(f.root))
        client = Client(bridge, env, f)
        try:
            initialized = client.request("initialize", {"protocolVersion": 1, "clientCapabilities": {
                "fs": {"readTextFile": True, "writeTextFile": True}, "terminal": True}})
            require(initialized["protocolVersion"] == 1, f"not ACP v1: {initialized}")
            created = client.request("session/new", {"cwd": str(f.workspace), "mcpServers": []})
            f.session_id = created["sessionId"]
            result = client.request("session/prompt", {"sessionId": f.session_id,
                "prompt": [{"type": "text", "text": "Run the client tool acceptance sequence."}]})
            require(result.get("stopReason") == "end_turn", f"prompt did not complete: {result}")
            require(not f.model_errors, f"mock provider assertions: {f.model_errors}")
            require(len(f.captures) == 6, f"expected 5 real tool calls + final model input, got {len(f.captures)}")
            require(f.buffers[str(f.write_path)] == f.write_content, "client write not applied")
            require(not f.write_path.exists(), "write fell back to local disk")
            require(f.read_path.read_text(encoding="utf-8") == "LOCAL_DECOY_NOT_CLIENT_CONTENT", "local decoy changed")
            require(len(f.terminals) == 2, f"expected two client terminals: {f.terminals}")
            for terminal in f.terminals.values():
                expected = (["terminal/create", "terminal/wait_for_exit", "terminal/kill", "terminal/release"]
                            if terminal["error"] else ["terminal/create", "terminal/wait_for_exit",
                                                       "terminal/output", "terminal/release"])
                require(terminal["methods"] == expected, f"incorrect terminal ownership cleanup: {terminal}")
            client.request("session/close", {"sessionId": f.session_id})
        finally:
            client.close()
        paths = list((f.root / "sessions").rglob("*.jsonl"))
        require(len(paths) == 1, f"expected one native transcript: {paths}")
        entries = [json.loads(line) for line in paths[0].read_text(encoding="utf-8").splitlines() if line.strip()]
        messages = [e["message"] for e in entries if e.get("type") == "message"]
        results = [m for m in messages if m.get("role") == "toolResult"]
        for index, (name, _, marker) in enumerate(f.steps):
            matching = [m for m in results if m.get("toolCallId") == f"client-tool-{index}"]
            require(len(matching) == 1, f"native transcript missing {name} result {index}: {results}")
            require(marker in text(matching[0]), f"native transcript lacks client marker: {matching}")
            require(bool(matching[0].get("isError")) == (index == 4), f"wrong native tool result status: {matching}")
        require("LOCAL_DECOY_NOT_CLIENT_CONTENT" not in text(results), "native transcript contains local read result")
        return {"result": "passed", "model_calls": len(f.captures), "native_tool_results": len(results),
                "client_methods": [r["method"] for r in f.requests], "terminal_error_kill_release": True,
                "transcript": str(paths[0])}
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bridge", type=Path)
    parser.add_argument("native", type=Path)
    parser.add_argument("--artifacts", type=Path)
    args = parser.parse_args()
    root = (args.artifacts or Path(tempfile.mkdtemp(prefix="rpi-acp-client-tools-native-"))).resolve()
    root.mkdir(parents=True, exist_ok=True)
    f = Fixture(root)
    report = {"result": "failed", "artifacts": str(root)}
    try:
        binaries = {}
        for name, path in (("bridge", args.bridge.resolve()), ("native", args.native.resolve())):
            require(path.is_file(), f"missing {name} candidate: {path}")
            binaries[name] = {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                              "mtime_ns": path.stat().st_mtime_ns}
        report["binaries"] = binaries
        report.update(run(args.bridge.resolve(), args.native.resolve(), f))
    except Exception:
        report["error"] = traceback.format_exc()
    finally:
        dump(root / "model-inputs.json", f.captures)
        dump(root / "acp-frames.json", f.frames)
        dump(root / "client-requests.json", f.requests)
        dump(root / "model-errors.json", f.model_errors)
        dump(root / "result.json", report)
    print(json.dumps(report, indent=2))
    return 0 if report["result"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
