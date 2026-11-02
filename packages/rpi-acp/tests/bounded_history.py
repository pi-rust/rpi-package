"""Bounded ACP v1 replay fixture; no credentials, providers or native changes.
Run: python -B tests/bounded_history.py --bridge <rpi-acp-bridge>
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import sys
import tempfile
import uuid
from restart import Client


def backend():
    root = Path(os.environ["HISTORY_FIXTURE_ROOT"])
    scenario = os.environ["HISTORY_FIXTURE_SCENARIO"]
    session_id = os.environ["HISTORY_FIXTURE_ID"]
    def emit(value):
        print(json.dumps(value), flush=True)
    emit(dict(type="ready", protocol="rpi", supportedCommands=[] if scenario == "old" else ["get_messages"]))
    for line in sys.stdin:
        command = json.loads(line)
        kind = command["type"]
        with (root / "requests.jsonl").open("a") as log:
            log.write(json.dumps(command) + "\n")
        if kind == "get_state":
            data = dict(sessionId=session_id, leafId="512")
        elif kind == "get_messages":
            assert scenario != "old"
            end = int(command["beforeId"]) if command.get("beforeId") else 513
            limit = command["limit"]
            assert 1 <= limit <= 128
            start = max(0, end - limit)
            data = dict(messages=[dict(entryId=str(i), role="user", content=str(i)) for i in range(start, end)],
                        total=513, leafId="512", firstId=str(start), lastId=str(end-1),
                        hasMore=start > 0, beforeId=command.get("beforeId"))
            if (root / "bad").exists():
                data["firstId"] = "malformed"
        elif kind == "get_available_models":
            data = dict(models=[])
        elif kind == "get_available_thinking_levels":
            data = dict(levels=[])
        elif kind in ("stop", "abort"):
            if kind == "stop":
                break
            continue
        else:
            raise AssertionError(command)
        emit(dict(type="response", id=command["id"], success=True, data=data))


def run(bridge):
    for scenario in ("paged", "old"):
        with tempfile.TemporaryDirectory(prefix="bounded-history-") as temp:
            root = Path(temp)
            session_id = str(uuid.uuid4())
            store = root / "sessions"
            directory = store / session_id / "workspace"
            directory.mkdir(parents=True)
            path = directory / f"fixture_{session_id}.jsonl"
            records = [dict(kind="header", version=4, id=session_id)]
            records += [dict(kind="entry", id=str(i), parentId=str(i-1) if i else None,
                             type="message", message=dict(role="user", content=str(i))) for i in range(513)]
            path.write_text("".join(json.dumps(record) + "\n" for record in records))
            script = Path(__file__).resolve()
            launcher = root / ("backend.cmd" if os.name == "nt" else "backend")
            if os.name == "nt":
                launcher.write_text(f'@echo off\n"{sys.executable}" -B "{script}" --backend %*\n')
            else:
                launcher.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} -B {shlex.quote(str(script))} --backend "$@"\n')
                launcher.chmod(0o700)
            env = dict(os.environ, RPI_BIN=str(launcher), RPI_ACP_SESSION_DIR=str(store),
                       RPI_ACP_NO_PERMISSION_GATE="1", HISTORY_FIXTURE_ROOT=str(root),
                       HISTORY_FIXTURE_ID=session_id, HISTORY_FIXTURE_SCENARIO=scenario)
            client = Client([bridge], env)
            params = dict(sessionId=session_id, cwd=str(root), mcpServers=[])
            try:
                assert "result" in client.request("initialize", dict(protocolVersion=1))[0]
                response, updates = client.request("session/load", params)
                assert "result" in response, response
                chunks = [u["params"]["update"]["content"]["text"] for u in updates if u.get("method") == "session/update"]
                assert chunks == [str(i) for i in range(513)], (scenario, len(chunks))
                before = (root / "requests.jsonl").read_text().count('"type": "get_messages"')
                response, updates = client.request("session/resume", params)
                assert "result" in response and not updates, (response, updates)
                assert (root / "requests.jsonl").read_text().count('"type": "get_messages"') == before
                # Failure must emit no prepared history, keep attached backend alive,
                # and never switch a bad advertised page to file fallback.
                if scenario == "paged":
                    (root / "bad").touch()
                else:
                    path.write_text("bad history\n")
                response, updates = client.request("session/load", params)
                assert response["error"]["code"] == -32603 and not updates, (response, updates)
                response, updates = client.request("session/resume", params)
                assert "result" in response and not updates, (response, updates)
                if scenario == "paged":
                    (root / "bad").unlink()
                    response, updates = client.request("session/load", params)
                    assert "result" in response and len(updates) == 513, (response, len(updates))
                for method in ("session/load", "session/resume"):
                    missing = dict(params, sessionId=str(uuid.uuid4()))
                    response, updates = client.request(method, missing)
                    assert response["error"]["code"] == -32603 and not updates
            finally:
                client.close()
        print(f"PASS {scenario}: 513 oldest-first before response; resume; load error; attached host survives; unknown session")


if __name__ == "__main__":
    if "--backend" in sys.argv:
        backend()
    else:
        parser = argparse.ArgumentParser()
        parser.add_argument("--bridge", required=True)
        run(parser.parse_args().bridge)
