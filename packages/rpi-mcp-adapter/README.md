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
`mcp__server__tool` registration is not claimed yet. The next adapter layer will
use a stable per-server dispatcher or require a future generic runtime-tool API;
it will not add MCP-specific behavior to the host.

Resources, prompts, sampling, elicitation, MCP OAuth, and dynamic
`tools/list_changed` registry updates remain follow-up work in this package.
