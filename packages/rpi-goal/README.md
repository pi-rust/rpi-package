# rpi-goal

Codex-style goal mode for the `rpi` Rust agent.

`goal` keeps one **persistent, context-injected** objective per project. The
agent records a goal, and every turn that ends while the goal is active the
extension appends a **custom entry** (`customType: "goal"`) carrying the goal
state. Once the host registers a `CustomEntryContextMessageProjector` for the
`"goal"` type, that entry is projected into the next turn's model context, so
the goal stays visible to the model across turns without the model having to
remember to ask. The goal survives sessions (stored in `.rpi/goals.json`) and
drives autonomous progress: once started, the agent keeps advancing it until it
is completed or paused.

## Tools

- `goal` — one tool, five actions:

| action | effect |
| --- | --- |
| `start` | Begin a goal (`title`, optional `notes`). Overwrites any previous active goal. |
| `update` | Record progress (`notes`/`status`). The new status is injected next turn. |
| `pause` / `resume` | Temporarily stop / restart autonomous advancing. |
| `complete` | Mark done, archive it, and stop injecting. |
| `show` / `list` | Render the current (or all) goal(s). |

## Storage

Per project: `.rpi/goals.json`:

```json
{
  "active": { "title": "Ship the goal plugin", "status": "active", "notes": "…", "startedAt": 1750000000000, "updatedAt": 1750000000000 },
  "archived": []
}
```

The file is written atomically (tmp + rename). A goal is active by default;
`pause` sets `status: "paused"` (the extension stops injecting on `TurnEnd`).

## Context injection (clean path)

On `TurnEnd`, while a goal is active and its state changed since the last
injection, the plugin calls the host runtime action `AppendEntry` with:

```json
{
  "customType": "goal",
  "data": { "title": "…", "status": "active", "notes": "…", "startedAt": …, "updatedAt": … }
}
```

`AppendEntry` does **not** drive a new run, so there is no feedback loop. The
host (pi-rust) registers a `CustomEntryContextMessageProjector` for the `"goal"`
custom type that turns this entry into a short user-visible text message in the
next turn's context. Injection is idempotent per goal-state change to avoid
transcript bloat.
