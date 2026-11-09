# rpi Node Core and protocol bindings

`rpi --node` is a host mode, not a new Agent tool extension. The host owns
node identity, owner generations, extension startup, discovery publication and
shutdown. A concrete extension owns its domain, such as the Work workspace.

```text
rpi --node
├── rpi-node-core   contracts and neutral coordination semantics
├── rpi-node        host lifecycle and protocol-adapter registry
└── extension       Work, another workspace, or another node domain
```

## Discovery is layered

There is no single universal network discovery protocol. The implementation
uses a common document and permits multiple providers:

| Layer | Mechanism | Purpose |
| --- | --- | --- |
| Known URL | RFC 8615 `/.well-known/rpi-node` | Retrieve authoritative metadata from an endpoint already known to the caller |
| Local child | JSONL `node_ready` | Tell a parent process that `rpi --node` is ready |
| LAN | mDNS/DNS-SD `_rpi-node._tcp` | Locate candidate endpoints on a trusted local network |
| Managed network | DNS-SRV or a registry | Locate nodes in an operator-managed environment |
| Foreign protocol | A2A Agent Card, MCP metadata, ACP handshake | Discover a binding that another open protocol understands |

The providers produce a normalized observation but do not grant access. A
client still authenticates and negotiates effective capabilities. Discovery
metadata never contains passwords, cookies, QR tickets, model credentials,
project listings or session content.

## Multiple protocol bindings

A node does not have one mandatory application protocol. Its manifest can carry
multiple bindings:

- `rpi-node` for common identity, lifecycle, operations and event semantics;
- Work's domain protocol for projects, sessions, files and Git;
- A2A for agent tasks and artifacts;
- MCP for tools/resources/prompts;
- ACP for agent sessions, commonly over stdio.

The common model keeps the protocol source on each capability. An A2A task is
not silently presented as an MCP tool call, and an MCP server is not silently
presented as a Work workspace. Adapters may expose a normalized operation only
when they can prove the mapping; unknown capabilities remain unknown.
`ExternalDescriptor` retains foreign descriptors for protocol-specific
consumers without making their schemas part of Node Core.

## Native `rpi --node` host integration

The native host remains the only owner of the `--node` command. Its integration
with this package is intentionally small:

1. load/select a concrete node extension;
2. load the stable `node_id` and create a fresh `owner_epoch`;
3. call `NodeHost::start` (or `start_with_adapters`);
4. write `NodeHost::ready_line()` as the first stdio JSONL frame when the parent
   selected stdio transport;
5. route protocol-specific requests through the registered adapter and the
   host's validated query/mutate/subscribe methods;
6. stop accepting new work, drain transports, and call `NodeHost::shutdown`.

The native host must not create one Work owner per protocol. A2A, MCP, ACP and
HTTP adapters all share the same extension instance and owner epoch. The
`rpi-node` crate supplies these lifecycle primitives; it does not add a second
plugin ABI or a second `rpi --...` command.

## Identity and lifecycle

`node_id` is stable for one node/workspace. `owner_epoch` changes on every owner
start. Operation references and event cursors include both, so a restart
cannot accidentally accept a stale operation or replay cursor.

Discovery is not identity proof: mDNS, a URL and a QR address can all be
spoofed or copied. Authentication, TLS/host checks and any protocol-specific
trust or signature rules remain separate adapters.

## Boundaries

`rpi-node-core` has no Axum, ACP, frontend, plugin ABI or workspace type.
`rpi-node` has no HTTP implementation; it composes transport adapters.
`rpi-httpcore::node_discovery_router` is the thin RFC 8615 JSON adapter and
must be composed with the deployment's Host/Origin and authentication policy.
`rpi_routes_with_node_discovery` provides that composition for the existing
HTTP service router without moving node semantics into `rpi-extcore`.
HTTP and WebSocket serialization remain in the HTTP adapter boundary. A future
Work extension should depend on `rpi-node-core`, create one Work owner, and
never create a second owner for each transport.
