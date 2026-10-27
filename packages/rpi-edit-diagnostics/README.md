# rpi-edit-diagnostics

A diagnosing `edit` tool. Matching is not relaxed: an `oldText` must be present
verbatim, or match after upstream Pi's normalization layer (trailing whitespace,
typographic quotes, dashes and spaces) — nothing else. When that fails, the tool
does not pick a likely region and change it. It answers like a compiler: the
line numbers of the closest real text, what exactly differs, and that text
verbatim so the next attempt can copy it. `replaceAll: true` is the one explicit
way to broaden an edit.

The package registers a tool named `edit`, which shadows the built-in one (the
host resolves an extension tool before a same-named built-in), so installing it
replaces the matching behavior and the failure reports of `edit` while the tool
name, the schema shape and the `oldText`/`newText`/`replaceAll` fields stay the
same.

## Matching contract

- Each `edits[].oldText` is matched against the **original** file snapshot, never
  incrementally after earlier edits, and is spliced only if every edit resolves.
- Exact text first; then `normalize_for_fuzzy_match` (NFKC plus per-line
  trailing-whitespace trim and typographic quotes/dashes/spaces) on both sides.
  Different indentation, different escapes, or a different middle line are *not*
  tolerated and are reported as such.
- An `oldText` that occurs more than once is never resolved by guessing an
  occurrence; set `replaceAll: true` to change all of them.
- Every failing edit is reported in one round trip and the file is left
  byte-identical.
- A successful edit appends a `note:` when its `oldText` only matched after
  normalization (so the model knows its text was not literal), and a `warning:`
  when the replacement changes the net `{}`/`()`/`[]` balance of that edit.

## Failure report

This is the exact output of the package's
`indentation_only_difference_is_reported_with_lines_and_help_text` test:

```text
error: edits[0].oldText not found in f.rs
  --> lines 1-3 are the closest match: only indentation differs (file uses 8 spaces, oldText 4 spaces)
  differing lines:
      2 - oldText:       let x = 1;
      2 + file:              let x = 1;
  help: this is the text at lines 1-3 (the "| " prefix is not part of it); use it as oldText:
  | fn a() {
  |         let x = 1;
  | }
```

An ambiguous `oldText` lists every occurrence, collapsing repeats on one line to
a count:

```text
error: edits[0].oldText occurs 3 times in edit.txt (line 1 (3 times))
  help: include the surrounding lines that distinguish the intended occurrence, or set replaceAll: true to change every occurrence
```

## Success report

```text
Successfully replaced 1 block(s) in src/a.rs.
note: edits[0] matched after normalizing trailing whitespace/quotes (line 1)
warning: edits[0] changes brace balance by -1 '}' (lines 1-2)
```

The tool result details also carry the display diff, the unified patch and
`first_changed_line`.

## Files

A leading BOM and the file's dominant line ending (CRLF or LF) are preserved,
and the new content is written to a temporary file in the same directory that is
renamed over the target, so an interrupted write cannot truncate a file. A
relative `path` is resolved against the host-injected `__rpi.cwd`, falling back
to the process working directory; unknown argument keys are ignored.

## Installation

```text
rpi install rpi-edit-diagnostics
```

From a checkout, build the cdylib and drop it into the extension directory:

```text
cargo build -p rpi-edit-diagnostics --release
cp target/release/librpi_edit_diagnostics.so ~/.rpi/agent/extensions/
```

`rpi --verbose` reports plugins it failed to load. Once it is loaded, a
deliberately failing edit — an `oldText` whose indentation differs from the file
— answers with the report shown above instead of a plain "not found"; that is
the cheapest way to confirm the extension is shadowing the built-in tool.

## Attribution

The applying, diagnostics and diff code is copied from `rpi` (the
`--fork`/`aia` line, `v0.9.4-aia.2`) — `crates/rpi-tools/src/tools/{edit_apply,edit_diagnostics,edit_diff,edit}.rs`
— MIT, (c) 2026 pi-rust contributors. The candidate finder in `src/semantic_edit/`
is a port of [`pi-semantic-edit`](https://github.com/k3-2o/pi-semantic-edit) by
K2 (MIT, (c) 2026 K2), used only to suggest the regions a failure report quotes.
The license texts are reproduced in [NOTICE](NOTICE).
