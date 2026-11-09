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

## LAN discovery

`MdnsDiscoveryProvider` browses `_rpi-node._tcp.local.` and returns bounded,
normalized observations. It can also publish an HTTP endpoint using the same
mDNS daemon:

```rust,no_run
use rpi_node::{DiscoveryProvider, MdnsDiscoveryProvider, MDNS_SERVICE_TYPE};
use rpi_node_core::NodeIdentity;

let mdns = MdnsDiscoveryProvider::new()?;
let identity = NodeIdentity {
    node_id: "node-1".into(),
    owner_epoch: "epoch-1".into(),
};
let _advertisement = mdns.publish_http(&identity, 8080)?;
let observations = mdns.discover(MDNS_SERVICE_TYPE).await?;
```

The advertisement contains only the service type, HTTP port, discovery path,
node identity and owner epoch. It does not contain credentials or capability
payloads. The caller must keep the `MdnsAdvertisement` alive, fetch
`/.well-known/rpi-node` from each observation, and then apply normal HTTP
authentication and authorization. mDNS is a locator, not a trust decision.

The native `rpi --node` integration still owns when to create the provider
and which HTTP port to advertise; `publish_http` supplies a deterministic
`.local.` hostname, while `publish` accepts an explicit hostname. If mDNS is
unavailable, the node can continue to run with HTTP discovery or stdio
discovery only.
