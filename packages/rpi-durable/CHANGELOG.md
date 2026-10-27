# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/pi-rust/rpi-package/compare/rpi-durable-v0.1.0...rpi-durable-v0.2.0) - 2026-10-27

### Other

- *(durable)* [**breaking**] persist incremental checkpoints with verified recovery ([#85](https://github.com/pi-rust/rpi-package/pull/85))

### Changed

- Persist versioned incremental workflow, tool, and queue changes with periodic
  complete checkpoints instead of rewriting the complete state after every event.
- Read existing plain checkpoints when upgrading. New journals require a
  journal-aware extension; downgrading to a plain-snapshot reader is unsupported.

### Fixed

- Restore the current branch before the first tool, command, or lifecycle event
  in non-interactive modes that do not emit `SessionStart`.
- Validate native entry IDs, contiguous delta anchors, legacy envelopes, and
  restore failures before permitting further state changes.
- Preserve commit-before-publish behavior after failed or unusable append
  acknowledgements, and reconcile unknown commits through an explicit reload.
- Keep journals replayable for out-of-order tool events and summarized tool ID
  collisions by falling back to a complete bounded tool list.

### Added

- Record bounded Agent and tool lifecycle checkpoints in the native session branch.
- Mark unfinished runs and tool calls as `interrupted` after restart or shutdown.
- Add `recover` and `forget_run` actions without unsafe automatic tool replay.
- Add a bounded branch-scoped work queue with claim, finish, and cancel transitions.

## [0.1.0] - 2026-10-25

### Added

- Add the `durable` tool for session-branch workflow checkpoints.
- Restore the latest checkpoint on session start and tree navigation.
- Add the `/durable` status command and validation-focused unit tests.
