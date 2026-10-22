# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.9](https://github.com/pi-rust/rpi-package/compare/rpi-orchestrator-v0.1.8...rpi-orchestrator-v0.1.9) - 2026-10-22

### Fixed

- *(orchestrator)* keep the crate clippy-clean

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-orchestrator-v0.1.7...rpi-orchestrator-v0.1.8) - 2026-10-22

### Fixed

- *(orchestrator)* run children on the calling session's provider/model
  `subagent`/`dispatch` now run children on the calling session's
  provider/model instead of silently falling back to the settings default. The
  binding is read from the host (`GetExecutionContext`) and passed as
  `--provider`/`--model`; explicit parameters still win. This is how a
  delegation could end up on a different model, or fail with `No API key` for a
  provider the session never used (`openai` + a catalog model with no credential).
  A failed child also reports the binding it ran on
  (`subagent exited with 2 (selection=openai/gpt-6-astra): …`), so an
  unattributable `No API key` / `No model matches` no longer forces guesswork.

### Changed

- *(orchestrator)* the `provider`/`model` tool parameters are documented: leave
  them empty to inherit this session's model, and only override with a provider
  known to be authenticated.

## [0.1.6](https://github.com/pi-rust/rpi-package/releases/tag/rpi-orchestrator-v0.1.6) - 2026-10-16

### Added

- *(orchestrator)* enable the guidance whenever a goal is active
- *(orchestrator)* non-blocking tools with cancellation and progress
- *(orchestrator)* opt-in guidance so the orchestrator actually uses the DAG
- *(orchestrator)* add plan/dispatch DAG orchestration (stage 2)
- *(orchestrator)* pass provider/model through to the subagent child
- *(orchestrator)* stage-1 bounded subagent delegation primitive

### Fixed

- *(orchestrator)* preserve pending nodes when cancelled before the first chunk
- *(orchestrator)* keep verification responsive and preserve Windows quotes
- *(orchestrator)* normalize duplicate dependency updates
- *(orchestrator)* cancel output collection and retain child process trees
- *(orchestrator)* guard wave results and cancel Windows process trees
- *(orchestrator)* bound the child lifecycle and scope the plan per session
- *(orchestrator)* run subagents with --no-session
- *(orchestrator)* kill the whole process tree on cancel
- *(orchestrator)* make completion engine-owned and verified

### Other

- *(orchestrator)* make the Windows descendant fixture deterministic
- *(orchestrator)* stop implying branch replay is the storage model
- *(orchestrator)* use slice::contains in the acyclic check
- *(orchestrator)* add ARCHITECTURE.md (host + rpi-goal + mcp-adapter)
- *(orchestrator)* AC7 is covered by rpi-goal (installed and verified)
- *(orchestrator)* record the acceptance run (AC1-AC6, AC8, AC10 pass)
- *(orchestrator)* drop branch replay; the graph is run-scoped
