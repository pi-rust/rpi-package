# rpi-optchat

OptChat's persistent chat memory for the `rpi` Rust agent.

A native port of the memory model in
[jonaslsaa/pi-optchat](https://github.com/jonaslsaa/pi-optchat) (MIT): the whole
conversation is kept in an append-only log, folded into a bounded summary view,
and read back with `zoom` and `search`. In Pi, OptChat *is* the chat — it sends
the view in place of the transcript. A plugin cannot rewrite a request, so this
port keeps OptChat's memory and exposes it to the host instead:

- the current session branch's messages and tool activity are logged as
  `optchat-entry` custom entries through the `AppendEntry` runtime action;
- the log is folded into a summary tree whose view stays inside a 128 KB budget;
- the model reads it with the `zoom` and `search` tools;
- the user reads it with `/optchat view`, `/optchat zoom`, `/optchat search`;
- `register_before_agent_start` appends one short pointer line to the system
  prompt so the model knows the memory exists.

Storage is the session branch itself: memory follows a fork, disappears with a
discarded branch, and reloads with a resumed session. No project file, no
background worker, no model call, no durable-scheduler state.

## Install

```sh
rpi install rpi-optchat
```

From a checkout: `rpi install --path packages/rpi-optchat --force`, or
`rpi dev --package rpi-optchat --no-watch`.

Requires an `rpi` host whose `GetSessionBranch` runtime action confirms
`filtered: true` and whose `AppendEntry` action accepts `{"customType","data"}`.
The plugin refuses to read a branch it cannot filter rather than treating the
whole branch as memory.

## Tools

| tool | purpose |
| --- | --- |
| `zoom` | `id`, optional `n`, `offset`, `limit`. `n = 1` opens message `id` whole from the log, paged at 25000 characters. A larger power of two opens the summary block of `n` messages starting at `id` (id must be a multiple of n) into its two halves. |
| `search` | `text`, optional `before`. Finds logged messages containing the text (case-insensitive), newest first, 20 per page; `before: id` continues with older matches. |

Both tools read the current session branch only; both return text plus
`details.kind = "optchat"`.

## Commands

```text
/optchat                 status: messages, view lines, bytes, kind counts
/optchat view [lines]    the summary view, newest lines first (default 40)
/optchat view all        the whole view, capped at 30000 characters
/optchat zoom <id> [n]   open message id, or the block of n messages from id
/optchat search <text>   find logged messages by content
/optchat note <text>     append a durable note to memory
/optchat import <file>   append a text/markdown/JSONL file as imported notes
/optchat fit             merge the view as far as the tree allows
/optchat help            the command list
```

`import` reads UTF-8 text up to 4 MiB, at most 2000 messages. Each non-empty
line becomes a `note` entry whose head reads `[IMP <day>] note`, so a later
reader can tell imported history from this session's own turns. A line that is
a JSON object is unwrapped through `text`/`message`/`content`/`body` when those
hold a string or content blocks; markdown headings lose their `#` markers.

## What is captured

| event | entry kind | text |
| --- | --- | --- |
| `before_agent_start` (`prompt`) | `user` | the prompt that starts the run |
| `message_end` (`user`) | `user` | text blocks |
| `message_end` (`assistant`) | `talk` | text blocks |
| `tool_execution_start` | `tool` | `name(arguments)`, clipped to 2000 characters, with the call id as `receipt` |
| `tool_execution_end` | `echo` | the result's text blocks, with the call id as `receipt` |
| `/optchat note`, `import` | `note` | as given, with an optional import origin |

`custom` messages and `toolResult` messages are skipped: the former belong to
other extensions, the latter duplicate the `echo` captured from tool
execution. The plugin's own `zoom`/`search` calls are not logged, because a
search echo of memory inside memory is noise. One message delivered twice within
10 seconds is logged once (a prompt arrives through both
`before_agent_start` and `message_end`).

Subagent processes (`RPI_ORCHESTRATOR_SUBAGENT`,
`LANGFUSE_PI_PARENT_TRACE_ID`) never write into the parent's memory.

## Summary lines are extractive

Upstream asks a model to compress every leaf and every merge into at most 512
bytes. A plugin must not block a turn on a nested model call, so this port uses
the deterministic fallback upstream itself uses when a compressor refuses: a
leaf keeps the head of its message (`kind: text`), and a merge keeps the head of
both halves joined by `⋯`, cut at 512 bytes on a character boundary. The log
keeps every message whole, so `zoom(id, 1)` is always the way to read the
original; the view is a navigation summary, not a lossy archive.

The view grows one line per message and, once it passes 128 KB, merges its most
due sibling pairs down to 64 KB (`/optchat fit` merges as far as the tree
allows). Nothing about the view is persisted: the merge order is a pure function
of the log, so a resumed session rebuilds the identical tree from its entries.

## Known limitations

- **Session scope, not profiles.** Upstream keeps several profiles with durable
  stores across chats; this port is scoped to one session branch. Cross-session
  memory would need a project store and a separate configuration surface.
- **No model compression.** Summary lines are extractive, so an old line is a
  truncated head rather than a written summary. The log and `zoom` keep the
  original text.
- **No Claude Code/Codex/OMP importers.** `/optchat import` reads generic
  text/markdown/JSONL; format-specific parsers are not implemented.
- **No background agents or UI panels.** rpi already provides child agents
  (`rpi-subagents`, `rpi-orchestrator`) and renders commands as messages.
- **Tool activity depends on host event dispatch.** rpi 0.24.0 dispatches
  `tool_execution_start`/`tool_execution_end` to plugins during interactive and
  `--mode rpc` runs, but not in `--print` runs; user and reply capture still
  works there.
- **The log is bounded at 20000 messages.** Capture stops there instead of
  growing without limit; existing messages stay readable through `zoom` and
  `search`.
- **Capture is best effort.** A failed `AppendEntry` never fails the turn and the
  entry is not committed to the in-memory log, so a later capture can retry
  without creating a phantom message. `/optchat note` reports the persistence
  error. Set `RPI_OPTCHAT_DEBUG=1` to see automatic-capture failures on stderr.

## Verification

```sh
cargo test -p rpi-optchat
cargo clippy -p rpi-optchat --all-targets
```

The unit tests cover the log/tree/view invariants (bounded lines, collapsing,
tiling, paging, UTF-8-safe clipping, search paging) and the command/schema
surface. End-to-end checks against a running host are described in the pull
request: `/optchat note` writes `optchat-entry` entries, a second process
resuming the session hydrates them, the `search` tool executes through the ABI,
and a `before_agent_start` run shows the pointer line in the provider request.

## Attribution

The memory model, constants and tool/command vocabulary are ported from
`jonaslsaa/pi-optchat` (`src/core/memory.ts`, `src/core/tools.ts`), MIT licensed,
Copyright (c) 2026 Jonas Silva. This crate is an independent Rust implementation
against the `rpi-plugin-sdk` ABI; it shares no code with, and does not depend on,
the upstream package or the Pi runtime.
