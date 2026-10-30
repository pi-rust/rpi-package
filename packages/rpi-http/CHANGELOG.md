# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.11](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.10...rpi-http-v0.1.11) - 2026-10-30

### Added

- *(web)* show authoritative elapsed time for completed runs

### Other

- Merge pull request #119 from pi-rust/fix/web-image-rendering

## [0.1.10](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.9...rpi-http-v0.1.10) - 2026-10-30

### Fixed

- *(web)* serve project raster image artifacts securely

## [0.1.9](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.8...rpi-http-v0.1.9) - 2026-10-29

### Added

- *(web)* add project archiving

### Other

- Merge main into feature-1028 and preserve canonical durable recovery

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.7...rpi-http-v0.1.8) - 2026-10-29

### Other

- updated the following local packages: rpi-extcore

## [0.1.7](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.6...rpi-http-v0.1.7) - 2026-10-28

### Other

- updated the following local packages: rpi-extcore

## [0.1.6](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.5...rpi-http-v0.1.6) - 2026-10-28

### Other

- updated the following local packages: rpi-extcore

## [0.1.5](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.4...rpi-http-v0.1.5) - 2026-10-28

### Other

- updated the following local packages: rpi-extcore

## [0.1.4](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.3...rpi-http-v0.1.4) - 2026-10-28

### Added

- consolidate durable state and publish commit updates ([#91](https://github.com/pi-rust/rpi-package/pull/91))

## [0.1.3](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.2...rpi-http-v0.1.3) - 2026-10-26

### Added

- *(web)* refine workspace composer and tool output

## [0.1.2](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.1...rpi-http-v0.1.2) - 2026-10-25

### Added

- *(durable)* add branch-backed durable runtime

### Added

- Add `GET` and `POST /api/v1/sessions/{id}/durable` for optional durable
  state, recovery and branch-scoped queue coordination. Hosts without native
  branch support keep ordinary HTTP sessions working and return
  `durable_unavailable` for these operations.
- Add `GET /api/v1/sessions/{id}/durable/events` with revision-based replay,
  reconnect cursors and explicit resync events.

## [0.1.1](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.0...rpi-http-v0.1.1) - 2026-10-25

### Other

- updated the following local packages: rpi-extcore

## [0.1.0](https://github.com/pi-rust/rpi-package/releases/tag/rpi-http-v0.1.0) - 2026-10-24

### Fixed

- *(web)* improve session loading and workspace UI
- resolve HTTP paths and finalize host sessions reliably

### Other

- extract shared extension core and HTTP adapter
