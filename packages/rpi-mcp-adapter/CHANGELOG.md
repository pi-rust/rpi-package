# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
