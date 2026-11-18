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
- The view shape is saved as `optchat-view`, so `/optchat fit` and model-written
  lines survive a resume; a shape that no longer tiles the window is refolded.

### Changed

- The log keeps a 20000-message window with absolute ids instead of refusing new
  messages at the cap, so a printed summary keeps naming the same message.
- Cached memory is validated against the branch's newest message, so
  `navigate_tree` inside one session reloads instead of drifting.

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
