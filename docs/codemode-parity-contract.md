# Code Mode native parity contract (third full-parity increment)

Companion worktrees/PRs: `pi-rust-codemode` (#238) and
`rpi-package-codemode` (#187). The **third increment is implemented, not the
full parity plan**. This adds ordered mixed native/inline content attachments
to the second increment's references, inspection, release and whole-result
attachment, preserving native execution/authority, cancellation drain gates and
Never-replay recovery.
The SDK ABI/layout and published `rpi-plugin-sdk = 0.3.20` pin are unchanged.
No install, deploy, production service/binary restart, merge or Web/ACP
implementation is included.

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

## Protocol and implementation map

| Layer | Current implementation |
| --- | --- |
| Native request/capabilities | `crates/rpi-agent/src/tool_program.rs`: action 30 / protocol v1, effective catalog, native execution, optional reference-mode starts and finalized accounting retention. |
| Native reference store | `crates/rpi-agent/src/tool_program_results.rs`: invocation-owned native/serialized result, quota, byte reads, content measurement, release and authoritative resolution. |
| Native Done parser/drivers | `crates/rpi-extensions/src/tool.rs` and `tool/wake.rs`, plus invocation binding in `actions.rs`: resolve metadata-only attachment before destroy in both drivers. |
| Package bridge | `packages/rpi-codemode/src/engine.rs` and `engine/references.rs`: strict manifests/chunks, materialization threshold, selection/lifetime checks, release and cleanup. |
| Guest/final envelope | `src/guest.js`: private WeakMap identity, frozen lazy receipts and helpers. `src/lib.rs`: whole-reference, ordered mixed-content-parts or bounded inline Done result. |

`list` advertises `{resultEnvelope:1,hostAccounting:1,parentProgress:1,
losslessValues:1,resultReferences:1,contentAttachments:1}`. The sixth native capability, `contentAttachments:1`, is additive;
the current extension requires **all six exact integer version-1 capabilities**
before execute/list output. Older/missing/mismatched hosts fail closed, not into
legacy parsing, direct calls or subprocesses. The extension always starts with
`resultMode:"reference"`; native clients omitting it retain their original
inline result path. Native tools not opting into `x-rpi-result-envelope:1`
retain legacy Done/Err behavior.

Reference poll returns a manifest, not payload:
`{requestId,status:"done",isError,resultReference:{version:1,id,byteLength,
contentBlocks,isError,terminate}}`. `read_result` uses owning-invocation resultId,
strict UTF-8 byte offset and integer limit **4..65536**, with a stable byteLength,
validated cursor/EOF and response frame at most 1 MiB. At EOF data is empty and
nextOffset null; invalid boundaries/past EOF fail. `release_result` frees only an
owned retained result; request `dispose` is independent. See the
[host contract](codemode-host-contract.md) for exact wire shapes.

## Small mutable results versus large read-only receipts

| Result | Guest behavior and exact selection |
| --- | --- |
| Nonterminal serialized result ≤1 MiB | Read into a normal mutable guest object; native reference released before guest delivery. `tools.result(r)` snapshots **current guest-mutated** supported content/details with captured native isError authority. Deleted/undefined details normalize to native null. |
| Serialized result >1 MiB | Frozen lazy receipt with private WeakMap provenance. First field inspection reads bounded chunks, parses the whole JSON and deep-freezes it in the same 32 MiB guest heap. `tools.result(r)` attaches the original native result without materializing it, not guest-generated fields. |
| Terminal child of either size | Do not resume JS. Cancel/dispose other requests and attach the original native receipt, preserving content/details/isError/terminate rather than substituting an extension summary. |

`tools.result` returns undefined and selects exactly once. Native isError cannot
be forged by setting a guest field, cloning or constructing an object. Exact
selection injects neither a Code Mode summary into selected details nor synthetic
error text. Repeated/fabricated/conflicting selection, tools.emit mixing, returning
another content object or invalid serialization fails the program; selection
misuse is sticky even if caught. Later throw, cancellation, timeout, OOM,
lifecycle failure or terminal child takes precedence and discards prior selection.

`tools.release(r)` frees a large receipt and invalidates future reads/forwarding,
including cached access; releasing a selected or emitted/pinned receipt fails before native free. For an
authenticated small materialized result it is a native no-op. The helper is not
a host tool call: a native tool named `release`, like `call/list/emit/result`,
requires `tools.call(name,args)`.

Examples (tool names/arguments must exist in the effective catalog):

```js
// Forward a large native result without reading its content/details in JS.
return tools.result(await tools.call('large_tool', {}));
```

```js
// Inspect only what you need in JS, then release each large receipt.
// Field access materializes the WHOLE receipt; this is not streaming inspection.
for (let i = 0; i < 20; i++) {
  const r = await tools.call('large_tool', {});
  try {
    print({blocks: r.content.length}); // bounded output, not the full payload
  } finally {
    tools.release(r);
  }
}
return {released: 20};
```

Ordinary returned-content promotion and tools.emit retain supported text/image
validation (including optional textSignature and canonical image base64/MIME).
Inline generated/emitted/small exact output stays **128 blocks / 1 MiB content**
plus a separate **1 MiB final envelope**; errors compact rather than truncate or
become success. Native attachment exempts payload from those inline bounds, not
from retention quotas. Multiple large receipts and generated inline content can
now interleave via `tools.emit(receipt, indices?)`. Multiple whole-result
selections and mixing `tools.result` with emit remain unsupported; implicit
returned-content promotion is not a no-read receipt API.

## Storage, lifetime and host authority

- At most **16 retained references per program**, with a shared **64 MiB tree
  quota** charged as `2*serialized_bytes + 4096 + 128*details_nodes +
  256*content_blocks`. The quota implies serialized results below 32 MiB; the
  engine independently rejects manifests above 32 MiB. Admission is not a
  guarantee inspection fits the **32 MiB QuickJS allocation limit**.
- This is **not a hard process RSS bound**: native clone/serialization may occur
  before quota checks, and Rust JSON/bridge buffers lie outside the guest heap.
- The opted-in Done attachment contains only
  `{$rpiToolResult:1,isError,resultReference:{version:1,id}}`, mutually exclusive
  with inline result. Both drivers resolve through their captured live owning
  invocation **before destroy**, checking native error identity. Missing/foreign/
  stale/released/cancelled IDs and mismatched error flags fail closed.
- Attachment removes usage/added names; host merge retains finalized direct-child
  metadata exactly once after descendant drain, even if JS throws or ignores a
  completed result. Multi-level rollup is direct-child only, not duplicate sums.
  Checked accounting/overflow rules and after-hook authority remain unchanged.
- Finish cancels/drains and clears retained refs; release/dispose do not undo
  side effects. Sequential outer admission, permits/gates through native cleanup,
  panic drain and finalized termination latching are preserved. No new automatic
  replay, retry or reconnect is introduced.

Business JSON keeps legitimate `runtimeKey/runtimeHandle/__rpi/__rpiRuntimeKey`
properties; injected top-level native ABI `__rpi` remains host-owned. Both drivers'
public-output guard matches actual injected metadata/credential values, not
business names or bare numbers, across callbacks, Pending, legacy Done,
structured successes/errors and plain errors, including aliases/serialized JSON/
collisions. This is not a malicious-plugin sandbox; arbitrary encoding/direct
plugin I/O are unguarded. Catalogs/hooks match native effective batch boundaries;
added names neither authorize tools nor refresh the frozen guest catalog.

Bounded parent lifecycle/update counts and intent/finish markers remain the
progress/history baseline, not original full child event/history snapshots.
No nested assistant calls or separate transcript store are synthesized.

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

## Historical third-increment evidence (before codec follow-up)


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

**Fresh Windows source-build smoke:**
`pi-rust-codemode/target/codemode-reference-native-smoke-3/{results.json,provenance.json}`
reports **PASSED, 46 cases, all true**. All four recorded hashes were verified
against the actual host, Code Mode DLL, fixture and gate binaries; values are in
the [matrix](codemode-full-parity-matrix.md). The first reference run (`-1`) failed
an obsolete harness assertion expecting the old terminal summary. The terminal path directly attaches the native receipt; `-1` is not passing evidence.

The 11 new cases comprise four direct baselines (2 MiB text, actual >1 MiB PNG,
legacy plain error, terminal success), four corresponding composed forwards,
lazy frozen inspection/mutation rejection, 20 inspect/release iterations with
once-only accounting, and select-then-JS-throw discarding the receipt. They run
in isolated sessions, compare canonical native end payload hashes to actual
direct baselines (images may be normalized), validate image delivery in actual
requests to the local fixture provider, and check streamed transcript markers
plus bounded final snapshots. This is **not full get_messages large-history
parity** or live-provider qualification. Legacy StepErr cannot carry terminate:
a real large legacy terminal error remains uncovered; small structured terminal
error is already covered.

Historical second-increment checkpoint: **92 package tests**, strict all-target Clippy
and fmt passed; native focused checks passed **33 agent tests**, **206 extension
unit tests plus 5 integration tests (1 ignored)**. One new extension test exercises
**22 scenarios across both drivers**, not 22 separate test functions. The complete
default-parallel agent/extensions/harness/SDK suite passed **1,117 tests, 0 failed,
6 ignored**, twice (`target/bridge-owner-all-parallel{,-repeat}.log`). A prior
parallel attempt exposed unrelated-session bridge pinning; weak owner filtering
before upgrade fixes it, with a deterministic red/green regression and no weakened
immediate-drop assertion. Historical **35-case full-parity-smoke-5**
and **1109 tests / 6 ignored** belong to the first increment, not current broad
qualification. These local results are not an installed-binary, CI-green or
Linux/macOS native integration claim.

## Remaining full-plan acceptance

Full parity remains **INCOMPLETE**: complete native child identity/order/updates/
final events; bounded native
history/query/replay/recovery with snapshot/live continuity; neutral extcore APIs
and thin HTTP/ACP mappings; Web full child cards/reload/HMR in the approved third
UI dev worktree (`D:/Projects/rpi-package-dev`, `dev/web-hmr`); explicit approved
recursion/self/ancestor policy; Linux/macOS native integration, live providers
and reload races. Multiple/mixed large content output is now implemented and qualified within
the stated bounds; multiple whole-result selection remains unsupported. The
current dev frontend remains untouched. Production assets,
services and installed binaries are unchanged. The
[matrix](codemode-full-parity-matrix.md) separates implemented, proven and remaining
scope; source coverage and approval to work on UI are not UI qualification.
