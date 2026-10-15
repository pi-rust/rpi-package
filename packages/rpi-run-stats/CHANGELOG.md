# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Count actual tool executions in a fixed-width Tools column.

### Changed

- Rename Steps to LLM Calls and show cumulative USD cost beside the counters.
- Document model pricing configuration and USD calculation by the host.

### Fixed

- Hide USD for models without pricing instead of displaying default zero costs.
- Keep footer metric columns stable as digits, units and missing values change.
- Avoid repeating unchanged show/hide actions on every monitor refresh.

## [0.1.2](https://github.com/pi-rust/rpi-package/compare/rpi-run-stats-v0.1.1...rpi-run-stats-v0.1.2) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
