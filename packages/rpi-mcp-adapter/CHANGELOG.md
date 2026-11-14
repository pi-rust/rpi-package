# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.11](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.10...rpi-mcp-adapter-v0.2.11) - 2026-11-14

### Fixed

- *(mcp-adapter)* drop out-of-range bounds on mcp_request.id ([#265](https://github.com/pi-rust/rpi-package/pull/265))

## [0.2.10](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.9...rpi-mcp-adapter-v0.2.10) - 2026-11-13

### Fixed

- *(ci)* avoid Linux ps diagnostics in ownership tests ([#275](https://github.com/pi-rust/rpi-package/pull/275))

## [0.2.9](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.8...rpi-mcp-adapter-v0.2.9) - 2026-11-06

### Fixed

- *(release)* publish musl builds for non-voice extensions ([#222](https://github.com/pi-rust/rpi-package/pull/222))

## [0.2.8](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.7...rpi-mcp-adapter-v0.2.8) - 2026-11-06

### Other

- honor per-crate musl support matrix ([#215](https://github.com/pi-rust/rpi-package/pull/215))

## [0.2.7](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.6...rpi-mcp-adapter-v0.2.7) - 2026-11-02

### Other

- updated the following local packages: rpi-mcp-core

## [0.2.6](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.5...rpi-mcp-adapter-v0.2.6) - 2026-11-01

### Added

- *(acp)* bridge per-session stdio MCP tools through extcore

### Other

- *(mcp)* recognize unreaped Unix child exit
- *(mcp)* derive wire identity expectations from adapter version
- Merge remote-tracking branch 'origin/main' into codex/acp-stage4-mcp-foundation
- *(mcp)* extract caller-owned ABI-free runtime

## [0.2.5](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.4...rpi-mcp-adapter-v0.2.5) - 2026-10-31

### Fixed

- *(tools)* clarify guidance and complete parameter schemas ([#135](https://github.com/pi-rust/rpi-package/pull/135))

## [0.2.4](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.3...rpi-mcp-adapter-v0.2.4) - 2026-10-16

### Fixed

- *(mcp)* reconnect after a rejected resource unsubscribe

## [0.2.3](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.2...rpi-mcp-adapter-v0.2.3) - 2026-10-16

### Fixed

- *(mcp)* preserve active subscriptions on repeated subscribe

## [0.2.2](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.1...rpi-mcp-adapter-v0.2.2) - 2026-10-16

### Added

- *(mcp-adapter)* MCP resources — list/read/subscribe with updates pushed to the model

### Fixed

- *(mcp-adapter)* retry resource delivery without reviving stale content
- *(mcp-adapter)* serialize connection teardown with active operations
- *(mcp-adapter)* restore resource subscriptions after reconnect
- *(mcp)* enforce resource pagination and text bounds
- *(mcp)* discard resource reads from retired subscriptions
- *(mcp)* resolve active session at resource injection boundary
- *(mcp-adapter)* isolate resource updates per session, and close without waiting
- *(mcp-adapter)* bound `mcp_resource read`, and reject HTTP subscribe

## [0.2.1](https://github.com/pi-rust/rpi-package/compare/rpi-mcp-adapter-v0.2.0...rpi-mcp-adapter-v0.2.1) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
