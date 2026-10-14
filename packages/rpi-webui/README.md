# rpi-webui

Launch Open WebUI from rpi and use the existing `rpi-acp` bridge for agent
sessions. No custom frontend or duplicate RPC backend is included.

```text
Open WebUI → RPI ACP Pipe → rpi-acp-bridge → rpi --mode rpc
```

## Use an existing Open WebUI server

Install this extension and point the launcher at your server:

```powershell
rpi install rpi-webui
$env:RPI_WEBUI_URL = "https://rpi-webui.laofu.online"
rpi --webui
```

For this already configured deployment, select **RPI (ACP)** after signing in.
The agent, model credentials and workspace run on the server. This command opens
the server's page; it does not connect your local project to a remote ACP process.
To open the page on every interactive start, set `RPI_WEBUI_AUTOSTART=1`.

Prebuilt libraries are available from the package's
[GitHub Release](https://github.com/pi-rust/rpi-package/releases). Extract the
archive for your exact platform and copy its library into
`~/.rpi/agent/extensions` to install without Rust/Cargo. Hosts with the prebuilt
installer support can download it through `rpi install`. Older hosts, including
the original rpi 0.3.23 installer, build from Cargo when using that command.

## Setup

1. Install [Open WebUI](https://docs.openwebui.com/getting-started/quick-start/)
   in a Python environment on the same machine as rpi. Its `open-webui`
   executable must be on PATH, or set `RPI_WEBUI_COMMAND` to its absolute path.
2. Install `rpi-webui`, `rpi-acp`, and `rpi-ask-user`. For permissions, install
   `rpi-permission-gate` too. Configure the rpi provider/model as usual.
3. Start `rpi --webui`. It starts `open-webui serve --host 127.0.0.1 --port 8080`
   when needed and opens the page once available. An already running server is
   reused. Startup does not block the TUI. The extension stops only the server
   it started when the host session shuts down.
4. Sign in to Open WebUI as an administrator. In **Workspace → Functions**,
   create a Function with the contents of the generated
   `~/.rpi/webui/rpi_acp_pipe-<package-version>.py`, save and enable it.
   The source also ships in this crate at `openwebui/rpi_acp_pipe.py`.
5. Set the Function's **Valves**:

   | Valve | Value |
   | --- | --- |
   | `ACP_COMMAND` | Absolute path to `rpi-acp-bridge` / `rpi-acp-bridge.exe` |
   | `ACP_ARGS` | `[]` |
   | `WORKSPACE` | Absolute path to your project |
   | `STATE_DIR` | Persistent directory, default `~/.rpi/openwebui` |

   Alternatively, set `ACP_COMMAND` to `rpi` / `rpi.exe` and `ACP_ARGS` to
   `["--acp"]`; the `rpi-acp` extension must then be installed in that host.
6. Select **RPI (ACP)** in the model selector. Tools, credentials and model
   selection are handled by rpi. Changing the project requires updating
   `WORKSPACE`; different workspaces have separate session mappings.

The Pipe needs Python 3.11+ and Pydantic 2, already supplied by current Open
WebUI. It uses standard-library subprocesses and SQLite, without another ACP
SDK. The standalone bridge is built by `cargo build --release -p rpi-acp --bin
rpi-acp-bridge` and included in our ACP prebuilt archives. Use a bridge built
with the negotiated question extension described in the rpi-acp README.

The extension launches Open WebUI but does not install Python/Open WebUI or
change its database. Creating/enabling the Function is a one-time setup.

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
| `RPI_WEBUI_MANAGE` | `1` for the default URL, otherwise `0`; managed URLs must be loopback |
| `RPI_WEBUI_COMMAND` | `open-webui` |
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
cargo test -p rpi-acp -p rpi-webui
python -m unittest discover -s integrations/openwebui/tests -v
```
