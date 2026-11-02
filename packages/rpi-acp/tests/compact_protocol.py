"""Deterministic native-RPC process fixture for ACP /compact (no model).
Run with Python -B compact_protocol.py --bridge <bridge> --diagnostics-dir <stage2/logs>.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import shlex
import sys
import threading
import time

from bounded_history_native import Peer, success
from restart import fixture_environment


def controls(updates, session=None):
    """Classify ONLY validated standard command announcements; retain all else."""
    other, announcements = [], []
    for item in updates:
        update = item.get("params", {}).get("update", {})
        if update.get("sessionUpdate") != "available_commands_update":
            other.append(item)
            continue
        assert item.get("method") == "session/update", item
        assert session is None or item["params"]["sessionId"] == session, item
        commands = update["availableCommands"]
        assert len(commands) == 1 and commands[0]["name"] == "compact", item
        assert isinstance(commands[0].get("description"), str) and commands[0]["description"], item
        assert commands[0].get("input", {}).get("hint"), item
        assert "_meta" not in item and "_meta" not in update, item
        announcements.append(item)
    return other, announcements


def environment(root):
    # Positive allowlist: never inherit provider/auth/config environment variables.
    env = {k: v for k, v in os.environ.items() if k.upper() in {
        "SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "TEMP", "TMP",
        "SYSTEMDRIVE", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}}
    home = root / "home"
    home.mkdir(parents=True, exist_ok=True)
    env.update(HOME=str(home), USERPROFILE=str(home), APPDATA=str(home / "AppData"),
               LOCALAPPDATA=str(home / "LocalAppData"))
    env.update(fixture_environment(root))
    (root / "agent" / "auth.json").write_text("{}", encoding="utf-8")
    env.update(RPI_ACP_SESSION_DIR=str(root / "sessions"),
               RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_ACP_NO_PERMISSION_GATE="1")
    return env


def send(peer, method, params, request_id=None):
    message = dict(jsonrpc="2.0", method=method, params=params)
    if request_id is not None:
        message["id"] = request_id
    peer.record("in", message)
    peer.process.stdin.write(json.dumps(message) + "\n")
    peer.process.stdin.flush()


def response(peer, request_id, timeout=15):
    updates = []
    deadline = time.monotonic() + timeout
    while True:
        item = peer.receive(deadline)
        if item.get("id") == request_id:
            return item, updates
        updates.append(item)


def text(updates):
    return "".join(u.get("params", {}).get("update", {}).get("content", {}).get("text", "")
                   for u in updates if u.get("params", {}).get("update", {}).get("sessionUpdate") == "agent_message_chunk")


def wait_file(path):
    deadline = time.monotonic() + 10
    while not path.exists():
        assert time.monotonic() < deadline, ("fixture did not accept command", path)
        time.sleep(.02)


def backend():
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    root = Path(os.environ["COMPACT_FIXTURE_ROOT"])
    enabled = os.environ["COMPACT_FIXTURE_CAP"] == "1"
    sid = sys.argv[sys.argv.index("--session-id") + 1]
    directory = Path(sys.argv[sys.argv.index("--session-dir") + 1]) / "fixture"
    directory.mkdir(parents=True, exist_ok=True)
    (directory / f"fixture_{sid}.jsonl").write_text(json.dumps(dict(kind="header", version=4, id=sid)) + "\n")
    lock, stop, workers = threading.Lock(), threading.Event(), []
    active = None

    def emit(value):
        with lock:
            print(json.dumps(value), flush=True)

    def settled(outcome, summary=None):
        emit(dict(type="agent_settled", runId="compact-fixture", outcome=outcome,
                  summary=summary, error="fixture compact failure" if outcome == "failed" else None))

    def release_worker(gate, cancelled, outcome):
        while not stop.wait(.02) and not cancelled.is_set():
            if gate.exists():
                settled(outcome, "NATIVE_COMPACT_SUMMARY" if outcome == "compacted" else "Nothing to compact yet.")
                return

    emit(dict(type="ready", protocol="rpi", supportedCommands=["get_messages"] + (["compact"] if enabled else [])))
    try:
        for line in sys.stdin:
            command = json.loads(line)
            with (root / "requests.jsonl").open("a", encoding="utf-8") as output:
                output.write(json.dumps(command) + "\n")
            kind = command["type"]
            data = {}
            if kind == "get_state":
                data = dict(sessionId=sid, leafId=None)
            elif kind == "get_messages":
                data = dict(messages=[], total=0, leafId=None, firstId=None, lastId=None, hasMore=False, beforeId=None)
            elif kind == "get_available_models":
                data = dict(models=[])
            elif kind == "get_available_thinking_levels":
                data = dict(levels=[])
            elif kind in ("compact", "prompt"):
                emit(dict(type="response", id=command["id"], success=True, data=dict(accepted=True)))
                if kind == "prompt":
                    emit(dict(type="message_update", eventType="text_delta", delta="NORMAL_PROMPT_OK"))
                    settled("completed")
                else:
                    assert enabled, command
                    instructions = command.get("customInstructions", "") or ""
                    active = threading.Event()
                    if instructions == "permission":
                        emit(dict(type="ui_dialog_request", requestId="permission", toolCallId="t", ui=dict(
                            purpose="permission", kind="selector", question="Allow?", options=[
                                dict(title="Allow once", permission="allow_once"), dict(title="Reject", permission="reject_once")])))
                    else:
                        # This is NOT completion. The controlling test releases settlement later.
                        emit(dict(type="agent_end"))
                        gate = root / (str(command["id"]) + ".release")
                        (root / "accepted.json").write_text(json.dumps(command), encoding="utf-8")
                        worker = threading.Thread(target=release_worker, args=(gate, active,
                            "failed" if instructions == "failed" else "declined" if instructions == "declined" else "compacted"))
                        workers.append(worker)
                        worker.start()
                continue
            elif kind == "ui_dialog_response":
                emit(dict(type="ui_dialog_closed", requestId="permission"))
                emit(dict(type="tool_execution_start", toolCallId="t", toolName="read", args=dict(path="fixture")))
                emit(dict(type="tool_execution_end", toolCallId="t", isError=False, result=dict(content=[])))
                settled("compacted", "NATIVE_COMPACT_SUMMARY")
                continue
            elif kind == "abort":
                if active:
                    active.set()
                # Current native can report provider abortion as failed even
                # though the caller explicitly cancelled the compact run.
                settled("failed" if instructions == "cancel-failed" else "aborted")
                continue
            elif kind == "stop":
                break
            else:
                raise AssertionError(command)
            emit(dict(type="response", id=command["id"], success=True, data=data))
    finally:
        stop.set()
        for worker in workers:
            worker.join(timeout=2)
            assert not worker.is_alive()


def run(bridge, logs):
    for enabled in (False, True):
        root = logs / ("cap-on" if enabled else "cap-off")
        root.mkdir(parents=True)
        env = environment(root)
        launcher = root / ("native.cmd" if os.name == "nt" else "native")
        script = str(Path(__file__).resolve())
        launcher.write_text(f'@echo off\n"{sys.executable}" -B "{script}" --backend %*\n' if os.name == "nt" else
                            f'#!/bin/sh\nexec {shlex.quote(sys.executable)} -B {shlex.quote(script)} --backend "$@"\n')
        launcher.chmod(0o700)
        env.update(RPI_BIN=str(launcher), COMPACT_FIXTURE_ROOT=str(root), COMPACT_FIXTURE_CAP=str(int(enabled)))
        peer = Peer([bridge], env, root, logs, root.name)
        try:
            success(peer, "initialize", dict(protocolVersion=1))
            result, before = success(peer, "session/new", dict(cwd=str(root), mcpServers=[]))
            sid = result["sessionId"]
            assert before == [], ("announcement preceded new response", before)
            params = dict(sessionId=sid, cwd=str(root), mcpServers=[])
            # Following request fences the post-response command announcement.
            _, updates = success(peer, "session/resume", params)
            other, announcements = controls(updates, sid)
            assert not other and len(announcements) == int(enabled), updates
            _, updates = success(peer, "session/resume", params)
            assert len(controls(updates, sid)[1]) == int(enabled) and not controls(updates, sid)[0], updates
            _, updates = success(peer, "session/load", params)
            assert len(controls(updates, sid)[1]) == int(enabled) and not controls(updates, sid)[0], updates
            # Exact text token only; resource-bearing prompt must never compact.
            for blocks in ([dict(type="text", text="/compactness")],
                           [dict(type="text", text="/compact"), dict(type="resource_link", uri="file:///fixture", name="fixture")],
                           [dict(type="text", text="/compact"), dict(type="resource", resource=dict(uri="file:///fixture", text="embedded context", mimeType="text/plain"))]):
                result, updates = success(peer, "session/prompt", dict(sessionId=sid, prompt=blocks))
                assert result["stopReason"] == "end_turn" and text(updates) == "NORMAL_PROMPT_OK", (result, updates)
            if not enabled:
                reply, updates = peer.request("session/prompt", dict(sessionId=sid, prompt=[dict(type="text", text="/compact")]))
                assert "error" in reply and not updates, (reply, updates)
                commands = [json.loads(line)["type"] for line in (root / "requests.jsonl").read_text().splitlines()]
                assert "compact" not in commands
                continue
            for index, instructions in enumerate(("", "Keep decisions\nKeep paths", "declined", "failed", "cancel", "cancel-failed", "permission", "eof")):
                accepted = root / "accepted.json"
                accepted.unlink(missing_ok=True)
                rid = 1000 + index
                send(peer, "session/prompt", dict(sessionId=sid, prompt=[dict(type="text", text="/compact" + (" \n" + instructions if instructions else ""))]), rid)
                if instructions == "permission":
                    updates = []
                    deadline = time.monotonic() + 10
                    while True:
                        item = peer.receive(deadline)
                        if item.get("method") == "session/request_permission":
                            option = next(o for o in item["params"]["options"] if o["kind"] == "allow_once")
                            peer.process.stdin.write(json.dumps(dict(jsonrpc="2.0", id=item["id"], result=dict(outcome=dict(outcome="selected", optionId=option["optionId"])))) + "\n")
                            peer.process.stdin.flush()
                            break
                        updates.append(item)
                    reply, tail = response(peer, rid)
                    assert reply.get("result", {}).get("stopReason") == "end_turn" and text(tail) == "NATIVE_COMPACT_SUMMARY", (reply, tail)
                    assert any(u.get("params", {}).get("update", {}).get("sessionUpdate") == "tool_call" for u in tail), tail
                    continue
                wait_file(accepted)
                command = json.loads(accepted.read_text())
                actual_instructions = command.get("customInstructions")
                assert actual_instructions == instructions if instructions else actual_instructions in (None, ""), command
                # Any queued command announcement is allowed, but no run response/chunk.
                pending = []
                while True:
                    try:
                        pending.append(peer.output.get(timeout=.2))
                    except queue.Empty:
                        break
                assert not controls(pending, sid)[0], ("early completion", pending)
                if instructions == "eof":
                    break  # close() must cancel/join the still-active run and child.
                for method, extra in (("session/prompt", dict(prompt=[dict(type="text", text="second")])), ("session/load", params)):
                    rejected, _ = peer.request(method, dict(sessionId=sid, **{k: v for k, v in extra.items() if k != "sessionId"}))
                    assert "error" in rejected, rejected
                cancelled = instructions in ("cancel", "cancel-failed")
                if cancelled:
                    send(peer, "session/cancel", dict(sessionId=sid))
                else:
                    (root / (str(command["id"]) + ".release")).touch()
                reply, updates = response(peer, rid)
                if instructions == "failed":
                    assert "fixture compact failure" in reply.get("error", {}).get("message", ""), reply
                else:
                    assert reply.get("result", {}).get("stopReason") == ("cancelled" if cancelled else "end_turn"), reply
                    assert text(updates) == ("" if cancelled else "Nothing to compact yet." if instructions == "declined" else "NATIVE_COMPACT_SUMMARY"), updates
                result, updates = success(peer, "session/prompt", dict(sessionId=sid, prompt=[dict(type="text", text="next prompt")]))
                assert result["stopReason"] == "end_turn" and text(updates) == "NORMAL_PROMPT_OK"
        finally:
            peer.close()
        print("PASS compact protocol capability", enabled)


if __name__ == "__main__":
    if "--backend" in sys.argv:
        backend()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("--bridge", required=True)
        parser.add_argument("--diagnostics-dir", required=True, type=Path)
        args = parser.parse_args()
        args.diagnostics_dir.mkdir(parents=True, exist_ok=True)
        run(str(Path(args.bridge).resolve()), args.diagnostics_dir.resolve())
