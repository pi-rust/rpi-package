# rpi-orchestrator

Bounded subagent orchestration for the `rpi` Rust agent. A subagent is a child
`rpi` process; the orchestrator's fan-out and fan-in already exist in the host
(parallel tool calls, results returned to the context), so this extension only
adds the delegation primitive and, later, the DAG scheduler on top of it.

Design and staged plan: [`DESIGN.md`](DESIGN.md). How it collaborates with the
host, `rpi-goal` and `rpi-mcp-adapter`: [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Status

Three tools share one in-memory task graph, keyed by the **session** the host is
serving. The host supplies session identity with each plugin tool call, so a
different session starts with an empty plan and switching back does not
resurrect the old one; deciding whether to resume from history or start over is
left to the human. A host that predates that field (or an ephemeral session with
no id) shares a single fallback graph rather than refusing to work.
`plan`/`dispatch` results still carry the graph in `details` for inspection.

### `subagent` — one bounded delegation

Run one task in a fresh `rpi -p` subprocess and return bounded output. Unlike
`rpi-subagents`' host-managed in-memory `delegate_task`, this launches a child
process. No parent conversation is copied: include exact workspace paths, allowed
files, constraints, deliverables and acceptance checks in `task`. Both mechanisms
share workspace files; neither creates a worktree or provides a sandbox. Assign
non-overlapping write ownership for parallel work and verify results yourself.
A successful exit (including reaching the turn ceiling) is not task acceptance.

```json
{"task":"review packages/rpi-lens for bugs","role":"reviewer","maxTurns":6,"timeoutSeconds":120}
```

| param | required | default | bounds |
| --- | --- | --- | --- |
| `task` | yes | — | 1–20000 bytes, non-empty |
| `role` | no | `worker` | recorded only today |
| `provider` | no | parent binding, best effort | nonblank override passed as `--provider`; disables inheritance of both fields |
| `model` | no | parent binding, best effort | nonblank override passed as `--model`; disables inheritance of both fields |
| `maxTurns` | no | 8 | 1–32 |
| `timeoutSeconds` | no | 300 | 10–1800 |

### `plan` — author/edit the DAG

| action | params | effect |
| --- | --- | --- |
| `set` | `nodes` | replace the graph (validates ids, deps, acyclicity) |
| `update` | `addNodes` / `removeIds` / `setDeps` / `reset` | edit the graph in place |
| `show` | — | render the graph |

A node is `{id, task, deps?, role?, provider?, model?, maxTurns?, timeoutSeconds?, expectFile?, verify?}`.
`id` and `task` are required and trimmed; the trimmed task must contain at most
20,000 UTF-8 bytes (the standalone `subagent` checks the original task bytes).
Ids must be unique; dependencies must name existing nodes and be acyclic.
`deps` defaults to `[]`; dependency ids are trimmed and deduplicated. Worker
budgets have the same defaults/bounds as `subagent`; role is only metadata.
There is no node-count cap. `set` requires a nonempty `nodes` array; `addNodes`
and `setDeps` may be empty. `setDeps` items require both `id` and `deps`:

```json
{"action":"update","setDeps":[{"id":"review","deps":["implement"]}]}
```

Unlike creation, `setDeps.id` is an exact lookup and is not trimmed. Updates apply
additions, removals, dependency changes, then resets, and validate atomically.
Unknown removal ids are ignored; reset ids must exist. Resetting a failed node
does not automatically re-arm its skipped dependents: reset those explicitly too.
An omitted action means `show`; other action-specific fields are ignored.
Top-level schemas permit additional fields for host compatibility.

This `plan` is an **execution DAG**, not `plan_mode_start` / `plan_mode_complete`
(the read-only design and user approval workflow). Use plan mode to agree on a
design before implementation, then use this DAG to schedule approved work.

**Completion is written only by `dispatch`** — there is no way to hand-mark a node
`done`, so a plan cannot be "completed" by the model. `reset` re-arms a node.
While a dispatch wave is running, `set` and `update` (including `reset`) return
a busy error. `show` remains available; edit the plan after the wave finishes.

### `dispatch` — run one wave

Runs every pending node whose dependencies are all `done` **at wave start**,
in batches of up to `maxParallel` children at once (default 4, cap 16), then
reports each node's result summary and the updated graph. Newly unblocked nodes
wait for the next call. One call advances **one wave**, so the orchestrator sees the
results and can reshape the graph before the next call.

Guards (all enforced in code, not asked of the model):

- a cycle is rejected by `plan` before anything runs;
- overlapping dispatch calls return a busy error; results from a wave whose
  session or plan generation has changed are discarded without modifying the
  active graph;
- a wave with nothing ready and pending nodes left reports "no progress" instead
  of spinning;
- a failed node makes its dependents `skipped` transitively;
- **per-node acceptance**: after a child exits successfully, a node that declared
  `expectFile` must name an existing path (directories also pass; existence is
  not proof this run produced it), and one that declared `verify` must pass
  its shell command — otherwise the node is `failed`. Without this a child that
  hit its turn cap still exits 0 and would be recorded as `done`;
- **non-blocking and cancellable**: each tool's builder runs on a worker thread,
  so `poll` never blocks. Progress streams as a pending partial (per wave for
  `dispatch`), and cancelling the run kills the in-flight child **and its
  descendants**. Cancellation is checked every ~100 ms; Unix walks the tree
  because nested shells can create new process groups, and Windows owns a job
  that also covers descendants after the direct child exits. Output collection
  continues checking cancellation and the deadline;
- **bounded acceptance**: a node's `verify` command uses the same timeout setting
  and cancellation control as its child, captures (and caps) its own output
  instead of inheriting the host's stdio, and reports a timeout as a node
  failure. Verification runs outside the graph lock, so `plan show` and session
  switches remain responsive; results are checked again before updating the
  active graph. Windows commands preserve quoted paths through `cmd.exe`;
- **resumable cancels**: a wave cancelled mid-flight returns nodes that never
  started to `pending`, so a later `dispatch` picks them up instead of leaving
  phantom `running` nodes that can never be scheduled again;
- **no session litter**: children run with `--no-session`, so a wave does not
  leave a ~2 MB session file per node in the parent's `.rpi/sessions/` directory.

### Provider / model passthrough

If both `provider` and `model` are omitted or blank, the extension queries the
host's current execution binding and passes it to the child. If the host bridge,
runtime handle or binding lookup is unavailable, missing values fall back to
child CLI defaults. This is best-effort inheritance, not an authentication check.

An explicit nonblank override of **either** field disables inheritance of **both**:
a provider-only override does not retain the parent's model, and a model-only
override does not retain its provider. The child CLI resolves the omitted half.
Prefer omitting both, or supplying a known authenticated, compatible pair; do not
invent provider/model names. These rules apply to standalone `subagent` and DAG
nodes, with the parent selection read once per tool call/wave.

Budget schemas advertise the intended ranges. Runtime clamps unsigned integers
(`maxTurns` 1–32, `timeoutSeconds` 10–1800, `maxParallel` 1–16); missing or
non-unsigned values use defaults. No parsing behavior is changed by this guidance.
`expectFile` and `verify` are trimmed optional strings; paths and verification
commands resolve in the shared process cwd. Verification has a separate timeout
of the node's configured length, runs through the platform shell, and is not a
sandbox. Prefer focused checks that test the requested deliverable.

## How it stays bounded

- **Turn budget** — `maxTurns` is passed as `RPI_MAX_TURNS_PER_RUN`, which the
  harness enforces regardless of what the child model wants.
- **Wall-clock budget** — the host kills the child after `timeoutSeconds`.
- **Recursion guard** — the child is launched with `--exclude-tools subagent,plan,dispatch`,
  so these orchestration tools cannot be called recursively.
- **Output bound** — child output is truncated before it re-enters the
  orchestrator's context.

## Task transport

The task is written to the child's **stdin** rather than passed positionally:
`rpi -p` treats the next non-flag token as its prompt, so a task beginning with
`-` would otherwise be parsed as a CLI flag. Piped stdin becomes the initial
prompt (`rpi-cli::app::build_initial_message`).

## Autonomy (goal-implied guidance)

Left alone, a capable orchestrator model just does the work itself (it solved a
test goal with `bash` and never touched `plan`/`dispatch`). To make it actually
orchestrate, this extension appends a short workflow section to the system prompt
whenever `rpi-goal` has an **active goal** in the project — `/goal` implies
orchestration, so no variable is needed.

`RPI_ORCHESTRATOR_GUIDE` only *overrides* that decision: `=0/false/no/off` forces
it off even with a goal, `=1/true/yes/on` forces it on even without one.

The guidance is never injected into a subagent (children carry
`RPI_ORCHESTRATOR_SUBAGENT=1` and get no `plan`/`dispatch` anyway).

With it on, an unattended run of “four independent crate summaries plus a
dependent index” self-planned a 5-node graph, ran the four summaries in one
parallel wave, then the index node after them, and finished — no human relaying.

## Recursion guard

The child is launched with `--exclude-tools subagent,plan,dispatch`, so a
subagent cannot orchestrate again.

## Developer schema compatibility

Top-level `additionalProperties: true` accepts host-injected `__rpi` context
without exposing reserved metadata as model properties. The host supplies
`__rpi.sessionId` for graph scoping as described under Status above; model input
does not configure this context.

## Build

Built against the local `rpi-plugin-sdk` (see the workspace `Cargo.toml`).

```bash
cargo test -p rpi-orchestrator
```
