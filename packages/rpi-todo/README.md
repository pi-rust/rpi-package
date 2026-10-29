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

Successful mutations append a `todo-state` custom entry before publishing the
new in-memory state or returning success. The append must acknowledge a nonempty
`entryId`; persistence does not depend on tool-completion events. Each successful
tool result still includes the full state in `details`. Listing, unchanged
updates, empty clears, and invalid inputs do not append entries.

Session restore queries `customTypes: ["todo-state"]`, `order: "newestFirst"`,
`limit: 1` and requires `filtered: true` with at most one valid todo entry; it
never reads a full transcript. Snapshots are self-contained, so every unblocked
tool or `/todos` command reads just the latest snapshot on the current branch.
This also covers hosts that send neither `SessionStart` on first use nor
`SessionTree` on navigation; cached state is never used to infer the branch.
A failed restore (including an unexpected response entry) blocks reads and
mutations until a successful explicit session reload (`SessionStart` or
`SessionTree`), even on first use. A rejected append leaves the previous state
and next ID unchanged, and that committed state remains listable. An
unreadable or missing append acknowledgement may mean the host already wrote the
snapshot: both reads and mutations return an explicit error until session reload,
rather than implicitly rereading or retrying from stale state. `/todos` reports
these errors as visible message results with an `error` field and `Error:` text,
not a nonzero callback status that would discard the detailed error in the host.

Tool-result-only sessions from earlier releases cannot be restored through this
custom-entry-only API. This matches Pi's extension model:

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

## Native regression test

With matching preview host and todo DLL artifacts:

```text
python packages/rpi-todo/tests/native_todo_smoke.py --rpi /path/to/rpi --extension /path/to/rpi_todo.dll
```

Use the platform's `.so`/`.dylib` path on Linux/macOS. The test uses an isolated
profile/session and a deterministic loopback model to execute the real tool,
then verifies snapshots, no-op reads, restart recovery, branch navigation, IDs,
statuses and clearing. It makes no external model calls and does not replace
installed plugins.
