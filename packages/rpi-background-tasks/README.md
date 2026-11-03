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

Job ids are monotonically increasing within the host process and start at `0`.
The counter is process-wide, so a session that starts its first job after
another session already did will not see id `0` — ids are unique, not dense.

## Session ownership

A job belongs to the session that started it, for its whole life. The host
injects `__rpi.sessionId` into every plugin tool call, and that value is
recorded when the job is created and never revised.

- `jobs` list/tail/kill only see the caller's own jobs.
- A forked or cloned session is a **different** session: it inherits the
  conversation, not the background work, and cannot see or stop the jobs of the
  session it was derived from.
- The durable `background-tasks` snapshot is written per session and lists only
  that session's jobs, so one session's transcript never contains another's
  commands or pids.
- A host that does not supply a session id cannot use these tools at all: the
  call fails rather than starting a job nobody owns.

  This deliberately departs from the SDK's advice to "prefer a project-scoped
  fallback when `sessionId` is missing rather than refusing to work"
  (`rpi-plugin-sdk/src/lib.rs`). A project-scoped fallback would make every
  session in the same directory share jobs, which is the opposite of the
  guarantee this plugin makes. Refusing is the honest outcome: a job with no
  owner is a job nobody can be held responsible for.

A job whose id belongs to another session reports
`job <id> belongs to another session` rather than "not found", so a mistyped id
and a refusal are distinguishable.

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

**Known limitation — where a snapshot lands.** `AppendEntry` writes to the
session the host is currently serving, and the SDK exposes no way to address a
different one. The snapshot's *contents* are always correct (only the owning
session's jobs are listed), but if the host switches sessions in-process between
a job's state change and the append, that session's snapshot is written into
the newly active session's transcript. A session's history can therefore
contain another session's job records.

Fixing this needs a host-side change — an append action that can address a
session — and is tracked separately. It does not weaken the ownership
guarantee: one session can never *see or control* another's jobs through the
tools, only observe stray records in its transcript.

## Lifecycle

- **Cancelling the tool call** does not kill a started job. `background` returns
  as soon as the process is spawned; the job continues and stays visible through
  `jobs`. Use `action: "kill"` to stop it.
- **Session shutdown** (`SessionShutdown`) marks that session's still-running
  jobs as `interrupted` and appends one final snapshot per affected session, so
  a supervisor never sees a finished session that looks permanently busy. Jobs
  belonging to another session are left alone: the host can serve several
  sessions from one process, and closing one must not stop the others' work.
- **Extension reload** (`/reload`) also dispatches `SessionShutdown`, because
  the host unloads the extension modules before loading new ones. That reload
  is in-process — the session and its history continue — but the jobs started
  under this extension are terminated with it, because the plugin instance that
  owned their process trees is being replaced.
- **Output capture** is bounded by both lines (500) and bytes (256 KiB) per job.
  Older output is dropped and `tail` says so, so a chatty server cannot grow the
  process table without limit. Both stdout and stderr are captured; stderr lines
  are prefixed with `[stderr] `.

## Platform behaviour

A job runs under **the same shell the `bash` tool uses**, so a command that
works there works here:

- macOS / Linux: `/bin/bash`, then `which bash`, then `/bin/sh`.
- Windows: `%ProgramFiles%\Git\bin\bash.exe`, then
  `%ProgramFiles(x86)%\Git\bin\bash.exe`, then `where bash.exe`.

There is **no `cmd.exe` fallback**. If no bash can be found, the call fails
with an explicit error rather than running the command under a different
shell — under `cmd`, `ls -la` fails while the same command succeeds through
the `bash` tool, and the model has no way to know which shell it is writing
for.

The probe mirrors the host's own (`rpi-tools::os_env`) because the plugin ABI
exposes no shell discovery. It is a copy, and will drift if the host changes
its probe; replace it with the SDK's resolver if one is ever exposed.

### Process-tree ownership

A job's process tree is owned through the SDK's `ProcessTree`, which is a
Windows **Job Object** and a Unix **process group**. The tree outlives the
shell that was spawned: a dev server started as `npm run dev` keeps running
after its `bash -c` wrapper exits, and `jobs kill` is what reaches it.

`taskkill /T` is not equivalent — it walks parent pids, and a git-bash child is
not on that chain, so it survives a kill that reported success. The Job Object
is inherited by every descendant, so nothing escapes it.

### Session identity

The host injects `{"__rpi": {"sessionId": …}}` into every plugin tool call.
That value is recorded when the job is created and never revised.

`SessionShutdown` carries no payload — the host puts it in the "no payload"
group — so the only identity channel at that point is the process-wide
`RPI_SESSION_ID` environment variable, which the host rewrites whenever a
session becomes active. That is a weaker channel than the per-call snapshot
(any code in the process can write an environment variable), but it is the only
one available to an event, and it is correct for the decision it drives:
shutdown stops the jobs of the session that is closing, not another's.

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
