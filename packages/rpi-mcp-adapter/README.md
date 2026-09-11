# rpi-mcp-adapter

An HTTP JSON-RPC transport for MCP servers, inspired by the popular
`pi-mcp-adapter` package. It registers the `mcp_request` tool.

The tool accepts `url`, `method`, optional `params`, `id`, and `timeoutSeconds`.
It blocks credential-bearing and private-IP URLs, posts a JSON-RPC 2.0 request,
and returns the bounded response body.
