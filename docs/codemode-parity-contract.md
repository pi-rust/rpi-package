# Code Mode native parity contract (second full-parity increment)

Companion worktrees/PRs: `pi-rust-codemode` (#238) and
`rpi-package-codemode` (#187). The **second increment is implemented, not the
full parity plan**. This adds host-backed result references, bounded inspection,
explicit release and native attachment while preserving native tool-chain
execution, invocation authority, cancellation drain gates and Never-replay recovery.
The SDK ABI/layout and published pin are unchanged. No install, deploy, production
service/binary restart, merge or Web/ACP implementation is included.

## Protocol and implementation map

| Layer | Current implementation |
| --- | --- |
| Native request/capabilities | `crates/rpi-agent/src/tool_program.rs`: action 30 / protocol v1, effective catalog, native execution, optional reference-mode starts and finalized accounting retention. |
| Native reference store | `crates/rpi-agent/src/tool_program_results.rs`: invocation-owned native/serialized result, quota, byte reads, release and authoritative resolution. |
| Native Done parser/drivers | `crates/rpi-extensions/src/tool.rs` and `tool/wake.rs`, plus invocation binding in `actions.rs`: resolve metadata-only attachment before destroy in both drivers. |
| Package bridge | `packages/rpi-codemode/src/engine.rs` and `engine/references.rs`: strict manifests/chunks, materialization threshold, selection/lifetime checks, release and cleanup. |
| Guest/final envelope | `src/guest.js`: private WeakMap identity, frozen lazy receipts and helpers. `src/lib.rs`: metadata-only reference envelope or bounded inline Done result. |

`list` advertises `{resultEnvelope:1,hostAccounting:1,parentProgress:1,
losslessValues:1,resultReferences:1}`. The fifth native capability is additive;
the current extension requires **all five exact integer version-1 capabilities**
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
including cached access; releasing an already selected receipt fails. For an
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
from retention quotas. Whole-large tools.emit/return-content, combining multiple
large payloads, and mixed generated/large selections are intentionally unsupported.
Use tools.result for one complete large receipt; do not treat this as full parity.

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

## Current evidence and its limits

**Fresh Windows source-build smoke:**
`pi-rust-codemode/target/codemode-reference-native-smoke-3/{results.json,provenance.json}`
reports **PASSED, 46 cases, all true**. All four recorded hashes were verified
against the actual host, Code Mode DLL, fixture and gate binaries; values are in
the [matrix](codemode-full-parity-matrix.md). The first reference run (`-1`) failed
an obsolete harness assertion expecting the old terminal summary. The current
terminal path directly attaches the native receipt; `-1` is not passing evidence.

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

Current local check checkpoint: **92 package tests**, strict all-target Clippy
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

Full parity remains **INCOMPLETE**: original native child events; bounded native
history/query/replay/recovery with snapshot/live continuity; neutral extcore APIs
and thin HTTP/ACP mappings; Web full child cards/reload/HMR in the approved third
UI dev worktree; multiple/mixed large payload combination; explicit approved
recursion/self/ancestor policy; Linux/macOS native integration, live providers
and reload races. The current dev frontend remains untouched. Production assets,
services and installed binaries are unchanged. The
[matrix](codemode-full-parity-matrix.md) separates implemented, proven and remaining
scope; source coverage and approval to work on UI are not UI qualification.
