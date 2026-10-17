# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.9](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.8...rpi-acp-v0.1.9) - 2026-10-17

### Added

- *(release)* add package-only backfill and prebuilt distribution
- *(release)* add cross-platform prebuilt extension distribution

### Fixed

- harden subagent lifecycle and integrations

### Other

- Merge main into feature-1014 and preserve release compatibility
- explain native Computer ACP setup and selective releases
- *(acp)* fold core back into ACP and remove WebUI integration
- Merge main into feature-1014 and preserve ACP integration checks
- *(release)* use reusable release assets workflow

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.7...rpi-acp-v0.1.8) - 2026-10-16

### Added

- Add `--bypass` to skip permission-gate approvals for ACP sessions and the standalone bridge.

## [0.1.7](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.6...rpi-acp-v0.1.7) - 2026-10-15

### Added

- *(acp)* advertise ACP terminal auth and generate the registry entry ([#14](https://github.com/pi-rust/rpi-package/pull/14))

## [0.1.6](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.5...rpi-acp-v0.1.6) - 2026-10-14

### Other

- *(acp)* consolidate ACP and use native Computer integration ([#12](https://github.com/pi-rust/rpi-package/pull/12))

## [0.1.5](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.4...rpi-acp-v0.1.5) - 2026-10-14

### Added

- *(webui)* automatically connect Open WebUI to the current project ([#8](https://github.com/pi-rust/rpi-package/pull/8))

## [0.1.4](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.3...rpi-acp-v0.1.4) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
