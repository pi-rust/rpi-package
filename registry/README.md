# ACP Registry packaging

Everything needed to publish rpi as an entry in the
[Agent Client Protocol registry](https://github.com/agentclientprotocol/registry),
which Zed surfaces as the **Agent Servers** store.

The registry only accepts two distribution shapes:

* `npx` — an npm package, or
* `binary` — a per-platform archive with a `cmd` entry point and a `sha256`.

A `cdylib` rpi extension cannot be published, so the registry entry ships the
**standalone bridge binary** [`rpi-acp-bridge`](../packages/rpi-acp/README.md#standalone-binary-acp-registry)
instead. It speaks ACP on its own stdin/stdout and spawns `rpi --mode rpc`
itself, so it needs no host extension loading.

## Produce the archives

Per platform (run on that platform — the archive contains a native binary):

```bash
task pack-acp
```

This stages `dist/rpi-acp-<slug>/` and builds a **flat** archive
(`dist/rpi-acp-<slug>.zip` on Windows, `dist/rpi-acp-<slug>.tar.gz` elsewhere),
then prints the new `sha256` and a ready-to-paste `distribution.binary` stanza.

`task pack-acp` names archives with the local cargo slug; the registry keys use
Go's `GOOS-GOARCH` spelling. Map them when filling in `agent.json`:

| archive (`pack-acp`)          | registry key      |
| ----------------------------- | ----------------- |
| `rpi-acp-linux-x86_64`        | `linux-x86_64`    |
| `rpi-acp-linux-aarch64`       | `linux-aarch64`   |
| `rpi-acp-macos-x86_64`        | `darwin-x86_64`   |
| `rpi-acp-macos-aarch64`       | `darwin-aarch64`  |
| `rpi-acp-windows-x86_64`      | `windows-x86_64`  |

## Fill in `rpi-acp/agent.json`

`rpi-acp/agent.json` is the entry to submit. It is **generated**, not hand-written:

```bash
python3 registry/render-agent-json.py --bundle-tag rpi-packages-<sha12>
```

The bundle release already publishes every value the entry needs — the per-platform
`acp_bridge` archive URL and `sha256` live in that release's `rpi-packages.json` —
so rendering from it removes the copy-by-hand step where a wrong checksum or a
stale version used to slip through. The version comes from
`packages/rpi-acp/Cargo.toml`, and only the platforms the release actually built
are listed (the schema allows a subset).

`--check` re-renders and exits non-zero when the committed file differs, which is
how CI keeps it in step with the last release.

`icon.svg` is served from the registry CDN
(`https://cdn.agentclientprotocol.com/registry/v1/latest/rpi-acp.svg`), so it is
not referenced from `agent.json`. It is the site mark (`rpi.laofu.online`) scaled
to the registry's required 16×16 and reduced to a single `currentColor` fill —
see *Icon* below.

## Authentication

The registry lists only agents whose `initialize` advertises an `agent` or
`terminal` auth method, and it verifies that by launching the configured agent
program itself. rpi advertises one `terminal` method:

```json
{
  "id": "rpi-credentials",
  "name": "Log in from the terminal",
  "type": "terminal",
  "args": ["--setup"]
}
```

`--setup` runs `rpi auth check` first (a credential in the environment or in
`~/.rpi` already counts, and exits `0` with no prompt), falls back to
`rpi auth login` when stdin is a terminal, and only then re-checks before
exiting `0`. On a pipe with nothing configured it fails fast with the exact
commands that work rather than hanging on a prompt it cannot reach. See
[`packages/rpi-acp/src/auth.rs`](../packages/rpi-acp/src/auth.rs).

`packages/rpi-acp/tests/registry_auth.py --bridge <binary>` reproduces the
registry's handshake locally, and CI runs it on every change.

## Icon

The registry requires a 16×16, square, monochrome SVG that uses `currentColor`
(hardcoded fills fail validation) and forbids HTML comments. The site favicon is
32×32 with a dark rounded background and a highlighted middle bar; `icon.svg`
keeps the three-bar geometry, scales it to fit inside 16×16, drops the plate so
the mark themes in light and dark mode, and uses one `currentColor` fill.

## Before opening the PR

The registry is curated. Checklist:

- [x] Release assets uploaded for every listed platform
      (`rpi-acp-<slug>.{zip,tar.gz}`), each `sha256` rendered from the release
      manifest rather than transcribed.
- [x] `rpi-acp-bridge` starts and answers `initialize` on the release binary
      (the registry's own `verify_agents.py` passes against it).
- [x] `LICENSE` is present (MIT, `license_url` set).
- [x] `icon.svg` satisfies the registry's validator (16×16, square, single
      `currentColor` fill, no comments).
- [x] `authMethods`: the `terminal` method above, present only when the client
      offered a terminal-auth capability, per the ACP auth-methods RFD.

## Submit

1. Fork `agentclientprotocol/registry`.
2. Add `rpi-acp/agent.json` and `rpi-acp/icon.svg` (copy from this folder).
3. Open a PR; the registry validates the schema and re-hashes the archives.

Until it is accepted, the local path still works: register the binary (or the
`rpi --acp` extension) under `agent_servers` in Zed's `settings.json` — see the
[package README](../packages/rpi-acp/README.md#configure-zed).
