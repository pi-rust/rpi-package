# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-10-27

### Added

- Register the `edit` tool from rpi's `--fork` line, shadowing the built-in `edit`.
- Match exact text, then upstream `normalize_for_fuzzy_match` only: no indentation, escape or middle-line fuzziness, and no guessing an ambiguous occurrence.
- Report every failing edit in one round trip with the closest region's line numbers, the kind of difference, the differing lines and the real text to copy.
- Support `edits[]` with optional `replaceAll`, all-or-nothing application, a `note:` for normalized matches and a `warning:` for bracket-balance changes.
- Return the display diff, unified patch and `first_changed_line` in the tool details.
- Preserve BOM and CRLF/LF, write through a temporary file plus rename, and resolve relative paths against the host-injected `__rpi.cwd`.
- Port the ported `pi-semantic-edit` candidate finder (MIT) for the failure reports.
