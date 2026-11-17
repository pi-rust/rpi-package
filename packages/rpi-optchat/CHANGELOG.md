# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0]

### Added

- Native port of OptChat's memory model: a session-branch message log stored as
  `optchat-entry` custom entries, a binary summary tree with a 128 KB bounded
  view, and extractive summary lines.
- `zoom` and `search` tools that read the log through the host ABI.
- `/optchat` commands: status, view, zoom, search, note, import, fit, help.
- Automatic capture of user prompts, assistant replies, tool calls and tool
  results, with a 10-second duplicate window and a subagent guard.
- One pointer line appended to the system prompt by `register_before_agent_start`
  so the model knows the memory tools exist.
