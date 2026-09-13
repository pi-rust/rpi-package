# rpi-extension-rpc

Cross-platform JSONL RPC transport for extensions and external `rpi`/Pi
processes. The package exposes two tools:

- `extension_rpc_client`: start a program directly, send one JSON request over
  stdin, or connect to a server `address`, and collect bounded JSONL
  responses/events.
- `extension_rpc_server`: bind `127.0.0.1` and forward each JSONL request to a
  fresh program process. Use `action=start|status|list|stop` to manage servers.

Programs and arguments are passed as an argv array, so Windows, macOS and Linux
do not depend on shell quoting. The transport accepts LF and CRLF records,
limits each request/response to 1 MiB, and enforces a configurable timeout.

Example client parameters:

```json
{
  "program": "rpi",
  "args": ["--mode", "rpc", "--no-session"],
  "request": {"id": "1", "type": "get_state"},
  "timeoutSeconds": 30
}
```

The host must provide an RPC-capable program. This package does not modify or
patch the `pi-rust` CLI; it is a transport adapter that can be installed as a
normal Rust extension.

For the paired server, pass the returned address to the client:

```json
{"address":"127.0.0.1:43127","request":{"type":"get_state"}}
```
