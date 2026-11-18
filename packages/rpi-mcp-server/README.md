# rpi-mcp-server

Publish the rpi agent as an **MCP server**. This is the opposite direction of
[`rpi-mcp-adapter`](../rpi-mcp-adapter/README.md), which consumes MCP servers.

Two entry points share one implementation:

- `rpi-mcp-bridge` — standalone MCP stdio server. **Recommended**: it owns its
  own stdin/stdout, so it works with a stock `rpi` and never competes with
  `--acp`, `--http` or `--webserver`.
- `rpi --mcp` — in-process plugin entry, using the host's existing
  `{"ownsStdio":true}` and `StdioLifecycle` ABI, the same contract `--acp` uses.

```text
MCP client -> MCP JSON-RPC over stdin/stdout -> rpi-mcp-bridge -> rpi --mode rpc
                                                               (one child host)
```

The adapter owns MCP framing, the tool catalog, resource exposure,
`tools/call` concurrency and cancellation mapping. Native session lifecycle,
prompt acceptance, settlement, cancellation and process-tree cleanup stay in
`rpi-extcore`.

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

Live checks against `rpi 0.24.0` covered `initialize`, `tools/list`,
`rpi_state`, `rpi_available_models`, `rpi_thinking_levels`,
`rpi_set_thinking_level`, `resources/list`, `resources/read` (transcript and
state), unknown-resource rejection and `notifications/cancelled` (the cancelled
prompt settled with `{"error":"prompt cancelled"}`). A full model round trip was
**not** verified here: in this environment the native `rpi --mode rpc` child
stalls after `message_start` with no adapter in the path, so the settle path is
covered by the scripted-host test instead of a live model call.
