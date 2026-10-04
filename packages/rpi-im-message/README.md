# rpi-im-message

Persistent Feishu/Lark messaging through the official Rust SDK WebSocket long
connection. The extension keeps the long connection in a dedicated server
runtime and exposes JSON actions through one rpi tool:

`start`, `status`, `list`, `receive`, `send`, and `stop`.

The `send` action accepts `content.type` of `text`, `markdown`, or `card`.
`markdown` is rendered through a CardKit 2.0 card and preserves the original
Markdown instead of degrading it into plain text or rich-text tags.

Configuration is loaded from `RPI_IM_CONFIG`, project `.rpi/im.json`, or the
global `~/.rpi/agent/im.json` (in that order). Each profile may provide either
`appSecret` directly or `appSecretEnv` with the name of an environment variable;
configure only one of them. Environment variables are recommended for shared
machines and source-controlled configuration.

Example:

```json
{
  "defaultProfile": "feishu-main",
  "profiles": {
    "feishu-main": {
      "provider": "feishu",
      "domain": "feishu",
      "appId": "cli_xxx",
      "appSecretEnv": "RPI_FEISHU_APP_SECRET",
      "transport": "long_connection",
      "allowChats": ["oc_xxx"],
      "mentionRequired": true,
      "autoReply": true,
      "autoReplyModel": "huoshan-copy/deepseek-v4-flash-ga-260731",
      "autoReplyTimeoutSeconds": 600,
      "ackReaction": true,
      "autoReconnect": true,
      "maxQueueSize": 256
    }
  }
}
```

For a local-only configuration, `appSecret` can be used instead:

```json
{
  "appId": "cli_xxx",
  "appSecret": "your-secret",
  "transport": "long_connection"
}
```

Do not commit a configuration containing `appSecret` to source control.

The Feishu app must enable bot messaging, subscribe to `im.message.receive_v1`
over long connection, and grant the required message permissions.

When `autoReply` is enabled, incoming text messages are passed to the active
rpi Agent through the ABI v2 runtime bridge. The generated final text is sent
back to the same Feishu conversation as a **CardKit 2.0 card**: the raw
Markdown goes into the card's `markdown` element and Feishu renders headings,
bold/italic, strikethrough, lists, code blocks, tables and links natively.
This follows the Hermes (`hermes-lark-streaming`) channel strategy. A light
pre-pass demotes headings to the levels Feishu renders, escapes stray `*` that
Feishu would mis-pair as emphasis, drops image refs that are not `img_` keys and
splits very long replies across several cards.

Auto replies use a Feishu/Lark channel system prompt: the model returns only the
user-facing text, without JSON/API envelopes or a `post`/card envelope. The
same channel rules are applied to the ABI runtime path and to the headless
`rpi --print` fallback. It defaults to `false`; without it,
messages remain available through the `receive` action only. In headless mode,
when the host has not built an Agent harness yet, the extension falls back to a
bounded `rpi --print --no-extensions` child process so automatic replies still
work without opening the TUI. The fallback uses a stable rpi session for each
Feishu conversation and serializes model runs, preserving conversation context
while avoiding concurrent session writes and provider rate spikes.

Set `autoReplyModel` to an explicit `provider/model` id when the rpi default
model is unavailable from the headless environment. If omitted, the fallback
uses the normal rpi model selection.

`autoReplyTimeoutSeconds` controls both the model request timeout and the
fallback process wait timeout. It defaults to 600 seconds (10 minutes) and
accepts values from 1 to 3600.

With `ackReaction` enabled (the default), every accepted incoming message is
immediately acknowledged with one random Feishu reaction: `了解`, `敲键盘`, or
`冲！`. Set it to `false` to disable this acknowledgement.

## Headless startup

The extension registers two ABI v2 CLI flags. Start the server without opening
the TUI with:

```text
rpi --im-message-server
```

The optional profile flag overrides `defaultProfile`:

```text
rpi --im-message-server --im-profile feishu-main
```

Headless mode keeps the `rpi` process attached to the long connection. Stop it
with `Ctrl+C`.
