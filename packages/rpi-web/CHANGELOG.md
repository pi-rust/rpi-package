# Changelog

## [Unreleased]

## [0.1.4](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.3...rpi-web-v0.1.4) - 2026-10-21

### Added

- *(web)* show a terminal server status page after startup ([#49](https://github.com/pi-rust/rpi-package/pull/49))

- Display a terminal Web server status page after binding, with version, listening IP, actual port, browser URL, project, authentication state, Host rules, and Ctrl+C guidance. Report stopping/stopped states during graceful shutdown.
- Support IPv6 listener addresses and use OS-assigned ports consistently in startup output and Host validation. Verify startup and HTTP access on all five release platforms.

## [0.1.3](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.2...rpi-web-v0.1.3) - 2026-10-21

### Fixed

- *(web)* preserve command feedback during initial history load ([#47](https://github.com/pi-rust/rpi-package/pull/47))

- Preserve local command feedback while the initial transcript snapshot loads, and wait for the model catalog before resolving slash model names.

## [0.1.2](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.1...rpi-web-v0.1.2) - 2026-10-21

### Added

- *(web)* display durable tool execution timings and support native extension ([#45](https://github.com/pi-rust/rpi-package/pull/45))

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
