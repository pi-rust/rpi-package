# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/pi-rust/rpi-package/compare/rpi-durable-v0.2.4...rpi-durable-v0.3.0) - 2026-10-29

### Other

- *(durable)* [**breaking**] require canonical filtered branch recovery ([#109](https://github.com/pi-rust/rpi-package/pull/109))

### Changed

- **Breaking preview:** require filtered `GetSessionBranch` reads using `customTypes`; reject unconfirmed host responses instead of accepting a full transcript.
- Restore only current-version `durable-state` journal snapshots/deltas. Remove legacy discriminator and unversioned state migration; upgrade the native host and plugin together.

### Tests

- Add a real native RPC/DLL smoke for filtering, retired query rejection, canonical journal commits and restart recovery without model calls.

## [0.2.4](https://github.com/pi-rust/rpi-package/compare/rpi-durable-v0.2.3...rpi-durable-v0.2.4) - 2026-10-29

### Other

- *(durable)* request filtered journal branch entries ([#107](https://github.com/pi-rust/rpi-package/pull/107))

## [0.2.3](https://github.com/pi-rust/rpi-package/compare/rpi-durable-v0.2.2...rpi-durable-v0.2.3) - 2026-10-28

### Other

- mark legacy durable reads for removal

### Deprecated

- Mark read-only compatibility for `rpi-durable-runtime` as deprecated. Existing
  sessions remain readable through the `0.2.x` durable / `0.1.x` core lines; the
  compatibility path is planned for removal in `rpi-durable` `0.3.0` and
  `rpi-durable-core` `0.2.0`. New writes use only `durable-state`.

## [0.2.2](https://github.com/pi-rust/rpi-package/compare/rpi-durable-v0.2.1...rpi-durable-v0.2.2) - 2026-10-28

### Added

- consolidate durable state and publish commit updates ([#91](https://github.com/pi-rust/rpi-package/pull/91))

### Fixed

- Serialize first-use branch restoration and mutations during branch reload so
  concurrent callbacks cannot append against a replaced branch.
- Keep restore failures published while the restore gate is held, and make
  durable status snapshots wait for branch reloads.
- Persist an explicit tool-call key namespace so encoded long IDs cannot collide
  with naturally short native IDs, and return the state committed by a mutation
  rather than a later branch snapshot.
- Use the newest valid self-contained snapshot as a replay base, ignoring
  malformed obsolete journal prefixes while still failing closed on suffix
  corruption.
- Persist a stable tool-call identity so bounded display IDs cannot associate a
  long tool result with the wrong execution, including when legacy entries are
  present; mark unrecoverable legacy truncations as unknown during restore.
- Use a cryptographic SHA-256 digest for new bounded long tool IDs while still
  recognizing the previous FNV-based IDs during migration.

## [0.2.1](https://github.com/pi-rust/rpi-package/compare/rpi-durable-v0.2.0...rpi-durable-v0.2.1) - 2026-10-27

### Fixed

- *(durable)* complete PR #85 review fixes ([#87](https://github.com/pi-rust/rpi-package/pull/87))

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
