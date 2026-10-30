# Actual native runtime schema acceptance

Status: **FAIL for captured v2 output; INCOMPLETE overall acceptance**. Acceptance-only; no runtime edits, builds, installs, or fabricated client frames.

## Scope and provenance

Validated 29 actual frame dumps (887 captured entries) from the named child-correlation and sampling suites, plus `acp-modalities-run-worker-03`. The OAuth suite directory exists but contains no discovered `frames.json` / `acp-frames.json`: its successful fixture assertions are not wire-schema acceptance.

The child suite `result.json` records native SHA-256 `758d107a4cfe9ff6a40b343c64992700d31a31e56258c67d56d4a846c3ae561b` and bridge `8ca1cc15dff412675f41169b33faf1bcee85cb8a78231d47bd3734256ede2164`. These are prior current-capture identities, NOT a claim about the latest ordered/partial runtime build. Other workers own ordered capture/runtime work.

Full official schema files in `D:/Temp/rpi-acp-work-ecefa506c465420d8c9d2a7453b88ee9` are SHA-256 pinned:
- v1: `1c57095acf9c0844054dd1fdc3624c121b3fdc24209ecc987fd6748c705fdef5`
- v2: `932d41d2ba07142af6e7294675337154a034e2ce033c096fc079053a76b0925e`

## Results

| Version | Captures | Entries | Validated | Failed frames | Unsupported | Unbound replies | Pending requests | Status |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 16 | 470 | 352 | 0 | 21 | 106 | 74 | INCOMPLETE |
| 2 | 13 | 417 | 294 | 9 | 30 | 94 | 40 | FAIL |

Validator controls: **28 passed**. Negative controls reject known invalid prompt/content, diff, auth, permission, plan, elicitation, tool update and error variants; positive `_meta` parent association is accepted. Draft subagent and future session-update variants are not promoted to known official acceptance.

Counts are evidence categories, not mutually exclusive: unbound errors can have their Error payload validated; requests can validate while their response remains uncaptured. EOF fixture markers are excluded from validated counts. Frame indexes below are zero-based; pointers are relative to the transmitted frame. Missing-field pointers identify the required absent location.

## Concrete captured defects

### `C:\Users\bigfish\.cargo\target\acp-child-correlation-run-parent-final-01\v2-normal\acp-frames.json`
Capture SHA-256: `b1fbc23df56ee2601a92bd5dc11bd877e9f1a059e5b6e3d71bc8e565afcf3992`

- Frame 2, `session/update`, `/params/update/availableCommands/0/type`: required property missing (`UpdateSessionNotification`).
- Frame 4, `session/update`, `/params/update/availableCommands/0/type`: required property missing (`UpdateSessionNotification`).
- Frame 19, `session/request_permission`, `/params/title`: required property missing (`RequestPermissionRequest`).
- Frame 26, `session/request_permission`, `/params/title`: required property missing (`RequestPermissionRequest`).
- Frame 33, `session/request_permission`, `/params/title`: required property missing (`RequestPermissionRequest`).
- Frame 65, `session/request_permission`, `/params/title`: required property missing (`RequestPermissionRequest`).
- Frame 72, `session/request_permission`, `/params/title`: required property missing (`RequestPermissionRequest`).
- Frame 79, `session/request_permission`, `/params/title`: required property missing (`RequestPermissionRequest`).
### `C:\Users\bigfish\.cargo\target\acp-modalities-run-worker-03\v2\acp-frames.json`
Capture SHA-256: `289175bbf50817299fd1a302cbf956df497b57fabb590b3b6bdb5f446f2141a5`

- Frame 119, `/params/update`: {'sessionUpdate': 'user_message', 'messageId': 'history:01a18bb3-717d-7190-b8da-a40f5b8e0668', 'content': [{'type': 'resource', 'resource': {'uri': 'http://127.0.0.1:60417/never-fetch', 'text': None}}]} is not valid under any of the given schemas
- Frame 119, `/params/update/content/0`: {'type': 'resource', 'resource': {'uri': 'http://127.0.0.1:60417/never-fetch', 'text': None}} is not valid under any of the given schemas
- Frame 119, `/params/update/content/0/resource`: {'uri': 'http://127.0.0.1:60417/never-fetch', 'text': None} is not valid under any of the given schemas

The v2 command advertisements require `availableCommands[0].type`; the v2 permission request requires top-level `params.title`. These are defects in captured output, not failures of unrelated union alternatives. Full JSON retains original schema rejection diagnostics as well as selected-variant checks. No remediation of runtime data was attempted.

## Positive audit and limits

Known notification variants observed (including failed instances): `agent_message_chunk`=77, `available_commands_update`=67, `state_update`=67, `tool_call`=70, `tool_call_update`=158, `usage_update`=65, `user_message`=36, `user_message_chunk`=2.
Actual agent-originated client requests, including v1 filesystem/terminal and permission/elicitation payloads, are matched to named official request definitions, not generic envelopes. The v2 filesystem/terminal method bindings absent from the pinned AgentRequest are unsupported, not runtime schema failures. Draft `subagent_update` / `session_message` notifications are unsupported, not future-fallback acceptance.

Capture arrays are agent-to-client only unless explicit direction/frame records exist. No originating initialize/prompt request or client reply is invented; response result shapes and numeric IDs do not determine methods. Existing captured negotiation results and adjacent `initialize.json` / `protocol-version.json` metadata establish version, not request correlation. Pending requests are incomplete evidence, not schema failures. Modalities output supplies positive notification audit only: uncaptured incoming multimodal prompts are not accepted by this report.

Validator-only fixes: suppress misleading leaf diagnostics from unrelated union alternatives; emit precise missing-property JSON pointers; read captured adjacent negotiation metadata; aggregate per-version statuses. Native implementation unchanged.

## Reproduce

```text
D:/Temp/rpi-acp-work-ecefa506c465420d8c9d2a7453b88ee9/verify-env/Scripts/python.exe -B packages/rpi-acp/tests/runtime_schema_validation.py --schema-dir D:/Temp/rpi-acp-work-ecefa506c465420d8c9d2a7453b88ee9 --self-test --captures C:/Users/bigfish/.cargo/target/acp-child-correlation-run-parent-final-01 C:/Users/bigfish/.cargo/target/acp-sampling-native-run-acp-backpressure-parent-01 C:/Users/bigfish/.cargo/target/acp-oauth-backpressure-parent-01 C:/Users/bigfish/.cargo/target/acp-modalities-run-worker-03 > C:/Users/bigfish/.cargo/target/acp-current-runtime-schema-results.json
```

Exit 1 is expected for the actual v2 defects. Full machine results: `C:/Users/bigfish/.cargo/target/acp-current-runtime-schema-results.json`. Remaining acceptance includes ordered bidirectional capture, missing OAuth frames, absent official draft definitions, GET listener/durable replay/browser/third-party coverage; no claim of all ACP messages or RFDs.

## Modalities failure clarification

The ninth v2 failed frame is modalities frame 119: emitted `user_message` contains an embedded resource at `/params/update/content/0/resource` with `text: null` and no blob alternative. The captured output fails the official resource union (the null text is at `/params/update/content/0/resource/text`). This is an invalid emitted notification, not proof the incoming prompt was valid: incoming client requests are absent, so whether it originated in a deliberately invalid prompt is unbound. Do not interpret this as a demonstrated valid-input-to-invalid-output conversion defect.
