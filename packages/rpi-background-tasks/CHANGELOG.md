# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/pi-rust/rpi-package/releases/tag/rpi-background-tasks-v0.1.0) - 2026-11-10

### Added

- *(node)* add protocol-neutral node host core ([#249](https://github.com/pi-rust/rpi-package/pull/249))
- add rpi-background-tasks (background jobs + jobs inspector)

### Fixed

- *(background-tasks)* cover Windows fixtures and settled tree shutdown
- *(background-tasks)* reap what shutdown terminates, and clean up a failed attach
- *(background-tasks)* bind each job to the session that started it
- *(background-tasks)* own the process tree, and run under the same shell as bash
- *(background-tasks)* bound the output read, not just the retained output

### Added

- `background` tool: starts a shell command, returns immediately with a job id,
  and captures stdout/stderr through drain threads so the child never blocks on
  a full pipe buffer.
- `jobs` tool with `list`, `tail`, and `kill` actions.
- `background-tasks` custom session snapshots on every status change, carrying a
  self-contained `{"jobs":[{"status":…}]}` list so a supervising UI can tell
  whether a session still has running work.
- Terminal statuses (`completed`, `failed`) recorded by a watcher thread, so
  status is correct without polling from a tool.
- `SessionShutdown` marks still-running jobs as `interrupted`, terminates retained
  process trees for settled jobs, and appends a final snapshot.
- Bounded per-job output retention (500 lines / 256 KiB) with truncation
  reporting in `tail`.
