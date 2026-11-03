# rpi-codemode

Native `codemode` tool: execute one bounded JavaScript **async function body**
against the tools already active in the calling invocation. No extra model turn,
MCP server, subprocess interpreter, or TypeScript compiler is involved. Ordinary
tools remain visible and usable normally; this package does not hide or replace
them.

## Compatibility and installation **from source**

This is source-only functionality, not an advertised published release or
prebuilt asset. It requires a **new native host implementing ToolProgram runtime
action 30, protocol version 1**, including authenticated per-invocation
`runtimeHandle` / `runtimeKey` capabilities. Installing the extension alone on an
older host is insufficient: it returns an explicit upgrade error and has **no
direct-call or subprocess fallback**. The package deliberately keeps the
workspace's published `rpi-plugin-sdk = 0.3.20` pin and uses a raw action-30 shim.
The registration includes `x-rpi-execution-mode: sequential`, interpreted by the
companion native adapter: the outer program cannot overlap another model tool
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

Returns `{protocolVersion:1, tools:[{name,description,parameters}],
nextOffset:number|null}`. Follow `nextOffset` for another page. Only the current
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
  punctuation or colliding with `call` / `list`. Tool errors (`isError`) and
  lifecycle failures abort execution with useful error text; catch ordinary `isError`
  results explicitly when continuing is appropriate.
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

Successful or terminated execution returns a JSON summary in both text content
and structured `details`. Failed execution uses **ABI `StepResult::err`**, with
the JSON summary as error text (not structured `details`):
`{status,result,error,logs,calls,durationMs,terminate}`. Status is `completed`,
`error`, `cancelled`, `timed_out`, or `terminated`. Calls contain tool name,
status, optional `isError`, and duration, not host IDs, arguments, or unbounded
copies of tool results. The encoded final output also has a 1 MiB limit, including
duplicated text and structured framing: an oversized output is rejected with a
compact error summary retaining logs, calls and termination, not truncated.
Failures preserve bounded logs/call summaries of partial
work. Invalid top-level arguments and missing/unsupported host bindings produce
an ABI tool error. Do not rely on `isError` inside a Done JSON envelope: the native adapter ignores
that field. Failure status is conveyed by the ABI error tag.
A finalized tool result with `terminate:true` stops the program immediately,
propagates termination, and cancels/disposes remaining calls without resuming JS.
Termination takes precedence even if that finalized result also has `isError`;
the call summary records its error flag and the terminal result remains available.
If final output encoding exceeds its budget, the compact ABI error preserves
`terminate` as summary data only; the ABI error channel cannot additionally
propagate the outer host termination flag.

Each invocation owns a fresh QuickJS runtime/context and a worker; no guest
state is retained. Limits:

| Resource | Limit |
| --- | --- |
| Guest QuickJS heap | 32 MiB |
| Guest QuickJS stack | 256 KiB |
| Code | 64 KiB UTF-8 |
| Arguments / host response frame / returned JSON value / encoded final tool output | 1 MiB each; reject, never silently truncate |
| Catalog | 1024 entries / 1 MiB total; bounded pages (a larger host catalog fails explicitly even if the host supports 4096) |
| Logs | 256 entries / 64 KiB text total; overflow fails |
| Host starts | 128 total / 8 active (disposed promptly) |
| Error text | 4096 UTF-8 bytes |

Memory limits cover the guest heap, not total process RSS. Rust bridge buffers,
JSON encoding, and final summary framing have separately bounded sizes. QuickJS
interrupts synchronous loops; a bounded manual Promise-job pump checks deadlines
and cancellation between microtasks, surfacing job exceptions rather than
ignoring them. Serialization can run guest getters/`toJSON`; it remains subject
to interrupts and budgets. A pending promise with no runnable jobs or
host requests fails rather than hanging. Budget failures cannot be converted
into success by catching their JavaScript exception, including during final
serialization. Host lifecycle `failed` responses also abort execution rather
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
cache, TypeScript compilation, streaming internal tool progress, catalog refresh,
or automatic retry/replay. No ordinary tool visibility is changed. Real host
integration and installed-binary qualification are separate from fake-host tests.

## Development checks

The engine is pinned to `rquickjs = 0.8.1`. Its fallible runtime job API wraps a
borrowed QuickJS context in an owning `JobException` without duplicating the
native reference. The package compensates on that error path with one captured
`Ctx` reference transferred to the wrapper; otherwise a job exception can crash
context destruction. Recheck/remove this version-specific compensation when
upgrading rquickjs. Regression tests exercise a throwing Promise species resolver
and invocation cleanup; no SDK/host-wide workaround is applied.


```sh
cargo fmt -p rpi-codemode -- --check
cargo test --locked -p rpi-codemode
cargo clippy --locked -p rpi-codemode --all-targets -- -D warnings
```

Tests cover dependency chains, genuine concurrent starts, full result semantics,
partial side effects, termination, fresh state, capability isolation, invalid
arguments, old/missing hosts, pending promises, synchronous and microtask-loop
cancellation/timeouts, memory/stack/code/result/log/call bounds, catalog paging,
cleanup, schema consistency, and nonblocking ABI lifecycle behavior.
