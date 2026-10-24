# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.15](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.14...rpi-acp-v0.1.15) - 2026-10-24

### Other

- extract shared extension core and HTTP adapter

## [0.1.14](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.13...rpi-acp-v0.1.14) - 2026-10-23

### Fixed

- *(acp)* hide todo tool JSON from Zed cards
- *(acp)* refresh Zed plan from todo updates

### Other

- Merge branch 'main' of github.com:pi-rust/rpi-package into feature/web-session-search
- *(release)* bump workspace to 0.1.9

<<<<<<< HEAD
=======
## [0.1.13](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.12...rpi-acp-v0.1.13) - 2026-10-23

### Fixed

- *(acp)* publish todo cards and live Zed plan updates ([#62](https://github.com/pi-rust/rpi-package/pull/62))

>>>>>>> c97625a2dd277655a4bfaf488a3212aeeef519ca
### Fixed

- Hide the `todo` tool's structured arguments and result envelope from ACP tool-call cards, showing a concise action title instead of raw JSON.
- Forward `todo` result snapshots as ACP plan updates so Zed's plan panel reflects pending, in-progress, and completed task states immediately.

## [0.1.12](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.11...rpi-acp-v0.1.12) - 2026-10-22

### Fixed

- *(acp)* expose Zed model and provider switching

### Fixed

- Make Zed model configuration resilient across native RPC state shapes, group models by Provider, and retain model switching when thinking-level discovery is unavailable. Configuration failures are now logged instead of silently hiding the ACP options.

## [0.1.11] - 2026-10-21

### Added

- Advertise ACP Session Config Options — a `model` and a `thought_level` selector — so clients (e.g. Zed) can offer a model picker, and implement `session/set_config_option` to switch model or thinking level without reconnecting.
- Advertise `logout` and `session/delete`, implementing both: log out the default provider, and tear down a session's backend plus its persisted files.

### Fixed

- Forward the host's `--model` / `--provider` flags to the spawned `rpi --mode rpc` child so a Zed `args: ["--acp", "--model", "gateway/model"]` actually selects that model for the session.

## [0.1.10] - 2026-10-21

### Fixed

- Align the ACP extension and standalone bridge with the current native RPC: send prompt `message`, read `success`/`data`, and wait for `agent_settled` after acceptance. Preserve final message snapshots and report rejected or failed runs correctly. Update RPC fixtures and add acceptance, state, and terminal-outcome regressions.
- Stop a desynchronized backend when its event queue overflows instead of waiting indefinitely for a lost acceptance or returning an incomplete reply. Require reconnecting before reusing that backend.
- Inherit the workspace's exact SDK version so published builds retain the tested plugin ABI.

## [0.1.9](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.8...rpi-acp-v0.1.9) - 2026-10-17

### Added

- *(release)* add package-only backfill and prebuilt distribution
- *(release)* add cross-platform prebuilt extension distribution

### Fixed

- harden subagent lifecycle and integrations

### Other

- Merge main into feature-1014 and preserve release compatibility
- explain native Computer ACP setup and selective releases
- *(acp)* fold core back into ACP and remove WebUI integration
- Merge main into feature-1014 and preserve ACP integration checks
- *(release)* use reusable release assets workflow

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.7...rpi-acp-v0.1.8) - 2026-10-16

### Added

- Add `--bypass` to skip permission-gate approvals for ACP sessions and the standalone bridge.

## [0.1.7](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.6...rpi-acp-v0.1.7) - 2026-10-15

### Added

- *(acp)* advertise ACP terminal auth and generate the registry entry ([#14](https://github.com/pi-rust/rpi-package/pull/14))

## [0.1.6](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.5...rpi-acp-v0.1.6) - 2026-10-14

### Other

- *(acp)* consolidate ACP and use native Computer integration ([#12](https://github.com/pi-rust/rpi-package/pull/12))

## [0.1.5](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.4...rpi-acp-v0.1.5) - 2026-10-14

### Added

- *(webui)* automatically connect Open WebUI to the current project ([#8](https://github.com/pi-rust/rpi-package/pull/8))

## [0.1.4](https://github.com/pi-rust/rpi-package/compare/rpi-acp-v0.1.3...rpi-acp-v0.1.4) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
