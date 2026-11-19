# rpi-mcp-server

Publish the rpi agent as an **MCP server**. This is the opposite direction of
[`rpi-mcp-adapter`](../rpi-mcp-adapter/README.md), which consumes MCP servers.

Transports:

| Transport | Entry point | Spec |
| --- | --- | --- |
| stdio | `rpi-mcp-bridge` (recommended) or `rpi --mcp` | MCP |
| Streamable HTTP | `rpi-mcp-bridge --http <ADDR>` → `POST /mcp` | MCP |
| WebSocket | `--http` listener → `GET /mcp/ws` | **private**, not an MCP transport |

`rpi-mcp-bridge` is the recommended entry point: it owns its own stdin/stdout, so
it works with a stock `rpi` and never competes with `--acp`, `--http` or
`--webserver`. The `rpi --mcp` plugin entry uses the host's existing
`{"ownsStdio":true}` and `StdioLifecycle` ABI, the same contract `--acp` uses.

```text
MCP client -> MCP JSON-RPC over stdin/stdout -> rpi-mcp-bridge -> rpi --mode rpc
                                                               (one child host)
```

The adapter owns MCP framing, the tool catalog, resource exposure,
`tools/call` concurrency and cancellation mapping. Native session lifecycle,
prompt acceptance, settlement, cancellation and process-tree cleanup stay in
`rpi-extcore`.

## HTTP and WebSocket

```bash
rpi-mcp-bridge --http 127.0.0.1:8080 --cwd /path/to/workspace
rpi-mcp-bridge --http 0.0.0.0:8080 --token "$TOKEN" --allow-origin https://ide.example
```

| Endpoint | Behaviour |
| --- | --- |
| `POST /mcp` | JSON-RPC frames. Requests get one JSON response, or an SSE stream when the client sends `Accept: text/event-stream` **and** the body calls `rpi_prompt`. Notifications/responses only → `202`. |
| `GET /mcp` | `405` with `Allow: POST, DELETE`: this server offers no standalone server stream. |
| `DELETE /mcp` | Terminates the session (`204`), so later requests get `404`. |
| `OPTIONS /mcp` | Browser preflight: `204` with CORS headers for an allowed origin, `403` otherwise. |
| `GET /mcp/ws` | Private WebSocket upgrade. First frame is `{"jsonrpc":"2.0","method":"rpi/session","params":{"sessionId":"…"}}`; reattach with `?session=<id>`. |

Session rules: `initialize` without `Mcp-Session-Id` opens a session and returns
the id in the response header; every later request must send it back. An unknown
or deleted session is `404` — re-`initialize` to continue. Re-initializing an
already-open session is `400` (delete it first) so a second native child is never
started behind a live session. `MCP-Protocol-Version`
is validated when present. Sessions are capped (`--max-sessions`, default 8) and
retired after 15 minutes of inactivity.

Security: loopback binds only need nothing extra; **a non-loopback bind requires
`--token`** and every request then needs `Authorization: Bearer <token>`.
Cross-origin requests are rejected unless the `Origin`/`Host` is loopback or
listed with `--allow-origin`; allowed origins also get CORS headers and a
preflight response so a browser client can call `POST /mcp` directly. Bearer
tokens are compared in constant time. There is no TLS and no OAuth: put it behind a
trusted proxy or keep it on loopback.

`/mcp/ws` is a convenience channel for our own clients (one bidirectional socket
on the same native session), not something an MCP client can use. It shares the
access checks, session table and idle reaper with the HTTP transport.

## Tools

| Tool | Arguments | Result |
| --- | --- | --- |
| `rpi_prompt` | `message` | Settled assistant text plus `outcome` (`completed`, `token_limit`) |
| `rpi_state` | — | Native session state (model, thinking level, active tools, context, queue, stats) |
| `rpi_available_models` | — | Models the native host can switch to |
| `rpi_thinking_levels` | — | Thinking levels the native host can switch to |
| `rpi_set_model` | `model`, optional `provider` | `{"updated":true}` |
| `rpi_set_thinking_level` | `level` | `{"updated":true}` |
| `rpi_compact` | — | Compaction summary and `outcome`; fails when the host cannot compact |
| `rpi_abort` | — | `{"aborted":true}` |

Results are returned as MCP `content` text plus `structuredContent`. A tool-level
failure is reported as `isError: true` with a JSON `error` object, not as a
JSON-RPC error; only protocol faults (unknown method, bad `tools/call` shape,
unknown tool, calling before `initialize`, too many concurrent calls) are
JSON-RPC errors.

`rpi_prompt` streams `notifications/progress` when the client sends
`_meta.progressToken`. `notifications/cancelled` targeting the active
`rpi_prompt` request aborts the native run; the prompt then settles with
`isError: true` and `{"error":"prompt cancelled"}`.

## Resources

| URI | MIME type | Content |
| --- | --- | --- |
| `rpi://session/transcript` | `text/markdown` | Committed messages of this session, read-only |
| `rpi://session/state` | `application/json` | Native session state (model, thinking, tools, context, queue) |

`resources/list` returns exactly these two; `resources/read` renders them without
starting a run. `resources/subscribe` is supported: whenever the native run
settles, the server sends `notifications/resources/updated` for each subscribed
URI so the client re-reads instead of polling. Reads are bounded to 256 KiB of
text and the transcript is capped at the newest 200 messages.

## Client configuration

```json
{
  "mcpServers": {
    "rpi": {
      "command": "/path/to/rpi-mcp-bridge",
      "args": ["--cwd", "/path/to/workspace"]
    }
  }
}
```

Bridge options:

```text
--cwd <DIR>            Workspace for the agent (default: current directory)
--model <MODEL>        Model forwarded to the native host
--provider <PROVIDER>  Provider forwarded to the native host
--session-dir <DIR>    Native session directory (default: native default)
--resume <FILE>        Continue an existing native session file
--bypass               Disable permission-gate approvals (trusted use only)
```

The native host is located with `RPI_BIN`, a sibling `rpi` next to the bridge, or
`rpi` on `PATH` (see `rpi_extcore::process::resolve_rpi_executable`).

`rpi --mcp` accepts the same `--model`, `--provider`, `--session-dir`,
`--resume` and `--bypass` flags from the host command line. `--resume` starts the
child with the native `--session <FILE>` flag, so an MCP client can continue an
existing transcript.

MCP has no approval channel, so a native `ui_dialog_request` (permission gate,
`ask_user`) is **denied** instead of leaving the run pending. The prompt still
settles; its result lists the denied titles in `deniedApprovals` and appends a
`[denied N approval request(s)]` note. Use `--bypass` only for a trusted client
and workspace when a run must be allowed to use gated tools.

## Protocol surface

- MCP JSON-RPC 2.0, one frame per line on stdin/stdout; stdout carries protocol
  frames only and all diagnostics go to stderr.
- Negotiated versions: `2025-06-18`, `2025-03-26`, `2024-11-05`, falling back to
  `2025-06-18` for an unknown request. Capabilities: `tools` and
  `resources` (`subscribe: true`).
- `initialize` must complete before `tools/call`;
  `notifications/initialized` and `ping` are accepted.

## Limits and lifecycle

- 16 MiB per input frame; oversized frames are rejected without dispatching.
- At most 16 concurrent `tools/call` requests; excess calls fail with `-32603`.
- 16 MiB per HTTP body and per WebSocket message; at most 8 HTTP sessions by
  default.
- 64 queued outbound frames of backpressure.
- 256 KiB of model-visible text per tool result, truncated with
  `\n[truncated]`.
- One `rpi_prompt` at a time: a second concurrent prompt fails closed with
  `-32603` instead of queueing behind the active run.
- Other tools run concurrently with an active prompt, so `rpi_state`,
  `rpi_available_models` and `rpi_abort` stay responsive while the agent works.
- Closing stdin drains already-dispatched calls for up to 600 s;
  `SessionShutdown` (or a stdin I/O error) aborts them and then shuts the native
  session down.

## Not implemented

MCP prompts, sampling, elicitation and Streamable HTTP transport. Approval
requests are denied rather than bridged to the client.
There is no `--mcp` HTTP listener, no credential bridging and no MCP-level
authentication: the bridge inherits the native host's provider credentials and
runs tools with the configured workspace's privileges. Only connect it to a
trusted MCP client.

## Validation

```sh
cargo test --locked -p rpi-mcp-server --all-targets
cargo clippy --locked -p rpi-mcp-server --all-targets -- -D warnings
```

`tests/dispatcher.rs` drives the real dispatcher against a scripted native host
(`rpi-extcore` `test-support`): framing, `initialize` gating, method errors, tool
validation, prompt settlement, approval denial, resource reads and subscription
notifications.
It does not replace a run against a real `rpi` binary.

`tests/http.rs` starts a loopback listener and covers the HTTP session lifecycle
(initialize/session header, `tools/list`, `202`, `400`, `403`, `404`, `405`,
`204`, re-initialize rejection, token and protocol-version checks, CORS
preflight, cancellation reaching the native host, SSE for `rpi_prompt`) plus the
WebSocket bridge (session hello, initialize, tools/list, reattach, rejection).

Live checks against `rpi 0.24.0` covered `initialize`, `tools/list`,
`rpi_state`, `rpi_available_models`, `rpi_thinking_levels`,
`rpi_set_thinking_level`, `rpi_compact`, `resources/list`, `resources/read`
(transcript and state), unknown-resource rejection and
`notifications/cancelled` (the cancelled
prompt settled with `{"error":"prompt cancelled"}`). A full model round trip was
**not** verified here: in this environment the native `rpi --mode rpc` child
stalls after `message_start` with no adapter in the path, so the settle path is
covered by the scripted-host test instead of a live model call.
