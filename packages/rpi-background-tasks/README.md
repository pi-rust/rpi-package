# rpi-background-tasks

Background shell jobs for the rpi Rust agent, plus a `jobs` inspector.

`rpi`'s `bash` tool is synchronous and has a default timeout, so dev servers,
watchers, large builds, and full test suites cannot be run there. This extension
adds the two tools such work needs, matching the tool surface of the
`@nklisch/pi-background-tasks` plugin from the `pi` ecosystem:

| Tool | Arguments | Behaviour |
| --- | --- | --- |
| `background` | `{ command, label? }` | Starts the command and **returns immediately** with a job id. Never waits for the process. |
| `jobs` | `{ action: "list" \| "tail" \| "kill", jobId?, lines?, includeDone? }` | `list` all jobs and statuses, `tail` the recent output of one, `kill` a running one. |

```json
{"command": "npm run dev", "label": "dev server"}
{"action": "list"}
{"action": "tail", "jobId": 0, "lines": 40}
{"action": "kill", "jobId": 0}
```

`background` returns the job id, the spawned pid, and a line pointing at the
`jobs` tool, because the process outlives the tool call:

```text
Started background job 0 (pid 90788) — label: dev server
Command: npm run dev
This call returned immediately; the process is still running.
Use the `jobs` tool with action=tail, jobId=0 to read its output; action=list for all jobs; action=kill to stop it.
```

Job ids are per-session, monotonically increasing, and start at `0`.

## Status vocabulary

Every job is always exactly one of:

| Status | Meaning |
| --- | --- |
| `running` | Started, has not exited. |
| `completed` | Exited with status `0`. |
| `failed` | Exited with a nonzero status, or the status was unreadable. |
| `cancelled` | Stopped by `jobs` action `kill`. |
| `interrupted` | Still running when the session shut down. |

`completed` / `failed` are recorded by a watcher thread, so status is correct
without calling a tool again. `cancel_requested` wins over the final exit status:
a killed job settles as `cancelled`, never `failed`.

## Session snapshot contract

Every status change appends **one complete snapshot** as a custom session entry:

```json
{"customType":"background-tasks","data":{"jobs":[
  {"id":"0","kind":"bash","label":"probe","command":"sleep 30","status":"running",
   "pid":90788,"startedAt":1793338545074}
]}}
```

Properties of this contract:

- `customType` is exactly `background-tasks` — **not** the crate name. External
  supervisors that understand the `pi` ecosystem key on this string.
- The snapshot is **self-contained**: it always lists every job of the session,
  so the last snapshot is the whole truth. No deltas, no replay needed.
- `status` is always present. `id` / `kind` / `label` / `command` / `pid` /
  `exitCode` / `startedAt` / `finishedAt` are for humans reading the JSONL.
- Writes go through the host's `AppendEntry` runtime action. A failed append is
  reported but never kills a process that is already running.

This lets a supervising UI answer "does this session still have work running?"
by reading only the latest snapshot.

## Lifecycle

- **Cancelling the tool call** does not kill a started job. `background` returns
  as soon as the process is spawned; the job continues and stays visible through
  `jobs`. Use `action: "kill"` to stop it.
- **Session shutdown** (`SessionShutdown`) marks every still-running job as
  `interrupted` and appends one final snapshot, so a supervisor never sees a
  finished session that looks permanently busy.
- **Output capture** is bounded by both lines (500) and bytes (256 KiB) per job.
  Older output is dropped and `tail` says so, so a chatty server cannot grow the
  process table without limit. Both stdout and stderr are captured; stderr lines
  are prefixed with `[stderr] `.

## Platform behaviour

- macOS / Linux: `sh -c <command>`
- Windows: `cmd /C <command>` with `CREATE_NO_WINDOW` (no console flash) and
  `CREATE_NEW_PROCESS_GROUP`.

## Not provided

- `monitor` (poll until a condition holds) — intentionally omitted; see below.
- Output retention after the process exits beyond the bounded buffer: there is
  no on-disk log file, so `tail` shows what fits in memory.
- Cross-session job visibility: the job table is per runtime instance, and the
  snapshot is per session branch.

## Build

```text
cargo build --release -p rpi-background-tasks
```

## Test

```text
cargo test -p rpi-background-tasks
```

The tests spawn real short-lived processes and a real `sleep`, so they are kept
in a single serial test: the process table and snapshot gate are process-wide
statics shared across cases.
