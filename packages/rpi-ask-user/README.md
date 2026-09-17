# rpi-ask-user

Structured questions for workflows that need an explicit user decision.
The `ask_user` tool accepts the native Pi shape (`question`, `context`, option
objects, `allowMultiple`, `allowFreeform`, `allowComment`, and `displayMode`)
and also accepts the earlier `questions[]` alias. Its ABI v2 result follows the
rpi `AgentToolResult` envelope: readable text is returned in `content`, while
the native selector payload is preserved in `details.ui`.

In the current rpi TUI, tool results are rendered as transcript text and
`details.ui` is metadata; it does not open an interactive selector by itself.
For an interactive selector, use the registered `/ask_user` command. The
command returns rpi's native `{"kind":"selector","items":[...]}` response
and the TUI opens the selector. This split avoids showing the transport JSON
when the tool is called by a headless host.

Example in the TUI:

```text
/ask_user {"question":"Which target?","options":["Linux","Windows"]}
```
