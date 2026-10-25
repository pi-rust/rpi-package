# Changelog

## [Unreleased]

## [0.1.10](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.9...rpi-web-v0.1.10) - 2026-10-25

### Added

- *(durable)* add branch-backed durable runtime

## [0.1.9](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.8...rpi-web-v0.1.9) - 2026-10-25

### Other

- *(snapshot)* parallelize messages and history requests
- add architecture boundary guidance

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.7...rpi-web-v0.1.8) - 2026-10-24

### Fixed

- *(web)* improve session loading and workspace UI

### Other

- extract shared extension core and HTTP adapter

### Fixed

- Show the session's project directory and Git branch beside the composer, with linked-worktree detection and compact path truncation on narrow screens.
- Load the latest 80 messages for the initial snapshot and fetch model catalogs after history, while retaining native pagination for earlier messages.
- Return native structured `ask_user` answers, including multiple choices and comments, without changing permission dialog responses.
- Keep the mobile composer at its intended 14px corner radius instead of inheriting the desktop's 17px radius.
- Use square corners for fenced code blocks and remove the nested preformatted border that doubled the frame and heading separator.
- Restore token and cache metrics from the latest historical response when legacy session totals are unavailable, retain them across diagnostic refreshes, and derive input/output counts from native aggregate statistics.
- Keep the composer footer on one line with input, output, cache hit rate and context usage/capacity; move total/cached tokens and secondary metrics into the expandable details.

## [0.1.7] - 2026-10-24

### Changed

- Prefer `rpi --webserver` for starting the Web server, including `=init`, `=help`, explicit configuration paths, and host `--port` / `--bind` overrides. Keep `--web` as a compatibility alias with the same configuration and session storage.
- Reject different values supplied to `--webserver` and `--web` together, and retain service-entry conflict checks and inactive RPC child behavior.
- Document how React/Vite assets are built, embedded in the extension, and served without a separate frontend process.

### Fixed

- Allow the generated, ignored frontend bundle to be included during crate publication, fixing the release failure caused by Cargo's dirty-file check.

## [0.1.6](https://github.com/pi-rust/rpi-package/compare/rpi-web-v0.1.5...rpi-web-v0.1.6) - 2026-10-23

### Added

- *(e2e)* mock stats data and update shell card assertions
- *(rpi-web)* add MenuSelect component and background process helper

### Fixed

- *(web)* restore desktop chrome after the mobile responsive pass
- *(rpi-web)* prevent console windows in background processes
- fix queued message editing

### Other

- *(web)* note the desktop chrome and toolbar fixes in the changelog
- *(web)* stop tracking generated frontend bundle
- *(release)* bump workspace to 0.1.9
- *(rpi-web)* add session tags and sidebar filters`

### Fixed

- Restore the desktop workspace chrome after the responsive pass: keep the rounded buttons, cards, popovers and composer instead of the global square-corner override, and restore the missing `--radius` token.
- Hide the mobile-only session switcher and its backdrop on desktop, and keep the connection status chip in the toolbar.
- Keep the model and thinking (provider) pickers in the composer only instead of duplicating them in the top-right toolbar.
- Prevent background RPC, Git and authentication refresh processes from opening console windows on Windows while preserving their standard I/O and shutdown behavior.

### Added

- Add durable session tags, server-side session list filters, and sidebar tag/status filtering.
- Add a session tag editor with bounded, normalized tag persistence.

### Notes

- Session search currently indexes project names, session titles, IDs, and tags without opening offline native hosts. Transcript full-text search remains a follow-up feature.

## [0.1.5] - 2026-10-21

### Changed

- Render shell tool calls as terminal cards: the command stays on one summary line with the elapsed time, the full command is shown with a copy button, and output is displayed as cleaned plain text (ANSI colours removed) instead of JSON. Long commands and output stay contained on narrow screens.

### Added

- Accept `rpi --web --port N` and `rpi --web --bind ADDR`: the extension reads the host's parsed CLI flags and applies them after every configuration file, matching the standalone `--port` / `--bind` behavior.
- Read global Web settings from `~/.rpi/agent/web.json`, honoring `RPI_CODING_AGENT_DIR`, so Web configuration lives beside the host's other agent files. A legacy `~/.rpi/web.json` is still read first and overridden by the agent-dir file.

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
