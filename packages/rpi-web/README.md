# rpi-web

Capability-driven Web workspace for the native `rpi --mode rpc` JSONL host.

## Install

Install the current native `rpi` host, then download the portable archive for
your platform from this package's `rpi-web-vX.Y.Z` GitHub Release. Extract it and
keep the `assets/` directory next to `rpi-web` (`rpi-web.exe` on Windows).
`rpi-web-release.json` lists all five platforms and their SHA-256 checksums.

Alternatively, install from source with `cargo install rpi-web --locked`.
Use `rpi-web --version` to check the installed version. The native host must
support capability discovery, prompt acceptance and `agent_settled` events.

## Run

```bash
cargo run -p rpi-web -- --project . --rpi-bin rpi --port 3030   [--session-dir PATH] [--registry PATH]
```

Add another project with repeatable `--add-project PATH`. The browser sees
these configured roots and projects added through the workspace; a new session uses the selected project, and fork/reopen
keep the source session's project. Legacy registry entries belong to the default
`--project` until they are reopened. For example:

```bash
rpi-web --project ../pi-rust --add-project . --assets packages/rpi-web/assets
```

### Projects and chats

Use **Add project** to create a project directory (with optional Git initialization)
or register an existing folder inside the managed project directory. Set
`--projects-root PATH` to choose that directory; it defaults to `projects/` next
to the session registry. Added projects are saved in `projects.json` beside the
registry and remain available after restart. On a server, this directory refers
to the server's filesystem. It should be mounted on persistent storage.

The sidebar groups chats under expandable projects. Every project has its own
new-chat action. Chat menus support rename, pin/unpin, archive and restore;
archiving keeps the transcript and does not cancel a running task. These settings
are saved on the server. Project collapse state and light/dark appearance are
remembered in the browser. Search filters both project names and chat titles.
Keyboard shortcuts: Ctrl/Cmd+N for a chat, Ctrl/Cmd+K for search, Ctrl/Cmd+B for
the sidebar, and Ctrl/Cmd+, for settings.

The password sign-in screen shares the workspace's light/dark appearance and
English/Chinese language preferences. It includes password visibility controls,
Caps Lock feedback, pending-request states, and localized login errors.

Additional endpoints:

- `POST /api/v1/projects` with `{ "name": "my-project", "initGit": true }`
- `POST /api/v1/projects` with `{ "name": "existing", "path": "folder-inside-projects-root" }`
- `POST /api/v1/sessions/:id/metadata` with `{ "pinned": true, "archived": false }`

An integration check starts a separate bridge and verifies actual project
creation, Git initialization, chat binding, rename, pin/archive, and reopen
after a bridge restart without invoking a model:

```bash
python packages/rpi-web/scripts/verify-projects.py --web /path/to/rpi-web --rpi /path/to/rpi --project .
```

`--assets PATH` selects the built frontend independently of the agent working
directory. Without it the server searches next to the binary, the source tree,
and the default project's `packages/rpi-web/assets` directory. Binary deployments
should ship the assets directory and pass its absolute path.

The workspace includes resizable sidebars (also adjustable with arrow keys), a
project selector, file tabs with line-numbered UTF-8 previews, and staged/working
Git diffs. Width preferences survive reloads. The file and Git panels are
read-only; they do not stage, commit, or edit files. New messages sent during a
run enter the follow-up queue. Failed sends restore the draft, and Chinese IME
confirmation does not submit a message.

Assistant replies and expandable thinking use the same Markdown renderer, with
GFM tables, nested/task lists, syntax highlighting, and preserved chat line
breaks. Code keeps its whitespace and scrolls horizontally; wide tables and
images stay inside the conversation. User messages preserve their original
line breaks as plain text.

Project endpoints, protected by the same authentication as sessions:

- `GET /api/v1/projects`
- `GET /api/v1/projects/:id/files?path=src`
- `GET /api/v1/projects/:id/file?path=src/main.rs`
- `GET /api/v1/projects/:id/git/status`
- `GET /api/v1/projects/:id/git/diff?path=src/main.rs&staged=false`

Previews are capped at 1 MiB and Git output at 2 MiB. Paths cannot leave their
configured root; `.git`, `.rpi`, `.env*`, `.deploy`, `node_modules`, and `target`
are excluded. Directories are loaded on demand. Git inspection disables external
diff/text conversion helpers and optional index locks.

The server binds to loopback by default and exposes:

- `GET /api/v1/health`
- `GET /api/v1/capabilities`
- `GET/POST/DELETE /api/v1/auth` for the optional password-protected browser
  session; when `RPI_WEB_PASSWORD` is set, all other API and SSE routes require
  the HttpOnly session cookie
- OAuth login is exposed as the capability-gated `oauth` RPC command: send
  `begin`, display the returned authorization URL/instructions, then send
  `exchange` with `{ "input": { "value": "..." } }`; credential material is
  saved natively and never returned to the browser
- `POST /api/v1/sessions` / `GET /api/v1/sessions` (list)
- `GET /api/v1/sessions/:id` / `DELETE /api/v1/sessions/:id`
- `POST /api/v1/sessions/:id/open` (reopen a remembered session)
- `POST /api/v1/sessions/:id/commands`
- `POST /api/v1/sessions/:id/fork` with optional `{ "entryId": "...", "title": "..." }`
- `POST /api/v1/sessions/:id/clone` with optional `{ "title": "..." }`
- `POST /api/v1/sessions/:id/rename` with `{ "title": "..." }`
- `GET /api/v1/sessions/:id/snapshot` (native transcript + caught-up events)
- `GET /api/v1/sessions/:id/events` (SSE, `?after=<seq>` replay)

Open `http://127.0.0.1:3030/` for the workspace UI.

The bridge can be protected for public deployment with `RPI_WEB_PASSWORD`.
Set `RPI_WEB_ALLOWED_HOSTS` to a comma-separated list of accepted Host headers;
when the password is unset, local development remains unauthenticated.

This bridge now exposes operations/history, queue, and stats surfaces, plus
provider authentication controls, fork/clone, and append-only branch navigation.
It deliberately leaves history deletion/editing and full parity unfinished.
`capabilities.parityComplete` stays `false`, and the UI only renders what
`GET /api/v1/capabilities` advertises:

- create a session (one native `rpi --mode rpc` child per session)
- send a prompt and watch `agent_start` → `message_*` → `agent_end` →
  `agent_settled`
- abort the active run
- follow tool execution events
- read a transcript projected into ordered rows: user/assistant text, collapsible
  thinking, tool calls with their input and result inline, and injected notices
- continue a run that recovery left interrupted (`/continue`, or the banner
  action), which spends the tokens that run never did
- compact the conversation (`/compact [instructions]`) and see the compaction
  boundary in the transcript, before and after a reload
- run browser-side slash commands (`/model`, `/thinking`, `/tools`, `/continue`,
  `/abort`, `/new`, `/help`) that call the same RPC commands as the Runtime panel
- watch context usage against the compaction threshold, from the same projection
  the next provider call will send
- pick a model, thinking level, and active tool set via `get_state`,
  `get_available_*` and `set_*`
- switch between live sessions in the sidebar, close them, and restore a
  transcript after a page reload
- inspect durable operation history in an independently scrollable timeline;
  expand records and open-operation payloads with credential fields redacted;
  use sequence pagination, inspect branch/leaf ids and token/cost stats, and keep
  settlement outcomes visible after reload
- view operation counts by type plus the active provider/model in the Stats panel
- queue follow-ups, steer messages, and next-turn messages; inspect pending or
  completed queue entries with lifecycle status, then cancel individual entries
  or clear the pending queue
- fork or clone a session with a custom name; the sidebar persists parent/child
  relation metadata as a compact branch tree, and sessions can be renamed
- configure provider API keys without returning or logging secret material; run
  the capability-gated OAuth begin/exchange flow through the extension provider,
  with credentials saved natively; refresh existing OAuth credentials through
  the native auth extension

### Persistence and reopen

Native hosts are spawned with `--session-id <uuid>`, so transcripts live in the
project's session store and survive a bridge restart. A tiny registry
(`~/.rpi-web/sessions.json` by default) remembers the ids and titles; `GET
/api/v1/sessions` reports remembered-but-not-running sessions as `offline`, and
`POST /api/v1/sessions/:id/open` brings them back (including a session whose
child exited). `--registry PATH` moves the registry file, `--session-dir PATH`
overrides the native session store.

### Snapshot, replay, and reload

History comes from the native host, not from browser state. `GET /snapshot`
issues a read-only `get_messages` command, then returns the transcript together
with any events published while the snapshot was being taken:

```json
{ "messages": [...], "lastSeq": 11, "events": [{ "seq": 12, "event": {...} }] }
```

The client renders `messages`, applies `events` in order, and then opens SSE with
`?after=<lastSeq>`. There is no gap and no overlap, and nothing is lost if events
land mid-snapshot. `leafId`, `pendingResume`, and the paging cursor fields ride
along so the UI can point at the branch tip, flag an interrupted run, and load
older transcript pages when the native host returned a truncated tail.

The native `get_messages` response keeps the newest `limit` entries by default
and returns `firstId`, `lastId`, and `hasMore`. Sending `beforeId: firstId` asks
for the previous page. Each projected message carries its session `entryId`, so
rows remain stable across reloads and pages; the UI uses that cursor for its
"Load earlier messages" action. `total` remains the full branch projection
count, while `truncated` is retained as a compatibility alias for `hasMore`.

Sessions also keep the last 512 projected events in memory with a monotonic
`seq`; SSE frames carry `id: <seq>`. That buffer is what makes `?after=` work, and
it is the fallback path for a host that does not advertise `get_messages`.
Reconnects honor `Last-Event-ID`. If the replay window was evicted, the host
sequence restarted, or a consumer fell behind, `resync_required` tells the browser
to reload the native snapshot before subscribing again. SSE sends keepalives.

The browser does not edit or delete historical records. Navigation only changes
which existing branch entry is the active leaf; the durable operation records and
settlements remain available for inspection.

## Smoke test

```bash
# Uses a separate bridge child, registry, and session directory.
RPI_BIN=/path/to/rpi RPI_WEB_BIN=/path/to/rpi-web \
  packages/rpi-web/scripts/smoke-dev-web.sh

# Optional provider-backed prompt. HTTP 429/quota is reported as SKIP (exit 0).
SMOKE_PROMPT=1 packages/rpi-web/scripts/smoke-dev-web.sh
```

The smoke script never targets or kills the current agent session; cleanup only
terminates the bridge process it started.

## Frontend

The React/Vite sources live in `frontend/`; the built bundle in `assets/` is what
the Rust server serves. `GET /` re-reads `assets/index.html` on every request, so
a frontend rebuild does not require recompiling the server.

```bash
cd packages/rpi-web/frontend
npm ci
npm run build        # typecheck + bundle into ../assets
npm run dev          # Vite on :3181, proxying /api to :3030
npm run e2e:production # builds and runs browser regression against the actual production bundle
```

`assets/` is generated output and is committed so the binary can be served
without a Node toolchain. Rebuild and commit it whenever `frontend/src` changes.

## Verified locally

The current review and its validation results are recorded in [REVIEW.md](REVIEW.md).
The subsequent ten review rounds are recorded in [REVIEW-10-ROUNDS.md](REVIEW-10-ROUNDS.md).
Rounds 11–20 are recorded in [REVIEW-ROUNDS-11-20.md](REVIEW-ROUNDS-11-20.md).
The isolated integration checks below start their own bridge and native host;
they never call a model or reuse the live registry. Supply absolute executable
paths (`rpi-web.exe` and `rpi.exe` on Windows).

```bash
python packages/rpi-web/scripts/verify-projects.py --web /path/to/rpi-web --rpi /path/to/rpi --project /path/to/project
python packages/rpi-web/scripts/verify-auth.py --web /path/to/rpi-web --rpi /path/to/rpi
# Linux/WSL only: synthetic RPC faults, no model or provider required.
python3 packages/rpi-web/scripts/verify-events.py --web /path/to/rpi-web
```

```text
browser → capabilities → create session → SSE → prompt → streaming → settled
```

with `--project D:/Projects/pi-rust` and a debug `rpi.exe` host.
