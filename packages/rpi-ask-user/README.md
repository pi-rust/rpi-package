# rpi-ask-user

Interactive questions for workflows that need an explicit user decision.

The ABI v2 `ask_user` tool normalizes the native Pi shape (`question`,
`context`, option objects, `allowMultiple`, `allowFreeform`, `allowComment`,
`displayMode`, `timeout`, and `suggest`) as well as the older `questions[]`
alias. Each question keeps a stable `id`, optional `header`, context, and
option descriptions.

When the host exposes the rpi UI runtime action, the tool sends a unique
`requestId`/`toolCallId`, returns `Pending`, and waits for the TUI answer before
returning `Done`. Freeform questions use an input editor; questions with
options use a selector. Timeout and cancellation close the pending UI request.
The tool result contains only a readable answer summary and structured answer
details, never the transport JSON.

Hosts without an interactive UI receive the explicit error
`ask_user requires an interactive UI`. The registered `/ask_user` command is
also available for hosts that expose the native command selector directly:

```text
/ask_user {"question":"Which target?","options":["Linux","Windows"]}
```

Install the published package with:

```text
rpi install rpi-ask-user
```
