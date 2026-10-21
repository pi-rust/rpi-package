# rpi-web

rpi-web is a browser workspace for the native `rpi` coding agent. It provides a project and session sidebar, streaming conversations, tool interactions, Markdown rendering, file previews, and Git diffs. Tool cards show elapsed execution time while commands run and keep the final duration after completion, reload, and service restart. The interface supports English and Simplified Chinese, with light and dark themes.

Runtime: `browser → rpi --web (extension) or rpi-web (standalone) → rpi --mode rpc → model and project tools`. Each open session runs a separate native host process. Project files, model credentials, and session records live on the computer or server running the service.

## Installation

This guide uses the **rpi 0.7.0** native host. Web shares one implementation between a native extension and a standalone executable.

### Native extension

The latest release supports native extension installation (Web 0.1.1 and earlier are standalone only):

```bash
rpi install rpi-web --github pi-rust/rpi-package
rpi --web=init  # Create project .rpi/web.json once.
rpi --web      # Start from that project directory.
```

The host also supports `rpi install rpi-web` with repository discovery from crates.io metadata. To try the current implementation from a local checkout before publication:

```bash
rpi install rpi-web --path packages/rpi-web
rpi --web=init
rpi --web
```

Run the installation command from the `rpi-package` repository root, then start Web from the project you want to use. The extension includes the complete frontend in its library; it needs no standalone Web executable, Node.js, or separate assets directory. Unless `rpiBin` is explicitly configured, sessions use the same `rpi` executable that loaded the extension. RPC children do not start another Web server.

Both entry points read the same `.rpi/web.json`. Use `rpi --web=/absolute/path/to/web.json` to select an explicit file, and `rpi --web=help` for help. Keep the host process running; Ctrl+C stops Web, closes streams and session children, and releases the listener. Saved transcripts remain available after restart. Choose one service entry point per process: `--web` cannot be combined with `--acp`, `--webui`, or `--server`.

### Standalone executable

The standalone program remains available for direct execution and container deployments:

1. Download and extract `rpi` for your platform from the [host v0.7.0 release](https://github.com/bigfish1913/pi-rust/releases/tag/v0.7.0).
2. Download and extract `rpi-web` from the [Web releases](https://github.com/pi-rust/rpi-package/releases?q=rpi-web-v). Builds are available for Windows x64, Linux x64 / ARM64, and macOS Intel / Apple Silicon.
3. The archive includes a complete `assets/` directory for external frontend overrides. Place the host executable alongside the Web executable, or select it with an absolute `--rpi-bin` path. The release's `rpi-web-release.json` contains checksums.

For example, a Windows installation can use this layout:

```text
C:/Tools/rpi-web/
├── rpi-web.exe
├── rpi.exe
└── assets/
    ├── index.html
    ├── favicon.svg
    └── assets/
```

On Linux / macOS, the executables are named `rpi-web` and `rpi`, without the `.exe` suffix. Prebuilt archives do not require Rust or Node.js. Git must be on the service process's PATH to use Git features.

You can also install both executables with Cargo:

```bash
cargo install rpi-cli --version 0.7.0 --locked
cargo install rpi-web --locked
rpi --version
rpi-web --version
```

The source version embeds the complete frontend in both entry points. Explicit `--assets` / JSON `assets` still selects an external frontend. Older standalone releases, including 0.1.1, require the archive's complete `assets/` directory.

## Recommended: save startup settings in .rpi

Configuration files are supported starting with Web **0.1.1**. Version `0.1.0` supports only the command-line startup described below.

Run this once from your project directory:

```bash
rpi-web --init
```

For an extension installation, use `rpi --web=init` instead. Either command creates `.rpi/web.json` without overwriting an existing file. The default contents are:

```json
{
  "project": "..",
  "rpiBin": "rpi",
  "port": 3030
}
```

With the host available, subsequent starts from that project directory need only:

```bash
rpi --web  # Extension installation.
# Or: rpi-web, when using the standalone executable.
```

Edit the configuration when you need different paths or settings. For example, on Windows:

```json
{
  "project": "..",
  "projects": ["D:/Projects/another-project"],
  "projectsRoot": "D:/Projects/web-projects",
  "rpiBin": "C:/Tools/rpi-web/rpi.exe",
  "assets": "C:/Tools/rpi-web/assets",
  "bind": "127.0.0.1",
  "port": 3030
}
```

Every field is optional. JSON paths are resolved relative to **the configuration file's directory**, so `"project": ".."` in a project's `.rpi/web.json` selects the project root. A bare `rpiBin` program name is found on PATH; a value containing a directory is resolved as a path. On Windows, use forward slashes such as `D:/Projects/...` to avoid JSON backslash escaping.

Settings are applied in this order: built-in defaults → `~/.rpi/web.json` → the startup project's `.rpi/web.json` → command-line options. The startup project defaults to the current directory. With `--project PATH`, the project configuration is discovered in that directory; parent directories are not searched. Global settings suit a service managing several projects, while project settings suit starting from a project directory.

Later fields replace earlier values. A project's `projects` array replaces the global list; CLI `--add-project` options append to the resulting list. For a temporary port change, use `rpi-web --port 3031`. If you configured `allowedHosts`, update its port entries as needed.

To load only a specific configuration file, followed by CLI overrides:

```bash
rpi-web --config /path/to/.rpi/web.json
# Create a configuration at a specific location.
rpi-web --config /path/to/.rpi/web.json --init
```

Additional fields include `sessionDir`, `registry`, and `allowedHosts`. Omitting them preserves the existing defaults and session-storage locations. Configuration uses strict JSON: unknown fields or invalid contents produce a startup error.

Supply the Web access password and model API keys through environment variables or the native host's credential configuration, rather than Web JSON. The `RPI_WEB_ALLOWED_HOSTS` environment variable overrides JSON `allowedHosts`.

## Command-line startup (also supports 0.1.0)

Choose an existing project directory and configure model credentials for the service process. The examples below use an Anthropic environment variable. You can instead set `OPENAI_API_KEY` and select a corresponding model in the Web interface. If the CLI is already configured, Web's native host uses the same service user's settings.

**Windows / PowerShell**, using the installation layout above:

```powershell
$env:ANTHROPIC_API_KEY = 'your-api-key'
C:/Tools/rpi-web/rpi-web.exe --project D:/Projects/my-project --rpi-bin C:/Tools/rpi-web/rpi.exe --assets C:/Tools/rpi-web/assets --port 3030
```

**Linux / macOS**, from the extracted Web directory:

```bash
export ANTHROPIC_API_KEY='your-api-key'
./rpi-web --project /path/to/my-project --rpi-bin /path/to/rpi --assets "$PWD/assets" --port 3030
```

Open [http://127.0.0.1:3030/](http://127.0.0.1:3030/). Keep the service process running; press Ctrl+C to stop it. By default, Web listens only on loopback and opens the workspace directly when no access password is configured.

Model API keys and the Web login password serve different purposes: API keys authorize model requests, while `RPI_WEB_PASSWORD` protects workspace access. See the [host configuration guide](https://github.com/bigfish1913/pi-rust/blob/main/crates/rpi-cli/README.md) for credentials, custom models, and gateways. Custom models are typically configured in `~/.rpi/agent/models.json`.

## First conversation

1. Select a project in the sidebar and click its **New session** action.
2. Click the model name in the toolbar or open **Runtime settings** to choose the model, thinking level, and tools. Use **Provider auth** to configure an API key; OAuth controls appear only when the host supports them.
3. Enter a request and press Enter to send, or Shift+Enter for a newline. Replies, thinking, and tool results stream into the conversation. Interactive tools display the corresponding dialogs.
4. Use the stop button to cancel a running task. Ordinary messages sent during a run enter the follow-up queue; inspect or cancel pending messages in the queue panel.
5. Open the file panel to inspect project files and Git changes. These panels provide read-only previews; the agent can modify project files through its tools when requested.
6. Open settings to choose English / Simplified Chinese and light / dark appearance. Language selection is also available on the login screen.

### Create or add projects

Click **Add project** in the sidebar:

- **Create project**: enter a name and optionally initialize Git. The service creates a directory inside `--projects-root`.
- **Open folder**: enter an existing subdirectory inside that root. Relative paths are resolved against `--projects-root`; absolute paths inside the root are also accepted.

When using a remote service, these are **server directories**. To register an existing project outside the managed root, use repeatable `--add-project` options at startup:

```bash
rpi-web --project /work/project-a --add-project /work/project-b --add-project /work/project-c --projects-root /work/new-projects --rpi-bin /path/to/rpi --assets /path/to/web/assets
```

Sessions are grouped by project. New sessions use the selected project as the tools' working directory; forks and clones retain their source project. Session menus support rename, pin, archive, and restore. Archiving retains history and does not cancel a running task. Sidebar search filters project names and session titles.

### Commands and keyboard shortcuts

| Action | Command or shortcut |
| --- | --- |
| Show command help | `/help` |
| Choose model / thinking level / tools | `/model`, `/thinking`, `/tools` |
| Create a session / stop a run | `/new`, `/abort` |
| Compact context | `/compact` or `/compact important details to preserve` |
| Resume an interrupted run | `/continue` or the interrupted-run banner action |
| Create a session | Ctrl / Cmd + N |
| Search sessions | Ctrl / Cmd + K |
| Toggle the sidebar | Ctrl / Cmd + B |
| Open settings | Ctrl / Cmd + , |

Available actions depend on host capabilities. This version does not yet provide full Codex feature parity.

## Startup options

Run `rpi-web --help` to list options.

| Option | Default and purpose |
| --- | --- |
| `--init` | Create project `.rpi/web.json`, or the `--config` destination; never overwrite an existing file |
| `--config PATH` | Load only this JSON configuration, skipping global / project discovery |
| `--project PATH` | Current working directory; must exist; selects the default project and project configuration location |
| `--add-project PATH` | None; repeat to add other existing projects |
| `--projects-root PATH` | `projects/` beside the registry; managed root for browser-created or added projects |
| `--rpi-bin PATH` | `rpi` on PATH; an absolute executable path is recommended |
| `--assets PATH` | Discover assets beside the executable or in the source tree; an absolute path to the complete frontend directory is recommended for deployment |
| `--bind IP` | `127.0.0.1`; use `0.0.0.0` for LAN or container access |
| `--port N` | `3030`; CLI overrides the configuration file |
| `--registry PATH` | `~/.rpi-web/sessions.json`; session index, titles, and organization metadata |
| `--session-dir PATH` | Native host's default session store; override to select an explicit transcript directory |
| `--version` / `-V` | Print the Web version |

## LAN and server access

The following examples use server address `192.168.1.10` and port `3030`; replace them with your own address. Configure the bind address, allowed Host values, and access password.

**Linux / macOS:**

```bash
export RPI_WEB_PASSWORD='replace-with-a-long-password'
export RPI_WEB_ALLOWED_HOSTS='192.168.1.10:3030,127.0.0.1:3030,localhost:3030'
rpi-web --project /work/my-project --rpi-bin /path/to/rpi --assets /path/to/web/assets --bind 0.0.0.0 --port 3030
```

**Windows / PowerShell:**

```powershell
$env:RPI_WEB_PASSWORD = 'replace-with-a-long-password'
$env:RPI_WEB_ALLOWED_HOSTS = '192.168.1.10:3030,127.0.0.1:3030,localhost:3030'
C:/Tools/rpi-web/rpi-web.exe --project D:/Projects/my-project --rpi-bin C:/Tools/rpi-web/rpi.exe --assets C:/Tools/rpi-web/assets --bind 0.0.0.0 --port 3030
```

From another device, open `http://192.168.1.10:3030/` and sign in with the configured password. `RPI_WEB_ALLOWED_HOSTS` is a comma-separated list of Host values: use the hostname or IP and the actual port, without a URL scheme, path, or trailing slash. Defaults allow only `127.0.0.1:port` and `localhost:port`.

For an HTTPS reverse proxy, add the public domain, such as `rpi-web.example.com`, to the allowlist. Preserve the browser's Host header, set `X-Forwarded-Proto: https`, and disable SSE response buffering. A proxy on another machine or container must be able to reach Web's listening address. Use HTTPS and an access password for public deployment. Authentication currently uses one shared access password; visitors share the service's projects and sessions.

Environment variables are read at startup; restart the service after changing them. The program does not automatically load `.env` files, so inject variables through your shell or process manager. Native-host environment variables, including model API keys, must reach the same service process.

## Persistence and upgrades

- The registry stores session IDs, titles, pinning, and archive state. Its sibling `projects.json` stores projects added through the browser.
- Source files stay in each project's directory. Native transcripts are stored by the host, or in the directory selected by `--session-dir`. The registry is not a substitute for transcript files.
- Model configuration, credentials, and extensions belong to the service user. Persist the corresponding directories when deploying in containers.
- After restart, saved sessions may appear offline; select a session to reopen it. Browser refreshes restore conversations from the host.
- Before upgrading, let active tasks finish, back up these data locations, and stop Web. Replace the executable and complete matching `assets/` directory, retain the same projects, registry, session directories, and service user, then restart and check `--version`.

## Troubleshooting

| Symptom | What to check |
| --- | --- |
| Cannot start `rpi` | Check `rpi --version` and set an absolute `--rpi-bin` path |
| Blank page or JS / CSS 404 | Check that `--assets` contains `index.html` and the nested `assets/` directory; update the complete frontend bundle and refresh |
| LAN access rejected | Check the bind address, port connectivity, and the Host / port entries in `RPI_WEB_ALLOWED_HOSTS` |
| Login succeeds but model calls fail | Configure model authorization separately from the Web password; check the service user's credentials and selected model |
| Model list or settings are stale | Update configuration for the service user on the server; reopen sessions after host startup-setting changes and restart Web after environment changes |
| History missing after restart | Check that the registry, native session directory, project paths, and service user are unchanged |
| Cannot add an existing folder | Browser-added directories must be inside `--projects-root`; use `--add-project` for other existing directories |

## Development and API reference

The following sections cover protocol details, persistence, and frontend development. For regular use, follow the installation and startup steps above.

### Run from source

From the `rpi-package` repository root, with a native host already installed or built:

```bash
cargo run --locked -p rpi-web -- --project /path/to/project --rpi-bin /path/to/rpi --assets packages/rpi-web/assets --port 3030
```

Built frontend assets are committed to the repository. Install Node.js dependencies and rebuild only when changing the frontend, as described below.

### API and capability discovery

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

`assets/` is generated output committed to the repository. Both entry points embed it during compilation, and the standalone server can also serve an external directory. Rebuild and commit it whenever `frontend/src` changes; rebuild the Rust library or executable to update its embedded frontend.

## Integration checks

These scripts start isolated Web and native-host processes with temporary data directories. Configuration, project, authentication, and event checks do not call a model. Chat verification uses a local provider fixture and consumes no real provider quota. On Windows, supply absolute `.exe` paths.

```bash
python packages/rpi-web/scripts/verify-extension.py --rpi /path/to/rpi --plugin /path/to/rpi_web_extension.dll --browser
# On Linux / macOS use librpi_web_extension.so / .dylib.
python packages/rpi-web/scripts/verify-config.py --web /path/to/rpi-web --rpi /path/to/rpi
python packages/rpi-web/scripts/verify-projects.py --web /path/to/rpi-web --rpi /path/to/rpi --project /path/to/project
python packages/rpi-web/scripts/verify-auth.py --web /path/to/rpi-web --rpi /path/to/rpi
# Linux / WSL: synthetic RPC fault checks.
python3 packages/rpi-web/scripts/verify-events.py --web /path/to/rpi-web
# --browser requires frontend npm dependencies and Playwright Chromium.
python packages/rpi-web/scripts/verify-chat.py --web /path/to/rpi-web --rpi /path/to/rpi --browser
```

Execution timings are stored beside the configured session registry in a directory with the same basename and a `.tool-timings` suffix (for example, `registry.tool-timings/`). Commands completed before this feature was installed have no recorded duration.
