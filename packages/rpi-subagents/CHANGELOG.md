# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.7](https://github.com/pi-rust/rpi-package/compare/rpi-subagents-v0.1.6...rpi-subagents-v0.1.7) - 2026-10-17

### Added

- *(release)* add package-only backfill and prebuilt distribution

### Fixed

- harden subagent lifecycle and integrations

### Other

- Merge main into feature-1014 and preserve release compatibility
- Merge main into feature-1014 and preserve ACP integration checks
- *(release)* use reusable release assets workflow

### Added

- Run `delegate_task` through the host-managed `ChildAgent` lifecycle with
  streaming progress, independent ephemeral sessions, current provider/model
  binding and project cwd/tool inheritance.
- Reuse the parent's permission hook and interactive UI directly; retain
  explicit headless denial instead of bypassing permissions.
- Bound streamed/final output, partial events, concurrent tasks, turns and
  timeout; clean up on parent end/cancellation, session switch and plugin reload.
- Join plugin workers before tool destruction can release the DLL keepalive.
- Verify cancelled invocations can reclaim children without starting new work,
  repeated cancellation reuses capacity, and dropped harnesses release their
  provider/plugin bindings. Host driver fixes deliver live progress and retain
  background provider/approval capacity until foreign work actually exits.

### Changed

- Require a host supporting ChildAgent (action 24). Remove the old subprocess
  RPC implementation and `RPI_SUBAGENT_BIN`; unsupported hosts now receive an
  explicit diagnostic instead of a default-config fallback.

### Fixed

- Bind workers to the current tool invocation's opaque handle/capability rather
  than a DLL-global registration pointer, keeping repeated registrations and
  failed-reload rollback isolated. Reject delegation from child aliases.

## [0.1.6](https://github.com/pi-rust/rpi-package/compare/rpi-subagents-v0.1.5...rpi-subagents-v0.1.6) - 2026-10-14

### Other

- update Cargo.toml dependencies

## [0.1.5](https://github.com/pi-rust/rpi-package/compare/rpi-subagents-v0.1.4...rpi-subagents-v0.1.5) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
