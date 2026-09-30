# rpi-goal

Codex-style goal mode for the `rpi` Rust agent.

`goal` keeps one persistent objective per project. The extension stores the
state in `.rpi/goals.json` and injects a generic custom message into the session
transcript when the goal changes. The host uses the normal message/context path;
there is no goal-specific logic in `pi-rust`.

## Tools

- `goal` — one tool, seven actions:

| action | effect |
| --- | --- |
| `start` | Begin a goal (`title`, optional `notes`). Overwrites any previous active goal. |
| `update` | Record progress (`notes`/`status`). |
| `pause` / `resume` | Temporarily stop / restart autonomous advancing. |
| `complete` | Mark done and archive it. |
| `show` / `list` | Render the current (or all) goal(s). |

## Storage

Per project: `.rpi/goals.json`:

```json
{
  "active": { "title": "Ship the goal plugin", "status": "active", "notes": "…", "startedAt": 1750000000000, "updatedAt": 1750000000000 },
  "archived": []
}
```

The file is written atomically (tmp + rename).

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
Injection is deduplicated per goal-state fingerprint to avoid repeated entries.
