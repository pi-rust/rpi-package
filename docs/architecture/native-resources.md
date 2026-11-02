# Neutral native resources: core contract and ownership

`rpi-extcore` owns the APIs below; HTTP/ACP retain their existing contracts.
Work owns installed-reference resolution, authorization, presentation and wire error
mapping. No Work workflow, package resolver, transcript store or ABI belongs here.
Native owns exact selection validation, registries/install transactions, resource
leases, loaded observations, binding CAS and native history.

## Requests and results

- `resources::ResourceSelection`: complete v1 native declaration, typed main
  package/scope, explicit Skills/extensions/instructions and default/only/none tools.
  Omission from creation is legacy; an empty declaration is explicitly exact.
- `SessionOptions.resources`: optional `ResourceStartup::Exact` or native
  `Transition`. These are trusted launch data, never arbitrary argv/environment.
- `service::CreateSessionRequest.resources`: optional exact selection. Service
  records it before launch, verifies configured ready plus actual resource state,
  and never retries in legacy mode. Ordinary reopen restores this declaration.
- `Service::apply_resources`: expected application revision, stable operation ID,
  complete target and optional `supersedes_operation_id`. Native expected
  revision/fingerprint are captured separately. Persist intent before stopping;
  identical payload retries use the original native expected state. A conflicting
  ID/payload or stale application revision conflicts. Supersede is explicit and
  is not rollback; see below.
- `Service::resource_configuration`, `resource_operation`, `resource_operations`:
  bounded registry facts. Failed legacy opt-in has no active configuration
  (`None`), but its original intent and all terminal identities remain queryable.
  `HostSession::resource_state` is bounded actual generation/binding/inventory JSON.
  A registry success never establishes readiness for another native generation.
- `Service::resource_saved_history(id, ResourceHistoryRequest)`: bounded,
  nonexecuting canonical native history for closed/blocked sessions. Returns
  native messages with entry IDs, leaf, source-anchored opaque cursor, filtered
  durable entries, native binding and pending facts, `executable:false`, and
  the current blocked operation ID if any. No executable host, resource loader,
  extension, settings/provider startup or alternate transcript store is opened.
  Live owners conflict: use the live history API. Pending history remains readable.
- `Service::resource_project_fence(project_id)`: immediate, short caller-owned
  exclusion for authorized filesystem CAS/snapshot/validation. Holds lifecycle
  ownership and admission gates for matching live native hosts; fresh canonical
  native queue/recovery/durable idle checks, no retirement/stop. Captured host Arcs
  are fenced; unrelated projects stay independent. This excludes ONLY this
  Service's owners. Work must fence ACP/other/external owners separately or refuse.
  Unsupported live external-tool/client-I/O authority is rejected, not assumed idle.
  Retain the guard inside any blocking worker through completion/cancellation.
- `Service::manage_resources`: typed list/inspect/install/update/reinstall/remove/
  recover, explicit project ID/scope/kind/name/source. Only the trusted configured
  executable is invoked, with fixed argument vectors; no Cargo/build option.
- `Service::release_idle_resources`: explicit owned-host release, preserving
  registry/history for restore. Global leases can still be held by external hosts;
  native conflict is authoritative. Release never unlinks locks or kills strangers.

## Admission, persistence and failure

Service's lifecycle mutex serializes restoration, replacement, release and forks.
Each host's existing run lease is shared by execution/configuration; retirement
permanently fences captured old host Arcs. Mutating native command entry points,
queue submission, durable mutations and run APIs use that lease. Native state,
queue, dialogs and durable projection must be idle before stopping. Native's
strict pending-history checks are a second authority, not a substitute for core
admission. Two sessions keep independent selections and native IDs.

Resource records live in the existing Service registry owner, separately from
transcripts and sidebar metadata. They include complete selection, monotonic
application revision, last verified observation and at most 64 stable operation
identities (maximum 2 MiB per session); overflow refuses admission, never evicts.
Corrupt records fail Service construction; write failure cannot authorize execution.
Intent/blocked records prevent ordinary reopen. Candidate failure is blocked,
not automatic rollback. Ordinary retry is only the identical operation, expected
binding and target. An explicit new operation may supersede ONLY the latest
pending/blocked intent, after fresh canonical native/durable idle AND original
uncommitted binding proof, independent owned child exit and normal headroom
preflight. The existing native shared observation lock stays owned through the
single synced atomic registry write: mark old `superseded` (retain its payload,
identity and original CAS), add the new `pending` intent at that SAME application
revision/native CAS. Only Applied advances application revision. Replaying the
old ID returns its terminal Superseded result; changed payload still conflicts.
If native already committed or its state differs/uncertain, supersede refuses:
recover the ORIGINAL ID/payload. Native idempotent startup may confirm that
original commit and publish application completion without another native commit.
Torn native history requires explicit native inspection/recovery;
core never repairs it or tries alternate selection automatically.

Apply runs in a Service-owned task retaining the Service and lifecycle exclusion
if the requesting future disappears. Shutdown waits for lifecycle ownership.
Native startup/readback have finite deadlines; uncertain startup/readback/persistence
keeps the record blocked. Replacement waits for old child exit before any startup;
exit timeout refuses replacement rather than admitting a second transcript writer.
Successful target observation (including native operation association) is persisted
before execution becomes reachable. Native generation is always read afresh.

Live external MCP authority cannot be serialized; incompatible external tools and
client-file-I/O owners are rejected before stop. Model/provider/thinking choices
are read before replacement and restored before publication; permission launch
choices remain host-owned. Native runtime model/thinking changes are NOT native
transcript persistence. Successful owned idle release/shutdown records a separate
`current_settings` stop capture (time/host identity) which ordinary restore consumes.
Immutable operation `restore_settings` survives retries; historical apply `settings`
are never used to overwrite newer runtime choices. Live apply/fork clears only its
own consumed stop capture; forking leaves an offline source capture intact. Crash,
external stop and unsuccessful shutdown capture are NOT preservation guarantees.
Configured forks pass exact selection to native and retain native-owned
history/navigation semantics. Omitted targets copy the actual current leaf,
including a resource custom entry; explicit historical targets retain ordinary
native message-target semantics. The destination starts with the source's current
validated declaration/inventory at binding revision 1, without its operation ID
or retry ledger—even when the chosen historical message predates that selection.
Copied source entries remain history, not destination retry authority. Native
stages this initialization inside its existing atomic fork publication; core does
not guess a message ancestor, rewrite a transcript, or implement another fork
engine. Fresh destinations clear app operation history and consumed stop capture;
ordinary configured reopen validates the new native authority as usual.

## Non-executing capability and local CLI boundary

Bounded `--help` must advertise `--resource-capabilities`; only then query the
standalone 16-KiB v1 profile. Require the actual requested feature/version before
resource CLI flags. Never probe unknown subcommands, infer ABI compatibility,
execute a provider or widen mode as fallback. Actual ready/state remains authoritative.
Local management stdout/stderr and JSON are bounded, with deadlines and owned
process cleanup. Installation receipts remain native facts; catalog is not loaded
state. Source compilation requires separate explicit native authorization.

## Canonical saved reader and offline admission

The pure bounded capability profile must explicitly offer `savedSessionRead` v1
before `--resource-session-read`. Core resolves only owner-known native locations
with a 2-second/256-directory/32,768-entry discovery budget and exact v4 header
identity, rejects duplicates/escapes, then delegates all branch replay and pending
semantics to native. Root resolution failure is explicit; no alternate path or
unconfigured resource fallback. Native recorded cwd must equal the owner project.

Reader budgets: request64KiB, response8MiB, total reads64MiB, mutations65,536,
lanes32, messages128/5MiB, custom entries1,024/2MiB, cooperative native10s and
contained caller15s. Core requests only native durable custom entries; `rpi.value`
and full SessionValues/settings dumps are forbidden. Pages are oldest-first on
the native canonical main branch; source fingerprints cover transcript/progress
bytes. Source/leaf changes reject cursors. Malformed, oversized, busy, unsupported
or stale sources fail explicitly, never empty-success or repaired history.

Offline apply/supersede/retry requires this FRESH native queue/recovery/progress
proof plus reduction of the canonical durable journal. Cached projections and
saved observations never authorize execution. A saved observation supplies only
the original native CAS expectation. Missing legacy lock or missing native idle
proof is Unsupported, not assumed empty. Native uses the pre-existing noncreating
shared writer lock and strict read-only cold replay; core retains a compatible
shared lease through registry decisions (safe platform primitive in the existing
`rpi-mcp-core` FFI boundary, no new dependency or unsafe core code). Native startup
then claims its exclusive writer lock and revalidates original CAS/payload itself.

## Verification and consumer integration

Real-native tests require explicit opt-in: set `RPI_RESOURCE_NATIVE_BIN` to the
absolute path of a trusted, explicitly built native `rpi` regular executable,
and `RPI_RESOURCE_TEST_EXTENSION` to the explicitly built fixture cdylib. There
is no PATH lookup, binary download, guessed release or unconfigured fallback.
The caller owns executable provenance and authorization. For example (POSIX
shell; use the platform's executable/cdylib filenames):

```sh
export RPI_RESOURCE_NATIVE_BIN="/absolute/native-target/debug/rpi"
export RPI_RESOURCE_TEST_EXTENSION="/absolute/fixture-target/debug/libplugin_stub.so"
cargo test --locked -p rpi-extcore --test resources_native \
  trusted_native_resource_service_lifecycle -- --ignored --exact --nocapture
# Run separately; requires native new-session fork binding initialization.
cargo test --locked -p rpi-extcore --test resources_native \
  trusted_native_resource_binding_leaf_fork_regression -- --ignored --exact --nocapture
```

Run the process tests separately: they use isolated roots and loopback providers
and deliberately control only their own test-process environment. Use caller-owned
build/test-tmp/evidence directories. Focused contract/negative fixtures precede
real-native tests; independent broad acceptance remains required. Work may use an
owned temporary Cargo patch during integration, not a final sibling path dependency.

The binding-leaf fork regression is covered by the second opt-in test. It checks
current-leaf fork, clone, explicit historical assistant-message fork, fresh native
identity/revision 1 with no inherited operation authority, current exact selection,
unchanged source fingerprint/binding and offline stop capture, restored runtime
settings, and ordinary configured reopen of every destination. The native fork
fix is required; an older native which rejects the binding leaf fails explicitly,
with no guessed-branch or legacy fallback. The lifecycle test separately covers
reader/settings, blocked history, native/durable offline refusal, explicit
supersede, legacy failed opt-in correction, caller cancellation after Pending,
and native-commit/app-pending recovery using the original operation ID/payload.
Focused checks do not replace independent acceptance. Stage C and downstream
Work integration remain gated on parent sign-off.
