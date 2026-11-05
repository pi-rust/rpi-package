#!/usr/bin/env python3
"""Opt-in Windows real-native post-ACK ToolProgram live proof (loopback model).

Requires explicit test binaries, prebuild source/binary provenance and a NEW
output directory. No installs, external credentials, production restarts or
fixture notifications. One real bash child blocks on a file until subscribe ACK;
final/closed updates must arrive afterward and match persisted native entries.
Does NOT qualify UI, Unix, live external providers, every lifecycle or full parity.
Uses the existing exact-handle bridge/launcher/native cleanup audit. The bash
child is additionally tracked by its explicit self-recorded PID/kernel identity.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import shlex
import shutil
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, HTTPServer

from tool_program_native import Peer, records, require, save, sha


class Provider:
    def __init__(self, code):
        self.calls, self.errors = [], []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                try:
                    require(self.path == "/v1/chat/completions", "unexpected provider route")
                    count = int(self.headers["Content-Length"])
                    require(count <= 1024 * 1024, "provider request too large")
                    body = json.loads(self.rfile.read(count))
                    owner.calls.append(body)
                    require(len(owner.calls) <= 2, "unexpected model continuation")
                    require("codemode" in {t["function"]["name"] for t in body.get("tools", [])},
                            "real Code Mode DLL not loaded")
                    if len(owner.calls) == 1:
                        delta = {"role": "assistant", "tool_calls": [{"index": 0,
                            "id": "native-live-codemode", "type": "function", "function": {
                                "name": "codemode", "arguments": json.dumps({"code": code})}}]}
                        finish = "tool_calls"
                    else:
                        require(any(m.get("role") == "tool" for m in body["messages"]),
                                "no native tool result in continuation")
                        delta, finish = {"role": "assistant", "content": "NATIVE_LIVE_DONE"}, "stop"
                    frames = [{"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                        {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
                         "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}]
                    wire = ("".join("data: " + json.dumps(frame) + "\n\n" for frame in frames)
                            + "data: [DONE]\n\n").encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(wire)))
                    self.end_headers()
                    self.wfile.write(wire)
                    self.wfile.flush()
                except BaseException as error:
                    owner.errors.append(repr(error))
                    self.close_connection = True

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)
        require(not self.thread.is_alive(), "provider did not stop")


def send(peer, method, params):
    peer.next_id += 1
    frame = dict(jsonrpc="2.0", id=peer.next_id, method=method, params=params)
    wire = (json.dumps(frame) + "\n").encode()
    peer.sent.write(wire)
    peer.sent.flush()
    peer.process.stdin.write(wire)
    peer.process.stdin.flush()
    peer.counts["requests"] += 1
    return peer.next_id


def receive(peer, deadline, wire):
    require(not peer.reader_errors, repr(peer.reader_errors))
    require(time.monotonic() < deadline, "live proof deadline exceeded")
    try:
        frame = peer.output.get(timeout=min(.1, max(.001, deadline - time.monotonic())))
    except queue.Empty:
        require(peer.process.poll() is None, "bridge exited during live proof")
        return None
    wire.append(dict(receivedNs=time.monotonic_ns(), frame=frame))
    peer.counts["responses" if "id" in frame else "notifications"] += 1
    require(frame.get("jsonrpc") == "2.0", "bad ACP frame")
    return frame


def run(args, out):
    agent, workspace = out / "agent", out / "workspace"
    (agent / "extensions").mkdir(parents=True)
    workspace.mkdir()
    (out / "home").mkdir()
    (out / "tmp").mkdir()
    shutil.copyfile(args.codemode, agent / "extensions" / "rpi_codemode.dll")
    child = workspace / "child.py"
    child.write_text("""import os, pathlib, time
root = pathlib.Path(__file__).parent
(root / 'child-started').write_text(str(os.getpid()))
print('NATIVE_CHILD_RUNNING', flush=True)
end = time.monotonic() + 40
while not (root / 'release-child').exists():
    if time.monotonic() > end: raise RuntimeError('release gate timed out')
    time.sleep(.02)
print('NATIVE_CHILD_DONE', flush=True)
""", encoding="utf-8")
    # Avoid the Windows venv redirector: pin the actual base interpreter image.
    child_executable = Path(getattr(sys, "_base_executable", sys.executable)).resolve()
    command = shlex.quote(child_executable.as_posix()) + " " + shlex.quote(child.as_posix())
    provider = Provider("const result = await tools.bash(" + json.dumps({"command": command})
                        + "); return result;")
    save(agent / "models.json", {"providers": {"live-fixture": {"api": "openai-completions",
        "baseUrl": "http://127.0.0.1:%s/v1" % provider.server.server_port, "apiKey": "local-fixture",
        "models": [{"id": "fixture", "name": "fixture", "reasoning": False,
                    "input": ["text"], "contextWindow": 100000, "maxTokens": 1024}]}}})
    save(agent / "settings.json", dict(defaultProvider="live-fixture", defaultModel="fixture",
                                       defaultThinkingLevel="off"))
    save(agent / "auth.json", {})
    env = {k: v for k, v in os.environ.items() if k.upper() in {
        "SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "SYSTEMDRIVE",
        "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}}
    env.update(RPI_BIN=str(args.launcher), RPI_CODEMODE_NATIVE_BIN=str(args.native),
        RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(agent / "extensions"),
        RPI_PROVIDER="live-fixture", RPI_MODEL="fixture", RPI_ACP_SESSION_DIR=str(out / "sessions"),
        RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_ACP_NO_PERMISSION_GATE="1", RPI_OFFLINE="0",
        HOME=str(out / "home"), USERPROFILE=str(out / "home"), TEMP=str(out / "tmp"), TMP=str(out / "tmp"))
    peer, child_handle = None, None
    wire, result = [], {}
    try:
        peer = Peer(args.bridge, env, out, 60, out / "sessions" / "native-pid.txt",
                    {role: sha(getattr(args, role)) for role in ("bridge", "native", "launcher")})
        peer.success("initialize", dict(protocolVersion=1, clientCapabilities={}))
        new, _ = peer.success("session/new", dict(cwd=str(workspace), mcpServers=[]))
        sid = new["sessionId"]
        peer.pids.capture()
        require(set(peer.pids.rows) == {"bridge", "launcher", "native"}, "native identity chain missing")
        prompt_id = send(peer, "session/prompt", dict(sessionId=sid,
                         prompt=[dict(type="text", text="Run the isolated native live proof.")]))
        deadline = time.monotonic() + 60
        while not (workspace / "child-started").exists():
            frame = receive(peer, deadline, wire)
            require(frame is None or "id" not in frame, "prompt failed before real child started")
        pid = int((workspace / "child-started").read_text())
        child_handle = peer.pids.api.open(pid)
        child_identity = peer.pids.api.identity(child_handle)
        require(Path(child_identity["imagePath"]).resolve() == child_executable,
                "real child image mismatch: " + repr(child_identity))
        require(peer.pids.rows["native"]["creationTime100ns"] <= child_identity["creationTime100ns"]
                <= peer.pids.api.now(), "child creation time outside owned run")
        require(peer.pids.api.state(child_handle)["alive"], "child ended before subscription")
        query = dict(children=dict(direction="newestFirst", limit=8, maxBytes=32768), phaseLimit=8)
        sub_id = send(peer, "session/tool_program_subscribe", dict(sessionId=sid,
                      subscriptionId="real-live", query=query))
        ack = None
        while ack is None:
            frame = receive(peer, deadline, wire)
            if frame is None:
                continue
            require(frame.get("method") != "session/tool_program_update", "notification before ACK")
            if "id" in frame:
                require(frame["id"] == sub_id and "result" in frame, repr(frame))
                ack = frame
        snapshot = ack["result"]["snapshot"]
        require(not snapshot["resyncRequired"], "uncertain initial snapshot")
        require(len(snapshot["page"]["children"]) == 1, "real running child absent from snapshot")
        child_id = snapshot["page"]["children"][0]["childId"]
        require(snapshot["observations"][0]["outcome"] == "unknown", "child already terminal at ACK")
        ack_ns = wire[-1]["receivedNs"]
        (workspace / "release-child").write_text("ACK received", encoding="utf-8")
        prompt, updates = None, []
        while prompt is None or not any(u["observation"] and u["observation"]["outcome"] == "succeeded" for u in updates):
            frame = receive(peer, deadline, wire)
            if frame is None:
                continue
            if "id" in frame:
                require(frame["id"] == prompt_id and "result" in frame, repr(frame))
                prompt = frame
            if frame.get("method") == "session/tool_program_update":
                update = frame["params"]["update"]
                require(update["type"] == "update", "resync is NOT live acceptance: " + repr(update))
                require(update["event"]["data"]["childId"] == child_id, "different live child")
                require(wire[-1]["receivedNs"] > ack_ns, "update not post-ACK")
                updates.append(update)
        require(prompt["result"]["stopReason"] == "end_turn", repr(prompt))
        kinds = [u["event"]["data"].get("phase") for u in updates]
        require("final" in kinds and "closed" in kinds, "post-ACK final/closed both required")
        require(len(provider.calls) == 2 and not provider.errors, repr(provider.errors))
        post, notifications = peer.success("session/tool_program_snapshot", dict(sessionId=sid, query=query))
        require(not notifications, "unexpected notification after closed")
        require(not post["resyncRequired"] and post["observations"][0]["outcome"] == "succeeded", repr(post))
        peer.success("session/tool_program_unsubscribe", dict(sessionId=sid, subscriptionId="real-live"))
        result = dict(status="PASSED_REAL_NATIVE_POST_ACK_UPDATES", sessionId=sid,
            childId=child_id, ackReceivedNs=ack_ns, subscribe=ack, prompt=prompt,
            updates=updates, postSnapshot=post, providerCalls=len(provider.calls), childProcess=child_identity)
    finally:
        (workspace / "release-child").write_text("cleanup", encoding="utf-8")
        save(out / "received.json", wire)
        save(out / "provider.json", dict(calls=provider.calls, errors=provider.errors))
        try:
            if peer:
                if child_handle:
                    require(peer.pids.api.k.WaitForSingleObject(child_handle, 10000) == 0,
                            "real child did not exit after release")
                    result["childExit"] = peer.pids.api.state(child_handle)
                    peer.pids.api.close(child_handle)
                result["cleanup"] = peer.close()
        finally:
            provider.close()
    # Inspect native wire AND native-owned transcript only after cleanup/drain.
    entries = []
    for path in (out / "sessions").rglob("*.jsonl"):
        rows = records(path)
        if rows and rows[0].get("id") == result["sessionId"]:
            entries.extend(row for row in rows if row.get("kind") == "entry")
    require(entries, "native primary transcript not retained")
    for update in result["updates"]:
        event = update["event"]
        matches = [e for e in entries if e.get("id") == event["entryId"]]
        require(len(matches) == 1, "live identity missing/duplicated in native transcript")
        e = matches[0]
        # Core's strict Option DTO serializes absent native optional fields as
        # null. Normalize only those five known optional schema members; all
        # identifiers, flags, indices and full descriptors still compare exactly.
        data = dict(e["data"])
        for optional in ("descriptor", "isError", "cancelled", "failed", "interrupted"):
            data.setdefault(optional, None)
        require(e["seq"] == event["seq"] and e["parentId"] == event["parentEntryId"]
                and e["customType"] == event["customType"] and e["lane"] == event["lane"]
                and data == event["data"], "live event differs from persisted native entry")
    native = records(out / "sessions" / "native-output.jsonl")
    live_ids = {u["event"]["entryId"] for u in result["updates"]}
    require(live_ids <= {e.get("entryId") for e in native if e.get("type") == "tool_program_child_phase"},
            "adapter update not present in actual native output")
    result["nativeMatchedUpdates"] = len(result["updates"])
    save(out / "results.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ("bridge", "native", "launcher", "codemode", "build-evidence", "out"):
        parser.add_argument("--" + field, required=True, type=lambda p: Path(p).resolve())
    args = parser.parse_args()
    require(os.name == "nt", "Windows proof only")
    require(not args.out.exists(), "new output directory required")
    evidence = json.loads(args.build_evidence.read_text(encoding="utf-8"))
    for role in ("bridge", "native", "launcher", "codemode"):
        require(sha(getattr(args, role)) == evidence["binaries"][role]["sha256"], "binary provenance mismatch: " + role)
    for source in evidence["sources"]:
        require(sha(source["path"]) == source["sha256"], "source changed since prebuild inventory")
    args.out.mkdir(parents=True)
    save(args.out / "provenance.json", evidence)
    try:
        result = run(args, args.out)
        print(json.dumps(dict(status=result["status"], updates=result["nativeMatchedUpdates"], out=str(args.out))))
    except BaseException:
        save(args.out / "failure.json", dict(status="FAILED", traceback=traceback.format_exc()))
        raise


if __name__ == "__main__":
    main()
