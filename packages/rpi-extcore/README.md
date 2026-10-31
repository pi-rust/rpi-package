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

`history::saved_session_metadata` validates the authoritative native v4 header
and exact ID using a 64 KiB first-line limit without reading messages or
persisting a sidecar. ACP uses this neutral metadata for nonempty-MCP fresh
load/resume preflight: existing recorded and requested workspaces must resolve
to the same directory, otherwise reject before spawning. Empty/missing recorded
directories retain native fallback; no-MCP compatibility remains unchanged.
This metadata snapshot is not a concurrent-filesystem sandbox.

## Read-only session discovery

`session_catalog::SessionCatalog::new(root, CatalogLimits::default())` is a
protocol-neutral, caller-owned catalog; construction performs validation only.
`page(CatalogQuery { cwd, cursor, limit }, CatalogControl)` advances a bounded
scan or returns a frozen page. Call `shutdown().await` to cancel and drain owned
workers. Dropping a page cancels its control without releasing admission early;
catalog drop cancels workers, which retain their cleanup owner until exit.

The explicit root contains `UUID/encoded-cwd/timestamp_UUID.jsonl`. Discovery
validates canonical lowercase UUID, native v4 header identity, creation timestamp,
filename and encoded absolute recorded cwd. Only the first line (64 KiB including
newline) is parsed; reads can include up to 1,023 discarded body bytes of bounded
read-ahead, charged to actual-read budgets. There are no body/tail parses, native
processes, writes, registry overlays or credential configuration. Matching
malformed/duplicate candidates fail; unrelated entries still consume budget.
Subdirectory symlinks and Windows reparse points are rejected; the configured
root itself may canonicalize. Metadata/open checks are separate operations, not
a hostile concurrent-filesystem sandbox. A cwd filter is an existing absolute
directory, compared canonically, not through blanket Windows lowercasing.
Missing recorded directories remain discoverable without a filter.

Defaults cap each step at 256 entries / 4 MiB actual reads / cooperative 250 ms;
a generation at 10,000 entries / 64 MiB reads / 16 MiB summary accounting /
30 seconds cumulative active work. These generation budgets fail with
`LimitExceeded`, never truncated success; caller cancellation/deadline have
separate errors. Header scratch, cursor/vector overhead are additionally bounded
by entry/path limits, not an exact allocator/RSS measurement. At most two workers
and two generations are admitted, without an unbounded wait queue. Synchronous
kernel I/O cannot be hard-preempted.

Incomplete scans return **empty sessions with a continuation**. Completion
freezes summaries sorted by `created_at_ms` descending then UUID ascending;
pages allow 1..=100 rows. Cursors are opaque, process-local and bound to canonical
cwd and limit. Scan tokens are single-use (even the one finishing discovery);
frozen-page tokens are retryable. Idle expiry is checked on admission after
60 seconds, and a fresh generation may evict the oldest nonbusy generation.
Stale/foreign/changed-filter tokens fail explicitly. This is observed discovery,
not a transactional filesystem snapshot; a listed session may later fail to load.
`modified_at_ms` is observed filesystem mtime, not authoritative last activity;
no title is invented from message text.

## Live runtime configuration

`configuration::ConfigurationRuntime::try_attach(Arc<HostSession>)` shares one
observer per host and returns `None` when `get_state` or `get_available_models`
is not advertised. Advertised malformed replies are errors, not legacy fallback.
`get_available_thinking_levels` is optional; without it, thinking is absent.
The neutral snapshot contains model identity/catalog, optional native thinking
current/choices and a monotonic revision. It introduces no ACP types or persisted
configuration, and does not acquire the prompt/run lease.

Polling starts after 2 seconds, uses a 2-second interval and skips missed ticks.
Each refresh or setter-plus-readback has one 10-second total budget, including
waiting for the shared mutation/refresh gate. Initial reading also has a
10-second budget, but waiting for the attach-owner lock is outside that budget.
Reads bracket the catalog and optional thinking choices with `get_state`, with
at most two consistency attempts. This detects ordinary changes, not an atomic
native transaction or ABA. Watch delivery coalesces to the latest state, not an
audit log; equal snapshots do not increment revision. Poll errors retain the
last good snapshot. Setter failure/readback timeout is not rollback: native
state may already have changed.

Validation caps catalogs at 1024 models, thinking choices at 32 and individual
strings at 4096 UTF-8 bytes. Native thinking current may legitimately be outside
its choices; core retains it faithfully and adapters decide representability.
`shutdown().await` signals cancellation and joins the owned poll worker; a
cancelled shutdown can be retried. Caller-owned refresh/setter futures observe
cooperative cancellation, rather than being joined by shutdown. Host closure
stops observation; owner drop cancels/aborts as a non-async fallback, with weak
host ownership to avoid cycles.

Fixture coverage lives in `src/configuration_tests.rs`; run
`cargo test --locked -p rpi-extcore --lib configuration::tests`.
This is fixture coverage, not real-native execution or installed-host validation.

## On-demand context usage

`HostSession::context_usage()` returns an optional neutral `ContextUsage` from
native `get_state.context`; `context_usage::parse_context_usage` also accepts an
already-read native state. Required used/window integers are validated strictly;
missing context or a zero/unknown window means no data, not zero usage. A known
context requires a nonempty native session ID (maximum 4096 UTF-8 bytes).
Native estimated/trailing provenance is retained when present. Valid usage above
the window is not clamped. No model-catalog dependency, monetary units, cumulative
stats, transcript read, persistent store or adapter types are introduced.

Each host admits one usage read with an immediate try-lock, no waiting queue.
The existing correlated native command path has a 5-second total caller budget,
including native writer backpressure, and host-closure cancellation. Caller drop
releases admission and correlation; late responses cannot settle a new read.
No run lease or background worker is acquired. A host not advertising `get_state`
is unsupported without a request; malformed advertised replies are errors.

This is native's compaction-aware current-context **estimate**, not a tokenizer
or a sum of historic token counters. `estimated: false` can still include
estimated trailing messages. Native `get_state` internally reads the full branch
and supplemental records; the bounded caller wait is not a bounded native
algorithm or a hard interruption of its work. Adapters choose emission timing,
identity matching, presentation coalescing and wire error handling.

## Modules

| Module | Responsibility |
| --- | --- |
| `host` | Native child launch, ready validation, isolated requests, raw/projected subscriptions, runtime settings and shutdown |
| `run`, `message` | Prompt acceptance, authoritative settlement, cancellation and cumulative text/thinking reconciliation |
| `service` | Project/session operations, forks, clones, organization metadata, transcript snapshots, session lifecycle and OAuth refresh |
| `events` | Replay boundary, event ordering, duplicate suppression and explicit resync requirements |
| `history` | Native transcript validation and active-branch recovery without client formatting |
| `session_catalog` | Caller-owned bounded native-header discovery, continuation and frozen summary pages |
| `configuration` | Shared bounded model/thinking observation, revision watch and serialized setters/readback |
| `context_usage` | Typed native context estimate parsing and single-flight, lease-free on-demand reads |
| `dialogs` | Question deadlines and validated answers, including multiple choices and comments |
| `registry`, `projects`, `timing` | Durable supplemental metadata, project catalog and tool timings |
| `workspace`, `process` | Scoped file/Git access and background native processes |
| `reply`, `protocol` | Neutral operation outcomes and the existing native command contract |

## Session-owned MCP tools (stdio and Streamable HTTP)

`external_tools::ToolServer::{Stdio, Http}` contains `StdioToolServer` or
`HttpToolServer { name, url, headers }` (`headers` is a `BTreeMap<String, String>`).
`HostSession::configure_tools(servers, cwd, scope)` accepts this neutral API;
`configure_stdio_tools` remains a compatibility wrapper. No ACP types,
temporary configuration files or configuration persistence are introduced.
Configuration is intentionally not `Debug`.
The caller must provide an absolute working directory and a scope. Empty initial
configuration works with older native hosts; nonempty configuration requires
both native commands `configure_external_tools` / `external_tool_response` and
events `external_tool_request` / `external_tool_request_closed` before any MCP
child starts or HTTP endpoint is contacted. Identical explicit live configuration
(including cwd/scope, with HTTP header names lowercased after duplicate checks)
is accepted; changed live configuration requires closing and reopening the session.
ACP handles only its
wire validation/mapping: omitted live configuration retains the existing one,
while a fresh load/resume requires reproviding configuration and credentials.
Configuration secrets (arguments/environment, URLs/opaque queries and headers)
are not persisted in transcripts, registry metadata or another database.
The neutral API bounds configuration to 16 servers
and 256 KiB encoded configuration.

HTTP targets use `ConfiguredHttp`, `discover_http` and `call_http`; stdio uses
the corresponding typed APIs. Both freeze live-generation admission: calls do
not requery, reconnect or replay after transport/RPC failure, cancellation or
HTTP 404. A result with `isError` is still an MCP result. HTTP requires an
absolute HTTP(S) URL without userinfo, fragment or controls, supports static
Authorization/custom headers, rejects case-insensitive duplicates and reserved
transport/hop-by-hop headers, and disables ambient proxies and redirects.
See the [core HTTP contract](../rpi-mcp-core/README.md#in-memory-streamable-http-targets-tools-only)
for the full header policy. This is trusted-client outbound authority, including
local/private HTTP, not an SSRF sandbox; prefer HTTPS. Legacy MCP APIs are unchanged.

Configuration holds the run lease. Discovery has one 30-second total deadline,
then all definitions are installed by one atomic native configure command before
publishing the session-owned bridge. Native rejection leaves no candidate owner;
cancellation after sending registration fails the host closed because commit is
otherwise ambiguous. Discovery drop cancels its `Control` and retains the blocking
worker join before runtime shutdown. No raw MCP/native errors containing server
configuration are forwarded by this bridge.

Native names are `mcp_` plus the first 60 hexadecimal SHA-256 digits of a
length-delimited `(server name, tool name)` tuple. Original labels appear only
in bounded descriptions. Schemas must already have an object root with
`type: object`; they are not silently rewritten. Native schema restrictions still
apply. Registration is limited to 128 tools / 256 KiB, descriptions to 8 KiB,
active calls and the reliable reader queue to 64, calls to 45 seconds, and native
outcome frames to 1 MiB. Queue overflow fails the connection closed, not just its
broadcast subscribers. Raw text/image results, `isError`, `structuredContent`,
and unknown metadata are retained in raw `externalToolResult` for native
validation/projection; unsupported content blocks produce explicit errors rather
than disappearing. Native rpi owns tool/schema validation, call provenance,
result projection and persisted history. ACP separately formats native events
and replay; it is not the native result projector. Runtime scope is explicit and
per-session, independent of the deterministic 64-character exported tool name.
This tools-only path does not add resources, prompts, sampling, OAuth, legacy
SSE/GET streams, plugin activation, remote attachment or shared service deployment.
POST response-body SSE is supported; no native or Web implementation change is
implied.

`HostSession` owns the bridge; reader/runtime routing is weak. Native closures
synchronously cancel calls and guarded queued output. Abort cancels current calls
without disposing the configured runtime (a cancelled MCP target needs explicit
fresh discovery before it can be used again; calls never automatically reconnect
or replay). Finish, termination, shutdown and owner
drop cancel the bridge. A separately retained supervisor joins all bounded call
workers and shuts down the MCP runtime on a blocking cleanup worker; it never
retains the session or bridge, avoiding owner cycles. Output already committed
to an OS pipe cannot be recalled, and cleanup inherits the core/platform limits
of terminating child processes and joining blocking I/O. HTTP cancellation
notification (200 ms) and session DELETE (1 second) are best-effort per-connection
request timeouts, not a global shutdown deadline, guaranteed remote cleanup or
undo of an already executed tool.

## MCP validation

Default package tests cover host registration/routing and lifecycle semantics.
Run `cargo test --locked -p rpi-extcore --features test-support --test mcp_stdio_bridge`
for real MCP shell-child cancellation, timeout, result and owner-drop fixtures.
Their native peer is in memory: these are not real-native smoke tests.
`cargo test --locked -p rpi-extcore --features test-support --test mcp_http_bridge`
adds loopback Streamable HTTP fixtures; default MCP core tests include
`configured_http_tests`. The three-OS CI runs these without a real-native binary.
The separate provenance-gated Windows Python harnesses are described in the
[ACP README](../rpi-acp/README.md#mcp-validation-and-native-smoke). Inspect their
retained diagnostic results for actual execution evidence. A source test or
configured CI step is not evidence that this native smoke passed; a successful
run qualifies only its recorded Windows source and binary hashes.

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

`begin_compaction(Option<String>)` requires the native `compact` command and
uses that same lease, subscription and cancellation path. Its `RunKind::Compact`
tracker waits for correlated acceptance then `agent_settled`, without requiring
compaction-specific intermediate events. It emits the optional terminal summary
once as a neutral text delta (or `Nothing to compact yet.` for a declined result
without a summary). Compacted/declined map to `Completed`, aborted to `Cancelled`,
and failed/unknown outcomes to errors. After a locally requested cancellation,
a failed settlement instead maps to `Cancelled` without a failure-summary delta
(native can report provider abortion as failed); acceptance and authoritative
settlement are still required. Prompt tracker behavior, run timeouts and shutdown
ownership are unchanged.

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

## Bounded active-branch replay preparation

`history::page::{BranchAnchor, MessagePage, HistoryRead, HistorySpool}` provides
protocol-neutral replay preparation. `HistoryRead::begin(Arc<HostSession>)` owns
the existing prompt/run lease until the adapter drops it after replay. It
subscribes before capturing a consistent retained event sequence boundary.
`prepare(saved_path)` validates every page and rechecks both the native anchor
and event boundary before returning a disk-backed oldest-first page stack.
Any intervening event requires retry; it is not silently merged into history.

Hosts advertising `get_messages` receive at most **128 messages** per RPC;
each serialized page is limited to **8 MiB**. Required entry IDs are nonempty,
at most 1024 UTF-8 bytes and unique throughout traversal. Cursor echo,
exclusivity, full intermediate windows, first/last IDs, progress, total count
and leaf anchoring are validated. Bad advertised replies never select fallback.
Preparation has a **30-second** budget beginning at `HistoryRead::begin`; each
RPC gets the lesser of **15 seconds** and the remaining preparation budget.
Deadline/cancellation checks also surround parsing and bounded synchronous
spool operations; blocking filesystem calls are not forcibly preempted.

The random `create_new` temporary spool uses Unix **0600** creation permissions
(and inherited temporary-directory ACLs on Windows). It holds at most **512 MiB**,
including eight-byte page trailers, and is closed/removed on error, cancellation
or owner drop. Only a page and entry-ID validation metadata are kept in memory
for native paging, not the full transcript or a growing spool-offset index.

Only hosts lacking the capability use the saved native transcript fallback.
The actual open file is read with limit-plus-one reads: **64 MiB per file** and
**8 MiB per JSONL line**, including line delimiters. This path reuses the native
branch validator, checks the leaf before/after reading, then builds the same
bounded spool. Its in-memory parsed entries/messages are bounded by that file
limit; it never calls the old unbounded file helper. These are history-layer
limits, not a new global native RPC frame-size limit or hostile-filesystem
sandbox. The native transcript remains the sole persisted source of truth.
