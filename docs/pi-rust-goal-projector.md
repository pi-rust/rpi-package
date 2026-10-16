# Goal context injection architecture

`rpi-goal` follows the native Pi custom-message path.

## Extension side

When the goal changes, `rpi-goal` stores the latest state with the generic
`AppendEntry` runtime action as a `goal-state` custom entry in the current
session branch. It separately queues a serialized `AgentMessage::Custom`
notice for context injection:

```json
{
  "message": {
    "kind": "custom",
    "role": "custom",
    "content": [{"type":"text","text":"🎯 Active goal: ..."}],
    "data": {
      "customType": "goal",
      "display": true,
      "details": {"status":"active"}
    },
    "timestamp": 0
  }
}
```

## Host side

The host treats the state entry as session metadata and the notice as an
ordinary message entry. The generic session context builder already forwards
`AgentMessage::Custom` messages through the normal custom-message conversion
path. No host code knows about `goal`, its status schema, or its rendering text.

State changes made by the goal tool are flushed at `TurnEnd`, after the tool
result has been appended. Slash-command changes are flushed when the command
handler returns. This avoids inserting an entry between an assistant
`tool_calls` message and its tool result. Failed notice appends stay pending and
are retried at a later safe boundary.

`custom` state entries remain state-only by default. Extensions that need model
context should use a custom message, matching native Pi's distinction between
`CustomEntry` and `CustomMessageEntry`.

## Guarantees

- `rpi-cli` has no goal-specific projector.
- `rpi-goal` owns goal formatting and lifecycle semantics.
- `/goal start` submits a first work message; the `goal` tool never starts a
  nested run while another turn is active.
- Notice injection is deferred to safe boundaries and only marked delivered
  after a successful append.
- The plugin fingerprint prevents duplicate messages for the same goal state.
