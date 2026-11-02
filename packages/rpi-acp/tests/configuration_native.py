"""Bounded Windows real-native configuration proof; never builds or installs.

Parent first uses --write-source-manifest <NEW stage8 target file>, then builds
bridge AND --example configuration_fixture from that frozen inventory. Execution
requires all explicit provenance arguments below. Fixture CI is not native proof.
Fixture marker schema: {dll, sha256, head, sourceManifest:{path,sha256},
sourceSha256, build:{exitCode,cwd,startedAtNs,finishedAtNs,args}}. Its args must
include --locked, --offline, -p rpi-acp, --example configuration_fixture.
Bridge/native marker schemas are the existing session_list_native.py schemas.
All output is restricted to the stage8 Cargo target evidence directory.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, HTTPServer

sys.dont_write_bytecode = True
import mcp_stdio_native as h
import session_list_native as s

TARGET = Path("C:/Users/bigfish/.cargo/target/acp-stage8-recovery-01")
s.TARGET = TARGET
s.PYTHON_INPUTS = set(s.PYTHON_INPUTS) | {"packages/rpi-acp/tests/configuration_native.py"}
require = s.require
TOOL = "configuration_native_fixture"


def options(value):
    array = value.get("configOptions")
    require(isinstance(array, list) and 1 <= len(array) <= 2, "not a full configuration array")
    result = {o["id"]: o for o in array}
    require(len(result) == len(array) and "model" in result, "missing/duplicate model option")
    for item in array:
        require(item.get("type") == "select" and isinstance(item.get("options"), list),
                "invalid select option")
        choices = [o["value"] for o in h.objects(item["options"]) if "value" in o]
        require(item.get("currentValue") in choices, "current value outside choices")
    return result


def config_frame(frame, sid):
    params = frame.get("params", {})
    update = params.get("update", {})
    if frame.get("method") == "session/update" and update.get("sessionUpdate") == "config_option_update":
        require(params.get("sessionId") == sid, "configuration crossed sessions")
        options(update)
        return update
    return None


class Model:
    """Two-call tool turn; second reply is held until ACP update is observed."""
    def __init__(self, root):
        self.calls, self.errors = [], []
        self.entered, self.release = threading.Event(), threading.Event()
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(10)
            def log_message(self, *_):
                pass
            def do_POST(self):
                try:
                    require(self.path == "/v1/chat/completions", "unexpected provider route")
                    size = int(self.headers.get("Content-Length", "0"))
                    require(0 < size < 1024 * 1024 and len(owner.calls) < 8, "provider input bound")
                    body = json.loads(self.rfile.read(size))
                    owner.calls.append(body)
                    h.dump(root / "provider-requests.json", owner.calls)
                    user = max(i for i, m in enumerate(body["messages"]) if m["role"] == "user")
                    tools = [m for m in body["messages"][user + 1:] if m["role"] == "tool"]
                    if tools:
                        require("NATIVE_CONFIGURATION_FIXTURE:high" in h.as_text(tools[-1].get("content")),
                                "native fixture action/readback did not succeed")
                        owner.entered.set()
                        require(owner.release.wait(25), "bounded settlement gate expired")
                        delta, reason = dict(role="assistant", content="CONFIGURATION_NATIVE_OK"), "stop"
                    else:
                        found = [t["function"] for t in body.get("tools", [])
                                 if t.get("function", {}).get("name") == TOOL]
                        require(len(found) == 1, "explicit test DLL tool was not loaded")
                        require(found[0]["parameters"]["required"] == ["level"], "fixture schema mismatch")
                        delta = dict(role="assistant", tool_calls=[dict(index=0, id="configuration-proof-call",
                            type="function", function=dict(name=TOOL, arguments='{"level":"high"}'))])
                        reason = "tool_calls"
                    events = [dict(choices=[dict(index=0, delta=delta, finish_reason=None)]),
                              dict(choices=[dict(index=0, delta={}, finish_reason=reason)],
                                   usage=dict(prompt_tokens=20, completion_tokens=5, total_tokens=25))]
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
                    self.close_connection = True
        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.05), daemon=True)
        self.thread.start()

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=12)
        require(not self.thread.is_alive() and not self.errors, "provider cleanup/errors: " + str(self.errors))


def fixture_provenance(args):
    require(args.fixture_dll.is_absolute() and args.fixture_dll.is_file(), "explicit fixture DLL required")
    s.target_path(args.fixture_dll)
    marker = json.loads(args.fixture_build_marker.read_text(encoding="utf-8"))
    manifest = json.loads(args.source_manifest.read_text(encoding="utf-8"))
    binding, build = marker["sourceManifest"], marker["build"]
    require(Path(marker["dll"]).resolve() == args.fixture_dll.resolve() and
            marker["sha256"] == args.fixture_sha == s.sha(args.fixture_dll), "fixture DLL hash/path mismatch")
    require(Path(binding["path"]).resolve() == args.source_manifest.resolve() and
            binding["sha256"] == s.sha(args.source_manifest), "fixture prebuild manifest binding mismatch")
    require(marker["sourceSha256"] == manifest["sourceSha256"] == s.inventory(s.REPO, s.PYTHON_INPUTS)
            and marker["head"] == manifest["head"] == s.head(s.REPO), "fixture source drift")
    require(build["exitCode"] == 0 and Path(build["cwd"]).resolve() == s.REPO, "fixture build failed/wrong checkout")
    require(manifest["createdAtNs"] <= args.source_manifest.stat().st_mtime_ns <=
            build["startedAtNs"] <= build["finishedAtNs"] <= args.fixture_build_marker.stat().st_mtime_ns,
            "fixture manifest must precede build (no retrospective inventory)")
    cmd = build["args"]
    require(cmd[:2] == ["cargo", "build"] and "--offline" in cmd and "--locked" in cmd and
            cmd[cmd.index("-p") + 1] == "rpi-acp" and
            cmd[cmd.index("--example") + 1] == "configuration_fixture", "fixture build command mismatch")
    require("packages/rpi-acp/examples/configuration_fixture.rs" in marker["sourceSha256"],
            "new example missing from full Rust inventory")
    return marker


def wait_configuration(peer, sid, desired, timeout=15):
    deadline, seen = time.monotonic() + timeout, []
    while time.monotonic() < deadline:
        frame = peer.output.get(timeout=max(.01, deadline - time.monotonic()))
        if isinstance(frame, BaseException):
            raise frame
        require("id" not in frame, "prompt settled before independent configuration was observed")
        seen.append(frame)
        update = config_frame(frame, sid)
        if update and options(update).get("thinking", {}).get("currentValue") == desired:
            return update, seen
    raise AssertionError("independent native configuration update deadline")


def canary_audit(root, canary):
    checked = 0
    for path in root.rglob("*"):
        if not path.is_file() or path == root / "agent/auth.json":
            continue
        require(path.stat().st_size <= 32 * 1024 * 1024, "evidence file bound exceeded")
        require(canary.encode() not in path.read_bytes(), "unused fake auth canary escaped: " + str(path))
        checked += 1
    h.dump(root / "secret-audit.json", dict(files=checked, passed=True,
        exclusion="agent/auth.json: intentional unused fake credential ingress"))


def run(args):
    require(not __debug__ is False, "do not run inherited smoke helpers with python -O")
    root = s.target_path(args.diagnostics_dir)
    require(not root.exists(), "use NEW diagnostics directory")
    root.mkdir(parents=True)
    state = dict(status="running", passed=[], qualifiers=[
        "Authored real-native harness; only completed execution is proof, not fixture CI",
        "Polling coalesces states; not an event audit or atomic/ABA guarantee",
        "No general backpressure, poll-count, cancellation or multi-session matrix here"])
    peer = model = jobs = None
    canary = "UNUSED_CONFIGURATION_AUTH_CANARY_" + os.urandom(12).hex()
    def landed(text):
        state["passed"].append(text)
        h.dump(root / "results.json", state)
    def close():
        nonlocal peer
        if peer:
            old, peer = peer, None
            try:
                old.close()
            finally:
                jobs.dead()
    try:
        native_repo, bridge_sources, native_sources = s.provenance(args, root)
        marker = fixture_provenance(args)
        h.dump(root / "fixture-provenance.json", marker)
        win = h.Windows()
        jobs = s.Jobs(win, root)
        model = Model(root)
        env = h.environment(root, model, args.rpi)
        # Only this explicit test DLL is copied into a fresh isolated extension directory.
        # No installed/native package files are changed.
        directory = root / "fixture-extension"
        directory.mkdir()
        dll = directory / "configuration_fixture.dll"
        dll.write_bytes(args.fixture_dll.read_bytes())
        env["RPI_CONFIGURATION_NATIVE_FIXTURE"] = "1"
        h.dump(root / "agent/settings.json", dict(defaultProvider="fixture", defaultModel="synthetic",
            defaultThinkingLevel="off", extensionDirs=[str(directory)]))
        h.dump(root / "agent/auth.json", {"never-used-fixture-provider":dict(type="api_key", key=canary)})
        models = json.loads((root / "agent/models.json").read_text(encoding="utf-8"))
        reasoning = models["providers"]["fixture"]["models"][0]
        reasoning["reasoning"] = True
        plain = dict(reasoning, id="plain", name="plain", reasoning=False,
                     thinkingLevelMap={"minimal": None, "low": None, "medium": None, "high": None})
        models["providers"]["fixture"]["models"].append(plain)
        h.dump(root / "agent/models.json", models)
        workspace = root / "workspace"
        workspace.mkdir()
        def start(name):
            nonlocal peer
            require(s.inventory(s.REPO, s.PYTHON_INPUTS) == bridge_sources and
                    s.inventory(native_repo) == native_sources and s.sha(args.bridge) == args.bridge_sha and
                    s.sha(args.rpi) == args.native_sha and s.sha(dll) == args.fixture_sha and
                    s.sha(args.fixture_dll) == args.fixture_sha, "preexecute source/binary drift")
            peer = s.Peer(args.bridge, env, root, name, win)
            peer.call("initialize", protocolVersion=1, clientCapabilities={})
            jobs.cold(peer, args.bridge)
        start("configuration")
        initial, before = peer.call("session/new", cwd=str(workspace), mcpServers=[])
        sid = initial["sessionId"]
        baseline = options(initial)
        require(baseline["model"]["currentValue"] == "fixture/synthetic" and
                baseline["thinking"]["currentValue"] == "off", "unexpected native initial configuration")
        require(not before and not model.calls, "initialization emitted early updates/provider calls")
        jobs.capture(peer)
        landed("1: real native initial full configuration, no early update/model call")
        def set_option(key, value):
            reply, frames = peer.call("session/set_config_option", sessionId=sid, configId=key, value=value)
            result = options(reply)
            require(result[key]["currentValue"] == value, "actual ACP setter mismatch")
            for frame in frames:
                config_frame(frame, sid)
            return result
        changed = set_option("thinking", "low")
        require(changed["model"] == baseline["model"], "thinking setter changed model")
        reduced = set_option("model", "fixture/plain")
        require("thinking" not in reduced or reduced["thinking"]["options"] != baseline["thinking"]["options"],
                "model-dependent choices did not change")
        restored = set_option("model", "fixture/synthetic")
        require(restored["thinking"]["options"] == baseline["thinking"]["options"], "dependent choices not restored")
        set_option("thinking", "off")
        landed("2: actual ACP thinking/model switches and dependent choices")
        request = peer.send("session/prompt", dict(sessionId=sid,
            prompt=[dict(type="text", text="CONFIGURATION_NATIVE_PROOF")]))
        update, frames = wait_configuration(peer, sid, "high")
        require(options(update)["model"] == baseline["model"], "independent update omitted/changed model")
        h.wait_until(model.entered.is_set, "native tool result did not reach provider", 10)
        require(not model.release.is_set(), "settlement gate released early")
        model.release.set()
        settled, after = peer.response(request)
        require(settled.get("stopReason") == "end_turn" and len(model.calls) == 2, "native tool turn failed")
        require("CONFIGURATION_NATIVE_OK" in json.dumps(after), "missing final native text")
        for frame in after:
            value = config_frame(frame, sid)
            if value:
                require(options(value)["thinking"]["currentValue"] == "high", "postsettlement stale update")
        # A real setter refresh after settlement confirms observer and native state agree.
        final = set_option("thinking", "high")
        require(final == options(update), "pre/post settlement configuration differs")
        landed("3: SDK action8 tool + native readback, full update BEFORE settlement, same state AFTER")
        peer.call("session/close", sessionId=sid)
        close()
        start("configuration-reopen")
        loaded, replay = peer.call("session/load", sessionId=sid, cwd=str(workspace), mcpServers=[])
        reopened = options(loaded)
        require(reopened["model"]["currentValue"] == "fixture/synthetic" and
                reopened["thinking"]["currentValue"] == "off", "reopen did not use fresh native startup configuration")
        require(len(model.calls) == 2, "load contacted provider")
        jobs.capture(peer)
        again = set_option("thinking", "low")
        require(again["thinking"]["currentValue"] == "low", "reopened setter failed")
        peer.call("session/close", sessionId=sid)
        close()
        landed("4: close/reopen reads fresh native startup config (no bridge persistence), no replay side effects, owned Job processes dead")
        state["status"] = "passed"
    except BaseException:
        state.update(status="failed", failure=traceback.format_exc())
        raise
    finally:
        errors = []
        if model:
            model.release.set()
        for name, action in [("peer/jobs", close), ("model", model.close if model else lambda: None)]:
            try:
                action()
            except BaseException:
                errors.append(dict(resource=name, error=traceback.format_exc()))
        try:
            canary_audit(root, canary)
        except BaseException:
            errors.append(dict(resource="canary audit", error="evidence canary/bound audit failed"))
        state["cleanup_errors"] = errors
        if errors:
            state["status"] = "failed"
        # Never serialize the ingress canary, including in exception text.
        encoded = json.dumps(state, ensure_ascii=False).replace(canary, "[REDACTED_CANARY]")
        (root / "results.json").write_text(encoded, encoding="utf-8")
        require(not errors, "cleanup/audit failed; inspect results.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write-source-manifest", type=Path)
    for flag in ("bridge", "rpi", "diagnostics-dir", "build-marker", "source-manifest",
                 "native-build-record", "native-verification", "fixture-dll", "fixture-build-marker"):
        parser.add_argument("--" + flag, type=Path)
    for flag in ("bridge-sha", "native-sha", "fixture-sha"):
        parser.add_argument("--" + flag)
    args = parser.parse_args()
    if args.write_source_manifest:
        s.write_manifest(args.write_source_manifest)
        return
    for name, value in vars(args).items():
        if name != "write_source_manifest":
            require(value is not None, "required explicit argument: --" + name.replace("_", "-"))
            if isinstance(value, Path):
                require(value.is_absolute(), "absolute path required: " + name)
    run(args)


if __name__ == "__main__":
    main()
