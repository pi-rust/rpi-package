# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Model-written summary lines: `/optchat compact on|off` (or
  `RPI_OPTCHAT_COMPACT=auto`) compresses merges through an ephemeral child
  `rpi -p`, storing them as `optchat-node` entries; failures fall back to the
  extractive cut.
- Cross-session profiles: memory mirrors into `.rpi/optchat/<profile>.jsonl`,
  `/optchat profile [name]` switches profile, and `/optchat attach` copies a
  store into the current session as imported notes.
- Importers for Claude Code JSONL, ChatGPT `conversations.json`, generic
  JSONL/JSON exports and plain text, with `/optchat import --format`.
- A footer count published through `SetStatus`, and the retained id window in
  `/optchat status`.
- `/optchat recall <text>` searches the profile store, so messages that left the
  20000-message window stay findable; a `zoom` past the window points at it.
- Session-tree lifecycle handling: `SessionBeforeSwitch`, `SessionBeforeFork`,
  `SessionBeforeTree`, `SessionTree`, `SessionBeforeCompact` and `SessionCompact`
  are registered, so a host that dispatches them drops the branch cache before
  the branch changes instead of detecting it afterwards.
- `packages/rpi-optchat/tests/host.rs`: the plugin ABI driven against a fake host
  (filter contract, paging, branch reload, persist-before-commit, duplicate
  window, footer lifecycle, renderer fall-through).
- `RPI_OPTCHAT_STORE_DIR` moves the profile store out of the project directory.
- An entry renderer (`optchat`) that turns `optchat-entry` and `optchat-node`
  into one transcript row each (`OptChat <kind> · <head>`,
  `OptChat summary <l>+<i> · <head>`), leaving other types to their own renderers.
- The view shape is saved as `optchat-view`, so `/optchat fit` and model-written
  lines survive a resume; a shape that no longer tiles the window is refolded.

### Changed

- The log keeps a 20000-message window with absolute ids instead of refusing new
  messages at the cap, so a printed summary keeps naming the same message.
- Cached memory is validated against the branch's newest message, so
  `navigate_tree` inside one session reloads instead of drifting.
- The `/optchat` footer is cleared when the session ends or moves to another
  branch, instead of leaving a stale count behind.

### Fixed

- `/optchat import` and `/optchat attach` preserve the source day in the entry
  date instead of stamping imported history with the current time.
- Profile-store JSONL reads and appends take OS file locks, so two processes
  sharing a profile cannot observe a half-written line or interleave appends.
- Trimming the retained log also drops model-written summary nodes that no
  longer fit inside the retained window.

## [0.1.0]

### Added

- Native port of OptChat's memory model: a session-branch message log stored as
  `optchat-entry` custom entries, a binary summary tree with a 128 KB bounded
  view, and extractive summary lines.
- `zoom` and `search` tools that read the log through the host ABI.
- `/optchat` commands: status, view, zoom, search, note, import, fit, help.
- Automatic capture of user prompts, assistant replies, tool calls and tool
  results, with a 10-second duplicate window and a subagent guard.
- One pointer line appended to the system prompt by `register_before_agent_start`
  so the model knows the memory tools exist.
