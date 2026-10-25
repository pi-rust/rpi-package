# rpi-acp

Run **rpi as an external agent in Computer, [Zed](https://zed.dev)** or any client
that speaks the [Agent Client Protocol](https://agentclientprotocol.com) over a
single `rpi --acp` process.

```
Computer / Zed  ──ACP over stdio──▶  rpi --acp  ──spawn──▶  rpi --mode rpc  (per session)
```

## Shared runtime

The source implementation delegates native RPC children, isolated command requests, run acceptance/settlement, text reconciliation, history traversal and question answer validation to [rpi-extcore](../rpi-extcore/README.md). This package retains ACP serialization, capabilities, client requests and Zed/terminal-auth integration. Its existing extension and bridge entrypoints and session storage paths remain unchanged. See the [architecture contract](../../docs/architecture/extension-core.md).

## How it works

Open WebUI Computer can use the same standard ACP entrypoint. Deploy and
configure Computer separately; this package provides the ACP protocol,
sessions and Zed registration. The standalone `rpi-acp-bridge` uses the same
server implementation as the `rpi-acp` extension. All implementation modules
are part of this package.

`rpi-acp` is a `cdylib` extension. It does three things:

1. **Claims the process stdio.** When `--acp` is present, the extension reads
   the flag *during plugin registration* and `declare`s
   `{"ownsStdio": true}`. The host then (a) does **not** drain piped stdin, and
   (b) runs its headless `Rpc` lifecycle instead of the TUI/print modes, waiting
   for the owner to finish or Ctrl+C. From that point the process's stdin/stdout belong to this
   extension. This is what makes an ACP stdio server possible inside an `rpi`
   process at all — see *Requirements* below.
2. **Serves ACP v1** on stdio (`initialize`, `session/new`, `session/prompt`,
   `session/cancel`, `session/load`, `session/resume`,
   `session/set_config_option` and the rpi extension `session/durable`. See
   `src/protocol.rs`.
3. **Backs each ACP session with a child `rpi --mode rpc` process** rooted at
   the session's `cwd`, translating its event stream into ACP `session/update`
   notifications (streamed text, thinking, tool calls). See `src/session.rs`.

The host process itself runs no model call: every prompt is served by the child,
so tools, models, auth, skills and extensions behave exactly as in an
interactive `rpi` session.

### Session persistence

New ACP sessions use stable UUIDs and persist native rpi JSONL history under
`~/.rpi/agent/acp/sessions`. `RPI_ACP_SESSION_DIR` can override this with an
absolute directory. The files remain when Zed or the ACP process exits.

`session/load` restores the backend context and replays the active branch's user
messages, assistant text/thinking, and tool calls/results before responding.
`session/resume` restores context without replay, for clients that already have
the transcript. An unknown or missing session returns an error rather than
silently opening an empty conversation. Sessions created by ACP 0.1.1 and older
were temporary (`--no-session`) and cannot be restored after their backend exits.

### Durable control plane

rpi-aware ACP clients may call the extension method
`session/durable` with `params.sessionId`. Use `{"operation":"status"}` to
read the branch-scoped durable state, or use `start_run`, `settle_run`,
`recover_run`, `enqueue`, `claim`, `finish` and `cancel` to coordinate durable
runs and queue items. The method persists through the native session branch,
never replays tool side effects, and does not start a background scheduler.
Standard ACP clients can ignore the extension; capability metadata marks
`rpi/durable` as optional. Successful durable mutations and durable state
changes observed during a prompt also produce the custom notification
`session/durable_update` with `{sessionId, revision, state}`. If the native host
lacks branch operations, normal ACP sessions continue to work and
`session/durable` returns an unavailable error.

### Event mapping

The child RPC uses `message` for prompt text and `success`/`data` for responses.
A prompt's `data.accepted: true` response starts the turn; ACP waits for
`agent_settled` before completing it and allowing another prompt.

Streaming text is checked against cumulative message snapshots, including
the final `message_end`. Missing text is sent before ACP reports completion,
without duplicating chunks already delivered. Backend failures return an ACP
request error; output length limits report `max_tokens` instead of `end_turn`.

| rpi `--mode rpc` event | ACP `session/update` |
| --- | --- |
| `message_update` / `text_delta` | `agent_message_chunk` |
| `message_update` / `thinking_delta` | `agent_thought_chunk` |
| `tool_execution_start` (`read`, `bash`, …) | `tool_call` (`in_progress`, `rawInput`) |
| `tool_execution_start` (`todo`) | `tool_call` with a concise action title (without raw JSON arguments) |
| `tool_execution_start` (`edit`/`write`) | `tool_call` with ACP `diff` content |
| `tool_execution_update` | `tool_call_update` (`in_progress`, content) |
| `tool_execution_end` (`todo`) | `tool_call_update` (`completed` / `failed`, content) |
| `tool_execution_end` | `tool_call_update` (`completed` / `failed`, `rawOutput`) |
| `tool_execution_end` (`plan_mode_complete`) | `plan` (step list + done markers) |
| `tool_execution_end` (`todo`) | `plan` (live todo statuses, including completion) |
| `agent_settled` with `outcome: aborted` | prompt `stopReason: cancelled` |
| `agent_settled` with `outcome: completed` | prompt `stopReason: end_turn` (or `max_tokens` for length limits) |
| rejected response or failed `agent_settled` | ACP request error |

## Requirements

* A current `rpi` RPC host with prompt `message`, `success`/`data` responses,
  and terminal `agent_settled` events.
* `rpi` whose plugin ABI supports the `ownsStdio` declaration (the field is
  additive on the `declare` callback) and `StdioLifecycle` runtime action (21).
  The owner reports EOF/completion to the host, which dispatches
  `SessionShutdown` and returns its exit code. Conflicting owners are rejected.
* A configured rpi provider/model. The ACP host process resolves the default
  model at startup (like `rpi --server`); a missing credential is reported on
  stderr and the process exits before the ACP server starts.
* An ACP client on stdin. `--acp` speaks ACP over stdin, so it must be launched
  by a client (Zed) through a pipe. Started from a terminal it fails fast — one
  message plus exit code 2 — instead of leaving the host parked in its headless
  lifecycle with a dead server. To register rpi from a terminal, use
  `rpi --install-zed`.

## Install

```powershell
rpi install rpi-acp
# or, in this repo:
task install
```

## Configure Computer (native ACP)

Deploy Open WebUI Computer separately and configure its native ACP agent to
launch the `rpi` executable with `--acp`. Use an absolute executable path if
Computer's environment does not put `rpi` on `PATH`. The host must load the
`rpi-acp` extension and have a configured provider/model before serving ACP.

The connection is JSON-RPC over stdin/stdout. Computer starts the agent process
and communicates through its pipes; a separately started process has no ACP
network endpoint to connect to. Each session starts a child `rpi --mode rpc` in
the `cwd` supplied by Computer.

For container deployments, make the runtime, extension library, provider
configuration and project accessible inside the container that starts `rpi`.
The path sent as `session/new.cwd` must exist there. Mount session history on a
writable persistent volume and configure the agent process environment:

| Variable | Purpose |
| --- | --- |
| `RPI_ACP_NO_AUTO_REGISTER_ZED=1` | Disable automatic Zed settings registration |
| `RPI_ACP_SESSION_DIR` | Absolute directory for persistent ACP session history |
| `RPI_CODING_AGENT_DIR` | rpi configuration directory |
| `RPI_BIN` | Optional override for the child rpi executable |

Permission dialogs use standard `session/request_permission`; the native client
controls how it presents or answers them. General `ask_user` questions require
the client to negotiate the [question extension](#question-extension-v1).
The agent cancels unsupported questions. Client support also determines how
session replay and cancellation appear in Computer's UI.

## Configure Zed

Add a custom external agent to Zed's `settings.json`
(`agent: open settings` → *External Agents* → *Add Agent* → *Custom*):

```json
{
  "agent_servers": {
    "rpi": {
      "type": "custom",
      "command": "rpi",
      "args": ["--acp"]
    }
  }
}
```

On Windows, point `command` at the full path to `rpi.exe` if it is not on
`PATH`. Start an rpi thread from the Agent Panel's new-thread menu.

You can pass ordinary rpi flags through `args`, e.g.
`["--acp", "--model", "gateway/model"]`. The child session inherits the process
environment, so provider API keys work as usual.

### Skip permission approvals

To skip bash/write/edit approval prompts, add `--bypass` to the existing Zed
agent's arguments:

```json
"args": ["--acp", "--bypass"]
```

Save the settings and restart Zed before starting a new rpi thread.

### Model and Provider selection

For an rpi host that supports the native RPC model catalog, each ACP session
returns a `model` Session Config Option. Its values use `provider/model`, so
selecting a model also selects its Provider; when several Providers are
available, Zed receives them as grouped choices. `session/set_config_option`
forwards the selected value to the child RPC as `modelId` plus `provider`.
Thinking levels are exposed as a separate `thought_level` option.

These controls are session-level settings in Zed's ACP thread configuration,
not Zed's global `agent.default_model` setting. If they do not appear, enable
`RPI_ACP_DEBUG=1` in Zed's environment and check its agent stderr log for
`config options unavailable`; the child rpi must support
`get_available_models`, `set_model`, and (optionally)
`get_available_thinking_levels`.

The flag only affects this process and its ACP session children; it does not change
system environment variables or saved permission settings. It overrides
`--permission-gate` and `RPI_PERMISSION_GATE=1`. Other extensions' policies and
ordinary user questions are unchanged. Omitting the flag keeps the default
approval prompts. This requires a version of `rpi-acp` containing the flag.

The standalone bridge accepts `rpi-acp-bridge --bypass` (Zed `args: ["--bypass"]`).
Existing environment configuration remains supported:
`RPI_ACP_NO_PERMISSION_GATE=1` stops ACP from adding `--permission-gate`, while
`RPI_PERMISSION_GATE=0` disables environment-based activation of the gate.

## Standalone binary (ACP Registry)

The same ACP server also builds as a standalone binary, `rpi-acp-bridge`. Unlike
the `cdylib` extension it **owns its stdin/stdout natively** — it does not need
the host `ownsStdio` declaration above, so it runs against a stock `rpi`:

```
Zed  ──ACP (JSON-RPC over stdio)──▶  rpi-acp-bridge  ──spawn──▶  rpi --mode rpc  (per session)
```

```bash
cargo build --release -p rpi-acp --bin rpi-acp-bridge
```

It also runs two maintenance commands, which is what an ACP client (or a package
manager) launches instead of the bare server:

```bash
rpi-acp-bridge --setup          # provider credential login (ACP terminal auth)
rpi-acp-bridge --install-zed    # register agent_servers.rpi in Zed
```

`rpi-acp-bridge` locates the agent binary to spawn in this order:

1. `RPI_BIN` (explicit path),
2. itself, when the process *is* `rpi` (the `cdylib`-extension case),
3. a sibling `rpi` / `rpi.exe` next to the bridge,
4. `rpi` on `PATH`.

Point Zed straight at the binary if you would rather not load the extension:

```json
{
  "agent_servers": {
    "rpi": { "type": "custom", "command": "rpi-acp-bridge", "args": [] }
  }
}
```

### Registering with Zed

Zed learns about external agents from the `agent_servers` key in its
`settings.json`. You never have to edit that by hand:

| Command | Registers |
| --- | --- |
| `rpi --install-zed` | `rpi --acp` — the extension form, no extra binary needed |
| `rpi-acp-bridge --install-zed` | the standalone bridge |
| `rpi --print-zed` / `rpi-acp-bridge --print-zed` | nothing — prints the snippet |

Both frontends share one implementation, and it is idempotent, writes
atomically, keeps a `settings.json.rpi-bak` copy the first time it changes the
file, and **preserves the file's comments and formatting**: Zed's
`settings.json` is JSONC (it allows `//` comments and trailing commas), so the
merge splices in one member rather than reparsing and re-emitting the document.

`rpi --install-zed` is a one-shot: the extension claims the process stdio like
`--acp` does, performs the merge, and reports the result through the
`StdioLifecycle` action so the host exits with `0` (registered) or `1` (could
not write). The bridge additionally accepts `--settings <FILE>`,
`--command <FILE>` and `--dry-run`.

Running **`rpi --acp` does this automatically**: the extension calls the same
merge at startup, because `--acp` is exactly the signal that the user wants Zed
integration. It is a no-op once `agent_servers.rpi` exists, so a Zed-launched
child never writes. Set `RPI_ACP_NO_AUTO_REGISTER_ZED=1` to refuse the write and
print the snippet instead (useful if your editor config is under version
control).

### Packaging for the ACP Registry

```bash
task pack-acp
```

stages `dist/rpi-acp-<platform>/` and builds a **flat** archive
(`dist/rpi-acp-<platform>.zip` on Windows, `.tar.gz` elsewhere) whose root holds
the binary — matching the registry's `cmd` convention (e.g. `./rpi-acp-bridge`).
It prints the sha256 and a ready-to-paste `distribution.binary` stanza; see
[`registry/rpi-acp/agent.json`](../../registry/rpi-acp/agent.json) and
[`registry/README.md`](../../registry/README.md).

## Authentication

`initialize` advertises one ACP **terminal** auth method, `rpi-credentials`,
whose descriptor is `args: ["--setup"]`. The ACP client runs the same program it
uses to launch the agent, plus `--setup`, as an interactive terminal process; a
zero exit status means the credentials are ready and the client reconnects.

`--setup [--provider <id>]` (default provider: `anthropic`) decides the exit code
by what is actually usable, not by whether a prompt was answered:

| State | Result |
| --- | --- |
| Credential already in the environment or `~/.rpi` | Prints the source, exits `0` — no prompt |
| Nothing configured, stdin is a terminal | Runs `rpi auth login`, re-checks, exits `0` only if usable |
| Nothing configured, stdin is a pipe | Exits `3` with the commands that work (never hangs on a prompt) |

The middle row is rpi's own login — provider-neutral API key, or a registered
OAuth provider extension. The last row matters because `rpi auth login` prompts
through the terminal only: running it on a pipe would block until some outer
timeout killed it, so `--setup` fails fast instead.

Per the ACP auth-methods RFD, the method is advertised **only when the client
announced it can open a terminal** (`clientCapabilities.auth.terminal`, or the
flat `clientCapabilities.terminal` + `_meta["terminal-auth"]` shape the ACP
Registry's verifier sends). Clients that cannot run it get no method, and no
`authenticate` call is needed either way — a terminal method is out of band.

`packages/rpi-acp/tests/registry_auth.py --bridge <binary>` reproduces the ACP
Registry's handshake (and its pass/fail rule) locally.

## Capabilities and limitations

* **Stable ACP v1** with durable sessions. `loadSession` and session resume are
  advertised; sessions restore native rpi history after reconnection.
* The client-side `fs/*` and `terminal/*` methods are **not** used: rpi runs its
  own `read`/`write`/`edit`/`bash` tools in the session `cwd`. This keeps the
  agent behavior identical to the terminal.
* Prompt input: `text`, `resource_link`, and embedded `resource` blocks are
  supported (`promptCapabilities.embeddedContext: true`). Images/audio are not
  (`rpi --mode rpc` takes a single text prompt).
* Only `agent_message_chunk`, `agent_thought_chunk`, `tool_call` (with `diff`
  content for `edit`/`write`), `tool_call_update`, and `plan` updates are
  emitted.
* **`session/request_permission`** is used: when a child's plugin opens a UI
  dialog mid-run (e.g. the `rpi-permission-gate` extension confirming a `bash`
  command), `rpi-acp` maps it to a `session/request_permission` request and
  sends the user's choice back to the plugin. The child is launched with
  `--permission-gate` by default so Zed shows approval prompts; use `--bypass`
  to disable that gate, or set `RPI_ACP_NO_PERMISSION_GATE=1` to opt out of
  automatic activation only. Only `confirm`/`selector` dialogs
  with explicit `purpose: "permission"` and per-option `permission` decisions
  map to a permission request. Option titles are preserved in the returned
  answer. Ordinary `ask_user` questions use standard ACP Elicitation when the
  client advertises form support, with the legacy private extension as a
  compatibility fallback. Clients that advertise neither capability cancel
  questions. Closure, timeout, prompt cancellation and disconnect retire pending
  requests, including when the client never answers.
* Prompt templates / slash commands are not advertised as
  `available_commands_update`: `--mode rpc` does not expand prompt templates
  (only the host TUI does), so a client-side command list would be misleading.
* Authentication is signalled out of band through the terminal method above, so
  `authenticate` is a tolerant no-op rather than a real handler; rpi never asks
  the client to collect a secret.

## Questions and ACP Elicitation

`rpi-ask-user` questions use the standard ACP v1 `elicitation/create` form
when the client advertises `clientCapabilities.elicitation.form`. Current Zed
clients advertise this capability and render the request as a form. The request
contains a JSON Schema generated from the native dialog:

```json
{"jsonrpc":"2.0","id":42,"method":"elicitation/create","params":{
  "mode":"form","sessionId":"session-uuid","toolCallId":"ask-1",
  "message":"Which languages?",
  "requestedSchema":{"type":"object","properties":{
    "answers":{"type":"array","items":{"type":"string",
      "enum":["Rust","Python"]}},"minItems":1},
    "comment":{"type":"string"}},"required":["answers"]}
}}
```

The client returns an ACP form response such as:

```json
{"action":"accept","content":{"answers":["Rust","Python"],"comment":"Both"}}
```

`rpi-acp` validates the returned values against the original native dialog and
converts them back to `ui_dialog_response`. `decline`, `cancel`, timeout,
prompt cancellation, and disconnect all cancel the native question. Single
choice and input questions use an `answer` string; multi-choice questions use
an `answers` array. `allowComment` is represented by the optional `comment`
field.

For older clients that explicitly advertise
`_meta["pi-rust/questions"]: 1`, `rpi-acp` retains the private
`_rpi/request_question` / `_rpi/dialog_closed` compatibility protocol. Standard
ACP Elicitation is preferred when both capabilities are present. A client that
advertises neither question capability receives a cancellation because ACP has
no other safe generic question transport.

`elicitation/create` is an agent-to-client JSON-RPC request; the response uses
the same request ID. No ACP Rust SDK is required—the existing hand-written
JSON-RPC dispatcher serializes the standard ACP envelopes directly.


## Diagnosing a failed connection

Everything the server says goes to **stderr**, and Zed forwards agent stderr into
its own log (`agent stderr:` lines in `%LOCALAPPDATA%\Zed\logs\Zed.log`). So
a client-side "incoming transport closed" always has a matching line:

```
[rpi-acp] server stopping (stdin closed (client hung up), exit 0)
```

that line names the cause: stdin EOF, a read error, or a host shutdown. One more
line per session records the child that session spawned — the usual breakage is
that path naming a deleted binary (a cleaned `target/`):

```
[rpi-acp] session rpi-1234-1: child pid 4242 = C:\...\rpi.exe --mode rpc
```

Set `RPI_ACP_DEBUG=1` to also trace the request stream, which shows exactly how
far a client got before giving up:

```
[rpi-acp] <- initialize 0
[rpi-acp] <- session/new 1
```

On Windows the variable must be set for Zed's own environment (`setx
RPI_ACP_DEBUG 1`, then restart Zed); setting it in a terminal only affects runs
started from that terminal.

If Zed shows only the first tool or reply and never completes the turn, test
with `rpi-goal` loaded as well as ACP. A host runtime-action bridge that blocks
its Tokio worker can deadlock when goal's `TurnEnd` observer reads the session
branch. This requires updating the **rpi host**, then restarting the ACP process;
updating only the ACP plugin does not repair the host's scheduler bridge.

The optional native regression uses a local model fixture and no provider calls.
Set `RPI_TEST_HOST`, `RPI_TEST_PLUGIN` (ACP library), and `RPI_TEST_GOAL_PLUGIN`,
then run `python -m unittest discover -s packages/rpi-acp/tests -p test_native_turns.py -v`.
It checks consecutive prompts, a real tool execution followed by another reply,
and native cancellation followed by another prompt. All model traffic stays
inside the local fixture server.

## Manual smoke test
```powershell
$ext = "target\debug"   # dir containing rpi_acp.dll / librpi_acp.so
'{ "jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}} }' |
  rpi --acp --no-session --extensions-dir $ext
```

The only thing on stdout must be the `initialize` result JSON — no banner, no
log line (all diagnostics go to stderr).

## License

MIT
