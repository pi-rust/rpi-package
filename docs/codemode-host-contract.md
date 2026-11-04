# Native Code Mode host contract (implementation v1, second increment)

The host (`D:/Projects/pi-rust-codemode`) and extension
(`D:/Projects/rpi-package-codemode`) implement this contract together, without
MCP or another model invocation. The **second full-parity increment is
implemented; full parity remains incomplete**. See the
[parity contract](codemode-parity-contract.md) and
[status matrix](codemode-full-parity-matrix.md) for qualification and remaining work.
The native SDK ABI/layout and published SDK pin are unchanged.

## Native invocation and operations

The existing runtime action is `RuntimeActionId::ToolProgram = 30`, protocol v1.
`list` advertises five exact integer capabilities:
`{resultEnvelope:1,hostAccounting:1,parentProgress:1,losslessValues:1,resultReferences:1}`.
The fifth capability is additive for native clients; the current Code Mode
extension requires **all five** before execute or list output. Missing, older or
mismatched capabilities fail closed, without a direct-call, subprocess, legacy
parser or automatic retry fallback. The extension uses a raw action-30 shim.

Every operation authenticates the actual host-owned invocation binding:
injected `__rpi.runtimeHandle` as runtime-call user_data and `__rpi.runtimeKey`
as reserved `__rpiRuntimeKey`. Credentials and native reference IDs are not guest APIs.

| Operation | Request and response |
| --- | --- |
| `list` | `{op:"list",offset?:0,limit?:32}` → `{protocolVersion:1,capabilities,tools:[{name,description,parameters}],nextOffset:number\|null}`. Only the effective active invocation tool set is visible; self/ancestors are excluded. |
| `start` | `{op:"start",name,arguments:object,timeoutMs?:30000,resultMode?:"reference"}` → `{requestId,status:"running"}`. Identity is host-generated. Omitting resultMode preserves the original inline native behavior and its 1 MiB frame bound. Other supplied modes are rejected. The extension always starts in reference mode. |
| `poll` | `{op:"poll",requestId}` → running, done or failed. Original inline done contains `{requestId,status:"done",isError,result:{content,details,terminate,addedToolNames,usage}}`. Reference-mode done contains `{requestId,status:"done",isError,resultReference:{version:1,id,byteLength,contentBlocks,isError,terminate}}`, not an inline result. Failed denotes lifecycle/bridge failure, not a finalized ordinary tool error. |
| `read_result` | `{op:"read_result",resultId,offset?:0,limit?:65536}` → `{resultId,offset,data,byteLength,nextOffset:number\|null}`. Offset is a strict nonnegative integer UTF-8 **byte** boundary at or before EOF; limit is a strict integer **4..65536**. Data ends at a UTF-8 boundary, at most limit bytes. Reads are stable and resumable while retained; EOF reads return empty data and null nextOffset. Past-EOF/nonboundary offsets fail. Each encoded response is at most 1 MiB. |
| `release_result` | `{op:"release_result",resultId}` → `{resultId,status:"released"}`. Only the owning invocation may release; unknown/already released IDs fail. Releases storage, not accounting or native side effects. |
| `cancel` / `dispose` | `{op,requestId}` → cancelled/disposed. Disposal of a request is **independent** of its finalized result reference: it does not release the result. Cleanup remains available while the binding is valid. Neither operation implies rollback. |

Starts reuse native argument preparation/schema validation, before/after hooks,
approvals, cwd, effective tools and safety rules. Catalogs follow native batch
boundaries; the extension freezes discovery for one execution. `addedToolNames`
is metadata, not authorization or a refresh signal. Protected self/ancestor
exclusion remains; changing recursion policy requires approval.

Timeouts are 1..120000 ms. Bounds are 128 starts / 8 active futures across the
invocation tree, 16 retained requests per invocation, arguments and action frames
at most 1 MiB. Native catalogs allow 4096 entries; the extension additionally
bounds discovery to 1024 entries / 1 MiB, with pages of 1..128. Never silently
truncate, retry or replay. Starts fail after parent cancellation/completion,
invalidation, terminal admission latch or session close/switch.

## Reference ownership, storage and native attachment

`crates/rpi-agent/src/tool_program_results.rs` owns finalized native results and
their serialized inspection JSON. There are at most **16 retained references per
program** and a shared **64 MiB invocation-tree storage quota**. Each retained
result is charged conservatively:

```text
2 * serialized_bytes + 4096 + 128 * details_nodes + 256 * content_blocks
```

A details node includes each JSON container/scalar. Permits follow retained
ownership and return on release/finish/drop. The quota implies a serialized
manifest below 32 MiB; the extension also rejects advertised byteLength above
**32 MiB**. This is not a hard RSS ceiling: native payload cloning/serialization
can happen before quota admission, and Rust JSON/bridge buffers are outside the
32 MiB QuickJS heap. Inspection can exhaust that heap even for an admitted receipt.

The opted-in final Done envelope can now carry metadata only:

```json
{"$rpiToolResult":1,"isError":false,"resultReference":{"version":1,"id":"host-owned-id"}}
```

`result` and `resultReference` are mutually exclusive. The native parser resolves
the ID through the driver-captured **live owning invocation** and checks isError
against the retained native outcome. Both polling and wake drivers resolve it
**before plugin destroy**; absent, foreign, stale, released, cancelled or
mismatched selections fail closed. Native attachment preserves native
text/images/details/isError/terminate without copying the payload through JS or
FFI. Attachment strips usage/added names; host accounting subsequently merges
finalized direct-child metadata exactly once after descendant drain. It cannot
forge transcript calls or double-count accounting. After hooks retain native authority.

The program/loop is Sequential (`x-rpi-execution-mode:"sequential"`); independent
nested Parallel tools may overlap. Cancel/dispose publish promptly but retain
permits/gates through cleanup. Panics cancel/drain descendants. Finish cancels,
drains and clears references; noncooperative native code may delay it rather
than allow unsafe overlap or DLL unload. Errors/throw discard selected receipts.
A terminal child stops without resuming JS and attaches its **original native
receipt**, not the extension summary; remaining calls are cancelled/disposed.
Finalized termination is latched even before fallible frame encoding.

## Guest results and selection

`codemode` accepts `action:"list"|"execute"` (default execute), nonblank JavaScript
async-body `code`, and optional timeoutMs 1..120000 (default 30000).

- `tools.<name>(args)` / `tools.call(name,args)` return native results; ordinary
  isError rejects with `error.result`. Use tools.call for helper collisions
  `call/list/emit/result/release`.
- Nonterminal results **at most 1 MiB serialized** are materialized as mutable
  guest objects and their native reference is immediately released.
  `tools.result(smallResult)` selects the current guest-mutated supported
  content/details; deleted/undefined details normalize to native null.
  Authoritative isError comes from captured WeakMap identity, not mutable fields.
- Results **over 1 MiB** are frozen lazy native receipts, with deeply frozen
  content/details on inspection. First field access reads/validates bounded
  chunks and parses the whole receipt in the same 32 MiB guest heap; it is not
  field streaming or unlimited memory. IDs/tokens stay private. WeakMap identity
  authenticates the receipt. `tools.result(receipt)` forwards the host-owned
  outcome **without reading it into JS**, even after successful inspection.
- `tools.release(receipt)` explicitly frees a large receipt and invalidates
  subsequent reads/forwarding, including cached reads. Releasing a selected
  receipt is a sticky failure. Release of a genuine materialized small result
  is a native no-op. It is not a host tool call; call a native tool named release
  through `tools.call('release',args)`.
- `tools.result` selects exactly once and returns undefined. Fabricated/cloned
  inputs, repeated/conflicting selections, mixing tools.emit, or returning a
  second content object fail; selection misuse is sticky. Later throw, cancel,
  timeout, OOM, lifecycle failure or terminal child overrides selection. No
  summary or synthetic error text is injected into an exact native selection.
- Ordinary `tools.emit` / returned-content promotion and generated small exact
  selections remain **128 blocks / 1 MiB encoded content**, with a separate
  **1 MiB final-envelope** bound. Native receipt attachment bypasses these
  inline payload bounds, **not** retained-storage quotas. Whole-large emission,
  mixed large/generated output and multiple full-receipt selection are unsupported.

Registration opts into `x-rpi-result-envelope:1`. Legacy non-opted native tools
keep their existing Done/Err parsing; malformed opted-in envelopes fail closed.
Ordinary script failures use bounded Done error envelopes; infrastructure
failures may use ABI Err. No automatic intermediate-image forwarding occurs.

## Output hygiene, progress and remaining scope

Business JSON preserves legitimate `runtimeKey`, `runtimeHandle`, `__rpi` and
`__rpiRuntimeKey` names, including nested data. Native ABI top-level injected
`__rpi` remains host-owned. Invocation-local guards sanitize actual injected
metadata/credential values in callbacks, Pending, legacy Done, opted-in success/
structured errors and plain errors in both drivers, including serialized JSON,
aliases and collisions. Ordinary names/numbers are not blanket-redacted. This
is output hygiene, not protection against malicious plugin encoding/direct I/O.

Guest logs/calls are bounded. No direct filesystem/network/process/import is
installed. RegExp compiler/runtime intrinsics are omitted. Each execution owns
fresh QuickJS state, allocation/stack/interrupt budgets and a joining worker;
deadlines are cooperative. Caught allocation failure cannot restore success.
ABI poll is nonblocking, cancel signals, destroy joins. No effect is rolled back.

Parent-associated lifecycle/update counts and bounded intent/finish audit markers
are implemented, not full native child events/history. Preserve checkpoint-before-
side-effect and Never-replay interrupted programs; do not feed nested child starts/
MessageEnd into SettlingEmitter as fabricated assistant calls. Complete native
child events, bounded native history/query/replay/recovery, extcore semantics with
thin HTTP/ACP mappings, and Web child cards/HMR/reload remain outstanding. No
install, deployment, production restart, merge or UI implementation is included.
