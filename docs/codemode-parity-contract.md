# Code Mode native parity contract (first full-parity increment)

Companion worktrees/PRs remain `pi-rust-codemode` (#238) and `rpi-package-codemode` (#187). This follow-up preserves the native tool chain, invocation authority, Never-replay recovery, cancellation drain gates and old plugin ABI layout. No Web/ACP changes or deployment.

This documents the **implemented first increment**, not completion of the full
parity plan. See the [status matrix](codemode-full-parity-matrix.md) for proven
checks, Windows source-build qualification, and outstanding work.

## Capability and final-result envelope

- ToolProgram v1 `list` adds `capabilities: { resultEnvelope: 1, hostAccounting: 1, parentProgress: 1, losslessValues: 1 }`. The updated extension requires all four exact integer version-1 capabilities and fails closed before guest execution on an older action-30 host; it must not quietly emit an envelope into a legacy lenient parser.
- Raw registration schema opts in with `"x-rpi-result-envelope": 1` (alongside existing Sequential marker).
- Opted-in ABI Done payload: `{ "$rpiToolResult": 1, "isError": boolean, "result": { "content": [text/image blocks], "details": any, "terminate": boolean, "usage"?: Usage|null, "addedToolNames"?: string[] } }`.
- Only opted-in tools use this parser; all legacy Done/Err behavior remains unchanged. Malformed opted-in envelopes fail closed. Parsing must preserve error + structured result simultaneously, via a native structured-tool-error variant handled by the existing loop before after hooks. No ABI slot/layout change. The SDK pin stays unchanged.
- Extension ordinary script failures now use an isError Done envelope retaining the bounded summary in details/text, not only ABI Err strings. Unsupported binding/lifecycle infrastructure failures may still use ABI Err. If output is oversized, compact the error while preserving terminate; never turn an error into success.

## Host-owned accounting and execution controls

- The host retains finalized metadata for each direct nested request exactly once, independent of poll/dispose and script return values. Aggregate available finalized usage and added tool names after descendant drain, even if the script throws/cancels or ignores an already completed result.
- Merge direct-child metadata into the containing native tool outcome (success or structured/ordinary error). Multi-level nesting must not double-count: each node rolls up its own direct children; the extension must NOT sum/re-emit child usage as its own usage.
- Use checked accounting (including Usage optional fields/cost), bounded/deduplicated tool names and deterministic overflow/error behavior; do not panic or silently accept NaN/infinite totals. Existing after hooks remain authoritative as with direct native execution.
- Finalized child termination is also retained on the host, including when result/frame encoding fails. Preserve the current termination admission latch and attach the available termination decision to the outer result even if the extension fails/compacts output.
- Catalogs match native batch boundaries: each invocation uses its batch's effective tool snapshot; the extension freezes discovery for that execution. `addedToolNames` is metadata, not authorization, registry mutation or catalog refresh. Later invocations see legitimate native batch changes. Protected self/ancestor tools remain excluded, so this is not 100% full-toolset parity. Panic outcomes without finalized results remain lifecycle failures; no automatic retry/replay.

## Lossless business JSON and output hygiene

- `losslessValues:1` removes the blanket recursive property-name blacklist.
  Legitimate `runtimeKey`, `runtimeHandle`, `__rpi` and `__rpiRuntimeKey` properties
  survive in business schemas/arguments/results and guest returns. The native
  extension ABI still owns injected top-level `__rpi`, as with direct plugins.
- An invocation-local host guard sanitizes actual injected metadata and credential
  values in callbacks, Pending, legacy Done, envelope success/structured errors
  and plain tool errors for polling and wake drivers. Ordinary serialized JSON,
  aliases and credential-redaction collisions are covered. Ordinary bare numbers
  are not blanket-redacted; business names alone do not trigger stripping.
- This is output hygiene, not a sandbox against malicious native plugins.
  Arbitrary encoding and direct plugin I/O are unguarded.

## Native content output

- JS receives the existing finalized result objects. Returning a native result object with a valid `content` array selects those blocks for outer native output. Ordinary JSON returns remain JSON summary data.
- Add `tools.emit(resultOrContentArray)` for explicitly selecting native text/image blocks from intermediate results while returning some other JSON summary. This is output selection, NOT a host tool call, and does not affect usage or permissions.
- Validate supported blocks, MIME/data shape and total count/encoded byte bounds. Text supports native optional `textSignature` (string/null); image supports `type`, `mimeType` and canonical standard-base64 `data`. Unsupported block properties are rejected explicitly rather than silently lost by typed native decoding. Emission serialization/validation/budget failures are sticky even when caught, including cyclic values and values that stringify to undefined. Do not auto-forward all intermediate images.
- Keep selected image bytes in native outer content, not multiple JSON-string copies; summaries describe promoted content without duplicating its base64. For ordinary return/content emission, on script failure append a bounded native error-text block even if the selection is empty or image-only. This is not the exact-selection behavior below. Native image normalization handles structured errors too. The composition owns a descendant cancellation token so closing its binding cannot cancel containing-tool image postprocessing. No guessing nested arbitrary JSON fields as images.

### Exact native result selection (new)

`tools.result(nativeResultOrErrorResult)` selects a captured child result or
caught `error.result`, returning `undefined`. A captured WeakMap ties object
identity to authoritative native `isError`; cloned/fabricated inputs cannot forge
it. Exactly one selection is allowed, without `tools.emit` or returning another
content object. Invalid/conflicting/multiple selections and serialization misuse
are sticky failures even if caught. This is object association, not an immutable
snapshot of mutable guest content/details.

Selection uses the current guest-mutated content/details snapshot, not an
immutable copy. Missing selected `details` (including deletion or `undefined`)
is normalized to native `null` in the required result-envelope field.
Authoritative native `isError` remains independent of guest mutations.

On normal completion, exact selection preserves supported native content,
text signatures, details and `isError`, without summary injection into selected
details or synthetic error-text appending. Ordinary return/content emission is
unchanged. Usage, added names and termination remain host-authoritative. Later
throw, cancel, timeout, OOM, lifecycle failure or terminal child overrides the
selection. Limits remain inline **128 blocks / 1 MiB content**, plus the 1 MiB
final envelope; no large-result transport is implemented.

## Parent-associated progress

- Host emits bounded progress via the existing outer native ToolExecutionUpdate callback, never ordinary nested ToolExecutionStart/MessageEnd/transcript tool results. Callback ownership/lifetime stays native; capture it in ToolProgram after constructing the normal parent update gate.
- Include root/parent/request identity, tool name and lifecycle phase (started/progress/completed/failed). Minimum useful projection is lifecycle + update count; do NOT forward raw arguments, invocation capabilities, unfiltered partial details or images as progress. This preserves identity/privacy and requires no extension worker queue or frontend changes.
- Bound updates and bytes across the invocation tree; discard/suppress late updates after cancellation/completion. Retained nested callbacks hold weak program references to avoid registry ownership cycles. Keep existing late-update and descendant drain invariants. Runtime action polling remains unchanged; progress does not require new SDK ABI.

## Validation checkpoint

- Package unit evidence: **72 passing tests**, including four-capability
  negotiation, lossless business JSON, exact selection, sticky conflicts,
  authoritative error identity and later failure/termination precedence.
- Three new neutral-layer native differential tests prove direct/nested business
  JSON and text signatures, hook rewrites/finalization and catalog batch behavior.
  They are not real DLL/provider or Web/ACP qualification.
**Current-increment local evidence:** fresh actual Windows host + Code Mode DLL,
test-only fixture and permission gate passed **35 real smoke cases**: 20 base,
14 fixture and one denial. Ten additions over the original 25 cover direct
success/error baselines, exact success/error selection, deleted/undefined selected
details normalized to native null, business JSON, actual binding success/error
echoes and helper-name aliases (`call/list/emit/result` via `tools.call`).
Final `results.json` reports PASSED for all 35 cases; `provenance.json` retains
all four artifact SHA-256 values. Evidence directory: `pi-rust-codemode/target/codemode-full-parity-smoke-5/`.
Earlier full-parity runs `-1/`, `-2/` and `-4/` failed; `-3/` passed 33 cases
before the final missing-details fix. They are not the final qualification.
A read-only review found that omitted selected details produced an invalid
required-field envelope; engine/final-envelope normalization to null is covered
by unit checks and two real cases in the final run.

Host `cargo test --locked -p rpi-agent -p rpi-extensions -p rpi-plugin-sdk -p rpi-harness`
passed **1109 tests, 6 ignored, 0 failed**; log: `pi-rust-codemode/target/codemode-full-parity-tests.log`.
Host `cargo fmt --all -- --check` passed. Package evidence is **72 passing tests**
and strict all-target Clippy passed with
`cargo clippy --locked -p rpi-codemode --all-targets -- -D warnings`.
This is Windows local source-build qualification, not deployment, installation,
live-provider or Linux/macOS integration qualification, or a new CI-green claim.

## Outstanding full-plan work

Host-backed references/chunks/attachments for values over 1 MiB; complete child
progress and original native history snapshots/recovery; Web/ACP full child cards
and reload/reconnect behavior; and explicit handling of protected self/ancestor
exclusion remain outstanding. A third UI dev worktree was approved; no UI changes
are implemented in this increment. Future acceptance and evidence requirements
are in the [matrix](codemode-full-parity-matrix.md).

## Acceptance checks (not a claim all have passed)

- Opted-in success and isError structured result round-trip; legacy plugins unchanged; malformed/version-mismatched envelope rejection; old action-30 host fails capability negotiation.
- Usage/name rollup for success, JS error, disposed/unawaited completed calls, cancel/drain, oversized terminal result, nested programs; no repeated-poll/double-counting and accounting-overflow panic.
- Tiny real PNG returned by a native read becomes an outer image block; explicit emission/ordinary JSON/bad content/oversize cases; no automatic intermediate image forwarding.
- Parent progress before final completion, root identity/no synthetic transcript calls, bounded updates, cancellation/late-update suppression and no runtime capability leaks.
- Relevant package suites, strict extension Clippy, build actual native host/DLL and rerun isolated real native smoke. Retain provenance/results before claiming qualification; no deployment.
