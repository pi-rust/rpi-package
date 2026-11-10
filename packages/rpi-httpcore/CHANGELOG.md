# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.22](https://github.com/pi-rust/rpi-package/compare/rpi-httpcore-v0.1.21...rpi-httpcore-v0.1.22) - 2026-11-10

### Other

- merge main into dev/web-hmr before PR merge

## [0.1.21](https://github.com/pi-rust/rpi-package/compare/rpi-httpcore-v0.1.20...rpi-httpcore-v0.1.21) - 2026-11-10

### Added

- *(node)* add protocol-neutral node host core ([#249](https://github.com/pi-rust/rpi-package/pull/249))

### Changed

- Renamed this crate from `rpi-http` to `rpi-httpcore`. The HTTP/SSE API, `/api/v1` routes, SSE contract, authentication/Host checks and the API-only executable behavior are unchanged; the binary is now `rpi-httpcore`. The `rpi-http` crate name now belongs to the native plugin entry point (0.2.0).

## [0.1.20](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.19...rpi-http-v0.1.20) - 2026-11-07

### Added

- *(web)* release idle session hosts

## [0.1.19](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.18...rpi-http-v0.1.19) - 2026-11-05

### Other

- updated the following local packages: rpi-extcore

## [0.1.18](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.17...rpi-http-v0.1.18) - 2026-11-05

### Other

- updated the following local packages: rpi-extcore

## [0.1.17](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.16...rpi-http-v0.1.17) - 2026-11-04

### Added

- *(adapters)* expose bounded Code Mode child queries and live feed

## [0.1.16](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.15...rpi-http-v0.1.16) - 2026-11-03

### Fixed

- *(web)* retire archived sessions and permanently delete native history ([#183](https://github.com/pi-rust/rpi-package/pull/183))

## [0.1.15](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.14...rpi-http-v0.1.15) - 2026-11-02

### Other

- updated the following local packages: rpi-extcore

## [0.1.14](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.13...rpi-http-v0.1.14) - 2026-11-01

### Added

- *(web)* add paged history, file previews and execution traces

### Other

- *(web)* make branding checks portable across clean checkouts

## [0.1.13](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.12...rpi-http-v0.1.13) - 2026-10-31

### Other

- updated the following local packages: rpi-extcore

## [0.1.12](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.11...rpi-http-v0.1.12) - 2026-10-31

### Other

- updated the following local packages: rpi-extcore

## [0.1.11](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.10...rpi-http-v0.1.11) - 2026-10-30

### Added

- *(web)* show authoritative elapsed time for completed runs

### Other

- Merge pull request #119 from pi-rust/fix/web-image-rendering

## [0.1.10](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.9...rpi-http-v0.1.10) - 2026-10-30

### Fixed

- *(web)* serve project raster image artifacts securely

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

## [0.1.3](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.2...rpi-http-v0.1.3) - 2026-10-26

### Added

- *(web)* refine workspace composer and tool output

## [0.1.1](https://github.com/pi-rust/rpi-package/compare/rpi-http-v0.1.0...rpi-http-v0.1.1) - 2026-10-25

### Other

- updated the following local packages: rpi-extcore

## [0.1.0](https://github.com/pi-rust/rpi-package/releases/tag/rpi-http-v0.1.0) - 2026-10-24

### Fixed

- *(web)* improve session loading and workspace UI
- resolve HTTP paths and finalize host sessions reliably

### Other

- extract shared extension core and HTTP adapter
