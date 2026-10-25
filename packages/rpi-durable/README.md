# rpi-durable

Durable, resumable workflow checkpoints for the `rpi` Rust agent.

`rpi-durable` stores the current workflow as a native session-branch custom
entry. The native rpi session remains the source of truth: the extension does
not create a second transcript, project database, or background worker. A
restored session or fork replays the latest `durable-state` entry for that
branch.

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
| `checkpoint` | `step`, `status`, `note?` | Update one step (`in_progress` is the default) |
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
  navigation.
- If the host lacks the required runtime action, the extension fails clearly;
  it does not silently fall back to a shared project file.

This makes the state branch-aware and restartable while preserving native rpi's
transcript ownership and event ordering. It provides safe recovery metadata but
not a background scheduler: it does not automatically execute steps, restart a
model run, or claim exactly-once tool execution after process termination.

## Build and install

```bash
cargo test --locked -p rpi-durable
cargo build --release --locked -p rpi-durable
```

Install from this workspace during development:

```bash
rpi install rpi-durable --path packages/rpi-durable
```
