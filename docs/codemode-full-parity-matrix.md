# Code Mode full-parity matrix — qualification checkpoints

Scope: companion host `D:/Projects/pi-rust-codemode`, package
`D:/Projects/rpi-package-codemode`, and task-only frontend work in
`D:/Projects/rpi-package-dev` (`dev/web-hmr`). **Overall full parity remains
INCOMPLETE.** No unrestricted full-toolset, source release, installation,
deployment, production restart, merge or CI-green claim. Older checkpoints below
are historical evidence, not claims that their then-remaining work is still absent.

## Current child adapter and Web increment

Implemented: core host-bound typed child queries/snapshot and caller-owned live
subscription; thin HTTP typed POSTs and a separate snapshot-first child SSE feed;
four optional ACP read-only typed RPCs with bounded asynchronous request tasks plus
an optional ACP `rpi` live subscription extension; frontend-owned child cards,
explicit pagination/body fragments and fresh bounded reconnect/reload. Native
transcripts and execution remain native-owned. No tool execution/replay, adapter
transcript database, hidden body/history scan or changed SDK 0.3.20/ABI,
auth/permissions/accounting/termination/recursion policy.

The live subscription preserves its initial pinned pages/cursors, validates source
and identities, and performs bounded fresh branch proof before each child delivery.
Lag, conflicting/evicted facts, branch changes and uncertain proof terminate with
explicit resync. ACP's 64-task query registry and 16-subscription connection
registry keep ordinary requests and cancellation responsive; dropping a query or
subscription retires its correlation, not the host. The live ACP extension is
explicitly vendor-scoped and uses `session/tool_program_update`, not standard
`session/update`.

Two P2 review counterexamples were fixed: native forks finalize their actual
native-selected destination from existing get_state readback exactly once rather
than using an unused requested ID; newly replayed/live phases do not borrow an
older pinned leaf for body reads. An explicit initial overlay-body read pins the
current branch through the authoritative body API, and continuations keep the
returned leaf. Fork regression is in-memory; no real-native live-fork qualification
is claimed. This focused review is not independent final approval.

### Evidence and limitations

- Default core/HTTP/ACP/Web Rust regression before this ACP-only extension: **621
  passed / 0 failed / 5 ignored**, `target/codemode-child-adapters-current-1.log`.
  The focused live proof now also covers every selected begin, including a selected
  child with no phase facts. The ACP live extension adds a focused **213 passed / 0
  failed** ACP lib run (including snapshot-first, duplicate-subscription and
  unsubscribe-race coverage). The first run encountered
  the previously observed unchanged catalog duplicate-header test failure; its
  isolated rerun and subsequent full runs passed. Preserve that failure in
  `target/codemode-child-adapters-parent-final-1.log`, not a clean-first-run claim.
- Package Rust formatting passes. Targeted diagnostic Clippy (lib/bins/tests)
  passes with existing durable.rs and ACP diagnostics; all-target Clippy is blocked
  by the existing configuration_fixture.rs export-macro raw-pointer diagnostic.
  `target/codemode-child-adapters-clippy-{targeted,final}-1.log`. No strict Clippy,
  whole-workspace formatting or CI-green claim.
- Actual Windows native → HTTP recovered reads and initial child SSE:
  `target/codemode-http-child-native-6/results.json` — **26 sessions / 70 children /
  38 selected / 2,374 body chunks / zero execution events**. The same run observed
  zero post-ACK child events after the subscription boundary and only initial SSE
  snapshots; the explicit blocked qualification is recorded in
  `target/codemode-native-live-overlap-blocked.json`. This is not ongoing
  live-native/core overlap or installed Web backend proof.
- Actual Windows stdio ACP bridge → launcher → frozen native:
  `target/codemode-acp-child-native-11/results.json` — **26 sessions / 70 children /
  38 selected / 38 first body chunks / zero execution frames**, 156 strict error
  checks. Prebuild source inventory, Cargo artifact and all binary hashes are in
  `target/codemode-acp-child-native-build-11/`. Source/copied primaries and progress
  sidecars remained unchanged. First-chunk coverage is not full-body aggregate
  integrity; the frozen read-only path provides no post-ACK live event to map into
  the newly implemented optional ACP extension, so real-native ACP live delivery
  remains unqualified.
- Frozen native stays SHA-256
  `559fd48b57dc671865205d5a8f1bb5ec6caad69628cb3bbfa3c6e838f8c9bfc7`.
  No installed/production binary was replaced or restarted. Proof launchers only
  supply fixed discovery exclusions and forward real native pipes.
- Frontend task commit in the approved dev worktree: `277a620`. Typecheck passes;
  **14 local and 14 public-dev browser fixture tests** cover exact native parent
  identity, explicit pages/fragments, new-leaf overlay body reads, independent SSE,
  reconnect/reload, auth/errors and navigation cancellation. **134 related local
  trace/session/timeline regressions** pass. Public CSS HMR and its exact restoration
  were observed (`target/codemode-web-child-public-hmr.json` in dev). A separate
  targeted actual-native Web recovery also passed through a local `rpi-http` server,
  Vite proxy and real Chromium frontend: one frozen-native saved session rendered,
  the child inspector opened, and the browser issued the bounded snapshot request
  without execution or replay (`D:/Projects/rpi-package-dev/target/codemode-web-native-local/results.json`).
  This is one Windows read-only recovery session, not ongoing native live overlap,
  full-body aggregate coverage, Unix/provider behavior, installed-runtime behavior,
  or production qualification.
- All six unrelated dev edits remain preserved: five files byte-exact to baseline,
  and App.tsx's inverse task patch matches its original canonical baseline. Only
  task-specific files/hunks are committed/transplanted; no reset or sweep.

### Failed proof safety incident

Earlier ACP probes failed on the invalid ready.sessionId assumption and overly
strict graceful-exit/optional launcher-receipt assumptions. More seriously,
`target/codemode-acp-child-native-6/session-4/cleanup.json` records an unsafe old
helper inferring descendants through historical PIDs and terminating unrelated
`work-cli` processes after PID reuse. **That run is failed evidence, not acceptance.**
No unknown process was restarted to conceal or guess at its prior state.

The helper now admits only the directly created bridge, verified direct launcher
and explicitly receipted native process, checking actual image path/SHA, kernel
creation time, direct parent and snapshot/open races. Retained handles never
expand into arbitrary PID trees; no PID/tree-kill command remains. **26 stdlib mock
safety tests** pass and fresh actual-native run 10 passes. Manual exact-handle
cleanup is always failure. Native EOF retirement records actual exit 0 or owner
termination 1; missing/incomplete wrapper receipts remain explicit. This is not
an all-system-console audit or graceful native-shutdown claim. Preliminary failed
runs 1–8 remain retained; earlier non-final passes are not substituted for run 10.

**Blocked by frozen native read-only boundary:** real-native ongoing adapter
live overlap/branch/generation qualification cannot be demonstrated because the
fresh subscription run emits no post-ACK child events without executing or
replaying a program (`target/codemode-native-live-overlap-blocked.json`). The
optional ACP live child notification contract is now implemented and
fixture-tested, but this frozen read-only path supplies no native frames to map
into it, so real-native ACP live delivery remains explicitly unqualified.
Final integration review is recorded below; the targeted actual-native Web
recovery is qualified only for the bounded one-session proof above. Protected
self/ancestor policy remains unchanged. Unix/live-provider, installed-runtime,
production, CI-green and full-parity qualification remain absent.

## Latest neutral child snapshot and replay integration

*Historical foundation checkpoint; current adapter/Web status is above.*

Core now exposes actual event validation and bounded child snapshot/replay
runtime semantics, not only page readers. `service::tool_program_events` validates
strict typed metadata under the separate exact event capability. The explicit
`service::child_snapshot` API subscribes before its boundary, reads a fresh
newest-first window of at most eight children / eight phases each, and reconciles
query/live overlap while preserving native pages and cursors. Complete output
and selected-event accounting are bounded to 128 KiB with a 30-second total
budget. Replay eviction/overflow requires reload; new out-of-window children and
native resync signals explicitly mark `resyncRequired`. No body hydration, generic
history fallback, new executable session, execution replay or parent settlement.

A snapshot-wide native identity registry rejects cross-child ID/sequence
contradictions, including query/event overlap. Outcome projection requires actual
adjacent final/closed records and flags; missing evidence stays uncertain. A
copied fork's facts remain historical, not a destination execution claim. Ordinary
transcript snapshots and existing adapter behavior remain unchanged.

Qualification of this core increment:

- Focused coverage: **39 passed**, plus one separately opt-in ignored native test
  (12 event tests, 13 snapshot/identity/lifecycle tests, 14 prior query tests).
- Core/HTTP/ACP/Web regression **552 passed / 0 failed / 4 ignored**, twice.
  Logs: package `target/codemode-core-child-snapshot-adapters-final{,-2}.log`.
- Diagnostic core Clippy has only the two existing durable.rs warnings; no new
  touched-file warnings. Extcore formatting and diff checks pass. This is not a
  strict-baseline-green or whole-workspace-format claim.
- Current-source real Windows native/core read-only acceptance:
  `target/codemode-core-child-snapshot-native-4/results.json` — **26 sessions /
  38 selected children / zero execution events**; largest observed complete
  snapshot **22,281 bytes**. Every selected final/closed record and observed
  outcome matched the source native transcript; source primary hashes and the
  copied primary during queries remained unchanged. The 8-child window does not
  claim all 70 source children were loaded by this API.
- The source-checked frozen native remains SHA-256
  `559fd48b57dc671865205d5a8f1bb5ec6caad69628cb3bbfa3c6e838f8c9bfc7`.
  A test-only std launcher adds fixed discovery exclusions and forwards pipes
  unchanged; it does not manufacture native protocol output. Its hash and snapshot
  source hash are retained in results. The Code Mode DLL stays unchanged.
- A focused review identified a P2 cross-child identity gap; the registry and
  regression checks fix it. The review hit its turn ceiling and is **not** an
  independent final approval. Preliminary native probes failed before acceptance:
  this frozen native lacks the unrelated safe-resource-query profile, and the
  first launcher probe put the copied primary outside the native session root.
  The final probe uses only fixed discovery exclusions and the correct copied
  layout. A paused-time test initially over-specified which of the equal 30-second
  deadlines wins; it now checks the total bound and safe subsequent read. A late
  cancelled-response fixture initially unwrapped expected no-subscriber delivery;
  corrected fixture behavior retains the cancellation/isolation assertion.

**Remaining:** real-native core-to-adapter live-overlap acceptance (including
ACP delivery), approved dev frontend child cards/reload/reconnect/HMR and final
integration. Frontend's six unrelated edits
remain untouched. No live-provider, Unix runtime, installed-binary upgrade,
production restart, deployment, merge, CI-green or full-parity claim.

<a id="latest-bounded-native-child-queries-commit-events-and-core-readers"></a>

## Historical bounded native child queries, commit events and core readers

This increment adds actual native query/event implementation and neutral package
readers, not just another codec or documentation checkpoint. Historical source
checkpoints below remain historical. Native implementation:
`4b75ea3a8f2937194b4ccb42c821ce12d199c195`; final pump cleanup/qualification:
`3fb2209920c8fadcd0c9212a1703abf3a0b1babe`. The package now includes core-reader
runtime code, not only documentation; the Code Mode DLL itself remains unchanged.
**Overall full parity remains incomplete.**

| Surface | Implemented contract / boundary |
| --- | --- |
| Native read-only RPC | Exact `toolProgramChildren` v1; `get_tool_program_children`, `get_tool_program_child_phases`, `get_tool_program_child_body`. Strict mandatory count/byte/direction arguments, no generic history fallback. |
| Summary/phase pages | At most 32 items / 128 KiB encoded data page, no oversized-first exception. Phase scans at most 128 exact-type metadata candidates; sparse empty continuation pages are valid. Summaries stay `unknown`; scan exhaustion is not lifecycle completion. |
| Native authority / recovery | Real assistant tool-call-only slot, root Never checkpoint, bounded parent chain, phase and descriptor ancestry. Cursors bind destination session/lane/selected leaf/direction/target. Native fork entries without copied run records explicitly use `originVerification: forkCopy`; partial/mismatched checkpoint evidence fails closed. Destination copies only, no source fallback, repair or execution replay. |
| Lazy body reads | Exact child/phase/descriptor authorization before existing 8-chunk / 64 KiB decoded pages. Returned chunk checksums validated; whole-body aggregate integrity requires streaming all pages. Summaries/phases do not read archival payloads. |
| Native live events | Separate `toolProgramChildEvents` v1; genuine begin/phase metadata after append ACK, pinned source session and native entry ID/seq/parent, at most 32 KiB/event, no body/private-request-ID insertion or parent settlement. Bounded broadcast is best-effort with explicit resync-on-lag/oversize, not exactly-once delivery. |
| Neutral package API | `rpi_extcore::service::{child_summaries, child_phases, child_body}` and named strict DTOs. Exact ready profile/all three commands required before sending; old hosts return Unsupported with zero new requests/scans. Existing live host owner/correlation/admission/timeouts, no new executable session. Count/bytes/cursors/order/descriptors/base64/chunk hashes validated. Unclassified native rejections are sanitized HostUnavailable, not presumed caller input errors. |
| Remaining | Core snapshot/live reconciliation, bounded combined child-state projection, thin HTTP/ACP mappings, approved dev frontend cards/reload/reconnect/HMR. Raw native events surviving core replay alone is not this integration. No frontend source changed yet. |

Qualification of that query/reader checkpoint:

- Native agent/extensions/harness/SDK **1,201 passed / 0 failed / 6 ignored,
  twice**, 73 result summaries. Includes 10 new query tests and 2 native event
  writer tests over the prior 1,189. CLI RPC **79 passed** (not the full CLI
  suite); all-features query/journal focus **26 passed**.
- Core package **288 passed / 0 failed / 3 ignored**, including **14 new reader
  tests**. Core/HTTP/ACP/Web default-parallel regression repeat **527 passed /
  0 failed / 3 ignored**. Initial broad adapter run failed the unchanged catalog
  duplicate-header test (continuation instead of expected error); isolated
  recheck and default full repeat passed. No unrelated test was changed.
- Native workspace fmt, package extcore fmt, builds and diff checks pass.
  Diagnostic native Clippy passes with 48 existing harness warnings; strict core
  Clippy remains blocked by two existing durable.rs diagnostics. Broader package
  fmt still reports existing rpi-voice pet.rs differences, untouched. No strict
  native/core baseline-green or whole-workspace-format claim.
- Fresh Windows host + unchanged Code Mode DLL/fixture/gate **59/59** differential
  smoke. The read-only audit matched **406 live events** exactly to primary
  entries (70 begins / 336 phases), including source, ID, seq, parent and data;
  all per-session ordered, maximum observed event 834 bytes, no observed fixture
  resyncs. Simulated actual broadcast lag explicitly requires resync in unit tests.
- Real CLI reopen of **copied** fresh transcripts recovered **26 sessions /
  70 children / 336 phases / 266 bodies / 101,830,803 decoded bytes**, through
  **2,079 bounded RPC pages**, verifying per-chunk and aggregate hashes, with
  **zero execution events** and original evidence files unchanged. This uses the
  real native RPC commands, not the local audit as a replacement reader.
- Evidence in native `target/codemode-native-query-events-smoke-2/` and
  `target/codemode-query-recovery-final-2/`; native full tests
  `target/codemode-query-events-full{,-repeat}.log`; package
  `target/codemode-query-adapters-regression-repeat.log` and
  `target/codemode-core-query-fix-results.md`.
- Frozen Windows host SHA-256:
  `559fd48b57dc671865205d5a8f1bb5ec6caad69628cb3bbfa3c6e838f8c9bfc7`.
  Code Mode DLL remains `5fccaaa062bdebb1fe74294a470d18816cbb59ffe8e79ad43196e220e8ebdd6f`,
  fixture `27830e8fe1ca4a1f94918c1be51912472d34bba6ed3ace716dd933339eb23bac`,
  gate `b52ce8ce8f10e032b701eb53cbca01e1abe04d567843aa96e2be9d880542cb34`.

Core fixtures and native RPC qualification are separate: **no real-native core
service-to-adapter/UI acceptance** yet. Metadata ancestry/eager custom storage
are not RSS guarantees. No live-provider, Linux/macOS runtime, installed DLL,
production binary replacement/restart, deployment, merge or full-parity claim.
An early recovery-script attempt used the wrong native session directory layout
and an early full-test invocation used the wrong SDK package name; both failed
before qualification and are not passing evidence.

<a id="latest-installed-native-child-journal-queries-and-ui-still-pending"></a>

## Historical installed native child journal (before query/event APIs)

Native source: `ccb342c8da18b3ddef9159acc06054ec71e1c090`. This package
follow-up is documentation only; package runtime/DLL remain at `3db4122`.

The harness now installs a native ToolProgram journal by default, using the
existing primary-session body primitive. This is **real persisted child history**,
not fabricated assistant/tool messages or data reconstructed from parent details.
It does **not yet** expose child RPC capabilities/commands/live events, a complete
child-summary/recovery query, extcore/HTTP/ACP projections, or Web cards/reload/HMR.
The six Code Mode capabilities, SDK pin, ABI and protected recursion policy stay
unchanged. Ordinary tool-only runs do not create child observations or workers.

Before a child can execute, native correlation verifies its checkpointed
Never-replay root against the committed assistant's actual tool-call-only slot,
ID and name. The journal generates a distinct `tpchild-<UUIDv4>` entry ID; real
nested parents resolve through active native mappings, not guest JSON. New public
metadata does not serialize private `tp-*` request IDs. Existing intent/finished
records retain their prior private-ID fields; arbitrary original arguments and
results are archived exactly, including sensitive values they may contain.

Primary custom entries are `rpi.tool_program.child_begin.v1` and
`rpi.tool_program.child_phase.v1`. Requested arguments ACK before publishing the
observation; prepared arguments ACK after the existing native execution-intent
checkpoint and before effects. Genuine ordered signed partials, the authoritative
**post-after-hook final result**, and a closed phase reference canonical durable
bodies. A prepared record is not proof effects ran; a missing final is not a
successful result. Partial bodies and uncertain ACKs are never repaired, retried,
or used to replay execution. No new fsync or power-loss guarantee is made.

Per-child partial admission is **8 queued/in-flight updates and 8 MiB of exact
encoded values**, with bounded metadata and separate borrowed final capacity
(up to the body primitive's 32 MiB). A single writer owns each child's channel;
a shared journal transaction gate orders writers. Quota/storage failures are
sticky run failures, cancel admission and drain ownership rather than silently
truncating/dropping a successful stream. Known native usage/termination accounting
is retained before fallible archival final encoding. Closed mappings retire;
there is no new run-wide 128-start budget across independent programs. Normal
close stops admission and joins/drains accepted work even on errors; abrupt Drop
only aborts and cannot promise an asynchronous join or a closed record.

Native facade mutations share a per-binding reader/writer gate. Ordinary
checkpoints remain concurrent under shared admission; journal metadata and whole
body revision/ancestry/CAS/ACK sections take exclusive admission. The gate is
never held while running tools or awaiting callback/queue drains. Clones/pins
share it, queued writes pin the old binding before waiting, and task-local
reentrancy does not propagate to spawned tasks. CAS still fails closed without
retry for raw-storage/independent-facade/external mutations. This is **not global
or cross-process serialization**, a storage-latency bound, or an RSS guarantee.

An explicit host observation sink is composed with, not substituted for, the
native journal; failures propagate and both normal close paths are awaited.
Generic Session/custom/provisioned appends **and guarded public scoped appends**
reject the exact begin/phase namespace before conditional writes. Ordinary and
similarly prefixed custom entries remain supported. Direct trusted storage
handles are not an extension sandbox; only body chunk/commit types are excluded
from semantic hooks. Begin/phase metadata remains visible as bounded custom data.

**Custom storage migration:** installed journaling requires real
`history_revision()` and atomic `append_entry_if_revision()` support. Existing
trait defaults fail closed; wrappers must forward to the same backing store or
implement atomic CAS. Do not emulate it with read-then-unconditional-append or
silently omit journaling. The prior `EntryQuery` field migration and metadata
ancestry fallback caveats below still apply.

Qualification: **1,189 passed / 0 failed / 6 ignored, twice**, native
agent/extensions/harness/SDK, 73 result summaries in
`target/codemode-journal-full-final{,-repeat}.log`. Includes **25 new tests**:
14 writer tests, 7 facade-coordination tests, 3 additional observation integration
tests and 1 guarded-namespace test. Existing observation/recovery tests were
retained and adapted for default installation, not counted again. A separate
all-features journal focus passed 14 tests. Coverage includes exact signed
partials/post-hook finals, eight concurrent children on real asynchronous JSONL,
immediate native finished writes, cold reopen, nested parent IDs, namespace
reservation on main/side, callback quotas, cancellation/join ownership, facade
switches, raw-CAS conflict/no replay and independent checkpoint concurrency.

Fresh rebuilt Windows host + unchanged DLL/fixture/gate smoke **59/59**:
`pi-rust-codemode/target/codemode-native-journal-smoke-2/`. The read-only local
`target/audit-codemode-journal.py` checked 26 real primary session logs: **70 child
begins, 266 complete bodies / 101,830,803 decoded bytes**, requested70/prepared68/
update58/final70/closed70. It verified native assistant slots, parent IDs,
monotonic phases, per-chunk and aggregate hashes, descriptors and artifact hashes.
This audit is evidence, **not a new runtime query API**. Current host SHA-256:
`3a623f1c1fd7557ea07bc1add026310005c4ce38ca0020074ab45a65505695f0`.

Fmt and CLI build pass. Diagnostic Clippy without `-D warnings` passes but strict
native Clippy remains blocked by 48 existing harness lib-test diagnostics, not
strict-green. Initial qualification caught the overly exclusive facade gate's
parallel-checkpoint regression; shared ordinary mutation admission fixed it and
the original regression test passes unchanged. Earlier reviews also caught
uncoordinated CAS and scoped-namespace bypasses; both were fixed and covered.
Earlier failing attempts/smoke artifacts are not final evidence. No installed
binary replacement, deployment, production restart, merge, live-provider,
Linux/macOS, full-workspace-all-features, performance/RSS or full-parity claim.

**Next:** capability-gated bounded child summary/body queries, branch/cursor and
snapshot/live reconciliation with explicit uncertain recovery states; native
child events; then neutral extcore, thin HTTP/ACP and approved dev frontend work.
The overall goal remains incomplete.

## Historical native archival-body primitive checkpoint

Native source: `142a307f2e2e4278c3feab774106f8d22ce64f47`. This package
follow-up is documentation only; package runtime/DLL remain at `3db4122`.

Native `rpi-harness::tool_program_body` now offers `append_body()` and
`read_body_page()`. This is a **standalone primary-session storage primitive**:
no default observer writer, child RPC capabilities/commands/events, core adapter
or Web presentation is installed. The six Code Mode caps and SDK ABI stay unchanged.

Borrowed canonical JSON is serialized into decoded **32 KiB chunks**, at most
**32 MiB / 1,024 chunks**. The synchronous encoder stages bounded chunk buffers;
it does not build an intermediate payload Value or contiguous full-body String.
All quota/encoding preflight occurs before any append. Primary custom chunk
entries ACK sequentially; the commit descriptor is appended **last**, with native
IDs, byte counts and SHA-256. Entry envelopes are at most **64 KiB**. Conditional
append conflicts/uncertain ACKs fail closed without retry or execution replay;
orphan chunks are not repaired or advertised as complete. Existing native
append durability applies, not a new fsync/power-loss guarantee.

Reads pin current native session/lane ownership and exact branch ancestry.
Cursors bind session/lane/leaf/descriptor/index. Pages cap decoded data at
**64 KiB**, count at **8**, and encoded responses at **128 KiB**, with no oversized
first-entry exception. Per-chunk checksums are verified; consumers must stream
all pages to verify the descriptor's aggregate hash. No blanket complete/available
flag is returned. Forks read copied destination entries only, never source-session
fallback. The primitive accepts an existing branch-local non-body custom entry
as its child anchor; this is **not a validated production child-begin authority**
or a host-authenticated custom-type namespace.

Exact reserved body types are excluded from semantic context, user transforms/
projectors, recovery/tip scans, unknown-outcome repair, notice lookup and branch
summarization **before cold payload reads** on native cold backends. Metadata-only
`EntryAncestry` preserves the true common ancestor even when it is archival;
ordinary custom entries and noncustom boundaries remain intact. Generic explicit
history/export/body reads remain broad and validate payloads. Label/checkpoint
reads are not redesigned here. Native metadata/ancestry still grow with history;
memory/ordinary SQLite retain eager backend behavior. Custom storage ancestry
fallbacks may hydrate entries. These are **not RSS or zero-I/O bounds**.

**Rust API migration:** `EntryQuery` has a new public
`exclude_tool_program_bodies: bool` field, default false. Exhaustive downstream
struct literals must add it or use `..Default::default()`; custom storage
implementations must honor it before payload loading. New default
`SessionStorage` / `SessionTree::branch_ancestry()` methods preserve implementer
compilation, but require metadata overrides for cold-read guarantees.

Qualification: **1,164 native tests passed, 0 failed, 6 ignored, twice** in
`target/codemode-native-body-tests-final-state{,-repeat}.log`. Includes 18 new
default-feature tests; do not count them again. A separate all-features body
focus passed **21 tests** (11 body, 4 context/recovery/notice, 3 branch-summary,
3 experimental selector). Cold poisoned-body probes, restart/fork/source deletion,
conditional write/lost ACK, session switch, UTF-8 chunk boundaries, quota/error
and bounded pages are covered. The first broad attempt failed on SQLite DiskFull;
a subsequent compile attempt found a test-only crate-root path bug in the shared
metadata oracle, fixed before the passing runs. Neither failed attempt is passing
evidence. Only this isolated worktree's generated incremental cache was cleared;
production files/binaries/services and existing smoke evidence were preserved.

Fresh rebuilt Windows host + unchanged DLL/fixture/gate smoke: **59/59**, local
`pi-rust-codemode/target/codemode-native-body-smoke-1/{results.json,provenance.json}`.
Host SHA-256: `977257c54277af187a6b0a01412fb8b4e114aad17c2396f4f512aad7bec833e9`;
all four actual artifact hashes verified. This validates unchanged normal native
host/DLL behavior, **not production archival observer integration**. Fmt/CLI build
pass; strict native Clippy remains blocked by existing diagnostics. A new test
lock-across-await warning was fixed; diagnostic Clippy without `-D warnings` is
not a strict-green claim. No deployment, install, production restart, Linux/macOS,
live-provider, full-workspace-all-features or full-parity claim.

**Next:** wire a native-owned bounded observation queue/writer with real
assistant/run/child identities and finalized result descriptors; capability-gated
summary/body/event queries and snapshot/live reconciliation; then neutral
extcore, thin HTTP/ACP and approved dev frontend cards/reload/reconnect/HMR.
Protected recursion policy is unchanged. Overall goal remains incomplete.

## Historical native observation seam (not durable history)

Native source: `02d30b6298bb120590d7fec181e34a4f8a131369`. This package
follow-up changes documentation only; package runtime/DLL remain at `3db4122`.

The host now provides an **optional native observation seam**, not a production
child journal or new RPC capability. It observes genuine requested/prepared
arguments, original signed partials and the actual post-hook final result.
Existing private `tp-*` request/progress/audit identity is unchanged; a future
native durable entry ID is separate. Default emitters return no observer, so the
normal CLI still has **no installed child-history writer**. Six negotiated Code
Mode capabilities and the SDK ABI/layout remain unchanged.

`AgentEmitter::tool_program_observer()` is forwarded through native settling,
frame and extension tee wrappers. Nested captures inherit their real parent;
guest JSON cannot select lineage or an observer. At the harness execution
barrier, effective arguments are observed only after the existing Never-replay
checkpoint ACK and before effects. Prepared-observer errors, including future
construction/poll panics, refuse effects and report a harness persistence error.

The driver retains finalized usage/names/termination **before** fallible observer
finish/serialization. An optional atomic admission/retirement gate drains accepted
in-flight callbacks before finish/close; saved callbacks hold only weak runtime
and writer references. One absolute timeout covers work and writer shutdown.
Cancellation/timeout replies remain prompt even during close, but the same native
work/close future and active permits are retained through drain. One tee observer
is forwarded; identical shared owner Arcs are deduplicated and distinct owners
fail closed before either begin. Ordinary event fan-out is unchanged.

The observer owns partially-created resources until begin returns success, and
must clean them up on error/panic. Close must drain/join even when returning an
error. These contracts and mock failure tests do **not** prove cleanup by an
as-yet-unimplemented production writer.

Qualification: default-parallel SDK/agent/extensions/harness **1,146 passed,
0 failed, 6 ignored**, twice; logs `target/codemode-native-observation-tests-final-repeat.log`
and `target/codemode-native-observation-tests-qualified-repeat.log`. This includes
**18 new tests** (10 agent observation, 6 real-harness observation, 2 tee), not
additional counts to sum again. Tests cover after-hook authority, prepared
checkpoint ordering, denial/invalid preparation, retained accounting/termination,
weak ownership, panic/failure, in-flight updates, and cancellation during close.
An independent second review accepted the optional seam after the four discovered
lifecycle/composition/panic issues were fixed.

Fresh Windows host plus unchanged Code Mode DLL/fixture/gate passed the existing
**59/59 cases**: `pi-rust-codemode/target/codemode-native-observation-smoke-1/{results.json,provenance.json}`.
All four actual artifact hashes were independently verified. This smoke proves
unchanged normal host/DLL behavior, **not production child archival capture**.
Native workspace fmt and CLI build pass. Strict native all-target Clippy remains
blocked by existing agent/extensions/harness diagnostics; the two introduced
manual-inspect diagnostics were fixed. Package runtime/checkpoint remains the
unchanged **124 tests plus strict all-target Clippy**. No deployment, installation,
production restart, live-provider, Linux/macOS or full-parity claim.

| Current verified Windows artifact | SHA-256 |
| --- | --- |
| Host | `1a80a4f96ded98ccf6e493dd275c443d2920cf578d65102958dc277100a17a95` |
| Code Mode DLL | `5fccaaa062bdebb1fe74294a470d18816cbb59ffe8e79ad43196e220e8ebdd6f` |
| Fixture DLL | `27830e8fe1ca4a1f94918c1be51912472d34bba6ed3ace716dd933339eb23bac` |
| Permission gate DLL | `b52ce8ce8f10e032b701eb53cbca01e1abe04d567843aa96e2be9d880542cb34` |

**Still pending:** native primary-session durable body/record writer, bounded
lazy queries and explicit capabilities/events, interruption/restart/fork and
snapshot/live continuity, neutral extcore/thin HTTP/ACP integration, and Web
cards/reload/reconnect/HMR in the approved `rpi-package-dev` worktree. Do not infer
any of these from this seam or the old intent/finish markers.

## Historical native result-codec checkpoint

The native codec follow-up adds camelCase `Serialize` for `AgentToolResult` and
untagged `Serialize` for `TextContentOrImage`. ToolProgram reference mode now
serializes the borrowed native result without an intermediate payload `Value`;
native RPC uses the same canonical serializer, fixing dropped optional
`textSignature` in `ToolExecutionUpdate` / `ToolExecutionEnd`. The native field
is `Option<String>`: even JSON-formatted signature strings retain their exact
string bytes, not a parsed/reformatted object. Result wire fields are unchanged
except this additive optional RPC field. Reference JSON now follows canonical
struct field order; byte-identical object-key order across the upgrade is **not**
promised. No `Deserialize` or new authority is added; `isError` remains external
to the result and host-authoritative. Native SDK ABI/layout and the Code Mode
DLL/source are unchanged; that DLL was not rebuilt for this follow-up.

This is a codec foundation for a **future native child journal**, not a fourth
history increment or full child-history parity. Actual child events/records,
bounded history/query/replay/recovery, neutral core/adapter integration and UI
remain pending. No install, deployment or running-service upgrade is implied.

| Historical codec evidence | Proven scope / limits |
| --- | --- |
| Current rebuilt Windows host + unchanged DLL/fixture/gate | `pi-rust-codemode/target/codemode-native-codec-smoke-2/results.json`: **PASSED, 59 cases, all true**. Same 59 cases, no new smoke cases. All four provenance hashes below verified against actual artifacts. |
| Broad native regression | Default-parallel SDK/agent/extensions/harness: **1,128 passed, 0 failed, 6 ignored**, twice. Native `target/codemode-native-codec-tests.log` and `target/codemode-native-codec-tests-repeat.log`. Includes the **5** added agent codec tests and existing **35** ToolProgram tests; do not add them again to the total. |
| Focused checks | CLI `remote::protocol::tests`: **8 passed**, a separate focused run, **not a full CLI suite**. After fixing the introduced `types.rs` test-module-placement lint, the focused **5** agent codec tests passed again; fresh native build and fmt passed. Native cargo check for agent/CLI and diff checks passed. |
| Package checks unchanged | **124 passed, 0 failed, 0 ignored**; package strict all-target Clippy and fmt passed in `target/codemode-content-parts-{tests,clippy}.log`. Package source and Code Mode DLL unchanged; no fresh package rebuild claimed. |
| Native strict Clippy — BLOCKED | Combined agent/CLI `--all-targets -- -D warnings` is **not green**: existing TUI **64** lints plus SDK/telemetry dependency blockers, recorded in native `target/codemode-native-codec-clippy.log`. Scoped agent `--all-targets --no-deps -- -D warnings` remains blocked by **5 existing lints**: `agent.rs:508` unwrap-default, `agent_loop.rs:915` doc indentation / `1623` single-match / `1457` existing test-module placement, and `events.rs:23` enum size. Final log: `target/codemode-native-codec-agent-clippy-final.log`; no remaining diagnostic in the new codec changes. No unrelated cleanup or suppression. |
| Smoke provenance boundary | `codemode-native-codec-smoke-1` also passed 59 cases, but predates the test-module move/rebuild; **final `-2` is current**. Earlier content-parts `-2` / 1,123-test evidence below remains historical. Current smoke checks the existing payload/lifecycle/snapshot boundaries, not full child-history or live-provider parity. |

Current evidence files:
`pi-rust-codemode/target/codemode-native-codec-smoke-2/{results.json,provenance.json}`.

| Windows artifact recorded in current codec provenance | Verified SHA-256 |
| --- | --- |
| Host `pi-rust-codemode/target/debug/rpi.exe` | `cc4afacd0d94d390959e3361a6630f29b9451cc6d82b7e8f3f0a718b6110fe13` |
| Code Mode `rpi-package-codemode/target/debug/rpi_codemode.dll` | `5fccaaa062bdebb1fe74294a470d18816cbb59ffe8e79ad43196e220e8ebdd6f` |
| Fixture `pi-rust-codemode/target/codemode-parity-fixture/debug/codemode_parity_fixture.dll` | `27830e8fe1ca4a1f94918c1be51912472d34bba6ed3ace716dd933339eb23bac` |
| Gate `rpi-package-codemode/target/debug/rpi_permission_gate.dll` | `b52ce8ce8f10e032b701eb53cbca01e1abe04d567843aa96e2be9d880542cb34` |

## Historical third-increment evidence checkpoint (before codec follow-up)

| Evidence | Proven scope / limits |
| --- | --- |
| Fresh Windows host + Code Mode DLL + fixture + gate | `pi-rust-codemode/target/codemode-content-parts-native-smoke-2/results.json`: **PASSED, 59 cases, all true**. All four provenance hashes below verified against actual local artifacts. |
| 13 new native smoke cases; previous 46 retained | Direct mixed baseline; large text/image emit; two separate large calls interleaved with inline; subset `[2,1,0,1]` (same image twice, one execution); caught large error content with completed outer result; emit then JS throw; pinned release; 32 MiB measurement overflow preserving first PNG; negative indices; NaN scalar sticky failure with no subsequent write; toJSON/prototype resistance; terminal overrides earlier PNG with two child executions. |
| Payload / history proof boundary | Provider validates actual image bytes for **each copy**, including the two-image case, before normalized direct-baseline comparisons. Exact native end ordering/hashes, engineSummary without base64/private attachment parts, intent/finish lifecycle and bounded final snapshots checked. **Not full native history parity** or live-provider qualification. |
| Package checks | **124 passed, 0 failed, 0 ignored**; strict all-target Clippy and fmt passed. Logs: package `target/codemode-content-parts-{tests,clippy}.log`. |
| Broad native regression | Default-parallel SDK/agent/extensions/harness: **1,123 passed, 0 failed, 6 ignored**, twice. Native `target/codemode-content-parts-tests.log` and `target/codemode-content-parts-tests-repeat.log`. Existing both-driver full-reference 22-scenario test and new content-parts scenario test are **test functions**, not separate extra scenario counts. |
| Failed content-parts attempt | `codemode-content-parts-native-smoke-1` **FAILED after 53 cases, before overflow**: masking numeric handle 3 corrupted the real `32 MiB` diagnostic. Implementation fixed; expected needle not weakened. Never cite `-1` as passing. |

Historical third-increment evidence files (not current host qualification):
`pi-rust-codemode/target/codemode-content-parts-native-smoke-2/{results.json,provenance.json}`.

| Windows artifact recorded in historical third-increment provenance | Recorded SHA-256 |
| --- | --- |
| Host `pi-rust-codemode/target/debug/rpi.exe` | `5dba0fa76a699b79f9b286721cd8b2ab1e21b79f3f30536090d086d76d2dc7c7` |
| Code Mode `rpi-package-codemode/target/debug/rpi_codemode.dll` | `5fccaaa062bdebb1fe74294a470d18816cbb59ffe8e79ad43196e220e8ebdd6f` |
| Fixture `pi-rust-codemode/target/codemode-parity-fixture/debug/codemode_parity_fixture.dll` | `27830e8fe1ca4a1f94918c1be51912472d34bba6ed3ace716dd933339eb23bac` |
| Gate `rpi-package-codemode/target/debug/rpi_permission_gate.dll` | `b52ce8ce8f10e032b701eb53cbca01e1abe04d567843aa96e2be9d880542cb34` |

These are recorded local checkpoints, not source-review/commit approval, release,
installed-binary, deployment, CI-green, Linux/macOS or full-plan completion claim.

## Historical second-increment evidence (not current qualification)

| Evidence | Proven scope / limits |
| --- | --- |
| Fresh Windows host + Code Mode DLL + fixture + gate | `pi-rust-codemode/target/codemode-reference-native-smoke-3/results.json`: **PASSED, 46 cases, all true**. Four provenance hashes below verified against actual binaries. |
| 11 new real-native smoke cases | Four direct baselines and four composed counterparts: 2 MiB text, actual >1 MiB PNG, legacy plain error, terminal success. Plus frozen lazy inspection/three rejected mutations, 20 inspect/release loops with once-only accounting, and select-then-JS-throw. |
| Large-payload proof boundary | Isolated sessions compare canonical native end payload hashes against direct baselines; image data may be normalized by native handling. Actual local fixture-provider requests validate image delivery, streamed native transcript markers validate lifecycle, and final snapshots are bounded. **Not** full get_messages large-history equality or live-provider qualification. |
| Historical package checks | **92 tests**, strict all-target Clippy and fmt passed. |
| Historical focused native checks | **33 agent tests**, **206 extension unit tests + 5 integration tests (1 ignored)** passed. One new extension test covers **22 scenarios across both drivers**, not 22 test functions. |
| Historical broad native regression | Default-parallel agent/extensions/harness/SDK suite passed **1,117 tests, 0 failed, 6 ignored**, twice: `target/bridge-owner-all-parallel.log` and `target/bridge-owner-all-parallel-repeat.log`. Historical **1109 / 6 ignored** and the 35-case smoke qualify the first increment only. |
| Retirement regression | A broad parallel attempt exposed cross-session pinning in the global bridge scan. Owner identity now filters weak registry entries **before** upgrading. The deterministic regression fails with the original scan; existing immediate-drop assertions remain unchanged. No claim about historical CI failure. |
| Failed reference attempt | `codemode-reference-native-smoke-1` failed an obsolete harness terminal-summary assertion. Terminal completion now directly attaches the original native receipt. Never cite `-1` as passing. |

Historical second-increment evidence files:
`pi-rust-codemode/target/codemode-reference-native-smoke-3/{results.json,provenance.json}`.

| Actual Windows artifact recorded in provenance | Historical verified SHA-256 |
| --- | --- |
| Host `pi-rust-codemode/target/debug/rpi.exe` | `185f0e7c031fb703c5a59d18047359e8a42e981d91c5658ed907205b9eb01383` |
| Code Mode `rpi-package-codemode/target/debug/rpi_codemode.dll` | `85761d9c99e102a8150ece8f043b4946cdad10edeadc5aff3dbcfc7d2b35f4df` |
| Fixture `pi-rust-codemode/target/codemode-parity-fixture/debug/codemode_parity_fixture.dll` | `d849ab1773aa0a463b0fc7f61e393d7c3066eba8946e24e20c6e9c632a29a245` |
| Gate `rpi-package-codemode/target/debug/rpi_permission_gate.dll` | `b52ce8ce8f10e032b701eb53cbca01e1abe04d567843aa96e2be9d880542cb34` |

## Implemented versus proven

| Area | Implemented behavior | Evidence / qualification boundary |
| --- | --- | --- |
| Negotiation / ABI | Protocol v1 adds native optional `resultReferences:1` and sixth `contentAttachments:1`; extension requires all **six** exact integer v1 caps (also resultEnvelope, hostAccounting, parentProgress, losslessValues). Fail closed, no fallback. SDK 0.3.20 pin and ABI/layout unchanged. | Package rejection checks; current real smoke checks all six. Native clients omitting resultMode and non-opted plugins preserve original behavior. |
| Reference transport | Extension starts `resultMode:'reference'`; poll manifest has version/id/byteLength/contentBlocks/isError/terminate. read_result uses strict byte offsets, integer 4..65536 limit, UTF-8 boundaries, stable EOF and ≤1 MiB responses. release_result is owner-only; dispose independent. | Native/package tests cover malformed manifests/chunks, lifetime and cleanup; real large forwards prove host/DLL integration, not all race cases. |
| Native storage | 16 references/program, shared 64 MiB tree quota: `2*serialized_bytes+4096+128*details_nodes+256*content_blocks`. Quota implies serialized size below 32 MiB; package guards advertised size at 32 MiB. | Unit quota/release checks and 20 real release loops. **No hard RSS ceiling**: native clone/serialization precedes quota admission; Rust JSON lies outside 32 MiB guest heap. |
| Small mutable selection | Nonterminal ≤1 MiB results materialize and release native storage; tools.result uses current guest-mutated content/details, deleted/undefined details → null, native isError via WeakMap. | Existing small direct/error/deleted-details cases retained in 59-case run, plus package checks. tools.release on genuine small results is a native no-op. |
| Large read-only selection | >1 MiB frozen lazy genuine WeakMap receipt. Inspection materializes/deep-freezes whole receipt under 32 MiB guest heap. tools.result forwards original native result without materialization. | Real large text/image/error direct-baseline comparisons and inspection/mutation rejection. Admitted receipt need not fit inspection heap. Large content emission supports ordered mixed parts; multiple whole-result selection remains unsupported. |
| Attachment / drivers | Metadata-only opted-in Done envelope resolves through captured live owning invocation before destroy in **both** drivers; preserves native content/details/isError/terminate, strips usage/names before host merge. | 22 new both-driver scenarios in one native test; real forwards verify native pipeline/provider image delivery. Invalid/released/foreign/cancelled/stale/mismatched selections fail closed. |
| Release / failure / terminal | tools.release invalidates large receipt even cached reads; selected release fails. Later failure discards whole selection; eligible explicit emits survive throw/OOM/deadline with outer error and last diagnostic while owner is live. Emitted receipts are pinned; release fails before native free. Terminal child never resumes JS; attaches original native receipt, cancels/disposes remaining calls. Finish cancels/drains/clears refs. | Package/native tests plus select-throw/release/terminal-success smoke. Large **legacy terminal error remains uncovered** because StepErr has no terminate; small structured terminal error already covered. |
| Accounting / gates | Host retains finalized direct-child usage/names/termination once after drain, including ignored/disposed/thrown outcomes. Sequential parent exclusion, native hooks/approvals, panic drain and termination latch preserved; no retry/replay or rollback. | Focused native tests and real 20-loop accounting/lifecycle checks. No duplicate accounting from attachment. Two complete default-parallel native regressions passed after the scoped-retirement fix. |
| Business JSON / hygiene | Preserve business runtimeKey/runtimeHandle/__rpi/__rpiRuntimeKey; ABI injected top-level __rpi remains host-owned. Both drivers guard actual credentials/metadata across callbacks, Pending, Done and errors, not ordinary names/numbers. | Existing differential/driver checks and real business/binding echo cases retained. Not malicious-plugin isolation; arbitrary encoding/direct I/O unguarded. |
| Catalog / aliases | Effective native batch snapshot and native hook finalization; added names do not authorize/refresh. Helper collisions call/list/emit/result/**release** accessible via tools.call. | Neutral differential checks and real alias case. Self/ancestors still excluded. |
| Inline output | Generated/emitted/small exact content stays 128 blocks / 1 MiB; separate 1 MiB final envelope. Native attachment bypasses inline payload limits, **not retention quota**. | Existing invalid-content/size cases retained. Ordered large/inline emission is supported within the attachment bounds below. |
| Mixed content attachments | tools.emit(receipt, indices?) uses captured ops/WeakMap identity without JS reads or native payload FFI. Ordered inline/reference parts; result.content forbidden, full resultReference mutually exclusive, strict keys, live ActionBridge resolution before destroy in both drivers. Caught native error content need not make outer result erroneous. | New smoke mixed/subset/repeated-image/error/throw/pin/overflow/index/terminal cases; no duplicate child execution/accounting or payload in summary. |
| Measurement / bounds | measure_result_content counts canonical serialized selected blocks via to_writer byte counter (no array punctuation/huge allocation). Omit/null all, [] none, reorder/repeat valid; native owner/range/cancel checks. Native 256 parts/blocks and 32 MiB final array checked before each clone. Engine 128 selected parts/blocks and combined 32 MiB **including 4096-byte reserve** for final diagnostic with reason bounded to 512 UTF-8 bytes/worst-case JSON escaping. | Generated inline 128 blocks/1 MiB and separate 1 MiB envelope unchanged; native payload not inline; no hard RSS ceiling. |
| Raw index safety | Validate before serialization in both guest paths: undefined/null all, otherwise max128 nonnegative integer primitives, once-read frozen null-prototype snapshot; captured intrinsics/property definition/primitive JSON encoding avoid setters/toJSON/NaN/Infinity/boxed-object widening. Native range validation remains authoritative. | Sticky failure prohibits later starts; new adversarial index smoke plus package tests. No ID-based authority forgery. |
| Failure eligibility probe | After cleanup an uncertain ordinary host-action/budget failure may recover only attachment eligibility via exact empty-index read-only measurements for every distinct already-emitted receipt (max16): matching resultId, zero blocks/bytes, exact bounded shape/frame. Sticky/root failure remains; no failed-operation retry, starts, payload reads or replay. | No recovery for cancelled/full-selection/terminal/known retained-cleanup failures; native final owner checks run again. Unit/source coverage, not a claim that all cleanup races are proven by smoke. |
| Native result codec | Shared canonical serialization, borrowed ToolProgram reference encoding and optional RPC textSignature preservation described above. No Deserialize or result-internal isError authority. | Historical codec 1,128-test regression twice, focused codec/protocol checks and rebuilt-host 59-case smoke; groundwork only, not native child journal/events/history completion. |
| Progress / history baseline | Bounded parent lifecycle/count updates and intent/finish audit markers; Never-replay interrupted programs; no synthetic assistant tool-result history. | Native/source and smoke markers only. Not complete child events, bounded native history/query/replay/recovery or Web/ACP parity. |

## Remaining full-plan acceptance

| Remaining area | Required before claiming completion |
| --- | --- |
| Complete native child events | Preserve original bounded child identity/order/updates/content/final events and cancellation/late-event behavior without credential leaks. |
| Bounded native history/query/replay/recovery | Native transcript authority, bounded resumable reads, exact finalized outcomes, interruption/restart and snapshot/live sequence continuity. Audit markers and bounded final smoke snapshots are insufficient. Never replay uncertain effects. |
| extcore / HTTP / ACP / Web | Typed core/HTTP/ACP reads, dedicated core/HTTP live feed, optional ACP live subscription and frontend cards/reconnect/reload are implemented and locally qualified above. A targeted actual-native Web read-only recovery is also recorded, but real-native ongoing overlap and ACP live delivery remain unqualified; the one-session browser proof and mocked public browser tests/HMR are not full parity or production qualification. |
| Protected recursion/self/ancestors | Explicit approval and tested authority/lifecycle/recursion policy; current exclusion prevents unrestricted full-toolset parity. |
| Wider real integration | Linux/macOS real native execution, live providers, reload races and separately identified installed binaries require fresh evidence. Local Windows fixture-provider success is not those claims. |

See [host contract](codemode-host-contract.md),
[parity contract](codemode-parity-contract.md), and
[package guide](../packages/rpi-codemode/README.md). Documentation does not upgrade
running binaries or extend the native-plugin security boundary.
