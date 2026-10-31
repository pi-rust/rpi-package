# rpi-mcp-core

Caller-owned MCP connections for Rust embedders, without a plugin SDK or ACP
protocol dependency. This crate is extracted from `rpi-mcp-adapter`; the adapter
continues to expose its existing four tools and library helpers.

## Ownership and request context

Each `McpRuntime` owns its connection registry, resource subscriptions and queued
updates. Two runtimes remain independent even with identical configuration paths,
server names and scope strings. Share an instance with `Arc<McpRuntime>` when
needed; `McpRuntime` itself is deliberately not `Clone`.

```no_run
use rpi_mcp_core::{Control, CoreRequestContext, McpRuntime};
use serde_json::json;

let runtime = McpRuntime::new();
let context = CoreRequestContext {
    scope: "session:example".into(),
    cwd: std::env::current_dir()?,
};
let catalog = runtime.list(
    &context,
    &json!({"configPath":".rpi/mcp.json", "server":"docs"}),
    &Control::default(),
)?;
let result = runtime.call(
    &context,
    &json!({"configPath":".rpi/mcp.json", "server":"docs",
            "tool":"lookup", "arguments":{"query":"ownership"}}),
    &Control::default(),
)?;
# let _ = (catalog, result);
# Ok::<(), Box<dyn std::error::Error>>(())
```

The embedding caller supplies scope and cwd. Core does not interpret `__rpi`,
read the SDK session environment, or remember an ambient active session. Relative
config paths resolve against the supplied cwd; configured relative server cwd
resolves against the config file. Convenience `*_from_config` instance methods
use the default library scope and process cwd; use the context-bearing methods
when managing multiple sessions.

`shutdown_connections()` cancels and drains the instance's current connections;
the instance can subsequently reconnect. It is not a permanent admission barrier
against other callers opening new keys concurrently. Dropping the final runtime
owner cleans up its process trees, pipe workers and notification dispatchers.
Closing one instance does not shut down another. Tool calls are never replayed
automatically after a connection failure.

## Resource updates

`take_resource_update_batch(scope)` returns a neutral delivery batch. Iterate
`updates()` to format messages in the embedding adapter. Dropping a batch accepts
or discards it; `retry()` restores only updates whose subscription lifetime and
read revision are still current, subject to the queue limit. A batch holds a weak
reference and does not keep the runtime or its child processes alive.

As in the existing adapter, a scoped take discards updates for other scopes. Use
one runtime per independently served session, or coordinate delivery boundaries
when intentionally sharing an instance. `take_resource_updates(scope)` provides
the non-retrying drain API.

Host custom-message formatting, turn-boundary timing, active-session environment
fallbacks and the decision to retry after a host switches sessions belong to the
embedding adapter, not this crate.

## Existing protocol behavior

- Persistent stdio and Streamable HTTP, initialized notification and negotiated
  protocol/session headers; supported versions: 2024-11-05, 2025-03-26, 2025-06-18.
- Bounded messages (16 MiB), 64 connection entries, 100 tool-discovery pages and
  16384 tools; repeated pagination cursors are rejected.
- Per-request timeouts and cancellation, including connection-lock waits and pipe
  operations. `Control` is caller-controlled; methods are blocking, so async
  embedders must arrange appropriate blocking execution and cancellation.
- Resource list/read/subscribe/unsubscribe, stdio notifications, subscription
  restoration and bounded lifetime-aware delivery retries.
- Redirects disabled for HTTP; failed calls are not automatically retried.

`ClientIdentity` lets an embedding adapter preserve its existing MCP clientInfo
and HTTP User-Agent independently of the core crate version. The legacy adapter
supplies its own name and version.

## Platform boundary and limits

The private `process` module owns OS containment. On Windows it creates a headless,
suspended child, installs a kill-on-close Job Object before resuming, and retains
cleanup ownership through all fallible setup. Unix uses a dedicated process
group. Tree termination does not depend on the leader still running.

The crate denies unsafe code by default. Only the private, documented platform
FFI module has an explicit exception; runtime and protocol modules retain the
deny lint. The core has **no plugin SDK/ABI dependency**, but is not claimed to be
entirely unsafe-free.

Cleanup depends on OS termination and pipe semantics. Escaped/inherited handles,
failed OS termination and permanently blocked OS I/O do not acquire a hard
shutdown deadline. HTTP resource push remains unsupported. OAuth, sampling,
elicitation and prompts are not implemented. This extraction does not connect
ACP `mcpServers` or change native tool registration.

## Validation

```text
cargo test --locked -p rpi-mcp-core -p rpi-mcp-adapter --all-targets
cargo test --locked -p rpi-mcp-core -p rpi-mcp-adapter --doc
cargo clippy --locked -p rpi-mcp-core --all-targets -- -D warnings
```

Adapter integration fixtures also exercise core instance isolation, explicit
context, process descendants, failed setup, owner drop and resource-delivery
compatibility. Windows tests cover actual child exit and controlled post-spawn
setup failures; they are not GUI validation or a claim that naturally failing
Win32 APIs were reproduced. Platform-specific runtime checks must be run on the
corresponding platform.
