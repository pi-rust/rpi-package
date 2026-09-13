# rpi-ask-user

Structured questions for workflows that need an explicit user decision.
The `ask_user` tool accepts the native Pi shape (`question`, `context`, option
objects, `allowMultiple`, `allowFreeform`, `allowComment`, and `displayMode`)
and also accepts the earlier `questions[]` alias. It returns a selector-shaped
`ui` payload so hosts can render native single/multi-select controls.
