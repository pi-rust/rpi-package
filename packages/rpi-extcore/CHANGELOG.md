# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.1.2...rpi-extcore-v0.1.3) - 2026-10-25

### Added

- *(durable)* add branch-backed durable runtime

### Fixed

- *(extcore)* keep test helpers out of published builds
- *(durable)* require advertised branch capabilities

### Added

- Connect `DurableCoordinator` to `Service` session lifecycles, native branch
  recovery, lifecycle event observation and atomic custom-entry persistence.
- Add native RPC support for reading the selected session branch and appending
  protocol-neutral custom entries.
- Add protocol-neutral durable control-plane mutations for run recovery and
  branch-scoped queue operations.
- Add `DurableRuntime` so Service and ACP share branch recovery, event
  observation, mutation persistence and append-failure rollback.
- Publish bounded revision-based durable changes for reconnectable adapters.
- Detect optional durable branch support from the native ready capability list
  before probing branch RPCs; hosts without an explicit capability list safely
  remain ordinary non-durable sessions.

## [0.1.2](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.1.1...rpi-extcore-v0.1.2) - 2026-10-25

### Added

- Add the protocol-neutral `durable` coordinator for bounded run, tool
  recovery and branch-scoped work queue semantics.
- Prepare the crate for independent crates.io consumption with explicit package
  metadata, docs.rs configuration and public API guidance.

## [0.1.1](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.1.0...rpi-extcore-v0.1.1) - 2026-10-25

### Other

- *(snapshot)* parallelize messages and history requests

## [0.1.0](https://github.com/pi-rust/rpi-package/releases/tag/rpi-extcore-v0.1.0) - 2026-10-24

### Fixed

- *(web)* improve session loading and workspace UI
- resolve HTTP paths and finalize host sessions reliably

### Other

- extract shared extension core and HTTP adapter
