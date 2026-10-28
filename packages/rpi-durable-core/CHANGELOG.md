# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
