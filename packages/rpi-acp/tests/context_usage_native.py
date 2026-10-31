"""Stage9 Windows real-native ACP usage_update proof (authoring is NOT a pass).

SAFE prebuild operation: --write-source-manifest <NEW absolute target JSON>.
Parent must then freeze/build the bridge and bind that exact manifest using the
session_list_native.py marker schema. Execution requires ALL explicit arguments;
never builds, discovers binaries, loads a fixture DLL, or changes native source.
Native source must be stage3/native-external-tools at 26842b37, with the complete
prebuild native inventory/build/verification gates used by session_list_native.

Only stable ACP fields used/size are required; cost must be ABSENT. Initial
usage follows successful new/load/resume responses; subsequent checks are at
prompt/compact settlement, NOT live polling. Independent native RPC probes load
the actual saved JSONL after ACP close, issue ONLY raw get_state, then EOF. Native
startup may append metadata/normalize its own state: before/after hashes and
sizes are recorded, never restored or rewritten by this harness.

This tests the native compaction-aware estimate, NOT an exact tokenizer or a
bounded native get_state implementation (native can scan the full branch).
No credential reads, external API, SDK changes, installation or deployment.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, HTTPServer

sys.dont_write_bytecode = True
import mcp_stdio_native as h
import session_list_native as s

s.TARGET = Path("C:/Users/bigfish/.cargo/target/acp-stage9-01")
s.PYTHON_INPUTS = set(s.PYTHON_INPUTS) | {
    "packages/rpi-acp/tests/context_usage_native.py",
    "packages/rpi-acp/tests/session_list_native.py",
    "packages/rpi-acp/tests/mcp_stdio_native.py",
    "packages/rpi-acp/tests/mcp_http_fixture.py",
}
require = s.require
NATIVE_SOURCE = Path("C:/Users/bigfish/.codex/worktrees/rpi-acp-stage3/native-external-tools")
NATIVE_HEAD = "26842b37b5d7efe07a67f5c883b4008b0c0ac794"
WINDOW = 100000
LIMIT = 16 * 1024 * 1024
FILE_TEXT = "CONTEXT_USAGE_NATIVE_READ_" + "7b39e02f_unique_workspace_only\n"
SUMMARY = "LOCAL_CONTEXT_USAGE_SUMMARY_42e9: retain the workspace read and decisions."


class Transport(s.Peer):
    """Same suspended-launch/owned Job contract, bounded BOTH pipes and queue.

    Custom argv is needed for the independent native probe, not a shim runtime.
    Reader failure is an out-of-band terminal event so a full queue cannot hide
    overflow or strand a cleanup thread. h.Peer.close owns EOF/Job termination.
    """
    def __init__(self, argv, env, root, name, win, cwd):
        self.win, self.job = win, win.job()
        self.root, self.name, self.counter = root, name, 0
        self.output, self.lock = queue.Queue(maxsize=256), threading.Lock()
        self.process, self.readers, self.closed = None, [], False
        self.wire_bytes = self.frame_count = 0
        self.failed, self.failure = threading.Event(), None
        self.stderr = (root / (name + ".stderr.log")).open("w", encoding="utf-8")
        try:
            self.process = subprocess.Popen([str(x) for x in argv], cwd=cwd, env=env,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=True, encoding="utf-8", creationflags=0x4 | 0x08000000)
            win.check(win.k.AssignProcessToJobObject(self.job, int(self.process._handle)))
            require(win.n.NtResumeProcess(int(self.process._handle)) == 0, "NtResumeProcess failed")
            h.append(root / "pid-audit.jsonl", dict(peer=name, pid=self.process.pid,
                argv=[str(x) for x in argv], suspendedAssignedBeforeResume=True))
            for target in (self.pump, self.pump_stderr):
                reader = threading.Thread(target=target, daemon=True)
                self.readers.append(reader)
                reader.start()
        except BaseException:
            if self.process is not None:
                self.process.kill()
                self.process.wait(timeout=5)
            win.k.TerminateJobObject(self.job, 92)
            win.k.CloseHandle(self.job)
            self.stderr.close()
            raise

    def record(self, direction, value):
        super().record(direction, value)
        if direction == "out":
            update = value.get("params", {}).get("update", {})
            if update.get("sessionUpdate") == "usage_update":
                require("cost" not in update, "cost present without currency provenance")

    def fail_reader(self, error):
        self.failure = error
        self.failed.set()

    def pump(self):
        try:
            with (self.root / (self.name + ".stdout.log")).open("w", encoding="utf-8") as raw:
                total = 0
                while True:
                    line = self.process.stdout.readline(1024 * 1024 + 1)
                    if not line:
                        self.output.put_nowait(EOFError("owned stdout closed"))
                        return
                    total += len(line.encode("utf-8"))
                    require(total <= LIMIT and len(line) <= 1024 * 1024 and line.endswith("\n"),
                            "stdout/frame bound exceeded")
                    raw.write(line)
                    raw.flush()
                    frame = json.loads(line)
                    self.record("out", frame)
                    self.output.put_nowait(frame)
        except BaseException as error:
            self.fail_reader(error)

    def pump_stderr(self):
        try:
            total = 0
            while True:
                line = self.process.stderr.readline(65537)
                if not line:
                    return
                total += len(line.encode("utf-8"))
                require(len(line) <= 65536 and total <= LIMIT, "stderr bound exceeded")
                self.stderr.write(line)
                self.stderr.flush()
        except BaseException as error:
            self.fail_reader(error)

    def receive(self, deadline):
        while time.monotonic() < deadline:
            if self.failed.is_set():
                raise self.failure
            try:
                value = self.output.get(timeout=min(.1, max(.01, deadline - time.monotonic())))
            except queue.Empty:
                continue
            if isinstance(value, BaseException):
                raise value
            return value
        raise AssertionError("bounded wire response timeout: " + self.name)

    def response(self, request_id, expect_error=False):
        require(not expect_error, "this proof expects only successful operations")
        frames, deadline = [], time.monotonic() + 45
        while True:
            value = self.receive(deadline)
            require(not ("id" in value and "method" in value), "unexpected ACP client request")
            if "id" in value:
                require(value["id"] == request_id and "error" not in value and "result" in value,
                        "unexpected/failed ACP response")
                return value["result"], frames
            frames.append(value)

    def close(self):
        if self.closed:
            return
        try:
            super().close()
        finally:
            if self.process and self.process.stderr:
                self.process.stderr.close()
        require(not self.failed.is_set(), "reader failure: " + str(self.failure))


def usage(frame, sid):
    params = frame.get("params", {})
    update = params.get("update", {})
    if frame.get("method") == "session/update":
        require(params.get("sessionId") == sid, "notification leaked another session's state")
    if update.get("sessionUpdate") != "usage_update":
        return None
    require("cost" not in update, "cost must be absent: no currency provenance")
    require(type(update.get("used")) is int and update["used"] >= 0 and
            type(update.get("size")) is int and update["size"] > 0, "invalid stable usage fields")
    return {"used": update["used"], "size": update["size"]}


def settled_usage(peer, sid, frames=(), initial=False):
    found = [u for f in frames if (u := usage(f, sid)) is not None]
    require(not (initial and found), "initial usage preceded successful session response")
    deadline = time.monotonic() + 12
    while not found:
        frame = peer.receive(deadline)
        require("id" not in frame, "unexpected response while awaiting usage")
        value = usage(frame, sid)
        if value is not None:
            found.append(value)
    require(len(found) == 1, "duplicate usage in one settlement/initialization")
    return found[0]


class Model:
    """Single bounded loopback HTTP server; planned replies, durable SSE usage.

    prompt_tokens INCLUDES cache read/write; native splits it before computing
    totalTokens. Report final usage in a separate choices=[] OpenAI SSE frame,
    not only in a transient delta. Every request/reply plan is retained.
    """
    def __init__(self, root):
        self.calls, self.errors = [], []
        self.plans = queue.Queue(maxsize=12)
        self.root = root
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(8)
            def log_message(self, *_):
                pass
            def do_POST(self):
                try:
                    require(self.path == "/v1/chat/completions", "unexpected provider route")
                    size = int(self.headers.get("Content-Length", "0"))
                    require(0 < size <= 1024 * 1024 and len(owner.calls) < 12,
                            "provider request bound exceeded")
                    raw = self.rfile.read(size)
                    require(len(raw) == size, "partial provider request")
                    body = json.loads(raw)
                    plan = owner.plans.get_nowait()
                    owner.calls.append(dict(request=body, responsePlan=plan))
                    h.dump(root / "provider-requests.json", owner.calls)
                    if plan["kind"] == "read":
                        require(any(t.get("function", {}).get("name") == "read"
                                    for t in body.get("tools", [])), "builtin read not offered")
                        delta = dict(role="assistant", tool_calls=[dict(index=0,
                            id="context-native-read", type="function", function=dict(name="read",
                            arguments=json.dumps(dict(path="usage-proof.txt"))))])
                        reason = "tool_calls"
                    else:
                        if plan["kind"] == "after-read":
                            results = [m for m in body["messages"] if m.get("role") == "tool"]
                            require(results and FILE_TEXT.strip() in h.as_text(results[-1].get("content")),
                                    "actual unique native read result did not reach provider")
                        delta = dict(role="assistant", content=plan["text"])
                        reason = "stop"
                    events = [dict(choices=[dict(index=0, delta=delta, finish_reason=None)]),
                              dict(choices=[dict(index=0, delta={}, finish_reason=reason)]),
                              dict(choices=[], usage=plan["usage"])]
                    data = ("".join("data: " + json.dumps(e) + "\n\n" for e in events)
                            + "data: [DONE]\n\n").encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                    self.wfile.flush()
                except BaseException:
                    owner.errors.append(traceback.format_exc())
                    h.dump(root / "provider-errors.json", owner.errors)
                    self.close_connection = True
        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.server.timeout = 1
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.05), daemon=True)
        self.thread.start()

    def plan(self, kind, prompt, output, text="CONTEXT_NATIVE_OK"):
        self.plans.put_nowait(dict(kind=kind, text=text, usage=dict(prompt_tokens=prompt,
            completion_tokens=output, total_tokens=prompt + output,
            prompt_tokens_details=dict(cached_tokens=17 if prompt else 0,
                                       cache_write_tokens=11 if prompt else 0))))

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=12)
        require(not self.thread.is_alive() and not self.errors and self.plans.empty(),
                "provider cleanup/errors/unconsumed plans; inspect evidence")


def transcript(root, sid):
    paths = list((root / "sessions" / sid).rglob("*.jsonl"))
    require(len(paths) == 1 and paths[0].stat().st_size <= LIMIT, "missing/oversized native JSONL")
    return paths[0]


def disk_state(path):
    return dict(sha256=s.sha(path), bytes=path.stat().st_size, mtimeNs=path.stat().st_mtime_ns)


def native_context(value):
    context = value.get("context")
    require(isinstance(context, dict), "native get_state.context missing")
    for key in ("tokens", "window", "trailingTokens"):
        require(type(context.get(key)) is int and context[key] >= 0, "invalid native context: " + key)
    require(type(context.get("estimated")) is bool, "native estimated flag missing")
    return context


def projected(context):
    return dict(used=context["tokens"], size=context["window"])


def estimate_plain(message):
    """Native tokens.rs byte-length ceil/4 for our text-only retained tail."""
    content = message.get("content", [])
    if isinstance(content, str):
        chars = len(content.encode("utf-8"))
    else:
        require(isinstance(content, list) and all(b.get("type") == "text" for b in content),
                "unexpected retained-tail type; do not invent an estimate")
        chars = sum(len(b["text"].encode("utf-8")) for b in content)
    return (chars + 3) // 4


def canary_audit(root, canary):
    checked = []
    for path in root.rglob("*"):
        if not path.is_file() or path == root / "agent/auth.json":
            continue
        require(path.stat().st_size <= s.MAX_WIRE, "evidence file bound exceeded")
        data = path.read_text(encoding="utf-8", errors="replace")
        require(canary not in data, "unused fake auth canary leaked: " + str(path))
        # Decode JSON escapes too (including JSONL and nested JSON string values).
        for line in data.splitlines() if path.suffix == ".jsonl" else [data]:
            try:
                decoded = json.loads(line)
            except ValueError:
                continue
            require(canary not in json.dumps(decoded, ensure_ascii=False), "escaped canary leaked")
        checked.append(str(path.relative_to(root)))
    h.dump(root / "secret-audit.json", dict(status="passed", checked=checked,
        exclusions=["agent/auth.json: explicit unused fake credential ingress"],
        limitation="No-leak alone does not prove authentication; environment allowlist and isolation are separate gates"))


def run(args):
    require(__debug__ and os.name == "nt", "Windows without python -O required for inherited safety checks")
    root = s.target_path(args.diagnostics_dir)
    require(not root.exists(), "NEW diagnostics directory required; preserve failure evidence")
    root.mkdir(parents=True)
    state = dict(status="running", passed=[], limits=[
        "Only execution results qualify recorded Windows binaries, not an installed extension/Unix/deployment",
        "Native estimate is not exact tokenization; get_state may scan full active branch",
        "Probe loads native session and may write startup metadata; hashes retained, no transcript rewrite",
        "No live usage polling or cost/currency claim; optional model-window switch not covered"])
    peer = probe = model = jobs = None
    canary = "UNUSED_CONTEXT_AUTH_CANARY_" + os.urandom(16).hex()
    def persist():
        (root / "results.json").write_text(json.dumps(state, indent=2).replace(canary, "[REDACTED]"), encoding="utf-8")
    def landed(text):
        state["passed"].append(text)
        persist()
    def close_peer():
        nonlocal peer
        if peer is not None:
            old, peer = peer, None
            try:
                old.close()
            finally:
                jobs.dead()
    def close_probe():
        nonlocal probe
        if probe is not None:
            old, probe = probe, None
            try:
                old.close()
            finally:
                jobs.dead()
    try:
        native_repo, bridge_sources, native_sources = s.provenance(args, root)
        require(native_repo == NATIVE_SOURCE.resolve() and s.head(native_repo) == NATIVE_HEAD,
                "native source must be the explicit qualified stage3 26842b37 checkout")
        win = h.Windows()
        jobs = s.Jobs(win, root)
        model = Model(root)
        env = h.environment(root, model, args.rpi)  # positive OS allowlist; isolated HOME/auth/agent/tmp
        h.dump(root / "environment-audit.json", dict(inheritedAllowlist=sorted({
            "SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "SYSTEMDRIVE",
            "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}), keys=sorted(env),
            isolatedHome=env["HOME"], isolatedAgent=env["RPI_CODING_AGENT_DIR"],
            ambientCredentialsRead=False, provider="loopback with explicit dummy key"))
        h.dump(root / "agent/auth.json", {"never-used-context-fixture": dict(type="api_key", key=canary)})
        h.dump(root / "agent/settings.json", dict(defaultProvider="fixture", defaultModel="synthetic",
            defaultThinkingLevel="off", compaction=dict(enabled=False, reserveTokens=16384, keepRecentTokens=1)))
        workspace = root / "workspace"
        workspace.mkdir()
        (workspace / "usage-proof.txt").write_text(FILE_TEXT, encoding="utf-8")
        def frozen():
            require(s.inventory(s.REPO, s.PYTHON_INPUTS) == bridge_sources and
                    s.inventory(native_repo) == native_sources and s.head(native_repo) == NATIVE_HEAD and
                    s.sha(args.bridge) == args.bridge_sha and s.sha(args.rpi) == args.native_sha,
                    "preexecute source/binary drift")
        def start(name):
            nonlocal peer
            frozen()
            peer = Transport([args.bridge], env, root, name, win, root)
            peer.call("initialize", protocolVersion=1, clientCapabilities={})
            jobs.cold(peer, args.bridge)
        def attach(method, sid=None):
            params = dict(cwd=str(workspace), mcpServers=[])
            if sid:
                params["sessionId"] = sid
            value, before = peer.call(method, **params)
            sid = value.get("sessionId", sid)
            require(isinstance(sid, str) and sid, "session identity missing")
            initial_usage = settled_usage(peer, sid, before, initial=True)
            jobs.capture(peer)
            return sid, initial_usage
        def prompt(sid, text):
            value, frames = peer.call("session/prompt", sessionId=sid,
                prompt=[dict(type="text", text=text)])
            require(value.get("stopReason") == "end_turn", "prompt did not settle successfully")
            return settled_usage(peer, sid, frames), frames
        def observe(sid, label):
            """Bridge fully closed first: no concurrent native writers/probe."""
            nonlocal probe
            path = transcript(root, sid)
            before = disk_state(path)
            calls = len(model.calls)
            frozen()
            probe = Transport([args.rpi, "--mode", "rpc", "--session", path, "--local-only"],
                              env, root, label + "-native", win, workspace)
            try:
                ready = probe.receive(time.monotonic() + 20)
                require(ready.get("type") == "ready" and "get_state" in ready.get("supportedCommands", []),
                        "actual native get_state capability required")
                jobs.capture(probe)
                request = dict(id="context-read-only", type="get_state")
                probe.record("in", request)
                probe.process.stdin.write(json.dumps(request) + "\n")
                probe.process.stdin.flush()
                reply = probe.receive(time.monotonic() + 20)
                require(reply.get("type") == "response" and reply.get("id") == request["id"] and
                        reply.get("success") is True and reply.get("command") == "get_state",
                        "actual native get_state failed")
                context = native_context(reply["data"])
                h.dump(root / (label + "-get-state.json"), reply)
            finally:
                close_probe()  # EOF only, no prompt/setter/compaction RPC
                h.dump(root / (label + "-probe-disk.json"), dict(path=str(path), before=before,
                    after=disk_state(path), readOnlyCommands=["get_state"], shutdown="stdin EOF",
                    note="native startup/load may mutate metadata; harness never rewrites transcript"))
            require(len(model.calls) == calls, "read-only native probe contacted provider")
            return context
        def checkpoint(sid, last, label, method):
            peer.call("session/close", sessionId=sid)
            close_peer()
            observed = observe(sid, label)
            require(projected(observed) == last, "settled ACP differs from independent native context")
            start(label + "-restore")
            restored_sid, initial = attach(method, sid)
            require(restored_sid == sid and initial == projected(observed), "restore initial usage differs from native")
            jobs.capture(peer)
            landed(label + ": settled ACP == independent raw native get_state == postresponse restore initial")
            return observed
        start("context-create")
        sid, initial = attach("session/new")
        require(initial == dict(used=0, size=WINDOW) and not model.calls, "new initial context not empty/configured")
        jobs.capture(peer)
        landed("new response precedes initial usage (0/configured window), no model call")
        model.plan("text", 101, 13, "FIRST_CONTEXT_RESPONSE")
        first, _ = prompt(sid, "CONTEXT_FIRST_DECISION " + "first decision. " * 160)
        require(first == dict(used=114, size=WINDOW), "first provider usage not native current context")
        checkpoint(sid, first, "first", "session/load")
        model.plan("text", 271, 29, "SECOND_CONTEXT_RESPONSE")
        second, _ = prompt(sid, "CONTEXT_SECOND_DECISION " + "second decision. " * 160)
        require(second == dict(used=300, size=WINDOW) and second["used"] != 114 + 300,
                "current context is not last provider input/output/cache total")
        checkpoint(sid, second, "second", "session/resume")
        model.plan("read", 431, 37)
        model.plan("after-read", 613, 41, "NATIVE_READ_CONTINUATION_OK")
        tool, frames = prompt(sid, "Read usage-proof.txt using native read, then report its unique content.")
        require(tool == dict(used=654, size=WINDOW) and
                any(f.get("params", {}).get("update", {}).get("sessionUpdate") == "tool_call" for f in frames),
                "native tool continuation/final noncumulative usage not proved")
        checkpoint(sid, tool, "tool", "session/load")
        # A real zero-usage final message ensures retained tail has no valid usage;
        # compaction must fall back to summary+tail estimate, not summary API usage.
        model.plan("text", 0, 0, "TAIL_WITHOUT_PROVIDER_USAGE")
        tail, _ = prompt(sid, "FINAL_TAIL_NO_USAGE " + "retained decision. " * 160)
        require(tail["used"] > 654, "native trailing byte estimate not added after last valid usage")
        tail_context = checkpoint(sid, tail, "trailing", "session/resume")
        require(not tail_context["estimated"] and tail_context["trailingTokens"] == tail["used"] - 654,
                "native last usage plus trailing estimate mismatch")
        # Native split-turn compaction may request history summary + turn-prefix
        # summary. Both get deterministic local text, each with deliberately large
        # provider usage that must NOT become the compacted context estimate.
        model.plan("summary", 8001, 101, SUMMARY)
        model.plan("summary", 9001, 103, SUMMARY)
        compacted, frames = prompt(sid, "/compact Keep only concise local decisions.")
        # One or two calls are allowed; do not leave an unused plan at cleanup.
        while not model.plans.empty():
            unused = model.plans.get_nowait()
            require(unused["kind"] == "summary", "unexpected unconsumed provider plan")
        peer.call("session/close", sessionId=sid)
        close_peer()
        path = transcript(root, sid)
        entries = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
        compactions = [e for e in entries if e.get("type") == "compaction"]
        require(compactions, "true native compaction entry absent (decline is not proof)")
        entry = compactions[-1]
        require(SUMMARY in entry["summary"], "local summary not persisted")
        retained = entry.get("retainedTail", [])
        require(retained and all(sum(m.get("usage", {}).get(k, 0) for k in
                ("input", "output", "cacheRead", "cacheWrite", "totalTokens")) == 0 for m in retained),
                "compaction retained a valid usage; estimate-only case not reached")
        expected = (len(entry["summary"].encode("utf-8")) + 3) // 4 + sum(estimate_plain(m) for m in retained)
        context = observe(sid, "compact")
        require(context["estimated"] and context["tokens"] == context["trailingTokens"] == expected and
                compacted == projected(context) and compacted["used"] < tail["used"],
                "compaction-aware summary/tail estimate disagrees with actual native state")
        start("compact-load")
        _, restored = attach("session/load", sid)
        require(restored == compacted, "compacted load initial mismatch")
        peer.call("session/close", sessionId=sid)
        close_peer()
        start("compact-resume")
        _, restored = attach("session/resume", sid)
        require(restored == compacted, "compacted resume initial mismatch")
        peer.call("session/close", sessionId=sid)
        close_peer()
        landed("true /compact: native persisted local summary, estimate-only retained tail, raw native equality after load AND resume")
        start("other-session")
        other, empty = attach("session/new")
        require(other != sid and empty == dict(used=0, size=WINDOW), "prior session context leaked into new session")
        peer.call("session/close", sessionId=other)
        close_peer()
        landed("distinct fresh native session remains zero; no cross-session usage leak")
        state["status"] = "passed"
    except BaseException:
        state.update(status="failed", failure=traceback.format_exc())
        raise
    finally:
        errors = []
        for label, action in (("probe/Job", close_probe), ("ACP/Job", close_peer),
                              ("PID handles", lambda: jobs.dead() if jobs else None),
                              ("provider", lambda: model.close() if model else None)):
            try:
                action()
            except BaseException:
                errors.append(dict(resource=label, error=traceback.format_exc()))
        state["cleanupErrors"] = errors
        if errors:
            state["status"] = "failed"
        persist()
        try:
            canary_audit(root, canary)
        except BaseException:
            state["status"] = "failed"
            errors.append(dict(resource="canary audit", error="final evidence canary/bound audit failed"))
            persist()
            h.dump(root / "secret-audit.json", dict(status="failed", leaked=None))
        if errors and sys.exc_info()[0] is None:
            raise AssertionError("cleanup/audit failed; see retained results.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--write-source-manifest", type=Path,
        help="SAFE prebuild inventory ONLY, no bridge/native launch")
    for flag in ("bridge", "rpi", "diagnostics-dir", "build-marker", "source-manifest",
                 "native-build-record", "native-verification"):
        parser.add_argument("--" + flag, type=Path)
    parser.add_argument("--bridge-sha")
    parser.add_argument("--native-sha")
    args = parser.parse_args()
    if args.write_source_manifest:
        s.write_manifest(args.write_source_manifest)
        return
    for key, value in vars(args).items():
        if key == "write_source_manifest":
            continue
        if value is None:
            parser.error("execution requires --" + key.replace("_", "-"))
        if isinstance(value, Path):
            require(value.is_absolute(), "explicit absolute path required: " + key)
    run(args)


if __name__ == "__main__":
    main()
