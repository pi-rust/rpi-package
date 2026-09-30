# Goal context injection architecture

`rpi-goal` follows the native Pi custom-message path.

## Extension side

When the goal changes, `rpi-goal` calls the generic `AppendEntry` runtime action
with a serialized `AgentMessage::Custom` payload:

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

The host treats this as an ordinary message entry. The generic session context
builder already forwards `AgentMessage::Custom` messages through the normal
custom-message conversion path. No host code knows about `goal`, its status
schema, or its rendering text.

`custom` state entries remain state-only by default. Extensions that need model
context should use a custom message, matching native Pi's distinction between
`CustomEntry` and `CustomMessageEntry`.

## Guarantees

- `rpi-cli` has no goal-specific projector.
- `rpi-goal` owns goal formatting and lifecycle semantics.
- `AppendEntry` does not start a new run, so there is no feedback loop.
- The plugin fingerprint prevents duplicate messages for the same goal state.
