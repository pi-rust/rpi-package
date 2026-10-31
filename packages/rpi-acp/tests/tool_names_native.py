"""Stage10 Windows stable ACP v1 tool names; AUTHORING IS NOT A NATIVE PASS.

SAFE before building: --write-source-manifest <NEW absolute target JSON>.
Uses stage9's complete prebuild manifest/build-marker/native-verification schema
through session_list_native.provenance. The parent must build/freeze the bridge
AFTER that manifest. Native is the already-qualified, unchanged stage3 26842b37
checkout/binary: this harness never builds, edits native, discovers binaries,
loads installed DLLs, or reads actual credentials. All execution arguments are
explicit. Output is NEW evidence beneath acp-stage10-01 only.

Three groups: independent raw RPC native read; ACP actual successful/failed read
plus native JSONL; fresh-process load replay and resume without tool replay.
Native read need not emit partial updates: exact observed phases are retained.
Partial-progress mapping requires separate protocol fixture tests, not fabricated
native coverage. No model-window/cancellation/cost/message-ID scope.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import socket
import sys
import threading
import time
import traceback
import uuid
from http.server import BaseHTTPRequestHandler, HTTPServer

sys.dont_write_bytecode = True
import context_usage_native as c
import session_list_native as s
import mcp_stdio_native as h

# mcp_http_fixture.py is a real transitive import of s, not an omitted build
# input. No MCP server is launched for this builtin-tool-only proof.
s.TARGET = Path("C:/Users/bigfish/.cargo/target/acp-stage10-01")
s.PYTHON_INPUTS = set(s.PYTHON_INPUTS) | {
    "packages/rpi-acp/tests/" + name for name in (
        "tool_names_native.py", "context_usage_native.py", "session_list_native.py",
        "mcp_stdio_native.py", "mcp_http_fixture.py")}
Transport = c.Transport
require = s.require
LIMIT = c.LIMIT
TOOL_PHASES = ("tool_execution_start", "tool_execution_update", "tool_execution_end")


class Model:
    """Finite, single-worker loopback provider with explicit per-request plans."""
    def __init__(self, root):
        self.calls, self.errors = [], []
        self.plans = queue.Queue(maxsize=6)
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(8)

            def log_message(self, *_):
                pass

            def do_GET(self):
                owner.errors.append("unexpected provider GET")
                self.send_error(404)

            def do_POST(self):
                try:
                    require(self.path == "/v1/chat/completions", "unexpected provider path")
                    size = int(self.headers.get("Content-Length", "0"))
                    require(0 < size <= 1024 * 1024 and len(owner.calls) < 6,
                            "provider request/count bound exceeded")
                    raw = self.rfile.read(size)
                    require(len(raw) == size, "partial provider body")
                    body, plan = json.loads(raw), owner.plans.get_nowait()
                    require(body.get("model") == "synthetic", "wrong isolated model")
                    owner.calls.append(dict(request=body, responsePlan=plan))
                    h.dump(root / "provider-requests.json", owner.calls)
                    if plan["kind"] == "read":
                        require(any(t.get("function", {}).get("name") == "read"
                                    for t in body.get("tools", [])), "native builtin read not offered")
                        delta = dict(role="assistant", tool_calls=[dict(index=0,
                            id=plan["id"], type="function", function=dict(name="read",
                                arguments=json.dumps(dict(path=plan["path"]))))])
                        reason = "tool_calls"
                    else:
                        results = [m for m in body["messages"] if m.get("role") == "tool"
                                   and m.get("tool_call_id") == plan["id"]]
                        require(len(results) == 1, "native tool result correlation missing at provider")
                        text = h.as_text(results[0].get("content"))
                        if plan["failed"]:
                            require(plan["path"] in text and text.strip(), "missing-file error absent")
                        else:
                            require(plan["text"].strip() in text, "unique actual native file content absent")
                        delta, reason = dict(role="assistant", content="LOCAL_READ_DONE"), "stop"
                    events = [dict(choices=[dict(index=0, delta=delta, finish_reason=None)]),
                              dict(choices=[dict(index=0, delta={}, finish_reason=reason)]),
                              dict(choices=[], usage=dict(prompt_tokens=50,
                                   completion_tokens=10, total_tokens=60))]
                    data = ("".join("data: " + json.dumps(e) + "\n\n" for e in events)
                            + "data: [DONE]\n\n").encode()
                    require(len(data) < 8192, "provider reply bound")
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(data)
                    self.wfile.flush()
                    self.close_connection = True
                except BaseException:
                    owner.errors.append(traceback.format_exc())
                    h.dump(root / "provider-errors.json", owner.errors)
                    self.close_connection = True

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.05),
                                       daemon=True)
        self.thread.start()

    def plan_read(self, call_id, path, text, failed=False):
        plan = dict(id=call_id, path=path, text=text, failed=failed)
        self.plans.put_nowait(dict(kind="read", **plan))
        self.plans.put_nowait(dict(kind="final", **plan))

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=12)
        require(not self.thread.is_alive() and not self.errors and self.plans.empty(),
                "provider cleanup/errors/unconsumed plans; inspect evidence")
        with socket.socket() as sock:
            sock.settimeout(1)
            require(sock.connect_ex(("127.0.0.1", self.server.server_port)) != 0,
                    "provider listener survived shutdown")


def wire(root, label):
    path = root / (label + "-wire.jsonl")
    require(path.is_file() and path.stat().st_size <= s.MAX_WIRE * 2, "wire evidence bound")
    return [v["message"] for line in path.read_text(encoding="utf-8").splitlines()
            if (v := json.loads(line)).get("direction") == "out"]


def acp_tools(frames, sid):
    tools = []
    for frame in frames:
        if frame.get("method") != "session/update":
            continue
        params = frame["params"]
        require(params.get("sessionId") == sid, "ACP notification crossed session identity")
        update = params["update"]
        if update.get("sessionUpdate") in ("tool_call", "tool_call_update"):
            tools.append(update)
    return tools


def check_acp(frames, sid, call_id, name, failed=False):
    tools = acp_tools(frames, sid)
    require(tools and all(t.get("toolCallId") == call_id and t.get("name") == name == "read"
                          for t in tools), "ACP tool identity/name differs from supplied raw native name")
    starts = [t for t in tools if t["sessionUpdate"] == "tool_call"]
    ends = [t for t in tools if t.get("status") in ("completed", "failed")]
    progress = [t for t in tools if t["sessionUpdate"] == "tool_call_update"
                and t.get("status") == "in_progress"]
    require(len(starts) == len(ends) == 1 and starts[0].get("status") == "in_progress"
            and ends[0].get("status") == ("failed" if failed else "completed")
            and tools[0] == starts[0] and tools[-1] == ends[0], "ACP start/end lifecycle mismatch")
    return dict(start=1, partialUpdates=len(progress), end=1, name=name, toolCallId=call_id)


def check_jsonl(path, sid, workspace, expected):
    require(path.stat().st_size <= LIMIT, "native transcript bound")
    entries = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
    header = entries[0]
    require(header.get("kind") == "header" and header.get("version") == 4
            and header.get("id") == sid and Path(header["cwd"]).resolve() == workspace.resolve(),
            "native v4 identity/workspace mismatch")
    assistants = [o for o in h.objects(entries) if o.get("role") == "assistant"]
    calls = [b for a in assistants for b in a.get("content", [])
             if isinstance(b, dict) and b.get("type") == "toolCall"]
    results = [o for o in h.objects(entries) if o.get("role") == "toolResult"]
    require(len(calls) == len(results) == len(expected), "native JSONL contains missing/extra tool execution")
    for call_id, name, failed, unique in expected:
        matched_calls = [b for b in calls if b.get("id") == call_id]
        matched_results = [r for r in results if r.get("toolCallId") == call_id]
        require(len(matched_calls) == len(matched_results) == 1, "native JSONL tool correlation missing/duplicate")
        call, result = matched_calls[0], matched_results[0]
        require(call.get("name") == result.get("toolName") == name == "read"
                and result.get("isError") is failed, "native JSONL names/outcome differ from raw toolName")
        require(unique.strip() in h.as_text(result.get("content")), "native JSONL actual result absent")
    return dict(path=str(path), sha256=s.sha(path), bytes=path.stat().st_size,
                expectedToolCalls=len(expected))


def run(args):
    require(__debug__ and os.name == "nt", "Windows without python -O required")
    root = s.target_path(args.diagnostics_dir)
    require(not root.exists(), "NEW diagnostics directory required; retain prior failures")
    root.mkdir(parents=True)
    state = dict(status="running", passed=[], observed={}, cleanupErrors=[], limits=[
        "Authored harness is not execution evidence or qualification of installed DLL/Unix/deployment",
        "Builtin read partial progress is optional; only observed phases count",
        "Separate protocol fixtures must exercise partial-progress mapping",
        "Parent-attested build provenance is not cryptographic attestation"])
    peer = raw = model = jobs = None
    canary = "UNUSED_TOOL_NAMES_AUTH_" + uuid.uuid4().hex

    def persist():
        (root / "results.json").write_text(json.dumps(state, indent=2).replace(canary, "[REDACTED]"),
                                            encoding="utf-8")

    def landed(text):
        state["passed"].append(text)
        persist()

    def dispose(slot):
        nonlocal peer, raw
        old = peer if slot == "ACP" else raw
        if slot == "ACP":
            peer = None
        else:
            raw = None
        if old is None:
            return
        # Inherited close may fail even after exit 0. Record, do not normalize
        # that failure away; preserve the scenario's original exception too.
        errors = []
        for resource, action in ((slot + "/Transport", old.close), (slot + "/PID", jobs.dead)):
            try:
                action()
            except BaseException:
                errors.append(dict(resource=resource, error=traceback.format_exc()))
        state["cleanupErrors"].extend(errors)
        persist()
        if errors:
            raise AssertionError("owned transport/PID cleanup failed; retained original diagnostics")

    persist()
    try:
        native_repo, bridge_sources, native_sources = s.provenance(args, root)
        require(native_repo == c.NATIVE_SOURCE.resolve() and s.head(native_repo) == c.NATIVE_HEAD,
                "require unchanged qualified native stage3 26842b37 checkout")
        win = h.Windows()
        jobs = s.Jobs(win, root)
        model = Model(root)
        env = h.environment(root, model, args.rpi)
        h.dump(root / "agent/auth.json", {"never-used-tool-name-fixture": dict(type="api_key", key=canary)})
        h.dump(root / "agent/settings.json", dict(defaultProvider="fixture", defaultModel="synthetic",
            defaultThinkingLevel="off", compaction=dict(enabled=False)))
        h.dump(root / "environment-audit.json", dict(keys=sorted(env),
            inheritedAllowlist=sorted({"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT",
                "SYSTEMDRIVE", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}),
            isolatedHome=env["HOME"], isolatedAgent=env["RPI_CODING_AGENT_DIR"],
            actualCredentialsRead=False, modelReasoning=False, dummyKey=True,
            provider="127.0.0.1 only", installedExtensionsLoaded=False))

        def frozen():
            require(s.inventory(s.REPO, s.PYTHON_INPUTS) == bridge_sources
                    and s.inventory(native_repo) == native_sources
                    and s.head(s.REPO) == json.loads(args.source_manifest.read_text(encoding="utf-8"))["head"]
                    and s.head(native_repo) == c.NATIVE_HEAD
                    and s.sha(args.bridge) == args.bridge_sha and s.sha(args.rpi) == args.native_sha,
                    "prelaunch full source/HEAD/binary drift")

        raw_workspace, workspace = root / "raw-workspace", root / "acp-workspace"
        raw_workspace.mkdir()
        workspace.mkdir()
        filename = "native-read-" + uuid.uuid4().hex + ".txt"
        raw_text = "RAW_NATIVE_TOOL_NAMES_" + uuid.uuid4().hex + "\n"
        acp_text = "ACP_NATIVE_TOOL_NAMES_" + uuid.uuid4().hex + "\n"
        (raw_workspace / filename).write_text(raw_text, encoding="utf-8")
        (workspace / filename).write_text(acp_text, encoding="utf-8")
        raw_sid, raw_id = str(uuid.uuid4()), "raw-read-" + uuid.uuid4().hex
        raw_root = root / "raw-native-sessions"
        raw_root.mkdir()
        h.dump(root / "scenario-identities.json", dict(rawSessionId=raw_sid,
            rawSessionRoot=str(raw_root), rawWorkspace=str(raw_workspace), acpWorkspace=str(workspace)))
        model.plan_read(raw_id, filename, raw_text)
        frozen()
        raw = Transport([args.rpi, "--mode", "rpc", "--session-id", raw_sid,
            "--session-dir", raw_root, "--local-only", "--no-extensions", "--no-skills",
            "--no-context-files", "--no-prompt-templates", "--provider", "fixture",
            "--model", "synthetic"], env, root, "raw-native", win, raw_workspace)
        ready = raw.receive(time.monotonic() + 20)
        require(ready.get("type") == "ready" and ready.get("protocol") == "rpi"
                and "prompt" in ready.get("supportedCommands", [])
                and ready.get("semantics", {}).get("promptResponse") == "accepted"
                and ready["semantics"].get("settlementEvent") == "agent_settled",
                "actual native ready command/settlement semantics missing")
        jobs.cold(raw, args.rpi)
        request = dict(id="raw-prompt-" + uuid.uuid4().hex, type="prompt", message="Read " + filename)
        raw.record("in", request)
        raw.process.stdin.write(json.dumps(request) + "\n")
        raw.process.stdin.flush()
        accepted, settled, events = False, False, []
        deadline = time.monotonic() + 45
        while not settled:
            frame = raw.receive(deadline)
            if frame.get("type") == "response":
                require(not accepted and frame.get("id") == request["id"]
                        and frame.get("command") == "prompt" and frame.get("success") is True
                        and frame.get("data", {}).get("accepted") is True, "raw prompt not actually accepted")
                accepted = True
            elif frame.get("type") in TOOL_PHASES:
                events.append(frame)
            elif frame.get("type") == "agent_settled":
                require(accepted and frame.get("outcome") == "completed", "raw native failed settlement")
                settled = True
        jobs.capture(raw)
        dispose("raw")  # Native EOF with exit 0 is normal; no synthetic stop RPC.
        all_raw = wire(root, "raw-native")
        events = [f for f in all_raw if f.get("type") in TOOL_PHASES]
        starts = [e for e in events if e["type"] == TOOL_PHASES[0]]
        ends = [e for e in events if e["type"] == TOOL_PHASES[2]]
        require(len(starts) == len(ends) == 1, "raw native start/end missing or duplicated")
        supplied_name = starts[0].get("toolName")
        require(supplied_name == "read" and all(e.get("toolCallId") == raw_id
                and e.get("toolName") == supplied_name for e in events)
                and ends[0].get("isError") is False, "raw native phase names/IDs/outcome mismatch")
        paths = list(raw_root.rglob("*.jsonl"))
        require(len(paths) == 1, "raw explicit session root missing/extra transcript")
        state["rawJsonl"] = check_jsonl(paths[0], raw_sid, raw_workspace,
                                       [(raw_id, supplied_name, False, raw_text)])
        state["observed"]["rawNative"] = {phase: sum(e["type"] == phase for e in events)
                                             for phase in TOOL_PHASES}
        state["observed"]["rawNative"]["partialProgressCovered"] = any(
            e["type"] == TOOL_PHASES[1] for e in events)
        require(len(model.calls) == 2 and model.plans.empty() and not model.errors, "raw provider plan mismatch")
        landed("raw RPC: actual accepted/settled native read, phase toolName correlation and native JSONL")

        def start(label):
            nonlocal peer
            frozen()
            peer = Transport([args.bridge], env, root, label, win, root)
            peer.call("initialize", protocolVersion=1, clientCapabilities={})
            jobs.cold(peer, args.bridge)

        start("acp-live")
        created, before = peer.call("session/new", cwd=str(workspace), mcpServers=[])
        sid = created["sessionId"]
        require(str(uuid.UUID(sid)) == sid and sid != raw_sid and not acp_tools(before, sid),
                "new explicit native identity invalid or unexpectedly replayed tools")
        state["acpSessionId"] = sid
        jobs.capture(peer)
        expected = []
        for label, failed in (("success", False), ("missing", True)):
            call_id = "acp-" + label + "-" + uuid.uuid4().hex
            path = "missing-" + uuid.uuid4().hex + ".txt" if failed else filename
            require(not failed or not (workspace / path).exists(), "missing file fixture exists")
            model.plan_read(call_id, path, acp_text, failed)
            value, frames = peer.call("session/prompt", sessionId=sid,
                                     prompt=[dict(type="text", text="Read " + path)])
            require(value.get("stopReason") == "end_turn", "ACP prompt did not settle")
            state["observed"]["acp-" + label] = check_acp(frames, sid, call_id, supplied_name, failed)
            expected.append((call_id, supplied_name, failed, path if failed else acp_text))
            persist()
        peer.call("session/close", sessionId=sid)
        dispose("ACP")
        state["acpJsonl"] = check_jsonl(c.transcript(root, sid), sid, workspace, expected)
        require(len(model.calls) == 6 and model.plans.empty() and not model.errors, "ACP provider count mismatch")
        landed("ACP actual native success + missing-file failure: start/end name equals raw toolName; IDs and native JSONL agree")

        for method, label in (("session/load", "fresh-load"), ("session/resume", "fresh-resume")):
            calls = len(model.calls)
            start(label)
            restored, frames = peer.call(method, sessionId=sid, cwd=str(workspace), mcpServers=[])
            require(restored.get("sessionId", sid) == sid, "restore changed session ID")
            jobs.capture(peer)
            if method == "session/load":
                for call_id, name, failed, _ in expected:
                    selected = [f for f in frames if f.get("params", {}).get("update", {}).get("toolCallId") == call_id]
                    state["observed"][label + "-" + call_id] = check_acp(selected, sid, call_id, name, failed)
                require(len(acp_tools(frames, sid)) == 4, "load omitted/duplicated/added tool replay")
            else:
                require(not acp_tools(frames, sid), "resume replayed tool frames")
            peer.call("session/close", sessionId=sid)
            dispose("ACP")
            # Whole closed transport log also fences postresponse queued updates.
            full = acp_tools(wire(root, label), sid)
            require(len(full) == (4 if method == "session/load" else 0), "late/duplicate restore tool frames")
            require(len(model.calls) == calls and model.plans.empty(), "restore reexecuted provider/tool plan")
            check_jsonl(c.transcript(root, sid), sid, workspace, expected)
            persist()
        landed("fresh load retains both start/end names and IDs; fresh resume emits no tool frames; no provider replay")
        state["status"] = "passed"
    except BaseException:
        state.update(status="failed", failure=traceback.format_exc())
        raise
    finally:
        for label, action in (("raw", lambda: dispose("raw")), ("ACP", lambda: dispose("ACP")),
                              ("PID handles", lambda: jobs.dead() if jobs else None),
                              ("provider", lambda: model.close() if model else None)):
            try:
                action()
            except BaseException:
                state["cleanupErrors"].append(dict(resource=label, error=traceback.format_exc()))
        if state["cleanupErrors"]:
            state["status"] = "failed"
        persist()
        try:
            c.canary_audit(root, canary)  # All evidence, including cleanup/results; only fake auth ingress excluded.
        except BaseException:
            state["status"] = "failed"
            state["cleanupErrors"].append(dict(resource="secret audit", error="final evidence canary/bound audit failed"))
            persist()
            h.dump(root / "secret-audit.json", dict(status="failed", leaked=None))
        if state["cleanupErrors"] and sys.exc_info()[0] is None:
            raise AssertionError("cleanup/audit failed; retained results.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--write-source-manifest", type=Path,
                        help="SAFE new target-only prebuild inventory; no binary/provider execution")
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
