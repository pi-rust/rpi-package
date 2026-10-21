"""Process-level ACP streaming checks using a deterministic RPC backend.

python tests/streaming.py --bridge /path/rpi-acp-bridge
Optionally add --rpi /path/rpi --extension /path/rpi_acp.dll.
No model calls or provider credentials are needed by the mock backend.
"""
import argparse
import json
import os
import shlex
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True
from restart import Client, fixture_environment

FINAL = "前半段🌊，中间部分，后半段。"


def backend():
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    session_id = sys.argv[sys.argv.index("--session-id") + 1]

    def emit(value):
        print(json.dumps(value, ensure_ascii=False), flush=True)

    def start():
        emit(dict(type="message_start", message=dict(role="assistant", content=[])))

    def delta(text, snapshot=None):
        event = dict(type="message_update", eventType="text_delta", delta=text)
        if snapshot is not None:
            event["message"] = dict(role="assistant", content=[dict(type="text", text=snapshot)])
        emit(event)

    def end(text, reason="stop", error=None):
        emit(dict(type="message_end", message=dict(role="assistant", stopReason=reason,
                                                   errorMessage=error, content=[dict(type="text", text=text)])))

    for line in sys.stdin:
        command = json.loads(line)
        kind = command["type"]
        if kind == "get_state":
            result = dict(sessionId=session_id, leafId=None)
        elif kind == "prompt":
            assert isinstance(command["message"], str) and "content" not in command
            scenario = command["message"]
            emit(dict(type="response", id=command["id"], command=kind, success=True, data=dict(accepted=True)))
            start()
            text, outcome = FINAL, "completed"
            if scenario == "middle":
                delta("前半段🌊", "前半段🌊")
                delta("后半段。", FINAL)
                end(FINAL)
            elif scenario == "end-snapshot":
                delta("前半段🌊")
                end(FINAL)
            elif scenario == "complete":
                delta(FINAL, FINAL)
                end(FINAL)
            elif scenario == "tools":
                delta("先读取。")
                end("先读取。", "toolUse")
                emit(dict(type="tool_execution_start", toolCallId="t", toolName="read", args=dict(path="file")))
                emit(dict(type="tool_execution_end", toolCallId="t", isError=False,
                          result=dict(content=[dict(type="text", text="mock file")])) )
                start()
                delta("前半段🌊")
            elif scenario == "failed":
                text, outcome = "前半段🌊", "failed"
                delta(text)
                end(text, "error", "mock provider disconnected")
            elif scenario == "length":
                delta(FINAL)
                end(FINAL, "length")
            elif scenario == "long":
                text = "测试🌊\n" * 10000
                delta("测试🌊\n")
            elif scenario != "no-deltas":
                # The final message snapshot repairs omitted deltas.
                delta("前半段🌊")
            if scenario not in ("middle", "end-snapshot", "complete", "failed", "length"):
                end(text)
            emit(dict(type="agent_settled", runId="fixture-run", outcome=outcome,
                      error="mock provider disconnected" if outcome == "failed" else None))
            continue
        elif kind == "abort":
            continue
        else:
            raise AssertionError(command)
        emit(dict(type="response", id=command["id"], command=kind, success=True, data=result))


def check(command, env, cwd):
    client = Client(command, env)
    try:
        client.request("initialize", dict(protocolVersion=1))
        response, _ = client.request("session/new", dict(cwd=cwd, mcpServers=[]))
        assert "result" in response, response
        session_id = response["result"]["sessionId"]
        for scenario in ["early-completion", "middle", "end-snapshot", "complete", "no-deltas", "tools", "failed", "length", "long"]:
            response, updates = client.request("session/prompt", dict(sessionId=session_id,
                                                prompt=[dict(type="text", text=scenario)]))
            message_updates = [item["params"]["update"] for item in updates if item.get("method") == "session/update"]
            text = "".join(item["content"]["text"] for item in message_updates if item["sessionUpdate"] == "agent_message_chunk")
            if scenario == "failed":
                assert response.get("error", {}).get("message") == "mock provider disconnected", response
                assert text == "前半段🌊", text
            else:
                expected = "测试🌊\n" * 10000 if scenario == "long" else ("先读取。" if scenario == "tools" else "") + FINAL
                assert text == expected, (scenario, len(text), len(expected), response, "".join(client.errors))
                assert response["result"]["stopReason"] == ("max_tokens" if scenario == "length" else "end_turn"), response
                if scenario == "tools":
                    assert [item["sessionUpdate"] for item in message_updates].count("tool_call") == 1
        print("PASS: early completion, missing middle/tail chunks, snapshots, tools, Unicode, long replies, failures and token limits")
    finally:
        client.close()


if __name__ == "__main__":
    if "--backend" in sys.argv:
        backend()
    else:
        parser = argparse.ArgumentParser()
        parser.add_argument("--bridge", required=True)
        parser.add_argument("--rpi")
        parser.add_argument("--extension")
        args = parser.parse_args()
        with tempfile.TemporaryDirectory(prefix="rpi-acp-stream-") as directory:
            root = Path(directory)
            script = Path(__file__).resolve()
            if os.name == "nt":
                launcher = root / "backend.cmd"
                launcher.write_text(f'@echo off\n"{sys.executable}" "{script}" --backend %*\n')
            else:
                launcher = root / "backend"
                launcher.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} {shlex.quote(str(script))} --backend "$@"\n')
                launcher.chmod(0o700)
            env = dict(os.environ, RPI_BIN=str(launcher), RPI_ACP_SESSION_DIR=str(root / "sessions"),
                       RPI_ACP_NO_PERMISSION_GATE="1", RPI_ACP_NO_AUTO_REGISTER_ZED="1")
            env.update(fixture_environment(root))
            check([args.bridge], env, str(root))
            if args.rpi and args.extension:
                check([args.rpi, "--acp", "--no-session", "--local-only", "--extension", args.extension], env, str(root))
