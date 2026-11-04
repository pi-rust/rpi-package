# Native Code Mode host contract (implementation v1, third increment)

The host (`D:/Projects/pi-rust-codemode`) and extension
(`D:/Projects/rpi-package-codemode`) implement this contract together, without
MCP or another model invocation. The **third full-parity increment is
implemented; full parity remains incomplete**. See the
[parity contract](codemode-parity-contract.md) and
[status matrix](codemode-full-parity-matrix.md) for qualification and remaining work.
The native SDK ABI/layout and published `rpi-plugin-sdk = 0.3.20` pin are unchanged.

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
See the [observation checkpoint](codemode-full-parity-matrix.md#latest-native-observation-seam-not-durable-history)
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
See the [latest matrix checkpoint](codemode-full-parity-matrix.md#historical-native-result-codec-checkpoint)
for hashes, logs, focused codec checks and native lint blockers. The earlier
third-increment 59-case / 1,123-test evidence and `5dba…` host hash are
historical, not qualification of the rebuilt host.

## Native invocation and operations

The existing runtime action is `RuntimeActionId::ToolProgram = 30`, protocol v1.
`list` advertises six exact integer capabilities:
`{resultEnvelope:1,hostAccounting:1,parentProgress:1,losslessValues:1,resultReferences:1,contentAttachments:1}`.
The sixth capability, `contentAttachments:1`, is additive for native clients;
the current Code Mode extension requires **all six** before execute or list output. Missing, older or
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
| `measure_result_content` | `{op:"measure_result_content",resultId,indices?:null\|[nonnegative integers]}` → `{resultId,contentBlocks,encodedBytes}`. Omitted/null selects all, `[]` none; reorder/repeats are valid. Native indices are bounded to 256, validated for range and owning live invocation. `encodedBytes` sums canonical serialized **selected blocks**, excluding array brackets/commas; repeated blocks count repeatedly. Uses `serde_json::to_writer` with a byte counter, not a huge temporary allocation. |
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
than allow unsafe overlap or DLL unload. Errors/throw discard whole-result selections, not eligible explicit emitted parts.
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
  subsequent reads/forwarding, including cached reads. Releasing a selected or emitted/pinned
  receipt is a sticky failure **before native free**. Release of a genuine materialized small result
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
  inline payload bounds, **not** retained-storage quotas. Ordered large/inline
  content emission is supported below; multiple whole-result selection remains
  unsupported, and `tools.result` still cannot mix with `tools.emit`.

Registration opts into `x-rpi-result-envelope:1`. Legacy non-opted native tools
keep their existing Done/Err parsing; malformed opted-in envelopes fail closed.
Ordinary script failures use bounded Done error envelopes; infrastructure
failures may use ABI Err. No automatic intermediate-image forwarding occurs.

## Ordered native content attachments (third increment)

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

## Output hygiene, progress and remaining scope

Business JSON preserves legitimate `runtimeKey`, `runtimeHandle`, `__rpi` and
`__rpiRuntimeKey` names, including nested data. Native ABI top-level injected
`__rpi` remains host-owned. Invocation-local guards sanitize actual injected
metadata/credential values in callbacks, Pending, legacy Done, opted-in success/
structured errors and plain errors in both drivers, including serialized JSON,
aliases and collisions. A numeric runtimeHandle is not a credential: masking
handle `3` previously corrupted the real `32 MiB` overflow diagnostic. That bug
is fixed, preserving business numbers/diagnostics while actual runtime keys and
private request IDs stay guarded. Ordinary names/numbers are not blanket-redacted. This
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
child identity/order/updates/final events, bounded native history/query/replay/
recovery with snapshot/live continuity, neutral extcore semantics with
thin HTTP/ACP mappings, and Web child cards/HMR/reload/reconnect remain outstanding in the approved
`D:/Projects/rpi-package-dev` worktree on `dev/web-hmr` (untouched). No
install, deployment, production restart, merge or UI implementation is included.

Third-increment evidence and historical checkpoints are recorded in the
[status matrix](codemode-full-parity-matrix.md); source review/tests/source commit
remain parent review work, not deployment or full-plan completion.
