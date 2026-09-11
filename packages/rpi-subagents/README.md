# rpi-subagents

A bounded delegation tool inspired by `pi-subagents`. It registers
`delegate_task`, which validates and emits a non-recursive delegation envelope
with explicit role, turn, and timeout limits. The current rpi ABI does not yet
expose a child-agent spawn action, so scheduling remains a host responsibility.
