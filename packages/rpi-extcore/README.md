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
| `compaction` (internal) | Native compact ACK/compaction identity/settlement correlation under the same foreground lease, with bounded cancellation and cleanup |
| `service` | Project/session operations, forks, clones, organization metadata, transcript snapshots, session lifecycle and OAuth refresh |
| `events` | Replay boundary, event ordering, duplicate suppression and explicit resync requirements |
| `history` | Native transcript validation and active-branch recovery without client formatting |
| `dialogs` | Question deadlines and validated answers, including multiple choices and comments |
| `registry`, `projects`, `timing` | Durable supplemental metadata, project catalog and tool timings |
| `workspace`, `process` | Scoped file/Git access and background native processes |
| `reply`, `protocol` | Neutral operation outcomes and the existing native command contract |

## Native compaction lifecycle

`Arc<HostSession>::compact(instructions)` returns the terminal native outcome,
not an asynchronous `accepted` acknowledgement. It subscribes before dispatch,
correlates the response and compaction ID, and retains the foreground run lease
through `agent_settled` (including `declined`). Legacy synchronous outcomes remain
supported. The default total deadline is 120 seconds; after submission,
cancellation or caller-drop owns abort and at most three seconds of settlement
drain before terminating an unconfirmed backend. Cancellation before queue
admission sends no abort and leaves the healthy host available. Presentation of
the outcome remains the adapter's responsibility.

## Opt-in native MCP catalog tools

`mcp::attach_with_options(host, config, McpOptions { catalog: true,
..Default::default() })` exposes resources and prompts through the native
host's generic external-tool mechanism. `catalog` defaults to **false**, retaining
existing tools-only behavior and descriptor counts. Only trusted callers should
enable it for explicitly configured servers after their own negotiation/policy;
it is not a model-controlled connection option.

Descriptors are gated on each connection's actual initialize capabilities:
`resources_list`, `resource_templates_list`, `resources_read`, `prompts_list`, and
`prompts_get`. Native names are deterministic, hashed, and domain-separated from
server-selected actual tool names. All tools together retain the existing
128-descriptor/256 KiB definition bounds. Model arguments cannot override the
configured server, config file, cwd or scope. Lists fetch one bounded page and
retain `nextCursor`; returned tool results are limited to 256 KiB without silent
truncation. Core operations retain cancellation and catalog operation deadlines.

Resource values and complete prompt messages (including roles, images, blobs and
metadata) are serialized as JSON **data in tool-result text**, not appended as
system instructions or executed as a new foreground prompt. Native permission
checks remain in the native external-tool path. Subscriptions and automatic
resource-update transcript append are deliberately **not advertised**: there is
no idle/generation-safe native append implementation in this integration.

`tests/mcp_catalog.rs` exercises mock HostSession external requests against real
stdio and loopback HTTP MCP peers. It does not establish real native/model
execution; that requires a separate native model fixture.

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

`rpi-durable-core` contains the protocol-neutral canonical schema and journal
replay. `durable::DurableRuntime` is only a branch-backed projection: it reads
`durable-state`/`rpi-durable-journal`, forwards mutations through the registered
`rpi-durable` command, and publishes bounded in-memory changes. It never
appends durable entries and never performs interrupted-run recovery. Lifecycle
events are not folded into this projection; adapters refresh at attach and
after a command commit, while a periodic branch watcher observes canonical
commits made by another adapter/process. This prevents ordinary Agent events
from becoming a second durable fact source. The native harness owns Agent execution recovery;
`rpi-durable` owns workflow/business state. The native host must advertise
`get_session_branch` and `execute_command`; hosts without those capabilities
safely remain ordinary non-durable sessions. The branch remains the only
durable source and no parallel database or scheduler is created.

Sharing this library does not share memory between separate OS processes. Multiple clients must connect to the same service to share live sessions. Keep one backend writer per registry; independent service processes should use separate registry files.

The `test-support` feature supplies deterministic RPC fixtures for adapter tests and is enabled only through dev dependencies.

Versioning follows the repository's independent package policy. In the `0.x`
series, compatible additions and fixes use patch releases where possible;
breaking public API changes use a minor release. See the
[architecture and migration contract](../../docs/architecture/extension-core.md)
and the [changelog](CHANGELOG.md).
