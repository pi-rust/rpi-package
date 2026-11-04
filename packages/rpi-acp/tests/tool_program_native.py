#!/usr/bin/env python3
"""Opt-in Windows frozen-native ACP read-only recovered-child acceptance.

Standard library only. Requires explicit trusted binaries, SHA-256s, retained
prebuild source evidence and a NEW output directory. Never builds, installs,
restarts services, searches PATH for rpi, prompts, replays queries or edits source
sessions. This qualifies only this Windows bridge -> launcher -> frozen native
transport chain against retained transcripts; NOT live history/execution overlap,
UI, provider behavior, full-body aggregate integrity, Unix, deployment or CI.
ACP EOF uses the existing owner terminate path, not graceful native stop. The
proof records native exit 0 or owner termination 1 after EOF and requires no
surviving owned processes; it does not qualify graceful native shutdown.
All wire logs may contain retained fixture content; keep proof artifacts local.
"""
import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import threading
import time
import traceback
import uuid

METHODS = ["session/tool_program_" + s for s in ("children", "phases", "body", "snapshot")]
READ_COMMANDS = {"get_state", "get_available_models", "get_available_thinking_levels",
                 "get_thinking_level", "get_context_usage", "get_session_branch",
                 "get_durable_state", "get_tool_program_children",
                 "get_tool_program_child_phases", "get_tool_program_child_body", "stop"}
EXECUTION = {"agent_start", "agent_end", "agent_settled", "operation_accepted",
             "tool_execution_start", "tool_execution_update", "tool_execution_end",
             "message_start", "message_update", "message_end", "external_tool_request",
             "tool_program_child_begin", "tool_program_child_phase", "file_io_request"}
SCOPE = "Windows actual frozen-native ACP recovered read-only child queries; first body page only, no live execution/UI/provider/Unix/CI/deployment qualification"


def require(value, message):
    if not value:
        raise AssertionError(message)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")


def records(path):
    with Path(path).open(encoding="utf-8") as stream:
        return [json.loads(line) for line in stream if line.strip()]


def absolute(value):
    path = Path(value)
    require(path.is_absolute(), "explicit absolute path required: " + value)
    return path.resolve()


# A closed explicit identity registry, NOT a process tree. Toolhelp parent PIDs
# alone never confer ownership (dead parents, PID reuse, snapshot/open races).
class WindowsProcesses:
    def __init__(self):
        self.k = ctypes.WinDLL("kernel32", use_last_error=True)
        H, D, B = wintypes.HANDLE, wintypes.DWORD, wintypes.BOOL
        P = ctypes.POINTER
        signatures = {
            "CreateToolhelp32Snapshot": (H, [D, D]),
            "OpenProcess": (H, [D, B, D]),
            "CloseHandle": (B, [H]),
            "GetCurrentProcess": (H, []),
            "DuplicateHandle": (B, [H, H, H, P(H), D, B, D]),
            "GetProcessId": (D, [H]),
            "GetProcessTimes": (B, [H] + [P(wintypes.FILETIME)] * 4),
            "QueryFullProcessImageNameW": (B, [H, D, wintypes.LPWSTR, P(D)]),
            "GetSystemTimePreciseAsFileTime": (None, [P(wintypes.FILETIME)]),
            "WaitForSingleObject": (D, [H, D]),
            "GetExitCodeProcess": (B, [H, P(D)]),
            "TerminateProcess": (B, [H, D]),
        }
        for name, (result, args) in signatures.items():
            getattr(self.k, name).restype = result
            getattr(self.k, name).argtypes = args

    @staticmethod
    def ticks(value):
        return (value.dwHighDateTime << 32) | value.dwLowDateTime

    def now(self):
        value = wintypes.FILETIME()
        self.k.GetSystemTimePreciseAsFileTime(ctypes.byref(value))
        return self.ticks(value)

    def duplicate(self, handle):
        result = wintypes.HANDLE()
        current = self.k.GetCurrentProcess()
        require(self.k.DuplicateHandle(current, int(handle), current, ctypes.byref(result),
                                       0, False, 2), "DuplicateHandle failed")
        return result.value

    def open(self, pid):
        handle = self.k.OpenProcess(0x1000 | 0x100000 | 0x0001, False, pid)
        require(bool(handle), "cannot open candidate process identity")
        return handle

    def kernel_key(self, handle):
        pid = self.k.GetProcessId(handle)
        require(pid, "GetProcessId failed")
        times = [wintypes.FILETIME() for _ in range(4)]
        require(self.k.GetProcessTimes(handle, *(ctypes.byref(t) for t in times)),
                "GetProcessTimes failed")
        return dict(pid=pid, creationTime100ns=self.ticks(times[0]))

    def identity(self, handle):
        identity = self.kernel_key(handle)
        image, size = ctypes.create_unicode_buffer(32768), wintypes.DWORD(32768)
        require(self.k.QueryFullProcessImageNameW(handle, 0, image, ctypes.byref(size)),
                "QueryFullProcessImageName failed during registration")
        return dict(identity, imagePath=image.value)

    def state(self, handle):
        wait = self.k.WaitForSingleObject(handle, 0)
        require(wait in (0, 258), "process wait failed")
        code = wintypes.DWORD()
        require(self.k.GetExitCodeProcess(handle, ctypes.byref(code)), "GetExitCodeProcess failed")
        return dict(alive=wait == 258, exitCode=code.value)

    def snapshot(self):
        class Entry(ctypes.Structure):
            _fields_ = [("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
                        ("th32ProcessID", wintypes.DWORD), ("th32DefaultHeapID", ctypes.c_size_t),
                        ("th32ModuleID", wintypes.DWORD), ("cntThreads", wintypes.DWORD),
                        ("th32ParentProcessID", wintypes.DWORD), ("pcPriClassBase", wintypes.LONG),
                        ("dwFlags", wintypes.DWORD), ("szExeFile", wintypes.WCHAR * 260)]
        for name in ("Process32FirstW", "Process32NextW"):
            getattr(self.k, name).restype = wintypes.BOOL
            getattr(self.k, name).argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
        # A process created after this bound cannot be the snapshotted object.
        started = self.now()
        snapshot = self.k.CreateToolhelp32Snapshot(2, 0)
        require(snapshot not in (None, wintypes.HANDLE(-1).value), "Toolhelp snapshot failed")
        entry, rows = Entry(), {}
        entry.dwSize = ctypes.sizeof(entry)
        try:
            ok = self.k.Process32FirstW(snapshot, ctypes.byref(entry))
            while ok:
                rows[entry.th32ProcessID] = dict(pid=entry.th32ProcessID,
                    parentPid=entry.th32ParentProcessID, executable=entry.szExeFile)
                ok = self.k.Process32NextW(snapshot, ctypes.byref(entry))
            require(ctypes.get_last_error() == 18, "incomplete Toolhelp snapshot")
        finally:
            self.close(snapshot)
        return started, rows

    def terminate(self, handle):
        require(self.k.TerminateProcess(handle, 1), "TerminateProcess failed")
        require(self.k.WaitForSingleObject(handle, 10000) == 0, "termination wait failed")

    def close(self, handle):
        self.k.CloseHandle(handle)


class Processes:
    def __init__(self, process, bridge, launcher, native, native_pid_file, api=None, expected_hashes=None):
        self.api = api if api is not None else WindowsProcesses()
        self.handles, self.rows = {}, {}
        self.native_pid_file = Path(native_pid_file)
        self.expected = {role: absolute(str(path)) for role, path in
                         (("bridge", bridge), ("launcher", launcher), ("native", native))}
        self.hashes = (dict(expected_hashes) if expected_hashes is not None else
                       {role: sha(path) for role, path in self.expected.items()})
        require(set(self.hashes) == set(self.expected), "explicit binary SHA bindings required")
        # Duplicate Popen's original handle; never reopen the bridge by PID.
        handle = self.api.duplicate(process._handle)
        try:
            identity = self.api.identity(handle)
            require(identity["pid"] == process.pid, "Popen handle PID mismatch")
            self.check_image("bridge", identity)
            self.handles["bridge"] = handle
            self.rows["bridge"] = dict(identity, role="bridge", parentPid=os.getpid(),
                                        imageSha256=self.hashes["bridge"], origin="directPopen")
        except BaseException:
            self.api.close(handle)
            raise

    def check_image(self, role, identity):
        require(Path(identity["imagePath"]).is_absolute() and
                Path(identity["imagePath"]).resolve() == self.expected[role],
                "unexpected " + role + " kernel image path")
        require(sha(self.expected[role]) == self.hashes[role], "registered image SHA changed")

    def exact(self, role):
        # An open handle never changes kernel objects. Image path/SHA were
        # verified during admission; do not query that transient image section
        # again during exit (it can disappear before the handle is signaled).
        # PID + creation time remain available through the retained object.
        identity = self.api.kernel_key(self.handles[role])
        require(all(identity[key] == self.rows[role][key]
                    for key in ("pid", "creationTime100ns")),
                "retained process identity changed: " + role)
        return self.api.state(self.handles[role])

    def register_child(self, role, parent, row, started):
        require(role not in self.handles, "role already registered")
        require(self.exact(parent)["alive"], "parent is no longer the retained live object")
        parent_row = self.rows[parent]
        require(row["parentPid"] == parent_row["pid"], "not a direct child")
        handle = self.api.open(row["pid"])
        try:
            identity = self.api.identity(handle)
            require(identity["pid"] == row["pid"], "candidate PID changed")
            self.check_image(role, identity)
            require(parent_row["creationTime100ns"] < identity["creationTime100ns"] < started,
                    "child creation time outside parent/snapshot bounds")
            require(self.api.state(handle)["alive"], "candidate already exited")
            _, after = self.api.snapshot()
            require(after.get(row["pid"]) == row, "snapshot/open race")
            require(self.api.identity(handle) == identity, "candidate identity changed during open")
            require(self.exact(parent)["alive"], "parent exited during candidate registration")
            self.handles[role] = handle
            self.rows[role] = dict(identity, role=role, parentPid=parent_row["pid"],
                parentCreationTime100ns=parent_row["creationTime100ns"],
                imageSha256=self.hashes[role], origin="verifiedDirectChild",
                snapshotStarted100ns=started)
        except BaseException:
            self.api.close(handle)
            raise

    def capture(self):
        # At most two explicit registrations. No historical roots, recursive
        # descendants, console heuristics, or filesystem/history discovery.
        if "launcher" not in self.handles:
            require(self.exact("bridge")["alive"], "cannot register under exited bridge")
            started, rows = self.api.snapshot()
            candidates = [row for row in rows.values()
                          if row["parentPid"] == self.rows["bridge"]["pid"] and
                          row["executable"].casefold() == self.expected["launcher"].name.casefold()]
            require(len(candidates) <= 1, "ambiguous launcher children")
            if candidates:
                self.register_child("launcher", "bridge", candidates[0], started)
        if "native" not in self.handles and self.native_pid_file.exists():
            require("launcher" in self.handles, "native PID receipt without verified launcher")
            with self.native_pid_file.open("rb") as stream:
                raw = stream.read(33)
            require(0 < len(raw) <= 32 and raw.isdigit(), "invalid native PID receipt")
            pid = int(raw)
            started, rows = self.api.snapshot()
            require(pid in rows, "receipt native not in snapshot")
            self.register_child("native", "launcher", rows[pid], started)

    def status(self):
        return [dict(self.rows[role], **self.exact(role)) for role in self.handles]

    def terminate_known(self):
        actions = []
        for role in ("native", "launcher", "bridge"):
            if role not in self.handles:
                continue
            try:
                if self.exact(role)["alive"]:
                    action = dict(self.rows[role], method="TerminateProcess(retainedHandle)")
                    actions.append(action)  # Even an unsuccessful attempt is failure.
                    self.api.terminate(self.handles[role])
                    action["after"] = self.exact(role)
            except BaseException as error:
                actions.append(dict(role=role, refusedOrFailed=repr(error)))
        return actions

    def close(self):
        for handle in self.handles.values():
            self.api.close(handle)
        self.handles.clear()


class Peer:
    def __init__(self, bridge, env, directory, timeout, native_pid_file, expected_hashes):
        self.directory, self.timeout = directory, timeout
        self.output = queue.Queue(maxsize=256)
        self.reader_errors = []
        self.reader_counts = {"stdout": 0, "stderr": 0}
        self.counts = {"requests": 0, "responses": 0, "notifications": 0}
        self.next_id = 0
        self.process = subprocess.Popen([str(bridge)], env=env, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            self.pids = Processes(self.process, bridge, env["RPI_BIN"],
                                  env["RPI_CODEMODE_NATIVE_BIN"], native_pid_file,
                                  expected_hashes=expected_hashes)
        except BaseException as error:
            # No identity proof means no termination authority. EOF only.
            self.process.stdin.close()
            save(directory / "cleanup.json", dict(bridgePid=self.process.pid,
                closedEOF=True, forcedKills=[], identityErrors=[repr(error)],
                ownershipScope="registration failed; no termination authority"))
            raise
        self.readers = [threading.Thread(target=self.read, args=(name, stream), daemon=True)
                        for name, stream in (("stdout", self.process.stdout), ("stderr", self.process.stderr))]
        for reader in self.readers:
            reader.start()
        self.sent = (directory / "acp-input.jsonl").open("wb")

    def read(self, name, stream):
        try:
            with (self.directory / ("acp-" + name + ".jsonl")).open("wb") as log:
                total = 0
                while True:
                    line = stream.readline(2 * 1024 * 1024 + 1)
                    if not line:
                        break
                    log.write(line)
                    log.flush()
                    total += len(line)
                    require(len(line) <= 2 * 1024 * 1024 and total <= 64 * 1024 * 1024,
                            "ACP wire capacity exceeded")
                    self.reader_counts[name] += 1
                    if name == "stdout":
                        self.output.put_nowait(json.loads(line))
        except BaseException as error:
            self.reader_errors.append(repr(error))

    def request(self, method, params):
        self.next_id += 1
        frame = dict(jsonrpc="2.0", id=self.next_id, method=method, params=params)
        wire = (json.dumps(frame, separators=(",", ":")) + "\n").encode()
        self.sent.write(wire)
        self.sent.flush()
        # No replay on any write timeout, reader error or uncertain native result.
        outcome = queue.Queue(maxsize=1)
        def write_once():
            try:
                self.process.stdin.write(wire)
                self.process.stdin.flush()
                outcome.put(None)
            except BaseException as error:
                outcome.put(error)
        writer = threading.Thread(target=write_once, daemon=True)
        writer.start()
        writer.join(timeout=self.timeout)
        require(not writer.is_alive(), "uncertain ACP write timed out; no retry")
        error = outcome.get_nowait()
        if error:
            raise error
        self.counts["requests"] += 1
        notifications = []
        deadline = time.monotonic() + self.timeout
        while True:
            require(not self.reader_errors, "ACP reader failed: " + repr(self.reader_errors))
            remaining = deadline - time.monotonic()
            require(remaining > 0, "ACP response timeout; no retry: " + method)
            try:
                message = self.output.get(timeout=min(.2, remaining))
            except queue.Empty:
                require(self.process.poll() is None, "bridge exited before response")
                continue
            require(message.get("jsonrpc") == "2.0", "bad ACP wire version")
            if "id" in message:
                require("method" not in message, "unexpected ACP client request (no authority provided)")
                require(message["id"] == self.next_id, "uncorrelated/duplicate ACP response")
                self.counts["responses"] += 1
                self.pids.capture()
                return message, notifications
            require("method" in message, "unrecognized ACP frame")
            self.counts["notifications"] += 1
            notifications.append(message)

    def success(self, method, params):
        response, notifications = self.request(method, params)
        require("result" in response and "error" not in response, repr(response))
        return response["result"], notifications

    def close(self):
        cleanup = {"bridgePid": self.process.pid, "forcedKills": [], "closedEOF": False,
                   "identityErrors": [], "ownershipScope": "explicit bridge/launcher/native only"}
        try:
            try:
                self.pids.capture()
                cleanup["beforeEOF"] = self.pids.status()
            except BaseException as error:
                cleanup["identityErrors"].append(repr(error))
            self.process.stdin.close()
            cleanup["closedEOF"] = True
            try:
                cleanup["bridgeExitCode"] = self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                pass
            deadline = time.monotonic() + 10
            try:
                while time.monotonic() < deadline and any(row["alive"] for row in self.pids.status()):
                    time.sleep(.1)
            except BaseException as error:
                cleanup["identityErrors"].append(repr(error))
            # Never PID/tree termination. Only retained, revalidated exact kernel
            # objects; any manual intervention makes this case FAILED.
            cleanup["forcedKills"] = self.pids.terminate_known()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                cleanup["identityErrors"].append("bridge did not exit")
            for reader in self.readers:
                reader.join(timeout=10)
            cleanup.update(after=self.pids.status(), bridgeExitCode=self.process.returncode,
                readersJoined=all(not reader.is_alive() for reader in self.readers),
                readerErrors=self.reader_errors, wireCounts=self.counts, wireLines=self.reader_counts)
        finally:
            self.sent.close()
            self.pids.close()
            save(self.directory / "cleanup.json", cleanup)
        require(not cleanup["identityErrors"], "identity audit incomplete")
        require(cleanup.get("bridgeExitCode") == 0 and not cleanup["forcedKills"], "unclean bridge shutdown")
        require(cleanup["readersJoined"] and not self.reader_errors, "readers did not cleanly finish")
        require({row["role"] for row in cleanup["after"]} == {"bridge", "launcher", "native"},
                "explicit process chain incomplete")
        require(not any(row["alive"] for row in cleanup["after"]), "known native/launcher process survived")
        return cleanup


def oracle(path):
    entries = records(path)
    header = entries[0]
    require(header["kind"] == "header" and header["version"] == 4, "not native v4")
    require(str(uuid.UUID(header["id"])) == header["id"], "noncanonical UUID")
    require(Path(header["cwd"]).is_dir(), "genuine header cwd must exist")
    by_id = {e["id"]: e for e in entries if e.get("kind") == "entry"}
    leaf = next(e["id"] for e in reversed(entries) if e.get("kind") == "entry" and e.get("lane") == "main")
    branch, cursor, seen = [], leaf, set()
    while cursor:
        require(cursor not in seen and cursor in by_id, "invalid source main ancestry")
        seen.add(cursor)
        entry = by_id[cursor]
        require(entry["lane"] == "main", "cross-lane source ancestry")
        branch.append(entry)
        cursor = entry.get("parentId")
    branch.reverse()
    begins = [e for e in branch if e.get("customType") == "rpi.tool_program.child_begin.v1"]
    phases, chunks = {}, {}
    for entry in branch:
        if entry.get("customType") == "rpi.tool_program.child_phase.v1":
            phase = dict(entry["data"], entryId=entry["id"], seq=entry["seq"])
            for key in ("descriptor", "isError", "cancelled", "failed", "interrupted"):
                phase.setdefault(key, None)
            phases.setdefault(phase["childId"], []).append(phase)
        if entry.get("customType") == "rpi.tool_program.body_chunk.v1":
            data = entry["data"]
            chunks[data["bodyId"], data["index"]] = data
    return header, leaf, begins, phases, chunks


def observe(child_id, phases):
    final = next((p for p in phases if p["phase"] == "final"), None)
    closed = next((p for p in phases if p["phase"] == "closed"), None)
    outcome = "finalObserved" if final else "unknown"
    if final and closed and closed["index"] == final["index"] + 1 and closed["seq"] > final["seq"]:
        outcome = ("interrupted" if closed.get("interrupted") else
                   "failed" if closed.get("failed") else
                   "cancelled" if final.get("cancelled") else
                   "failed" if final.get("isError") else "succeeded")
    return dict(childId=child_id, latestPhase=max(phases, key=lambda p: p["seq"])["phase"] if phases else None,
                finalPhase=final, closedPhase=closed, outcome=outcome)


def query(peer, method, sid, value):
    result, _ = peer.success("session/tool_program_" + method, dict(sessionId=sid, query=value))
    return result


def audit_native(case):
    locations = list((case / "sessions").rglob("native-input.jsonl"))
    require(len(locations) == 1, "exactly one actual native launcher required")
    directory = locations[0].parent
    sent, received = records(locations[0]), records(directory / "native-output.jsonl")
    bad_commands = [e for e in sent if e.get("type") not in READ_COMMANDS]
    executions = [e for e in received if e.get("type") in EXECUTION]
    require(not bad_commands and not executions, "native execution or non-read-only command observed")
    require(sent and any(e.get("type") == "ready" for e in received), "missing actual native traffic")
    cleanup = json.loads((case / "cleanup.json").read_text())
    pid = int((directory / "native-pid.txt").read_text())
    # ACP Session::kill calls the existing owner terminate path on EOF. Native
    # may exit zero on pipe closure or be retired with the job's exit code 1;
    # neither a missing wrapper receipt nor forced owner retirement is a claim
    # of graceful native stop. Retained handles disambiguate PID reuse/crashes.
    native_status = next((row for row in cleanup["after"] if row["role"] == "native" and row["pid"] == pid), None)
    before_eof = next((row for row in cleanup["beforeEOF"] if row["role"] == "native" and row["pid"] == pid), None)
    require(before_eof is not None and before_eof["alive"], "native exited before owned EOF cleanup")
    require(native_status is not None and not native_status["alive"] and native_status["exitCode"] in (0, 1),
            "native PID missing, survived, or exited with an unexpected code")
    require(cleanup["bridgeExitCode"] == 0 and not cleanup["forcedKills"],
            "native retirement must belong to the normal bridge EOF cleanup")
    receipt = directory / "native-exit.json"
    native_exit, receipt_state = None, "missing"
    if receipt.exists():
        raw_receipt = receipt.read_bytes()
        require(len(raw_receipt) <= 1024, "oversized launcher exit receipt")
        try:
            native_exit = json.loads(raw_receipt)
            receipt_state = "complete"
        except (json.JSONDecodeError, UnicodeDecodeError):
            # Job retirement may also interrupt the optional receipt write.
            # Preserve the file and mark it incomplete; the process handle,
            # not this wrapper artifact, is the authoritative exit evidence.
            receipt_state = "incomplete"
    require(native_exit is None or native_exit["code"] == native_status["exitCode"],
            "launcher receipt disagrees with retained native process handle")
    counts = {}
    for frame in sent:
        counts[frame["type"]] = counts.get(frame["type"], 0) + 1
    result = dict(nativePid=pid, nativeCommands=len(sent), nativeFrames=len(received),
                  commandCounts=counts, executionFrames=0, nonReadOnlyCommands=0,
                  nativeExit=native_exit, nativeProcessExit=native_status,
                  launcherReceiptObserved=native_exit is not None,
                  launcherReceiptState=receipt_state,
                  retirementMode="pipeExitZero" if native_status["exitCode"] == 0 else "ownerTerminateAfterEOF",
                  gracefulNativeShutdownQualified=False)
    save(case / "native-audit.json", result)
    return result


def run_case(args, original, index):
    case = args.out / ("session-" + str(index))
    case.mkdir()
    header, leaf, begins, phases, chunks = oracle(original)
    sid = header["id"]
    copied_dir = case / "sessions" / sid / "copied"
    copied_dir.mkdir(parents=True)
    copied = copied_dir / original.name
    source_files = [original]
    progress = Path(str(original) + ".progress.jsonl")
    require(progress.is_file(), "source progress sidecar required")
    source_files.append(progress)
    hashes = []
    for source in source_files:
        destination = copied_dir / source.name
        before = sha(source)
        shutil.copyfile(source, destination)
        require(sha(destination) == before, "copy checksum mismatch")
        hashes.append(dict(source=str(source), copied=str(destination), beforeSha256=before))
    save(case / "transcript-hashes.json", hashes)
    agent = case / "agent"
    (agent / "extensions").mkdir(parents=True)
    # Explicit synthetic identity, unusable local endpoint, no ambient models/auth.
    save(agent / "models.json", {"providers": {"resource-fixture": {
        "api": "openai-completions", "baseUrl": "http://127.0.0.1:1/v1", "apiKey": "no-provider-calls",
        "models": [{"id": "synthetic", "name": "synthetic", "reasoning": False,
                    "input": ["text"], "contextWindow": 8192, "maxTokens": 1024}]}}})
    save(agent / "settings.json", dict(defaultProvider="resource-fixture", defaultModel="synthetic",
                                       defaultThinkingLevel="off"))
    save(agent / "auth.json", {})
    env = {key: value for key, value in os.environ.items()
           if not key.upper().startswith(("RPI_", "PI_", "ANTHROPIC_", "OPENAI_", "AZURE_", "GOOGLE_", "GEMINI_", "AWS_"))
           and not key.upper().endswith(("_API_KEY", "_AUTH_TOKEN"))}
    env.update(RPI_BIN=str(args.launcher), RPI_CODEMODE_NATIVE_BIN=str(args.native),
               RPI_ACP_SESSION_DIR=str(case / "sessions"), RPI_ACP_NO_AUTO_REGISTER_ZED="1",
               RPI_ACP_NO_PERMISSION_GATE="1", RPI_CODING_AGENT_DIR=str(agent),
               RPI_EXTENSIONS_DIR=str(agent / "extensions"), RPI_OFFLINE="1",
               RPI_PROVIDER="resource-fixture", RPI_MODEL="synthetic")
    peer = Peer(args.bridge, env, case, args.timeout,
                case / "sessions" / "native-pid.txt",
                {role: getattr(args, role + "_sha256").lower()
                 for role in ("bridge", "launcher", "native")})
    result = dict(sessionId=sid, source=str(original), allChildren=0, selectedChildren=0,
                  verifiedBodyChunks=0, childContinuations=0, phaseContinuations=0,
                  bodyScope="first page, at most one chunk per selected child; no full-body aggregate claim")
    try:
        init, _ = peer.success("initialize", dict(protocolVersion=1, clientCapabilities={}))
        capability = init["agentCapabilities"]["_meta"]["rpi/tool-program-children"]
        require(capability == dict(version=1, optional=True, recovery="readOnly", methods=METHODS), "read-only methods capability mismatch")
        resumed, notifications = peer.success("session/resume", dict(sessionId=sid, cwd=header["cwd"], mcpServers=[]))
        require("configOptions" in resumed, "resume was not successful configuration handshake")
        require(not any(n.get("params", {}).get("update", {}).get("sessionUpdate") in
                        {"user_message_chunk", "agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update"}
                        for n in notifications), "resume unexpectedly replayed history")
        peer.pids.capture()
        require(set(peer.pids.rows) == {"bridge", "launcher", "native"}, "actual bridge/launcher/native process chain missing")
        snap = query(peer, "snapshot", sid, dict(children=dict(direction="newestFirst", limit=8, maxBytes=32768), phaseLimit=8))
        require(snap["sessionId"] == sid and snap["lane"] == "main" and not snap["resyncRequired"] and not snap["events"], "snapshot binding/events mismatch")
        require(snap["page"]["leafId"] == leaf, "snapshot source leaf mismatch")
        expected_ids = [e["id"] for e in reversed(begins)][:8]
        require([e["childId"] for e in snap["page"]["children"]] == expected_ids, "latest selected IDs mismatch")
        require(snap["observations"] == [observe(cid, phases.get(cid, [])) for cid in expected_ids], "snapshot final/closed/outcome mismatch")
        require(len(json.dumps(snap, separators=(",", ":")).encode()) <= 131072, "snapshot exceeded bound")
        result["selectedChildren"] = len(expected_ids)
        result["snapshotObservations"] = snap["observations"]
        cursor, ids = None, []
        for _ in range(256):
            page = query(peer, "children", sid, dict(leafId=leaf, cursor=cursor, direction="newestFirst", limit=1, maxBytes=32768))
            require(page["leafId"] == leaf and len(page["children"]) <= 1, "summary page bound/binding")
            require(len(json.dumps(page, separators=(",", ":")).encode()) <= 32768, "summary byte bound")
            for child in page["children"]:
                begin = next(e for e in begins if e["id"] == child["childId"])
                expected = dict(begin["data"], seq=begin["seq"], state="unknown", originVerification="nativeCheckpoint")
                require(child == expected, "summary differs from actual source begin")
                ids.append(child["childId"])
            cursor = page["nextCursor"]
            if cursor is None:
                require(page["scanExhausted"], "unexhausted summary scan without cursor")
                break
            result["childContinuations"] += 1
        require(cursor is None and ids == [e["id"] for e in reversed(begins)], "all begin paging mismatch")
        result["allChildren"] = len(ids)
        for cid in expected_ids:
            cursor, actual = None, []
            for _ in range(256):
                page = query(peer, "phases", sid, dict(childId=cid, leafId=leaf, cursor=cursor, direction="newestFirst", limit=1, maxBytes=32768))
                require(page["leafId"] == leaf and page["childId"] == cid and len(page["phases"]) <= 1 and page["scanned"] <= 128, "phase bound/binding mismatch")
                require(len(json.dumps(page, separators=(",", ":")).encode()) <= 32768, "phase byte bound")
                actual.extend(page["phases"])
                cursor = page["nextCursor"]
                if cursor is None:
                    require(page["scanExhausted"], "unexhausted phase scan without cursor")
                    break
                result["phaseContinuations"] += 1
            require(cursor is None and actual == list(reversed(phases.get(cid, []))), "exact phase metadata mismatch")
            phase = next((p for p in actual if p.get("descriptor")), None)
            if phase:
                descriptor = phase["descriptor"]
                body = query(peer, "body", sid, dict(childId=cid, phaseEntryId=phase["entryId"], descriptorId=descriptor["descriptorId"], leafId=leaf, limit=1, maxBytes=65536))
                require(body["childId"] == cid and body["phaseEntryId"] == phase["entryId"] and body["body"]["descriptor"] == descriptor, "body source descriptor mismatch")
                require(body["body"]["leafId"] == leaf and len(body["body"]["chunks"]) == 1 and body["body"]["byteLength"] <= 65536, "body first chunk bound mismatch")
                for chunk in body["body"]["chunks"]:
                    source = chunks[descriptor["bodyId"], chunk["index"]]
                    require(chunk["index"] == 0 and source["data"] == chunk["data"] and source["sha256"] == chunk["sha256"], "native source body chunk mismatch")
                    decoded = base64.b64decode(chunk["data"], validate=True)
                    require(len(decoded) == chunk["byteLength"] and hashlib.sha256(decoded).hexdigest() == chunk["sha256"], "independent body checksum mismatch")
                    result["verifiedBodyChunks"] += 1
        # Strict malformed DTOs and numeric bounds; never bypass native validators.
        invalid = [dict(sessionId=sid, query={}), dict(sessionId=sid, query=dict(direction="newestFirst", limit=1, maxBytes=32768, unknown=True)),
                   dict(sessionId=sid, query=dict(direction="newestFirst", limit=33, maxBytes=32768)),
                   dict(sessionId=sid, query=dict(direction="newestFirst", limit=1, maxBytes=32768), _meta=None)]
        for params in invalid:
            response, _ = peer.request(METHODS[0], params)
            require(response.get("error", {}).get("code") == -32602, "malformed/client-bound error mismatch")
        response, _ = peer.request(METHODS[3], dict(sessionId=sid, query=dict(children=dict(direction="newestFirst", limit=9, maxBytes=32768), phaseLimit=8)))
        require(response.get("error", {}).get("code") == -32602, "snapshot limit error mismatch")
        response, _ = peer.request(METHODS[0], dict(sessionId=sid, query=dict(leafId=str(uuid.uuid4()), direction="newestFirst", limit=1, maxBytes=32768)))
        require(response.get("error") == dict(code=-32001, message="native child query rejected", data=dict(code="host_unavailable")), "actual frozen-native absent leaf must be sanitized -32001, not guessed conflict: " + repr(response))
        result["strictErrorsChecked"] = len(invalid) + 2
        result["acpWireCounts"] = dict(peer.counts)
    except BaseException as error:
        result.update(status="FAILED", error=repr(error), traceback=traceback.format_exc())
        raise
    finally:
        try:
            peer.close()
        finally:
            for row in hashes:
                row.update(sourceAfterSha256=sha(row["source"]), copiedAfterSha256=sha(row["copied"]))
            save(case / "transcript-hashes.json", hashes)
            result["transcriptsPreserved"] = all(row["beforeSha256"] == row["sourceAfterSha256"] == row["copiedAfterSha256"] for row in hashes)
            result["acpWireCounts"] = dict(peer.counts)
            try:
                result.update(audit_native(case))
            finally:
                save(case / "result.json", result)
            require(result["transcriptsPreserved"], "source/copied primary/progress changed")
    result["status"] = "PASSED"
    save(case / "result.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("bridge", "native", "launcher", "source-root", "source-evidence", "build-evidence", "out"):
        parser.add_argument("--" + name, required=True)
    for name in ("bridge", "native", "launcher", "source-evidence", "build-evidence"):
        parser.add_argument("--" + name + "-sha256", required=True)
    parser.add_argument("--timeout", type=float, default=35)
    args = parser.parse_args()
    for name in ("bridge", "native", "launcher", "source_root", "source_evidence", "build_evidence", "out"):
        setattr(args, name, absolute(getattr(args, name)))
    args.out.mkdir(parents=True, exist_ok=False)  # Never retry/overwrite an earlier proof.
    state = dict(status="RUNNING", scope=SCOPE, results=[], provenance={})
    try:
        require(os.name == "nt", "this acceptance is Windows-only")
        require(1 <= args.timeout <= 60, "timeout must be 1..60 seconds")
        for name in ("bridge", "native", "launcher", "source_evidence", "build_evidence"):
            path = getattr(args, name)
            expected = getattr(args, name + "_sha256").lower()
            require(len(expected) == 64 and sha(path) == expected, "trusted SHA mismatch: " + name)
            state["provenance"][name] = dict(path=str(path), sha256=expected)
        evidence = json.loads(args.source_evidence.read_text(encoding="utf-8"))
        require(evidence["files"] and Path(evidence["cwd"]).resolve() == Path.cwd().resolve(), "source evidence workspace mismatch")
        require(all(sha(Path(evidence["cwd"]) / p) == digest for p, digest in evidence["files"].items()), "prebuild source inventory changed")
        build = json.loads(args.build_evidence.read_text(encoding="utf-8"))
        require(build["exitCode"] == 0 and build["sourceInputsUnchanged"] and Path(build["binaryPath"]).resolve() == args.bridge and build["binarySha256"] == args.bridge_sha256, "build provenance mismatch")
        require(Path(build["sourceEvidence"]).resolve() == args.source_evidence and build["sourceEvidenceSha256"] == args.source_evidence_sha256, "prebuild evidence binding mismatch")
        state["scriptSha256"] = sha(__file__)
        save(args.out / "provenance.json", state["provenance"])
        originals = sorted(p for case in args.source_root.iterdir() if (case / "sessions").is_dir()
                           for p in (case / "sessions").rglob("*.jsonl") if not p.name.endswith(".progress.jsonl"))
        require(len(originals) == 26 and not any(p.is_symlink() for p in originals), "exactly 26 retained native primaries required")
        for index, original in enumerate(originals):
            state["currentCase"] = index
            state["results"].append(run_case(args, original, index))
            save(args.out / "results.json", state)
            print("PASS session", index, original.parent.parent.parent.name, flush=True)
        for name in ("bridge", "native", "launcher"):
            require(sha(getattr(args, name)) == getattr(args, name + "_sha256"), "binary changed during proof")
        for key in ("allChildren", "selectedChildren", "verifiedBodyChunks", "childContinuations", "phaseContinuations", "nativeCommands", "nativeFrames", "executionFrames", "nonReadOnlyCommands", "strictErrorsChecked"):
            state[key] = sum(row[key] for row in state["results"])
        require(state["allChildren"] == 70 and state["selectedChildren"] == 38 and state["verifiedBodyChunks"] > 0, "retained source coverage mismatch")
        state.update(status="PASSED", sessions=len(state["results"]), providerCallsObservedViaNativeExecution=0)
    except BaseException as error:
        state.update(status="FAILED", error=repr(error), traceback=traceback.format_exc())
        save(args.out / "results.json", state)
        print(state["traceback"], flush=True)
        return 1
    save(args.out / "results.json", state)
    print(json.dumps({key: value for key, value in state.items() if key != "results"}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
