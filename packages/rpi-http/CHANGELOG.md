# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.2.1...rpi-http-v0.2.2) - 2026-11-10

### Other

- updated the following local packages: rpi-httpcore

## [0.2.1](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.2.0...rpi-http-v0.2.1) - 2026-11-10

### Other

- updated the following local packages: rpi-httpcore

## [0.2.0](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.21...rpi-http-v0.2.0) - 2026-11-09

### Other

- *(http)* split HTTP core server modules ([#243](https://github.com/pi-rust/rpi-package/pull/243))

### Changed

- Renamed this crate from `rpi-http-extension` to `rpi-http`, published as 0.2.0 because crates.io already holds `rpi-http` 0.1.x from the former library crate (now [`rpi-httpcore`](../rpi-httpcore/CHANGELOG.md)). The plugin ABI, `rpi --http`/`--http=CONFIG`/`rpi --http=help`, `--bind`/`--port`, `/api/v1`, SSE and authentication behavior are unchanged.
- Migration: install with `rpi install rpi-http` instead of `rpi install rpi-http-extension`, and replace a Rust dependency on `rpi-http` with `rpi-httpcore`. The built cdylib file name changes from `rpi_http_extension` to `rpi_http`; the `rpi-http-service` executable keeps its name.

## [0.1.2](https://github.com/pi-rust/rpi-package/compare/rpi-http-extension-v0.1.1...rpi-http-extension-v0.1.2) - 2026-11-08

### Other

- updated the following local packages: rpi-http

## [0.1.1](https://github.com/pi-rust/rpi-package/compare/rpi-http-extension-v0.1.0...rpi-http-extension-v0.1.1) - 2026-11-07

### Other

- updated the following local packages: rpi-http

## [0.1.0](https://github.com/pi-rust/rpi-package/releases/tag/rpi-http-extension-v0.1.0) - 2026-11-06

### Added

- *(http)* add an API-only native extension entry point ([#230](https://github.com/pi-rust/rpi-package/pull/230))
