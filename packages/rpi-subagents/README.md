# rpi-subagents

Registers `delegate_task`, a bounded subagent runner. It uses the host's
`ChildAgent` runtime action (24), streams text via the normal tool-progress
callback, and returns a final JSON report with `kind: "rpi-delegation"`.

## Choosing and specifying a delegation

`delegate_task` creates a **host-managed, in-memory child session**. It does not
launch the `subagent` subprocess tool, create a separate process, provision a
worktree, or provide a filesystem sandbox. The child uses the parent's cwd,
workspace and extension state. Coordinate file ownership before delegating writes;
parallel children must not make conflicting edits. Use a subprocess-oriented tool
only when its separate launch/configuration semantics are actually wanted; this
package exposes no subprocess fallback or launch options.

Give the child a self-contained `task`: objective, necessary context and paths,
allowed files/scope, constraints, acceptance checks, and the expected final report.
Do not assume that the parent's conversation is available. `role` is only a label;
it does not select a predefined agent, prompt profile, tools or permissions.
Children cannot delegate further. The caller remains responsible for reviewing
reported changes, checking acceptance and dealing with partial/turn-limited work;
`status: "completed"` alone is not proof that the requested work is complete.

Example (only model-supplied fields):

```json
{
  "task": "Review packages/rpi-subagents/README.md against src/lib.rs. Read only; do not modify files or delegate. Check parameter defaults and host-managed versus subprocess wording. Report mismatches with paths and suggested corrections; acceptance: every exposed parameter checked.",
  "role": "reviewer",
  "maxTurns": 8,
  "timeoutSeconds": 300
}
```

## Developer invocation context

Children run in independent **in-memory sessions inside the host**, not a new
`rpi` process. They inherit the parent's current model/provider binding (including
private authentication and endpoint), thinking level, prompt/resources, active
project tools, working directory and approval hooks. Interactive model switches
and explicitly loaded providers therefore work without reconstructing CLI flags.
No key or provider credential is serialized into plugin arguments.

Permission requests and `ask_user` use the parent's UI mailbox directly. With no
interactive consumer, approval retains its explicit rejection/error behavior;
there is no implicit allow or disabled permission gate. Shared extension tools
remain parent-owned: their injected `__rpi.sessionId` names the parent's extension
state; the returned `sessionId` separately identifies the child's transcript.
The worker uses host-injected `__rpi.runtimeHandle` and `runtimeKey` for that
specific tool invocation, and verifies session identity (or cwd when no ID is
available) against `GetExecutionContext`. Only host function pointers are cached
globally, never a registration's `user_data`. Duplicate DLL registrations and
failed candidate reloads cannot replace an existing call's execution binding.
Expired handles and context mismatches fail closed.

## Bounds and cleanup

- `task` is the only required model field; there are no action-dependent required
  fields. It must be a string with non-whitespace text, at most 20,000 **UTF-8
  bytes**. The original text (including surrounding whitespace) is forwarded.
- `role`: omitted or non-string values default to `worker`. Strings are trimmed;
  the trimmed value must contain 1–200 UTF-8 bytes. Empty/whitespace-only strings
  are rejected rather than defaulted.
- `maxTurns`: defaults to 8, clamped to 1–32; enforced per child, without changing
  process-wide environment variables.
- `timeoutSeconds`: defaults to 300, clamped to 10–1800.
- Budgets use JSON unsigned 64-bit integer parsing: zero clamps to the lower
  bound; larger representable integers clamp to the upper bound. Missing,
  negative, floating-point (including `1.0`), wrong-type or unrepresentable values
  use the default.
  Schema defaults and budget descriptions guide model input; the parser applies
  its own defaults and clamping. The schema deliberately omits character-count
  `maxLength` constraints:
  byte limits and role trimming cannot be expressed accurately that way.
- The top-level schema permits additional properties for host injection but
  exposes only task/role/budget fields. Reserved invocation metadata is described
  in the developer section above; unknown model fields are not runtime options.
- Streamed and final text share a 4 MiB total budget; at most 8192 text deltas.
- The host retains at most 16 child handles, including completed tasks awaiting
  disposal, and at most 16 active/cleaning-up children. The plugin always disposes
  its handle on completion, error or cancellation.
- Parent completion/abort, session switch/close, plugin unload/reload, timeout and
  tool cancellation stop the child. The host signals cooperative cleanup and
  allows up to two more seconds for foreground settlement. Background drivers
  retain their own parameters, DLL mapping and cleanup capacity until their final
  foreign callback returns, including provider requests and approval handlers;
  an uncooperative thread is not forcibly terminated. A cancelled invocation can
  still poll/cancel/dispose existing children with its original capability, but
  cannot start new work. Queued starts also inherit invocation cancellation.
- The child's tool registry excludes `delegate_task`; its harness and runtime
  invocation reject further child creation, including through alias tools. Its
  prompt also forbids delegation. This is not an OS sandbox:
  allowed shell/other tools retain their normal capabilities.
- Tool destruction cancels and **joins the plugin worker before returning**, so
  no plugin thread can outlive the host bridge or DLL mapping.

The host's child API can restrict `tools` to a parent-active subset, but the
`delegate_task` model schema remains task/role/budget only. It cannot expand the
parent's tools, override credentials or discover extra extensions.

## Developer compatibility notes

Top-level `additionalProperties: true` accepts reserved `__rpi` invocation
capabilities without exposing them as model schema properties. Execution requires
valid host-injected capabilities and matching session/project context.

Requires a host implementing `RuntimeActionId::ChildAgent` (24), with per-call
`__rpi.runtimeHandle` and `runtimeKey` capabilities. Unsupported
hosts fail with an upgrade diagnostic. There is deliberately **no legacy RPC
subprocess fallback**, because it could silently switch models, cwd, provider
loading scope or permission behavior. `RPI_SUBAGENT_BIN` is no longer used.
`RPI_SUBAGENT_CHILD=1` still suppresses registration for external legacy launches.

The package retains the workspace's published SDK pin. Its documented action ID
is a small compatibility shim, following `rpi-ask-user`, until that SDK enum is
published. Host-side SDK validation remains authoritative; the vtable is unchanged.

## Verification

Package tests read the same local schema builder used by registration (not a
copied JSON fixture), validate JSON/nested property structure, and check defaults,
UTF-8/trim behavior, budget normalization and host-metadata tolerance against the
actual parser. They complement the host integration checks below.

```sh
cargo test -p rpi-subagents
cargo build -p rpi-subagents
```

In `pi-rust`, set `RPI_TEST_SUBAGENT_LIBRARY` to the built DLL/so/dylib, then run:

```sh
cargo test -p rpi-cli --test subagent_packages
```

This loads the actual plugin and uses a local faux provider/temporary project to
verify progress, project cwd, inherited tools and parent approval UI, including
headless rejection, alias recursion denial, duplicate DLL registration and
candidate-reload rollback, repeated cancellation beyond the handle capacity,
and harness/provider/DLL release. No real API key is required. See
`pi-rust/docs/extensions/subagents.md` for the complete host protocol.
