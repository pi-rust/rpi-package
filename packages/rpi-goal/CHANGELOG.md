# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.11](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.10...rpi-goal-v0.1.11) - 2026-10-17

### Added

- *(release)* add package-only backfill and prebuilt distribution

### Fixed

- *(goal)* keep TUI responsive when starting work

### Other

- Merge main into feature-1014 and preserve release compatibility
- Merge main into feature-1014 and preserve ACP integration checks
- *(release)* use reusable release assets workflow

## [0.1.10](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.9...rpi-goal-v0.1.10) - 2026-10-17

### Fixed

- *(goal)* keep TUI responsive when starting work ([#26](https://github.com/pi-rust/rpi-package/pull/26))

## [0.1.9](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.8...rpi-goal-v0.1.9) - 2026-10-16

### Added

- *(rpi-goal)* say that starting a goal does not begin work

### Fixed

- *(goal)* retry failed notices at safe context boundaries
- *(goal)* capture inherited subagent role before runs
- *(rpi-goal)* don't count a subagent's turns against the parent's goal
- *(rpi-goal)* inject the goal notice at a safe boundary, not inside a tool batch

### Other

- *(rpi-goal)* cover the boundary injection and the subagent budget in isolation

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.7...rpi-goal-v0.1.8) - 2026-10-14

### Other

- update Cargo.toml dependencies

## [0.1.7](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.6...rpi-goal-v0.1.7) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
