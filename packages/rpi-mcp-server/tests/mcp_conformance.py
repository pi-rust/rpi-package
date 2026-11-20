#!/usr/bin/env python3
"""Exercise a live rpi MCP HTTP endpoint end to end.

This is a client-side conformance check, not a replacement for a real MCP client:
it drives the wire contract with the standard library only, so it runs anywhere
Python does. Start a server first:

    rpi-mcp-bridge --http 127.0.0.1:8080 --cwd /path/to/workspace
    python3 packages/rpi-mcp-server/tests/mcp_conformance.py --url http://127.0.0.1:8080/mcp

Every check prints PASS/FAIL and the process exits non-zero if any check fails.
Tool calls that need a working model are opt-in (`--call-state`), because they
depend on the local provider configuration.
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request

PROTOCOL = "2025-06-18"


class Client:
    def __init__(self, url: str, token: str | None) -> None:
        self.url = url
        self.token = token
        self.session: str | None = None

    def request(
        self,
        method: str,
        body: dict | None = None,
        headers: dict[str, str] | None = None,
        session: bool = True,
        read: bool = True,
    ) -> tuple[int, dict[str, str], bytes]:
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.url, data=data, method=method)
        request.add_header("accept", "application/json, text/event-stream")
        if data is not None:
            request.add_header("content-type", "application/json")
        if self.token:
            request.add_header("authorization", f"Bearer {self.token}")
        if session and self.session:
            request.add_header("mcp-session-id", self.session)
        for name, value in (headers or {}).items():
            request.add_header(name, value)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                # SSE responses never end, so callers ask for headers only.
                return response.status, dict(response.headers), response.read(4096) if read else b""
        except urllib.error.HTTPError as error:
            return error.code, dict(error.headers), error.read(4096)

    def post(self, payload: dict, **kwargs) -> tuple[int, dict[str, str], bytes]:
        return self.request("POST", payload, **kwargs)


def check(name: str, condition: bool, detail: str = "") -> bool:
    print(f"{'PASS' if condition else 'FAIL'}  {name}{'' if condition else f'  [{detail}]'}")
    return condition


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8080/mcp")
    parser.add_argument("--token", default=None, help="Bearer token, when the server requires one")
    parser.add_argument(
        "--call-state",
        action="store_true",
        help="also call rpi_state (needs a working native host)",
    )
    parser.add_argument(
        "--skip-delete",
        action="store_true",
        help="keep the session open at the end (for inspecting it by hand)",
    )
    args = parser.parse_args()

    client = Client(args.url, args.token)
    results: list[bool] = []

    status, headers, body = client.post(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL,
                "capabilities": {"elicitation": {}},
                "clientInfo": {"name": "rpi-conformance", "version": "1"},
            },
        },
        session=False,
    )
    payload = json.loads(body or b"{}")
    client.session = headers.get("mcp-session-id")
    results.append(check("initialize answers 200", status == 200, f"status={status} {body[:120]!r}"))
    results.append(check("initialize issues a session id", bool(client.session)))
    results.append(
        check(
            "initialize negotiates a protocol version",
            payload.get("result", {}).get("protocolVersion") in {PROTOCOL, "2025-03-26", "2024-11-05"},
            str(payload)[:160],
        )
    )
    capabilities = payload.get("result", {}).get("capabilities", {})
    results.append(check("advertises tools", "tools" in capabilities))
    results.append(check("advertises resource subscriptions", capabilities.get("resources", {}).get("subscribe") is True))

    status, _, body = client.post({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    payload = json.loads(body or b"{}")
    tools = [tool.get("name") for tool in payload.get("result", {}).get("tools", [])]
    results.append(check("tools/list returns the catalog", status == 200 and "rpi_prompt" in tools, str(tools)))
    results.append(
        check(
            "every tool has an object schema",
            all(tool.get("inputSchema", {}).get("type") == "object" for tool in payload.get("result", {}).get("tools", [])),
        )
    )

    status, _, body = client.post({"jsonrpc": "2.0", "id": 3, "method": "resources/list"})
    payload = json.loads(body or b"{}")
    uris = [resource.get("uri") for resource in payload.get("result", {}).get("resources", [])]
    results.append(
        check(
            "resources/list returns the session resources",
            status == 200 and "rpi://session/transcript" in uris,
            str(uris),
        )
    )

    status, _, body = client.post(
        {
            "jsonrpc": "2.0",
            "id": 4,
            "method": "resources/read",
            "params": {"uri": "rpi://session/state"},
        }
    )
    payload = json.loads(body or b"{}")
    contents = payload.get("result", {}).get("contents", [])
    results.append(
        check(
            "resources/read returns the state document",
            status == 200 and contents and contents[0].get("mimeType") == "application/json",
            str(contents)[:160],
        )
    )

    status, _, body = client.post(
        {
            "jsonrpc": "2.0",
            "id": 5,
            "method": "resources/read",
            "params": {"uri": "rpi://session/nope"},
        }
    )
    payload = json.loads(body or b"{}")
    results.append(
        check(
            "unknown resources are refused",
            status == 200 and payload.get("error", {}).get("code") == -32602 or status == 404,
            f"status={status} {payload.get('error')}",
        )
    )

    status, headers, _ = client.request(
        "GET",
        session=True,
        headers={"accept": "text/event-stream"},
        read=False,
    )
    results.append(check("GET /mcp opens the standalone stream", status == 200, f"status={status}"))
    results.append(
        check(
            "standalone stream is SSE",
            headers.get("content-type", "").startswith("text/event-stream"),
            headers.get("content-type", ""),
        )
    )

    status, _, body = client.post({"jsonrpc": "2.0", "method": "notifications/initialized"})
    results.append(check("notifications are accepted without a body", status == 202, f"status={status} {body[:80]!r}"))

    status, _, _ = client.post({"jsonrpc": "2.0", "id": 6, "method": "does/not/exist"})
    payload = json.loads(_ or b"{}")
    results.append(
        check("unknown methods are refused", status == 200 and payload.get("error", {}).get("code") == -32601, f"status={status}")
    )

    status, _, _ = client.post({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}, session=False)
    results.append(check("requests without a session id are refused", status == 400, f"status={status}"))

    status, _, _ = client.post(
        {"jsonrpc": "2.0", "id": 8, "method": "tools/list"},
        session=False,
        headers={"mcp-session-id": "does-not-exist"},
    )
    results.append(check("unknown sessions answer 404", status == 404, f"status={status}"))

    if args.call_state:
        status, _, body = client.post(
            {
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {"name": "rpi_state", "arguments": {}},
            }
        )
        payload = json.loads(body or b"{}")
        result = payload.get("result", {})
        results.append(
            check(
                "rpi_state returns native state",
                status == 200 and result.get("isError") is False and isinstance(result.get("structuredContent"), dict),
                str(payload)[:200],
            )
        )

    if not args.skip_delete and client.session:
        status, _, _ = client.request("DELETE")
        results.append(check("DELETE retires the session", status == 204, f"status={status}"))
        status, _, _ = client.post({"jsonrpc": "2.0", "id": 10, "method": "tools/list"})
        results.append(check("a retired session answers 404", status == 404, f"status={status}"))

    failed = results.count(False)
    print(f"\n{len(results) - failed}/{len(results)} checks passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
