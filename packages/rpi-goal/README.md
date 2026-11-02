# rpi-goal

Codex-style goal mode for the `rpi` Rust agent.

`goal` keeps one objective per session branch. The extension stores the latest
state as a `goal-state` custom entry in the current session, then injects a
generic custom message into the session transcript when the goal changes. The
host uses the normal message/context path; there is no goal-specific logic in
`pi-rust`. A slash-command start also submits the first work message
automatically in the background; a goal tool call made during an existing
turn does not nest a second run.

## Slash command

```
/goal                     show status
/goal start <title>       start a goal (--notes "..." optional)
/goal <title...>          shorthand for start
/goal pause | resume      pause / resume the active goal
/goal stop                complete and archive the goal (aliases: done, complete)
/goal turns <N>           max turns (0 clears)
/goal time <dur>          max duration, e.g. 30m / 2h / 90s / 1h30m (0 clears)
/goal limit <N> [<dur>]   set both budgets
/goal clear               clear both budgets
/goal list                list active + archived goals
/goal help                usage
```

Bare duration values are minutes (`/goal time 15` = 15 minutes). Units `s`,
`m`, `h`, `d` combine, so `/goal time 1h30m` works.

Words `status`, `start`, `pause`, `resume`, `stop`, `done`, `complete`, `turns`,
`time`, `limit`, `clear`, `list`, `help` are reserved as the first token; use
`/goal start <title>` if your title begins with one of them.

## Tool

- `goal` — one tool, eight actions:

| action | effect |
| --- | --- |
| `start` | Begin a goal (`title`, optional `notes`, `maxTurns`, `maxMinutes`). Overwrites any previous active goal. |
| `update` | Record progress (`notes`/`status`). |
| `pause` / `resume` | Temporarily stop / restart autonomous advancing. |
| `complete` | Mark done and archive it. |
| `limit` | Set `maxTurns` and/or `maxMinutes` on the active goal. |
| `show` / `list` | Render the current (or all) goal(s) in this branch, not all projects or sessions. |

Use `goal` for one objective and progress notes across turns; use `durable` for
explicit workflow steps, recovery journals, or a manually consumed work queue.
This tool does not spawn workers or start a nested agent run. State is scoped
to the current **native session branch**, not the shared cwd/project. A
subagent must not use it to manage its parent's objective; subagent `TurnEnd`
events skip goal accounting and context injection.

### Parameter contract

- Omitted `action` defaults to `show`; action names are case-insensitive, and
  `status` is a compatibility alias for `show`.
- `start` requires a nonblank `title` (at most 500 Unicode scalar values after
  trimming). It archives/replaces the old goal and resets used turns. Optional
  `notes` default to empty; budgets default to unlimited.
- `update` requires `notes` and/or `status` (`active` or `paused`, case-sensitive).
  Notes replace existing notes, not append; `""` clears them. Notes are limited
  to 20,000 Unicode scalar values after trimming. The schema documents trimmed
  limits rather than rejecting padded input with a raw `maxLength`.
- `limit` requires `maxTurns` and/or `maxMinutes`. Omitted fields preserve their
  current values. `update`, `pause`, `resume`, `complete`, and `limit` require
  an existing goal.
- Budget schemas recommend safe integers `1..4294967295`; they intentionally
  do not enumerate every input tolerated by the runtime. The current parser casts
  start budgets and limit turns to `u32`, rather than checking that range;
  larger values can wrap. Limit minutes use signed millisecond arithmetic, so
  very large values are unsafe. Runtime parsing is unchanged.
- Zero is accepted but differs by action: on `start`, zero means unlimited;
  on `limit`, zero turns becomes **1**, and zero minutes sets the deadline to
  **now**. The tool cannot clear an existing budget; use `/goal turns 0`,
  `/goal time 0`, or `/goal clear` instead.
- The schema advertises the eight canonical actions with conditional requirements.
  The runtime ignores unrelated fields; `null` is not a substitute for a required
  string or budget.

### Developer compatibility notes

Top-level `additionalProperties: true` permits host-injected `__rpi` metadata,
including nested values, without exposing a reserved model property. The schema
is recommended model input, not an exhaustive description of parser tolerance;
case-insensitive actions and the `status` alias remain runtime compatibility
behavior.

Examples:

```json
{"action":"start","title":"Ship the release","maxTurns":20,"maxMinutes":30}
{"action":"update","notes":"Implementation complete; validating"}
{"action":"limit","maxTurns":40}
{"action":"show"}
```

## Budgets

A goal can carry a budget:

- **turns** — counted once per completed agent turn while the goal is active.
- **time** — a wall-clock deadline, set as a duration from the moment you set it.

When either budget is exhausted the goal **auto-pauses** and a short notice is
injected into the context. `/goal resume` (or `goal resume`) continues; while
paused, turns are not counted. Resume does not reset `turnsUsed` or extend the
deadline: update the budget before resuming an exhausted goal. `maxTurns` is a
total count from the goal start, not a count of additional turns.

## Storage

The live extension stores state as a `goal-state` custom entry in the current
session branch. This keeps goals isolated between unrelated sessions while
allowing a resumed/forked branch to retain its own goal history. Runtime reads
query `customTypes: ["goal-state"]` and require `filtered: true`; failed or
unconfirmed reads report an error and never fall back to a shared project file.
The `.rpi/goals.json` file is used only when no runtime is installed (library tests).

The stored data has this shape:

```json
{
  "active": {
    "title": "Ship the goal plugin",
    "status": "active",
    "notes": "…",
    "startedAt": 1750000000000,
    "updatedAt": 1750000000000,
    "turnsUsed": 3,
    "maxTurns": 20,
    "deadlineMs": 1750003600000
  },
  "archived": []
}
```

When no runtime is available, the test fallback is written atomically (tmp +
rename). Fields added after the first release default when missing.

## Context injection

When the goal state changes, the extension calls the generic `AppendEntry`
runtime action with a `goal-state` custom entry. The user-facing goal notice is
queued and flushed at a safe turn/command boundary, so it cannot be inserted
between an assistant `tool_calls` message and its tool result. Failed appends
remain queued and are retried; the injection fingerprint is updated only after
a successful append.

The notice uses the generic custom-message path used by the native Pi
architecture. `pi-rust` does not know about the `goal` type or its state schema;
it simply persists and forwards the custom message to the model.

Injection is deduplicated per goal-state fingerprint (`title|status|notes`). The
live turn counter is deliberately excluded from the fingerprint so a running
budget does not re-inject every turn.
