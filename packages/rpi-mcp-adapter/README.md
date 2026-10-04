# rpi-mcp-adapter

An MCP client extension foundation for RPI, aligned with Pi's replaceable
MCP extension model. It keeps the original `mcp_request` compatibility tool and
adds package-owned MCP configuration, JSON-RPC, connection handshake, tool
listing/call request builders, and stdio/streamable HTTP transport primitives.

## Current scope

Implemented inside this package:

- Pi-style `mcpServers` configuration parsing;
- JSON-RPC 2.0 request/response/notification types;
- `initialize` and `notifications/initialized` helpers;
- `tools/list` and `tools/call` helpers;
- bounded HTTP transport with header and private-URL validation;
- stdio child-process transport with environment/cwd support and cleanup;
- MCP result-to-text conversion.

The host remains MCP-agnostic. The existing `mcp_request` tool accepts `url`,
`method`, optional `params`, `id`, and `timeoutSeconds`.

## Deliberate boundary

The current RPI ABI registers tools during extension registration and does not
provide runtime tool registration. Therefore dynamic Pi-style
`mcp__server__tool` registration is not claimed. This package now exposes a
stable `mcp_call` dispatcher:

```json
{"url":"https://example.com/mcp","tool":"read_file","arguments":{}}
```

The dispatcher sends an MCP `tools/call` request without adding MCP-specific
behavior to the host. It accepts either a direct HTTP endpoint or a Pi-style
config/server pair:

```json
{
  "configPath": "mcp.json",
  "server": "filesystem",
  "tool": "read_file",
  "arguments": {"path": "README.md"}
}
```

For configured servers it performs `initialize`, sends
`notifications/initialized`, calls `tools/list`, verifies the requested tool,
and then performs `tools/call`. Both configured HTTP and stdio servers are
supported. The package also exposes `discover_from_config`, which returns the
server info and discovered tool schemas for status/list integrations without
adding MCP behavior to the host. A future adapter layer can add per-server
dispatchers or use a future generic runtime-tool API.

Resources, prompts, sampling, elicitation, MCP OAuth, and dynamic
`tools/list_changed` registry updates remain follow-up work in this package.
