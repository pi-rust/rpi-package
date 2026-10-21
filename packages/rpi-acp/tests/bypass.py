"""Check child permission policy through both ACP frontends, without model calls."""
import argparse
import json
import os
from pathlib import Path
import shlex
import sys
import tempfile

sys.dont_write_bytecode = True
from restart import Client


def backend():
    Path(os.environ["RPI_TEST_POLICY_OUTPUT"]).write_text(json.dumps({
        "args": sys.argv[1:], "gate": os.environ.get("RPI_PERMISSION_GATE"),
    }), encoding="utf-8")
    session_id = sys.argv[sys.argv.index("--session-id") + 1]
    for line in sys.stdin:
        command = json.loads(line)
        if command["type"] == "get_state":
            print(json.dumps(dict(type="response", id=command["id"], command="get_state", success=True,
                                  data=dict(sessionId=session_id, leafId=None))), flush=True)


def check(command, env, root, bypass, legacy=False):
    output = root / "policy.json"
    output.unlink(missing_ok=True)
    env = dict(env, RPI_TEST_POLICY_OUTPUT=str(output), RPI_PERMISSION_GATE="1",
               RPI_ACP_NO_PERMISSION_GATE="1" if legacy else "0")
    client = Client(command, env)
    try:
        response, _ = client.request("initialize", dict(protocolVersion=1))
        assert "result" in response, response
        # Policy must reach every session created by the same ACP process.
        for _ in range(2):
            response, _ = client.request("session/new", dict(cwd=str(root), mcpServers=[]))
            assert "result" in response, (response, "".join(client.errors))
            policy = json.loads(output.read_text(encoding="utf-8"))
            assert ("--bypass" in policy["args"]) == bypass, policy
            assert ("--permission-gate" in policy["args"]) == (not bypass and not legacy), policy
            assert policy["gate"] == ("0" if bypass else "1"), policy
    finally:
        client.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bridge", required=True)
    parser.add_argument("--rpi")
    parser.add_argument("--extension")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="rpi-acp-bypass-") as directory:
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
                   RPI_ACP_NO_AUTO_REGISTER_ZED="1")
        frontends = [[args.bridge]]
        if args.rpi and args.extension:
            agent = root / "agent"
            agent.mkdir()
            env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(agent / "extensions"),
                       ANTHROPIC_API_KEY="offline-plugin-fixture")
            frontends.append([args.rpi, "--acp", "--offline", "--no-session", "--local-only",
                              "--provider", "anthropic", "--model", "claude-sonnet-4-5",
                              "--extension", args.extension])
        for command in frontends:
            check(command, env, root, False)
            check(command + ["--bypass"], env, root, True)
            check(command, env, root, False, legacy=True)
            if command[0] != args.bridge:
                check(command + ["--bypass=false"], env, root, False)
            print(f"PASS: default, bypass, inherited gate env, legacy opt-out: {command[0]}")


if __name__ == "__main__":
    if "--backend" in sys.argv:
        backend()
    else:
        main()
