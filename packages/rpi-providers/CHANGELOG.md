# Changelog

## [Unreleased]

## [0.1.4](https://github.com/pi-rust/rpi-package/compare/rpi-providers-v0.1.3...rpi-providers-v0.1.4) - 2026-10-23

### Fixed

- *(providers)* reuse lifecycle resources and ship pending transport fixes
- *(providers)* preserve case insensitive request header precedence
- *(providers)* adopt stream watchdog and preserve compat overrides

### Other

- *(providers)* cover BOM and CR-only streaming through the DLL
- *(providers)* verify named and empty SSE keepalives
- *(providers)* cover non-reasoning models and reload diagnostics
- *(providers)* pin final reviewed SDK revision

### Fixed

- Ship the pending stream idle timeout, keepalive/BOM/CR framing, DeepSeek compat/reasoning replay and case-insensitive header fixes together.
- Pin the released 0.8.3 SDK and propagate client construction errors as terminal provider errors.

### Changed

- Reuse lifecycle-scoped runtimes and bounded HTTP connection pools for all four providers, while keeping credentials and request headers separate.
- Cancel active work and reject new calls on reload; older hosts without lifecycle support retain the request-scoped fallback.

## [0.1.3](https://github.com/pi-rust/rpi-package/compare/rpi-providers-v0.1.2...rpi-providers-v0.1.3) - 2026-10-22

### Fixed

- *(providers)* reserve thinking budget for DeepSeek reasoning models

### Other

- *(providers)* pin the released 0.8.0 SDK

### Fixed

- DeepSeek 的 reasoning 模型把思考 token 算进 `max_tokens`，8192 的答案额度会被长思考全部吃掉，
  流以 `finish_reason: length` 结束而正文为空或被截断。模型现在声明
  `reasoningCountsTowardMaxTokens`，由 provider 协议在答案额度之上按当前思考级别预留预算；
  答案额度同时从 8192 提到 16384。该修复需要 rpi-ai / rpi-plugin-sdk 0.8.0。

### Changed

- 将 `rpi-ai`、`rpi-plugin-sdk` 与 `rpi-extensions` 升级到 0.8.0。

## [0.1.2] - 2026-10-21

### Added

- Publish the unified DeepSeek, OpenRouter, llama.cpp and Proxy extension to crates.io, alongside the five-platform GitHub prebuilt archives.

### Changed

- Use the published SDK 0.6.3 with its optional `provider` feature and matching core dependencies instead of a pinned Git checkout.
- Keep the existing GitHub-only v0.1.1 release and its archives unchanged.
