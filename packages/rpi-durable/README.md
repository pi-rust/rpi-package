# rpi-durable

Durable, resumable workflow checkpoints for the `rpi` Rust agent.

`rpi-durable` stores the current workflow as a native session-branch custom
entry. The native rpi session remains the source of truth: the extension does
not create a second transcript, project database, or background worker. A
restored session or fork replays the latest complete checkpoint and the
following deltas for that branch.

## Tool

The `durable` tool returns the complete state in `details` after every
successful mutation. The action is also available as the `/durable` command for
quick status output.

Start a workflow with explicit stable step IDs:

```json
{
  "action": "start",
  "name": "Release rpi-durable",
  "steps": [
    {"id": "implement", "title": "Implement the extension"},
    {"id": "test", "title": "Run tests"},
    {"id": "document", "title": "Update documentation"}
  ]
}
```

Then record progress:

```json
{"action":"checkpoint","step":"implement","status":"completed","note":"ABI and state tests pass"}
{"action":"checkpoint","step":"test","status":"in_progress"}
{"action":"status"}
```

| Action | Parameters | Effect |
| --- | --- | --- |
| `status` | — | Show the current workflow; also the default action |
| `start` | `name`, `steps[]` | Create or replace a workflow |
| `checkpoint` | `step`, `status?`, `note?` | Update one step (`pending`, `in_progress`, or `completed`; default `in_progress`) |
| `pause` | `reason?` | Pause a running workflow |
| `resume` | — | Resume a paused workflow |
| `complete` | — | Complete the workflow when every step is completed |
| `fail` | `reason?` | Mark a running or paused workflow as failed |
| `reset` | — | Remove the current workflow from the branch |
| `recover` | — | Convert an interrupted run into a queued recovery point; does not replay tools |
| `forget_run` | — | Remove the run journal while preserving the workflow |
| `enqueue` | `text` | Add a durable work item to the branch queue |
| `queue` | — | Show the current work item queue |
| `claim` | `item` | Atomically claim a queued item for manual execution |
| `finish` | `item`, `status`, `note?` | Mark a claimed item as completed or failed |
| `cancel` | `item`, `note?` | Cancel a queued or claimed item |

Invalid transitions are returned as structured tool details and are not
persisted. A failed host persistence call is returned as a tool error; the
in-memory state is not advanced in that case.

## Choosing and calling the tool

Use `durable` for explicit workflow steps, recovery metadata, and manually
consumed work items. Use `goal` for a single context-injected objective across
turns. Durable state belongs to the current **native session branch**, not the
cwd or a shared workspace. Independent native sessions (including fresh
`subagent` subprocesses) do not automatically share this queue or its claims.
A host-managed child created by `delegate_task` instead uses the parent's
extension tools and **shares the parent workflow, queue, and claims**. Its
separate transcript/session ID does not provide isolated durable state. From
such a child, do not `start`/`reset` the workflow without explicit authorization
from the parent. Durable does not delegate work or run background workers.

- Omit `action` to read `status`; actions are case-sensitive.
- `start` requires both `name` and a nonempty `steps` array. It replaces the
  workflow, but does not clear the run journal or queue. Each step requires
  `id` and `title`; IDs must be unique after trimming.
- `checkpoint` requires an existing step in a running workflow. `failed` is
  **not** a step status: use `fail` for the workflow, or `finish` with `failed`
  for a claimed queue item. `finish` requires both `item` and `status`.
- `claim`, `finish`, and `cancel` require the exact queue ID returned by
  `enqueue`/`queue`, not an array index. Only queued items can be claimed;
  only claimed items can be finished; queued or claimed items can be cancelled.
- Optional `note` and `reason` accept omission or `null`; a supplied string
  must be nonblank. A missing/null cancellation note becomes `cancelled`;
  a missing/null failure reason becomes `workflow marked as failed`.
- Text is trimmed before validation; limits below count Unicode scalar values
  **after trimming** (so the schema documents them rather than imposing raw
  `maxLength`). Enqueued text is limited to 4,000, and queue IDs to 128.
- The registered schema uses conditional requirements and nested step objects.
  Fields unrelated to an action are ignored by the runtime.

`reset` clears only the workflow; `forget_run` clears only the run journal.
Inspect `status` and prior tool outcomes before `recover` or retrying side
effects. Recovery creates a queued checkpoint, not a tool replay or a guarantee
that previous external side effects did not happen.

## Run recovery journal

The extension also observes native lifecycle events and records a bounded run
journal in the same `durable-state` entry:

- `AgentStart` creates a `running` run.
- Tool execution start/end records tool name, call ID, bounded argument/result
  summaries, and a terminal tool status.
- `AgentSettled` records a settled run.
- Session restart, branch navigation, or shutdown converts an unfinished run
  and unfinished tool calls to `interrupted`.
- `recover` creates a new `queued` recovery point and retains the old tool
  outcomes for inspection.

An interrupted tool is never automatically replayed. This is intentional:
tool calls can have external side effects, so retrying them requires an agent
or user to inspect the journal and decide whether the operation is idempotent.
The journal is limited to the latest 64 tool calls and summaries are limited to
1,000 Unicode scalar values. The run journal is recovery metadata, not a second
transcript, scheduler, worker queue, or exactly-once execution engine.

## Durable work queue

`enqueue` stores explicit work items in the same branch state. A consumer must
claim an item before executing it, then mark it `completed` or `failed`:

```json
{"action":"enqueue","text":"Run the integration tests"}
{"action":"claim","item":"item-1"}
{"action":"finish","item":"item-1","status":"completed","note":"All tests passed"}
```

Claiming is persisted and terminal transitions are not repeatable. The queue
holds at most 32 items, including completed and cancelled history. It is a
durable coordination primitive for an explicit consumer, not an automatic
worker, scheduler, or exactly-once delivery system.

## Limits and persistence

- At most 128 steps per workflow.
- Workflow names are limited to 500 Unicode scalar values.
- Step IDs are limited to 128 characters and must be unique.
- Step titles are limited to 1,000 characters.
- Notes and pause/failure reasons are limited to 4,000 characters.
- State is stored with `RuntimeActionId::AppendEntry` using the custom type
  `durable-state` and restored with `GetSessionBranch` on session start/tree
  navigation. Because native `--print` and `--mode json` do not dispatch
  `SessionStart`, the first durable tool, command, or lifecycle event lazily
  restores the current branch before reading or mutating it. A failed lazy
  restore puts the store in a failed state: it reports the error, exposes no
  fabricated/empty healthy state, and blocks mutations until an explicit
  successful `SessionStart` or `SessionTree` reload reconciles the branch.
- The host must support `GetSessionBranch` with
  `{"customTypes":["durable-state"]}` and explicitly return `filtered: true`
  with an `entries` array of matching native custom entries. Missing/false or
  non-boolean confirmation, rejected filters, and invalid entry envelopes fail
  restore and block mutations until a successful explicit reload. Older hosts
  must be upgraded; there is no unfiltered full-branch fallback or shared
  project-file fallback.

This makes the state branch-aware and restartable while preserving native rpi's
transcript ownership and event ordering. It provides safe recovery metadata but
not a background scheduler: it does not automatically execute steps, restart a
model run, or claim exactly-once tool execution after process termination.

## Journal format and compatibility

> **Breaking preview upgrade — no migration:** only canonical versioned
> `rpi-durable-journal` records under `durable-state` are supported. Historical
> `rpi-durable-runtime` entries are not requested, restored, or migrated, and
> plain unversioned `DurableState` snapshots are no longer restore bases. Use a
> new session/branch without unsupported durable history; there is no automatic
> migration or compatibility mode. Existing canonical journals remain readable.
> Upgrade the native host together with the plugin to support required branch
> filtering.

Writes use a versioned `rpi-durable-journal` record in the native session
branch. The first write on a branch is a complete `snapshot`; later writes are
typed `delta` records, with a complete snapshot at least every 32 meaningful
commits or whenever a delta would not be smaller. Deltas carry their base
revision and the native
`id` of the previous durable record (the append response calls this value
`entryId`). Replay validates the selected complete snapshot, its suffix
revision sequence, branch anchors, state bounds, and operation paths before
installing any state. A failed `AppendEntry` therefore leaves the in-memory
state, revision, snapshot cadence, and anchor unchanged.

Unversioned snapshots alone, or after the selected canonical snapshot, are
rejected rather than migrated. A malformed or unsupported durable payload
before the newest valid self-contained snapshot is treated as obsolete prefix
data, never as a migration source. Invalid native entry envelopes or unexpected
custom types in a filtered response are always rejected. Records after that
snapshot, or any non-empty journal without a valid self-contained snapshot,
still fail closed rather than falling back to stale/default state; a later
successful session-start/tree restore can recover the extension. A running
restored run is checkpointed as interrupted using the same commit-before-publish
ordering and is never automatically replayed.

A complete snapshot is self-contained for restore: when a branch is truncated
before the latest snapshot, that snapshot's own revision and native entry ID
become the replay anchor, and only its following deltas must have contiguous
revisions and exact predecessor IDs. The reader scans all filtered canonical
entries to locate that snapshot; bounded cursor/limit reads require native
runtime support and are a separate optimization.

Older extension builds understand only plain snapshots and cannot read the
journal deltas. Downgrading a canonical journal to those builds is unsupported;
start a separate session with the older build instead. A successful append with
an unusable acknowledgement (missing ID or invalid JSON) cannot be retried
against the old anchor; reload the branch explicitly before continuing. New
tool records may also contain an optional `callKey` used to preserve long tool
identity; older records without it remain readable.

## Developer schema compatibility

Top-level `additionalProperties: true` tolerates host-injected `__rpi` metadata
and other unknown fields without exposing reserved metadata as model properties.

## Build and install

```bash
cargo test --locked -p rpi-durable
cargo build --release --locked -p rpi-durable
```

Run the real-host filtered-query and DLL restart smoke with matching preview
artifacts (no model calls, isolated temporary profile/session):

```bash
python packages/rpi-durable/tests/native_branch_smoke.py --rpi /path/to/rpi --extension /path/to/rpi_durable.dll
```

Use the platform's `.so`/`.dylib` path on Linux/macOS. The check covers modern
queries, retired query rejection, snapshot/delta writes, and restoration after
restarting the native host. It does not install or replace the running agent.

Install from this workspace during development:

```bash
rpi install rpi-durable --path packages/rpi-durable
```
