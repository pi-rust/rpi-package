# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
