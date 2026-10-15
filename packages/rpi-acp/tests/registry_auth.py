#!/usr/bin/env python3
"""Local stand-in for the ACP Registry's `verify_agents.py --auth-check`.

Mirrors `.github/workflows/client.py` from agentclientprotocol/registry: spawn the
agent exactly as a registry client would, send one `initialize` with the terminal
auth capability, then require at least one `agent`/`terminal` auth method.

    python packages/rpi-acp/tests/registry_auth.py --bridge path/to/rpi-acp-bridge

Exit code 0 means the reply would pass the registry's auth gate.
"""
import argparse
import json
import os
import subprocess
import sys

TERMINAL_CAPABILITY = {
    "terminal": True,
    "fs": {"readTextFile": True, "writeTextFile": True},
    "_meta": {"terminal_output": True, "terminal-auth": True},
}


def handshake(binary, cwd):
    """Return (auth_methods, result) from one initialize round trip."""
    process = subprocess.Popen(
        [binary],
        cwd=cwd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    try:
        request = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientInfo": {"name": "ACP Registry Validator", "version": "1.0.0"},
                "clientCapabilities": TERMINAL_CAPABILITY,
            },
        }
        process.stdin.write(json.dumps(request) + "\n")
        process.stdin.flush()
        line = process.stdout.readline()
        if not line:
            raise AssertionError(
                "no initialize response; stderr: " + process.stderr.read()[-2000:]
            )
        response = json.loads(line)
        assert "error" not in response, response
        result = response["result"]
        return result.get("authMethods", []), result
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        process.stdout.close()
        process.stderr.close()


def validate(methods):
    """Same rule as the registry: at least one agent/terminal method."""
    typed = []
    for method in methods:
        kind = method.get("type")
        if not kind:
            meta = method.get("_meta", {})
            kind = "terminal" if "terminal-auth" in meta else "agent"
        typed.append((method.get("id", ""), kind))
    valid = [pair for pair in typed if pair[1] in {"agent", "terminal"}]
    if not valid:
        return False, f"no agent/terminal auth method; found {typed}"
    return True, ", ".join(f"{name}({kind})" for name, kind in valid)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", required=True)
    args = parser.parse_args()

    methods, result = handshake(args.bridge, os.getcwd())
    ok, message = validate(methods)
    print(f"agentInfo:    {result.get('agentInfo')}")
    print(f"authMethods:  {json.dumps(methods)}")
    print(f"verdict:      {'PASS' if ok else 'FAIL'} - {message}")

    # A `terminal` method must tell the client how to launch the login flow.
    for method in methods:
        if method.get("type") == "terminal":
            assert method.get("args"), "terminal auth without args"
            assert "command" not in method, "terminal auth must not pick a command"
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
