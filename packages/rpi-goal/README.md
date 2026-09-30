# rpi-goal

Codex-style goal mode for the `rpi` Rust agent.

`goal` keeps one persistent objective per project. The extension stores the
state in `.rpi/goals.json` and injects a generic custom message into the session
transcript when the goal changes. The host uses the normal message/context path;
there is no goal-specific logic in `pi-rust`.

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
| `show` / `list` | Render the current (or all) goal(s). |

## Budgets

A goal can carry a budget:

- **turns** — counted once per completed agent turn while the goal is active.
- **time** — a wall-clock deadline, set as a duration from the moment you set it.

When either budget is exhausted the goal **auto-pauses** and a short notice is
injected into the context. `/goal resume` (or `goal resume`) continues; while
paused, turns are not counted.

## Storage

Per project: `.rpi/goals.json`:

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

The file is written atomically (tmp + rename). Fields added after the first
release (`turnsUsed` / `maxTurns` / `deadlineMs`) default when missing, so older
files keep loading.

## Context injection

When the goal state changes, the extension calls the generic `AppendEntry`
runtime action with an `AgentMessage::Custom` payload:

```json
{
  "message": {
    "kind": "custom",
    "role": "custom",
    "content": [{ "type": "text", "text": "🎯 Active goal: …" }],
    "data": {
      "customType": "goal",
      "display": true,
      "details": { "status": "active" }
    },
    "timestamp": 1750000000000
  }
}
```

This is the same generic custom-message path used by the native Pi
architecture. `pi-rust` does not know about the `goal` type or its state
schema; it simply persists and forwards the custom message to the model.

Injection is deduplicated per goal-state fingerprint (`title|status|notes`). The
live turn counter is deliberately excluded from the fingerprint so a running
budget does not re-inject every turn.
