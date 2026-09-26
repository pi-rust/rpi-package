# rpi-todo

Session-scoped todo lists for the rpi Rust agent.

The `todo` tool supports `add`, `list`, `done` (aliases `complete` / `check` /
`toggle`), `remove` and `clear`. `add` takes optional `tags`; `list` takes
`includeDone` to expand completed items.

## Two lists, not one

A todo list is a *plan*, and a plan belongs to the conversation that made it.
0.1.x kept one list per project, so a new session opened on the previous
session's abandoned items and the agent planned against work that was not its
own. 0.2.0 separates them:

| `scope` | File | What it is |
| --- | --- | --- |
| `session` (default) | `<cwd>/.rpi/todos/<sessionId>.json` | This conversation's plan |
| `project` | `<cwd>/.rpi/todos/project.json` | The project's durable backlog |

Ids are per list and restart at 1, so `#3` in a session is not `#3` in the
backlog. Every reply names the list it acted on (`details.scope`).

The plugin does not ask the model for either path or id: the host injects them
into every call's arguments under the reserved `__rpi` key —

```json
{ "action": "add", "text": "ship it",
  "__rpi": { "cwd": "D:\\Projects\\pi-rust", "sessionId": "01adb026-…" } }
```

— which is why dropping the model-facing `cwd` parameter is safe. When there is
no session id (an ephemeral session, or an rpi older than the injection), the
session scope falls back to the project list and says so in the reply rather
than inventing a shared pseudo-session.

## Responses

Every action returns a confirmation plus the current list, as a task list the
TUI renders with checkboxes (one line per item, instead of a bordered row whose
widest column is always the task text). Completed items are folded into a count
unless `includeDone=true`:

```
✓ Added #2 ship the release

📋 2 pending · 1 done · ██████░░░░ 33%

- [ ] #2 ship the release · `release`
- [ ] #3 write the notes

✓ 1 completed — `includeDone=true` lists them
```

The reply carries `details.markdown = true`, which the rpi TUI uses to render
the body with its Markdown component (`- [ ]` → `☐`, tags as inline code)
instead of printing the source literally. Hosts that ignore the flag still get
readable text.

## Store integrity

- **Atomic writes.** `save` writes a complete document to a unique temp file
  and renames it into place (`MOVEFILE_REPLACE_EXISTING` on Windows), so a
  reader sees either the old document or the new one — never a mixture or a
  stale tail.
- **Legacy formats.** `load` reads the current JSON array plus older JSONL and
  concatenated-JSON documents, so an interrupted older write does not make the
  store unusable. The pre-0.2 single store (`.rpi/todo.json`) is adopted as the
  project list the first time that scope is read, and the next mutation writes
  it to `todos/project.json`; the old file is left on disk untouched.
- **Concurrent calls.** The read-modify-write is serialized in-process, so two
  tool calls in one turn cannot lose each other's updates or reuse an id.
  Cross-process writers (two rpi sessions on one project) are protected against
  corruption by the atomic rename but can still overwrite each other — a
  known limitation.
- **Re-adding.** Adding a task that is already pending returns the existing
  entry instead of tracking it twice; a completed task may be added again.
  Plan-list markers (`1.`, `- `, `•`) are stripped from incoming text.
- **Containment.** A session id arrives as JSON over the plugin boundary, so
  path separators are stripped before it becomes a filename: a malformed id
  cannot write outside `.rpi/todos/`.

## Build

Build with `cargo build --release -p rpi-todo` and load the resulting cdylib
through `rpi --extensions-dir`.
