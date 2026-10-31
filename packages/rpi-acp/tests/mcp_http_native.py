"""Provenance-gated Windows real-native ACP -> loopback MCP HTTP integration.

Required: --bridge --rpi --diagnostics-dir --build-marker --native-build-record
--native-verification --native-sha. Never builds or searches PATH for a runtime.
Only a NEW directory under C:/Users/bigfish/.cargo/target/acp-stage6-01 is used.
--self-test DIRECTORY tests only the HTTP fixture, never executes a bridge/native.
See native-harness-report.md in the stage6 diagnostics root for the marker contract.
"""
import argparse
import ctypes
import hashlib
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time
import traceback

# Set before importing sibling harnesses: even an invocation without -B must not
# leave Python bytecode in the checkout. No fixture/leaf entrypoint is executed.
sys.dont_write_bytecode = True
import mcp_stdio_native as h
from mcp_http_fixture import Fixture, META, VERSION

TARGET = Path("C:/Users/bigfish/.cargo/target/acp-stage6-01")
NATIVE = Path("C:/Users/bigfish/.cargo/target/acp-stage3-01/build-native/debug/rpi.exe")
NATIVE_SHA = "d37624c676f0f2658e10001393e5fd3be1f8fea276295ab02de937fa789e7313"
NATIVE_HEAD = "26842b37b5d7efe07a67f5c883b4008b0c0ac794"


def new_root(path):
    root = path.resolve()
    assert root.is_relative_to(TARGET.resolve()) and root != TARGET.resolve(), "diagnostics must be under stage6-01"
    assert not root.exists(), "use a NEW diagnostics directory; preserve prior evidence"
    root.mkdir(parents=True)
    return root


def git_head(path):
    return subprocess.check_output(["git", "-C", str(path), "rev-parse", "HEAD"],
                                   text=True, stderr=subprocess.PIPE).strip()


EXECUTED_PYTHON = {"packages/rpi-acp/tests/" + name for name in
                   ("mcp_http_native.py", "mcp_http_fixture.py", "mcp_stdio_native.py")}
# Required implementation keys are present in the existing stage3 verification
# record. Do not demand a new whole-native-tree attestation from that old record.
NATIVE_SOURCE_KEYS = {"Cargo.lock", "crates/rpi-agent/src/agent_loop.rs",
    "crates/rpi-cli/src/modes.rs", "crates/rpi-tools/src/tools/external.rs",
    "crates/rpi-tools/src/tools/mod.rs"} | {
    "crates/rpi-cli/src/rpc/" + name + ".rs" for name in
    ("mod", "protocol", "framing", "writer", "client_requests", "external_tools")}


def bridge_source_inputs(repo):
    """All current Git-owned Rust/build/config + executed local Python inputs.

    Git enumeration includes untracked new sources, not ignored build artifacts;
    do not recursively glob unrelated files already lying in the checkout.
    """
    paths = subprocess.check_output(["git", "-C", str(repo), "ls-files",
        "--cached", "--others", "--exclude-standard", "-z"]).decode().split("\0")
    return {p for p in paths if p and (p.endswith(".rs") or
        Path(p).name in ("Cargo.toml", "Cargo.lock") or
        (".cargo" in Path(p).parts and Path(p).name in ("config", "config.toml")))} | EXECUTED_PYTHON


def verify_source_manifest(repo, manifest, required, label):
    repo = repo.resolve()
    assert isinstance(manifest, dict) and manifest, label + " sourceSha256 must be nonempty"
    assert required <= manifest.keys(), label + " source manifest omits required inputs"
    for relative, expected in manifest.items():
        assert isinstance(relative, str) and relative and "\\" not in relative and ":" not in relative, "manifest paths must be relative / paths"
        assert not Path(relative).is_absolute() and ".." not in Path(relative).parts, "manifest path escapes checkout"
        path = (repo / relative).resolve()
        assert path.is_relative_to(repo) and path != repo, "manifest path escapes checkout"
        assert isinstance(expected, str) and len(expected) == 64 and all(c in "0123456789abcdef" for c in expected), "invalid manifest SHA256"
        assert hashlib.sha256(path.read_bytes()).hexdigest() == expected, label + " source mismatch: " + relative


def verify_bridge_sources(repo, manifest):
    """HEAD is necessary, never sufficient: attest dirty proof/build inputs too."""
    verify_source_manifest(repo, manifest, bridge_source_inputs(repo), "bridge")


def verify_native_sources(repo, manifest):
    verify_source_manifest(repo, manifest, NATIVE_SOURCE_KEYS, "native")


def provenance(args, root):
    """Fail before starting even a fixture if the parent build is not ready."""
    assert args.build_marker.is_file(), "parent build marker missing; no binaries executed"
    marker = json.loads(args.build_marker.read_text(encoding="utf-8"))
    build = json.loads(args.native_build_record.read_text(encoding="utf-8"))
    verification = json.loads(args.native_verification.read_text(encoding="utf-8"))
    assert args.rpi.resolve() == NATIVE.resolve(), "unexpected native binary path"
    assert args.native_sha == NATIVE_SHA, "unexpected requested native SHA"
    actual = hashlib.sha256(args.rpi.read_bytes()).hexdigest()
    bridge_sha = hashlib.sha256(args.bridge.read_bytes()).hexdigest()
    repo = Path(__file__).resolve().parents[3]
    head = git_head(repo)
    native_head = git_head(Path(build["cwd"]))
    h.dump(root / "provenance.json", dict(native=str(args.rpi), sha256=actual,
        expected=args.native_sha, native_build=build, source_verification=verification,
        native_head=native_head, bridge=str(args.bridge), bridge_sha256=bridge_sha,
        bridge_build_marker=marker, checkout_head=head))
    assert actual == args.native_sha, "native hash mismatch"
    assert build["exitCode"] == 0, "native build failed"
    assert verification["nativeBinary"]["sha256"] == actual, "verification hash mismatch"
    assert Path(verification["nativeBinary"]["path"]).resolve() == args.rpi.resolve()
    assert verification["head"] == native_head == NATIVE_HEAD, "native source HEAD mismatch"
    verify_native_sources(Path(build["cwd"]), verification.get("sourceSha256"))
    assert marker["build"]["exitCode"] == 0, "bridge build failed"
    assert Path(marker["bridge"]).resolve() == args.bridge.resolve(), "bridge marker path mismatch"
    assert marker["sha256"] == bridge_sha, "bridge marker SHA mismatch"
    assert marker["head"] == head, "bridge marker/checkout HEAD mismatch"
    # Mandatory even for a clean checkout: parent creates this manifest BEFORE
    # compilation and attaches it to the successful binary build marker.
    verify_bridge_sources(repo, marker.get("sourceSha256"))


class HttpPeer(h.Peer):
    """Retain ALL stdout, including malformed/non-JSON output, for secret audit."""
    def pump(self):
        try:
            with (self.root / (self.name + ".stdout.log")).open("w", encoding="utf-8") as raw:
                for line in self.process.stdout:
                    raw.write(line)
                    raw.flush()
                    value = json.loads(line)
                    self.record("out", value)
                    self.output.put(value)
        except BaseException as error:
            self.output.put(error)
        finally:
            self.output.put(EOFError("bridge stdout closed"))


class Processes:
    """Positively-live handles from OUR Job only; no global PID/name search."""
    def __init__(self, root, win):
        self.root, self.win, self.handles = root, win, {}
        query = win.k.QueryInformationJobObject
        query.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p,
                          ctypes.c_ulong, ctypes.c_void_p]
        query.restype = ctypes.c_int

    def capture(self, peer):
        # JOBOBJECT_BASIC_PROCESS_ID_LIST: two DWORDs then ULONG_PTR array.
        buffer = ctypes.create_string_buffer(8 + 64 * ctypes.sizeof(ctypes.c_size_t))
        self.win.check(self.win.k.QueryInformationJobObject(peer.job, 3, buffer, len(buffer), None))
        assigned = ctypes.c_ulong.from_buffer(buffer, 0).value
        count = ctypes.c_ulong.from_buffer(buffer, 4).value
        assert 2 <= count == assigned <= 64, "expected bridge and native child in owned Job"
        pids = (ctypes.c_size_t * count).from_buffer(buffer, 8)
        for pid in pids:
            if pid not in self.handles:
                self.handles[pid] = self.win.open_live(pid)
                h.append(self.root / "pid-audit.jsonl", dict(peer=peer.name, pid=pid, live=True))

    def close(self):
        try:
            for pid, handle in self.handles.items():
                h.wait_until(lambda: not self.win.alive(handle), "owned child alive after Job cleanup", 15)
                h.append(self.root / "pid-audit.jsonl", dict(pid=pid, dead=True))
        finally:
            for handle in self.handles.values():
                self.win.k.CloseHandle(handle)


def secret_audit(root, fixture, require_provider=True):
    """Audit local harness evidence, not arbitrary remote-output DLP.

    The fixture never echoes configured credentials into tool descriptions/results.
    Real remote-controlled tool content can deliberately echo them; forwarding that
    content is not a bridge non-disclosure/DLP guarantee.
    """
    secrets = fixture.secret_canaries()
    capture = root / "provider-requests.json"
    if require_provider:
        assert capture.is_file(), "successful native run requires provider request capture"
        calls = json.loads(capture.read_text(encoding="utf-8"))
        assert isinstance(calls, list) and calls and all(isinstance(c, dict) for c in calls), "provider request capture must be nonempty"
    checked, exclusions = [], []

    def check(data, location):
        assert all(s not in data for s in secrets), "secret in local evidence: " + location

    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(root).as_posix()
        if relative == "http-fixture-wire.jsonl":
            exclusions.append(relative + " (fixture-owned fake HTTP Authorization/query wire)")
            continue
        if path.parent == root and path.name.endswith("-wire.jsonl"):
            for line in path.read_text(encoding="utf-8").splitlines():
                item = json.loads(line)
                assert item.get("direction") in ("in", "out"), "unknown ACP wire direction"
                if item["direction"] == "out":
                    check(json.dumps(item, ensure_ascii=False), relative)
            checked.append(relative + " (ACP outbound only)")
            exclusions.append(relative + " (ACP inbound intentional client configuration)")
            continue
        # Full recursive inventory includes provider requests (including ALL tool
        # definitions), root provider error JSON, tmp/spool, isolated persistence,
        # and raw bridge stdout/stderr. Decode JSON too so escapes cannot hide a
        # canary. Native RPC stdout is not separately captured by this harness;
        # bridge stdout is ACP outbound, NOT intentional inbound configuration.
        data = path.read_bytes().decode("utf-8", errors="replace")
        check(data, relative)
        if path.suffix == ".json":
            check(json.dumps(json.loads(data), ensure_ascii=False), relative)
        checked.append(relative)
    h.dump(root / "secret-audit.json", dict(checked=checked, leaked=False,
        providerCaptureRequired=require_provider, exclusions=exclusions,
        scope="all files under local diagnostics root, including redirected TMP/TEMP/TMPDIR; not OS-global temp, ambient environment or remote-output DLP"))


def native_metadata(root, sid):
    entries = h.transcript(root, sid)
    results = [o["externalToolResult"] for o in h.objects(entries) if "externalToolResult" in o]
    good = [r for r in results if r.get("structuredContent") == h.STRUCTURED]
    assert any(r.get("_meta") == META and any(c.get("_meta") == META and
        c.get("annotations") == {"audience": ["assistant"], "priority": .5}
        for c in r.get("content", [])) for r in good), "raw metadata/annotations lost in native JSONL"
    calls = [o for o in h.objects(entries) if o.get("type") == "toolCall"]
    assert calls and all(c.get("arguments") == dict(mode=c["arguments"]["mode"], query=h.QUERY)
                         for c in calls if c.get("name", "").startswith("mcp_")), "native args changed"
    assert "LATE_HTTP_RESULT" not in json.dumps(entries), "late cancellation result persisted"


def roundtrip(peer, model, sid, label, mode):
    """Reuse native/provider checks, then assert the actual ACP raw wire too."""
    path = peer.root / (peer.name + "-wire.jsonl")
    offset = path.stat().st_size
    h.roundtrip(peer, model, sid, label, mode)
    with path.open("rb") as wire:
        wire.seek(offset)
        messages = [json.loads(line)["message"] for line in wire
                    if json.loads(line).get("direction") == "out"]
    updates = list(h.objects(messages))
    starts = [o for o in updates if o.get("sessionUpdate") == "tool_call"]
    assert len(starts) == 1 and starts[0].get("rawInput") == dict(mode=mode, query=h.QUERY), "ACP rawInput changed"
    ends = [o for o in updates if o.get("sessionUpdate") == "tool_call_update" and
            o.get("status") in ("completed", "failed")]
    assert len(ends) == 1 and ends[0]["toolCallId"] == starts[0]["toolCallId"], "ACP completion correlation"
    if mode in ("success", "image"):
        raw = [o["externalToolResult"] for o in h.objects(ends[0].get("rawOutput"))
               if "externalToolResult" in o]
        assert len(raw) == 1, "ACP rawOutput lost native raw result"
        assert raw[0].get("structuredContent") == h.STRUCTURED and raw[0].get("_meta") == META
        assert raw[0]["content"][0]["text"] == h.TEXT + ":" + label
        assert raw[0]["content"][0]["_meta"] == META
        assert h.TEXT + ":" + label in json.dumps(ends[0].get("content")), "ACP rendered text missing"
        if mode == "image":
            assert raw[0]["content"][1]["data"] == h.PNG, "ACP raw image changed"
    if mode == "is_error":
        # Native AgentError intentionally has no details channel. Its existing
        # contract projects explicit error content/structuredContent as JSON in
        # error text, not a successful externalToolResult or backend metadata.
        output = ends[0]["rawOutput"]
        assert output["details"] is None and ends[0]["status"] == "failed"
        text = output["content"][0]["text"]
        assert text.startswith("tool error: "), "native tool-error projection missing"
        error = json.loads(text.removeprefix("tool error: "))
        assert error == dict(content=[dict(type="text", text="MCP_EXPLICIT_ERROR")],
                             isError=True, structuredContent=h.STRUCTURED)
        assert "MCP_EXPLICIT_ERROR" in json.dumps(ends[0]["content"]), "ACP rendered error missing"


def run(args):
    root = new_root(args.diagnostics_dir)
    state = dict(status="running", phase="provenance", passed=[])
    peer = model = fixture = processes = None

    def landed(name):
        state["passed"].append(name)
        h.dump(root / "results.json", state)

    def start(name):
        p = HttpPeer(args.bridge, env, root, name, win)
        try:
            capabilities, _ = p.call("initialize", protocolVersion=1)
            caps = capabilities["agentCapabilities"]["mcpCapabilities"]
            assert caps["http"] is True and caps["sse"] is False, "HTTP/SSE capability mismatch"
            return p
        except BaseException:
            p.close()
            raise

    def provision(label, method="session/new", session_id=None):
        params = dict(cwd=str(first), mcpServers=fixture.config(label))
        if session_id:
            params["sessionId"] = session_id
        before = len(model.calls)
        calls_before = fixture.count("tools/call")
        result, replay = peer.call(method, **params)
        assert len(model.calls) == before and fixture.count("tools/call") == calls_before, "restore/create caused side effect"
        fixture.session_for(label)
        processes.capture(peer)
        return result.get("sessionId", session_id), replay

    def close_session(sid, label):
        _, transport = fixture.session_for(label)
        peer.call("session/close", sessionId=sid)
        # Local reachable fixture: test DELETE delivery, without claiming a
        # remote server releases resources or even receives DELETE reliably.
        h.wait_until(lambda: transport["deleted"], "local fixture did not receive DELETE", 10)

    try:
        provenance(args, root)
        win = h.Windows()
        model = h.Model(root)
        env = h.environment(root, model, args.rpi)
        fixture = Fixture(root, h)
        processes = Processes(root, win)
        first, second = root / "workspace-one", root / "workspace-two"
        first.mkdir()
        second.mkdir()
        peer = start("roundtrip")
        state["phase"] = "two-session-discovery"
        sid, _ = provision("one")
        sid2, _ = provision("two")
        http1, transport1 = fixture.session_for("one")
        http2, transport2 = fixture.session_for("two")
        assert http1 != http2, "HTTP session IDs reused"
        roundtrip(peer, model, sid, "one", "success")
        roundtrip(peer, model, sid2, "two", "success")
        assert model.selections[-1]["name"] != model.selections[-2]["name"], "tool identities crossed"
        close_session(sid2, "two")
        assert not transport1["deleted"], "closing second session closed first"
        roundtrip(peer, model, sid, "one", "success")
        assert len(transport2["calls"]) == 1 and len(transport1["calls"]) == 2
        landed("same URL/different static headers: isolated sessions, catalogs, args and DELETE lifecycle")

        for mode in h.MODES[1:-1]:
            state["phase"] = "result-" + mode
            roundtrip(peer, model, sid, "one", mode)
            roundtrip(peer, model, sid, "one", "success")
            landed(mode + ": actual native provider/ACP projection plus next successful MCP call")
        h.verify_native(root, sid, h.MODES[:-1])
        native_metadata(root, sid)
        landed("native v4 JSONL: raw content, structuredContent, metadata, image, isError, exact args and paired operations")

        state["phase"] = "blocked-cancel-recover"
        discovery = fixture.count("initialize"), fixture.count("tools/list")
        request = peer.send("session/prompt", dict(sessionId=sid,
            prompt=[dict(type="text", text="MCP:one:blocked")]))
        h.wait_until(lambda: (root / "http-blocked.json").exists(), "blocked HTTP call not reached")
        blocked = json.loads((root / "http-blocked.json").read_text(encoding="utf-8"))
        assert blocked["session"] == http1
        peer.send("session/cancel", dict(sessionId=sid), notification=True)
        result, _ = peer.response(request)
        assert result.get("stopReason") == "cancelled", "ACP cancellation did not settle"
        h.wait_until(lambda: fixture.count("notifications/cancelled") == 1, "HTTP cancellation notification missing")
        count, selections = fixture.count("tools/call"), len(model.selections)
        h.prompt(peer, sid, "MODEL_ONLY_AFTER_CANCEL")
        time.sleep(.25)  # bounded late-result observation; model roundtrip is synchronization too
        assert fixture.count("tools/call") == count and len(model.selections) == selections, "cancelled tool retried"
        assert discovery == (fixture.count("initialize"), fixture.count("tools/list")), "cancel caused discovery replay"
        close_session(sid, "one")
        peer.close()
        peer = None
        h.verify_native(root, sid, h.MODES[:-1])
        native_metadata(root, sid)
        landed("blocked HTTP call: correlated cancellation notification, ignored late result, model-only recovery, no retry/discovery")

        for method, label in (("session/load", "load"), ("session/resume", "resume")):
            state["phase"] = method
            peer = start(label)
            before, provider = fixture.count(), len(model.calls)
            error, updates = peer.response(peer.send(method, dict(sessionId=sid, cwd=str(second),
                mcpServers=fixture.config(label))), expect_error=True)
            assert "saved session workspace differs" in json.dumps(error), "cwd mismatch not rejected"
            assert not updates and fixture.count() == before and len(model.calls) == provider, "cwd rejection contacted HTTP/replayed"
            _, replay = provision(label, method, sid)
            if method == "session/load":
                assert h.TEXT in json.dumps(replay) and "MODEL_ONLY_AFTER_CANCEL" in json.dumps(replay), "history replay missing"
            roundtrip(peer, model, sid, label, "success")
            close_session(sid, label)
            peer.close()
            peer = None
            landed(method + ": fresh bridge rejects cwd mismatch with ZERO HTTP; explicit config reprovisions without tool/model replay")

        state["phase"] = "omitted-config-fresh-load"
        peer = start("omitted")
        before, provider = fixture.count(), len(model.calls)
        peer.call("session/load", sessionId=sid, cwd=str(first))
        processes.capture(peer)
        assert fixture.count() == before and len(model.calls) == provider, "omitted config was persisted/reprovisioned"
        h.prompt(peer, sid, "MODEL_ONLY_OMITTED_CONFIG")
        assert fixture.count() == before, "omitted config contacted HTTP"
        assert not [t for t in model.calls[-1].get("tools", []) if t.get("function", {}).get("name", "").startswith("mcp_")], "omitted config retained catalog"
        peer.call("session/close", sessionId=sid)
        peer.close()
        peer = None
        landed("fresh bridge load without mcpServers: history only, no HTTP/catalog/credential persistence")

        # Genuine native restore failure preflight/discovery paths, not a mock
        # native peer. Fail-closed status fixtures never redirect/replay history.
        for path in ("/401", "/404", "/redirect"):
            state["phase"] = "configured-failure" + path
            peer = start("failure-" + path[1:])
            before, provider = fixture.count(), len(model.calls)
            _, updates = peer.response(peer.send("session/load", dict(sessionId=sid,
                cwd=str(first), mcpServers=fixture.config("failure", path))), expect_error=True)
            assert not updates, "failed configuration replayed history"
            assert fixture.count() == before + 1, "configured failure retried/followed redirect"
            assert len(model.calls) == provider, "failed configuration contacted model"
            peer.close()
            peer = None
            landed(path + ": configured restore failed, no redirect/retry/model/history replay")
        secret_audit(root, fixture)
        landed("header/query canaries absent from all local evidence including provider requests/tool definitions/errors, redirected tmp, persistence, ACP output and process logs")
        state["status"] = "passed"
    except BaseException:
        state.update(status="failed", failure=traceback.format_exc())
        raise
    finally:
        errors = []
        for name, item in (("bridge", peer), ("http-fixture", fixture), ("model", model), ("owned-pids", processes)):
            if item is not None:
                try:
                    item.close()
                except BaseException:
                    errors.append(dict(resource=name, error=traceback.format_exc()))
        if fixture is not None:
            try:
                secret_audit(root, fixture, require_provider=state["status"] == "passed")
            except BaseException:
                errors.append(dict(resource="secret-audit", error=traceback.format_exc()))
        state["cleanup_errors"] = errors
        if errors:
            state["status"] = "failed"
        h.dump(root / "results.json", state)
        if errors:
            raise AssertionError("cleanup/audit failed; inspect results.json") from None
    print("PASS: loopback MCP HTTP native integration; evidence: " + str(root))


def self_test(path):
    """Exercise fixture over actual loopback sockets, without runtime processes."""
    root = new_root(path)
    fixture = Fixture(root, h)
    port = fixture.server.server_port
    serial = 0

    def request(method, params=None, sid=None, label="one", delete=False, endpoint="/mcp"):
        nonlocal serial
        serial += 1
        notification = method.startswith("notifications/")
        value = dict(jsonrpc="2.0", method=method, params=params or {})
        if not notification:
            value["id"] = serial
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
        try:
            headers = {"Authorization": fixture.tokens[label], "Content-Type": "application/json"}
            if sid:
                headers.update({"Mcp-Session-Id": sid, "MCP-Protocol-Version": VERSION})
            connection.request("DELETE" if delete else "POST", endpoint,
                               None if delete else json.dumps(value).encode(), headers)
            response = connection.getresponse()
            # Read through transport EOF, not merely Content-Length completion.
            # This tests the fixture's Connection: close contract as well.
            body = response.fp.read()
            assert len(body) == int(response.getheader("Content-Length")), "short HTTP response"
            assert response.fp.read(1) == b"", "missing HTTP EOF"
            response.close()
            h.append(root / "http-client-audit.jsonl", dict(method=method, status=response.status,
                bytesRead=len(body), eof=True, responseClosed=response.isclosed()))
            return response.status, dict(response.getheaders()), body
        finally:
            connection.close()

    try:
        for label in ("one", "two"):
            status, headers, body = request("initialize", {"protocolVersion": VERSION}, label=label)
            assert status == 200 and json.loads(body)["result"]["protocolVersion"] == VERSION
            sid = headers["Mcp-Session-Id"]
            assert request("notifications/initialized", sid=sid, label=label)[0] == 202
            assert json.loads(request("tools/list", sid=sid, label=label)[2])["result"]["nextCursor"] == "page-two"
            status, headers, body = request("tools/list", {"cursor": "page-two"}, sid, label)
            assert status == 200 and headers["Content-Type"] == "text/event-stream"
            data = "\n".join(line[6:] for line in body.decode().splitlines() if line.startswith("data: "))
            assert json.loads(data)["result"]["tools"][0]["inputSchema"] == h.SCHEMA
            for mode in h.MODES[:-1]:
                status, headers, body = request("tools/call", dict(name="probe", arguments=dict(mode=mode, query=h.QUERY)), sid, label)
                assert status == 200
                if mode == "image":
                    body = "\n".join(line[6:] for line in body.decode().splitlines() if line.startswith("data: "))
                result = json.loads(body)["result"]
                if mode == "success":
                    assert result["_meta"] == META and result["structuredContent"] == h.STRUCTURED
                if mode == "image":
                    assert result["content"][1]["data"] == h.PNG
            assert request("DELETE", sid=sid, label=label, delete=True)[0] == 200
        # Test a real concurrent blocked POST and correlated 202 cancellation.
        _, headers, _ = request("initialize", {"protocolVersion": VERSION}, label="load")
        blocked_sid = headers["Mcp-Session-Id"]
        request("notifications/initialized", sid=blocked_sid, label="load")
        outcomes = []

        def blocked_client():
            try:
                outcomes.append(request("tools/call", dict(name="probe",
                    arguments=dict(mode="blocked", query=h.QUERY)), blocked_sid, "load"))
            except BaseException as error:
                outcomes.append(error)

        worker = threading.Thread(target=blocked_client, name="fixture-self-test-client", daemon=False)
        worker.start()
        try:
            h.wait_until(lambda: (root / "http-blocked.json").exists(), "self-test blocked call", 5)
            blocked = json.loads((root / "http-blocked.json").read_text(encoding="utf-8"))
            assert request("notifications/cancelled", {"requestId": blocked["id"], "reason": "fixture-self-test"},
                           blocked_sid, "load")[0] == 202
        finally:
            # Local release is always available, independent of remote DELETE.
            for release in fixture.sessions[blocked_sid]["blocked"].values():
                release.set()
            worker.join(timeout=7)
            assert not worker.is_alive(), "self-test client thread leaked"
        assert len(outcomes) == 1 and not isinstance(outcomes[0], BaseException), "blocked self-test failed"
        assert json.loads(outcomes[0][2])["result"]["content"][0]["text"] == "LATE_HTTP_RESULT"
        assert request("DELETE", sid=blocked_sid, label="load", delete=True)[0] == 200
        for endpoint, code in (("/401", 401), ("/404", 404), ("/redirect", 307)):
            assert request("initialize", endpoint=endpoint)[0] == code

        # A vanished client must leave a positively audited write failure, not a
        # silently swallowed exception or a worker waiting for remote DELETE.
        _, headers, _ = request("initialize", {"protocolVersion": VERSION}, label="resume")
        dropped_sid = headers["Mcp-Session-Id"]
        request("notifications/initialized", sid=dropped_sid, label="resume")
        (root / "http-blocked.json").unlink()
        dropped = socket.create_connection(("127.0.0.1", port), timeout=5)
        body = json.dumps(dict(jsonrpc="2.0", id="dropped-client", method="tools/call",
            params=dict(name="probe", arguments=dict(mode="blocked", query=h.QUERY)))).encode()
        headers = (f"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {fixture.tokens['resume']}\r\n"
            f"Mcp-Session-Id: {dropped_sid}\r\nMCP-Protocol-Version: {VERSION}\r\n"
            f"Content-Length: {len(body)}\r\n\r\n").encode()
        try:
            dropped.sendall(headers + body)
            h.wait_until(lambda: (root / "http-blocked.json").exists(), "dropped client reached fixture", 5)
            import struct
            dropped.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER,
                struct.pack("hh" if os.name == "nt" else "ii", 1, 0))
        finally:
            dropped.close()
        assert request("notifications/cancelled", {"requestId": "dropped-client"}, dropped_sid, "resume")[0] == 202
        h.wait_until(lambda: any(r["mode"] == "blocked" and r["dropped"] for r in fixture.responses),
                     "dropped response write audit", 5)
        assert not fixture.sessions[dropped_sid]["deleted"], "drop test unexpectedly used DELETE"

        # Leave another blocked call and an idle accepted socket to local owner
        # cleanup. Neither receives DELETE or cancellation from the client.
        (root / "http-blocked.json").unlink()
        abandoned = socket.create_connection(("127.0.0.1", port), timeout=5)
        idle = socket.create_connection(("127.0.0.1", port), timeout=5)
        try:
            abandoned.sendall(headers + body.replace(b"dropped-client", b"abandon-client"))
            h.wait_until(lambda: (root / "http-blocked.json").exists(), "abandoned call reached fixture", 5)
            h.wait_until(lambda: len(fixture.sockets) >= 2, "idle socket accepted", 5)
            fixture.close()
            for client in (abandoned, idle):
                try:
                    while client.recv(65536):
                        pass
                except (ConnectionResetError, ConnectionAbortedError):
                    pass
        finally:
            abandoned.close()
            idle.close()
    finally:
        fixture.close()
    # Listener must no longer accept connections after explicit local cleanup.
    with socket.socket() as sock:
        sock.settimeout(.5)
        assert sock.connect_ex(("127.0.0.1", port)) != 0, "listener still accepts"
    repo = Path(__file__).resolve().parents[3]
    inputs = bridge_source_inputs(repo)
    helper = "packages/rpi-acp/tests/mcp_stdio_native.py"
    new_sources = {"packages/rpi-mcp-core/src/configured_http_tests.rs",
                   "packages/rpi-mcp-core/tests/http_proxy_isolation.rs",
                   "packages/rpi-extcore/tests/mcp_http_bridge.rs"}
    # Independent explicit regressions for the omissions established in B1.
    assert {helper, ".cargo/config.toml"} | new_sources <= inputs, "B1 inventory regression"
    manifest = {p: hashlib.sha256((repo / p).read_bytes()).hexdigest() for p in inputs}
    verify_bridge_sources(repo, manifest)
    h.dump(root / "source-sha256.json", manifest)
    rejected = []

    def rejects(label, operation, message=None):
        try:
            operation()
        except AssertionError as error:
            assert message is None or message in str(error), "wrong rejection: " + label
            rejected.append(label)
        else:
            raise AssertionError("invalid proof accepted: " + label)

    bad_manifests = [("missing", None), ("empty", {}),
        ("partial", {"Cargo.toml": manifest["Cargo.toml"]}),
        ("mismatch", dict(manifest, **{"Cargo.toml": "0" * 64})),
        ("helper-drift", dict(manifest, **{helper: "0" * 64})),
        ("missing-helper", {p: sha for p, sha in manifest.items() if p != helper}),
        ("escape", dict(manifest, **{"../outside": "0" * 64})),
        ("absolute", dict(manifest, **{repo.as_posix() + "/Cargo.toml": "0" * 64})),
        ("invalid-hash", dict(manifest, **{helper: "not-a-sha256"}))]
    bad_manifests += [("missing-new-source-" + str(i),
                      {p: sha for p, sha in manifest.items() if p != source})
                     for i, source in enumerate(sorted(new_sources))]
    for label, bad in bad_manifests:
        rejects(label, lambda bad=bad: verify_bridge_sources(repo, bad))

    # Native validator tests use only NEW evidence-local files. Never modify the
    # real native checkout or original verification/build/binary artifacts.
    native_repo = root / "synthetic-native-source"
    native_manifest = {}
    for relative in NATIVE_SOURCE_KEYS:
        path = native_repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("synthetic native validator input\n" + relative, encoding="utf-8")
        native_manifest[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
    verify_native_sources(native_repo, native_manifest)
    for label, bad in (("native-missing", None), ("native-empty", {}),
        ("native-missing-rpc", {p: sha for p, sha in native_manifest.items()
                               if p != "crates/rpi-cli/src/rpc/external_tools.rs"}),
        ("native-escape", dict(native_manifest, **{"../outside": "0" * 64})),
        ("native-invalid-hash", dict(native_manifest, **{"Cargo.lock": "bad"})),
        ("native-mismatch", dict(native_manifest, **{"Cargo.lock": "0" * 64}))):
        rejects(label, lambda bad=bad: verify_native_sources(native_repo, bad))

    secret_audit(root, fixture, require_provider=False)  # fixture-only, no model yet
    rejects("missing-provider-capture", lambda: secret_audit(root, fixture),
            "requires provider request capture")
    capture = root / "provider-requests.json"
    h.dump(capture, [])
    rejects("empty-provider-capture", lambda: secret_audit(root, fixture), "must be nonempty")
    clean_calls = [dict(messages=[], tools=[dict(type="function", function=dict(
        name="synthetic_audit_only", description="clean", parameters=h.SCHEMA))])]
    h.dump(capture, clean_calls)
    secret_audit(root, fixture)
    # Inject each full/stripped Authorization and query canary into an actual
    # model-request tools definition and root provider error JSON. Restore only
    # evidence-local test files; no real source edits or runtime execution.
    for i, canary in enumerate(fixture.secret_canaries()):
        dirty = json.loads(json.dumps(clean_calls))
        dirty[0]["tools"][0]["function"]["description"] = canary
        h.dump(capture, dirty)
        rejects("provider-tool-secret-" + str(i), lambda: secret_audit(root, fixture),
                "secret in local evidence: provider-requests.json")
        h.dump(capture, clean_calls)
        error_path = root / "provider-errors.json"
        h.dump(error_path, [canary])
        rejects("provider-error-secret-" + str(i), lambda: secret_audit(root, fixture),
                "secret in local evidence: provider-errors.json")
        error_path.unlink()
    tmp_probe = root / "tmp" / "audit-spool.json"
    tmp_probe.parent.mkdir(exist_ok=True)
    h.dump(tmp_probe, {"value": fixture.query_canary})
    rejects("tmp-secret", lambda: secret_audit(root, fixture), "secret in local evidence: tmp/")
    tmp_probe.unlink()
    # Inbound ACP configuration is intentional, whereas raw ACP stdout is not.
    h.append(root / "self-test-wire.jsonl", dict(direction="in", message=fixture.config("one")))
    h.append(root / "self-test-wire.jsonl", dict(direction="out", message={"result": "clean"}))
    stdout_probe = root / "self-test.stdout.log"
    stdout_probe.write_text(fixture.tokens["one"], encoding="utf-8")
    rejects("stdout-secret", lambda: secret_audit(root, fixture), "secret in local evidence: self-test.stdout.log")
    stdout_probe.unlink()
    secret_audit(root, fixture)
    h.dump(root / "self-test.json", dict(status="passed", nativeExecuted=False,
        providerCapture="synthetic audit-only requests, NOT native/model execution evidence",
        manifestInputs=len(manifest), rejectedManifests=rejected[:len(bad_manifests)],
        rejectedProofs=rejected,
        responses=len(fixture.responses), drops=sum(r["dropped"] for r in fixture.responses),
        covered=["JSON", "split multiline Unicode SSE", "all result modes", "two header identities",
                 "DELETE", "correlated cancellation", "late result", "client reset/write drop",
                 "owner close with blocked/idle clients and no DELETE", "response bytes/EOF/close",
                 "listener/workers/socket cleanup", "mandatory source manifest including helper/new Rust/config",
                 "nonempty native RPC source manifest", "provider request/tools/error canary rejection",
                 "missing/empty provider capture rejection", "tmp and stdout canary rejection",
                 "explicit fixture-wire/ACP-inbound exclusions"]))
    print("PASS: HTTP fixture and security self-tests only; no bridge/native executed")


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--self-test":
        self_test(Path(sys.argv[2]))
        return
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("bridge", "rpi", "diagnostics-dir", "build-marker", "native-build-record", "native-verification"):
        parser.add_argument("--" + flag, required=True, type=Path)
    parser.add_argument("--native-sha", required=True)
    args = parser.parse_args()
    args.bridge, args.rpi = args.bridge.resolve(), args.rpi.resolve()
    run(args)


if __name__ == "__main__":
    try:
        main()
    except BaseException:
        # Full evidence stays local; do not accidentally print fake wire values
        # through assertion/HTTP exception reprs on a CI console.
        if sys.exc_info()[0] is SystemExit:
            raise
        print("FAIL: HTTP native harness; inspect the requested diagnostics directory", file=sys.stderr)
        sys.exit(1)
