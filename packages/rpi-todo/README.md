# rpi-todo

Pi-compatible session todo extension for the rpi Rust agent.

The `todo` tool manages the current conversation's todo list. Task IDs are
**zero-based**: the first task added is `#0`. Always call `list` first and use
the exact returned `id` for `update` or `toggle`; the ID is stable and is not
necessarily the array position after tasks are removed or a session is
restored.

```text
todo: list | add | update | toggle | clear
```

State is stored in each tool result's `details` and reconstructed by replaying
`todo` tool results on the current session branch. This matches Pi's extension
model:

- session restore replays the current branch;
- forked sessions inherit the fork point and then diverge;
- tree navigation restores the selected branch's state;
- no external todo JSON file is used.

## Tool actions

```json
{"action":"add","text":"Run the tests"}
{"action":"list","includeDone":true}
{"action":"update","id":0,"status":"in_progress"}
{"action":"update","id":0,"status":"completed"}
{"action":"toggle","id":0}
{"action":"clear"}
```

The result details contain the complete state:

```json
{
  "todos": [
    {"id": 0, "text": "Run the tests", "done": false, "status": "pending"}
  ],
  "nextId": 1,
  "action": "add"
}
```

New sessions start at ID `0`. Existing session-branch snapshots keep their
original IDs when restored, so changing this convention does not silently
rewrite history. `/todos` shows the current session branch. The extension
intentionally does not provide project scope, tags, file migration, or an
external backlog: those are outside Pi's todo semantics.

The interactive RPI UI groups successful todo updates into one task list.
Pending tasks use `[ ]`, the active task uses `[•]`, and completed tasks use
`[✓]` with dimmed, struck-through text. Each mutation returns the whole list;
older session snapshots with only `done` remain supported. This presentation
requires the matching updated RPI host; other clients receive a plain checklist.

## Build

```text
cargo build --release -p rpi-todo
```
