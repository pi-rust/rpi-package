"""Stage7 ACP session/list: provenance-gated, loopback-only real-native smoke.

Never builds, searches PATH for a runtime, or qualifies an installed binary.
--write-source-manifest is a SAFE pre-build inventory operation (no native launch).
Execution requires a parent freeze/build marker; see --help and the stage7 report.
Windows Job containment is required. Synthetic header-only fixtures are explicitly
NOT evidence of native-created conversations. All output is target-only.
"""
import argparse
import ctypes
import datetime
import hashlib
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import sys
import threading
import time
import traceback
import uuid

sys.dont_write_bytecode = True
import mcp_stdio_native as h
from mcp_http_fixture import Fixture

TARGET = Path("C:/Users/bigfish/.cargo/target/acp-stage7-01")
REPO = Path(__file__).resolve().parents[3]
PYTHON_INPUTS = {"packages/rpi-acp/tests/" + name for name in
                 ("session_list_native.py", "mcp_stdio_native.py", "mcp_http_fixture.py")}
MAX_FRAMES = 4096
MAX_WIRE = 32 * 1024 * 1024


def require(condition, message):
    # Do not disable safety or assertions under python -O.
    if not condition:
        raise AssertionError(message)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def target_path(path):
    require(path.is_absolute(), "output path must be absolute")
    result = path.resolve()
    require(result != TARGET.resolve() and result.is_relative_to(TARGET.resolve()),
            "all output must be beneath the stage7 target evidence root")
    return result


def head(repo):
    return subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"],
                                   text=True, stderr=subprocess.PIPE).strip()


def inventory(repo, python_inputs=()):
    paths = subprocess.check_output(["git", "-C", str(repo), "ls-files", "--cached",
        "--others", "--exclude-standard", "-z"]).decode("utf-8").split("\0")
    selected = {p for p in paths if p and (p.endswith(".rs") or
        Path(p).name in ("Cargo.toml", "Cargo.lock") or
        (".cargo" in Path(p).parts and Path(p).name in ("config", "config.toml")))}
    selected.update(python_inputs)
    require(selected, "empty source inventory")
    result = {}
    for name in sorted(selected):
        path = (repo / name).resolve()
        require(path.is_relative_to(repo.resolve()) and path.is_file(),
                "source inventory has missing/escaping input: " + name)
        result[name] = sha(path)
    return result


def write_manifest(path):
    path = target_path(path)
    require(not path.exists(), "manifest must be new; preserve previous evidence")
    path.parent.mkdir(parents=True, exist_ok=True)
    h.dump(path, dict(schema="stage7-session-list-source-v1", head=head(REPO),
        createdAtNs=time.time_ns(), sourceSha256=inventory(REPO, PYTHON_INPUTS)))
    print(str(path))


def provenance(args, root):
    for name in ("bridge", "rpi", "build_marker", "source_manifest",
                 "native_build_record", "native_verification"):
        path = getattr(args, name)
        require(path.is_absolute() and path.is_file(), "explicit absolute file required: " + name)
    marker = json.loads(args.build_marker.read_text(encoding="utf-8"))
    manifest = json.loads(args.source_manifest.read_text(encoding="utf-8"))
    native_build = json.loads(args.native_build_record.read_text(encoding="utf-8"))
    native = json.loads(args.native_verification.read_text(encoding="utf-8"))
    require(marker["build"]["exitCode"] == 0 and native_build["exitCode"] == 0,
            "successful parent bridge/native builds required")
    require(Path(marker["bridge"]).resolve() == args.bridge.resolve(), "bridge path mismatch")
    require(marker["sha256"] == args.bridge_sha == sha(args.bridge), "bridge SHA mismatch")
    require(native["nativeBinary"]["sha256"] == args.native_sha == sha(args.rpi), "native SHA mismatch")
    require(Path(native["nativeBinary"]["path"]).resolve() == args.rpi.resolve(), "native path mismatch")
    require(marker["head"] == manifest["head"] == head(REPO), "bridge HEAD mismatch")
    binding = marker["sourceManifest"]
    require(Path(binding["path"]).resolve() == args.source_manifest.resolve() and
            binding["sha256"] == sha(args.source_manifest), "prebuild manifest not bound to marker")
    require(manifest["createdAtNs"] <= args.source_manifest.stat().st_mtime_ns <=
            marker["build"]["startedAtNs"] <= marker["build"]["finishedAtNs"] <=
            args.build_marker.stat().st_mtime_ns, "manifest must precede successful parent build")
    require(Path(marker["build"]["cwd"]).resolve() == REPO, "bridge build checkout mismatch")
    require(marker["sourceSha256"] == manifest["sourceSha256"] == inventory(REPO, PYTHON_INPUTS),
            "source inventory mismatch (including new/untracked Rust and imported Python)")
    native_repo = Path(native_build["cwd"]).resolve()
    native_binding = native_build["sourceManifest"]
    native_manifest_path = Path(native_binding["path"])
    require(native_manifest_path.is_absolute() and native_manifest_path.is_file(),
            "explicit existing native prebuild manifest required")
    native_manifest = json.loads(native_manifest_path.read_text(encoding="utf-8"))
    require(Path(native["sourceManifest"]["path"]).resolve() == native_manifest_path.resolve() and
            native["sourceManifest"]["sha256"] == native_binding["sha256"] == sha(native_manifest_path),
            "native build and verification must bind the same exact prebuild manifest")
    require(native["buildRecordSha256"] == sha(args.native_build_record),
            "native verification must bind the exact successful build record")
    require(native["head"] == native_manifest["head"] == head(native_repo), "native HEAD mismatch")
    require(native["sourceSha256"] == native_manifest["sourceSha256"] == inventory(native_repo),
            "native provenance must cover exact current Rust/build inventory, not legacy partial keys")
    require(native_manifest["createdAtNs"] <= native_manifest_path.stat().st_mtime_ns <=
            native_build["startedAtNs"] <= native_build["finishedAtNs"] <=
            args.native_build_record.stat().st_mtime_ns <= args.native_verification.stat().st_mtime_ns,
            "native manifest must precede build, build record and verification (no retrospective inventory)")
    require(native_build["args"] ==
            ["cargo", "build", "--offline", "--locked", "-p", "rpi-cli", "--bin", "rpi"],
            "native build record must identify the qualified locked/offline rpi-cli binary build")
    native_target = Path(native_build["target"])
    require(native_target.is_absolute() and
            args.rpi.resolve() == (native_target / "debug" / "rpi.exe").resolve(),
            "native binary must match recorded Cargo target/debug output")
    # A second complete inventory is checked immediately before the first Popen.
    h.dump(root / "provenance.json", dict(bridgeMarker=marker, sourceManifest=manifest,
        nativeBuild=native_build, nativeVerification=native,
        nativeSourceManifest=native_manifest, nativeSourceManifestPath=str(native_manifest_path),
        bridge=str(args.bridge), native=str(args.rpi), bridgeSha=sha(args.bridge),
        nativeSha=sha(args.rpi), limitation="parent attestation; not a cryptographic build attestation"))
    return native_repo, manifest["sourceSha256"], native["sourceSha256"]


class Peer(h.Peer):
    """Reuse suspended launch / Job cleanup; bound frames, queue and wire evidence."""
    def __init__(self, *args):
        self.wire_bytes = 0
        self.frame_count = 0
        super().__init__(*args)

    def record(self, direction, value):
        encoded = json.dumps(value, ensure_ascii=False)
        with self.lock:
            self.frame_count += 1
            self.wire_bytes += len(encoded.encode("utf-8"))
            require(self.frame_count <= MAX_FRAMES and self.wire_bytes <= MAX_WIRE,
                    "ACP evidence bound exceeded")
            h.append(self.root / (self.name + "-wire.jsonl"), dict(direction=direction, message=value))

    def absent_params(self):
        self.counter += 1
        value = dict(jsonrpc="2.0", id=self.counter, method="session/list")
        self.record("in", value)
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()
        return self.response(self.counter)

    def any_response(self, request_id):
        deadline = time.monotonic() + h.TIMEOUT
        while time.monotonic() < deadline:
            try:
                value = self.output.get(timeout=max(.01, deadline - time.monotonic()))
            except queue.Empty:
                break
            if isinstance(value, BaseException):
                raise value
            require(value.get("id") == request_id and "method" not in value,
                    "unexpected response/notification during list cancellation")
            return value
        raise AssertionError("list cancellation response timed out")

    def pump(self):
        # Parent creates the queue before starting this thread. Do not replace it:
        # response() may already be waiting on that exact queue instance.
        try:
            with (self.root / (self.name + ".stdout.log")).open("w", encoding="utf-8") as raw:
                while True:
                    line = self.process.stdout.readline(1024 * 1024 + 1)
                    if not line:
                        break
                    require(len(line) <= 1024 * 1024 and line.endswith("\n"), "oversized/partial ACP frame")
                    raw.write(line)
                    raw.flush()
                    value = json.loads(line)
                    self.record("out", value)
                    self.output.put_nowait(value)
        except BaseException as error:
            self.output.put(error, timeout=2)
        finally:
            self.output.put(EOFError("bridge stdout closed"), timeout=2)


class Jobs:
    """Account total ever-assigned processes, including short-lived listing children."""
    def __init__(self, win, root):
        self.win, self.root, self.handles = win, root, {}
        win.k.QueryInformationJobObject.argtypes = [ctypes.c_void_p, ctypes.c_int,
            ctypes.c_void_p, ctypes.c_ulong, ctypes.c_void_p]
        win.k.QueryInformationJobObject.restype = ctypes.c_int
        win.k.QueryFullProcessImageNameW.argtypes = [ctypes.c_void_p, ctypes.c_ulong,
            ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_ulong)]
        win.k.QueryFullProcessImageNameW.restype = ctypes.c_int
        win.k.GetSystemDirectoryW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint]
        win.k.GetSystemDirectoryW.restype = ctypes.c_uint

    def cold(self, peer, bridge):
        # CREATE_NO_WINDOW can still produce an owned headless conhost.exe.
        # Do not baseline arbitrary children: validate each live image, and
        # require total-ever == live count so exited transient children fail too.
        pids = self.capture(peer)
        system = ctypes.create_unicode_buffer(32768)
        length = self.win.k.GetSystemDirectoryW(system, len(system))
        require(0 < length < len(system), "GetSystemDirectoryW failed")
        console = (Path(system.value) / "conhost.exe").resolve()
        console_count = 0
        for pid in pids:
            size = ctypes.c_ulong(32768)
            image = ctypes.create_unicode_buffer(size.value)
            self.win.check(self.win.k.QueryFullProcessImageNameW(
                self.handles[pid], 0, image, ctypes.byref(size)))
            actual = Path(image.value).resolve()
            if pid == peer.process.pid:
                require(actual == bridge.resolve(), "cold bridge image mismatch")
            else:
                require(actual == console, "unexpected cold child image")
                console_count += 1
            h.append(self.root / "pid-audit.jsonl", dict(peer=peer.name, pid=pid,
                image=str(actual), cold=True))
        require(peer.process.pid in pids and console_count <= 1 and
                self.total(peer) == len(pids), "cold Job contains an unexpected/exited child")
        peer.cold_process_count = len(pids)

    def total(self, peer):
        buffer = ctypes.create_string_buffer(48)
        self.win.check(self.win.k.QueryInformationJobObject(peer.job, 1, buffer, len(buffer), None))
        # Four LARGE_INTEGER times, then DWORD pageFaultCount (offset32),
        # totalProcesses (offset36), activeProcesses (40), terminatedProcesses (44).
        total = ctypes.c_ulong.from_buffer(buffer, 36).value
        h.append(self.root / "pid-audit.jsonl", dict(peer=peer.name, totalEverAssigned=total))
        return total

    def capture(self, peer):
        buffer = ctypes.create_string_buffer(8 + 64 * ctypes.sizeof(ctypes.c_size_t))
        self.win.check(self.win.k.QueryInformationJobObject(peer.job, 3, buffer, len(buffer), None))
        assigned = ctypes.c_ulong.from_buffer(buffer, 0).value
        count = ctypes.c_ulong.from_buffer(buffer, 4).value
        require(1 <= count == assigned <= 64, "Job PID inventory overflow")
        pids = list((ctypes.c_size_t * count).from_buffer(buffer, 8))
        for pid in pids:
            if pid not in self.handles:
                self.handles[pid] = self.win.open_live(pid)
                h.append(self.root / "pid-audit.jsonl", dict(peer=peer.name, pid=pid, live=True))
        return pids

    def dead(self):
        try:
            for pid, handle in self.handles.items():
                h.wait_until(lambda: not self.win.alive(handle), "owned process survived cleanup", 15)
                h.append(self.root / "pid-audit.jsonl", dict(pid=pid, dead=True))
        finally:
            for handle in self.handles.values():
                self.win.k.CloseHandle(handle)
            self.handles.clear()


def list_all(peer, cwd=None):
    params = {} if cwd is None else {"cwd": str(cwd)}
    sessions, pages, seen = [], [], set()
    deadline = time.monotonic() + 45
    for _ in range(256):
        require(time.monotonic() < deadline, "catalog overall deadline exceeded")
        page, updates = peer.call("session/list", **params)
        require(not updates and isinstance(page.get("sessions"), list), "unexpected list output")
        require(len(page["sessions"]) <= 50, "wire page exceeds fixed50")
        for item in page["sessions"]:
            require(set(item) <= {"sessionId", "cwd", "updatedAt"} and
                    {"sessionId", "cwd"} <= set(item), "unsafe/missing summary fields")
            require(str(uuid.UUID(item["sessionId"])) == item["sessionId"] and
                    Path(item["cwd"]).is_absolute(), "invalid summary identity/cwd")
            if "updatedAt" in item:
                datetime.datetime.fromisoformat(item["updatedAt"].replace("Z", "+00:00"))
        pages.append(page)
        sessions.extend(page["sessions"])
        token = page.get("nextCursor")
        if token is None:
            require(len({s["sessionId"] for s in sessions}) == len(sessions), "duplicate catalog IDs")
            return sessions, pages
        require(isinstance(token, str) and 0 < len(token) <= 256 and token.isascii(), "invalid opaque cursor")
        # Empty sessions WITH a continuation is incomplete work, never EOF.
        require(token not in seen, "cursor loop")
        seen.add(token)
        params["cursor"] = token
    raise AssertionError("catalog page/scan-step bound exceeded")


def invalid(peer, params, code=-32602):
    error, updates = peer.response(peer.send("session/list", params), expect_error=True)
    require(error.get("code") == code and not updates, "wrong list validation response")
    return error


def files_state(path):
    return {p.relative_to(path).as_posix(): (sha(p), p.stat().st_size, p.stat().st_mtime_ns)
            for p in path.rglob("*") if p.is_file()}


def audit(root, fixture):
    secrets = fixture.secret_canaries()
    checked = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(root).as_posix()
        if relative == "http-fixture-wire.jsonl":
            continue  # intentional fake credentials on fixture-owned HTTP ingress
        if path.parent == root and path.name.endswith("-wire.jsonl"):
            for line in path.read_text(encoding="utf-8").splitlines():
                item = json.loads(line)
                if item.get("direction") == "out":
                    require(not any(s in json.dumps(item, ensure_ascii=False) for s in secrets),
                            "canary in ACP outbound")
        else:
            data = path.read_bytes().decode("utf-8", errors="replace")
            if path.suffix in (".json", ".jsonl"):
                try:
                    data += json.dumps(json.loads(data), ensure_ascii=False)
                except ValueError:
                    for line in data.splitlines():
                        try:
                            require(not any(s in json.dumps(json.loads(line), ensure_ascii=False)
                                            for s in secrets), "escaped canary in persistence")
                        except ValueError:
                            pass
            require(not any(s in data for s in secrets), "canary in output/persistence: " + relative)
        checked.append(relative)
    calls = json.loads((root / "provider-requests.json").read_text(encoding="utf-8"))
    require(calls and any(c.get("tools") for c in calls), "provider/tool definitions capture missing")
    h.dump(root / "secret-audit.json", dict(checked=checked, leaked=False,
        exclusions=["http-fixture-wire.jsonl: intentional fake credential ingress",
                    "ACP inbound configuration: intentional fake credentials"],
        scope="all local evidence, provider requests and ALL tool definitions, isolated persistence/temp; not remote-output DLP"))


def run(args):
    root = target_path(args.diagnostics_dir)
    require(not root.exists(), "use a fresh evidence directory")
    root.mkdir(parents=True)
    state = dict(status="running", passed=[], pending=["full native run until completion"])
    peer = model = fixture = jobs = None
    cleanup_errors = []

    def landed(text):
        state["passed"].append(text)
        h.dump(root / "results.json", state)

    def start(name, environment=None, listing=False):
        # Recheck exact source and binary binding before EVERY bridge execution.
        require(inventory(REPO, PYTHON_INPUTS) == bridge_sources and
                inventory(native_repo) == native_sources and
                sha(args.bridge) == args.bridge_sha and sha(args.rpi) == args.native_sha,
                "preexecute source/binary drift")
        p = Peer(args.bridge, environment or env, root, name, win)
        try:
            caps, _ = p.call("initialize", protocolVersion=1, clientCapabilities={})
            require(caps["agentCapabilities"]["sessionCapabilities"]["list"] == {},
                    "session/list capability missing")
            jobs.cold(p, args.bridge)
            if listing:
                store = Path((environment or env)["RPI_ACP_SESSION_DIR"])
                p.listing_fence = (store, len(model.calls), fixture.count(), files_state(store))
            return p
        except BaseException:
            p.close()
            raise

    def close():
        nonlocal peer
        if peer is not None:
            p, peer = peer, None
            try:
                if hasattr(p, "listing_fence"):
                    # Check AFTER graceful process exit but BEFORE disposing the
                    # Job. This fences even the immediate-EOF/transient-child
                    # interval, and does not rely on sampling a surviving PID.
                    p.process.stdin.close()
                    require(p.process.wait(timeout=20) == 0, "listing bridge nonzero exit")
                    store, calls, requests, disk = p.listing_fence
                    require(jobs.total(p) == p.cold_process_count,
                            "cold listing lifecycle spawned an extra process")
                    require((calls, requests) == (len(model.calls), fixture.count()) and
                            disk == files_state(store),
                            "cold listing lifecycle contacted fixtures or wrote sessions")
            finally:
                try:
                    p.close()
                finally:
                    jobs.dead()

    def listing_only(p, cwd=None):
        before = len(model.calls), fixture.count(), jobs.total(p)
        disk = files_state(Path(env["RPI_ACP_SESSION_DIR"]))
        result = list_all(p, cwd)
        require(before == (len(model.calls), fixture.count(), jobs.total(p)) and
                before[2] == p.cold_process_count,
                "listing spawned a child or contacted provider/MCP")
        require(disk == files_state(Path(env["RPI_ACP_SESSION_DIR"])), "listing wrote session files")
        return result

    try:
        require(sys.flags.optimize == 0, "do not run with -O: imported fixture assertions must remain enabled")
        native_repo, bridge_sources, native_sources = provenance(args, root)
        win = h.Windows()
        jobs = Jobs(win, root)
        model = h.Model(root)
        env = h.environment(root, model, args.rpi)
        fixture = Fixture(root, h)
        workspaces = [root / "workspace-one", root / "workspace-two"]
        for path in workspaces:
            path.mkdir()
        ids = []
        peer = start("native-create")
        for i in range(3):
            label = ("one", "two", "failure")[i]
            workspace = workspaces[i % 2]
            result, _ = peer.call("session/new", cwd=str(workspace), mcpServers=fixture.config(label))
            sid = result["sessionId"]
            ids.append(sid)
            jobs.capture(peer)
            before = fixture.count("tools/call")
            h.roundtrip(peer, model, sid, label, "success")
            require(fixture.count("tools/call") == before + 1, "real side-effect fixture call missing")
            peer.call("session/close", sessionId=sid)
            entries = h.transcript(root, sid)
            require(any(o.get("type") == "toolCall" for o in h.objects(entries)), "native tool history missing")
        close()
        landed("REAL-NATIVE: three model+MCP conversations in two cwd values, native v4 JSONL, graceful close/restart")
        peer = start("native-list", listing=True)
        listed, _ = listing_only(peer)
        require({s["sessionId"] for s in listed} == set(ids), "persisted native IDs not discovered")
        filtered, _ = listing_only(peer, workspaces[1])
        require([s["sessionId"] for s in filtered] == [ids[1]], "cwd filter incorrect")
        close()
        landed("REAL-NATIVE: persisted list/filter, no native/MCP/provider contact or session writes (Job total-ever PID accounting)")
        selected = ids[0]
        for method in ("session/load", "session/resume"):
            peer = start(method.replace("/", "-"))
            before = fixture.count("tools/call"), len(model.calls)
            _, replay = peer.call(method, sessionId=selected, cwd=str(workspaces[0]),
                                  mcpServers=fixture.config(method.split("/")[1]))
            jobs.capture(peer)
            require(before == (fixture.count("tools/call"), len(model.calls)), "restore replayed side effect/model")
            if method == "session/load":
                require("MCP:one:success" in json.dumps(replay) and h.TEXT + ":one" in json.dumps(replay),
                        "load failed to replay selected history")
                require(all(h.TEXT + ":" + other not in json.dumps(replay) for other in ("two", "failure")),
                        "load replay crossed conversation identity")
            else:
                require(not any(o.get("sessionUpdate") in ("user_message_chunk", "agent_message_chunk",
                    "tool_call", "tool_call_update") for o in h.objects(replay)), "resume replayed history")
            tool_before = fixture.count("tools/call")
            h.prompt(peer, selected, "MODEL_ONLY_AFTER_" + method)
            require(fixture.count("tools/call") == tool_before, "old side effect replayed on continuation")
            require("MCP:one:success" in json.dumps(model.calls[-1]["messages"]),
                    "continued native model lost selected saved context")
            tool_text = "\n".join(h.as_text(m.get("content")) for m in model.calls[-1]["messages"]
                                  if m.get("role") == "tool")
            require(h.TEXT + ":one" in tool_text and
                    all(h.TEXT + ":" + other not in tool_text for other in ("two", "failure")),
                    "continued provider context lost/crossed selected tool result")
            structured = []
            for index, char in enumerate(tool_text):
                if char == "{":
                    try:
                        structured.append(json.JSONDecoder().raw_decode(tool_text[index:])[0])
                    except ValueError:
                        pass
            require(h.STRUCTURED in structured, "continued provider context lost selected structured result")
            peer.call("session/close", sessionId=selected)
            close()
        landed("REAL-NATIVE: selected ID load/history and resume/no-history, model continuation retains context, no old tool side-effect replay")
        peer = start("native-delete")
        peer.call("session/delete", sessionId=selected)
        close()
        peer = start("native-post-delete", listing=True)
        listed, _ = listing_only(peer)
        require({s["sessionId"] for s in listed} == set(ids[1:]), "deleted ID remains in fresh list")
        close()
        landed("REAL-NATIVE: delete followed by a fresh-process list excludes the deleted ID")

        # Separate storage root. Header-only fixtures deliberately make NO claim
        # about native creation, model context, message reads or tool execution.
        template_path = next((root / "sessions" / ids[1]).rglob("*.jsonl"))
        with template_path.open(encoding="utf-8") as stream:
            template = json.loads(stream.readline())
        synthetic = root / "synthetic101-header-only-NOT-native-created"
        synthetic.mkdir()
        synthetic_env = dict(env, RPI_ACP_SESSION_DIR=str(synthetic))
        expected = []
        for i in range(101):
            sid = str(uuid.UUID(int=i + 1, version=4))
            created = 1700000000000 + (i // 3) * 1000
            header = dict(template, id=sid, cwd=str(workspaces[i % 2]), createdAt=created)
            # Native encoding for each cwd is taken from an actual native path.
            source = next((root / "sessions" / ids[1 if i % 2 else 2]).rglob("*.jsonl"))
            directory = synthetic / sid / source.parent.name
            directory.mkdir(parents=True)
            instant = datetime.datetime.fromtimestamp(created / 1000, datetime.timezone.utc)
            prefix = instant.strftime("%Y-%m-%dT%H-%M-%S-") + f"{created % 1000:03}Z"
            (directory / (prefix + "_" + sid + ".jsonl")).write_text(json.dumps(header) + "\n", encoding="utf-8")
            expected.append((created, sid, i % 2))
        expected.sort(key=lambda item: (-item[0], item[1]))
        peer = start("synthetic101", synthetic_env, listing=True)
        before = len(model.calls), fixture.count(), jobs.total(peer), files_state(synthetic)
        rows, pages = list_all(peer)
        require([s["sessionId"] for s in rows] == [x[1] for x in expected], "synthetic sort/order/IDs mismatch")
        require([len(p["sessions"]) for p in pages if p["sessions"]] == [50, 50, 1], "fixed50 pagination mismatch")
        require(any(not p["sessions"] and p.get("nextCursor") for p in pages),
                "101-file scan did not exercise incomplete empty-page continuation")
        first = next(p for p in pages if p["sessions"])
        token = first["nextCursor"]
        again, _ = peer.call("session/list", cursor=token)
        again2, _ = peer.call("session/list", cursor=token)
        require(again == again2 and len(again["sessions"]) == 50, "completed-page cursor not idempotent")
        invalid(peer, dict(cursor=token, cwd=str(workspaces[0])))
        invalid(peer, dict(cursor="0"))
        invalid(peer, dict(cursor="../../outside"))
        invalid(peer, dict(cursor="x" * 257))
        invalid(peer, dict(cursor=123))
        invalid(peer, dict(cwd="relative"))
        invalid(peer, dict(cwd=str(root / "missing-cwd")))
        invalid(peer, [])
        filtered, _ = list_all(peer, workspaces[0])
        require([s["sessionId"] for s in filtered] == [x[1] for x in expected if x[2] == 0], "synthetic cwd filter mismatch")
        absent, _ = peer.absent_params()
        require("sessions" in absent, "absent optional list params rejected")
        for params in (None, {"cwd": None, "cursor": None}):
            result, _ = peer.response(peer.send("session/list", params))
            require("sessions" in result, "nullable optional list params rejected")
        require(before[2] == peer.cold_process_count and before == (len(model.calls), fixture.count(), jobs.total(peer), files_state(synthetic)),
                "synthetic list performed I/O beyond catalog reads")
        close()
        peer = start("cursor-other-connection", synthetic_env, listing=True)
        invalid(peer, dict(cursor=token))
        close()
        landed("SYNTHETIC ONLY: 101 valid header-only files, empty incomplete scan continuation, createdAt desc/UUID asc, fixed50 pages, cursor retry/opacity/connection+filter binding, invalid/absent/null params, no children/contact/writes")
        peer = start("cancel-list-race", synthetic_env, listing=True)
        request_id = peer.send("session/list", {})
        peer.send("$/cancel_request", dict(requestId=request_id), notification=True)
        envelope = peer.any_response(request_id)
        if "error" in envelope:
            require(envelope["error"].get("code") == -32800, "wrong cancellation error")
            state["cancelRaceOutcome"] = "cancelled"
        else:
            require(isinstance(envelope.get("result", {}).get("sessions"), list), "invalid cancellation-race success")
            state["cancelRaceOutcome"] = "already completed valid page (allowed race)"
        # A subsequent full list is a fence: duplicate/late cancellation responses
        # are returned as updates and rejected by list_all, not silently swallowed.
        list_all(peer)
        require(jobs.total(peer) == peer.cold_process_count, "cancellation spawned native")
        close()
        peer = start("eof-active-list", synthetic_env, listing=True)
        peer.send("session/list", {})
        close()  # stdin EOF while work may still be active; joined Job/PID audit
        landed("SYNTHETIC ONLY: cancel race has exactly one result or -32800 then healthy list; immediate EOF drains bridge/Job within cleanup bounds")

        malformed = root / "malformed-header"
        malformed.mkdir()
        sid = str(uuid.uuid4())
        directory = malformed / sid / template_path.parent.name
        directory.mkdir(parents=True)
        (directory / (template_path.name.rsplit("_", 1)[0] + "_" + sid + ".jsonl")).write_text("{bad\n", encoding="utf-8")
        peer = start("malformed", dict(env, RPI_ACP_SESSION_DIR=str(malformed)), listing=True)
        error, _ = peer.response(peer.send("session/list", {}), expect_error=True)
        require(error.get("code") == -32001 and error.get("message") == "session/list unavailable" and
                str(malformed) not in json.dumps(error) and "{bad" not in json.dumps(error),
                "malformed header must return exact sanitized Unavailable error")
        require(jobs.total(peer) == peer.cold_process_count, "malformed list spawned native")
        close()
        landed("SYNTHETIC ONLY: matching malformed header fails with sanitized error and no child")
        missing = root / "missing-session-root"
        peer = start("missing-root", dict(env, RPI_ACP_SESSION_DIR=str(missing)), listing=True)
        before = len(model.calls), fixture.count()
        rows, _ = list_all(peer)
        require(not rows and not missing.exists() and jobs.total(peer) == peer.cold_process_count and
                before == (len(model.calls), fixture.count()), "missing-root list spawned/contacted/wrote")
        close()
        landed("EMPTY FIXTURE: missing session root lists empty without creating it, spawning children or contacting fixtures")
        state["pending"] = ["deterministic cancellation after positively observed active catalog worker",
            "server resource-budget exhaustion / cursor TTL / duplicate concurrent IDs (Rust focused tests required)"]
        state["status"] = "passed"
    except BaseException:
        state["status"] = "failed"
        state["error"] = traceback.format_exc()
        raise
    finally:
        for name, action in (("bridge/Job", close),
                             ("PID handles", lambda: jobs.dead() if jobs else None),
                             ("HTTP fixture", lambda: fixture.close() if fixture else None),
                             ("model", lambda: model.close() if model else None)):
            try:
                action()
            except BaseException:
                cleanup_errors.append(dict(resource=name, error=traceback.format_exc()))
        if model is not None:
            try:
                sock = socket.socket()
                sock.settimeout(1)
                try:
                    require(sock.connect_ex(("127.0.0.1", model.server.server_port)) != 0,
                            "model listener still reachable after close")
                finally:
                    sock.close()
            except BaseException:
                cleanup_errors.append(dict(resource="model listener", error=traceback.format_exc()))
        state["cleanupErrors"] = cleanup_errors
        if cleanup_errors:
            state["status"] = "failed"
        h.dump(root / "results.json", state)
        # Audit the final persisted status AND cleanup diagnostics, not the last
        # intermediate landed() snapshot. Never leave a stale successful audit.
        if fixture is not None and model is not None:
            try:
                audit(root, fixture)
            except BaseException:
                cleanup_errors.append(dict(resource="canary audit", error="final persisted evidence audit failed"))
                state["status"] = "failed"
                # Audit failures/other cleanup tracebacks may themselves contain
                # canaries. Redact strings recursively before persisting failure.
                def redact(value):
                    if isinstance(value, str):
                        for secret in fixture.secret_canaries():
                            value = value.replace(secret, "[REDACTED]")
                        return value
                    if isinstance(value, list):
                        return [redact(item) for item in value]
                    if isinstance(value, dict):
                        return {redact(key): redact(item) for key, item in value.items()}
                    return value
                state = redact(state)
                h.dump(root / "results.json", state)
                try:
                    # Include the rewritten final failure result in the audit too.
                    audit(root, fixture)
                except BaseException:
                    pass  # Already a recorded failure; never promote it to pass.
                finally:
                    h.dump(root / "secret-audit.json", dict(status="failed", leaked=None,
                        error="final persisted evidence audit failed; no clean-audit claim"))
        if cleanup_errors and sys.exc_info()[0] is None:
            raise AssertionError("cleanup/audit failed; see results.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--write-source-manifest", type=Path,
        help="SAFE: new target-only JSON inventory BEFORE parent build; executes no binaries")
    for flag in ("bridge", "rpi", "diagnostics-dir", "build-marker", "source-manifest",
                 "native-build-record", "native-verification"):
        parser.add_argument("--" + flag, type=Path, help="explicit absolute path (required for execution)")
    parser.add_argument("--bridge-sha", help="explicit parent-built bridge SHA256")
    parser.add_argument("--native-sha", help="explicit native SHA256; no hardcoded stage6 path/hash")
    args = parser.parse_args()
    if args.write_source_manifest:
        write_manifest(args.write_source_manifest)
        return
    for field in ("bridge", "rpi", "diagnostics_dir", "build_marker", "source_manifest",
                  "native_build_record", "native_verification", "bridge_sha", "native_sha"):
        if getattr(args, field) is None:
            parser.error("execution requires --" + field.replace("_", "-"))
    run(args)


if __name__ == "__main__":
    main()
