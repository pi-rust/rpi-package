# rpi-webui

Install once, then open Open WebUI connected to the current project:

```sh
rpi install rpi-webui
rpi --webui=openwebui
```

No manual Pipe import, bridge path, Valves editing or model selection is required
for the managed local UI. Configure rpi's default provider/model normally first.
The extension embeds the shared ACP server; no separate ACP installation is needed.

```sh
rpi --webui=openwebui --webui-permission=bypass
rpi --webui=openwebui --webui-workspace /path/to/project
```

`--webui` is shorthand for `--webui=openwebui`. Open WebUI is currently the only
adapter; unsupported names fail explicitly. Other UI adapters can be added around
the same shared ACP server.

On first launch, an existing `open-webui` command is used. Otherwise
[uv](https://docs.astral.sh/uv/getting-started/installation/) installs Open WebUI
0.12.0 with Python 3.12 in an isolated environment. Install uv once if neither
command is available. Downloads use the Tsinghua PyPI mirror by default; an
existing `UV_INDEX_URL` takes precedence. First launch can take several minutes.

The managed UI listens only on loopback (default `127.0.0.1:8080`), uses single-user
mode and stores its database in `~/.rpi/webui/data`. Startup automatically
imports/updates its own Function, fills the rpi executable and workspace, activates
the model and opens the browser with it selected. Projects use distinct stable
models and session mappings. Other Functions, settings and chats are preserved.
Closing rpi stops only the UI process this invocation started.

Install `rpi-ask-user` for agent question tools and `rpi-permission-gate` for gated
tool approvals, as with normal rpi. Default `ask` retains that gate; `bypass`
disables it for UI agent children. Ordinary questions still appear in bypass mode.

## Existing or remote Open WebUI

```sh
rpi --webui=openwebui --webui-url https://rpi-webui.laofu.online
```

This opens the already configured server's RPI model. The agent, credentials and
workspace run on that server; the command does not connect your local project.

To automatically import/update the adapter on an authenticated server, provide
its admin session/API token in `RPI_WEBUI_ADMIN_TOKEN`. Existing server-side ACP
paths, workspace and permission mode are retained; `--webui-permission` explicitly
changes permissions. For a new remote adapter, specify `RPI_WEBUI_ACP_COMMAND`
and `RPI_WEBUI_REMOTE_WORKSPACE`, with optional `RPI_WEBUI_ACP_ARGS` and
`RPI_WEBUI_REMOTE_STATE_DIR`. These paths belong on the UI server/container.
A host executable cannot be launched through `host.docker.internal`.

For an existing local Docker server, set `RPI_WEBUI_MANAGE=0` and provide an admin
token plus container-side ACP paths. Existing authenticated servers keep their
authentication; automatic single-user mode is only used for a managed loopback UI.

Prebuilt libraries are available from the package's
[GitHub Releases](https://github.com/pi-rust/rpi-package/releases). Extract your
platform archive into `~/.rpi/agent/extensions` to install without Rust/Cargo.
Hosts with prebuilt installer support can use `rpi install`; the original rpi
0.3.23 installer still builds from Cargo.

## Questions and permissions

In the Pipe's **Valves**, set `PERMISSION_MODE` to `bypass` to execute tools
without the rpi permission gate. The default `ask` mode confirms each gated
tool call. Switching modes applies to the next turn, including restored chats.
`ask_user` questions and any other business dialogs still appear in bypass mode.
This is an administrator setting for the Pipe and applies to its users.

Text replies stream to the chat. Tool progress uses status events; completed
tools render as escaped details blocks rather than requesting Open WebUI to
execute the same tool again. Questions and permissions use native input
dialogs. Single-choice questions and permissions use a native dropdown when
supported by Open WebUI. For multiple options, enter `1,2`; labels with commas can be entered as
a JSON array, e.g. `["a,b", "中文"]`. Custom input uses `text:your answer`.
Multi-selection, descriptions, suggested text and optional comments are
supported. A comment is requested after the selection when enabled.

Permissions always require an explicit offered choice. Closing a dialog,
disconnecting, missing browser callbacks or timing out cancels the request.
There is no implicit approval. Standard ACP clients without the question
capability retain their existing permission behavior and cancel ordinary
questions. Open WebUI Computer's built-in profiles are not configured by this
package; this integration targets the main Open WebUI project's Functions.

Session mappings are isolated by user, chat, workspace and launch command and
persist across server restarts. A changed history (edit, regenerate or fork)
starts a fresh session seeded with the supplied transcript. Missing saved
sessions produce an error. Cancelled or failed turns retire their mapping;
the next turn starts from the visible transcript. One turn per chat is allowed,
including across workers. Title/tag generation never runs agent tools. Inputs
are currently text-only; image/audio requests are rejected.

## Launcher configuration

| Environment variable | Default / behavior |
| --- | --- |
| `RPI_WEBUI_AUTOSTART` | `0`; set to `1` to launch before every interactive TUI starts |
| `RPI_WEBUI_URL` | `http://127.0.0.1:8080` |
| `RPI_WEBUI_MANAGE` | `1` for loopback URLs, otherwise `0` |
| `RPI_WEBUI_COMMAND` | `open-webui`; uv fallback when missing |
| `RPI_WEBUI_UI` | `openwebui`; `--webui=<adapter>` wins |
| `RPI_WEBUI_WORKSPACE` | Current directory; CLI wins |
| `RPI_WEBUI_PERMISSION` | `ask`; CLI wins |
| `RPI_WEBUI_ADMIN_TOKEN` | Optional admin token for an existing server |
| `RPI_WEBUI_ARGS` | JSON array, default `["serve","--host","127.0.0.1","--port","8080"]` |
| `RPI_WEBUI_OPEN_BROWSER` | `1`; set to `0` for headless operation |

Server logs are written to `~/.rpi/webui/openwebui.log`. ACP children set
`RPI_WEBUI_CHILD=1` and disable autostart to prevent recursive launches.

For an existing Docker deployment, set `RPI_WEBUI_URL` to its published URL and
`RPI_WEBUI_MANAGE=0`. The Pipe runs **inside the Open WebUI server/container**:
rpi, its extensions, the ACP executable, provider credentials and workspace
must be available there. A host executable cannot be launched through
`host.docker.internal`; this integration uses ACP over local stdio.

## Validation

```text
cargo test -p rpi-acp -p rpi-acp-core -p rpi-webui
python -m unittest discover -s integrations/openwebui/tests -v
```
