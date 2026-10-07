# rpi-mcp-adapter

MCP tools for the RPI Rust extension ABI, with persistent stdio and Streamable
HTTP connections. The host registers three stable tools:

- `mcp_list`: list configured servers or discover a server's tools and schemas;
- `mcp_call`: call a discovered tool, preserving text/images and structured results;
- `mcp_request`: send a raw HTTP JSON-RPC request for compatibility/diagnostics.
  This low-level tool does not manage initialization; use the other two for normal MCP calls.

## Configuration and discovery

```json
{"mcpServers":{"playwright":{"command":"node","args":["server.js","--extension"],"timeout":60},"remote":{"url":"http://localhost:8931/mcp","headers":{}}}}
```

List server names without starting processes with `mcp_list`:

```json
{"configPath":".rpi/mcp.json"}
```

Discover all tool schemas, including paginated tool lists:

```json
{"configPath":".rpi/mcp.json","server":"playwright"}
```

Call a tool with `mcp_call`:

```json
{"configPath":".rpi/mcp.json","server":"playwright","tool":"browser_tabs","arguments":{"action":"list"},"timeoutSeconds":60}
```

Both tools also accept `url` instead of `configPath`/`server` for a direct HTTP
server. Loopback and private addresses are supported. HTTP redirects are disabled
so configured authentication headers are not forwarded to another endpoint.
Relative config paths resolve against the host project cwd. A stdio server starts
in that cwd unless `cwd` is configured; a relative configured cwd resolves against
the config file's directory. Environment settings are passed to the subprocess.

### Playwright extension screenshots

Playwright's action timeout is separate from the adapter's request timeout.
For slower screenshots, add `--timeout-action`, `15000` to the Playwright server
arguments and keep the outer `timeoutSeconds` at 60. Increasing only the outer
timeout does not override Playwright's default 5000 ms action timeout.

When using `--extension`, select the target tab before taking a screenshot:

```json
{"configPath":".rpi/mcp.json","server":"playwright","tool":"browser_tabs","arguments":{"action":"select","index":0},"timeoutSeconds":60}
```

Use the index returned by `browser_tabs` for the intended page. Selecting a tab
brings it to the front; a DOM snapshot can show updated state while a background
tab still has stale rendered content. Then call `browser_take_screenshot` with a
filename such as `.rpi/browser-output/lightbox.png`. In extension mode, a filename
may resolve against the workspace directory, even with `--output-dir` configured.

## Connections, bounds and cancellation

Connections are isolated by host session ID and canonical config path/server name
(or HTTP URL). Library callers without host context use a library-scoped fallback.
Initialization occurs once per connection. Config changes replace the connection.
Tool discovery refreshes before each call, so tool-list changes are reflected.
`mcp_list` with `action: "close"` releases an individual connection. Session shutdown
releases all connections in the extension instance. The host's reload flow must
finish old extension shutdown handlers before unloading DLLs.

The ABI executes on a background worker; `poll` is nonblocking. Cancellation and
per-request deadlines cover stdin writes, stdout reads, connection lock waits and
HTTP exchanges. The configured `timeout` defaults to 60 seconds; `timeoutSeconds`
overrides it for a call/list operation (1-300 seconds). Each handshake/list/call
request has its own deadline. Timed-out/cancelled stdio connections are stopped,
including their process tree and owned pipe workers. HTTP cancellations send a
best-effort cancellation notification. Failed transports are discarded and the
next invocation reinitializes; tool calls are never automatically replayed.

Requests/responses are limited to 16 MiB. Discovery is limited to 100 pages and
16384 tools, with repeated pagination cursors rejected. The connection registry is
limited to 64 entries; close unused entries to free capacity. The last 8 KiB of
server stderr is attached to stdio transport errors.

Streamable HTTP supports JSON and incremental SSE responses, initialized
notifications, negotiated protocol/session headers, session expiration and DELETE
cleanup. JSON-RPC responses are matched by ID. Server ping requests are answered;
other client requests return method-not-supported. Supported negotiated versions
are 2024-11-05, 2025-03-26 and 2025-06-18. Legacy HTTP+SSE endpoints (`/sse`) are
outside this adapter's scope; use a Streamable HTTP MCP endpoint (`/mcp`).

MCP `isError` results become host tool errors. Successful text and image blocks
cross the ABI as native host content. Other MCP blocks receive a text representation;
the original result, including structured content, is retained in result details.

## Scope and validation

The stable dispatcher avoids runtime tool registration. Resources, prompts,
sampling, elicitation, OAuth, and automatic dynamic tool registration are not
implemented. `exposure` is parsed for configuration compatibility; the stable
dispatcher does not implement Pi's dynamic exposure modes.

Run `cargo test -p rpi-mcp-adapter` and
`cargo clippy -p rpi-mcp-adapter --all-targets -- -D warnings`.
Tests exercise persistent processes, pagination, blocked writes, cancellation,
size limits, stderr diagnostics, HTTP JSON/SSE lifecycle, session headers,
expiration, ping replies and native images. The separate package smoke harness
also tests the built DLL through the real host ABI:

```powershell
$env:RPI_MCP_DLL = 'C:\path\to\rpi_mcp_adapter.dll'
$env:RPI_FAKE_MCP_SERVER = 'C:\path\to\fake_mcp_server.exe'
cargo test --manifest-path tools/rpi-package-smoke/Cargo.toml mcp_dll -- --ignored
```
