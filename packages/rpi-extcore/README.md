# rpi-extcore

Shared native-agent services for protocol adapters. This is a normal Rust library, not an installable rpi extension. It depends on neither Axum nor ACP nor the plugin ABI, and contains no frontend bundle.

The Web and ACP adapters use the same native RPC driver, request correlation, bounded command writer, child cleanup and run tracker. Native transcripts remain owned by the rpi host. Existing Web and ACP storage locations are preserved by their entrypoints.

## Use as a crates.io dependency

`rpi-extcore` is published as an ordinary Rust library. Depend on the `0.1`
minor line so compatible patch releases can be selected automatically:

```toml
[dependencies]
rpi-extcore = "0.1"
```

The crate requires Rust 1.78 or newer. It does not start a server by itself;
applications create one [`service::Service`] per backend and pass its `Arc` to
protocol adapters. The `test-support` feature is intended for adapter fixtures
and is not required by normal consumers:

```toml
[dev-dependencies]
rpi-extcore = { version = "0.1", features = ["test-support"] }
```

The public API is protocol-neutral, but it still expects a compatible native
`rpi --mode rpc` executable at runtime. The crate does not bundle that binary,
manage credentials, or provide an HTTP/ACP transport.

## Modules

| Module | Responsibility |
| --- | --- |
| `host` | Native child launch, ready validation, isolated requests, raw/projected subscriptions, runtime settings and shutdown |
| `run`, `message` | Prompt acceptance, authoritative settlement, cancellation and cumulative text/thinking reconciliation |
| `service` | Project/session operations, forks, clones, organization metadata, transcript snapshots, durable coordinator lifecycle and OAuth refresh |
| `events` | Replay boundary, event ordering, duplicate suppression and explicit resync requirements |
| `history` | Native transcript validation and active-branch recovery without client formatting |
| `dialogs` | Question deadlines and validated answers, including multiple choices and comments |
| `registry`, `projects`, `timing` | Durable supplemental metadata, project catalog and tool timings |
| `workspace`, `process` | Scoped file/Git access and background native processes |
| `reply`, `protocol` | Neutral operation outcomes and the existing native command contract |

## Reuse

Create one `Arc<Service>` per backend instance and pass clones to protocol adapters. Rust clients can call the same operations directly:

```rust
use rpi_extcore::service::{self, BackendConfig, Service};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
let core = Service::new(BackendConfig::default())?;
let projects = service::list_projects(&core).await;
assert!(projects["projects"].is_array());
core.shutdown().await;
Ok(())
}
```

`HostSession::spawn_options` supports explicitly configured executables, model/provider, permission policy, new sessions, exact saved files and forks. `begin_prompt` subscribes before sending, holds a run lease and returns a protocol-independent run tracker. Prompt acceptance is not completion; only authoritative settlement completes the run. Adapters map `RunOutcome` to their wire formats.

`HostSession::subscribe_raw` is an internal adapter feed, including native response frames. Request callers receive their original IDs; native request IDs are isolated internally. Slow live subscribers receive an explicit resync requirement rather than silently truncated success. ACP treats overflow of its prompt stream as a failed run and stops that backend, retaining its established behavior.

`durable::DurableCoordinator` contains the protocol-neutral state machine, while
`durable::DurableRuntime` owns its native branch-backed lifecycle. `Service` and
ACP both use the runtime for branch recovery, interrupted-run handling, event
observation, mutation persistence and append-failure rollback. Durable changes
also expose a bounded revision-based replay stream for adapters. It keeps tool
summaries and queue transitions bounded and never replays side effects. The
native host must advertise the two RPC commands in its ready capability list;
hosts without an explicit capability list safely remain ordinary non-durable
sessions. The branch remains the only durable source and no parallel database
or scheduler is created.

Sharing this library does not share memory between separate OS processes. Multiple clients must connect to the same service to share live sessions. Keep one backend writer per registry; independent service processes should use separate registry files.

The `test-support` feature supplies deterministic RPC fixtures for adapter tests and is enabled only through dev dependencies.

Versioning follows the repository's independent package policy. In the `0.x`
series, compatible additions and fixes use patch releases where possible;
breaking public API changes use a minor release. See the
[architecture and migration contract](../../docs/architecture/extension-core.md)
and the [changelog](CHANGELOG.md).
