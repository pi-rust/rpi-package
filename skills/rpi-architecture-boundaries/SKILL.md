---
name: rpi-architecture-boundaries
description: |-
  Use when reviewing or changing the rpi Web/HTTP/ACP/native architecture, deciding where a feature or data belongs, moving session/message loading, designing shared services, or reviewing extcore/http/acp/web responsibilities. Triggers on rpi-extcore, rpi-httpcore, rpi-http (plugin entry), rpi-acp, rpi-web, architecture boundary, session snapshot, transcript, message loading, SSE, ACP, Web adapter, protocol adapter, data ownership, 模块边界, 架构审查, 会话快照, 消息加载, 数据应该放在哪里.
globs: ["packages/rpi-extcore/**", "packages/rpi-httpcore/**", "packages/rpi-http/**", "packages/rpi-acp/**", "packages/rpi-web/**", "docs/architecture/**"]
---

# rpi architecture boundary guide

Use this skill before adding or relocating behavior between `rpi-extcore`, `rpi-httpcore`, `rpi-http`, `rpi-acp`, and `rpi-web`. `rpi-httpcore` is the HTTP/SSE library adapter; the `rpi-http` crate is only its native plugin entry point (`rpi --http`). The goal is to keep one protocol-neutral runtime core while allowing each adapter to preserve its wire contract and presentation model.

## Non-negotiable dependency direction

```text
Browser -> rpi-web -> rpi-httpcore -> rpi-extcore -> native `rpi --mode rpc`
(rpi --http -> rpi-http plugin -> rpi-httpcore -> rpi-extcore)
ACP/Zed/terminal -> rpi-acp -> rpi-extcore -> native `rpi --mode rpc`
```

- `rpi-extcore` must not depend on Axum, ACP types, frontend code, or the plugin ABI.
- `rpi-httpcore` and `rpi-acp` may depend on `rpi-extcore`; the core must not know either adapter. The `rpi-http` plugin crate may depend on `rpi-httpcore` only.
- `rpi-web` owns the bundled frontend and entrypoint composition. It uses the HTTP adapter and embeds/serves assets.
- The native host remains the owner of the native RPC contract and transcript files. Do not replace native history with a new Web/ACP database.
- Separate service processes do not share live sessions merely by sharing JSON files. One service instance owns each live session registry.

## Ownership matrix

| Concern or data | Canonical owner | Allowed adapter work | Must not be moved to |
| --- | --- | --- | --- |
| Native child process, RPC writer, request correlation, ready handshake | `rpi-extcore::host` | Map errors/status to the protocol | HTTP handlers or ACP session code |
| Prompt acceptance, run lease, cancellation, settlement, text/thinking reconciliation | `rpi-extcore::{run,message,host}` | Choose wire-level completion names | React components or ACP transport |
| Live event buffer, sequence numbers, replay boundary, lag/resync | `rpi-extcore::{events,host}` | Encode SSE or ACP notifications | Frontend-only caches |
| Native transcript/history and active branch | Native host, accessed/validated by `rpi-extcore::history` and `service` | Request a bounded snapshot/page | A parallel adapter-specific transcript store |
| Session snapshot assembly, message/history limits, pending dialog/runtime state | `rpi-extcore::service` | Serialize the existing neutral result | `rpi-httpcore` route-specific business logic |
| Projects, workspace files/Git, registry metadata, tool timings | `rpi-extcore::{projects,workspace,registry,timing}` | Convert reply kinds to status codes or protocol errors | Frontend local storage as source of truth |
| Neutral errors and operation outcomes | `rpi-extcore::reply` | Map to HTTP status or ACP error/stopReason | Stringly typed duplication in every adapter |
| HTTP extraction, JSON serialization, status mapping, cookies/password, Host/Origin, SSE | `rpi-httpcore` | Call core service functions | `rpi-extcore` or React |
| Plugin flag registration, `SessionStart`/`SessionShutdown`/`StdioLifecycle` mapping, `--http` argument adaptation | `rpi-http` (plugin) | Forward host flags to `rpi-httpcore` config | `rpi-httpcore` or the frontend |
| ACP JSON-RPC, capabilities, content blocks, `stopReason`, tool/diff/plan formatting | `rpi-acp` | Call core runs/events and format ACP updates | `rpi-extcore` or HTTP handlers |
| ACP client request tasks and transport-scoped dialogs | `rpi-acp` | Forward validated answers to core | Core, unless the behavior is transport-neutral |
| Vite/React routes, row projection, Markdown, tool cards, scrolling, virtualization, theme | `rpi-web/frontend` | Fetch snapshots/pages and subscribe to events | `rpi-extcore` or HTTP/ACP |
| Web configuration discovery, `--webserver`/`--web` aliases, assets embedding and startup | `rpi-web` | Build an `AppConfig` and compose routers | `rpi-httpcore` or frontend runtime |
| Native extension entrypoint and plugin ABI | `rpi-web` plugin/host boundary | Preserve registration and lifecycle behavior | Shared core APIs |

## Feature placement rules

1. **Ask whether two protocol clients need the behavior.** If Web and ACP need the same session/run/history semantics, implement it once in `rpi-extcore` and expose a neutral Rust API. If only one wire protocol needs it, keep it in that adapter.
2. **Separate meaning from presentation.** “What happened?” belongs in core; “how is it encoded or displayed?” belongs in the adapter. For example, a settled run is core; HTTP `502`, ACP `stopReason`, and a React status badge are not.
3. **Keep transport resources with the transport.** Axum extractors, cookies, SSE streams, ACP JSON-RPC writers, ACP request tasks, and Zed registration stay outside core.
4. **Keep UI projection in the UI.** `rowsFromMessages()`, Markdown parsing, tool-card merging, scroll anchoring, and virtualized rendering are frontend concerns even when their input comes from a core snapshot.
5. **Keep source-of-truth data with its existing owner.** Native transcripts remain native-host data; `rpi-extcore` validates, bounds, combines, and serves them. Registry and timing files are supplemental metadata, not transcript replacements.
6. **Prefer bounded and resumable reads.** A snapshot API should carry limits/cursors and the event sequence boundary needed to subscribe without a gap. Do not make an adapter fetch an unbounded transcript and then trim it in JavaScript.
7. **Do not hide protocol differences in generic core JSON.** A neutral `Value` is acceptable at the existing native boundary, but new core APIs should use named Rust request/result types when a concept is shared and stable. ACP-only or HTTP-only fields must not leak into those types.
8. **Preserve lifecycle ownership.** The component that creates a child, subscription, listener, or task must define cancellation, shutdown, timeout, and cleanup. Do not make a frontend or protocol adapter kill a native process behind core’s back.

## Session and message loading contract

The current shared operation is `rpi_extcore::service::snapshot_with_options()` and the HTTP adapter’s `snapshot()` route is only an adapter. Treat the following as one core contract:

- capture the event sequence boundary;
- obtain a bounded native message page;
- obtain bounded native operation/history data;
- include runtime status, pending dialogs, settlement and replayable events;
- detect event eviction/races and require a retry/resync instead of silently returning an inconsistent view.

The Web client may then:

- project `messages` into display rows;
- render Markdown and tool results;
- request older pages using the snapshot pagination fields;
- subscribe to SSE and merge events;
- cache already-opened sessions or virtualize very long transcripts.

Those optimizations must not reimplement run settlement, event sequence ownership, native history validation, or transcript persistence in TypeScript. ACP should consume the same core semantics but independently format ACP updates and history responses.

## Decision checklist for a change

Before editing code, answer these questions in the change description or review notes:

1. Is the behavior semantic/runtime, transport, entrypoint, or presentation?
2. Does it need to work for both Web and ACP? If yes, what neutral core API expresses it?
3. Which component owns the underlying resource and its cleanup?
4. What is the source of truth, and is the new data only supplemental metadata?
5. Is the read bounded, paged, cancellable, and resumable after reconnect?
6. Can a client subscribe before sending/taking the replay boundary so no event is lost?
7. Which existing wire contracts must remain byte/field compatible (`/api/v1`, SSE names/IDs, cookies, ACP v1)?
8. What tests prove both the shared behavior and the adapter-specific mapping?

## Common wrong turns

- Putting Axum `StatusCode`, `Json`, `Sse`, cookies, or request extractors in `rpi-extcore`.
- Putting ACP `session/update`, `ContentBlock`, Zed registration, or `stopReason` in `rpi-extcore`.
- Moving `rowsFromMessages()` or Markdown/tool-card rendering into Rust core.
- Loading all messages on every session switch and relying on frontend filtering to fix latency.
- Creating a second transcript database in Web or ACP.
- Letting separate service processes mutate the same registry and assuming they share in-memory sessions.
- Completing a run on `agent_end`/a transport event instead of the authoritative settlement semantics.
- Changing `--webserver`, `--web`, `.rpi/web.json`, legacy environment names, explicit assets, `/api/v1`, or SSE behavior without compatibility tests.

## Verification expectations

For an architecture-only review, inspect the implementation, call sites, diff, and tests; do not infer ownership from commit titles. For an implementation change, run the smallest relevant checks and then the workspace checks when practical:

```text
cargo test --locked -p rpi-extcore -p rpi-httpcore -p rpi-http -p rpi-acp -p rpi-web
cargo clippy --locked -p rpi-extcore -p rpi-httpcore -p rpi-http -p rpi-acp -p rpi-web --all-targets -- -D warnings
cd packages/rpi-web/frontend && npm run typecheck
```

Also verify native smoke/integration behavior when changing snapshots, SSE, startup, authentication, ACP streaming, or lifecycle cleanup. Do not claim a runtime upgrade or performance improvement until the actual binary/extension source and a restarted service have been verified.

## Related source of truth

- `docs/architecture/extension-core.md` — dependency direction and preserved behavior.
- `packages/rpi-extcore/README.md` — shared service/core API and lifecycle notes.
- `packages/rpi-httpcore/README.md` — HTTP adapter library and standalone service contract.
- `packages/rpi-http/README.md` — native plugin entry point (`rpi --http`) and `rpi-http-service`.
- `packages/rpi-web/README.md` — Web entrypoints, configuration, and asset boundary.
- `packages/rpi-acp/README.md` — ACP wire and session behavior.
