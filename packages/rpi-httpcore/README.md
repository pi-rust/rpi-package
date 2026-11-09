# rpi-httpcore

HTTP/SSE protocol adapter library for `rpi-extcore`. It provides a reusable Axum router and an API-only executable. It contains no frontend resources and does not depend on the plugin SDK.

The `rpi-http` extension crate is a separate package: it maps the native plugin ABI (`rpi --http`) onto this library. Depend on `rpi-httpcore` for the server, router, configuration and security contract.

## Standalone service

From the repository root, with the native host installed:

```bash
cargo run --locked -p rpi-httpcore -- --project /path/to/project --rpi-bin /path/to/rpi --port 3030
```

This serves `/api/v1`, including projects, sessions, commands, snapshots, SSE, model/provider operations and file/Git previews. `/` and `/assets/*` return 404. Use `rpi --webserver` from the `rpi-web` extension when you want the bundled browser workspace.

`--help` lists the supported options. `--port 0` selects an available port and startup output shows the actual address. An explicit JSON file is optional:

```json
{
  "project": ".",
  "rpiBin": "/absolute/path/to/rpi",
  "port": 3030,
  "registry": "state/sessions.json",
  "sessionDir": "history",
  "admissionLimit": 64
}
```

```bash
cargo run --locked -p rpi-httpcore -- --config /path/to/http.json --port 3031
```

JSON paths are relative to the file, except a bare executable name, which is resolved on PATH. CLI options override JSON regardless of their order. `sessionTimeoutSeconds` optionally releases idle native hosts while preserving their saved sessions; `0` disables it. This new standalone entrypoint loads only an explicitly selected configuration file; the existing `rpi-web` global/project configuration discovery remains in Web.

## Authentication and compatibility

Use `RPI_HTTP_PASSWORD` for access authentication and `RPI_HTTP_ALLOWED_HOSTS` for allowed Host authorities. Both fall back to their legacy `RPI_WEB_*` names. The Web entrypoint keeps `RPI_WEB_*` precedence.

The existing `/api/v1` payloads, cookie authentication, `X-Rpi-Web: 1` mutation marker, SSE event names/IDs and `Last-Event-ID` behavior are preserved. Log in with `POST /api/v1/auth`, retain its cookie, and include the mutation marker on POST/DELETE requests. Secure cookies follow HTTPS Origin or trusted reverse-proxy `Forwarded`/`X-Forwarded-Proto` signals while remaining usable for direct local HTTP. Protected owner mutations perform a second `authorize_before_commit` check and suppress a successful response if the Lease was revoked while the operation was running. Model API credentials remain separate and are handled by the native host.

The public `access` module contains transport-neutral lifecycle primitives for HTTP/SSE/WebSocket owners: revocable `AuthLease` values, sharing generations, `Admission` permits and drain, and one-time generation-bound `PairingStore` tickets. The public `transport` module adds Axum adapters for protocol-neutral connection admission, bearer authentication, Host/Origin and mutation-marker checks, Lease-bound invalidation, authorized event streams, connection/message limits and bounded outbound queues; it does not define Work frames or operations. `transport::authorized_stream(...)` guards any owner-defined SSE/event stream without imposing event payload or replay semantics. Compose `same_origin_router`, `bearer_auth_router`, `lifecycle_router` and an owner-specific WebSocket handler for a new adapter. `TransportState::with_pairing_store(...)` attaches an optional pairing store whose issuance is stopped and tickets are cleared at the beginning of `drain_with(...)`. The host can then use the callback to await owner-specific cleanup after transport connections and admitted operations have drained. Authenticated SSE connections now bind to their Lease and stop on logout, expiry or generation revocation. The standard API router also uses a configurable admission gate (`--admission-limit`, default 64) and drains admitted requests before backend shutdown; saturated requests receive a sanitized 429 and shutdown rejects new operations with 503. The access layer has no Work or Axum dependency, while the transport adapters have no Work or concrete rpi service-owner dependency; the current router continues to expose the standard `rpi-extcore::Service` API.

For compatibility the default registry remains `~/.rpi-web/sessions.json`. Run one backend writer per registry. Separate standalone instances can select their own `--registry` paths.

## Project images

`GET /api/v1/projects/{id}/image?path=artifacts/example.png` returns raw image bytes
with the detected raster MIME type, rather than the JSON text preview returned by
`/file`. It uses the same authentication and Host/Origin checks as other project
reads. Responses, including failures, have `Cache-Control: private, no-store` and
`X-Content-Type-Options: nosniff`.

Paths are relative to the configured project. Canonical containment and hidden-path
rules are enforced by core. Only regular files of at most 20 MiB are accepted;
PNG, JPEG, GIF, WebP and BMP require matching extensions and signatures. SVG,
HTML, traversal and files outside the project are not served. This does not add
an arbitrary file server, external-image proxy or replacement `/artifacts` root.

The Web client maps local raster image references to the selected project's image
endpoint. This requires a backend containing this route; restarting an older
backend without upgrading it will not add image support.

## Run timings

`POST /api/v1/sessions/{id}/run-timings/query` reads persisted native v4 run
elapsed metadata for a known session without starting a native host. It uses the
same cookie authentication, Host/Origin checks and `X-Rpi-Web: 1` marker as other
POST endpoints.

```json
{"entryIds":["assistant-entry-id"],"cursor":null}
```

`entryIds` accepts at most 400 unique, nonempty IDs; `cursor` is optional and
unknown fields are rejected. The response contains `timings` (each with `runId`,
`lane`, `finalEntryId`, `startedAt`, `finishedAt`, `elapsedMs`, `outcome`),
`complete`, `cursor`, `scannedBytes` and `totalBytes`. If `complete` is false,
repeat the query with the returned cursor to continue the bounded scan. A 409
invalidates accumulated timings and the cursor; retry without the cursor.
Invalid requests return 400 and unknown session UUIDs return 404.

## Read-only Code Mode child queries

These additive routes inspect an **already opened, live** session through the
core's typed child services. Each accepts `Content-Type: application/json`:

| POST route | Example JSON body |
| --- | --- |
| `/api/v1/sessions/{id}/tool-program/children/query` | `{"direction":"newestFirst","limit":8,"maxBytes":32768}` |
| `/api/v1/sessions/{id}/tool-program/phases/query` | `{"childId":"tpchild-01234567-89ab-4cde-8fab-0123456789ab","direction":"newestFirst","limit":8,"maxBytes":32768}` |
| `/api/v1/sessions/{id}/tool-program/body/query` | `{"childId":"tpchild-01234567-89ab-4cde-8fab-0123456789ab","phaseEntryId":"tpphase-01234567-89ab-4cde-8fab-0123456789ab","descriptorId":"native-descriptor-id","limit":2,"maxBytes":65536}` |
| `/api/v1/sessions/{id}/tool-program/snapshot` | `{"children":{"direction":"newestFirst","limit":8,"maxBytes":32768},"phaseLimit":2}` |

Replace example IDs with native IDs from prior responses. Page queries optionally
accept `leafId` and a typed `cursor`; pass the returned `nextCursor` unchanged,
retaining direction and target. Summary/phase direction is `oldestFirst` or
`newestFirst`; body cursors always traverse ascending chunk indexes. Snapshot
accepts only a fresh newest-first window, without a supplied leaf or cursor.
Unknown fields (including nested cursor fields), null bodies, malformed JSON and
invalid types are rejected, not defaulted. Query bounds and cursor binding are
validated by core.

Although these POSTs are read-only, they retain the **same cookie authentication,
Host/Origin checks and `X-Rpi-Web: 1` marker** as other POST routes. Log in through
`/api/v1/auth` first; omission of the marker is 403, not a read-only exemption.
The existing JSON extractor body limit (2 MiB) is unchanged. Extraction failures,
including oversized input, return a sanitized 400 without echoing supplied data.

Summary and phase requests require explicit `limit` (1–32) and `maxBytes`
(1–131072, complete encoded page). Phase scans consume at most 128 candidates;
an empty page with continuation is valid. Body requests allow 1–8 chunks and
1–65536 decoded bytes per page. Snapshots select 1–8 children and 1–8 phases per
child, have a 128 KiB complete-result budget and a 30-second total core deadline.
There is no hidden pagination loop or body hydration in a summary/snapshot.

Success is 200 with the typed core result, without an adapter envelope. Core
errors retain the existing mapping: invalid query 400, unknown/not-live session
404, stale/conflicting snapshot or cursor 409, exceeded result budget 413,
unsupported native capability 501, malformed/unavailable host reply 502, timeout
504. An old or mismatched live host lacking the exact child query profile is
unsupported; snapshots additionally require the child event profile. These reads
do not open a session, execute a tool, scan fallback history or create another
transcript store. Native transcripts remain authoritative.

A summary's `state: "unknown"` and `scanExhausted` never imply lifecycle success.
Snapshot observations report only validated final/closed facts; historical fork
origins do not imply destination execution. Body pages verify returned chunks,
not unread chunks or whole-body integrity; aggregate verification requires all
pages. Snapshot `startSeq`/`lastSeq`, replay metadata and `resyncRequired` are
serialized unchanged from core. Existing ordinary snapshot and SSE routes are
unchanged; the child feed below has its own boundary.

### Dedicated child SSE

`GET /api/v1/sessions/{id}/tool-program/events` creates a caller-owned core
subscription for eight newest children / eight phases each. It sends one bounded
`tool_program_child_snapshot`, then validated `tool_program_child_update` items;
terminal uncertainty sends `resync_required` and stops. Source/lane, identity,
branch proof, budgets and cancellation remain core semantics. HTTP only encodes
these typed items. Dropping the stream releases its subscription, not the shared
native host. Existing cookie/Host/Origin checks, authorization rechecks,
`auth_required` behavior, keepalive and listener shutdown still apply.

This is a fresh-snapshot feed, **not transcript replay**: it does not accept query
parameters; `Last-Event-ID` is not a child resume token. SSE IDs are core sequence
numbers, while nested native `event.seq` is a separate sequence domain. After
resync/disconnection, retire the old stream and request a fresh bounded snapshot
and subscription; never silently rebase historical/native cursors.

The opt-in `tests/tool_program_native.rs` proof with explicit frozen native path,
SHA and new evidence directory passed 26 Windows recovered sessions / 70 children,
38 selected children and 2,374 body chunks, with zero execution events. It covers
typed HTTP reads and initial SSE, not real-native ongoing overlap, a running Web
backend upgrade, live UI/provider qualification, Unix, deployment, CI or full
parity. See the [current qualification](../../docs/codemode-full-parity-matrix.md#current-child-adapter-and-web-increment).

## Embed the API

`rpi_routes(core, &config, shutdown_receiver)` builds the standard API router around an existing `Arc<rpi_extcore::service::Service>`. Supply the actual bound port in the configuration so Host checks use the advertised endpoint. `router(...)` remains a compatibility alias. Generic owners can compose their own routes with `rpi_httpcore::transport::lifecycle_router(...)` without depending on `rpi_extcore::Service`. Rust callers and HTTP clients then observe the same service state.

`serve_until(config, frontend_router, on_listen, stop)` owns a listener and a core instance with bounded shutdown; Web uses this to combine the API with its own frontend routes. `run(config)` starts the API-only service and stops on Ctrl+C.

## Verification

```bash
cargo test --locked -p rpi-extcore -p rpi-httpcore
python packages/rpi-httpcore/tests/native_smoke.py --http /path/to/rpi-httpcore --rpi /path/to/rpi
```

The native smoke test exercises authentication, metadata, snapshots, SSE, restart recovery and the absence of frontend routes without model calls. It also checks archive PID exit/history preservation, explicit restore/reopen, permanent deletion, independent clone survival, and locked-file deletion retry on Windows. All state and credentials are synthetic; an owned process tree is cleaned up on exit. Optional `--http-sha` and `--rpi-sha` pin the exact binaries; actual hashes are included in the result.

Archival still uses `POST /api/v1/sessions/{id}/metadata` with
`{"archived":true}`; restoration uses `{"archived":false}` and does not spawn a
host. `DELETE /api/v1/sessions/{id}` now waits for idle host exit and verified
history cleanup before returning 204. Busy sessions return 409 `session_busy`;
unknown idle state returns 503 `session_idle_unknown`. Archived opens return 409
`session_archived`. A failed partial deletion remains visible as `deleting: true`
and blocks reopen with 409 `session_deleting`; retry DELETE explicitly. Missing
history in the configured root returns 409 `deletion_history_missing`, without
forgetting the session or deleting timings. No history-search fallback to another
root is attempted. See the [core lifecycle contract](../rpi-extcore/README.md#session-archival-and-permanent-deletion).

## Optional paginated message queries

Matching native ready frames advertise `get_message_summaries` and
`get_message_window`. Core exposes typed, validated user-only summary pages
(default 10, maximum 100) and read-only nearby message windows (default 48,
maximum 100). Cursors are exclusive native entry IDs; `leafId` pins the branch
snapshot. A removed snapshot leaf is a conflict. There is no full-transcript
fallback or second transcript store for unsupported hosts.

HTTP maps these services to authenticated `GET
/api/v1/sessions/{id}/message-summaries?role=user&limit=10&beforeId=...&leafId=...`
and `GET /api/v1/sessions/{id}/message-window?anchorId=...&limit=48&leafId=...`.
Window continuation uses exactly one of `anchorId`, `beforeId`, `afterId`.
Old native hosts return unsupported (HTTP 501); malformed queries return 400,
invalid host pages 502, and removed snapshot leaves 409. Existing `/api/v1`,
legacy `get_messages`, snapshot/SSE replay boundaries and native ownership are
unchanged. Installing/restarting matching runtime layers is a separate step.
