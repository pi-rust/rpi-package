# Changelog

## [Unreleased]

- Show live tool execution durations, freeze timers on completion or interruption, and preserve completed Bash timings across page reloads and service restarts.

- Add a native `rpi --web` extension entry point alongside the standalone executable, including `rpi --web=init`, shared JSON configuration, embedded frontend assets, and bounded shutdown of HTTP streams and native session children.
- Publish a standard `rpi-extension.json` with five-platform extension libraries, plus separate standalone archives built with the same verified frontend.

## [0.1.1](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.0...rpi-web-v0.1.1) - 2026-10-21

### Added

- *(web)* load startup settings from .rpi/web.json ([#43](https://github.com/pi-rust/rpi-package/pull/43))

- Load global `~/.rpi/web.json` and project `.rpi/web.json` settings, with explicit `--config`, CLI overrides and non-overwriting `--init` setup. Keep existing session storage defaults and support `allowedHosts` in JSON.

## [0.1.0] - 2026-10-21

- Initial project and chat workspace with Chinese/English interfaces, password login, Markdown rendering, streaming RPC, tool/permission dialogs, model settings, history recovery and file previews.
- Publish portable binaries with the verified frontend for Windows, Linux and macOS. The native `rpi` host is installed separately.
