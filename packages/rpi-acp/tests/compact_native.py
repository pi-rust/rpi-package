"""Real fd47e29 native + ACP bridge, loopback SSE only; no real credentials.
All fixtures/logs are retained below --diagnostics-dir (use a new directory).
"""
import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
from pathlib import Path
import threading
import time
import uuid

from bounded_history_native import Peer, success
from compact_protocol import controls, environment, response, send, text


class Model:
    def __init__(self, logs):
        self.calls, self.errors = [], []
        self.mode = "summary"
        self.entered, self.release = threading.Event(), threading.Event()
        self.tool_sent = False
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(10)

            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    assert self.path == "/v1/chat/completions", self.path
                    length = int(self.headers["Content-Length"])
                    assert 0 < length < 4 * 1024 * 1024
                    call = json.loads(self.rfile.read(length))
                    owner.calls.append(call)
                    (logs / "provider-requests.json").write_text(json.dumps(owner.calls, indent=2), encoding="utf-8")
                    mode = owner.mode
                    if mode == "blocked":
                        owner.entered.set()
                        assert owner.release.wait(20), "blocked provider was not released"
                    delta = dict(role="assistant", content="COMPACTION_PROVIDER_SUMMARY_83f")
                    reason = "stop"
                    if mode == "tool" and not owner.tool_sent:
                        owner.tool_sent = True
                        delta = dict(role="assistant", tool_calls=[dict(index=0, id="compact-read", type="function",
                            function=dict(name="read", arguments=json.dumps(dict(path=str(logs / "canary.txt")))))])
                        reason = "tool_calls"
                    elif mode == "tool":
                        delta = dict(role="assistant", content="AFTER_CANCEL_TOOL_OK")
                    events = [dict(choices=[dict(index=0, delta=delta, finish_reason=None)]),
                              dict(choices=[dict(index=0, delta={}, finish_reason=reason)],
                                   usage=dict(prompt_tokens=50, completion_tokens=10, total_tokens=60))]
                    data = ("".join("data: " + json.dumps(e) + "\n\n" for e in events) + "data: [DONE]\n\n").encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    pass  # Expected when native cancels a blocked provider call.
                except Exception as error:
                    owner.errors.append(repr(error))
                    self.close_connection = True

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.05))
        self.thread.start()

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=15)
        assert not self.thread.is_alive(), "model fixture failed to join"
        assert not self.errors, self.errors


def seed(path):
    """Only called with bridge/native stopped; valid native v4 user entries.

    Native defaults keep_recent_tokens=20000, reserve_tokens=16384. Eight
    ~24KB turns exceed retained history while staying below 100000 context.
    Distinct user turns avoid the split-turn two-summary path.
    """
    entries = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
    assert entries[0]["version"] == 4
    seq = max(e.get("seq", 0) for e in entries)
    leaf = None
    for entry in entries:
        if entry.get("kind") == "entry" and entry.get("lane") == "main":
            leaf = entry["id"]
        elif entry.get("kind") == "lane" and entry.get("lane") == "main":
            leaf = entry.get("leafId")
    with path.open("a", encoding="utf-8") as output:
        for i in range(8):
            seq += 1
            entry_id = str(uuid.uuid4())
            message = dict(kind="user", role="user", content=f"NATIVE_COMPACT_HISTORY_{i}\n" + "old context decision. " * 1200, timestamp=0)
            output.write(json.dumps(dict(kind="entry", lane="main", type="message", id=entry_id,
                parentId=leaf, seq=seq, timestamp=int(time.time()*1000), message=message)) + "\n")
            leaf = entry_id


def prompt(peer, sid, value):
    return peer.request("session/prompt", dict(sessionId=sid, prompt=[dict(type="text", text=value)]))


def native_wire(rpi, env, root, logs):
    """Direct native probe remains runnable before an adapter build exists."""
    peer = Peer([rpi, "--mode", "rpc", "--no-session", "--local-only"], env, root, logs, "native-direct", native=True)
    try:
        deadline = time.monotonic() + 25
        while True:
            item = peer.receive(deadline)
            if item.get("type") == "ready":
                assert "compact" in item["supportedCommands"], item
                break
        data, events = success(peer, "compact", {})
        assert data["accepted"] is True and not events, (data, events)
        events = []
        while True:
            item = peer.receive(time.monotonic() + 15)
            events.append(item)
            if item.get("type") == "agent_settled":
                assert item["outcome"] == "declined" and item["summary"] == "Nothing to compact yet.", item
                break
        assert not any(e.get("type") in ("compaction_started", "compaction_finished") for e in events), events
    finally:
        peer.close()


def run(bridge, rpi, logs, native_only=False):
    root = logs / "fixture"
    root.mkdir(parents=True)
    env = environment(root)
    env["RPI_BIN"] = rpi
    model = Model(logs)
    peer = None
    try:
        path = root / "agent" / "models.json"
        models = json.loads(path.read_text())
        provider = models["providers"]["acp-fixture"]
        provider["baseUrl"] = f"http://127.0.0.1:{model.server.server_port}/v1"
        provider["models"][0]["contextWindow"] = 100000
        path.write_text(json.dumps(models), encoding="utf-8")
        native_wire(rpi, env, root, logs)
        assert not model.calls, "empty compaction must not contact provider"
        if native_only:
            print("PASS direct native: compact capability, accepted then declined+summary; no synthetic compaction events/provider call; EOF joined")
            return
        peer = Peer([bridge], env, root, logs, "native-create")
        success(peer, "initialize", dict(protocolVersion=1))
        result, updates = success(peer, "session/new", dict(cwd=str(root), mcpServers=[]))
        assert not updates, updates
        sid = result["sessionId"]
        reply, updates = prompt(peer, sid, "/compact")
        assert reply.get("result", {}).get("stopReason") == "end_turn", reply
        assert text(updates) == "Nothing to compact yet." and not model.calls, updates
        assert len(controls(updates, sid)[1]) == 1, updates
        peer.close()
        peer = None
        files = list((root / "sessions" / sid).rglob("*.jsonl"))
        assert len(files) == 1, files
        seed(files[0])
        params = dict(sessionId=sid, cwd=str(root), mcpServers=[])
        peer = Peer([bridge], env, root, logs, "native-populated")
        success(peer, "initialize", dict(protocolVersion=1))
        success(peer, "session/resume", params)
        instructions = "PRESERVE_COMPACT_DECISIONS_42\nKeep paths verbatim."
        reply, updates = prompt(peer, sid, "/compact \n" + instructions)
        assert reply.get("result", {}).get("stopReason") == "end_turn", (reply, updates)
        assert text(updates).startswith("Conversation compacted · ") and text(updates).count("Conversation compacted") == 1, updates
        assert model.calls and all(instructions in json.dumps(c, ensure_ascii=False).replace("\\n", "\n") for c in model.calls), model.calls
        entries = [json.loads(line) for line in files[0].read_text(encoding="utf-8").splitlines()]
        compacted = [e for e in entries if e.get("type") == "compaction"]
        assert len(compacted) == 1 and "COMPACTION_PROVIDER_SUMMARY_83f" in json.dumps(compacted), compacted
        peer.close()
        peer = None
        peer = Peer([bridge], env, root, logs, "native-restart-cancel")
        success(peer, "initialize", dict(protocolVersion=1))
        _, updates = success(peer, "session/load", params)
        replay = "".join(u.get("params", {}).get("update", {}).get("content", {}).get("text", "") for u in updates)
        assert all(f"NATIVE_COMPACT_HISTORY_{i}" in replay for i in range(8)), "restart lost original transcript messages"
        # Native get_messages exposes a compaction boundary, not its model summary.
        # Prove restored MODEL context separately on the next provider request.
        calls_before = len(model.calls)
        reply, _ = prompt(peer, sid, "Continue after restart.")
        assert reply.get("result", {}).get("stopReason") == "end_turn", reply
        assert "COMPACTION_PROVIDER_SUMMARY_83f" in json.dumps(model.calls[calls_before:]), "persisted summary missing from restarted model context"
        model.mode = "blocked"
        send(peer, "session/prompt", dict(sessionId=sid, prompt=[dict(type="text", text="/compact keep decisions")]), 9001)
        assert model.entered.wait(15), "native compaction did not enter provider"
        send(peer, "session/cancel", dict(sessionId=sid))
        reply, _ = response(peer, 9001)
        assert reply.get("result", {}).get("stopReason") == "cancelled", reply
        model.release.set()
        model.mode = "tool"
        (logs / "canary.txt").write_text("LOCAL_COMPACT_TOOL_CANARY", encoding="utf-8")
        reply, updates = prompt(peer, sid, "Read the fixture file, then reply.")
        assert reply.get("result", {}).get("stopReason") == "end_turn" and text(updates) == "AFTER_CANCEL_TOOL_OK", (reply, updates)
        assert any(u.get("params", {}).get("update", {}).get("sessionUpdate") == "tool_call" for u in updates), updates
        assert any("LOCAL_COMPACT_TOOL_CANARY" in json.dumps(c) for c in model.calls), "tool result did not return to provider"
        print("PASS real native: empty decline; populated compact/instructions; v4 persistence/restart replay; blocked cancellation; next prompt/tool; EOF joined")
    finally:
        model.release.set()
        try:
            if peer is not None:
                peer.close()
        finally:
            model.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge")
    parser.add_argument("--rpi", required=True)
    parser.add_argument("--native-only", action="store_true")
    parser.add_argument("--diagnostics-dir", required=True, type=Path)
    args = parser.parse_args()
    assert args.native_only or args.bridge, "--bridge required unless --native-only"
    args.diagnostics_dir.mkdir(parents=True, exist_ok=True)
    run(str(Path(args.bridge).resolve()) if args.bridge else None, str(Path(args.rpi).resolve()), args.diagnostics_dir.resolve(), args.native_only)
