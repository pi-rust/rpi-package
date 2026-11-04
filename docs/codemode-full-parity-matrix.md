# Code Mode full-parity matrix — second increment

Scope: companion host `D:/Projects/pi-rust-codemode` and package
`D:/Projects/rpi-package-codemode`. **Second increment implemented; overall full
parity INCOMPLETE.** No unrestricted full-toolset, source release, installation,
deployment, production restart, merge or CI-green claim. Current dev frontend
is untouched; approval for a third UI dev worktree does not prove UI work.

## Current evidence checkpoint

| Evidence | Proven scope / limits |
| --- | --- |
| Fresh Windows host + Code Mode DLL + fixture + gate | `pi-rust-codemode/target/codemode-reference-native-smoke-3/results.json`: **PASSED, 46 cases, all true**. Four provenance hashes below verified against actual binaries. |
| 11 new real-native smoke cases | Four direct baselines and four composed counterparts: 2 MiB text, actual >1 MiB PNG, legacy plain error, terminal success. Plus frozen lazy inspection/three rejected mutations, 20 inspect/release loops with once-only accounting, and select-then-JS-throw. |
| Large-payload proof boundary | Isolated sessions compare canonical native end payload hashes against direct baselines; image data may be normalized by native handling. Actual local fixture-provider requests validate image delivery, streamed native transcript markers validate lifecycle, and final snapshots are bounded. **Not** full get_messages large-history equality or live-provider qualification. |
| Current package checks | **92 tests**, strict all-target Clippy and fmt passed. |
| Current focused native checks | **33 agent tests**, **206 extension unit tests + 5 integration tests (1 ignored)** passed. One new extension test covers **22 scenarios across both drivers**, not 22 test functions. |
| Broad native regression | Default-parallel agent/extensions/harness/SDK suite passed **1,117 tests, 0 failed, 6 ignored**, twice: `target/bridge-owner-all-parallel.log` and `target/bridge-owner-all-parallel-repeat.log`. Historical **1109 / 6 ignored** and the 35-case smoke qualify the first increment only. |
| Retirement regression | A broad parallel attempt exposed cross-session pinning in the global bridge scan. Owner identity now filters weak registry entries **before** upgrading. The deterministic regression fails with the original scan; existing immediate-drop assertions remain unchanged. No claim about historical CI failure. |
| Failed reference attempt | `codemode-reference-native-smoke-1` failed an obsolete harness terminal-summary assertion. Terminal completion now directly attaches the original native receipt. Never cite `-1` as passing. |

Final evidence files:
`pi-rust-codemode/target/codemode-reference-native-smoke-3/{results.json,provenance.json}`.

| Actual Windows artifact recorded in provenance | Verified SHA-256 |
| --- | --- |
| Host `pi-rust-codemode/target/debug/rpi.exe` | `185f0e7c031fb703c5a59d18047359e8a42e981d91c5658ed907205b9eb01383` |
| Code Mode `rpi-package-codemode/target/debug/rpi_codemode.dll` | `85761d9c99e102a8150ece8f043b4946cdad10edeadc5aff3dbcfc7d2b35f4df` |
| Fixture `pi-rust-codemode/target/codemode-parity-fixture/debug/codemode_parity_fixture.dll` | `d849ab1773aa0a463b0fc7f61e393d7c3066eba8946e24e20c6e9c632a29a245` |
| Gate `rpi-package-codemode/target/debug/rpi_permission_gate.dll` | `b52ce8ce8f10e032b701eb53cbca01e1abe04d567843aa96e2be9d880542cb34` |

## Implemented versus proven

| Area | Implemented behavior | Evidence / qualification boundary |
| --- | --- | --- |
| Negotiation / ABI | Protocol v1 adds native optional `resultReferences:1`; extension requires all **five** exact integer caps (also resultEnvelope, hostAccounting, parentProgress, losslessValues). Fail closed, no fallback. SDK ABI/layout unchanged. | Package rejection checks; real smoke checks all five. Native clients omitting resultMode and non-opted plugins preserve original behavior. |
| Reference transport | Extension starts `resultMode:'reference'`; poll manifest has version/id/byteLength/contentBlocks/isError/terminate. read_result uses strict byte offsets, integer 4..65536 limit, UTF-8 boundaries, stable EOF and ≤1 MiB responses. release_result is owner-only; dispose independent. | Native/package tests cover malformed manifests/chunks, lifetime and cleanup; real large forwards prove host/DLL integration, not all race cases. |
| Native storage | 16 references/program, shared 64 MiB tree quota: `2*serialized_bytes+4096+128*details_nodes+256*content_blocks`. Quota implies serialized size below 32 MiB; package guards advertised size at 32 MiB. | Unit quota/release checks and 20 real release loops. **No hard RSS ceiling**: native clone/serialization precedes quota admission; Rust JSON lies outside 32 MiB guest heap. |
| Small mutable selection | Nonterminal ≤1 MiB results materialize and release native storage; tools.result uses current guest-mutated content/details, deleted/undefined details → null, native isError via WeakMap. | Existing small direct/error/deleted-details cases retained in 46-case run, plus package checks. tools.release on genuine small results is a native no-op. |
| Large read-only selection | >1 MiB frozen lazy genuine WeakMap receipt. Inspection materializes/deep-freezes whole receipt under 32 MiB guest heap. tools.result forwards original native result without materialization. | Real large text/image/error direct-baseline comparisons and inspection/mutation rejection. Admitted receipt need not fit inspection heap. Whole-large emit and multiple/mixed full receipts unsupported. |
| Attachment / drivers | Metadata-only opted-in Done envelope resolves through captured live owning invocation before destroy in **both** drivers; preserves native content/details/isError/terminate, strips usage/names before host merge. | 22 new both-driver scenarios in one native test; real forwards verify native pipeline/provider image delivery. Invalid/released/foreign/cancelled/stale/mismatched selections fail closed. |
| Release / failure / terminal | tools.release invalidates large receipt even cached reads; selected release fails. Later throw/cancel/timeout/OOM/lifecycle failure discards selection. Terminal child never resumes JS; attaches original native receipt, cancels/disposes remaining calls. Finish cancels/drains/clears refs. | Package/native tests plus select-throw/release/terminal-success smoke. Large **legacy terminal error remains uncovered** because StepErr has no terminate; small structured terminal error already covered. |
| Accounting / gates | Host retains finalized direct-child usage/names/termination once after drain, including ignored/disposed/thrown outcomes. Sequential parent exclusion, native hooks/approvals, panic drain and termination latch preserved; no retry/replay or rollback. | Focused native tests and real 20-loop accounting/lifecycle checks. No duplicate accounting from attachment. Two complete default-parallel native regressions passed after the scoped-retirement fix. |
| Business JSON / hygiene | Preserve business runtimeKey/runtimeHandle/__rpi/__rpiRuntimeKey; ABI injected top-level __rpi remains host-owned. Both drivers guard actual credentials/metadata across callbacks, Pending, Done and errors, not ordinary names/numbers. | Existing differential/driver checks and real business/binding echo cases retained. Not malicious-plugin isolation; arbitrary encoding/direct I/O unguarded. |
| Catalog / aliases | Effective native batch snapshot and native hook finalization; added names do not authorize/refresh. Helper collisions call/list/emit/result/**release** accessible via tools.call. | Neutral differential checks and real alias case. Self/ancestors still excluded. |
| Inline output | Generated/emitted/small exact content stays 128 blocks / 1 MiB; separate 1 MiB final envelope. Native attachment bypasses inline payload limits, **not retention quota**. | Existing invalid-content/size cases retained. No arbitrary multi-large-content composition claim. |
| Progress / history baseline | Bounded parent lifecycle/count updates and intent/finish audit markers; Never-replay interrupted programs; no synthetic assistant tool-result history. | Native/source and smoke markers only. Not complete child events, bounded native history/query/replay/recovery or Web/ACP parity. |

## Remaining full-plan acceptance

| Remaining area | Required before claiming completion |
| --- | --- |
| Complete native child events | Preserve original bounded updates, identity/order/content and cancellation/late-event behavior without credential leaks. |
| Bounded native history/query/replay/recovery | Native transcript authority, bounded resumable reads, exact finalized outcomes, interruption/restart and snapshot/live sequence continuity. Audit markers and bounded final smoke snapshots are insufficient. Never replay uncertain effects. |
| extcore / HTTP / ACP / Web | Neutral core semantics first; thin HTTP/ACP mappings and frontend-owned cards. Verify full child cards, errors/progress, reload/reconnect/history, and public dev HMR in the approved third UI dev worktree. None implemented here. |
| Multiple/mixed large output | Define native combination/ownership/accounting/bounds for multiple full receipts and generated content. Whole-large tools.emit and mixing selections remain intentionally unsupported. |
| Protected recursion/self/ancestors | Explicit approval and tested authority/lifecycle/recursion policy; current exclusion prevents unrestricted full-toolset parity. |
| Wider real integration | Linux/macOS real native execution, live providers, reload races and separately identified installed binaries require fresh evidence. Local Windows fixture-provider success is not those claims. |

See [host contract](codemode-host-contract.md),
[parity contract](codemode-parity-contract.md), and
[package guide](../packages/rpi-codemode/README.md). Documentation does not upgrade
running binaries or extend the native-plugin security boundary.
