# rpi-goal

Goal mode for the `rpi` Rust agent. Give the agent one session-scoped objective
and it keeps working after every settled run — until the work is complete, the
goal is blocked, it is waiting for an external event, or a budget/safety limit
stops it.

```text
/goal Ship the release, then open the PR
```

Automatic continuation starts immediately and continues turn after turn. Every
managed run ends for an explicit reason, and no run is started from inside a live
turn.

## What it does

- **One objective per session branch.** State lives in a `goal-state` custom entry
  in the current session branch, not in a project file, so a goal cannot leak into
  another session or workspace.
- **Continues from the settled boundary only.** `AgentSettled` wakes a single
  process-wide worker; the worker sends at most one continuation per settled
  boundary through rpi's blocking `SendMessage` action. No continuation is
  injected into a live run.
- **Explicit stopping reasons.** `goal_complete`, `goal_blocked`, and `goal_wait`
  are separate tools, each guarded by the current goal id. Plain assistant text
  never completes a goal.
- **Stops are immediate.** `/goal pause` and every limit-driven stop cancel the
  in-flight run through the host's `Abort` action, so work ends now instead of
  after the current turn. A host without that action rejects the call with an
  explicit error, and the stop then takes effect at the next turn boundary.
- **Stale turns cannot restart work.** A continuation, block, wait, or completion
  carries the goal id it was created for. After a replacement, pause, clear,
  completion, or limit transition, the delayed call is rejected.
- **Budgets and safety limits stop work instead of guessing.** Turns, wall-clock
  time, and cumulative tokens are separate from the two continuation guards
  (automatic turn limit and repeated no-progress output).
- **Survives reload and compaction.** Usage, safety counters, elapsed active time,
  and waiting state persist across reload and compaction; the model-visible notice
  is re-appended after a compaction.

## Install

The package is a native `rpi` extension:

```text
rpi install rpi-goal
```

Goal mode can start repeated paid model turns and edit the current workspace. The
default limits exist because of that; they are not a dollar-cost cap.

## Commands

```text
/goal <objective>          start goal mode
/goal start <objective>    explicit start (--notes "...", --tokens 100k)
/goal                       show status
/goal status                show status
/goal pause                 stop automatic work after the current run
/goal resume                resume a paused, blocked, or limited goal
/goal stop                  complete and archive (aliases: done, complete)
/goal clear                 drop the goal and any legacy project file
/goal turns <N>             turn budget (0 clears)
/goal time <dur>            wall-clock budget, e.g. 30m / 2h / 1h30m (0 clears)
/goal tokens <N>            token budget, e.g. 100k / 1.5m (0 clears)
/goal limit <N> [<dur>]     set the turn and/or time budget
/goal list                  list active and archived goals
/goal settings              show the continuation limits (read-only)
/goal help                  usage
```

Words `status`, `start`, `pause`, `resume`, `stop`, `done`, `complete`, `clear`,
`list`, `help`, `turns`, `time`, `tokens`, `limit`, and `settings` are reserved as
the first token; use `/goal start <objective>` when the objective begins with one
of them. Objectives are limited to 4,000 characters; reference a file for longer
instructions.

## Tools

| Tool | Purpose |
| --- | --- |
| `goal` | `start`, `update`, `pause`, `resume`, `complete`, `limit`, `show`, `list`. Also reachable from `/goal`. |
| `goal_complete` | Requires the exact current `goal_id` and an evidence-based `summary`. Renders the accepted summary as Markdown. |
| `goal_blocked` | Requires the current `goal_id`, a reason, evidence, and `repeated_turns >= 3`. |
| `goal_wait` | Requires the current `goal_id` and a reason; keeps the goal quiet. `resume_after_ms` is a safety wake, not a poll interval (values below 10000ms are clamped). |

`goal` never spawns a nested run: a `start` made inside a live turn only arms the
goal, and the continuation starts from that turn's settled boundary. Subagents must
not manage the parent's goal, and their turns are not counted against it.

## States

| State | Meaning | Continues automatically |
| --- | --- | --- |
| `active` | Working toward the objective | yes |
| `waiting` | Quiet until an external event or a wait deadline | no |
| `paused` | Stopped by the user, an abort, or a safety limit | no |
| `blocked` | A repeated, evidenced external impasse | no |
| `usage_limited` | The provider reported a usage/quota limit | no |
| `budget_limited` | A turn, time, or token budget is exhausted | no |
| `complete` | `goal_complete` (or `/goal stop`) accepted | no |

`/goal resume` from any stopped state rotates the goal id and resets the safety
epoch; resuming a waiting goal keeps its id.

## Settings

`~/.rpi/agent/goal.json` (`RPI_CODING_AGENT_DIR` overrides the directory):

```json
{
  "continuationLimits": {
    "automaticTurns": 25,
    "noProgressTurns": 3
  }
}
```

- `automaticTurns` counts model responses in goal-owned automatic work, including
  tool loops. `null` means Unlimited and requires no confirmation — the status line
  says `Unlimited`.
- `noProgressTurns` counts repeated tool-free automatic output. Output is compared
  through a fixed-size fingerprint; the raw text is never stored. `null` disables
  the guard.
- A missing file uses these defaults without creating it. An invalid file also
  uses the defaults and reports itself as read-only in `/goal settings`, so a typo
  cannot silently disable a safety limit.
- Manual edits apply at the next session start or `/reload`. Writing the file from
  the TUI is not implemented.

Neither guard, nor the token budget, is a dollar-cost cap.

## Budgets and accounting

- **turns** — counted once per completed turn while the goal is active.
- **time** — an absolute wall-clock deadline.
- **tokens** — cumulative provider-reported usage for the current branch, taken from
  `usage.total_tokens` when present and otherwise from
  `input + output + cacheRead + cacheWrite` (reasoning is a subset of output and
  cacheWrite1h a subset of cache writes, so neither is added).
- Elapsed time accrues only while the goal is active and not waiting; paused,
  blocked, limited, waiting, shutdown, and offline time do not count.
- Budget exhaustion stops automatic work without injecting a wrap-up turn and
  without scheduling another model request. Raise the budget and `/goal resume`.
- **Usage comes from the terminal assistant message.** `MessageEnd` carries the
  serialized `AgentMessage`, so an assistant message reports `usage.totalTokens`
  (or the `input + output + cacheRead + cacheWrite` components), `stopReason`, and
  `errorMessage`. `AfterProviderResponse` is an optional second carrier on hosts
  that dispatch it; the same response is never counted twice. If a host delivers
  neither, the goal says so once (`/goal status` shows `usage: not reported by
  this host`) instead of pretending a token budget is enforced, and the turn, time,
  and continuation limits remain the effective caps. On rpi 0.24.0/0.25.0 the
  provider-hook observer is not dispatched to plugins, so `MessageEnd` is the
  carrier that actually counts.

## Write visibility

`AppendEntry` is queued while a run is active and committed by the host only after
that turn, so a branch read does not see it immediately. The extension therefore
keeps its newest local intent until a branch read confirms the same write revision;
`/goal status`, the terminal tools, and the worker all see the goal during that
window. A session switch, fork, or tree change drops the local intent so it can
never leak into a different branch.

## Non-goals for 0.3.0

- **No cross-extension workflow mutex.** rpi's plugin ABI has no plugin-to-plugin
  bus, so goal mode cannot coordinate mutual exclusion with other workflow
  extensions (for example `rpi-plan-mode`). Two workflow extensions can both be
  active; neither yields to the other.
- **No provider usage queries.** Usage-limit recovery opt-in, reset-hint lookups,
  and scheduled quota retries are not implemented: a recognized usage limit stops
  the goal and waits for `/goal resume`.
- **No managed-run RPC.** Extensions cannot start, observe, or cancel this goal.
- **No rich TUI manager or settings editor.** `/goal`, its subcommands, and the
  status line are the interface.
- **Headless modes continue too.** `rpi -p` and `--mode rpc` are not gated, so a
  goal continues until its limit or completion.

## Runtime reload

A native reload can register the same loaded library again without clearing its
statics. The plugin refreshes its API template on registration, binds command,
tool, and event callbacks to the owner supplied by the host, and retains API
snapshots for in-flight tools and the continuation worker. No API lock is held
across host calls.

Registering a candidate does not prove that the host committed the reload:
callbacks from a surviving previous registry still use their original owner.
Expired owners remain errors; they are not redirected to the latest session.

## Migration from 0.2.x

- `goal-state` entries written by 0.2.x load unchanged; missing fields default to
  the new ones. An old entry has no goal id, so the terminal tools reject it until
  the goal is restarted or resumed.
- The legacy `.rpi/goals.json` project file is no longer read as truth. `/goal
  clear` removes any leftover entry for the current working directory.
- Automatic continuation is on by default (this is the behavior change), together
  with the 25/3 safety limits.
