# rpi-codemode

Native `codemode` executes one bounded JavaScript **async function body** against
the tools already active in the calling invocation. No extra model turn, MCP
server, subprocess interpreter or TypeScript compiler. Ordinary tools remain
visible and usable normally. The **third full-parity increment is implemented;
overall full parity remains INCOMPLETE**. See the
[status matrix](../../docs/codemode-full-parity-matrix.md) for evidence and gaps.

## Latest native observation seam (not durable history)

The host now provides an **optional native observation seam**, not a production
child journal or new RPC capability. It observes genuine requested/prepared
arguments, original signed partials and the actual post-hook final result.
Existing private `tp-*` request/progress/audit identity is unchanged; a future
native durable entry ID is separate. Default emitters return no observer, so the
normal CLI still has **no installed child-history writer**. Six negotiated Code
Mode capabilities and the SDK ABI/layout remain unchanged.

Native **1,146 tests passed twice (6 ignored)**; the existing Windows smoke
remains **59/59** with a freshly rebuilt host and unchanged fixture/DLL/gate.
See the [observation checkpoint](../../docs/codemode-full-parity-matrix.md#latest-native-observation-seam-not-durable-history)
for exact scope, lifecycle tests, current artifact evidence, native Clippy blockers
and the still-pending durable history/core/adapters/Web work.

## Historical native result-codec checkpoint

The committed native codec follow-up adds camelCase `Serialize` for `AgentToolResult` and
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

Historical codec Windows evidence:
`pi-rust-codemode/target/codemode-native-codec-smoke-2/{results.json,provenance.json}`: **59 cases PASSED, all true**, with all four
artifact hashes verified. SDK/agent/extensions/harness passed **1,128 tests,
0 failed, 6 ignored, twice**; focused CLI protocol tests passed **8** (not the
full CLI suite). Package **124 tests and strict all-target Clippy remain passing**
from unchanged package evidence; **native strict Clippy is blocked**, not green.
See the [latest matrix checkpoint](../../docs/codemode-full-parity-matrix.md#historical-native-result-codec-checkpoint)
for hashes, logs, focused codec checks and native lint blockers. The earlier
third-increment 59-case / 1,123-test evidence and `5dba…` host hash are
historical, not qualification of the rebuilt host.

## Compatibility and installation from source

This is source-only functionality, not an advertised published/prebuilt release.
It requires the companion native host's ToolProgram action **30**, protocol v1,
with all **six exact integer version-1 capabilities**:
`resultEnvelope`, `hostAccounting`, `parentProgress`, `losslessValues`, and
`resultReferences`, and `contentAttachments`. Authentication uses injected per-invocation
`runtimeHandle/runtimeKey`; these credentials are never guest/model APIs.
An older/missing/mismatched host fails before guest execution or list output.
There is **no direct-call, subprocess or legacy-parser fallback**.

The published `rpi-plugin-sdk = 0.3.20` pin and native ABI/layout are unchanged;
the extension uses a raw action-30 shim. Registration sets
`x-rpi-execution-mode: sequential` and `x-rpi-result-envelope: 1`. The outer
program cannot overlap a model tool batch peer; independent nested Parallel
tools can overlap. The host rejects starts without Sequential admission.

Build with Rust/Cargo and a native C compiler (MSVC build tools on Windows):

```sh
cargo build --locked -p rpi-codemode --release
# Load the locally built artifact with the matching new host:
rpi --extension ./target/release/rpi_codemode.dll
# Linux: ./target/release/librpi_codemode.so
# macOS: ./target/release/librpi_codemode.dylib
```

Cargo build-dir settings may change artifact paths; use your actual build output.
If using `--tools`, include codemode and its allowed tools, e.g.
`--tools read,write,codemode`. These instructions/tests do not install, publish,
deploy, merge or restart production services/binaries.

## Public API

```json
{"action":"list","offset":0,"limit":32}
```

Returns `{protocolVersion:1,capabilities:{resultEnvelope:1,hostAccounting:1,
parentProgress:1,losslessValues:1,resultReferences:1,contentAttachments:1},tools:[{name,description,
parameters}],nextOffset:number|null}` in the result summary's structured details.
Follow nextOffset for another page. Only the effective current invocation catalog
is visible, not the root registry. Self/ancestor tools are excluded; changing
that protected policy requires approval. Catalogs follow native batch boundaries
and freeze for one execution. Added tool names do not authorize or refresh tools.

```json
{"code":"const r = await tools.read({path:'Cargo.toml'}); print(r.content.length); return r.details;","timeoutMs":30000}
```

| Parameter | Contract |
| --- | --- |
| action | execute (default) or list |
| code | Required for execute, nonblank, ≤65536 UTF-8 bytes; strict-mode JavaScript async body with await/return, not TypeScript/module/full request |
| timeoutMs | Strict integer 1..120000, default 30000; includes catalog discovery |
| offset | List offset 0..1024, default 0 |
| limit | List page size 1..128, default 32 |

Unknown top-level fields are tolerated for native metadata injection, not guest
configuration/authority. Reserved fields are not business schema properties.

### Guest globals

- `await tools.<name>(args)` / `await tools.call(name,args)` require JSON object
  arguments. Use tools.call for punctuation or helper collisions
  **call/list/emit/result/release**. Ordinary native isError rejects with an Error
  carrying the finalized result/receipt at `error.result`; lifecycle failures
  abort even if caught.
- Results retain native content (including images), details, terminate,
  addedToolNames and usage where supplied. Small results are mutable; large
  receipts are read-only, as described below.
- `tools.list()` returns a fresh JSON copy of the bounded frozen catalog.
- `tools.emit(resultOrContentArray, indices?)` selects ordered inline or genuine
  large-receipt text/image content while returning another summary. Large receipt
  emission needs no JS payload read or native-payload FFI transport; small ordinary
  values remain mutable inline. It is not a tool execution.
- `tools.result(nativeResultOrErrorResult)` selects exactly one native child
  result with authoritative isError; returns undefined, not another tool result.
- `tools.release(nativeResult)` releases a large receipt; it is a native no-op
  for an authenticated small materialized result. It is not a host tool call.
- `print`, `console.log/info/warn/error` capture bounded logs. Non-string values
  serialize as JSON and can throw. Do not log whole large receipts.
- Helpers/globals are frozen/nonreplaceable and capture bridge dependencies.
  No direct filesystem, network, process, require, timers, module loader or
  ambient host context is installed. Allowed native tools retain their authority.

Independent starts occur before completion polling, so concurrency is genuine:

```js
const [a, b] = await Promise.all([
  tools.read({path: 'Cargo.toml'}),
  tools.read({path: 'packages/rpi-codemode/Cargo.toml'})
]);
return {a: a.content, b: b.content};
```

At most **8 active starts**, with no excess-call queue; batch larger sets yourself.
Do not parallelize conflicting mutations. Native arguments/schema validation,
before/after hooks, approvals, cwd and effective tools remain authoritative.

## Results and exact native selection

Ordinary inline Done envelopes have this shape:

```json
{"$rpiToolResult":1,"isError":false,"result":{"content":[{"type":"text","text":"JSON summary"}],"details":{"status":"completed","result":42,"error":null,"logs":[],"calls":[],"durationMs":1,"terminate":false,"isError":false},"terminate":false}}
```

Script/resource/timeout/cancellation failures use bounded Done error envelopes;
negotiation/binding/request/worker infrastructure failures may use ABI Err.
Summary statuses are completed/error/cancelled/timed_out/terminated. Call records
contain name/status/isError/duration, not host IDs/arguments/unbounded payloads.
The **1 MiB final envelope** bound includes framing; oversized inline output
compacts to Done error, never truncates into success or loses termination.

### Small mutable results versus large read-only receipts

| Native serialized result | Guest and tools.result behavior |
| --- | --- |
| Nonterminal **≤1 MiB** | Materialized mutable guest object, native reference immediately released. Selection snapshots **current guest-mutated** supported content/details, not an immutable original. Deleted/undefined details normalize to native null. |
| **>1 MiB** | Frozen lazy receipt with private WeakMap provenance. First field inspection reads bounded chunks, parses the **whole receipt** and deep-freezes it in the same 32 MiB guest heap. Selection forwards the **original native result without materialization**, even after successful inspection. |
| Terminal child, either size | Never resumes JS; directly attaches original native receipt, not an extension summary, and cancels/disposes other calls. |

```js
// Exact native output without a Code Mode summary:
return tools.result(await tools.read({path: 'tiny.png'}));
```

```js
// large_tool must exist in this invocation's catalog.
// Forward a large receipt without accessing content/details in JS:
return tools.result(await tools.call('large_tool', {}));
```

```js
// Exact native error, without synthetic error text:
try { await tools.call('some_tool', {}); }
catch (error) { return tools.result(error.result); }
```

A captured WeakMap authenticates native identity and isError, including caught
error.result. Clones/fabrications cannot forge authority; setting isError on a
small object cannot alter the native flag. Exact selection injects neither a
Code Mode summary into selected details nor synthetic error-text blocks.

Exactly one selection is allowed. Repeated/fabricated/conflicting selections,
invalid serialization, mixing tools.emit (even empty), or returning another
content object fails; selection misuse is sticky even if caught. Later throw,
cancel, timeout, OOM, lifecycle failure or terminal child overrides/discards
prior selection. Usage/names/termination remain host-owned, not guest-forgeable.

### Inspection and release

```js
// Inspect and release each result so a long loop does not retain them all.
// Field access materializes the whole receipt, not only the requested field.
for (let i = 0; i < 20; i++) {
  const r = await tools.call('large_tool', {});
  try { print({blocks: r.content.length}); }
  finally { tools.release(r); }
}
return {released: 20};
```

Release invalidates large receipts, including cached reading/forwarding.
Releasing a selected or emitted/pinned receipt fails before native free. A genuine small result needs no
native release and the helper is a native no-op. Use `tools.call('release',args)`
for a native tool of that name. Release does not undo effects/accounting. Native
storage admission does not guarantee inspection fits the guest heap.

### Ordinary native content promotion

```js
return await tools.read({path: 'tiny.png'});
// Or, in a separate program:
const image = await tools.read({path: 'tiny.png'});
tools.emit(image); // also accepts image.content
return {processed: true};
```

A returned non-array object with its own content selects inline blocks. Returning
an array remains ordinary JSON; use tools.emit(array). Nested arbitrary JSON is
not searched for images, and intermediate images are not auto-forwarded.

Supported blocks are `{type:'text',text,textSignature?:string|null}` and
`{type:'image',mimeType,data}`. MIME is image/png, image/jpeg, image/gif or
image/webp; data must be nonempty canonical padded standard base64 (no whitespace,
URL alphabet or data URI). Encoding validation is not image decoding. Unsupported
block fields/types fail explicitly rather than silently disappear; business
details are not a block whitelist. Serialization/budget failures are sticky.

Inline generated/emitted/small exact content remains **128 blocks / 1 MiB**, with
a **separate 1 MiB final-envelope** bound. tools.emit([]) selects empty output.
Ordinary promotion retains the summary in details, replaces summary text with
blocks, and appends bounded error text on failure (unlike exact selection).
Promoted summary content becomes `{promoted:true,count:N}`, avoiding duplicate
image base64; explicit raw copies elsewhere remain ordinary caller JSON.

Native attachment bypasses **inline payload bounds, not retention quotas**.
Multiple large receipts and inline content can now be emitted in order.
Multiple **whole-result** selections and mixing tools.result with tools.emit
remain unsupported. Use tools.result for one authoritative native content/details/
error outcome, or tools.emit for ordered content composition; not full parity.

## Ordered native content attachments (third increment)

```js
// Tool names must exist in the effective invocation catalog.
const first = await tools.call('large_tool', {});
tools.emit([{type:'text',text:'before'}]);
tools.emit(first);             // all native blocks, without inspecting in JS
const second = await tools.call('another_large_tool', {});
tools.emit([{type:'text',text:'between'}]);
tools.emit(second, [2, 1, 0, 1]); // range must be valid; repeat is intentional
return {processed: 2};
```


`tools.emit(value, indices?)` accepts a genuine large WeakMap receipt without
reading content/details into JS or transporting its native payload through FFI.
Captured native operations measure the selected content, then retain ordered
metadata-only parts. Small ordinary values remain mutable and emit inline.
Multiple large receipts and generated inline blocks can interleave; subset,
reorder, repeated indices and empty selection are valid. Repeated emission of
one receipt still represents **one executed direct child**, not extra accounting.
There is no public/private ID-based authority forgery.

```json
{"$rpiToolResult":1,"isError":false,"result":{"details":{"status":"completed"},"terminate":false},"contentParts":[{"content":[{"type":"text","text":"before"}]},{"reference":{"version":1,"id":"host-owned-id"},"indices":[2,1,0,1]}]}
```

Here `result.details` is the bounded **engineSummary**, not child-native details;
native content/base64 and private attachment parts are not duplicated into it.
With `contentParts`, `result.content` is forbidden and whole `resultReference`
is mutually exclusive. Inline parts allow only `content`; reference parts allow
only `reference` and optional `indices`, with exact reference keys `version:1`
and nonempty `id`. Omitted/null indices select all; `[]` selects no blocks.
The parser produces typed inline/reference parts and resolves them in order
through the captured **live ActionBridge before destroy in both drivers**.
Cancelled, expired, foreign, released or otherwise unverifiable bindings/IDs
fail closed. Native owner/range/cancellation checks run again at final resolution.

Unlike `tools.result`, content emission does **not** select native error/details
authority. Emitting a caught native error's content can complete with outer
`isError:false`; native error accounting is still retained once. `tools.result`
continues to select exactly **one** authoritative native content/details/error
outcome, cannot mix with emit, and is discarded on later failure.

### Indices and bounds

Both large-reference and inline guest paths validate raw indices **before
serialization**: undefined/null means all; otherwise an array of at most **128
nonnegative integer primitives** is required. Length and each index are read
once into a new frozen null-prototype snapshot. Captured intrinsics, property
definition and JSON primitive-number encoding avoid Array prototype setters.
Custom toJSON, NaN/Infinity, scalar or boxed objects cannot widen selection to
all. Native selection also validates range. Invalid directives latch failure,
even when JS catches the exception; no subsequent tool start is permitted.

Native attachment limits are **256 parts / 256 blocks** and **32 MiB final
serialized content array**, checking each block's serialized cost **before each
clone**, including duplicates. Measurement excludes array punctuation; final
budgeting includes brackets/commas. The extension is stricter: **128 selected
parts / blocks** and a combined **32 MiB array including 4096 reserved bytes**
for a final diagnostic block (reason bounded to **512 UTF-8 bytes**, including
worst-case JSON escaping in the reserve). Inline-generated output remains
**128 blocks / 1 MiB** and the final metadata envelope separately stays **1 MiB**.
Native payload is not an inline frame. The unchanged 16-reference / shared
64 MiB retention quota and charge above still apply; these are **not RSS caps**.

### Failure, pins and handoff

Explicit emitted receipts are **pinned**: release attempts latch failure before
native free. Eligible explicit parts survive JS throw, OOM or deadline with
outer error and a compact **last** diagnostic, only while their owning native
binding remains live. Cancellation/expiry/unverifiable ownership cannot attach.
A terminal child overrides **all** emits and whole selection, releases unrelated
receipts, and attaches its original native outcome without resuming JS.

An ordinary host budget/action error is not necessarily owner expiry. After
cleanup, an uncertain binding can regain **attachment eligibility only** by
exact read-only `measure_result_content` requests with `indices:[]` for **every
distinct already-emitted receipt** (at most 16). Every reply must have exactly
matching resultId, contentBlocks:0, encodedBytes:0 and bounded frame shape/size.
All probes must pass; sticky failure and the original root error remain.
This does **not** retry the failed operation, start tools, read payloads, replay
effects or grant new authority. Cancellation, whole-result selection, terminal
handoff and known retained-reference cleanup failure never use this recovery.
Native final decoding checks live ownership again.

## Host transport, ownership and bounds

The sixth native capability, contentAttachments:1, is additive, but required
by this extension (all six must be exact integer v1). It
always starts with `resultMode:'reference'`; native clients omitting that option
retain their original inline path. Reference poll manifests contain
version/id/byteLength/contentBlocks/isError/terminate. read_result uses strict
UTF-8 **byte offsets**, integer limits **4..65536**, stable EOF and ≤1 MiB response
frames. release_result is owner-only; request dispose is independent of references.
See the [host contract](../../docs/codemode-host-contract.md) for wire shapes.

The opted-in final attachment is metadata only:
`{$rpiToolResult:1,isError,resultReference:{version:1,id}}`, mutually exclusive
with inline result. Both native drivers resolve through their captured **live
owning invocation before plugin destroy**, preserving native text/images/details/
isError/terminate without payload transfer through JS/FFI. Stale/foreign/released/
cancelled/mismatched refs fail closed. Legacy non-opted native Done/Err is unchanged.

Attachment removes usage/added names, then the host merges finalized direct-child
usage/names/termination **once after descendant drain**, including ignored/disposed
or thrown outcomes. The extension never sums/re-emits child accounting. After
hooks retain authority; names neither authorize tools nor refresh the catalog.
Terminal child output forwards original native content without resuming JS.
Small structured terminal error is covered; real large **legacy terminal error**
remains uncovered because StepErr cannot carry terminate.

| Resource | Bound |
| --- | --- |
| Guest QuickJS charged allocations | 32 MiB, including conservative allocation overhead |
| Guest stack | 256 KiB |
| Code | 64 KiB UTF-8 |
| Arguments / host response frame / generated returned JSON / final envelope | 1 MiB each; native attachment payload is not inline |
| Native retained references | 16/program, shared 64 MiB invocation-tree storage quota |
| Advertised receipt JSON | At most 32 MiB engine guard; conservative native quota implies below 32 MiB |
| Catalog | 1024 entries / 1 MiB; pages 1..128 (native allows 4096) |
| Logs | 256 entries / 64 KiB text total |
| Starts | 128/tree total / 8 active; 16 retained requests/program |
| Error text | 4096 UTF-8 bytes in summary; appended content diagnostic reason bounded to 512 UTF-8 bytes |
| Selected native content | Engine 128 parts/blocks, 32 MiB array including 4096-byte diagnostic reserve; native 256 parts/blocks, 32 MiB array |

Native retention is charged as
`2*serialized_bytes + 4096 + 128*details_nodes + 256*content_blocks`.
There is **no hard process RSS ceiling**: native clone/serialization may precede
quota checks; Rust JSON and bridge buffers are outside the 32 MiB guest heap.
The guest allocator counts live rounded allocations/overhead, not fragmentation
or total RSS. Allocation failure latches outside JS; catching OOM cannot restore
success or host effects. Only heap.rs permits unsafe Rust, delegating to the
pinned RustAllocator without manual pointer/header arithmetic.

**RegExp is unsupported**, including literals, constructor, dynamically compiled
regex and implicit String.match/search/matchAll regex creation: both compiler
and runtime intrinsics are omitted because the pinned backtracking engine does
not poll interrupts. Plain string replace/split remain available.

Deadlines are cooperative, not OS hard wall clocks. QuickJS interrupts ordinary
loops; a bounded Promise-job pump checks cancellation/deadlines and surfaces job
exceptions. Other native builtins may finish before interrupting. Getters/toJSON
remain subject to budgets. Pending promises with no runnable jobs or host calls
fail. Caught bridge/lifecycle/allocation failures cannot become success; ordinary
finalized isError is catchable. Await outcomes that matter; unawaited work is
cancelled and may never finalize.

Each invocation owns fresh JS state and a worker. ABI execute starts it, poll is
nonblocking, cancel signals, destroy cancels/joins. On every exit pending calls
are cancelled/disposed; completed requests dispose before guest delivery,
independently of retained receipts. Nonselected/unemitted refs release; only a valid final
selection, eligible emitted parts or terminal receipt hands off. Native finish cancels/drains/clears refs.
Permits/gates outlive settling native workers; noncooperative code may delay
cleanup rather than permit unsafe overlap or DLL unload. A failed/uncertain host
operation is **never automatically retried/replayed**; cleanup depends on a
responsive host and does not roll back effects.

## Security and remaining scope

This is a bounded language environment, **not an OS security sandbox**. Allowed
shell/file/network tools retain their permissions/effects. QuickJS/native-plugin
bugs are outside isolation guarantees. Review partial effects before an explicit
retry; cancellation/release/disposal is not rollback.

Business JSON preserves legitimate runtimeKey/runtimeHandle/__rpi/__rpiRuntimeKey
names. ABI top-level injected __rpi stays host-owned. Both drivers' public-output
guard sanitizes actual injected metadata/credential values across callbacks,
Pending, legacy Done, opted-in successes/structured errors and plain errors,
including aliases/serialized JSON/collisions. It does not blanket-strip ordinary
names or bare numbers, nor defend against malicious plugin encoding/direct I/O.

Bounded parent lifecycle/update counts and intent/finish audit markers are
implemented, not original full native child events/history. Never-replay recovery
and native transcript ownership remain; no fabricated assistant calls or parallel
transcript store. No extra model invocation, direct MCP integration, automatic
retry/replay, catalog refresh, cross-invocation cache or TypeScript compilation.

Remaining full-plan work: native full child events; bounded native history/query/
replay/recovery with snapshot/live continuity; neutral extcore and thin HTTP/ACP
mappings; Web child cards/reload/reconnect/HMR in the approved
`D:/Projects/rpi-package-dev` worktree on `dev/web-hmr`;
approved protected recursion/self/ancestor policy;
Linux/macOS real native execution, live providers and reload races. Current dev
frontend, production assets/services and installed binaries remain untouched.

## Development checks and qualification

```sh
cargo fmt -p rpi-codemode -- --check
cargo test --locked -p rpi-codemode
cargo clippy --locked -p rpi-codemode --all-targets -- -D warnings
```

The engine is pinned to rquickjs 0.8.1. A fallible job exception wrapper lacks
native context duplication; the package transfers one captured Ctx reference on
that path to avoid destruction crashes. Recheck this compensation and allocator
accounting in heap.rs on upgrades. Tests cover Promise-species exceptions,
concurrency, cancellation/OOM/regex/budgets, catalog/authority, output selection,
reference chunks/lifetime/immutability/quota and both normal/error cleanup.

### Historical third-increment qualification (before codec follow-up)


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

### Historical second-increment checkpoint (not current qualification)

**92 package tests**, strict all-target Clippy and fmt passed.
Focused native checks: **33 agent**, **206 extension unit + 5 integration tests
(1 ignored)** passed. One new native test covers **22 scenarios across both
drivers**, not 22 separate tests. The complete default-parallel agent/extensions/
harness/SDK suite passed **1,117 tests, 0 failed, 6 ignored**, twice; logs:
`pi-rust-codemode/target/bridge-owner-all-parallel{,-repeat}.log`.
A parallel attempt exposed unrelated-session bridge pinning; filtering weak
owner identities before upgrade fixes it. A deterministic red/green regression
covers the fix without weakening the original immediate-drop assertion.

Fresh Windows evidence:
`pi-rust-codemode/target/codemode-reference-native-smoke-3/{results.json,provenance.json}`
reports **PASSED, all 46 cases true**. All four hashes match actual host/DLL/fixture/
gate artifacts; see the [matrix](../../docs/codemode-full-parity-matrix.md).
Eleven new cases cover four direct baselines/four composed counterparts (2 MiB
text, actual >1 MiB PNG, legacy plain error, terminal success), frozen lazy
inspection/mutation rejection, 20 inspect/release loops with once-only accounting,
and select-then-JS-throw/discard. Large cases use isolated sessions, actual direct
canonical native end payload hashes (images may be normalized), actual image
delivery to the local fixture provider, streamed native transcript markers and
bounded final snapshots. **Not full get_messages large-history parity**, live-
provider, Linux/macOS, installed-binary, reload-race or CI-green qualification.

The original fixture parity_probe is I/O-free; new legacy parity_large reads
only two fixed scratch-workspace JSON files, never caller-provided paths. Harness
bounds are provider requests 16 MiB / 128 requests, retained provider summaries
8 MiB, RPC/transcript lines 16 MiB, process logs 64 MiB: **not RSS ceilings**.
`codemode-reference-native-smoke-1` failed an obsolete terminal-summary assertion;
terminal children now directly attach native receipts. Never cite it as passing.
Historical **35-case codemode-full-parity-smoke-5** and **1109 tests / 6 ignored**
belong to the **first increment**, not current broad qualification. See the
[parity contract](../../docs/codemode-parity-contract.md) for remaining acceptance.
