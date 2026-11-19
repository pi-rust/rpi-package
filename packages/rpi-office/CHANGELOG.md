# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/pi-rust/rpi-package/releases/tag/rpi-office-v0.1.0) - 2026-11-19

### Added

- Native port of the pi-for-office bridge: a loopback WebSocket server (wire
  protocol v3) that lets the unchanged Excel/Word/PowerPoint task pane drive
  Office.js through 32 `office_*` tools plus `office_list_documents`.
- `/office` command family (`status`, `connect [port]`, `close`,
  `restart [port]`, `help`) and `/office-tools`, with a manual lifecycle so the
  bridge only claims port 38617 when asked.
- Multi-document routing by id, unique name or focus, with actionable ambiguity
  errors; the bridge-only `document` argument is stripped before the pane sees
  the call.
- Per-pane op capability handshake, so a legacy 0.2.x add-in is limited to the
  v1 op set and a mismatched pair fails loudly instead of degrading silently.
- Pane-prompt tagging, streamed `agent_message` deltas plus an authoritative
  final reply routed only to the originating pane, `tool_activity` progress and
  a loopback `GET /health` surface with the upstream CORS allow list.
- `before_agent_start` system-prompt block listing the attached documents and
  the tool-usage rules.
- `client-abort` backed by the host's `RuntimeActionId::Abort`, reload-safe
  shutdown through `register_plugin_lifecycle`, and per-tool
  `promptSnippet`/`promptGuidelines` through `register_tool_prompt`.
- `#document` editor completion and `/office <Tab>` argument completion on SDK
  0.26 hosts; both degrade silently when the host vtable predates the slots.
- `--office-bridge-port` / `--office-bridge-autostart` flags with
  `RPI_OFFICE_BRIDGE_*` (and upstream `PI_OFFICE_BRIDGE_*`) environment
  fallbacks.

### Notes

- The Office task-pane add-in is not vendored here; it stays a separate
  upstream artifact. Protocol, catalog and routing rules are ported from
  [dieuluucanc/pi-for-office](https://github.com/dieuluucanh/pi-for-office)
  (MIT); see `THIRD_PARTY_NOTICES.md`.
- Requires `rpi-plugin-sdk` 0.26 (ABI 4). Older hosts load the extension with
  the capabilities their vtable supports.
