"""Verify Web file-link prompt guidance with a real native host and loopback model.

Build rpi-web first, then run with explicit trusted --rpi and --extension paths.
All model traffic, profiles, transcripts and child processes are test-owned.
Does not install extensions, restart services or contact external model providers.
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

MARKER = "<rpi_web_file_links_v1>"
BASE = "FILE_LINK_FIXTURE_BASE"
EXTRA = "FILE_LINK_FIXTURE_EXISTING_APPEND"
QUESTION = "Reference a project file without using tools."


class Rpc:
    def __init__(self, command, env, cwd):
        self.events = queue.Queue()
        self.errors = []
        self.process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding="utf-8")
        self.readers = [threading.Thread(target=self.read, daemon=True),
                        threading.Thread(target=lambda: self.errors.extend(self.process.stderr), daemon=True)]
        for reader in self.readers:
            reader.start()
        try:
            assert self.events.get(timeout=20)["type"] == "ready", self.errors
        except BaseException:
            self.close()
            raise

    def read(self):
        for line in self.process.stdout:
            try:
                self.events.put(json.loads(line))
            except ValueError:
                self.errors.append("non-JSON stdout: " + line)

    def prompt(self):
        self.process.stdin.write(json.dumps({"type": "prompt", "message": QUESTION}) + "\n")
        self.process.stdin.flush()
        while True:
            event = self.events.get(timeout=30)
            if event.get("type") == "response":
                assert event.get("success"), (event, self.errors)
            if event.get("type") == "agent_settled":
                assert event.get("outcome") == "completed", (event, self.errors)
                return

    def close(self):
        try:
            self.process.stdin.close()
            self.process.wait(timeout=10)
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            for reader in self.readers:
                reader.join(timeout=5)
            self.process.stdout.close()
            self.process.stderr.close()


def smoke(rpi, extension):
    requests = []

    class Model(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
            events = [
                {"choices": [{"index": 0, "delta": {"role": "assistant", "content": "Fixture complete."}, "finish_reason": None}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
            data = ("".join("data: " + json.dumps(event) + "\n\n" for event in events)
                    + "data: [DONE]\n\n").encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    serving = threading.Thread(target=server.serve_forever, daemon=True)
    serving.start()
    try:
        with tempfile.TemporaryDirectory(prefix="rpi-web-file-links-") as directory:
            root = Path(directory).resolve()
            agent = root / "agent"
            project = root / "project with spaces"
            agent.mkdir()
            project.mkdir()
            (agent / "models.json").write_text(json.dumps({"providers": {"fixture": {
                "api": "openai-completions", "baseUrl": f"http://127.0.0.1:{server.server_port}/v1",
                "apiKey": "loopback-fixture", "models": [{"id": "fixture", "name": "fixture",
                    "reasoning": False, "input": ["text"], "contextWindow": 32768, "maxTokens": 256}]
            }}}), encoding="utf-8")
            (agent / "settings.json").write_text(json.dumps({"defaultProvider": "fixture",
                "defaultModel": "fixture", "retry": {"enabled": False}}), encoding="utf-8")
            env = {key: value for key, value in os.environ.items()
                   if not key.endswith(("_API_KEY", "_AUTH_TOKEN", "_BASE_URL", "_PROXY"))
                   and not key.startswith("RPI_")}
            env.update(RPI_CODING_AGENT_DIR=str(agent), NO_PROXY="127.0.0.1,localhost")
            (project / "instructions.md").write_text(BASE + "\n" + EXTRA, encoding="utf-8")
            for mode, enabled, turns in [("legacy", True, 2), ("exact-selected", True, 1), ("exact-unselected", False, 1)]:
                command = [str(rpi), "--mode", "rpc", "--approve", "--session-dir", str(root / mode)]
                if mode == "legacy":
                    command += ["--local-only", "--extension", str(extension), "--no-builtin-tools",
                                "--no-skills", "--no-context-files", "--no-prompt-templates",
                                "--system-prompt", BASE, "--append-system-prompt", EXTRA]
                else:
                    selection = project / f"{mode}.json"
                    selection.write_text(json.dumps({"version": 1, "instructions": ["instructions.md"],
                        "skills": [], "extensions": [str(extension)] if enabled else [],
                        "tools": {"mode": "none"}}), encoding="utf-8")
                    command += ["--resource-config", str(selection)]
                rpc = Rpc(command, env, project)
                try:
                    for _ in range(turns):
                        before = len(requests)
                        rpc.prompt()
                        assert len(requests) == before + 1, "unexpected provider calls"
                        messages = requests[-1]["messages"]
                        system = "\n".join(str(m["content"]) for m in messages if m["role"] in ("system", "developer"))
                        assert BASE in system and EXTRA in system, "lost existing instructions"
                        assert system.count(MARKER) == int(enabled), (mode, "wrong guide count")
                        if enabled:
                            assert system.index(BASE) < system.index(MARKER), "base must precede guide"
                            assert json.dumps(str(project), ensure_ascii=False) in system, "wrong project root"
                            assert "another Git worktree" in system
                        user = [m["content"] for m in messages if m["role"] == "user"]
                        assert user and all(content == QUESTION for content in user), "user text was rewritten"
                finally:
                    rpc.close()
                assert rpc.process.returncode == 0, "".join(rpc.errors)
                print(f"PASS {mode}: original instructions and user text preserved; guide count={int(enabled)}")
    finally:
        server.shutdown()
        server.server_close()
        serving.join(timeout=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpi", type=Path, required=True)
    parser.add_argument("--extension", type=Path, required=True)
    args = parser.parse_args()
    for path in (args.rpi, args.extension):
        if not path.is_absolute() or not path.is_file():
            parser.error("supply explicit absolute paths to existing trusted artifacts")
    smoke(args.rpi.resolve(), args.extension.resolve())
