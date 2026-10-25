# rpi-packages

Rust-native extension packages for the `rpi` agent, built against the stable
`rpi-plugin-sdk` ABI from `pi-rust`.

The workspace includes tools, workflow extensions, provider integrations and
client adapters. Each package has its own version and setup guide. The
[package catalog](catalog/packages.json) records installation commands and
versioned prebuilt manifests for catalog extensions.

## Packages

| Package | Tools and capabilities |
| --- | --- |
| [rpi-mcp-adapter](packages/rpi-mcp-adapter/README.md) | Connect MCP servers with `mcp_list`, `mcp_call` and `mcp_request` |
| [rpi-subagents](packages/rpi-subagents/README.md) | Delegate work with `delegate_task` |
| [rpi-orchestrator](packages/rpi-orchestrator/README.md) | Bounded child agents and task graphs with `subagent`, `plan` and `dispatch` |
| [rpi-durable](packages/rpi-durable/README.md) | Persist and resume workflow checkpoints with `durable` |
| [rpi-lens](packages/rpi-lens/README.md) | Inspect code with `code_lens` |
| [rpi-codegraph](packages/rpi-codegraph/README.md) | Query code relationships with `codegraph` |
| [rpi-todo](packages/rpi-todo/README.md) | Track tasks with `todo` |
| [rpi-goal](packages/rpi-goal/README.md) | Maintain a persistent project objective with `goal` |
| [rpi-plan-mode](packages/rpi-plan-mode/README.md) | Plan work with `/plan`, `plan_mode_start` and `plan_mode_complete` |
| [rpi-btw](packages/rpi-btw/README.md) | Read-only side conversations with `/btw`; requires rpi 0.6.0 or newer |
| [rpi-ask-user](packages/rpi-ask-user/README.md) | Ask interactive questions with `ask_user` |
| [rpi-permissions](packages/rpi-permissions/README.md) | Manage tool policies with `permissions` |
| [rpi-permission-gate](packages/rpi-permission-gate/README.md) | Confirm `bash`, `write` and `edit` through host UI dialogs |
| [rpi-websearch](packages/rpi-websearch/README.md) | Search the web with `websearch` |
| [rpi-webfetch](packages/rpi-webfetch/README.md) | Fetch web content with `webfetch` |
| [rpi-im-message](packages/rpi-im-message/README.md) | Run a headless Feishu/Lark service |
| [rpi-voice](packages/rpi-voice/README.md) | Dictation and spoken replies with `/voice`; offline STT enabled by default |
| [rpi-run-stats](packages/rpi-run-stats/README.md) | Display live usage, timing and cost in the TUI |
| [rpi-langfuse](packages/rpi-langfuse/README.md) | Export traces and manage scores and prompts with Langfuse |
| [rpi-providers](packages/rpi-providers/README.md) | Add DeepSeek, OpenRouter, llama.cpp and Proxy providers |
| [rpi-server](packages/rpi-server/README.md) | Serve TCP JSONL requests and streaming subscriptions with `--server` |
| [rpi-acp](packages/rpi-acp/README.md) | Connect ACP clients, including Zed and Open WebUI Computer |
| [rpi-ui-adapter](packages/rpi-ui-adapter/README.md) | Connect Open WebUI directly through `--webui=openwebui` |

Extensions inspired by Pi packages are independent Rust implementations, not
ports of upstream source code.

`rpi-voice` embeds sherpa-onnx + SenseVoice for offline speech recognition by
default. OpenAI-compatible STT is also available; see the
[voice guide](packages/rpi-voice/README.md) for model setup, build options and
speech output configuration.

`rpi-run-stats` adds a live monitoring row below the usage footer: turns, LLM calls, tool executions,
output tok/s, time to first token, response time, rolling 60-second TPM,
input/output/cache tokens, errors and USD cost. Use `/stats on`, `/stats off`
or `/stats` to toggle it, or press F8. The row uses compact monochrome symbols
in three groups and retains measured timing between requests. USD accumulates model usage costs calculated by the host from
model pricing. It uses the host's generic `UiComponent` text interface.
See [rpi-run-stats](packages/rpi-run-stats/README.md) for measurement definitions.

## Provider integrations

`rpi-providers` is one independently loaded package for DeepSeek, OpenRouter,
llama.cpp and Proxy. It keeps a single native plugin entry and separate
provider IDs, model catalogs and authentication policies. Install or upgrade
one package; maintain vendor implementations in separate modules.

```bash
rpi install rpi-providers --github pi-rust/rpi-package
```

Starting with 0.1.2, this package is published to crates.io and distributed as
prebuilt archives through GitHub Releases. It uses `rpi-plugin-sdk` 0.6.3 with the optional
`provider` feature and matching `rpi-ai` 0.6.3; use the rpi 0.6.3 host for this
package. See [the package guide](packages/rpi-providers/README.md) for compatibility,
source linking, migration and tests.

## Web workspace (rpi-web)

`rpi-web` provides project-based conversations in a browser, with streaming replies, tool interactions, Markdown rendering, file previews, and Git diffs. The interface supports English and Simplified Chinese. It supports a native extension entry point and a standalone program, sharing the same Web core.

Download and extract a [Web release](https://github.com/pi-rust/rpi-package/releases?q=rpi-web-v), keep the complete `assets/` directory, and install the [rpi 0.7.0 host](https://github.com/bigfish1913/pi-rust/releases/tag/v0.7.0). Configure model credentials for the service process before starting:

```bash
# Run from the extracted Web directory with an existing project directory.
./rpi-web --project /path/to/project --rpi-bin /path/to/rpi --assets ./assets --port 3030
```

Open [http://127.0.0.1:3030/](http://127.0.0.1:3030/), select a project, and create a session. On Windows, use `rpi-web.exe` and `rpi.exe`.

The [Web user guide](packages/rpi-web/README.md) covers Windows / Linux / macOS setup, project creation, model credentials, password login, remote access, persistence, and upgrades. Web 0.1.7 uses `rpi --webserver` to start the native extension with embedded frontend resources; `--web` remains a compatibility alias. Install it with `rpi install rpi-web --github pi-rust/rpi-package`, or from a checkout using `rpi install rpi-web --path packages/rpi-web`.

Web 0.1.1 supports `.rpi/web.json` startup settings. Run `rpi-web --init` once in a project directory, configure the host and assets paths, then use `rpi-web` for subsequent starts. Global defaults can be stored in `~/.rpi/web.json`, and CLI arguments provide temporary overrides. Users of Web 0.1.0 must upgrade to use configuration files.

## Shared backend and HTTP API

`rpi-extcore` now holds the common ACP/Web native RPC, run, session, history, event and workspace capabilities. `rpi-http` adapts those services to the existing HTTP/SSE API and provides an API-only executable. `rpi-web` adds the embedded browser frontend; `rpi-acp` maps the common runtime to ACP.

```bash
cargo run --locked -p rpi-http -- --project /path/to/project --rpi-bin /path/to/rpi --port 3030
```

`rpi-extcore` is also published as a reusable crates.io library. Applications
can depend on the compatible minor line with `rpi-extcore = "0.1"`; it requires
Rust 1.78+ and a compatible native `rpi --mode rpc` executable at runtime. See
[rpi-extcore](packages/rpi-extcore/README.md), [rpi-http](packages/rpi-http/README.md) and the [architecture contract](docs/architecture/extension-core.md). The new packages are ordinary Rust libraries; existing extension installation and startup commands remain the same.

## Native ACP integration

`rpi-acp` contains the ACP protocol, session history, streaming, question handling
and Zed registration. The extension and standalone `rpi-acp-bridge` use the same
implementation within this package.

```text
Computer / Zed -> ACP over stdin/stdout -> rpi --acp -> rpi --mode rpc
                                                       (one child per session)
```

Install the extension with the `rpi` host:

```bash
rpi install rpi-acp
```

For **Open WebUI Computer**, deploy Computer separately and configure its native
ACP agent to launch the `rpi` executable with `--acp`. The process must have
access to the installed extension, provider credentials, project directory and
writable session storage. Tools run in the workspace sent by the client's
`session/new` request. Container deployments must make that same path available
to the agent process.

Set `RPI_ACP_NO_AUTO_REGISTER_ZED=1` in Computer's agent environment to disable
automatic Zed registration. Set `RPI_ACP_SESSION_DIR` to an absolute directory
when session history needs a persistent container volume. See the
[ACP package README](packages/rpi-acp/README.md#configure-computer-native-acp)
for the connection requirements and client capabilities.

For **Zed**, run `rpi --install-zed` to register the agent or `rpi --print-zed`
to print the settings snippet. Registration preserves comments and creates a
backup when updating an existing file.

The standalone bridge owns stdin/stdout and starts `rpi --mode rpc` itself.
Build its archive with `task pack-acp`; use `RPI_BIN` to select the backend
executable. Its packaging and proposed ACP Registry entry are documented in
[registry/README.md](registry/README.md).

## UI adapters

`rpi-ui-adapter` adds `rpi --webui=openwebui` to connect an existing Open WebUI
instance directly to rpi ACP, with streaming, progress, tool details and cancellation.
Each UI has its own project configuration in `.rpi/webui/<UI>.json`;
`--webui` only selects the adapter.
It automatically installs a managed Pipe; Computer is not required. rpi and
the project can remain outside the UI container. See the
[Chinese setup guide](packages/rpi-ui-adapter/README.md) for configuration examples,
container networking and session behavior.

## Build and install

Source builds use stable Rust and Cargo. Task is required for the `task`
commands; Python 3 is required for the ACP integration tests. The `rpi` host
must already be installed and configured to run the extensions.

```powershell
cargo build -p rpi-providers --locked
cargo test --workspace --locked
task install
```

Build `rpi-providers` before running its integration tests, which load the
actual native library.

`task install` builds the workspace and copies catalog extensions to
`~/.rpi/agent/extensions`, or `$RPI_CODING_AGENT_DIR/extensions` when configured.
The CLI discovers the default directory automatically; an
explicit directory also works:

```powershell
rpi --extensions-dir "$HOME/.rpi/agent/extensions"
```

These are `cdylib` extensions, so plain `cargo install` is not the right
consumer installation mechanism. The cross-platform consumer command is built
into the `rpi` host:

```powershell
rpi install rpi-todo
rpi install rpi-todo --version 0.2.3
```

For GitHub prebuilt installation, select this repository explicitly:

```powershell
rpi install rpi-ui-adapter --github pi-rust/rpi-package
rpi install rpi-providers --github pi-rust/rpi-package
rpi update
```

Use `rpi uninstall <package>` to remove a package installed through the host.
Check each package guide for host requirements and source-install alternatives.

Users can also download a platform bundle from
[GitHub Releases](https://github.com/pi-rust/rpi-package/releases) and run its
installer.
The archive is self-contained: it carries the extension libraries, the catalog
and the `rpi-package-install` binary itself, so no shell script is involved.

```bash
# Linux / macOS
tar -xzf rpi-packages-linux-x86_64.tar.gz
cd rpi-packages-linux-x86_64
./rpi-package-install install --source-dir .
```

```powershell
# Windows
Expand-Archive .\rpi-packages-windows-x86_64.zip .
cd .\rpi-packages-windows-x86_64
.\rpi-package-install.exe install --source-dir .
```

Developers can instead clone this repository and run `task install`. Most
extensions use the published `rpi-plugin-sdk` 0.3.20 pinned in the workspace;
`rpi-providers` independently pins the published SDK and core dependencies to
0.6.3, enabling `provider` only for that package. A sibling `pi-rust` checkout
is not needed for the workspace build. Linux builds with voice support also need native audio
and TLS development libraries (`libasound2-dev`, `libssl-dev` and `pkg-config`
on Ubuntu).

To install an already-downloaded archive without unpacking it by hand:

```bash
task install-bundle ARCHIVE=dist/rpi-packages-windows-x86_64.zip
```

The optional `task smoke` maintainer check exercises the host loader and does
require a sibling `pi-rust` checkout.

To test ACP unit tests and deterministic streaming scenarios, run:

```bash
task test-acp
```

Optional real-host lifecycle checks also run when `RPI_TEST_HOST` and
`RPI_TEST_PLUGIN` point to the `rpi` executable and built ACP library.

## Headless Feishu/Lark service

`rpi-im-message` runs the Feishu/Lark long-connection server with:

```powershell
rpi --im-message-server --im-profile feishu-main
```

The profile is optional when `defaultProfile` is set in `.rpi/im.json` (or
`~/.rpi/agent/im.json`). Set the app secret through the profile's environment
variable before starting the process.

## Automatic releases and prebuilt extensions

Merging to `main` runs version generation and Cargo publishing. The workflow
updates packages with unpublished changes and publishes their new versions;
already published versions are skipped. Package contents and dependency changes
determine which packages need a release.

When package releases are available, CI builds Windows x64, Linux x64/ARM64 and
macOS Intel/Apple Silicon assets. The matching package Releases receive native
libraries and a versioned `rpi-extension.json` manifest. A separate bundle Release
contains all workspace extensions, the installer and the standalone ACP bridge.
The full workspace build does not mean every extension gets a new version.

The workflow can also be run manually on `main` to retry a release.

See the [release workflow and distribution protocol](docs/prebuilt-distribution.md)
for publishing credentials, branch protection, retry behavior and host installation.
The [JSON schema](docs/rpi-extension.schema.json) defines the discovery manifest.

For a local release preflight, run `task publish-dry-run`. Manual crates.io
publishing requires `cargo login` and an unpublished package version. Each
extension is self-contained and can be published in any order.
`task pack` also creates a directly installable platform bundle under `dist/`
for a GitHub Release or another binary registry.
