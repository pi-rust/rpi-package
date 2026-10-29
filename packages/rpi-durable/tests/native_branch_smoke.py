"""Real native RPC + durable DLL smoke, without model/network calls.

python native_branch_smoke.py --rpi /path/rpi --extension /path/rpi_durable.dll
Uses only an isolated temporary profile/session; never touches installed plugins.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading


class Rpc:
    def __init__(self, command, env, cwd):
        self.lines = queue.Queue()
        self.errors = []
        self.process = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding="utf-8")
        self.readers = [threading.Thread(target=self.read, daemon=True),
                        threading.Thread(target=lambda: self.errors.extend(self.process.stderr), daemon=True)]
        for reader in self.readers:
            reader.start()
        try:
            ready = self.lines.get(timeout=20)
            assert ready["type"] == "ready", ready
        except BaseException:
            self.close(check=False)
            raise
        self.next_id = 0

    def read(self):
        for line in self.process.stdout:
            self.lines.put(json.loads(line))

    def request(self, kind, **params):
        self.next_id += 1
        key = str(self.next_id)
        self.process.stdin.write(json.dumps(dict(type=kind, id=key, **params)) + "\n")
        self.process.stdin.flush()
        while True:
            reply = self.lines.get(timeout=20)
            if reply.get("type") == "response" and reply.get("id") == key:
                return reply

    def call(self, kind, **params):
        reply = self.request(kind, **params)
        assert reply["success"], reply
        return reply["data"]

    def durable(self, **args):
        result = self.call("execute_command", command="durable", args=json.dumps(args))
        assert not result.get("error"), result
        assert not result.get("details", {}).get("error"), result
        return result

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
    with tempfile.TemporaryDirectory(prefix="rpi-canonical-smoke-") as directory:
        root = Path(directory)
        agent = root / "agent"
        agent.mkdir()
        (agent / "models.json").write_text(json.dumps({"providers": {"fixture": {
            "api": "openai-completions", "baseUrl": "http://127.0.0.1:1/v1",
            "apiKey": "unused", "models": [{"id": "fixture", "name": "fixture",
                "input": ["text"], "contextWindow": 8192, "maxTokens": 1024}]
        }}}), encoding="utf-8")
        (agent / "settings.json").write_text(json.dumps({"defaultProvider": "fixture",
            "defaultModel": "fixture", "defaultThinkingLevel": "off"}), encoding="utf-8")
        env = dict(os.environ, RPI_CODING_AGENT_DIR=str(agent), RPI_OFFLINE="1")
        for key in ("RPI_SESSION_ID", "RPI_EXTENSIONS_DIR", "RPI_PROVIDER", "RPI_MODEL"):
            env.pop(key, None)
        command = [str(rpi), "--mode", "rpc", "--offline", "--local-only",
                   "--extension", str(extension), "--session-dir", str(root / "sessions"),
                   "--no-skills", "--no-context-files", "--no-prompt-templates"]
        rpc = Rpc(command, env, root)
        try:
            assert rpc.call("get_session_branch", customTypes=["durable-state"])["entries"] == []
            for kind in ["alpha", "beta", "unrelated", "alpha", "beta"]:
                rpc.call("append_entry", customType=kind, data={"fixture": kind})
            result = rpc.call("get_session_branch", customTypes=["alpha", "beta", "alpha"])
            entries = result["entries"]
            assert result["filtered"] is True and result["count"] == 4, result
            assert [e["customType"] for e in entries] == ["alpha", "beta", "alpha", "beta"]
            assert [e["seq"] for e in entries] == sorted(e["seq"] for e in entries)
            for order, expected in [("oldestFirst", entries[:2]), ("newestFirst", entries[::-1][:2])]:
                assert rpc.call("get_session_branch", customTypes=["alpha", "beta"],
                                order=order, limit=2)["entries"] == expected
            assert rpc.call("get_session_branch", customTypes=["alpha", "beta"],
                            afterSeq=entries[1]["seq"])["entries"] == entries[2:]
            for args in [{}, {"customType": "alpha"}, {"custom_types": ["alpha"]},
                         {"customTypes": ["alpha"], "after_seq": 0}, {"customTypes": []}]:
                assert rpc.request("get_session_branch", **args)["success"] is False, args
            rpc.durable(action="start", name="canonical-smoke", steps=[{"id": "test", "title": "Smoke"}])
            rpc.durable(action="checkpoint", step="test", status="completed", note="verified")
            rpc.durable(action="enqueue", text="check restart")
            before = rpc.call("get_session_branch", customTypes=["durable-state"])["entries"]
            assert len(before) == 3, before
            assert before[0]["data"]["kind"] == "snapshot", before
            assert [e["data"]["revision"] for e in before] == [1, 2, 3], before
            rpc.durable(action="status")
        finally:
            rpc.close()
        saved = list((root / "sessions").rglob("*.jsonl"))
        assert len(saved) == 1, saved
        rpc = Rpc(command + ["--session", str(saved[0])], env, root)
        try:
            after = rpc.call("get_session_branch", customTypes=["durable-state"])["entries"]
            assert after == before, (before, after)
            status = rpc.durable(action="status")
            assert "canonical-smoke" in json.dumps(status), status
            assert "check restart" in json.dumps(status), status
        finally:
            rpc.close()
        print("PASS: customTypes/order/limit/exclusive cursor; legacy query rejection; "
              "real DLL snapshot/delta commits and native restart recovery")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpi", type=Path, required=True)
    parser.add_argument("--extension", type=Path, required=True)
    args = parser.parse_args()
    smoke(args.rpi.resolve(), args.extension.resolve())
