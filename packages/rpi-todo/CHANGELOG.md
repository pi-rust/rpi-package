# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/pi-rust/rpi-package/compare/rpi-todo-v0.2.4...rpi-todo-v0.3.0) - 2026-10-29

### Other

- *(durable)* [**breaking**] require canonical filtered branch recovery ([#109](https://github.com/pi-rust/rpi-package/pull/109))

### Fixed

- Persist todo mutations directly before returning success, requiring a valid
  append entry ID instead of relying on the native host's unused `ToolResult`
  event. Listing, unchanged mutations, and invalid inputs no longer write snapshots.
- Serialize mutation commits and session recovery without holding the state lock
  across host calls. Restore lazily on first use when no session-start event is
  sent, preserve state and IDs after rejected appends, and block implicit retries
  after recovery errors or unknown append acknowledgements until session reload.
- Refresh from the current branch's latest self-contained todo snapshot on every
  unblocked tool/command use (`newestFirst`, `limit: 1`), so navigation without a
  `SessionTree` event cannot copy stale tasks into another branch. Reject
  unexpected filtered response shapes instead of treating them as empty state.
- Preserve detailed `/todos` recovery errors as visible message results rather
  than returning a nonzero callback status that hides them behind commandfailed.

## [0.2.4](https://github.com/pi-rust/rpi-package/compare/rpi-todo-v0.2.3...rpi-todo-v0.2.4) - 2026-10-17

### Fixed

- *(goal)* keep TUI responsive when starting work ([#26](https://github.com/pi-rust/rpi-package/pull/26))

## [0.2.3](https://github.com/pi-rust/rpi-package/compare/rpi-todo-v0.2.2...rpi-todo-v0.2.3) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
