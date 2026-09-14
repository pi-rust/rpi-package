# rpi-extension-rpc

Cross-platform client process manager for the native `rpi --mode rpc` JSONL
protocol. The package exposes two tools:

- `extension_rpc_server`: start one persistent `rpi --mode rpc` child and manage
  it with `action=start|status|list|stop`.
- `extension_rpc_client`: send a native `{id?, type, ...}` RPC command to that
  child and return its correlated response plus preceding events.

The server maps its parameters to rpi's reserved CLI options instead of taking
an arbitrary program and argv. It always inserts `--mode rpc`; supported
options include `provider`, `model`, `thinking`, `name`, `session`, `sessionId`,
`sessionDir`, `noSession`, tool/resource filters, and extension paths. It uses
the current `rpi` executable, so it does not depend on shell quoting or PATH
lookup on Windows, macOS, or Linux.

Start an ephemeral RPC session:

```json
{
  "action": "start",
  "noSession": true,
  "model": "openai/gpt-5",
  "thinking": "high"
}
```

Then pass the returned `id` to the client:

```json
{"serverId":"rpi-rpc-1","command":{"type":"get_state"}}
```

When `command.id` is omitted, the extension uses rpi's ABI-provided tool call
ID, preserving native RPC request/response correlation. The same child remains
alive across commands, so session state is retained. Records are strict JSONL,
bounded to 1 MiB, and requests have a configurable timeout.

This package is a client/process adapter; an extension cannot replace the
host's CLI mode dispatcher. The selected `rpi` executable must implement
`--mode rpc`. A build that only parses the reserved option and reports
`rpc mode is not implemented` is not sufficient and will return that error to
the client.
