# Serving the rpi agent over MCP

`rpi-mcp-server` publishes the agent to MCP clients. This is the opposite
direction of [`rpi-mcp-adapter`](../packages/rpi-mcp-adapter/README.md), which
lets the agent *use* MCP servers, and it is unrelated to
[`rpi-server`](../packages/rpi-server/README.md), which serves rpi's own JSON-RPC
to rpi clients.

```text
MCP client -> MCP (stdio | Streamable HTTP) -> rpi-mcp-server -> rpi --mode rpc
```

The adapter owns MCP framing, the tool catalog, resources, progress, cancellation
and the transports. Native session lifecycle, prompt acceptance, settlement and
child cleanup stay in `rpi-extcore`, so this package cannot drift from the Web and
ACP adapters.

## Choosing a transport

| Transport | Start it with | Spec | When to use |
| --- | --- | --- | --- |
| stdio | `rpi-mcp-bridge` (recommended) or `rpi --mcp` | MCP | Local client that can spawn a command |
| Streamable HTTP | `rpi-mcp-bridge --http 127.0.0.1:8080` | MCP | Container, remote client, or one server shared by several clients |
| WebSocket | `rpi-mcp-bridge --http …`, connect to `GET /mcp/ws` | **private** | Our own clients that want one bidirectional socket |

`/mcp/ws` is a convenience channel, not an MCP transport: stock MCP clients only
speak stdio and Streamable HTTP.

## Getting the binaries

```bash
rpi install rpi-mcp-server          # the extension, for `rpi --mcp`
task pack-mcp                       # dist/rpi-mcp-<platform>.tar.gz with the bridge
```

`pack-mcp` produces a flat archive whose root holds `rpi-mcp-bridge` (plus this
package's README), and prints the client entry to paste. Releases publish the
same archive as `rpi-mcp-<platform>.tar.gz` and record it as `mcp_bridge` in
`rpi-packages.json`.

## Client configuration

Any client that understands `mcpServers` can launch the bridge:

```json
{
  "mcpServers": {
    "rpi": {
      "command": "/absolute/path/to/rpi-mcp-bridge",
      "args": ["--cwd", "/absolute/path/to/workspace"]
    }
  }
}
```

Bridge options: `--cwd`, `--model`, `--provider`, `--session-dir`, `--resume`,
`--bypass`, plus the HTTP options `--http`, `--token`, `--allow-origin`,
`--max-sessions`. The native host is resolved from `RPI_BIN`, a sibling `rpi`, or
`PATH`.

For a client that speaks HTTP instead of spawning processes, point it at
`http://127.0.0.1:8080/mcp` and let it run the `initialize` handshake. The server
returns `Mcp-Session-Id`, requires it on later requests, and answers `404` once a
session is gone.

`GET /mcp` with that session id opens the standalone server stream: an SSE stream
carrying server notifications (resource updates) that is independent of any POST.
It honours `Last-Event-ID` for replay and answers `400` without a session header.

## What the client can call

Tools: `rpi_prompt`, `rpi_state`, `rpi_available_models`,
`rpi_thinking_levels`, `rpi_set_model`, `rpi_set_thinking_level`, `rpi_compact`,
`rpi_abort`.

Resources: `rpi://session/transcript` (markdown) and `rpi://session/state`
(json), both read-only, with `resources/subscribe` sending
`notifications/resources/updated` when a run settles.

The [package README](../packages/rpi-mcp-server/README.md) documents the exact
arguments, defaults and limits.

## Trust and side effects

- Prompts run the agent with this workspace's privileges: tools may edit files,
  run commands and reach the network. The MCP client's approval UI does not
  protect you — the agent runs inside rpi, not inside the client.
- A native permission dialog becomes MCP `elicitation/create` when the client
  advertised that capability, so the client's own UI decides. Otherwise (or when
  the client ignores it, or the dialog has no answerable shape) it is **denied**
  and reported in `deniedApprovals`; the run still settles. Use `--bypass` only
  when you accept unattended execution.
- stdio needs no authentication: whoever can spawn the bridge is trusted.
- HTTP defaults to loopback. A non-loopback bind requires `--token`, every request
  then needs `Authorization: Bearer …`, and `Origin`/`Host` are checked against
  loopback plus `--allow-origin`. There is no TLS and no OAuth: terminate TLS in a
  proxy, or keep it on loopback.
- Remote tool output and resources are untrusted data. They are not instructions.

## Limits

16 MiB per frame/body/WebSocket message, 16 concurrent `tools/call` requests,
8 HTTP sessions by default, 16 retained streams per session, 512 retained SSE
events per stream, 256 KiB of model-visible text per result, 600 s per request,
15-minute idle session retirement.

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `404 unknown or expired MCP session` | The session was deleted, retired after 15 minutes idle, or the server restarted. Re-run `initialize`. |
| SSE stream ends after a reconnect | Resume with `Last-Event-ID`; a stream whose events expired also answers `404`. |
| `429 too many MCP sessions` | `--max-sessions` reached; `DELETE /mcp` or raise the limit. |
| `-32603 another tool call is already driving the agent` | One prompt at a time per session; wait or `rpi_abort`. |
| Prompt returns `isError` with `prompt cancelled` | `notifications/cancelled` aborted the run. |
| `deniedApprovals` in the result | A dialog was denied: the client did not advertise `elicitation`, declined it, did not answer, or the dialog had no answerable shape. |
| Prompt never settles | In this environment the native child can stall after `message_start`; check `rpi --mode rpc` directly before blaming the adapter. |

## Validation

Fixtures cover the dispatcher (against a scripted native host), the HTTP/WebSocket
lifecycle, stream resumption, cancellation and elicitation; see the package
README.

For a live endpoint, run the wire-contract script against a running server:

```bash
rpi-mcp-bridge --http 127.0.0.1:8080 --cwd /path/to/workspace &
python3 packages/rpi-mcp-server/tests/mcp_conformance.py --url http://127.0.0.1:8080/mcp --call-state
```

It reports 19 checks (initialize, session header, catalog, resources, standalone
stream, `202`/`400`/`404`, `rpi_state`, DELETE). Neither the fixtures nor the
script exercise a real client UI or a model conversation, so try your own client
against a real workspace before relying on it.
