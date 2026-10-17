# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4](https://github.com/pi-rust/rpi-package/compare/rpi-run-stats-v0.1.3...rpi-run-stats-v0.1.4) - 2026-10-17

### Added

- *(rpi-run-stats)* persist timing metrics across turns
- *(rpi-run-stats)* replace text labels with monochrome glyphs
- *(release)* add package-only backfill and prebuilt distribution
- *(run-stats)* stabilize metrics and show tool calls and priced costs

### Other

- Merge main into feature-1014 and preserve release compatibility
- Merge main into feature-1014 and preserve ACP integration checks
- *(release)* use reusable release assets workflow

### Changed

- Group monochrome, single-column icons into counts, response performance and usage, with a space before each value.
- Use compact numbers and three-decimal USD costs; preserve configured-pricing visibility.

### Fixed

- Retain the latest valid timing and token speed across prompts and tool continuations instead of briefly showing missing values.
- Keep slow first-token waiting separate from retained measurements and use theme colors for warnings and errors.

## [0.1.3](https://github.com/pi-rust/rpi-package/compare/rpi-run-stats-v0.1.2...rpi-run-stats-v0.1.3) - 2026-10-15

### Added

- *(run-stats)* stabilize metrics and show tool calls and priced costs ([#17](https://github.com/pi-rust/rpi-package/pull/17))

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
