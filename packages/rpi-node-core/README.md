# rpi-node-core

Transport-neutral contracts for nodes started by `rpi --node`.

This crate defines the stable vocabulary shared by a node host, node
extensions, and discovery/transport adapters. It deliberately does not own an
HTTP listener, authentication credentials, an Axum type, the plugin ABI, or a
workspace implementation.

## Discovery layers

The canonical network representation is an HTTP document at
`/.well-known/rpi-node`, following RFC 8615 well-known URI conventions. The
same `NodeReady` contract can be emitted over stdio when a parent process starts
`rpi --node`. mDNS/DNS-SD and registries are optional discovery providers; they
locate an endpoint but never grant access. The manifest carries multiple
protocol bindings rather than a single protocol string, so an extension can
advertise rpi-node, A2A, MCP, ACP or another open protocol without pretending
that their operation semantics are identical. Protocol-specific descriptors
may be retained as `ExternalDescriptor` raw data by an adapter.

`node_id` is a stable node/workspace identity. `owner_epoch` changes whenever a
node owner starts and prevents stale operations, event cursors, and connections
from being reused after restart.

## Boundary

`rpi-node-core` is the contract layer. `rpi-node` supplies the native host
lifecycle and protocol-adapter registry. A concrete implementation such as
`rpi-work` should provide a separate node extension crate and map its projects,
sessions, files, and Git operations into these contracts. MCP is modeled as a
tool/resource binding, A2A as a task/agent binding, and ACP as an agent-session
binding; none is treated as a replacement for the common node identity or
lifecycle model.
