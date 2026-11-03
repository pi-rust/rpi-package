# rpi-codemode

Native `codemode` tool: execute one bounded JavaScript **async function body**
against the tools already active in the calling invocation. No extra model turn,
MCP server, subprocess interpreter, or TypeScript compiler is involved. Ordinary
tools remain visible and usable normally; this package does not hide or replace
them.

## Compatibility and installation **from source**

This is source-only functionality, not an advertised published release or
prebuilt asset. It requires a **new native host implementing ToolProgram runtime
action 30, protocol version 1**, advertising all three parity capabilities at
version 1: `resultEnvelope`, `hostAccounting`, and `parentProgress`, including authenticated per-invocation
`runtimeHandle` / `runtimeKey` capabilities. Installing the extension alone on an
older host is insufficient: it returns an explicit upgrade error and has **no
direct-call or subprocess fallback**. The package deliberately keeps the
workspace's published `rpi-plugin-sdk = 0.3.20` pin and uses a raw action-30 shim.
The registration includes `x-rpi-execution-mode: sequential`, interpreted by the
companion native adapter, and `x-rpi-result-envelope: 1` to opt into structured
Done results. Capability negotiation rejects older action-30 hosts before guest
execution or list output; the extension never feeds an envelope into a legacy
lenient result parser. The outer program cannot overlap another model tool
batch peer, while independent Parallel tools inside it may still overlap.
The host rejects starts from a program not admitted with Sequential semantics.

Build in this repository with Rust/Cargo and a native C compiler (QuickJS builds
C sources; on Windows use the MSVC build tools):

```sh
cargo build --locked -p rpi-codemode --release
```

Load the resulting **locally built** native extension with a compatible host:

```sh
# Windows (use your actual checkout/build output path)
rpi --extension ./target/release/rpi_codemode.dll
# Linux
rpi --extension ./target/release/librpi_codemode.so
# macOS
rpi --extension ./target/release/librpi_codemode.dylib
```

A user/global Cargo `build.build-dir` setting may change the artifact location;
use the path reported by your build. No install, release publication, service
restart, or deployment is performed by these instructions or package tests.
If launching with an explicit `--tools` allowlist, include `codemode` as well as
the tools it should use, for example `--tools read,write,codemode`.

## Public API

```json
{"action":"list","offset":0,"limit":32}
```

Returns `{protocolVersion:1,
capabilities:{resultEnvelope:1,hostAccounting:1,parentProgress:1},
tools:[{name,description,parameters}], nextOffset:number|null}` as the result
summary's structured `details`. Follow `nextOffset` for another page. Only the current
invocation's effective catalog is available, not the root session registry;
the host excludes the running program tool and ancestor tool names.

```json
{
  "code": "const a = await tools.read({path:'Cargo.toml'}); print(a.content); return a.details;",
  "timeoutMs": 30000
}
```

- `action`: `execute` (default) or `list`.
- `code`: required for execute, nonblank string, at most **65,536 UTF-8 bytes**.
  It is a JavaScript async body accepting `await` and `return`, not TypeScript,
  a module, or a complete JSON request. The body runs in strict mode.
- `timeoutMs`: integer **1..120000**, default **30000**; invalid values are
  rejected, not clamped. Execution deadline includes catalog discovery.
- `offset`: list offset **0..1024**, default **0**.
- `limit`: list page size **1..128**, default **32**.
- Unknown top-level fields are tolerated for host metadata injection; they do
  not configure the guest or widen authority. Reserved fields are not exposed
  as schema properties. The schema conditionally requires code for execute,
  including the default action.

### Guest globals

- `await tools.<name>(arguments)` and `await tools.call(name, arguments)`:
  arguments must be a JSON object. Use `tools.call` for names containing
  punctuation or colliding with `call` / `list` / `emit`. Ordinary tool errors
  (`isError`) reject with an Error whose `.result` is the filtered finalized raw
  result object, so callers can inspect it in `catch`. Uncaught errors abort the
  program; lifecycle failures abort even if their exception is caught.
- Calls resolve to the **finalized result object**, retaining `content`
  (including image blocks), `details`, `terminate`, `addedToolNames`, and `usage`
  when supplied by the host, rather than just text. Reserved transport metadata
  is removed recursively, including object properties named `__rpi`,
  `__rpiRuntimeKey`, `runtimeHandle`, or `runtimeKey`. The same filtering applies
  to catalog schemas and returned JSON values; these names are also forbidden
  recursively in tool arguments. Thus arbitrary property names/schemas are NOT
  preserved losslessly, and tools requiring these names cannot be used here.
  Exact capability-key text is also redacted from host strings. Host capability
  values and request handles are never guest APIs.
- `tools`, its function set, `print`, and `console` are frozen/non-replaceable;
  bridge operations capture their intrinsic dependencies before guest code runs.
- `tools.emit(resultOrContentArray)`: explicitly selects native text/image blocks
  for outer output, in emission order; returns `undefined`. It is not a host call
  and does not alter permissions or accounting. A returned native result object's
  own `content` array is selected after emitted blocks. See output rules below.
- `tools.list()`: a fresh JSON copy of the bounded catalog discovered at the
  beginning of execution. Newly added tools do not refresh this frozen snapshot.
- `print(...)`, `console.log/info/warn/error(...)`: captured structured logs.
  Non-string arguments are JSON serialized. Circular/nonserializable values can
  throw, as with JavaScript JSON serialization.
- Standard JavaScript builtins, but **no direct filesystem, network, process,
  `require`, module loader, timers, or ambient host context** are installed.
  Imports cannot load native modules or files.

Independent calls actually start before completion polling, so this is real
concurrency, not synchronous calls wrapped in promises:

```js
const [a, b] = await Promise.all([
  tools.read({path: 'Cargo.toml'}),
  tools.read({path: 'packages/rpi-codemode/Cargo.toml'})
]);
return {a: a.content, b: b.content};
```

There is no queue for excess active calls: `Promise.all` with more than **8**
active starts fails the whole program. Batch larger sets in groups of at most 8.
Do not parallelize conflicting mutations. A dependent call can use the result
of an earlier `await` normally. Host argument validation, approvals, before/after
hooks, cwd, effective tools, and safety rules remain authoritative.

## Results, limits, and cleanup

The opted-in ABI Done payload is:

```json
{
  "$rpiToolResult": 1,
  "isError": false,
  "result": {
    "content": [{"type":"text","text":"JSON summary, unless native content was selected"}],
    "details": {"status":"completed","result":42,"error":null,"logs":[],"calls":[],"durationMs":1,"terminate":false,"isError":false},
    "terminate": false
  }
}
```

Script failures, resource-budget failures, cancellation and timeout also return
**Done with `isError:true`**, retaining the bounded summary in structured details
and (when no native content was selected) text. Binding, negotiation, invalid
request and worker/lifecycle infrastructure failures may still use ABI Err.
Summary status is `completed`, `error`, `cancelled`, `timed_out`, or `terminated`.
Calls contain tool name, status, optional `isError`, and duration, not host IDs,
arguments or unbounded copies of tool results. The final encoded envelope is
limited to **1 MiB**, including text/structured framing; oversized output becomes
a compact **Done error** retaining termination and bounded logs/calls, never a
success or an ABI Err that loses the termination flag.

### Native content selection

```js
// Promote a native read's finalized content to outer native blocks:
return await tools.read({path: 'tiny.png'});

// Explicitly select intermediate output while returning another JSON summary:
const image = await tools.read({path: 'tiny.png'});
tools.emit(image); // also accepts image.content
return {processed: true};
```

- A returned non-array object with its own `content` property is treated as a
  native-result selection and must contain a valid content array. Arbitrary
  nested JSON is not searched for images. Returning an array alone remains
  ordinary JSON; use `tools.emit(array)` to select its blocks.
- Supported block fields only: `{type:'text',text:string,textSignature?:string|null}`
  and `{type:'image',mimeType:string,data:string}`. Image MIME types are exactly
  `image/png`, `image/jpeg`, `image/gif`, or `image/webp`; data must be nonempty
  canonical padded standard base64 (no whitespace, URL alphabet, or data URI).
  Validation checks encoding, not image decoding/signatures. Reserved transport
  fields are filtered recursively first; all other unsupported block fields
  (including extra metadata) are rejected explicitly rather than silently lost
  by the native host. Supported text signatures are preserved.
  Unsupported/malformed blocks fail explicitly, never disappear silently.
- Selections share **128 blocks / 1 MiB encoded content** across emission and
  return. Emission serialization/validation/budget failures are sticky even if caught. The
  final envelope still has its separate 1 MiB budget. `tools.emit([])` explicitly
  selects empty native output.
- Selected blocks replace the normal summary text block; structured `details`
  always retains the summary. On failure, a short bounded native error text block
  is appended after selected blocks (including an empty selection), subject to
  the final envelope budget. For a returned native result, its summary
  `result.content` becomes `{promoted:true,count:N}`. Image base64 then occurs
  only in outer native content, not again in summary text/details. Other raw
  finalized fields remain summary data. Explicitly copying raw content into
  arbitrary JSON remains the caller's ordinary JSON data, not auto-promotion.
- Intermediate images are **not automatically forwarded**. JS retains access to
  raw per-call content/details/usage/names until it chooses output.

A finalized child with `terminate:true` stops execution without resuming JS,
cancels/disposes remaining calls, and propagates termination. If it also has
`isError`, both flags survive simultaneously; invalid/oversized terminal content
still produces a Done error with `terminate:true`.

The **host** retains and rolls up finalized direct-child usage, added tool names
and termination exactly once after descendant drain, even when JS throws,
ignores results or disposes requests. The extension neither invents usage nor
sums/re-emits child usage/names as its own native result metadata. Returned
usage/name fields are only raw summary data; they do not refresh the catalog or
widen authority. The host also emits bounded parent-associated lifecycle progress
through the normal parent update callback; there is no extension progress
thread/queue, synthetic child transcript, raw-argument forwarding or ABI change.

Each invocation owns a fresh QuickJS runtime/context and a worker; no guest
state is retained. Limits:

| Resource | Limit |
| --- | --- |
| Guest QuickJS charged allocations (including conservative block overhead) | 32 MiB |
| Guest QuickJS stack | 256 KiB |
| Code | 64 KiB UTF-8 |
| Arguments / host response frame / returned JSON value / encoded final tool output | 1 MiB each; reject, never silently truncate |
| Catalog | 1024 entries / 1 MiB total; bounded pages (a larger host catalog fails explicitly even if the host supports 4096) |
| Logs | 256 entries / 64 KiB text total; overflow fails |
| Host starts | 128 total / 8 active (disposed promptly) |
| Error text | 4096 UTF-8 bytes |

The custom allocator counts live allocations, rounded payloads and conservative
per-block overhead, not total process RSS or allocator fragmentation. It latches
allocation failure outside guest JS. Even a caught OOM makes the invocation fail
and prevents later host starts; frees/recovery allocations remain possible for
error handling and destruction. Only `heap.rs` is permitted unsafe Rust: it
validates sizes and delegates allocation/layout/free to the pinned binding's
`RustAllocator`, without doing pointer/header arithmetic. Rust bridge buffers,
JSON encoding, and final summary framing have separately bounded sizes.

**RegExp is unsupported.** Both its native compiler and runtime intrinsic are
omitted, rather than merely deleting the global constructor. This rejects regex
literals, `RegExp`, dynamic compilation of regex literals and implicit regex
construction through `String.match/search/matchAll`. Plain string `replace` and
`split` remain available. The pinned native backtracking engine does not poll
interrupts and could otherwise block the worker long after cancellation.

Execution deadlines are cooperative, not an OS-enforced hard wall clock. Other
native builtins may finish work before yielding to an interrupt; this environment
is not intended for adversarial CPU workloads. QuickJS interrupts ordinary
synchronous loops; a bounded manual Promise-job pump checks deadlines
and cancellation between microtasks, surfacing job exceptions rather than
ignoring them. Serialization can run guest getters/`toJSON`; it remains subject
to interrupts and budgets. A pending promise with no runnable jobs or
host requests fails rather than hanging. Allocation and bridge budget failures
cannot be converted into success by catching their JavaScript exception,
including during final serialization. Native stack-overflow exceptions may be
caught, but the stack cap remains enforced. Host lifecycle `failed` responses also abort execution rather
than becoming catchable ordinary tool errors. Ordinary finalized `isError`
results remain catchable; unawaited calls are cancelled on early return and need
not be polled/finalized. Always await work whose outcome matters.

ABI `execute` starts the worker; `poll` never drives JS or waits for host calls;
`cancel` only signals; `destroy` cancels and joins the worker before returning.
All known outstanding host requests are cancelled and disposed on every exit,
including thrown JS, result serialization failure, OOM, timeout, cancellation,
and success with unawaited calls. Completed requests are disposed before JS gets
their results. A failed/uncertain host operation is **never automatically retried**.
If a broken host starts work without returning an ID, only that host can reclaim
it; the extension cannot invent an ID or safely replay the start. Cleanup calls
are best effort and depend on a responsive supporting host. Host resource guards
retain ownership of any still-settling native tool workers.

## Security and limitations

This is a bounded language environment, **not an OS security sandbox**. An allowed
shell/file/network tool retains its normal permissions and effects. The native
QuickJS engine runs in-process; engine bugs are outside this isolation guarantee.
Tools may already have modified files or external systems when a program fails.
Cancellation/disposal do **not** imply side-effect rollback. Review partial call
summaries before any explicit retry. Do not use untrusted programs with powerful
tools unless their authority is appropriate.

There is no direct MCP integration, extra model invocation, cross-invocation
cache, TypeScript compilation, forwarding of raw internal progress, catalog refresh,
or automatic retry/replay. No ordinary tool visibility is changed. Real host
integration and installed-binary qualification are separate from fake-host tests.

## Development checks

The engine is pinned to `rquickjs = 0.8.1`. Its fallible runtime job API wraps a
borrowed QuickJS context in an owning `JobException` without duplicating the
native reference. The package compensates on that error path with one captured
`Ctx` reference transferred to the wrapper; otherwise a job exception can crash
context destruction. Recheck/remove this version-specific compensation when
upgrading rquickjs. The allocator's rounded-size/header accounting also depends
on this pinned binding; recheck `heap.rs` on upgrades. Regression tests exercise
a throwing Promise species resolver and invocation cleanup; no SDK/host-wide
workaround is applied.


```sh
cargo fmt -p rpi-codemode -- --check
cargo test --locked -p rpi-codemode
cargo clippy --locked -p rpi-codemode --all-targets -- -D warnings
```

Tests cover dependency chains, genuine concurrent starts, full result semantics,
partial side effects, termination, fresh state, capability isolation, invalid
arguments, old/missing hosts, pending promises, synchronous and microtask-loop
cancellation/timeouts, memory/stack/code/result/log/call bounds, catalog paging,
cleanup, schema consistency, and nonblocking ABI lifecycle behavior. Parity tests
also cover strict three-capability negotiation (including ABI Err for old hosts),
Done error/termination round-trips, native PNG promotion, explicit emission,
ordinary JSON, malformed blocks/base64/MIME/text signatures, recursive reserved
field filtering, unsupported metadata rejection, sticky emit serialization,
selected-output error diagnostics, shared selection
budgets and oversized Done-error compaction without losing termination. Additional
regressions cover unavailable regex paths (including dynamic/implicit creation),
caught OOM before host effects and during serialization, sticky allocator
failure, overflow/layout validation, and realloc ownership/accounting.

Companion source-build qualification on Windows passed **25 real native + DLL
cases**, including PNG delivery to an image-capable loopback provider, explicit
selection and selected-image failure, no implicit forwarding, structured script
errors, trusted native usage/name aggregation, terminal error without another
model turn, parent-linked native progress, cancellation and approval denial.
The host's `scripts/codemode-smoke.py` and optional test-only ABI fixture reproduce
these cases; evidence remains local at
`pi-rust-codemode/target/codemode-parity-native-smoke-5/` with binary SHA-256.
See the [parity contract](../../docs/codemode-parity-contract.md). This does not
qualify live providers, installed binaries, native reload races or real native
integration on Linux/macOS; it is not installation/deployment.
