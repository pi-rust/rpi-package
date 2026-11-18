# Host requirements

What `rpi-optchat` needs from the `rpi` host. Each item names the code or
behaviour it depends on, what the plugin does without it, and how the change
can be accepted.

Status: items 1 and 4 are implemented on the host's `feat/session-tree-lifecycle`
branch (verified against `pi-rust` `b0ac963` + that branch's working tree) and
consumed here; items 2, 3 and 5 are still open. The plugin keeps its
workarounds, because an installed host may predate the branch and the plugin has
no way to ask which behaviours it dispatches.

### Implemented on the host branch

1. Session-tree lifecycle dispatch — see below.
4. Entry-renderer request/response contract — see below.

Items 2, 3 and 5 remain outstanding.

## 1. Dispatch the session-tree lifecycle events — implemented

Accepted on `feat/session-tree-lifecycle`; the plugin now registers all eight
session tags and resets its branch cache from them. The text below is the
original request; the probe described at the end is kept as the fallback for
older hosts.

`rpi_plugin_sdk::EventTag` declares `SessionBeforeSwitch`, `SessionBeforeFork`,
`SessionBeforeCompact`, `SessionCompact`, `SessionBeforeTree`, and `SessionTree`,
but the host never dispatches them:

```text
$ grep -rn "EventTag::SessionTree\|EventTag::SessionBeforeTree\|EventTag::SessionBeforeSwitch\|\
EventTag::SessionBeforeFork\|EventTag::SessionCompact" crates --include=*.rs | grep -v plugin-sdk
(no output)
```

The only dispatched session tags are `SessionStart` (`app.rs:639`,
`interactive_tui.rs:3132`) and `SessionShutdown` (`app.rs:694`,
`modes.rs:3137`, `session.rs:1847`).

Why it matters: memory is keyed to the current session **branch**. `navigate_tree`
and fork change the branch without changing `RPI_SESSION_ID`, so a plugin cannot
tell that its cached log now belongs to another branch. Today the plugin detects
this with a watermark (`GetSessionBranch` with `limit: 1`, `order: newestFirst`):
a newest `seq` lower than the cached one means the branch moved, so the log is
reloaded. That works, but it costs a host call per turn boundary and cannot
distinguish "branch replaced" from "branch truncated".

Proposed: dispatch `SessionBeforeTree`/`SessionTree` around navigation and
`SessionBeforeFork`/`SessionBeforeSwitch` around those transitions, payload-free
like `SessionStart`. Acceptance: a plugin registered for `SessionTree` observes
one invocation per branch switch in an interactive session, and a test asserts a
handler fires on `navigate_tree`.

Workaround shipped here: watermark reload plus reload-on-demand for every tool
and command call.

## 2. Publish `GetExecutionContext` (blocking for model-written summaries)

The host implements `RuntimeActionId::GetExecutionContext` as raw action id `23`
— `packages/rpi-orchestrator/src/host.rs` documents it, and hardcodes the number
because the published SDK does not declare it:

```rust
const GET_EXECUTION_CONTEXT: u32 = 23;
```

`rpi-plugin-sdk = "=0.3.20"` stops at `UiComponent = 22`, and the workspace pins
that exact version, so every plugin that needs the parent binding must hardcode
an undocumented id.

Why it matters: a model-written summary needs the provider/model the session is
already running on; a child `rpi -p` process otherwise falls back to the default
provider, which is the wrong model or unauthenticated.

Proposed: declare the variant in the SDK (`GetExecutionContext`), publish it, and
bump the workspace pin. Acceptance: a plugin reads the parent provider/model with
no numeric literal, and the reply shape stays
`{"model":{"id","provider","api"},"thinkingLevel":…}`.

Workaround shipped here: the id is isolated in one constant with a test that
fails if the host rejects it, and compaction degrades to extractive summaries
when the action is unavailable.

## 3. Investigate tool events in print mode (observed, not yet explained)

Observed on `rpi 0.24.0` with the same plugin build:

| mode | `MessageEnd` | `tool_execution_start` / `end` |
| --- | --- | --- |
| `--mode rpc` | received | received, captured as `tool`/`echo` entries |
| `-p` (print) | received | **not received** |

Reading the host code, both modes share one harness and one emitter
(`session.rs:1116-1125` builds `TeeEmitter{broadcast, ExtensionEmitter}`;
`app.rs:579` routes print to `modes::print`, which drives
`harness.lane("main").prompt_text(…)` like RPC drives the same lane). So the
divergence is not explained by "print mode has no emitter".

Ask: confirm whether this is intended, and if not, locate the emitter path that
print mode uses for tool events. Reproduction: a plugin that logs every tag it
receives; run one prompt that triggers a tool under `-p` and under `--mode rpc`
with `RPI_EVENT_LOG=1`.

No plugin-side workaround exists for the *content*, because
`get_session_branch` refuses a full scan: `branch_entry_query` in
`crates/rpi-cli/src/modes.rs` requires a non-empty `customTypes` array and
"never fall back to a full scan", so a plugin cannot read back ordinary message
or tool-result entries to reconcile. The plugin therefore documents that
`--print` runs record user prompts and replies only.

Related ask: if tool events stay undispatched in print mode, allow a bounded
full-branch read (the `customTypes` filter is a privacy/performance guard, and a
plugin asking for entries after its own last sequence is the same class of read).

## 4. Document the entry-renderer payload — implemented

Accepted: the SDK now states the envelope (`{"customType":"...","data":...}`) and
the accepted replies (`{"text","markdown"}` or `{"lines":[...]}`), and the TUI
passes `customType`/`data` through `extension_entry_component`. The plugin
registers `optchat_entry_renderer` for `optchat-entry` and `optchat-node`; the
text below is the original request.

`register_entry_renderer` is declared as

```rust
pub type RenderFn = extern "C" fn(input_json: StbStringRef, out: *mut StbString,
                                  user_data: *mut c_void) -> i32;
```

but neither the input envelope (`entry`, `customType`, `data`, theme/capability
flags?) nor the accepted output (`{"text":…}`? markdown? rows?) is documented,
and no package in this workspace registers one. Rendering memory rows inline
(the way OptChat shows `zoom` results in its transcript) needs that contract.

Ask: document the request and response shape with an example, and state whether
the output is a single message row or a component tree.

Workaround shipped here: `/optchat view` prints text through the command
response; no inline row rendering.

## 5. Consider a project-scoped durable store (nice to have)

Cross-session memory currently needs a file next to the project
(`.rpi/optchat/<profile>.jsonl`), following `rpi-goal`'s `.rpi/goals.json`
precedent, with session custom entries mirroring the latest state. That means
plugins invent their own atomic-write, locking, and migration rules.

Ask: a runtime action that reads and writes a plugin-namespaced document scoped
to the project (or to the agent dir), with the host owning atomicity and
concurrency. Acceptance: two `rpi` processes on one project can read and append
without the plugin implementing a lock file.

Workaround shipped here: single-writer-by-convention file store with
`tmp`+rename, bounded reads, and the session entry as the source of truth for the
active branch.
