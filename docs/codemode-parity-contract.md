# Code Mode native parity contract (follow-up)

Companion worktrees/PRs remain `pi-rust-codemode` (#238) and `rpi-package-codemode` (#187). This follow-up preserves the native tool chain, invocation authority, Never-replay recovery, cancellation drain gates and old plugin ABI layout. No Web/ACP changes or deployment.

## Capability and final-result envelope

- ToolProgram v1 `list` adds `capabilities: { resultEnvelope: 1, hostAccounting: 1, parentProgress: 1 }`. The updated extension requires these capabilities and fails clearly before guest execution on an older action-30 host; it must not quietly emit an envelope into a legacy lenient parser.
- Raw registration schema opts in with `"x-rpi-result-envelope": 1` (alongside existing Sequential marker).
- Opted-in ABI Done payload: `{ "$rpiToolResult": 1, "isError": boolean, "result": { "content": [text/image blocks], "details": any, "terminate": boolean, "usage"?: Usage|null, "addedToolNames"?: string[] } }`.
- Only opted-in tools use this parser; all legacy Done/Err behavior remains unchanged. Malformed opted-in envelopes fail closed. Parsing must preserve error + structured result simultaneously, via a native structured-tool-error variant handled by the existing loop before after hooks. No ABI slot/layout change. The SDK pin stays unchanged.
- Extension ordinary script failures now use an isError Done envelope retaining the bounded summary in details/text, not only ABI Err strings. Unsupported binding/lifecycle infrastructure failures may still use ABI Err. If output is oversized, compact the error while preserving terminate; never turn an error into success.

## Host-owned accounting and execution controls

- The host retains finalized metadata for each direct nested request exactly once, independent of poll/dispose and script return values. Aggregate available finalized usage and added tool names after descendant drain, even if the script throws/cancels or ignores an already completed result.
- Merge direct-child metadata into the containing native tool outcome (success or structured/ordinary error). Multi-level nesting must not double-count: each node rolls up its own direct children; the extension must NOT sum/re-emit child usage as its own usage.
- Use checked accounting (including Usage optional fields/cost), bounded/deduplicated tool names and deterministic overflow/error behavior; do not panic or silently accept NaN/infinite totals. Existing after hooks remain authoritative as with direct native execution.
- Finalized child termination is also retained on the host, including when result/frame encoding fails. Preserve the current termination admission latch and attach the available termination decision to the outer result even if the extension fails/compacts output.
- No catalog refresh/authority widening is implied by addedToolNames. Panic outcomes without finalized results remain lifecycle failures; no automatic retry/replay.

## Native content output

- JS receives the existing finalized result objects. Returning a native result object with a valid `content` array selects those blocks for outer native output. Ordinary JSON returns remain JSON summary data.
- Add `tools.emit(resultOrContentArray)` for explicitly selecting native text/image blocks from intermediate results while returning some other JSON summary. This is output selection, NOT a host tool call, and does not affect usage or permissions.
- Validate supported blocks, MIME/data shape, reserved metadata filtering and total count/encoded byte bounds. Text supports native optional `textSignature` (string/null); image supports `type`, `mimeType` and canonical standard-base64 `data`. After reserved-field filtering, unsupported block properties are rejected explicitly rather than silently lost by typed native decoding. Emission serialization/validation/budget failures are sticky even when caught, including cyclic values and values that stringify to undefined. Do not auto-forward all intermediate images.
- Keep selected image bytes in native outer content, not multiple JSON-string copies; summaries describe promoted content without duplicating its base64. On script failure, append a bounded native error-text block even if the selection is empty or image-only. Native image normalization handles structured errors too. The composition owns a descendant cancellation token so closing its binding cannot cancel containing-tool image postprocessing. No guessing nested arbitrary JSON fields as images.

## Parent-associated progress

- Host emits bounded progress via the existing outer native ToolExecutionUpdate callback, never ordinary nested ToolExecutionStart/MessageEnd/transcript tool results. Callback ownership/lifetime stays native; capture it in ToolProgram after constructing the normal parent update gate.
- Include root/parent/request identity, tool name and lifecycle phase (started/progress/completed/failed). Minimum useful projection is lifecycle + update count; do NOT forward raw arguments, invocation capabilities, unfiltered partial details or images as progress. This preserves identity/privacy and requires no extension worker queue or frontend changes.
- Bound updates and bytes across the invocation tree; discard/suppress late updates after cancellation/completion. Retained nested callbacks hold weak program references to avoid registry ownership cycles. Keep existing late-update and descendant drain invariants. Runtime action polling remains unchanged; progress does not require new SDK ABI.

## Acceptance

- Opted-in success and isError structured result round-trip; legacy plugins unchanged; malformed/version-mismatched envelope rejection; old action-30 host fails capability negotiation.
- Usage/name rollup for success, JS error, disposed/unawaited completed calls, cancel/drain, oversized terminal result, nested programs; no repeated-poll/double-counting and accounting-overflow panic.
- Tiny real PNG returned by a native read becomes an outer image block; explicit emission/ordinary JSON/bad content/oversize cases; no automatic intermediate image forwarding.
- Parent progress before final completion, root identity/no synthetic transcript calls, bounded updates, cancellation/late-update suppression and no runtime capability leaks.
- Relevant package suites, strict extension Clippy, build actual native host/DLL and rerun isolated real native smoke. Update existing PRs after verification; no deployment.
