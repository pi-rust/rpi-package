"""Stage12 AUTHOR-ONLY source; a passing execution requires BOTH fresh binaries.

No build, binary discovery, installed DLL or real credential access. Stdlib and
existing stdlib-only bounded Windows Job transport. --prebuild-inventory is a
parent-created JSON: {"createdAtNs": N, "sources": {ABS_REPO: {REL: SHA256}},
"builds": {"native": {"exitCode": 0, "startedAtNs": N}, "bridge": {...}}}.
Every tracked/untracked Rust/Cargo/Python/build-config input must be inventoried;
parent freezes it BEFORE either build. Records are parent attestations, not a
cryptographic proof. No synthetic native/session peer is used by this test.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import queue
import socket
import struct
import subprocess
import sys
import threading
import time
import traceback
import uuid
import zlib
from http.server import BaseHTTPRequestHandler, HTTPServer

sys.dont_write_bytecode = True
import context_usage_native as transport
import mcp_stdio_native as helpers

TARGET = Path("C:/Users/bigfish/.cargo/target/acp-stage12-01")
PACKAGE = Path(__file__).resolve().parents[3]
NATIVE = PACKAGE.parent / "native-message-image-fs"
ALLOW = {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "SYSTEMDRIVE",
         "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}
LIMIT = 16 * 1024 * 1024


def require(ok, message):
    if not ok:
        raise AssertionError(message)


def dump(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False), encoding="utf-8")


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def inventory(repo):
    names = subprocess.check_output(["git", "-C", str(repo), "ls-files", "--cached",
        "--others", "--exclude-standard", "-z"]).decode().split("\0")
    selected = sorted({n for n in names if n and (n.endswith((".rs", ".py")) or
        Path(n).name in ("Cargo.toml", "Cargo.lock") or ".cargo" in Path(n).parts)})
    require(selected, "empty source inventory")
    return {n: sha(repo / n) for n in selected}


def images(root):
    # Valid 1x1 RGB PNG and baseline grayscale JPEG, generated without Pillow.
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(b"\0\xff\0\0")) + chunk(b"IEND", b"")
    def segment(marker, data):
        return bytes([255, marker]) + struct.pack(">H", len(data) + 2) + data
    jpeg = b"\xff\xd8" + segment(0xdb, b"\0" + bytes([1]) * 64)
    jpeg += segment(0xc0, b"\x08\x00\x01\x00\x01\x01\x01\x11\x00")
    jpeg += segment(0xc4, b"\x00\x01" + bytes(15) + b"\x00" + b"\x10\x01" + bytes(15) + b"\x00")
    jpeg += segment(0xda, b"\x01\x01\x00\x00\x3f\x00") + b"\x3f\xff\xd9"
    blocks = [dict(type="text", text="BEFORE_IMAGE"),
              dict(type="image", mimeType="image/png", data=base64.b64encode(png).decode()),
              dict(type="text", text="BETWEEN_IMAGES"),
              dict(type="image", mimeType="image/jpeg", data=base64.b64encode(jpeg).decode()),
              dict(type="text", text="AFTER_IMAGE")]
    (root / "input.png").write_bytes(png)
    (root / "input.jpg").write_bytes(jpeg)
    return blocks


def text(content):
    if isinstance(content, str):
        return content
    return "\n".join(b["text"] for b in content if isinstance(b, dict) and b.get("type") == "text")


class Model:
    """Actual OpenAI chat SSE provider: five strictly ordered requests per run."""
    def __init__(self, root, blocks):
        self.root, self.blocks, self.calls, self.errors = root, blocks, [], []
        self.steps = queue.Queue(maxsize=12)
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(8)
            def log_message(self, *_):
                pass
            def do_POST(self):
                try:
                    require(self.path == "/v1/chat/completions", "unexpected provider path")
                    count = int(self.headers.get("Content-Length", 0))
                    require(0 < count <= 1024 * 1024 and len(owner.calls) < 10, "provider bound")
                    raw = self.rfile.read(count)
                    require(len(raw) == count, "partial model request")
                    index = len(owner.calls)
                    (root / f"provider-{index:02d}.request.json").write_bytes(raw)
                    body = json.loads(raw)
                    owner.calls.append(body)
                    step = owner.steps.get_nowait()
                    require(body.get("model") == "synthetic", "nonfixture model")
                    users = [m for m in body["messages"] if m.get("role") == "user"]
                    expected = [dict(type="text", text=b["text"]) if b["type"] == "text" else
                        dict(type="image_url", image_url=dict(url="data:" + b["mimeType"] +
                             ";base64," + b["data"])) for b in blocks]
                    require(any(m.get("content") == expected for m in users),
                            "provider lost/reordered/rewrote exact PNG/JPEG or text spans")
                    if step:
                        prior = [m for m in body["messages"] if m.get("tool_call_id") == f"{step-1}-call"]
                        require(len(prior) == 1, "tool result correlation lost")
                        if step == 1:
                            result = text(prior[0]["content"])
                            require("UNSAVED" in result and all(s not in result for s in ("DISK", "FIRST", "LAST")),
                                    "read did not inspect single-sliced unsaved client buffer")
                    delta = dict(role="assistant", content=f"CHUNK_{step}")
                    reason = "stop"
                    if step < 4:
                        name, arguments = [
                            ("read", dict(path="buffer.txt", offset=2, limit=1)),
                            ("write", dict(path="buffer.txt", content="WRITTEN\n")),
                            ("edit", dict(path="buffer.txt", edits=[dict(oldText="WRITTEN", newText="EDITED")])),
                            ("write", dict(path="never-host-created/new.txt", content="CLIENT_ONLY\n"))][step]
                        require(any(t.get("function", {}).get("name") == name for t in body["tools"]),
                                "actual native builtin absent")
                        delta["tool_calls"] = [dict(index=0, id=f"{step}-call", type="function",
                            function=dict(name=name, arguments=json.dumps(arguments)))]
                        reason = "tool_calls"
                    events = [dict(choices=[dict(index=0, delta=delta, finish_reason=None)]),
                              dict(choices=[dict(index=0, delta={}, finish_reason=reason)]),
                              dict(choices=[], usage=dict(prompt_tokens=40, completion_tokens=8, total_tokens=48))]
                    data = ("".join("data: " + json.dumps(e) + "\n\n" for e in events) + "data: [DONE]\n\n").encode()
                    (root / f"provider-{index:02d}.response.sse").write_bytes(data)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(data)
                    self.wfile.flush()
                except BaseException:
                    owner.errors.append(traceback.format_exc())
                    dump(root / "provider-errors.json", owner.errors)
                finally:
                    self.close_connection = True
        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.05), daemon=True)
        self.thread.start()

    def plan(self):
        for step in range(5):
            self.steps.put_nowait(step)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(10)
        require(not self.thread.is_alive() and not self.errors and self.steps.empty(), "provider cleanup/plan failure")
        with socket.socket() as sock:
            require(sock.connect_ex(("127.0.0.1", self.server.server_port)) != 0, "provider listener survived")


class Client:
    def __init__(self, workspace, sid):
        self.workspace, self.sid, self.requests = workspace, sid, []
        self.buffers = {str((workspace / "buffer.txt").resolve()): "FIRST\nUNSAVED\nLAST\n"}

    def file(self, method, params):
        require(params.get("sessionId", self.sid) == self.sid, "cross-session file request")
        path = Path(params["path"])
        require(path.is_absolute() and path.resolve().is_relative_to(self.workspace), "client path not absolute/workspace scoped")
        path = str(path.resolve())
        self.requests.append(dict(method=method, **params))
        if method in ("fs/read_text_file", "read_text_file"):
            content = self.buffers[path]
            if "line" in params:
                # Native read asks for one extra sentinel line to preserve
                # truncation/continuation hints. ACP forwards that range once.
                require(params["line"] == 2 and params["limit"] == 2, "offset/sentinel range not mapped exactly once")
                content = "\n".join(content.splitlines()[params["line"]-1:params["line"]-1+params["limit"]])
            return dict(content=content)
        require(method in ("fs/write_text_file", "write_text_file"), "unexpected file operation")
        self.buffers[path] = params["content"]
        return {}

    def check(self):
        require(self.buffers[str(self.workspace / "buffer.txt")] == "EDITED\n", "write/edit did not modify client buffer")
        require(self.buffers[str(self.workspace / "never-host-created/new.txt")] == "CLIENT_ONLY\n", "client-only write missing")
        require((self.workspace / "buffer.txt").read_text() == "DISK", "host disk changed")
        require(not (self.workspace / "never-host-created").exists(), "delegated write created host directory")
        require(len(self.requests) == 5 and sum("line" in r for r in self.requests) == 1, "file request count/range mismatch")


class Peer(transport.Transport):
    def __init__(self, *args):
        self.client, self.seen = None, []
        super().__init__(*args)

    def receive(self, deadline):
        value = super().receive(deadline)
        require(len(self.seen) < 4096, "event count bound")
        self.seen.append(value)
        return value

    def write(self, value):
        self.record("in", value)
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def handle(self, value):
        if value.get("type") == "file_io_request":
            result = self.client.file(value["operation"], value)
            self.write(dict(id="file-" + uuid.uuid4().hex, type="file_io_response", requestId=value["requestId"], result=result))
            return True
        if "id" not in value or "method" not in value:
            return False
        params, method = value["params"], value["method"]
        if method == "session/request_permission":
            require(params["sessionId"] == self.client.sid and params["toolCall"]["toolCallId"], "permission identity")
            options = [o for o in params["options"] if o["kind"] == "allow_once"]
            require(len(options) == 1, "missing unambiguous correlated allow option")
            result = dict(outcome=dict(outcome="selected", optionId=options[0]["optionId"]))
        else:
            result = self.client.file(method, params)
        self.write(dict(jsonrpc="2.0", id=value["id"], result=result))
        return True

    def response(self, request_id, expect_error=False):
        require(not expect_error, "unexpected error expectation")
        frames, deadline = [], time.monotonic() + 60
        while True:
            value = self.receive(deadline)
            if self.handle(value):
                continue
            if value.get("id") == request_id:
                require("error" not in value, "ACP error: " + str(value))
                return value["result"], frames
            frames.append(value)

    def native(self, kind, **fields):
        request_id = "native-" + uuid.uuid4().hex
        self.write(dict(type=kind, id=request_id, **fields))
        deadline = time.monotonic() + 60
        while True:
            value = self.receive(deadline)
            if self.handle(value):
                continue
            if value.get("id") == request_id:
                require(value.get("success") is True and value.get("command") == kind, "native command failed")
                return value.get("data")


def ready(peer):
    value = peer.receive(time.monotonic() + 20)
    require(value.get("type") == "ready" and value.get("protocol") == "rpi", "missing native ready")
    require(value["semantics"].get("messageIds") == "sessionEntryId-v1", "native identity token not enabled; do not waive")
    require(value["semantics"].get("settlementEvent") == "agent_settled" and
            value["semantics"].get("promptResponse") == "accepted", "native settlement contract")
    require(value["promptContent"].get("contentBlocksVersion") == 1 and value["promptContent"].get("image") is True
            and value["fileIO"].get("version") == 1, "native capability profile missing")


def entries(path):
    require(path.stat().st_size <= LIMIT, "transcript size bound")
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def check_durable(path, blocks, frames, acp=False):
    data = entries(path)
    messages = [e for e in data if e.get("kind") == "entry" and e.get("type") == "message"]
    users = [e for e in messages if e["message"].get("role") == "user"]
    assistants = [e for e in messages if e["message"].get("role") == "assistant"]
    require(len(users) == 1 and users[0]["message"]["content"] == blocks, "native durable ordered image payload differs")
    durable_ids = {e["id"] for e in assistants}
    results = [e["message"] for e in messages if e["message"].get("role") == "toolResult"]
    require(len(results) == 4 and all(r.get("isError") is False for r in results), "native tool failed or result missing")
    require({r.get("toolCallId") for r in results} == {f"{i}-call" for i in range(4)}, "durable tool correlation mismatch")
    if acp:
        updates = [f.get("params", {}).get("update", {}) for f in frames]
        chunks = [u for u in updates if u.get("sessionUpdate") == "agent_message_chunk"]
        require(chunks and all(u.get("messageId") in durable_ids for u in chunks), "ACP live IDs not native durable IDs")
        live_ids = {u["messageId"] for u in chunks}
        for index, entry in enumerate(assistants):
            joined = "".join(u["content"]["text"] for u in chunks if u["messageId"] == entry["id"])
            require(joined == f"CHUNK_{index}", "ACP text duplicated/lost across stable message identity")
    else:
        chunks = [f for f in frames if f.get("type") in ("message_start", "message_update", "message_end")
                  and f.get("message", {}).get("role") == "assistant"]
        require(chunks and all(f.get("messageId") in durable_ids for f in chunks), "native live IDs not durable entry IDs")
        live_ids = {f["messageId"] for f in chunks}
    require(live_ids == durable_ids and len(live_ids) == 5, "assistant message boundary missing/duplicated")
    require([text(e["message"]["content"]) for e in assistants] == [f"CHUNK_{i}" for i in range(5)],
            "durable assistant text missing/duplicated")
    return users[0]["id"], durable_ids


def pages(peer, path):
    before, collected, leaf = None, [], None
    for _ in range(32):
        result = peer.native("get_messages", limit=1, **({"beforeId": before} if before else {}))
        require(result["beforeId"] == before and len(result["messages"]) == 1, "page boundary/size mismatch")
        leaf = result["leafId"] if leaf is None else leaf
        require(result["leafId"] == leaf, "paged history leaf changed")
        collected = result["messages"] + collected
        if not result["hasMore"]:
            break
        require(result["firstId"] != before, "paging made no progress")
        before = result["firstId"]
    else:
        raise AssertionError("page count bound")
    expected = [dict(e["message"], entryId=e["id"]) for e in entries(path)
                if e.get("kind") == "entry" and e.get("type") == "message"]
    # Native Option<Value> deserializes persisted details:null as None and
    # omits it on cold reload. Normalize ONLY that established serde case.
    def canonical(message):
        message = dict(message)
        if message.get("role") == "toolResult" and message.get("details") is None:
            message.pop("details", None)
        return message
    require([canonical(m) for m in collected] == [canonical(m) for m in expected],
            "paged native history lost IDs/image spans/payload")


def run(args):
    require(os.name == "nt" and __debug__, "Windows nonoptimized Python required")
    root = args.diagnostics_dir.resolve()
    require(args.diagnostics_dir.is_absolute() and root.is_relative_to(TARGET.resolve()) and root != TARGET.resolve()
            and not root.exists(), "NEW absolute target-only run directory required")
    root.mkdir(parents=True)
    state, owned, model = dict(status="running", caveats=["No thought/reasoning proof", "No old-native qualification",
        "Permission gate disabled ONLY for this isolated filesystem fixture; denial requires existing fixtures",
        "Cancellation/late-response behavior requires separate protocol/native fixtures"]), [], None
    manifest = json.loads(args.prebuild_inventory.read_text(encoding="utf-8"))
    manifest_sha, started = sha(args.prebuild_inventory), time.time_ns()
    def frozen(label):
        require(sha(args.prebuild_inventory) == manifest_sha, "parent evidence mutated during execution")
        actual = {str(repo.resolve()): inventory(repo) for repo in (PACKAGE, NATIVE)}
        require(actual == manifest["sources"], "source inventory drift/incomplete parent manifest")
        for name, path, expected in (("bridge", args.bridge, args.bridge_sha), ("native", args.rpi, args.native_sha)):
            require(path.is_absolute() and path.is_file() and sha(path) == expected, name + " binary drift")
            build = manifest["builds"][name]
            require(build["exitCode"] == 0 and manifest["createdAtNs"] < build["startedAtNs"]
                    <= build["finishedAtNs"] < started, "not a successful postfreeze pre-execution build")
            require(Path(build["path"]).resolve() == path.resolve() and build["sha256"] == expected,
                    "parent build record does not bind this binary")
        dump(root / (label + "-provenance.json"), dict(sources=actual, binaries={str(p): sha(p) for p in (args.bridge, args.rpi)},
            parentPrebuildInventory=str(args.prebuild_inventory), inventorySha256=sha(args.prebuild_inventory)))
    try:
        frozen("start")
        blocks = images(root)
        model = Model(root, blocks)
        env = helpers.environment(root, model, args.rpi)  # positive allowlist; isolated HOME/auth/models
        env["RPI_AGENT_DIR"] = env["RPI_CODING_AGENT_DIR"]
        dump(root / "environment-audit.json", dict(keys=sorted(env), inheritedAllowlist=sorted(ALLOW),
            permissionGate="RPI_ACP_NO_PERMISSION_GATE=1 fixture ONLY; not bypass/denial proof", actualCredentialsRead=False,
            isolatedHome=env["HOME"], isolatedAgent=env["RPI_CODING_AGENT_DIR"], provider="127.0.0.1/dummy-key"))
        win = helpers.Windows()
        def launch(label, argv, cwd):
            peer = Peer(argv, env, root, label, win, cwd)
            owned.append(peer)
            return peer
        def raw_args(sid, directory, saved=None):
            return [args.rpi, "--mode", "rpc", "--session-id", sid, "--session-dir", directory,
                "--local-only", "--no-extensions", "--no-skills", "--no-context-files", "--no-prompt-templates",
                "--provider", "fixture", "--model", "synthetic"] + (["--session", saved] if saved else [])
        def workspace(label):
            path = (root / label).resolve()
            path.mkdir()
            (path / "buffer.txt").write_text("DISK", encoding="utf-8")
            return path
        def bridge(label):
            peer = launch(label, [args.bridge], root)
            init, _ = peer.call("initialize", protocolVersion=1,
                clientCapabilities=dict(fs=dict(readTextFile=True, writeTextFile=True)))
            require(init.get("protocolVersion") == 1, "not ACP v1")
            return peer, init
        ws, sid = workspace("raw-workspace"), str(uuid.uuid4())
        session_dir = root / "raw-sessions"
        session_dir.mkdir()
        raw = launch("raw-live", raw_args(sid, session_dir), ws)
        ready(raw)
        raw.client = Client(ws, sid)
        raw.native("configure_file_io", readText=True, writeText=True)
        model.plan()
        require(raw.native("prompt", contentBlocks=blocks)["accepted"] is True, "native prompt not accepted")
        deadline = time.monotonic() + 60
        while True:
            frame = raw.receive(deadline)
            raw.handle(frame)
            if frame.get("type") == "agent_settled":
                require(frame.get("outcome") == "completed", "native did not complete")
                break
        paths = list(session_dir.rglob("*.jsonl"))
        require(len(paths) == 1, "raw transcript count")
        pages(raw, paths[0])
        raw.client.check()
        raw.close()
        check_durable(paths[0], blocks, raw.seen)
        state["rawNative"] = dict(transcript=str(paths[0]), sha256=sha(paths[0]), fileRequests=raw.client.requests)
        ws = workspace("acp-workspace")
        peer, init = bridge("acp-live")
        result, _ = peer.call("session/new", cwd=str(ws), mcpServers=[])
        sid = result["sessionId"]
        peer.client = Client(ws, sid)
        model.plan()
        result, frames = peer.call("session/prompt", sessionId=sid, prompt=blocks)
        require(result.get("stopReason") == "end_turn", "ACP prompt did not settle")
        peer.client.check()
        peer.call("session/close", sessionId=sid)
        peer.close()
        path = transport.transcript(root, sid)
        user_id, assistant_ids = check_durable(path, blocks, frames, acp=True)
        state["acp"] = dict(sessionId=sid, transcript=str(path), sha256=sha(path), fileRequests=peer.client.requests)
        probe = launch("acp-native-pages", raw_args(sid, root / "sessions" / sid, path), ws)
        ready(probe)
        pages(probe, path)
        probe.close()
        for method in ("load", "resume"):
            peer, init = bridge("fresh-" + method)
            require(method != "resume" or "resume" in init["agentCapabilities"].get("sessionCapabilities", {}), "resume not advertised")
            peer.client = Client(ws, sid)
            _, replay = peer.call("session/" + method, sessionId=sid, cwd=str(ws), mcpServers=[])
            updates = [f["params"]["update"] for f in replay if f.get("method") == "session/update"]
            chunks = [u for u in updates if u.get("sessionUpdate") in ("user_message_chunk", "agent_message_chunk", "agent_thought_chunk")]
            if method == "load":
                users = [u for u in chunks if u["sessionUpdate"] == "user_message_chunk"]
                require([u["content"] for u in users] == blocks and all(u.get("messageId") == user_id for u in users), "load user images/order/IDs differ")
                agents = [u for u in chunks if u["sessionUpdate"] == "agent_message_chunk"]
                require({u.get("messageId") for u in agents} == assistant_ids, "load changed live assistant IDs")
            else:
                require(not chunks and not any(u.get("sessionUpdate") in ("tool_call", "tool_call_update") for u in updates), "resume replayed history")
            peer.call("session/close", sessionId=sid)
            peer.close()
            wire = [json.loads(line)["message"] for line in (root / ("fresh-" + method + "-wire.jsonl")).read_text(encoding="utf-8").splitlines()
                    if json.loads(line).get("direction") == "out"]
            all_chunks = [f.get("params", {}).get("update", {}) for f in wire if f.get("method") == "session/update"]
            all_chunks = [u for u in all_chunks if u.get("sessionUpdate") in
                          ("user_message_chunk", "agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update")]
            expected_chunks = [u for u in updates if u.get("sessionUpdate") in
                               ("user_message_chunk", "agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update")]
            require(all_chunks == expected_chunks, "late restore replay after response")
            require(not peer.client.requests and len(model.calls) == 10, "restore executed model/file operations")
        require(len(model.calls) == 10 and not model.errors and model.steps.empty(), "provider execution/count failure")
        frozen("end")
        state["status"] = "passed"
    except BaseException:
        state.update(status="failed", failure=traceback.format_exc())
        raise
    finally:
        state["cleanupErrors"] = []
        for resource in reversed(owned):
            try:
                resource.close()
            except BaseException:
                state["cleanupErrors"].append(traceback.format_exc())
        if model:
            try:
                model.close()
            except BaseException:
                state["cleanupErrors"].append(traceback.format_exc())
        if state["cleanupErrors"]:
            state["status"] = "failed"
        dump(root / "results.json", state)
        if state["cleanupErrors"] and sys.exc_info()[0] is None:
            raise AssertionError("owned resource cleanup failed; inspect results.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("bridge", "rpi", "diagnostics-dir", "prebuild-inventory"):
        parser.add_argument("--" + flag, type=Path, required=True)
    for flag in ("bridge-sha", "native-sha"):
        parser.add_argument("--" + flag, required=True)
    args = parser.parse_args()
    require(args.prebuild_inventory.is_absolute() and args.prebuild_inventory.is_file(), "explicit parent prebuild inventory required")
    run(args)


if __name__ == "__main__":
    main()
