# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.14](https://github.com/pi-rust/rpi-package/compare/rpi-codemode-v0.1.13...rpi-codemode-v0.1.14) - 2026-11-06

### Other

- honor per-crate musl support matrix ([#215](https://github.com/pi-rust/rpi-package/pull/215))

## [0.1.13](https://github.com/pi-rust/rpi-package/compare/rpi-codemode-v0.1.12...rpi-codemode-v0.1.13) - 2026-11-05

### Added

- *(catalog)* publish codemode as an installable extension

## [0.1.12](https://github.com/pi-rust/rpi-package/releases/tag/rpi-codemode-v0.1.12) - 2026-11-04

### Added

- *(extcore)* reconcile bounded child snapshots with native commit replay
- *(extcore)* add capability-gated native ToolProgram child readers
- *(codemode)* emit ordered mixed native content without payload reads
- *(codemode)* forward large results through native receipts
- *(codemode)* select exact native results without lossy field filtering
- *(codemode)* preserve structured errors and selected native content
- *(codemode)* compose native tools with bounded QuickJS

### Fixed

- *(codemode)* disable native regex and make heap exhaustion fatal

### Other

- *(codemode)* qualify installed native child journal
- *(codemode)* qualify native archival body primitives
- *(codemode)* qualify optional native child observation seam
- *(codemode)* qualify the native result codec follow-up
