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
| `service` | Project/session operations, forks, clones, organization metadata, transcript snapshots, session lifecycle and OAuth refresh |
| `events` | Replay boundary, event ordering, duplicate suppression and explicit resync requirements |
| `history` | Native transcript validation and active-branch recovery without client formatting |
| `dialogs` | Question deadlines and validated answers, including multiple choices and comments |
| `registry`, `projects`, `timing` | Durable supplemental metadata, project catalog and tool timings |
| `workspace`, `process` | Scoped file/Git access and background native processes |
| `reply`, `protocol` | Neutral operation outcomes and the existing native command contract |

## Project image reads

`workspace::image(&Project, &str)` and
`service::project_image(&Service, String, FileQuery)` return
`Result<workspace::ProjectImage, (ReplyKind, serde_json::Value)>`.
`ProjectImage { bytes: Vec<u8>, media_type: &'static str }` carries binary content
without a JSON or HTTP dependency. The service resolves the configured project
ID; the workspace reuses its existing project-relative path resolver, including
hidden-path and canonical symlink/junction containment checks.

Only regular files up to 20 MiB are accepted, with metadata checks before and
after opening and a limit-plus-one bounded read. PNG, JPEG (`jpg`/`jpeg`), GIF,
WebP and BMP extensions must match their raster signatures; SVG, HTML and
mismatched content are rejected. This is signature validation, not full image
decoding or sanitization. As with existing workspace reads, canonical resolution
and opening are separate operations: concurrently hostile local filesystem
mutation is not protected by a race-free descriptor-relative sandbox.

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

### Compact native message updates

Native hosts may advertise `ready.semantics.messageUpdates` as `compact-deltas`;
absence means the older `snapshot` mode. Compact text/thinking/tool-call deltas
retain `eventType`, `contentIndex`, `delta` and the nested event, but omit the
top-level cumulative message and nested partial. Structural events and final
`message_end` remain authoritative. This changes the default native wire format;
upgrade adapters and clients before upgrading the host, or explicitly request
`--rpc-message-updates=snapshot` as a temporary migration fallback.

The raw feed remains unchanged. The projected/replay feed supplements compact
text/thinking deltas with a live message assembled from structural snapshots so
existing Web row projection continues to stream. Compact tool deltas may also
carry the provider's current cumulative `id`/`toolName`; the projection uses
those identities without guessing, and updates arguments when their raw JSON
stream parses completely. While JSON is incomplete, it retains the last native
argument snapshot rather than fabricating a value. Initial block content
is retained, snapshots are never appended twice, and final metadata comes from
the host. This downstream snapshot projection is not a guarantee of linear SSE
traffic or lossless event reconstruction; compact native RPC and adapter/UI
projection have different contracts.

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
safely remain ordinary non-durable sessions. When those capabilities are present,
branch-query or replay errors propagate instead of silently disabling durable
state. `HostSession::branch_entries_of_types` makes one native request with a
non-empty `customTypes` array, requires `filtered: true`, and returns entries
without an unfiltered/full-transcript fallback. Durable projection queries only
`durable-state` and decodes the current journal format/version; raw-state and
legacy-discriminator reads are not supported. The branch remains the only
durable source and no parallel database or scheduler is created.

Sharing this library does not share memory between separate OS processes. Multiple clients must connect to the same service to share live sessions. Keep one backend writer per registry; independent service processes should use separate registry files.

The `test-support` feature supplies deterministic RPC fixtures for adapter tests and is enabled only through dev dependencies.

Versioning follows the repository's independent package policy. In the `0.x`
series, compatible additions and fixes use patch releases where possible;
breaking public API changes use a minor release. See the
[architecture and migration contract](../../docs/architecture/extension-core.md)
and the [changelog](CHANGELOG.md).

## Native run elapsed query (read-only)

`service::run_timings(&Service, id: String, query: RunTimingsQuery) -> ServiceReply`
reads persisted native v4 JSONL, including appends from a live native session,
without starting a host, watcher, registry writer or sidecar. The request type
is available at both `service::RunTimingsQuery` and
`run_timing::RunTimingsQuery`. HTTP route/extraction remains adapter-owned.

Request JSON: `{ "entryIds": ["committed-message-id"], "cursor": null }`.
Unknown fields are rejected. `entryIds` defaults to an empty list and accepts
at most 400 unique nonempty IDs, each at most 256 UTF-8 bytes. An empty list
returns no timings but still advances indexing. `cursor` is optional and at
most 128 bytes. Session IDs must be UUIDs and already known to this service.

Success is a `run_timing::RunTimingsPage` serialized as:

```json
{
  "timings": [{
    "runId": "native-operation-start-id", "lane": "main",
    "finalEntryId": "actual-committed-message-id",
    "startedAt": 1234, "finishedAt": 7890, "elapsedMs": 6656,
    "outcome": "completed"
  }],
  "complete": true, "cursor": "opaque-generation-token",
  "scannedBytes": 1042, "totalBytes": 1042
}
```

Timestamps are native signed Unix milliseconds; elapsed is a checked,
nonnegative `u64` difference, never message timestamps or client wall time.
Only matching `operation_started.id` (`intent.kind = run`) and
`operation_finished.runId` in the same lane generate a timing. The footer binds
to the last actual committed displayable user/assistant/toolResult message
within that interval (nonempty text/thinking, images and tool calls count), never
reserved IDs or an unchanged source leaf. `image_url` requires a nested nonempty
string `image_url.url`, matching the frontend's object shape.
Missing/reversed boundaries, no-output operations, navigation/compaction and
suspended unfinished runs produce no fabricated footer. Absent lane is treated
as `main` for backward fixtures; explicit lane strings remain isolated. Native
fork-import entries encode `lane: null` and do not count toward an active run or
replace its last entry. Explicit null lanes are ignored on records too (native
operation records require a lane string).

Each call advances at most 4 MiB / 4096 complete records. Lines up to 32 MiB
are assembled incrementally; an unfinished final line is not EOF-complete.
`scannedBytes` is the cumulative consumed file offset (including partial-line
bytes); `totalBytes` is that call's file-length snapshot. `complete` means all
complete records through that snapshot were scanned, **not** that a live run
settled or that future appends cannot occur. Continue/poll with the returned
cursor, even after `complete: true`; the entry selection may change freely.
Each reply includes all currently indexed matches for the current selection,
not only the newest scan's matches. No-cursor calls reuse a healthy cached index.
A generation cursor is returned even on complete pages.

The service owns a 16-session LRU, at most 20,000 completed timing records per
session and 128 active lanes. Only timing/active-run metadata is retained after
record processing; a bounded incomplete-line scratch buffer may temporarily
contain JSON content up to the 32 MiB line bound. After each completed line,
scratch capacity above 64 KiB is released; partial lines remain intact across
continuations. No message/tool body is written to disk. Two blocking
worker permits are held inside worker closures through cancellation; one cache
mutex serializes scans (including reads of the same file). Metadata capacity,
oversized lines and discovery caps return `LimitExceeded`, never silent eviction
of individual timing records or a fake complete result.

Resolution uses the known session's registered project, explicit
`BackendConfig.session_dir`, then native user `settings.json`'s `sessionDir`,
then `RPI_CODING_AGENT_SESSION_DIR`, then `<project>/.rpi/sessions`. Relative
roots are project-relative. User settings are read with a 1 MiB bound; settings
with comments/unsupported JSON return explicit `Unavailable` rather than risk
selecting the wrong file (configure `session_dir` to override). Flat files and
one subdirectory level are supported, with at most 10,000 directory entries per
initial discovery. Successful paths are cached for continuations; filename IDs
and bounded headers must match the exact UUID and native header version 4.
Canonical containment rejects transcript symlink escapes. Directory symlinks
are not traversed. No filesystem path is accepted from the query/session ID.

Error mapping: `InvalidInput` for invalid requests; `NotFound` for unknown
service sessions; `Unavailable` for missing/unsupported native storage or bad
headers/JSON; `AccessDenied` for scope escapes; `Conflict` for stale/evicted or
invalid cursors, duplicate transcripts, truncation or detected replacement.
**On Conflict, clients must clear accumulated timings and cursor and retry
without a cursor.** A stale cursor is rejected without removing or promoting a
healthy cached generation in the LRU. Once a cursor matches (or is absent), scan
errors, including detected file changes, remove the affected scan rather than
return stale timings.
Identity, size, modification and a trailing 4 KiB checkpoint detect ordinary
replacement/truncation. This is not a race-free local filesystem sandbox or a
full-file integrity audit: hostile concurrent rewrites outside the checkpoint
are not guaranteed to be detected. No deployment/restart is required or
performed by this API.
