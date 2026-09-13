# rpi-todo

Persistent project todo management inspired by Pi todo extensions. The
`todo` tool supports `add`, `list`, `done`, `remove`, and `clear`, storing data
in `<cwd>/.rpi/todo.json` (or the current directory when `cwd` is omitted).

Version 0.1.3 returns check-mode display data (`[x]` / `[ ]`) and supports
the `check` and `toggle` actions. It reads legacy JSONL and concatenated JSON
stores, avoiding trailing-characters parse errors after older or interrupted
writes.

Build with `cargo build --release -p rpi-todo` and load the resulting cdylib
through `rpi --extensions-dir`.
