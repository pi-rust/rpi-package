# Code Mode full-parity matrix — first increment

Scope: companion host `D:/Projects/pi-rust-codemode` and package
`D:/Projects/rpi-package-codemode`. This is the **implemented first increment**
of the full-parity plan, not a declaration that the plan or unrestricted native
tool parity is complete. No source release, installation, deployment or UI
implementation is claimed here.

## Evidence checkpoint

- **Proven (unit/neutral layer):** 72 package tests passed; strict package Clippy
  passed (`cargo clippy --locked -p rpi-codemode --all-targets -- -D warnings`).
  Three native differential tests compare direct/nested business JSON and text
  signatures, hook rewrites/finalization and catalog batch boundaries.
- **Proven (host regression):**
  `cargo test --locked -p rpi-agent -p rpi-extensions -p rpi-plugin-sdk -p rpi-harness`
  passed **1109 tests, 6 ignored, 0 failed**. Log:
  `pi-rust-codemode/target/codemode-full-parity-tests.log`.
  Host `cargo fmt --all -- --check` passed.
- **Proven (real Windows source-build integration):** fresh actual host + Code
  Mode DLL + test-only fixture + gate passed **35 real smoke cases**: 20 base,
  14 fixture and one denial. Ten additions over the original 25 cover direct
  success/error baselines, exact success/error selection, deleted/undefined
  selected details normalized to native null, business JSON, actual binding
  success/error echoes and helper-name aliases.
- Final evidence: `pi-rust-codemode/target/codemode-full-parity-smoke-5/`.
  `results.json` reports PASSED with all 35 cases true; `provenance.json` retains
  the four artifact hashes below (verified against the local binaries).
  Earlier full-parity `-1/`, `-2/` and `-4/` runs failed; `-3/` passed 33 cases
  before the final missing-details fix. The original Windows 25-case evidence
  at `target/codemode-parity-native-smoke-5/` belongs to the previous increment.
- A read-only review found omitted selected details produced an invalid required
  envelope field. Engine/final-envelope normalization to null is covered by unit
  checks and two real cases in the final run. Native error authority is unchanged.
- No deployment, installed-binary, Web/ACP, live-provider or new CI-green claim.

| Final Windows artifact | SHA-256 recorded in provenance |
| --- | --- |
| Host `rpi.exe` | `5a04cd70140ea1a7484b6b67ff8bcd4240f1c322c74e906eacbec1d596317081` |
| Code Mode `rpi_codemode.dll` | `14e38076bf35325e342cac3d968f8d386b62784d5cfa7b95bd73aab1fb315873` |
| Fixture `codemode_parity_fixture.dll` | `e94be14e246749f454460edb8f6e0d8784430a3fbbb0daf8a81d9601fe0395bd` |
| Gate `rpi_permission_gate.dll` | `b52ce8ce8f10e032b701eb53cbca01e1abe04d567843aa96e2be9d880542cb34` |

## Implemented scope and qualification

| Area | Implemented behavior | Evidence/status and next acceptance |
| --- | --- | --- |
| Fail-closed negotiation | Protocol v1 requires all four exact integer caps `{resultEnvelope:1,hostAccounting:1,parentProgress:1,losslessValues:1}`. Older/missing/mismatched hosts fail before execute/list output; no fallback. | Proven by package checks and the real-host list/capability check in the final 35-case run; old/mismatched-host rejection remains unit coverage. |
| Business JSON | No recursive property-name blacklist. Legitimate `runtimeKey`, `runtimeHandle`, `__rpi`, `__rpiRuntimeKey` survive business schemas/args/results and guest returns. Native extension ABI still owns injected top-level `__rpi`, as with direct plugins. | Proven by package checks, neutral differential tests and real fixture schema/business round-trip. |
| Public-output guard | Invocation-local host matching of actual injected metadata/credentials across callbacks, Pending, legacy Done, envelope success/structured errors and plain tool errors, in polling and wake drivers; ordinary serialized JSON, aliases and collisions covered. No blanket bare-number redaction. | Driver checks cover these paths; real success/error binding echoes passed in the final run. Not a malicious-native-plugin sandbox: arbitrary encoding/direct plugin I/O unguarded. |
| Exact native selection | `tools.result(nativeResultOrErrorResult)` uses captured WeakMap identity and authoritative `isError`, including caught `error.result`. Exactly one selection; cannot mix emit or return another content object. Misuse is sticky. | Package checks cover success/error, fabricated identity, conflicts and failure precedence. Real exact success/error comparisons against direct baselines passed. |
| Exact output shape | Keeps supported content/text signatures/details/isError, without summary injection into selected details or synthetic error-text appending. Ordinary return/content emission remains unchanged. | Proven by package checks, real direct-baseline equality and two deleted/undefined details cases. Selection uses the current guest-mutated content/details snapshot; missing selected details normalizes to native null. Authoritative native isError remains independent. |
| Accounting/lifecycle | Host retains usage/names/termination; later throw, cancel, timeout, OOM, lifecycle failure or terminal child overrides selection. No automatic retry or side-effect rollback. | Package/neutral regression coverage plus final Windows accounting/terminal and exact-selection cases. Broader lifecycle claims remain bounded by the checks actually exercised. |
| Catalogs/hooks | Match effective native batch boundaries and native argument preparation/before/after hooks. `addedToolNames` neither authorizes tools nor refreshes the frozen guest catalog. | Three neutral differential tests prove business/signature/hook/catalog behavior; real helper aliases (`call/list/emit/result` via `tools.call`) passed. |
| Inline bounds | 128 selected blocks / 1 MiB encoded content and separate 1 MiB final-envelope bound. Reject/compact errors rather than silently truncate or turn failure into success. | Package coverage and final real guest-size/final-envelope-size rejection cases; no large-result transport claim. |
| Progress/history baseline | Bounded parent-associated lifecycle/update counts and intent/finish audit markers; Never-replay interrupted programs. No fabricated assistant tool-result history. | Existing baseline only, not complete child progress or original native child history snapshots/recovery. |

## Outstanding full-plan work and future acceptance

| Outstanding area | Acceptance required before claiming completion |
| --- | --- |
| Values over 1 MiB | Design host-backed refs/chunks/attachments with explicit ownership, bounded/resumable reads and cleanup. Compare direct/native and composed large content/details/images without truncation, duplicated payloads or authority widening. Exercise failure/cancel/reload cleanup. No such transport is implemented here. |
| Complete child progress | Preserve original native child updates, identity/order and content within explicit bounds rather than only lifecycle/count projection. Test success/error, late updates, cancellation, reconnect and snapshot/live boundaries without credential leakage. |
| Original native history snapshots/recovery | Define bounded native-owned child history/snapshot and recovery semantics, not a parallel adapter transcript store. Verify restart/interruption, bounded paging, exact finalized outcomes and no replay of uncertain effects. Audit markers alone do not satisfy this. |
| Web/ACP full child cards and reload | Expose neutral semantics through extcore, then thin HTTP/ACP mappings and frontend presentation. Verify live child cards, errors, progress, reload/reconnect/history and snapshot-to-live event continuity. A third UI dev worktree is approved; no UI changes have been made in this increment. |
| Protected self/ancestor exclusion | Document and test recursion/authority/lifecycle policy explicitly. Current tools exclude self/ancestors; do not claim 100% full-toolset parity while that exclusion remains. |
| Real integration qualification | Final 35-case Windows host/DLL/fixture/gate run passed with retained provenance/results. Linux/macOS native integration, reload races, live providers and installed-binary qualification still require their own evidence; local Windows success and CI configuration do not prove them. |

See the [host contract](codemode-host-contract.md),
[parity contract](codemode-parity-contract.md) and
[package guide](../packages/rpi-codemode/README.md). Documentation changes do not
upgrade a running binary or extend any native plugin security boundary.
