# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.13](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.12...rpi-extcore-v0.2.13) - 2026-11-08

### Other

- Fix native saved-read request and resource tests
- Remove legacy durable runtime from package adapters

## [0.2.12](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.11...rpi-extcore-v0.2.12) - 2026-11-07

### Added

- *(web)* release idle session hosts

## [0.2.11](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.10...rpi-extcore-v0.2.11) - 2026-11-05

### Added

- *(web)* edit and delete provider configurations
- *(web)* add provider model discovery and connection test
- *(web)* add provider configuration form
- *(web)* add paged history, file previews and execution traces

### Fixed

- *(test)* tolerate Windows process startup jitter
- *(acp)* sync inline image prompt limits
- *(extcore)* gate Windows-only test imports

### Other

- sync dev web branch with main
- Merge remote-tracking branch 'origin/main' into dev/web-hmr
- *(web)* make branding checks portable across clean checkouts

## [0.2.10](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.9...rpi-extcore-v0.2.10) - 2026-11-05

### Fixed

- *(extcore)* prove live children with native branch identity

## [0.2.9](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.8...rpi-extcore-v0.2.9) - 2026-11-04

### Added

- *(adapters)* expose bounded Code Mode child queries and live feed
- *(extcore)* reconcile bounded child snapshots with native commit replay
- *(extcore)* add capability-gated native ToolProgram child readers

### Fixed

- *(catalog)* reject duplicate headers and page test scans
- *(extcore)* reprove every selected child before live delivery

### Other

- tolerate Windows process cleanup latency

## [0.2.8](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.7...rpi-extcore-v0.2.8) - 2026-11-03

### Fixed

- *(web)* retire archived sessions and permanently delete native history ([#183](https://github.com/pi-rust/rpi-package/pull/183))

## [0.2.7](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.6...rpi-extcore-v0.2.7) - 2026-11-02

### Added

- *(extcore)* own exact session resource lifecycle and recovery

### Fixed

- *(extcore)* retain resource authority across fork and recovery failures
- *(extcore)* inspect safe legacy package names

### Other

- *(extcore)* synchronize fork capacity admission deterministically

## [0.2.6](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.5...rpi-extcore-v0.2.6) - 2026-11-01

### Added

- *(web)* add paged history, file previews and execution traces
- *(acp)* bridge native message IDs, inline images and client file IO
- *(acp)* report native context usage at session and run boundaries
- *(acp)* synchronize live native model and thinking configuration
- *(acp)* discover saved native sessions with bounded pagination
- *(acp)* bridge session-scoped Streamable HTTP MCP tools
- *(acp)* bridge per-session stdio MCP tools through extcore

### Fixed

- *(extcore)* await catalog worker exit before returning page results

### Other

- Merge remote-tracking branch 'origin/main' into feat/web-workspace-history-traces
- *(web)* make branding checks portable across clean checkouts
- *(extcore)* validate raw filename recognition without APFS byte names

## [0.2.5](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.4...rpi-extcore-v0.2.5) - 2026-10-31

### Added

- *(acp)* route compact through the shared native run lifecycle

## [0.2.4](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.3...rpi-extcore-v0.2.4) - 2026-10-31

### Fixed

- *(acp)* bound active-branch history replay preparation

## [0.2.3](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.2...rpi-extcore-v0.2.3) - 2026-10-30

### Added

- *(web)* show authoritative elapsed time for completed runs

### Other

- Merge pull request #119 from pi-rust/fix/web-image-rendering

## [0.2.2](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.1...rpi-extcore-v0.2.2) - 2026-10-30

### Fixed

- *(web)* serve project raster image artifacts securely
- *(rpc)* adapt server and shared projections to compact native deltas

### Other

- Merge pull request #116 from pi-rust/fix/rpc-compact-deltas

### Fixed

- Preserve native compact-delta streaming for snapshot-oriented Web consumers through a shared live message projection. Raw native frames stay compact, initial content is retained, late tool identities and complete argument JSON update without guessing, and structural/final snapshots remain authoritative. Text/thinking reconciliation no longer discards nonempty initial content.

## [0.2.1](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.2.0...rpi-extcore-v0.2.1) - 2026-10-29

### Added

- *(acp)* harden protocol lifecycle and capability coverage
- *(durable)* refresh branch state incrementally
- *(web)* add project archiving

### Other

- Merge main into feature-1028 and preserve canonical durable recovery
- Merge remote-tracking branch 'origin/feat/project-archive' into HEAD
- *(durable)* verify branch cursor advances

## [0.2.0](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.1.8...rpi-extcore-v0.2.0) - 2026-10-29

### Other

- *(durable)* [**breaking**] require canonical filtered branch recovery ([#109](https://github.com/pi-rust/rpi-package/pull/109))

### Changed

- Query native branches once with a non-empty `customTypes` array and require `filtered: true`; remove full-branch/singular APIs, per-type merging and old-host fallback.
- Project only canonical durable journals and propagate advertised-capability query/replay failures instead of silently disabling durable state. Sessions without durable capabilities remain supported; HTTP, ACP and Web wire contracts are unchanged.

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.1.7...rpi-extcore-v0.1.8) - 2026-10-28

### Fixed

- *(durable)* replay journal deltas and legacy bases

### Other

- test durable legacy read compatibility
- preserve legacy durable reads during refresh
- mark legacy durable reads for removal
- Revert "remove legacy durable entry discriminator"
- remove legacy durable entry discriminator
- simplify durable replay to canonical branch reads

## [0.1.7](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.1.6...rpi-extcore-v0.1.7) - 2026-10-28

### Other

- Merge pull request #99 from pi-rust/feature-1028
- *(durable)* remove background polling

## [0.1.6](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.1.5...rpi-extcore-v0.1.6) - 2026-10-28

### Other

- *(transcript)* reduce session snapshot and branch polling cost

## [0.1.5](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.1.4...rpi-extcore-v0.1.5) - 2026-10-28

### Added

- consolidate durable state and publish commit updates ([#91](https://github.com/pi-rust/rpi-package/pull/91))

## [0.1.4](https://github.com/pi-rust/rpi-package/compare/rpi-extcore-v0.1.3...rpi-extcore-v0.1.4) - 2026-10-26

### Added

- *(web)* refine workspace composer and tool output

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
