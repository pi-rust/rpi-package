# rpi-todo

Pi-compatible session todo extension for the rpi Rust agent.

The `todo` tool manages the current conversation's todo list:

```text
todo: list | add | toggle | clear
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
{"action":"toggle","id":1}
{"action":"clear"}
```

The result details contain the complete state:

```json
{
  "todos": [
    {"id": 1, "text": "Run the tests", "done": false}
  ],
  "nextId": 2,
  "action": "add"
}
```

`/todos` shows the current session branch. The extension intentionally does
not provide project scope, tags, file migration, or an external backlog: those
are outside Pi's todo semantics.

## Build

```text
cargo build --release -p rpi-todo
```
