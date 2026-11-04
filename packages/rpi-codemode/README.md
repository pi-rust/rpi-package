# rpi-codemode

Native `codemode` executes one bounded JavaScript **async function body** against
the tools already active in the calling invocation. No extra model turn, MCP
server, subprocess interpreter or TypeScript compiler. Ordinary tools remain
visible and usable normally. The **second full-parity increment is implemented;
overall full parity remains INCOMPLETE**. See the
[status matrix](../../docs/codemode-full-parity-matrix.md) for evidence and gaps.

## Compatibility and installation from source

This is source-only functionality, not an advertised published/prebuilt release.
It requires the companion native host's ToolProgram action **30**, protocol v1,
with all **five exact integer version-1 capabilities**:
`resultEnvelope`, `hostAccounting`, `parentProgress`, `losslessValues`, and
`resultReferences`. Authentication uses injected per-invocation
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
parentProgress:1,losslessValues:1,resultReferences:1},tools:[{name,description,
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
- `tools.emit(resultOrContentArray)` explicitly selects inline native text/image
  blocks while returning another summary. It is not a host call.
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
Releasing an already selected receipt fails. A genuine small result needs no
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
Whole-large tools.emit/return-content, mixing generated and large payloads, and
multiple full-receipt selections remain intentionally unsupported. Use
`tools.result(receipt)` for one complete large output. This is not full parity.

## Host transport, ownership and bounds

The fifth native capability is additive, but required by this extension. It
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
| Error text | 4096 UTF-8 bytes |

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
independently of retained receipts. Nonselected refs release; only a valid final
selection/terminal receipt hands off. Native finish cancels/drains/clears refs.
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
mappings; Web child cards/reload/HMR in the approved third UI dev worktree;
multiple/mixed large output; approved protected recursion/self/ancestor policy;
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

Current checkpoint: **92 package tests**, strict all-target Clippy and fmt passed.
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
