# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/pi-rust/rpi-package/compare/rpi-durable-core-v0.1.1...rpi-durable-core-v0.2.0) - 2026-10-29

### Other

- *(durable)* [**breaking**] require canonical filtered branch recovery ([#109](https://github.com/pi-rust/rpi-package/pull/109))

### Changed

- Accept only canonical `durable-state` entries in the current journal format and exact version; remove legacy/raw-state decoding, lossy `from_branch`, and the raw-state `entry()` helper.
- Replay from the newest valid self-contained snapshot, fail closed on malformed suffixes, and validate contiguous delta revisions, native entry anchors, typed mutation paths and bounded state using the plugin's current persisted schema.
- Reject malformed filtered native envelopes (type, discriminator, non-empty string id and data presence) even before the selected snapshot; only obsolete payload errors may be ignored. Preserve existing public state field defaults.

## [0.1.1](https://github.com/pi-rust/rpi-package/compare/rpi-durable-core-v0.1.0...rpi-durable-core-v0.1.1) - 2026-10-28

### Fixed

- *(durable)* replay journal deltas and legacy bases

### Other

- mark legacy durable reads for removal
- Revert "remove legacy durable entry discriminator"
- remove legacy durable entry discriminator
- simplify durable replay to canonical branch reads

### Deprecated

- Mark `rpi-durable-runtime` read compatibility as deprecated. It remains
  supported through the `0.1.x` line and is planned for removal in `0.2.0`.
  New writes use only `durable-state`.

## [0.1.0](https://github.com/pi-rust/rpi-package/releases/tag/rpi-durable-core-v0.1.0) - 2026-10-28

### Added

- consolidate durable state and publish commit updates ([#91](https://github.com/pi-rust/rpi-package/pull/91))
