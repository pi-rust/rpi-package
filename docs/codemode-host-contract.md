# Native Code Mode host contract (implementation v1)

The host (`D:/Projects/pi-rust-codemode`) and extension
(`D:/Projects/rpi-package-codemode`) implement this contract together, without
MCP or another model invocation. The **first full-parity increment is
implemented, not the full parity plan**. See the
[parity contract](codemode-parity-contract.md) and
[status matrix](codemode-full-parity-matrix.md) for evidence and outstanding work.

## Native invocation and operations

- Existing runtime action channel: `RuntimeActionId::ToolProgram = 30`, protocol
  version 1. The extension requires all four exact integer capabilities
  `{resultEnvelope:1,hostAccounting:1,parentProgress:1,losslessValues:1}` before
  execute or list output. Missing, older or mismatched capabilities fail closed;
  no direct-call, subprocess or legacy-parser fallback. The published SDK pin
  remains unchanged; the extension uses a raw action-30 shim.
- Every operation authenticates the actual host-owned invocation binding:
  injected `__rpi.runtimeHandle` as runtime-call user_data and `__rpi.runtimeKey`
  as reserved `__rpiRuntimeKey`. Credentials are not guest/model APIs.
- `list`: `{op:"list",offset?:0,limit?:32}` returns
  `{protocolVersion:1,capabilities:{resultEnvelope:1,hostAccounting:1,parentProgress:1,losslessValues:1},tools:[{name,description,parameters}],nextOffset:number|null}`.
  Only the effective active invocation tool set is visible, never an arbitrary
  root registry. Self/ancestor tool names are excluded. Catalogs follow native
  batch boundaries: later invocations see their batch's effective snapshot;
  `addedToolNames` neither authorizes a tool nor refreshes a running program.
  The extension catalog is frozen for that execution. Exclusion means this is
  not 100% full-toolset parity.
- `start`: `{op:"start",name:string,arguments:object,timeoutMs?:30000}` returns
  `{requestId:string,status:"running"}`. Identity is host-generated. Execution
  reuses argument preparation/schema validation, before/after hooks, approvals,
  cwd, effective tools and native safety rules, not another model turn.
- `poll`: `{op:"poll",requestId}` returns
  `{requestId,status:"running"|"done"|"failed",result?:{content,details,terminate,addedToolNames,usage?},isError?:boolean,error?:string}`.
  Done is a finalized native result, possibly erroneous; failed denotes
  lifecycle/bridge failure. Parent-associated lifecycle/count progress uses the
  normal outer update callback, not raw child partial payloads.
- `cancel` / `dispose`: `{op,requestId}` returns cancelled/disposed. Idempotent
  within the authenticated invocation where practical; never implies rollback.
  Cleanup stays available after cancellation while the binding is valid.
  Native ownership must outlive still-running workers; no premature DLL unload.
- Timeout is 1..120000 ms. Limits: 128 starts / 8 active request futures across
  an invocation tree, 16 retained requests per invocation, arguments/results at
  most 1 MiB each (reject, never silently truncate). Host catalogs allow 4096
  entries; the extension additionally bounds discovery to 1024 entries / 1 MiB,
  with pages of 1..128. Never retry/replay automatically. New starts fail after
  parent cancellation/completion, invalidation or session close/switch.
- The program tool or its loop must be Sequential. The extension registers
  `parameters["x-rpi-execution-mode"] = "sequential"` without ABI changes.
  Independent nested Parallel tools may overlap. Cancellation/disposal publishes
  failure promptly but retains permits/gates through native cleanup. Panics
  cancel/drain descendants before gate release. Noncooperative native code may
  delay the parent indefinitely rather than permit unsafe overlap. Finalized
  termination is latched before fallible reply encoding.
- Root and child retain their own effective tools, cwd and approvals. Nested
  work is a descendant of the existing assistant-to-program call, not fabricated
  assistant calls. Do not route nested ToolExecutionStart/MessageEnd through
  SettlingEmitter. Preserve checkpoint-before-side-effect, bounded intent/finish
  audit markers and Never-replay recovery for interrupted programs. These markers
  are not original native child history snapshots.

## Lossless business values and native output hygiene

Business schemas, arguments, results and guest returns preserve legitimate
`runtimeKey`, `runtimeHandle`, `__rpi` and `__rpiRuntimeKey` properties, including
nested data. There is no blanket recursive property-name blacklist. At the
native extension ABI, injected top-level `__rpi` is still host-owned, exactly as
for direct plugin calls; it is not available as a business argument slot.

An invocation-local host public-output guard captures the actual injected
metadata and credential. Both polling and wake drivers sanitize native callbacks,
Pending payloads, legacy Done, opted-in envelope successes/structured errors and
plain tool errors. Actual metadata objects and credential values (including
aliases, ordinary serialized JSON and collision cases) are guarded, not all
objects with similar names. Ordinary bare numbers are not blanket-redacted.
This is output hygiene, not a sandbox against malicious native plugins:
arbitrary encoding and direct plugin I/O are unguarded.

## Extension API and result selection

Selection uses the current guest-mutated content/details snapshot, not an
immutable copy. Missing selected `details` (including deletion or `undefined`)
is normalized to native `null` in the required result-envelope field.
Authoritative native `isError` remains independent of guest mutations.

`codemode` accepts `action:"list"|"execute"` (default execute), required nonblank
`code` for execute, and optional `timeoutMs` 1..120000 (default 30000). Code is a
JavaScript async body with `await`/`return`, not TypeScript. Guest APIs:

- `tools.<name>(arguments)` / `tools.call(name,arguments)` return finalized native
  results; ordinary `isError` rejects with useful text and the native object at
  `error.result`. Use `tools.call` for helper collisions `call/list/emit/result`.
- `tools.list()` copies the discovered frozen catalog.
- `tools.emit(resultOrContentArray)` selects native blocks; returning an object
  with its own valid `content` also promotes blocks. Existing summary semantics
  remain unchanged; nested JSON and intermediate images are not auto-forwarded.
- **New:** `tools.result(nativeResultOrErrorResult)` selects exact supported
  content/details and authoritative `isError` once. A captured WeakMap binds
  native child result identity (including caught `error.result`) to the error
  flag. Cloned/fabricated objects cannot supply authority. No summary is injected
  into selected details, and no synthetic error text is appended to an exact
  native error. The API returns `undefined`, not a new host result.
  Multiple selections, mixing with `tools.emit`, returning another content
  object, or invalid input/serialization cause sticky failure even if caught.
  Later throw/cancel/timeout/OOM/lifecycle failure/terminal child wins. Usage,
  added names and termination remain host-authoritative, never guest-forged.
- Native selections remain inline: 128 blocks / 1 MiB encoded content, plus a
  separate 1 MiB final-envelope limit. No large-value refs/chunks/attachments yet.
- `print` / `console.log/info/warn/error` capture bounded logs. No direct guest
  filesystem/network/process/require/import. RegExp compiler/runtime intrinsics
  are omitted, including dynamic/implicit regex creation. Deadlines are
  cooperative, not OS-enforced. Allocation exhaustion is latched outside JS and
  cannot be caught to restore host effects or success. Each invocation owns fresh
  QuickJS state, memory/stack/interrupt budgets and bounded logs/calls/results.

Registration opts into `x-rpi-result-envelope:1`. Success, termination and ordinary
program failure use versioned Done envelopes preserving native content, details
and `isError`; negotiation/infrastructure failures may still use ABI Err. The
host rolls up finalized direct-child usage, deduplicated added names and
termination once after descendant drain; the extension never sums/re-emits them
as its own accounting. After hooks retain native final authority.

Promise.all must start independent requests before waiting, not fake concurrency.
On every exit cancel/dispose pending requests. ABI poll is nonblocking, cancel
signals only, destroy joins the owner worker. No installs, deployment or Web/ACP
implementation changes are included. Complete child progress, original history
snapshots/recovery and Web/ACP child cards/reload remain full-plan work; approval
for a third UI dev worktree is not evidence of implemented UI changes.
