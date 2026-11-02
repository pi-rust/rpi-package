# Model-facing tool guidance

Tool metadata is part of the extension's public contract. A README does not
become model context automatically: the registered description and parameter
schema must be sufficient to choose and call the tool safely.

## Where guidance belongs

- **Tool description:** purpose, when to choose it over similar tools, important
  preconditions, externally visible side effects, and result limitations.
- **Parameter description/schema:** canonical names and nested item structures,
  action-dependent requirements, defaults, supported bounds and units. State
  whether a limit bounds results, characters, bytes, request time or total time.
- **README:** complete action tables, examples, compatibility aliases,
  configuration, known implementation limitations and operational details.
- **Runtime system guidance:** only workflow-specific advice that genuinely
  depends on runtime state. Do not append every tool's entire description to the
  system prompt or add an unconditional hook for static metadata.

The workspace's general extension SDK pin remains unchanged. Use capabilities
present in the SDK and host being built against; do not assume that an API
mentioned in documentation for another host version is available. This change
uses the existing `StableToolSchema` registration contract, not a new ABI slot.

## Schema compatibility

1. Advertise recommended canonical arguments, not every permissive parser alias
   or coercion. Runtime compatibility forms may remain accepted and documented
   without appearing as extra choices in the model schema.
2. Keep optional fields optional; do not turn every property into `required`.
   Where actions differ, explain required fields in parameter descriptions even
   if conditional JSON Schema also expresses them. Provider adapters may omit
   `if`/`then` and other validation keywords.
3. Give arrays concrete `items` schemas and objects concrete `properties`.
   Inline nested shapes rather than relying on `$ref`/`$defs`: the current
   Antigravity/Gemini converter does not resolve references. A union's canonical
   object branch must remain understandable when a provider flattens the union.
4. The host injects reserved `__rpi` context after model argument parsing.
   Tolerate extra top-level keys, but do **not** advertise reserved host context,
   runtime handles, capability keys or credentials as model parameters.
5. A schema bound need not enumerate every value the runtime clamps or tolerates.
   It should guide valid, predictable calls. Describe byte-based limits in text
   rather than mislabeling JSON Schema character counts as bytes.
6. Register and test the same metadata builder/constants/JSON files. Do not keep
   a second test-only schema or implement another JSON Schema validator solely
   to test these descriptions.

## Tool coverage

The guidance review covers 33 tools in these 17 extensions:

| Extension | Tools | Important contract |
| --- | --- | --- |
| rpi-ask-user | ask_user | Interactive UI, canonical inline option/question shapes, confirmation versus selection |
| rpi-codegraph | search, callers, callees, impact, explore, node, status, files (all codegraph_ prefixed) | CLI requirement, automatic index initialization/sync writes, bounded best-effort results |
| rpi-durable | durable | Branch-scoped checkpoints/recovery/manual queue; no automatic replay or scheduling |
| rpi-edit-diagnostics | edit | Read first, snapshot-based replacements, exact/normalized matching and failure limits |
| rpi-goal | goal | Branch-scoped objective, replacement/update and budget semantics |
| rpi-im-message | im_message_server | External sends, destructive FIFO receive, connection cleanup and uncertain delivery |
| rpi-langfuse | langfuse_score, langfuse_prompt, langfuse_trace | Action requirements, remote writes, named prompt lookup and one-page trace limits |
| rpi-lens | code_lens | Allowlisted diagnostics, working directory, build side effects and exit-code interpretation |
| rpi-mcp-adapter | mcp_request, mcp_call, mcp_list, mcp_resource | Raw versus managed sessions, target selection, untrusted remote data and subscription cleanup |
| rpi-orchestrator | subagent, plan, dispatch | Self-contained tasks, shared workspace, typed DAG nodes and explicit acceptance |
| rpi-permissions | permissions | User-authorized policy mutation, grant-deny versus rule removal, effective-policy checks |
| rpi-plan-mode | plan_mode_start, plan_mode_complete | Read-only design/review, not execution or automatic approval |
| rpi-server | rpc_server, rpc_client | TCP listener/child lifecycle, secret tokens, approval and nested protocol errors |
| rpi-subagents | delegate_task | Host-managed bounded child, inherited binding, shared files/state and report verification |
| rpi-todo | todo | Native-session list, returned stable IDs, explicit clearing |
| rpi-webfetch | webfetch | Public-page retrieval, truncation/error envelopes and untrusted content |
| rpi-websearch | websearch | Public reference lookup, result-count limits and source verification |

This is a metadata/documentation change, not a fix for existing lifecycle,
pagination, isolation or security limitations. In particular, documenting an
uncertain write does not make retry safe, a child is not a sandbox, and an index
query is not proof of completeness. Runtime changes need their own tests.

## Validation

Run the unit/integration tests of affected packages and lint their real targets.
Schema tests should check names, JSON shape, action defaults, nested structures,
required versus optional fields, key safety constraints and parser behavior
without contacting external services. Avoid exact full-prose comparisons.

```sh
cargo test --locked -p rpi-ask-user -p rpi-codegraph -p rpi-durable \
  -p rpi-edit-diagnostics -p rpi-goal -p rpi-im-message -p rpi-langfuse \
  -p rpi-lens -p rpi-mcp-adapter -p rpi-orchestrator -p rpi-permissions \
  -p rpi-plan-mode -p rpi-server -p rpi-subagents -p rpi-todo \
  -p rpi-webfetch -p rpi-websearch
```

For behavior changes, also run the relevant native/plugin smoke tests described
in each package README. Passing schema tests alone does not prove that a running
host has loaded the changed extension; installation/restart and model-call
measurements are separate steps.
