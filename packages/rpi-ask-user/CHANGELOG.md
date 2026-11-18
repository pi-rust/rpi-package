# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- *(tools)* steer callers away from `timeout` on open-ended `ask_user` input questions: the model-facing description, schema and README now recommend omitting the deadline (or allowing at least 300 seconds for short choices), so a question that needs a human to look something up is no longer closed by a too-short timeout.

### Fixed

- *(tools)* release the host UI dialog entry after timeout/cancellation/abort via the new `forget` op. On older hosts that do not implement the op the call is a best-effort no-op, so cancellation behavior is unchanged.

## [0.1.10](https://github.com/pi-rust/rpi-package/compare/rpi-ask-user-v0.1.9...rpi-ask-user-v0.1.10) - 2026-11-06

### Fixed

- *(release)* publish musl builds for non-voice extensions ([#222](https://github.com/pi-rust/rpi-package/pull/222))

## [0.1.9](https://github.com/pi-rust/rpi-package/compare/rpi-ask-user-v0.1.8...rpi-ask-user-v0.1.9) - 2026-11-06

### Other

- honor per-crate musl support matrix ([#215](https://github.com/pi-rust/rpi-package/pull/215))

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-ask-user-v0.1.7...rpi-ask-user-v0.1.8) - 2026-10-31

### Fixed

- *(tools)* clarify guidance and complete parameter schemas ([#135](https://github.com/pi-rust/rpi-package/pull/135))

## [0.1.7](https://github.com/pi-rust/rpi-package/compare/rpi-ask-user-v0.1.6...rpi-ask-user-v0.1.7) - 2026-10-14

### Other

- update Cargo.toml dependencies

## [0.1.6](https://github.com/pi-rust/rpi-package/compare/rpi-ask-user-v0.1.5...rpi-ask-user-v0.1.6) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
