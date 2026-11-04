# Code Mode full-parity matrix — third increment

Scope: companion host `D:/Projects/pi-rust-codemode` and package
`D:/Projects/rpi-package-codemode`. **Third increment implemented; overall full
parity INCOMPLETE.** No unrestricted full-toolset, source release, installation,
deployment, production restart, merge or CI-green claim. Current dev frontend
is untouched; approval for a third UI dev worktree does not prove UI work.

## Current third-increment evidence checkpoint

| Evidence | Proven scope / limits |
| --- | --- |
| Fresh Windows host + Code Mode DLL + fixture + gate | `pi-rust-codemode/target/codemode-content-parts-native-smoke-2/results.json`: **PASSED, 59 cases, all true**. All four provenance hashes below verified against actual local artifacts. |
| 13 new native smoke cases; previous 46 retained | Direct mixed baseline; large text/image emit; two separate large calls interleaved with inline; subset `[2,1,0,1]` (same image twice, one execution); caught large error content with completed outer result; emit then JS throw; pinned release; 32 MiB measurement overflow preserving first PNG; negative indices; NaN scalar sticky failure with no subsequent write; toJSON/prototype resistance; terminal overrides earlier PNG with two child executions. |
| Payload / history proof boundary | Provider validates actual image bytes for **each copy**, including the two-image case, before normalized direct-baseline comparisons. Exact native end ordering/hashes, engineSummary without base64/private attachment parts, intent/finish lifecycle and bounded final snapshots checked. **Not full native history parity** or live-provider qualification. |
| Package checks | **124 passed, 0 failed, 0 ignored**; strict all-target Clippy and fmt passed. Logs: package `target/codemode-content-parts-{tests,clippy}.log`. |
| Broad native regression | Default-parallel SDK/agent/extensions/harness: **1,123 passed, 0 failed, 6 ignored**, twice. Native `target/codemode-content-parts-tests.log` and `target/codemode-content-parts-tests-repeat.log`. Existing both-driver full-reference 22-scenario test and new content-parts scenario test are **test functions**, not separate extra scenario counts. |
| Failed content-parts attempt | `codemode-content-parts-native-smoke-1` **FAILED after 53 cases, before overflow**: masking numeric handle 3 corrupted the real `32 MiB` diagnostic. Implementation fixed; expected needle not weakened. Never cite `-1` as passing. |

Current evidence files:
`pi-rust-codemode/target/codemode-content-parts-native-smoke-2/{results.json,provenance.json}`.

| Windows artifact recorded in current provenance | Recorded SHA-256 |
| --- | --- |
| Host `pi-rust-codemode/target/debug/rpi.exe` | `5dba0fa76a699b79f9b286721cd8b2ab1e21b79f3f30536090d086d76d2dc7c7` |
| Code Mode `rpi-package-codemode/target/debug/rpi_codemode.dll` | `5fccaaa062bdebb1fe74294a470d18816cbb59ffe8e79ad43196e220e8ebdd6f` |
| Fixture `pi-rust-codemode/target/codemode-parity-fixture/debug/codemode_parity_fixture.dll` | `27830e8fe1ca4a1f94918c1be51912472d34bba6ed3ace716dd933339eb23bac` |
| Gate `rpi-package-codemode/target/debug/rpi_permission_gate.dll` | `b52ce8ce8f10e032b701eb53cbca01e1abe04d567843aa96e2be9d880542cb34` |

These are recorded local checkpoints, not source-review/commit approval, release,
installed-binary, deployment, CI-green, Linux/macOS or full-plan completion claim.

## Historical second-increment evidence (not current qualification)

| Evidence | Proven scope / limits |
| --- | --- |
| Fresh Windows host + Code Mode DLL + fixture + gate | `pi-rust-codemode/target/codemode-reference-native-smoke-3/results.json`: **PASSED, 46 cases, all true**. Four provenance hashes below verified against actual binaries. |
| 11 new real-native smoke cases | Four direct baselines and four composed counterparts: 2 MiB text, actual >1 MiB PNG, legacy plain error, terminal success. Plus frozen lazy inspection/three rejected mutations, 20 inspect/release loops with once-only accounting, and select-then-JS-throw. |
| Large-payload proof boundary | Isolated sessions compare canonical native end payload hashes against direct baselines; image data may be normalized by native handling. Actual local fixture-provider requests validate image delivery, streamed native transcript markers validate lifecycle, and final snapshots are bounded. **Not** full get_messages large-history equality or live-provider qualification. |
| Historical package checks | **92 tests**, strict all-target Clippy and fmt passed. |
| Historical focused native checks | **33 agent tests**, **206 extension unit tests + 5 integration tests (1 ignored)** passed. One new extension test covers **22 scenarios across both drivers**, not 22 test functions. |
| Historical broad native regression | Default-parallel agent/extensions/harness/SDK suite passed **1,117 tests, 0 failed, 6 ignored**, twice: `target/bridge-owner-all-parallel.log` and `target/bridge-owner-all-parallel-repeat.log`. Historical **1109 / 6 ignored** and the 35-case smoke qualify the first increment only. |
| Retirement regression | A broad parallel attempt exposed cross-session pinning in the global bridge scan. Owner identity now filters weak registry entries **before** upgrading. The deterministic regression fails with the original scan; existing immediate-drop assertions remain unchanged. No claim about historical CI failure. |
| Failed reference attempt | `codemode-reference-native-smoke-1` failed an obsolete harness terminal-summary assertion. Terminal completion now directly attaches the original native receipt. Never cite `-1` as passing. |

Historical second-increment evidence files:
`pi-rust-codemode/target/codemode-reference-native-smoke-3/{results.json,provenance.json}`.

| Actual Windows artifact recorded in provenance | Historical verified SHA-256 |
| --- | --- |
| Host `pi-rust-codemode/target/debug/rpi.exe` | `185f0e7c031fb703c5a59d18047359e8a42e981d91c5658ed907205b9eb01383` |
| Code Mode `rpi-package-codemode/target/debug/rpi_codemode.dll` | `85761d9c99e102a8150ece8f043b4946cdad10edeadc5aff3dbcfc7d2b35f4df` |
| Fixture `pi-rust-codemode/target/codemode-parity-fixture/debug/codemode_parity_fixture.dll` | `d849ab1773aa0a463b0fc7f61e393d7c3066eba8946e24e20c6e9c632a29a245` |
| Gate `rpi-package-codemode/target/debug/rpi_permission_gate.dll` | `b52ce8ce8f10e032b701eb53cbca01e1abe04d567843aa96e2be9d880542cb34` |

## Implemented versus proven

| Area | Implemented behavior | Evidence / qualification boundary |
| --- | --- | --- |
| Negotiation / ABI | Protocol v1 adds native optional `resultReferences:1` and sixth `contentAttachments:1`; extension requires all **six** exact integer v1 caps (also resultEnvelope, hostAccounting, parentProgress, losslessValues). Fail closed, no fallback. SDK 0.3.20 pin and ABI/layout unchanged. | Package rejection checks; current real smoke checks all six. Native clients omitting resultMode and non-opted plugins preserve original behavior. |
| Reference transport | Extension starts `resultMode:'reference'`; poll manifest has version/id/byteLength/contentBlocks/isError/terminate. read_result uses strict byte offsets, integer 4..65536 limit, UTF-8 boundaries, stable EOF and ≤1 MiB responses. release_result is owner-only; dispose independent. | Native/package tests cover malformed manifests/chunks, lifetime and cleanup; real large forwards prove host/DLL integration, not all race cases. |
| Native storage | 16 references/program, shared 64 MiB tree quota: `2*serialized_bytes+4096+128*details_nodes+256*content_blocks`. Quota implies serialized size below 32 MiB; package guards advertised size at 32 MiB. | Unit quota/release checks and 20 real release loops. **No hard RSS ceiling**: native clone/serialization precedes quota admission; Rust JSON lies outside 32 MiB guest heap. |
| Small mutable selection | Nonterminal ≤1 MiB results materialize and release native storage; tools.result uses current guest-mutated content/details, deleted/undefined details → null, native isError via WeakMap. | Existing small direct/error/deleted-details cases retained in 59-case run, plus package checks. tools.release on genuine small results is a native no-op. |
| Large read-only selection | >1 MiB frozen lazy genuine WeakMap receipt. Inspection materializes/deep-freezes whole receipt under 32 MiB guest heap. tools.result forwards original native result without materialization. | Real large text/image/error direct-baseline comparisons and inspection/mutation rejection. Admitted receipt need not fit inspection heap. Large content emission supports ordered mixed parts; multiple whole-result selection remains unsupported. |
| Attachment / drivers | Metadata-only opted-in Done envelope resolves through captured live owning invocation before destroy in **both** drivers; preserves native content/details/isError/terminate, strips usage/names before host merge. | 22 new both-driver scenarios in one native test; real forwards verify native pipeline/provider image delivery. Invalid/released/foreign/cancelled/stale/mismatched selections fail closed. |
| Release / failure / terminal | tools.release invalidates large receipt even cached reads; selected release fails. Later failure discards whole selection; eligible explicit emits survive throw/OOM/deadline with outer error and last diagnostic while owner is live. Emitted receipts are pinned; release fails before native free. Terminal child never resumes JS; attaches original native receipt, cancels/disposes remaining calls. Finish cancels/drains/clears refs. | Package/native tests plus select-throw/release/terminal-success smoke. Large **legacy terminal error remains uncovered** because StepErr has no terminate; small structured terminal error already covered. |
| Accounting / gates | Host retains finalized direct-child usage/names/termination once after drain, including ignored/disposed/thrown outcomes. Sequential parent exclusion, native hooks/approvals, panic drain and termination latch preserved; no retry/replay or rollback. | Focused native tests and real 20-loop accounting/lifecycle checks. No duplicate accounting from attachment. Two complete default-parallel native regressions passed after the scoped-retirement fix. |
| Business JSON / hygiene | Preserve business runtimeKey/runtimeHandle/__rpi/__rpiRuntimeKey; ABI injected top-level __rpi remains host-owned. Both drivers guard actual credentials/metadata across callbacks, Pending, Done and errors, not ordinary names/numbers. | Existing differential/driver checks and real business/binding echo cases retained. Not malicious-plugin isolation; arbitrary encoding/direct I/O unguarded. |
| Catalog / aliases | Effective native batch snapshot and native hook finalization; added names do not authorize/refresh. Helper collisions call/list/emit/result/**release** accessible via tools.call. | Neutral differential checks and real alias case. Self/ancestors still excluded. |
| Inline output | Generated/emitted/small exact content stays 128 blocks / 1 MiB; separate 1 MiB final envelope. Native attachment bypasses inline payload limits, **not retention quota**. | Existing invalid-content/size cases retained. Ordered large/inline emission is supported within the attachment bounds below. |
| Mixed content attachments | tools.emit(receipt, indices?) uses captured ops/WeakMap identity without JS reads or native payload FFI. Ordered inline/reference parts; result.content forbidden, full resultReference mutually exclusive, strict keys, live ActionBridge resolution before destroy in both drivers. Caught native error content need not make outer result erroneous. | New smoke mixed/subset/repeated-image/error/throw/pin/overflow/index/terminal cases; no duplicate child execution/accounting or payload in summary. |
| Measurement / bounds | measure_result_content counts canonical serialized selected blocks via to_writer byte counter (no array punctuation/huge allocation). Omit/null all, [] none, reorder/repeat valid; native owner/range/cancel checks. Native 256 parts/blocks and 32 MiB final array checked before each clone. Engine 128 selected parts/blocks and combined 32 MiB **including 4096-byte reserve** for final diagnostic with reason bounded to 512 UTF-8 bytes/worst-case JSON escaping. | Generated inline 128 blocks/1 MiB and separate 1 MiB envelope unchanged; native payload not inline; no hard RSS ceiling. |
| Raw index safety | Validate before serialization in both guest paths: undefined/null all, otherwise max128 nonnegative integer primitives, once-read frozen null-prototype snapshot; captured intrinsics/property definition/primitive JSON encoding avoid setters/toJSON/NaN/Infinity/boxed-object widening. Native range validation remains authoritative. | Sticky failure prohibits later starts; new adversarial index smoke plus package tests. No ID-based authority forgery. |
| Failure eligibility probe | After cleanup an uncertain ordinary host-action/budget failure may recover only attachment eligibility via exact empty-index read-only measurements for every distinct already-emitted receipt (max16): matching resultId, zero blocks/bytes, exact bounded shape/frame. Sticky/root failure remains; no failed-operation retry, starts, payload reads or replay. | No recovery for cancelled/full-selection/terminal/known retained-cleanup failures; native final owner checks run again. Unit/source coverage, not a claim that all cleanup races are proven by smoke. |
| Progress / history baseline | Bounded parent lifecycle/count updates and intent/finish audit markers; Never-replay interrupted programs; no synthetic assistant tool-result history. | Native/source and smoke markers only. Not complete child events, bounded native history/query/replay/recovery or Web/ACP parity. |

## Remaining full-plan acceptance

| Remaining area | Required before claiming completion |
| --- | --- |
| Complete native child events | Preserve original bounded child identity/order/updates/content/final events and cancellation/late-event behavior without credential leaks. |
| Bounded native history/query/replay/recovery | Native transcript authority, bounded resumable reads, exact finalized outcomes, interruption/restart and snapshot/live sequence continuity. Audit markers and bounded final smoke snapshots are insufficient. Never replay uncertain effects. |
| extcore / HTTP / ACP / Web | Neutral core semantics first; thin HTTP/ACP mappings and frontend-owned cards. Verify full child cards, errors/progress, reload/reconnect/history, and public dev HMR in the approved `D:/Projects/rpi-package-dev` worktree on `dev/web-hmr`. None implemented here; that frontend remains untouched. |
| Protected recursion/self/ancestors | Explicit approval and tested authority/lifecycle/recursion policy; current exclusion prevents unrestricted full-toolset parity. |
| Wider real integration | Linux/macOS real native execution, live providers, reload races and separately identified installed binaries require fresh evidence. Local Windows fixture-provider success is not those claims. |

See [host contract](codemode-host-contract.md),
[parity contract](codemode-parity-contract.md), and
[package guide](../packages/rpi-codemode/README.md). Documentation does not upgrade
running binaries or extend the native-plugin security boundary.
