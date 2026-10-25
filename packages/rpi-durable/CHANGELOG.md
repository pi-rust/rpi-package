# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Record bounded Agent and tool lifecycle checkpoints in the native session branch.
- Mark unfinished runs and tool calls as `interrupted` after restart or shutdown.
- Add `recover` and `forget_run` actions without unsafe automatic tool replay.
- Add a bounded branch-scoped work queue with claim, finish, and cancel transitions.

## [0.1.0] - 2026-10-25

### Added

- Add the `durable` tool for session-branch workflow checkpoints.
- Restore the latest checkpoint on session start and tree navigation.
- Add the `/durable` status command and validation-focused unit tests.
