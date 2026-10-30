"""Exercise the real todo DLL via native tool execution and restart recovery.

python packages/rpi-todo/tests/native_todo_smoke.py --rpi /path/rpi --extension /path/rpi_todo.dll
Only a loopback deterministic model fixture is used; no external model calls.
All native children, HTTP threads, profiles and sessions are test-owned.
"""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading


class Rpc:
    def __init__(self, command, env, cwd):
        self.output = queue.Queue()
        self.errors = []
        self.process = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding="utf-8")
        self.readers = [threading.Thread(target=self.read, daemon=True), threading.Thread(
            target=lambda: self.errors.extend(self.process.stderr), daemon=True)]
        for reader in self.readers:
            reader.start()
        try:
            assert self.output.get(timeout=20)["type"] == "ready"
        except BaseException:
            self.close(check=False)
            raise
        self.next_id = 0

    def read(self):
        for line in self.process.stdout:
            self.output.put(json.loads(line))

    def call(self, kind, **params):
        self.next_id += 1
        key = str(self.next_id)
        self.process.stdin.write(json.dumps(dict(type=kind, id=key, **params)) + "\n")
        self.process.stdin.flush()
        while True:
            event = self.output.get(timeout=30)
            if event.get("type") == "response" and event.get("id") == key:
                assert event["success"], event
                return event["data"]

    def todo(self, **params):
        accepted = self.call("prompt", message="todo-fixture:" + json.dumps(params))
        assert accepted["accepted"], accepted
        results = []
        while True:
            event = self.output.get(timeout=30)
            if event["type"] == "tool_execution_end":
                assert event["toolName"] == "todo", event
                assert not event["isError"], event
                results.append(event["result"])
            elif event["type"] == "agent_settled":
                assert event["outcome"] == "completed", (event, self.errors)
                assert len(results) == 1, results
                return results[0]["details"]

    def snapshots(self):
        result = self.call("get_session_branch", customTypes=["todo-state"])
        assert result["filtered"] is True, result
        return result["entries"]

    def close(self, check=True):
        try:
            self.process.stdin.close()
            self.process.wait(timeout=15)
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            for reader in self.readers:
                reader.join(timeout=5)
            self.process.stdout.close()
            self.process.stderr.close()
        if check:
            assert self.process.returncode == 0, "".join(self.errors)


def smoke(rpi, extension):
    requests = []
    provider_errors = []

    class Model(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(body)
                last = body["messages"][-1]
                is_call = last["role"] == "user"
                if is_call:
                    content = last["content"]
                    if isinstance(content, list):
                        content = "".join(part.get("text", "") for part in content)
                    assert content.startswith("todo-fixture:"), last
                    arguments = json.loads(content.removeprefix("todo-fixture:"))
                    assert any(tool["function"]["name"] == "todo" for tool in body["tools"]), body
                    delta = {"tool_calls": [{"index": 0, "id": f"todo-{len(requests)}",
                        "type": "function", "function": {"name": "todo", "arguments": json.dumps(arguments)}}]}
                else:
                    assert last["role"] == "tool", last
                    delta = {"content": "Fixture completed."}
                events = [
                    {"choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": None}]},
                    {"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                    {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if is_call else "stop"}]},
                ]
                data = ("".join("data: " + json.dumps(event) + "\n\n" for event in events)
                        + "data: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except BaseException as error:
                provider_errors.append(repr(error))
                self.send_error(500)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    serving = threading.Thread(target=server.serve_forever, daemon=True)
    serving.start()
    try:
        with tempfile.TemporaryDirectory(prefix="rpi-todo-native-") as directory:
            root = Path(directory)
            agent = root / "agent"
            agent.mkdir()
            (agent / "models.json").write_text(json.dumps({"providers": {"fixture": {
                "api": "openai-completions", "baseUrl": f"http://127.0.0.1:{server.server_port}/v1",
                "apiKey": "fixture-only", "models": [{"id": "fixture", "name": "fixture",
                    "reasoning": False, "input": ["text"], "contextWindow": 32768, "maxTokens": 1024}]
            }}}), encoding="utf-8")
            (agent / "settings.json").write_text(json.dumps({"defaultProvider": "fixture",
                "defaultModel": "fixture", "defaultThinkingLevel": "off"}), encoding="utf-8")
            env = {key: value for key, value in os.environ.items()
                   if not key.endswith(("_API_KEY", "_AUTH_TOKEN")) and key not in
                   ("RPI_SESSION_ID", "RPI_EXTENSIONS_DIR", "RPI_PROVIDER", "RPI_MODEL")}
            env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_OFFLINE="1")
            command = [str(rpi), "--mode", "rpc", "--offline", "--local-only", "--extension", str(extension),
                       "--session-dir", str(root / "sessions"), "--no-builtin-tools", "--no-skills",
                       "--no-context-files", "--no-prompt-templates", "--approve"]
            rpc = Rpc(command, env, root)
            try:
                assert rpc.todo(action="list")["todos"] == []
                assert rpc.snapshots() == [], "list must not append"
                added = rpc.todo(action="add", text="A")
                assert added["todos"][0]["id"] == 0 and added["nextId"] == 1, added
                first = rpc.snapshots()
                assert len(first) == 1, "successful todo add did not persist a todo-state snapshot"
                rpc.todo(action="add", text="B")
                rpc.todo(action="update", id=0, status="in_progress")
                rpc.todo(action="toggle", id=1)
                expected = rpc.todo(action="list", includeDone=True)
                assert [todo["status"] for todo in expected["todos"]] == ["in_progress", "completed"], expected
                before = rpc.snapshots()
                assert len(before) == 4, before
                rpc.todo(action="update", id=0, status="in_progress")
                assert rpc.snapshots() == before, "no-op update appended"
            finally:
                rpc.close()
            files = list((root / "sessions").rglob("*.jsonl"))
            assert len(files) == 1, files
            resume = command + ["--session", str(files[0])]
            rpc = Rpc(resume, env, root)
            try:
                # The first command/tool in a fresh RPC process must restore even
                # though this frontend does not dispatch SessionStart.
                text = rpc.call("execute_command", command="todos")["text"]
                assert "A" in text and "B" in text and "1/2 completed" in text, text
                restored = rpc.todo(action="list", includeDone=True)
                assert restored["todos"] == expected["todos"] and restored["nextId"] == 2, restored
                assert rpc.snapshots() == before, "reload/list must not append"
                navigation = rpc.call("navigate", targetId=first[0]["id"])
                assert navigation["outcome"] == "completed", navigation
                added = rpc.todo(action="add", text="C")
                assert [item["text"] for item in added["todos"]] == ["A", "C"], added
                assert [item["id"] for item in added["todos"]] == [0, 1], added
                assert added["nextId"] == 2, added
                assert len(rpc.snapshots()) == 2, "navigation must follow selected ancestry"
            finally:
                rpc.close()
            rpc = Rpc(resume, env, root)
            try:
                restored = rpc.todo(action="list", includeDone=True)
                assert [item["text"] for item in restored["todos"]] == ["A", "C"], restored
                assert restored["nextId"] == 2, restored
                cleared = rpc.todo(action="clear")
                assert cleared["todos"] == [] and cleared["nextId"] == 0, cleared
                before_clear_noop = rpc.snapshots()
                rpc.todo(action="clear")
                assert rpc.snapshots() == before_clear_noop
            finally:
                rpc.close()
            rpc = Rpc(resume, env, root)
            try:
                empty = rpc.todo(action="list", includeDone=True)
                assert empty["todos"] == [] and empty["nextId"] == 0, empty
                added = rpc.todo(action="add", text="D")
                assert added["todos"][0]["id"] == 0, added
            finally:
                rpc.close()
        assert not provider_errors, provider_errors
        print("PASS: real todo execution commits once per mutation; no-op/list do not append; "
              "fresh-process restore, branch navigation, status/IDs and clear survive restart")
    finally:
        server.shutdown()
        server.server_close()
        serving.join(timeout=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpi", type=Path, required=True)
    parser.add_argument("--extension", type=Path, required=True)
    args = parser.parse_args()
    smoke(args.rpi.resolve(), args.extension.resolve())
