# rpi-node

Native host-side lifecycle for the `rpi --node` mode.

This crate intentionally does not contain an HTTP server. It loads a node
extension, owns the node identity and owner epoch, exposes a validated
`node_ready` frame, and delegates transport encoding to adapters such as
`rpi-httpcore`. `encode_ready_line` and `decode_ready_line` provide the bounded
JSONL stdio framing used by a parent process. The protocol adapter registry is
intentionally open-ended:
A2A, MCP, ACP and future adapters can be registered without adding another
`rpi --...` host command. Discovery mechanisms are registered separately
through `DiscoveryProvider`; the host does not assume that HTTP, stdio, mDNS or
an operator registry is the only source of observations.

The native host should write `NodeHost::ready_line()` as the first stdio
frame, route all protocol adapters through the same host instance, and call
`NodeHost::shutdown()` after transport drain. A Work implementation belongs in
a separate extension crate and should depend on `rpi-node-core`, not on this
host crate's internal state.
