# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- automatic continuation from the settled boundary: one goal-owned run per
  `AgentSettled` through the blocking `SendMessage` action, serialized by a single
  continuation worker
- `goal_complete`, `goal_blocked`, and `goal_wait` tools with the current `goal_id`
  as a stale-turn guard; plain assistant text no longer completes a goal
- goal states `waiting`, `blocked`, `usage_limited`, and `budget_limited` alongside
  `active`, `paused`, and `complete`
- cumulative token budgets from provider-reported usage, plus a read-only
  `~/.rpi/agent/goal.json` with `continuationLimits.automaticTurns` (25) and
  `.noProgressTurns` (3)
- `/goal tokens`, `/goal settings`; a footer status line
- `/goal pause`, budget exhaustion, and safety stops abort the in-flight run via
  the host `Abort` action (id 27, not yet in the published SDK). A host that does
  not support it rejects the call and the stop applies at the next turn boundary
- compaction re-injects the goal notice, and reload restores usage, safety counters,
  elapsed active time, and waiting state
- completion verification host bridge: `/goal _verify-enable on|off` plus
  `_verify-start` / `_verify-result` (host-facing, underscored). An armed host
  decides after every settled run; `pass` completes in place, `notSatisfied`
  continues with the verdict in the prompt, `failed`/`cancelled` pause with a
  named cause. Per-run iteration snapshots and verdicts are persisted (bounded at
  50 rows / 100 items / capped strings), and a missing boundary or verdict
  recovers via a visible notice or a `verification_timeout` pause instead of
  stalling the goal
- goal state gains `verifying`, `pendingDispatch`, `verifyIteration`,
  `verifyStartedAt`, `iterations[]`, and `verifications[]` (all defaulted, so old
  entries load unchanged)

### Changed

- `/goal start` and the `goal start` action arm the goal; the first work message now
  comes from the continuation worker instead of the command handler
- provider usage limits stop the goal (`usage_limited`); other provider failures
  leave it active but quiet (`waiting`)
- the legacy `.rpi/goals.json` project file is no longer a source of truth
- `/goal complete`, the `goal complete` action, and `goal_complete` now keep the
  goal visible with status `complete` instead of archiving it; `/goal stop` is the
  archive verb and `/goal start` archives the previous goal. Command help, tool
  descriptions, and the README agree on this split
- continuation prompts carry the previous `notSatisfied` verdict's reason and next
  action, so the next run knows what the verifier rejected

### Fixed

- token usage and interruption classification now come from the assistant message
  carried by `MessageEnd` (the host does not dispatch `AfterProviderResponse` to
  plugins), and are read with the host's camelCase field names
  (`usage.totalTokens`, `stopReason`, `errorMessage`); the previous snake_case
  lookups counted zero tokens and never classified an interruption
- the no-progress fingerprint hashes the assistant's visible text instead of the
  serialized message, so identical replies are detected even though timestamps and
  token counts differ
- a state write that the host defers until the current turn ends stays visible:
  the newest local intent is served until a branch read confirms its revision, so
  `/goal status` and the terminal tools no longer report "no active goal" mid-run
- a token budget that this host cannot report usage for is reported once instead
  of silently never triggering
- an unsupported abort never blocks a stop or repeats its warning: the goal still
  reaches its stopped state
- token usage is counted as observed only when a response carries positive tokens;
  a zeroed usage object from a host that reported nothing no longer hides the
  unenforceable token-budget warning
- `goal_complete`, `goal_blocked`, and `goal_wait` validate their `goal_id` inside
  the same state lock that applies the transition, so a replacement between the
  tool's read and the write cannot affect the new goal
- continuation dispatch re-checks the goal revision and status immediately before
  `SendMessage`, so a pause or replacement that lands after evaluation does not
  start a stale run
- the dispatch worker reads host-backed state, settings, and active tools before
  taking the mutation lock

## [0.2.5](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.2.4...rpi-goal-v0.2.5) - 2026-11-06

### Fixed

- *(release)* publish musl builds for non-voice extensions ([#222](https://github.com/pi-rust/rpi-package/pull/222))

## [0.2.4](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.2.3...rpi-goal-v0.2.4) - 2026-11-06

### Other

- honor per-crate musl support matrix ([#215](https://github.com/pi-rust/rpi-package/pull/215))

## [0.2.3](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.2.2...rpi-goal-v0.2.3) - 2026-11-05

### Fixed

- make strict warning builds pass

## [0.2.2](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.2.1...rpi-goal-v0.2.2) - 2026-11-02

### Fixed

- *(goal)* bind runtime actions to callback owners across reloads

## [0.2.1](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.2.0...rpi-goal-v0.2.1) - 2026-10-31

### Fixed

- *(tools)* clarify guidance and complete parameter schemas ([#135](https://github.com/pi-rust/rpi-package/pull/135))

## [0.2.0](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.11...rpi-goal-v0.2.0) - 2026-10-29

### Other

- *(durable)* [**breaking**] require canonical filtered branch recovery ([#109](https://github.com/pi-rust/rpi-package/pull/109))

## [0.1.11](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.10...rpi-goal-v0.1.11) - 2026-10-17

### Added

- *(release)* add package-only backfill and prebuilt distribution

### Fixed

- *(goal)* keep TUI responsive when starting work

### Other

- Merge main into feature-1014 and preserve release compatibility
- Merge main into feature-1014 and preserve ACP integration checks
- *(release)* use reusable release assets workflow

## [0.1.10](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.9...rpi-goal-v0.1.10) - 2026-10-17

### Fixed

- *(goal)* keep TUI responsive when starting work ([#26](https://github.com/pi-rust/rpi-package/pull/26))

## [0.1.9](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.8...rpi-goal-v0.1.9) - 2026-10-16

### Added

- *(rpi-goal)* say that starting a goal does not begin work

### Fixed

- *(goal)* retry failed notices at safe context boundaries
- *(goal)* capture inherited subagent role before runs
- *(rpi-goal)* don't count a subagent's turns against the parent's goal
- *(rpi-goal)* inject the goal notice at a safe boundary, not inside a tool batch

### Other

- *(rpi-goal)* cover the boundary injection and the subagent budget in isolation

## [0.1.8](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.7...rpi-goal-v0.1.8) - 2026-10-14

### Other

- update Cargo.toml dependencies

## [0.1.7](https://github.com/pi-rust/rpi-package/compare/rpi-goal-v0.1.6...rpi-goal-v0.1.7) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))
