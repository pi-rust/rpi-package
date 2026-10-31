"""Isolated real-native ACP -> MCP stdio smoke (Python standard library only).

Run with explicit --bridge, --rpi and a NEW --diagnostics-dir under Cargo target
or TEMP. No builds, PATH binary lookup, SDK, installed configuration or real
credentials. All wire/model/fixture logs and failure evidence are retained.
Windows only: a kill-on-close Job contains each suspended bridge BEFORE it runs.
The same file implements the finite MCP fixture and its finite leaf process.
"""
import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import sys
import threading
import time
import traceback
import uuid
import zlib

TIMEOUT = 35
MODES = ["success", "image", "is_error", "unsupported", "oversize",
         "malformed_base64", "textSignature", "blocked"]
SCHEMA = {"type": "object", "properties": {
    "mode": {"type": "string", "enum": MODES}, "query": {"type": "string"}},
    "required": ["mode", "query"], "additionalProperties": False}
QUERY = "exact arguments / spaces / 雪"
TEXT = "MCP_NATIVE_TEXT_83ac"
STRUCTURED = {"answer": 42, "unicode": "雪", "nested": {"ok": True}}
def png_fixture():
    """One opaque red RGBA pixel, with real PNG chunk lengths and CRCs."""
    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data) & 0xffffffff))
    data = (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00\xff"))
            + chunk(b"IEND", b""))
    return base64.b64encode(data).decode("ascii")


PNG = png_fixture()


def dump(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def append(path, value):
    with path.open("a", encoding="utf-8") as out:
        out.write(json.dumps(value, ensure_ascii=False) + "\n")


def objects(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from objects(child)
    elif isinstance(value, list):
        for child in value:
            yield from objects(child)


def as_text(value):
    if isinstance(value, str):
        return value
    return "".join(x.get("text", "") for x in value or [] if isinstance(x, dict))


def wait_until(test, label, timeout=TIMEOUT):
    deadline = time.monotonic() + timeout
    while not test():
        assert time.monotonic() < deadline, "deadline: " + label
        time.sleep(.03)


def fixture(root, label):
    """Finite server; never interpret a client command as an executable."""
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    # Even if the harness crashes, every fixture and leaf has a finite lifetime.
    timer = threading.Timer(150, lambda: os._exit(91))
    timer.daemon = True
    timer.start()
    leaf = subprocess.Popen([sys.executable, "-B", str(Path(__file__).resolve()), "--leaf"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    identity = dict(pid=os.getpid(), leaf=leaf.pid, label=label, cwd=os.getcwd(),
                    value=os.environ.get("MCP_SESSION_VALUE"), empty=os.environ.get("MCP_EMPTY"),
                    home=os.environ.get("HOME"), agent=os.environ.get("RPI_CODING_AGENT_DIR"))
    dump(root / (label + "-" + str(os.getpid()) + ".identity.json"), identity)
    try:
        for line in sys.stdin:
            request = json.loads(line)
            append(root / (label + "-wire.jsonl"), request)
            method = request.get("method")
            if "id" not in request:
                continue
            result = {}
            if method == "initialize":
                result = dict(protocolVersion="2024-11-05", capabilities={"tools": {}},
                              serverInfo=dict(name="native-smoke-fixture", version="1"))
            elif method == "tools/list":
                result = {"tools": [dict(name="probe", description="NATIVE_MCP_PROBE_LABEL",
                                         inputSchema=SCHEMA)]}
            elif method == "tools/call":
                assert request["params"]["name"] == "probe", request
                args = request["params"]["arguments"]
                assert args == dict(mode=args["mode"], query=QUERY), args
                mode = args["mode"]
                append(root / (label + "-calls.jsonl"), dict(arguments=args, **identity))
                if mode == "blocked":
                    dump(root / (label + "-blocked.json"), dict(id=request["id"], **identity))
                    # Wait for owner cancellation; no spontaneous response or reconnect.
                    time.sleep(120)
                    continue
                result = dict(content=[dict(type="text", text=TEXT + ":" + label)],
                              structuredContent=STRUCTURED, isError=False)
                if mode == "image":
                    result["content"].append(dict(type="image", data=PNG, mimeType="image/png"))
                elif mode == "is_error":
                    result.update(content=[dict(type="text", text="MCP_EXPLICIT_ERROR")], isError=True)
                elif mode == "unsupported":
                    result["content"] = [dict(type="resource_link", uri="urn:smoke:unsupported", name="x")]
                elif mode == "oversize":
                    result["content"] = [dict(type="text", text="x" * (1100 * 1024))]
                elif mode == "malformed_base64":
                    result["content"] = [dict(type="image", data="not!base64", mimeType="image/png")]
                elif mode == "textSignature":
                    result["content"] = [dict(type="text", text=TEXT, textSignature={"invalid": True})]
            else:
                print(json.dumps(dict(jsonrpc="2.0", id=request["id"],
                      error=dict(code=-32601, message="fixture unsupported method"))), flush=True)
                continue
            print(json.dumps(dict(jsonrpc="2.0", id=request["id"], result=result)), flush=True)
    finally:
        timer.cancel()
        if leaf.poll() is None:
            leaf.kill()
        leaf.wait(timeout=5)


class Windows:
    """Only owned handles/PIDs; never taskkill by name or inspect user processes."""
    def __init__(self):
        assert os.name == "nt", "this smoke requires Windows Job containment"
        self.k = ctypes.WinDLL("kernel32", use_last_error=True)
        self.n = ctypes.WinDLL("ntdll")
        H, D, P = wintypes.HANDLE, wintypes.DWORD, ctypes.c_void_p
        for name, args, result in [
            ("CreateJobObjectW", [P, wintypes.LPCWSTR], H),
            ("SetInformationJobObject", [H, ctypes.c_int, P, D], wintypes.BOOL),
            ("AssignProcessToJobObject", [H, H], wintypes.BOOL),
            ("TerminateJobObject", [H, wintypes.UINT], wintypes.BOOL),
            ("OpenProcess", [D, wintypes.BOOL, D], H),
            ("GetExitCodeProcess", [H, ctypes.POINTER(D)], wintypes.BOOL),
            ("CloseHandle", [H], wintypes.BOOL)]:
            fn = getattr(self.k, name)
            fn.argtypes, fn.restype = args, result
        self.n.NtResumeProcess.argtypes = [H]
        self.n.NtResumeProcess.restype = ctypes.c_long

    def check(self, result):
        if not result:
            raise ctypes.WinError(ctypes.get_last_error())
        return result

    def job(self):
        class Basic(ctypes.Structure):
            _fields_ = [("user", ctypes.c_longlong), ("job", ctypes.c_longlong),
                        ("flags", wintypes.DWORD), ("min", ctypes.c_size_t),
                        ("max", ctypes.c_size_t), ("active", wintypes.DWORD),
                        ("affinity", ctypes.c_size_t), ("priority", wintypes.DWORD),
                        ("scheduling", wintypes.DWORD)]
        class Extended(ctypes.Structure):
            _fields_ = [("basic", Basic), ("io", ctypes.c_ulonglong * 6),
                        ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                        ("peak_process", ctypes.c_size_t), ("peak_job", ctypes.c_size_t)]
        handle = self.check(self.k.CreateJobObjectW(None, None))
        limits = Extended()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; NO breakaway.
        try:
            self.check(self.k.SetInformationJobObject(handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)))
        except BaseException:
            self.k.CloseHandle(handle)
            raise
        return handle

    def open_live(self, pid):
        handle = self.check(self.k.OpenProcess(0x1000 | 0x100000, False, pid))
        if not self.alive(handle):
            self.k.CloseHandle(handle)
            raise AssertionError(("fixture was not positively live", pid))
        return handle

    def alive(self, handle):
        code = wintypes.DWORD()
        self.check(self.k.GetExitCodeProcess(handle, ctypes.byref(code)))
        return code.value == 259


class Peer:
    def __init__(self, bridge, env, root, name, win):
        self.win, self.job = win, win.job()
        self.root, self.name, self.counter = root, name, 0
        self.output, self.lock = queue.Queue(), threading.Lock()
        self.process = None
        self.readers = []
        self.closed = False
        self.stderr = (root / (name + ".stderr.log")).open("w", encoding="utf-8")
        try:
            self.process = subprocess.Popen([str(bridge)], cwd=root, env=env,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr,
                text=True, encoding="utf-8", creationflags=0x4 | 0x08000000)
            win.check(win.k.AssignProcessToJobObject(self.job, int(self.process._handle)))
            assert win.n.NtResumeProcess(int(self.process._handle)) == 0, "NtResumeProcess failed"
            reader = threading.Thread(target=self.pump, daemon=True)
            self.readers.append(reader)
            reader.start()
        except BaseException:
            if self.process is not None:
                self.process.kill()
                self.process.wait(timeout=5)
            win.k.CloseHandle(self.job)
            self.stderr.close()
            raise

    def record(self, direction, value):
        with self.lock:
            append(self.root / (self.name + "-wire.jsonl"), dict(direction=direction, message=value))

    def pump(self):
        try:
            for line in self.process.stdout:
                value = json.loads(line)
                self.record("out", value)
                self.output.put(value)
        except BaseException as error:
            self.output.put(error)
        finally:
            self.output.put(EOFError("bridge stdout closed"))

    def send(self, method, params, notification=False):
        self.counter += 1
        value = dict(jsonrpc="2.0", method=method, params=params)
        if not notification:
            value["id"] = self.counter
        self.record("in", value)
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()
        return self.counter

    def response(self, request_id, expect_error=False):
        updates, deadline = [], time.monotonic() + TIMEOUT
        while True:
            try:
                value = self.output.get(timeout=max(.01, deadline - time.monotonic()))
            except queue.Empty:
                raise AssertionError("ACP timeout; see " + self.name) from None
            if isinstance(value, BaseException):
                raise value
            assert "method" not in value or "id" not in value, ("unexpected ACP client request", value)
            if value.get("id") == request_id:
                if expect_error:
                    assert "error" in value and "result" not in value, value
                    return value["error"], updates
                assert "error" not in value, value
                return value["result"], updates
            updates.append(value)

    def call(self, method, **params):
        return self.response(self.send(method, params))

    def close(self):
        if self.closed:
            return
        self.closed = True
        try:
            self.process.stdin.close()
            assert self.process.wait(timeout=20) == 0, "bridge nonzero exit"
            for reader in self.readers:
                reader.join(timeout=3)
            assert not any(t.is_alive() for t in self.readers), "bridge pipe not joined"
        finally:
            # Also on timeout/error: kill the ENTIRE owned tree, not only bridge.
            self.win.k.TerminateJobObject(self.job, 92)
            self.win.k.CloseHandle(self.job)
            if self.process.poll() is None:
                self.process.wait(timeout=5)
            for reader in self.readers:
                reader.join(timeout=5)
            self.process.stdout.close()
            self.stderr.close()


class Model:
    def __init__(self, root):
        self.calls, self.errors, self.selections = [], [], []
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(10)
            def log_message(self, *_args):
                pass
            def do_GET(self):
                owner.errors.append("unexpected GET " + self.path)
                self.send_error(404)
            def do_POST(self):
                try:
                    assert self.path == "/v1/chat/completions", self.path
                    size = int(self.headers["Content-Length"])
                    assert 0 < size < 8 * 1024 * 1024, size
                    call = json.loads(self.rfile.read(size))
                    owner.calls.append(call)
                    dump(root / "provider-requests.json", owner.calls)
                    messages = call["messages"]
                    # Image projection may append a synthetic user image message.
                    # Select the actual harness prompt, not that synthetic message.
                    user_index = next(i for i in range(len(messages) - 1, -1, -1)
                        if messages[i]["role"] == "user" and
                        as_text(messages[i].get("content")).startswith(("MCP:", "MODEL_")))
                    prompt = as_text(messages[user_index]["content"])
                    fields = prompt.split(":")
                    delta = dict(role="assistant", content="MODEL_OK:" + prompt)
                    reason = "stop"
                    if fields[0] == "MCP" and not any(
                            m.get("role") == "tool" for m in messages[user_index + 1:]):
                        _, label, mode = fields
                        tools = [t["function"] for t in call["tools"]
                                 if t.get("function", {}).get("name", "").startswith("mcp_")]
                        assert len(tools) == 1, tools
                        tool = tools[0]
                        assert "MCP server: " + label + "\nMCP tool: probe\n" in tool["description"], tool
                        assert "NATIVE_MCP_PROBE_LABEL" in tool["description"], tool
                        assert tool["parameters"] == SCHEMA, tool
                        owner.selections.append(dict(prompt=prompt, name=tool["name"]))
                        delta = dict(role="assistant", tool_calls=[dict(index=0,
                            id="mcp-call-" + uuid.uuid4().hex, type="function", function=dict(
                            name=tool["name"], arguments=json.dumps(dict(mode=mode, query=QUERY))))])
                        reason = "tool_calls"
                    events = [dict(choices=[dict(index=0, delta=delta, finish_reason=None)]),
                              dict(choices=[dict(index=0, delta={}, finish_reason=reason)],
                                   usage=dict(prompt_tokens=50, completion_tokens=10, total_tokens=60))]
                    data = ("".join("data: " + json.dumps(e) + "\n\n" for e in events)
                            + "data: [DONE]\n\n").encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    pass
                except BaseException:
                    owner.errors.append(traceback.format_exc())
                    dump(root / "provider-errors.json", owner.errors)
                    self.close_connection = True
        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.05), daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=10)
        assert not self.thread.is_alive(), "model thread did not join"
        assert not self.errors, self.errors


def environment(root, model, rpi):
    allowed = {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "SYSTEMDRIVE",
               "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}
    env = {k: v for k, v in os.environ.items() if k.upper() in allowed}
    for name in ("home", "agent", "tmp", "sessions", "appdata", "localappdata"):
        (root / name).mkdir()
    env.update(HOME=str(root / "home"), USERPROFILE=str(root / "home"),
        APPDATA=str(root / "appdata"), LOCALAPPDATA=str(root / "localappdata"),
        XDG_CONFIG_HOME=str(root / "home/.config"), XDG_CACHE_HOME=str(root / "home/.cache"),
        XDG_DATA_HOME=str(root / "home/.local/share"), TEMP=str(root / "tmp"),
        TMP=str(root / "tmp"), TMPDIR=str(root / "tmp"), NO_COLOR="1", RPI_OFFLINE="1",
        RPI_CODING_AGENT_DIR=str(root / "agent"), RPI_ACP_SESSION_DIR=str(root / "sessions"),
        RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_ACP_NO_PERMISSION_GATE="1", RPI_BIN=str(rpi))
    dump(root / "agent/auth.json", {})
    dump(root / "agent/settings.json", dict(defaultProvider="fixture", defaultModel="synthetic",
                                          defaultThinkingLevel="off"))
    dump(root / "agent/models.json", {"providers": {"fixture": dict(api="openai-completions",
        baseUrl=f"http://127.0.0.1:{model.server.server_port}/v1", apiKey="local-dummy-not-a-credential",
        models=[dict(id="synthetic", name="synthetic", reasoning=False, input=["text", "image"],
                     contextWindow=100000, maxTokens=1024)])}})
    return env


class Audit:
    def __init__(self, root, win):
        self.root, self.win, self.handles = root, win, {}

    def capture(self, label, cwd):
        paths = list(self.root.glob(label + "-*.identity.json"))
        assert len(paths) == 1, ("expected exactly ONE fixture spawn", label, paths)
        current = []
        for path in paths:
            item = json.loads(path.read_text(encoding="utf-8"))
            if item["pid"] in self.handles:
                continue
            assert Path(item["cwd"]).resolve() == cwd.resolve(), item
            assert item["value"] == "value:" + label + ":雪" and item["empty"] == "", item
            assert Path(item["home"]).resolve() == self.root / "home", item
            assert Path(item["agent"]).resolve() == self.root / "agent", item
            for pid in (item["pid"], item["leaf"]):
                self.handles[pid] = self.win.open_live(pid)
                append(self.root / "pid-audit.jsonl", dict(pid=pid, label=label, live=True))
                current.append(pid)
        assert current, ("no NEW positively live fixture/leaf", label)
        return current

    def dead(self, pids):
        for pid in pids:
            wait_until(lambda: not self.win.alive(self.handles[pid]), "child/leaf still alive " + str(pid), 15)
            append(self.root / "pid-audit.jsonl", dict(pid=pid, dead=True))

    def close(self):
        try:
            self.dead(list(self.handles))
        finally:
            for handle in self.handles.values():
                self.win.k.CloseHandle(handle)


def config(root, label):
    return [dict(name=label, command=sys.executable,
        args=["-B", str(Path(__file__).resolve()), "--fixture", str(root), label],
        env=[dict(name="MCP_SESSION_VALUE", value="value:" + label + ":雪"),
             dict(name="MCP_EMPTY", value="")])]


def prompt(peer, sid, text):
    result, updates = peer.call("session/prompt", sessionId=sid, prompt=[dict(type="text", text=text)])
    assert result.get("stopReason") == "end_turn", (result, updates)
    assert "MODEL_OK:" + text in json.dumps(updates, ensure_ascii=False), updates
    return updates


def roundtrip(peer, model, sid, label, mode):
    start = len(model.calls)
    updates = prompt(peer, sid, "MCP:" + label + ":" + mode)
    assert len(model.calls) == start + 2, "expected exactly one real tool roundtrip"
    assert any(o.get("sessionUpdate") == "tool_call" for o in objects(updates)), updates
    tool_messages = [m for m in model.calls[start + 1]["messages"] if m.get("role") == "tool"]
    assert tool_messages, "native tool result missing from provider request"
    text = as_text(tool_messages[-1].get("content"))
    if mode == "success":
        assert TEXT + ":" + label in text, text
        decoder, values = json.JSONDecoder(), []
        for index, char in enumerate(text):
            if char == "{":
                try:
                    values.append(decoder.raw_decode(text[index:])[0])
                except ValueError:
                    pass
        assert STRUCTURED in values, ("structuredContent JSON projection missing", text)
    if mode == "image":
        # Frozen native prepare_inline_image retains small supported images with
        # no orientation transform. Check the actual provider image block, not
        # an incidental base64 string in text or raw result metadata.
        images = [o for o in objects(model.calls[start + 1]["messages"])
                  if o.get("type") == "image_url"]
        assert any(o.get("image_url", {}).get("url") == "data:image/png;base64," + PNG
                   for o in images), ("image not projected to provider", images)
    errors = dict(is_error="MCP_EXPLICIT_ERROR", unsupported="unsupported external tool content block",
                  oversize="external tool result is too large",
                  malformed_base64="unsupported external tool content block",
                  textSignature="unsupported external tool content block")
    if mode in errors:
        assert errors[mode] in text, (mode, text)
        assert any(o.get("sessionUpdate") == "tool_call_update" and o.get("status") == "failed"
                   for o in objects(updates)), ("ACP did not mark tool failed", updates)


def transcript(root, sid):
    paths = list((root / "sessions" / sid).rglob("*.jsonl"))
    assert len(paths) == 1, paths
    entries = [json.loads(line) for line in paths[0].read_text(encoding="utf-8").splitlines()]
    assert entries[0].get("kind") == "header" and entries[0].get("version") == 4, entries[0]
    return entries


def verify_native(root, sid, expected_modes):
    entries = transcript(root, sid)
    results = [o for o in objects(entries) if o.get("role") == "toolResult"]
    assert len(results) >= len(expected_modes), ("native toolResult missing", results)
    raw = [o["externalToolResult"] for o in objects(results) if "externalToolResult" in o]
    assert any(o.get("structuredContent") == STRUCTURED for o in raw), raw
    assert any(TEXT in json.dumps(o) for o in raw), raw
    serialized = json.dumps(entries)
    # Exact native v4 operation wire shape, correlated to each original prompt.
    started = [e for e in entries if e.get("kind") == "record" and e.get("type") == "operation_started"]
    finished = [e for e in entries if e.get("kind") == "record" and e.get("type") == "operation_finished"]
    assert started and finished, "native operation records missing"
    for operation in started:
        ends = [e for e in finished if e.get("runId") == operation["id"]]
        assert len(ends) == 1, ("unpaired native operation", operation, ends)
        original = json.dumps(operation.get("intent", {}).get("originalPrompt"))
        expected = "aborted" if ":blocked" in original else "completed"
        assert ends[0]["outcome"] == expected, (operation, ends)
    if "image" in expected_modes:
        assert any(o.get("type") == "image" and o.get("data") == PNG for o in objects(raw)), raw
    for mode, error in [("is_error", "MCP_EXPLICIT_ERROR"),
                        ("unsupported", "unsupported external tool content block"),
                        ("oversize", "external tool result is too large"),
                        ("malformed_base64", "unsupported external tool content block"),
                        ("textSignature", "unsupported external tool content block")]:
        if mode in expected_modes:
            assert any(o.get("isError") is True and error in json.dumps(o) for o in results), (mode, results)
    assert "MCP_SESSION_VALUE" not in serialized and "value:" not in serialized, "MCP config/env persisted"
    dump(root / (sid + "-native-validation.json"), dict(files="native v4 JSONL", toolResults=len(results), modes=expected_modes))


def run(args):
    root = args.diagnostics_dir.resolve()
    assert not root.exists(), "use a new diagnostics directory; preserve old evidence"
    allowed = [args.build_marker.resolve().parent, Path(os.environ["TEMP"]).resolve()]
    assert any(root.is_relative_to(p) and root != p for p in allowed), "diagnostics must be under build evidence/TEMP"
    root.mkdir(parents=True)
    state = dict(passed=[], phase="provenance", status="running")
    peer = model = audit = None
    def landed(name):
        state["passed"].append(name)
        dump(root / "results.json", state)
    try:
        assert args.build_marker.is_file(), "parent build marker missing; do not execute binaries"
        marker = json.loads(args.build_marker.read_text(encoding="utf-8"))
        build = json.loads(args.native_build_record.read_text(encoding="utf-8"))
        verification_text = args.native_verification.read_text(encoding="utf-8")
        verification = json.loads(verification_text)
        actual = hashlib.sha256(args.rpi.read_bytes()).hexdigest()
        bridge_sha = hashlib.sha256(args.bridge.read_bytes()).hexdigest()
        dump(root / "provenance.json", dict(native=str(args.rpi), sha256=actual,
            expected=args.native_sha, native_build=build, source_verification=verification,
            bridge=str(args.bridge), bridge_sha256=bridge_sha, bridge_build_marker=marker))
        assert actual == args.native_sha, "native binary hash MISMATCH; stopped before execution"
        assert build["exitCode"] == 0, "native build provenance failed"
        assert args.native_sha in verification_text, "native verification hash missing"
        assert marker["build"]["exitCode"] == 0, "bridge build provenance failed"
        assert Path(marker["bridge"]).resolve() == args.bridge, "bridge marker path mismatch"
        assert marker["sha256"] == bridge_sha, "bridge marker hash mismatch"
        win = Windows()
        model = Model(root)
        env = environment(root, model, args.rpi)
        audit = Audit(root, win)
        first, second = root / "workspace-one", root / "workspace-two"
        first.mkdir()
        second.mkdir()
        peer = Peer(args.bridge, env, root, "create-close", win)
        peer.call("initialize", protocolVersion=1)
        state["phase"] = "create-once-and-close"
        sid = peer.call("session/new", cwd=str(first), mcpServers=config(root, "create"))[0]["sessionId"]
        pids = audit.capture("create", first)
        assert not model.calls, "session creation unexpectedly contacted model"
        peer.call("session/close", sessionId=sid)
        audit.dead(pids)
        peer.close()
        peer = None
        landed("create-once/close: fixture+leaf positively live then dead")

        peer = Peer(args.bridge, env, root, "roundtrip", win)
        peer.call("initialize", protocolVersion=1)
        sid = peer.call("session/new", cwd=str(first), mcpServers=config(root, "one"))[0]["sessionId"]
        one_pids = audit.capture("one", first)
        sid2 = peer.call("session/new", cwd=str(second), mcpServers=config(root, "two"))[0]["sessionId"]
        two_pids = audit.capture("two", second)
        state["phase"] = "native-tool-success"
        roundtrip(peer, model, sid, "one", "success")
        landed("success: discovered hashed tool, exact schema/arguments, text and structuredContent")
        prompt(peer, sid2, "MCP:two:success")
        assert model.selections[0]["name"] != model.selections[-1]["name"], "session tool identities crossed"
        for label, cwd in [("one", first), ("two", second)]:
            calls = [json.loads(l) for l in (root / (label + "-calls.jsonl")).read_text(encoding="utf-8").splitlines()]
            assert calls and all(c["label"] == label and Path(c["cwd"]).resolve() == cwd for c in calls), calls
        peer.call("session/close", sessionId=sid2)
        audit.dead(two_pids)
        assert all(win.alive(audit.handles[p]) for p in one_pids), "closing session two killed session one"
        landed("two-session cwd/env/argument and lifecycle isolation")

        state["phase"] = "blocked-call-cancel-and-model-only-recovery"
        request_id = peer.send("session/prompt", dict(sessionId=sid,
            prompt=[dict(type="text", text="MCP:one:blocked")]))
        wait_until(lambda: (root / "one-blocked.json").is_file(), "MCP call did not enter blocked fixture")
        peer.send("session/cancel", dict(sessionId=sid), notification=True)
        reply, _ = peer.response(request_id)
        assert reply.get("stopReason") == "cancelled", reply
        count = len(model.selections)
        prompt(peer, sid, "MODEL_ONLY_AFTER_CANCEL")
        assert len(model.selections) == count, "recovery unexpectedly called cancelled MCP target"
        peer.call("session/close", sessionId=sid)
        audit.dead(one_pids)
        peer.close()
        peer = None
        verify_native(root, sid, ["success"])
        landed("blocked MCP cancellation, next model-only prompt, native JSONL toolResult/operation")

        for method, label in [("session/load", "load"), ("session/resume", "resume")]:
            state["phase"] = method + "-reprovide"
            peer = Peer(args.bridge, env, root, label, win)
            peer.call("initialize", protocolVersion=1)
            before = len(model.calls)
            bad_label = label + "-wrong-cwd"
            bad_config = config(root, bad_label)
            bad_config[0]["command"] = os.path.relpath(sys.executable, second)
            assert not Path(bad_config[0]["command"]).is_absolute(), bad_config
            identities_before = set(root.glob("*.identity.json"))
            error, rejected_updates = peer.response(peer.send(method, dict(
                sessionId=sid, cwd=str(second), mcpServers=bad_config)), expect_error=True)
            assert "saved session workspace differs" in json.dumps(error), error
            assert not rejected_updates, ("rejected restore replayed history", rejected_updates)
            assert set(root.glob("*.identity.json")) == identities_before, "rejected restore spawned MCP"
            assert len(model.calls) == before, "rejected restore contacted provider"
            landed(method + ": recorded cwdA/request cwdB rejected before MCP spawn (relative command)")
            _, replay = peer.call(method, sessionId=sid, cwd=str(first), mcpServers=config(root, label))
            assert len(model.calls) == before, "history replay triggered a model/tool side effect"
            pids = audit.capture(label, first)
            if method == "session/load":
                assert TEXT in json.dumps(replay) and "MODEL_ONLY_AFTER_CANCEL" in json.dumps(replay), "native replay lost tool/recovery history"
            roundtrip(peer, model, sid, label, "success")
            # Mandatory create/success/cancel recovery has already landed. Run
            # the output matrix on a fresh target, never reconnect the cancelled one.
            if method == "session/load":
                for mode in MODES[1:-1]:
                    state["phase"] = "native-tool-" + mode
                    roundtrip(peer, model, sid, label, mode)
                    # A successful MCP call, not only a model-only prompt, must
                    # follow every invalid/error output on the SAME host.
                    if mode != "image":
                        roundtrip(peer, model, sid, label, "success")
                    landed("tool roundtrip + next successful call: " + mode)
            peer.call("session/close", sessionId=sid)
            audit.dead(pids)
            peer.close()
            peer = None
            verify_native(root, sid, MODES[:-1])
            landed(method + ": fresh bridge, reprovided config, no side effects on replay, new MCP call")
        state["phase"] = "missing-recorded-directory-fallback"
        missing_a = root / "fallback-workspace-A"
        missing_a.mkdir()
        peer = Peer(args.bridge, env, root, "fallback-create", win)
        peer.call("initialize", protocolVersion=1)
        fallback_sid = peer.call("session/new", cwd=str(missing_a),
            mcpServers=config(root, "fallback-original"))[0]["sessionId"]
        pids = audit.capture("fallback-original", missing_a)
        before = len(model.calls)
        peer.call("session/close", sessionId=fallback_sid)
        audit.dead(pids)
        peer.close()
        peer = None
        saved = transcript(root, fallback_sid)
        assert len(saved) == 1, ("fallback session must have no prior model turns", saved)
        assert Path(saved[0]["cwd"]).resolve() == missing_a.resolve(), saved[0]
        missing_a.rename(root / "fallback-workspace-A-moved")
        assert not missing_a.exists()
        peer = Peer(args.bridge, env, root, "fallback-load", win)
        peer.call("initialize", protocolVersion=1)
        peer.call("session/load", sessionId=fallback_sid, cwd=str(second),
                  mcpServers=config(root, "fallback-loaded"))
        pids = audit.capture("fallback-loaded", second)
        assert len(model.calls) == before, "fallback restore contacted model"
        assert transcript(root, fallback_sid) == saved, "restore mutated native transcript/header"
        roundtrip(peer, model, fallback_sid, "fallback-loaded", "success")
        peer.call("session/close", sessionId=fallback_sid)
        audit.dead(pids)
        peer.close()
        peer = None
        verify_native(root, fallback_sid, ["success"])
        landed("missing recorded directory: real saved header, rename after close, load uses cwdB")
        state["status"] = "passed"
    except BaseException:
        state.update(status="failed", failure=traceback.format_exc())
        raise
    finally:
        cleanup = []
        for name, item in [("bridge", peer), ("model", model), ("pid-audit", audit)]:
            if item is not None:
                try:
                    item.close()
                except BaseException:
                    cleanup.append(dict(resource=name, error=traceback.format_exc()))
        state["cleanup_errors"] = cleanup
        if cleanup:
            state["status"] = "failed"
        dump(root / "results.json", state)
        if cleanup:
            raise AssertionError("cleanup failed; see " + str(root / "results.json"))
    print("PASS: " + "; ".join(state["passed"]))


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--leaf":
        time.sleep(145)
        return
    if len(sys.argv) > 1 and sys.argv[1] == "--fixture":
        fixture(Path(sys.argv[2]), sys.argv[3])
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", required=True, type=Path)
    parser.add_argument("--rpi", required=True, type=Path)
    parser.add_argument("--diagnostics-dir", required=True, type=Path)
    parser.add_argument("--build-marker", required=True, type=Path)
    parser.add_argument("--native-build-record", required=True, type=Path)
    parser.add_argument("--native-verification", required=True, type=Path)
    parser.add_argument("--native-sha", required=True)
    args = parser.parse_args()
    args.bridge, args.rpi = args.bridge.resolve(), args.rpi.resolve()
    run(args)


if __name__ == "__main__":
    main()
