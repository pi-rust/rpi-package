# rpi-mcp-core

ABI-free, caller-owned MCP runtime extracted from `rpi-mcp-adapter`.
Dependencies flow from the plugin adapter to this core, never back to the SDK.

## Embedding API

- Construct `McpRuntime::new()` (or `Default`). Each instance owns its connection
  registry, subscriptions, pending updates, notification dispatchers and children.
  Share an instance explicitly with `Arc<McpRuntime>` when desired; dropping its
  last owner synchronously cancels operations, closes transports and joins workers.
- Pass `RequestContext { scope: String, cwd: PathBuf }` to
  `call(&context, &params, &control)`, `list(...)`, and `resources(...)`.
  Params are the existing JSON tool arguments: `configPath` + `server`, or `url`,
  plus operation-specific fields. Results are `Result<serde_json::Value, String>`.
  No plugin context fields or SDK environment variables are read by core.
- `call` takes `tool` and optional object `arguments`; `list` accepts `list` or
  `close`; `resources` accepts `list`, `read`, `subscribe`, or `unsubscribe`.
  Connections are keyed by scope, canonical config identity/server (or URL),
  within this instance only. `Control::cancel()` interrupts owned operations.
- `prompts_list(...) -> PromptListResult`, `prompts_get(...) -> PromptResult`, and
  `resource_templates_list(...) -> ResourceTemplateListResult` take the same
  explicit `RequestContext`, JSON target params, and `Control`. `prompts(...)`
  dispatches `action: "list" | "get"`; `resources(...)` additionally delegates
  `action: "templates/list"` to the typed template API. No adapter edits or prompt
  tool registration are included. Initialization still requests **2025-06-18**,
  not MCP 2026; existing supported version negotiation is unchanged.
- Prompt get uses `name` and optional string-valued `arguments`. Identifiers must
  be nonblank, contain no controls and fit 200 UTF-8 bytes; they are never treated
  as paths or instructions. Descriptors, arguments, messages, content blocks and
  templates retain unknown fields through flattened maps, including `_meta`,
  model hints, annotations, image/audio data and embedded resources. No prompt
  content is flattened or silently truncated. `uri_template` serializes as
  `uriTemplate`; content retains its wire `type` and structured fields.
- New catalog lists fetch **one page by default**. Supply `cursor` to resume,
  or `maxPages: 1..100` for bounded aggregation; a remaining `nextCursor` is
  returned, not dropped. Repeated cursors are rejected within a call (including
  the input cursor). Cross-call cursor history belongs to the caller. Aggregated
  results retain original pages in `pageResults`, preserving per-page metadata;
  a collision with that reserved aggregation field fails explicitly.
- New operations enforce `maxResponseBytes` (default 1 MiB, maximum 16 MiB),
  at most 16,384 list items and 8,192-byte cursors. Aggregation also counts its
  preserved raw pages toward the result byte limit. Exceeding a limit errors,
  never truncates. Existing transport wire limits remain 16 MiB. Cancellation,
  EOF, shutdown and reconnect use existing instance-owned session cleanup; new
  operations share one deadline across lock/init and page requests, with the
  transport's existing whole-second timeout granularity.
- `take_resource_updates(Option<&str>)` drains neutral
  `ResourceUpdate { scope, key, uri, text }` records. Selecting a scope discards
  other scopes; `None` drains all. `deliver_resource_updates(current_scope,
  append)` provides bounded, subscription/revision-checked retry on failed
  delivery. The caller owns message formatting and the safe delivery boundary.
- `shutdown_connections()` explicitly releases current connections; the runtime
  remains usable afterward. `list` with `action: "close"` closes one target.
- Convenience methods `call_from_config`, `call_from_config_with_timeout`,
  `discover_from_config`, `close_from_config`, and `resources_from_config` use
  `RequestContext::default()` (`library` scope and process current directory).
  Embedders requiring session isolation should use the explicit-context methods.
- Existing configuration, JSON-RPC, discovery, stdio and Streamable HTTP helpers
  remain available. The operation methods currently live on public
  `runtime::RuntimeState`, reached through `McpRuntime`'s `Deref`; state fields are
  private and there is no global registry in core.

The plugin adapter retains ABI tool/content mapping, SDK session/cwd mapping,
TurnEnd presentation and SessionShutdown integration. HTTP resource push,
sampling, elicitation, default-on OAuth and dynamic tool
registration remain out of scope. MCP wire identity strings retain
`rpi-mcp-adapter` for compatibility.

### Optional HTTP credential provider

`McpRuntime::with_auth(provider)` and `HttpTransport::with_auth(...)` opt in to a
trusted, caller-owned `CredentialProvider`; default construction is unchanged.
Core binds leases to the runtime namespace, scope, canonical cwd, target, resource
and configuration generation. Close and shutdown retire identity admission and
release connection bookkeeping; reopening uses a fresh generation. Config edits
retire the previous generation before it can acquire another session.

A provider's `CachedOnly` lookup must never prompt. A valid Bearer 401 challenge
allows at most one credential acquisition and one identical request replay.
Every rejected local lease, including a final 401 or malformed/missing challenge,
is reported through the noninteractive `rejected_lease` callback. Providers must
invalidate only that exact binding/lease ID, not a newer concurrent grant.
Providers own cache isolation, refresh/single-flight, discovery and issuer/account
policy, real OAuth exchange, and drop-safe cooperative cancellation.

Cancellation notifications (200 ms) and session DELETE (1 second) are bounded,
best-effort cleanup. They attach only a still-valid local lease (or configured
static Authorization), never call the provider, prompt, refresh or retry. Explicit
close clears the local lease even if DELETE fails; it neither guarantees remote
deletion nor evicts a provider cache/revokes an account grant. Namespace/generation
IDs are process-local; persistent providers must add cross-process isolation.

## Managed HTTP OAuth public client (opt-in)

`oauth::OAuthProvider` implements the existing `CredentialProvider` contract.
Construct it with `OAuthOptions`, an `OAuthNetwork`, and an
`Arc<dyn AuthorizationInteraction>`, then inject it via `with_auth`. This is
neutral MCP HTTP OAuth, **not** ACP native-agent/model-provider authentication.
Default runtimes remain header-only. No AuthProcess, browser launcher, native
credential file, transcript, plugin, ACP types or persistent secret store is used.

- Each `ResourcePolicy` pins an exact normalized resource, exact issuer, minimal
  nonempty scopes, and either a pre-registered public client ID or explicit DCR
  permission. Unknown authorization servers are never automatically selected.
  DCR permission is trusted, pre-provisioned configuration approval of registration
  side effects for that pinned resource/issuer **before** any browser interaction.
  It must not be enabled from challenge/metadata input. With no client ID and no
  such approval, construction fails; there is no silent registration. Browser
  `Accepted`/`Declined` controls authorization, not this prior registration approval.
- `OAuthOptions::authorization_origins` separately approves initial browser
  destinations. `NetworkPolicy` must explicitly approve every discovery, DCR and
  token destination; metadata cannot authorize a new origin. The trusted browser
  interaction controls subsequent browser navigation (not the OAuth HTTP client).
- Interaction receives the actual authorization URL, binding, issuer, scopes,
  control and whole-operation deadline. Its boxed future returns only `Accepted`
  or `Declined`. Acceptance is **not a token**: the provider waits for its real
  loopback callback, checks state/optional issuer, and exchanges the code using
  a fresh 32-byte random S256 verifier and resource parameter.
- Provider-owned Tokio listener binds only `127.0.0.1:0`, fixed `/oauth/callback`,
  exact Host, 8 KiB request-header limit and one callback attempt (the first invalid
  request terminates the flow fail-closed). Bodies, transfer encoding, fragments,
  duplicate query keys and percent-decoded controls are rejected. There are no
  spawned listener tasks; dropping/cancelling the future closes the socket.
  Pre-registered clients must permit this RFC8252 loopback ephemeral-port pattern.
- Cache keys are complete `AuthBinding`s. Tokens/refresh grants stay in memory;
  secret-bearing Debug output is redacted. Per-binding async gates work across
  distinct caller runtimes. `CachedOnly` never discovers, refreshes or prompts.
  Exact lease rejection preserves refresh capability without invalidating a newer
  lease. Ambiguous refresh outcomes discard the possibly consumed refresh grant;
  only `invalid_grant` allows one new consent flow under the original deadline.
- Cache admission is bounded (`max_bindings`, 1..4096); at capacity a new binding
  fails closed, rather than evicting an active gate. Embedders must call the
  noninteractive `forget_binding(&binding)` when retiring a binding, or `clear()`
  to retire all current bindings. These free admission capacity, clear secrets and
  cancel pending work/waiters in those slot generations, including code/token
  exchanges. Old futures cannot commit to a forgotten slot or resurrect it after
  the same binding is admitted again. They perform no network I/O, prompt or remote
  revocation; already-returned leases remain the caller's responsibility. Dropping
  an operation future discards its pending grant; no background tasks are spawned.
  Known token lifetimes must be integer seconds in 1..86400; reuse subtracts
  `min(30 seconds, lifetime/10)`. Unknown expiry is bounded to five minutes with
  the same skew. Returned token scopes must equal the configured requested set:
  neither missing scopes nor excess server-granted privileges are accepted.
- HTTP loopback metadata/AS/token fetches require the explicit
  `allow_development_origin` exception and matching browser-origin permission.
  This is a development profile, not production HTTPS conformance. The tests use
  this profile; they do not prove TLS or a live external account. Private HTTPS
  grants use the network policy's exact origin/address exception.

Validation evidence and intentionally uncovered scenarios are recorded in
`C:/Users/bigfish/.cargo/target/acp-oauth-provider.md`. This module alone does not
complete broader ACP OAuth integration or the full security test matrix.

## Validation

```text
cargo test --locked -p rpi-mcp-core -p rpi-mcp-adapter
cargo clippy --locked -p rpi-mcp-core -p rpi-mcp-adapter --all-targets -- -D warnings
cargo tree --locked -p rpi-mcp-core
```

Workspace MSRV remains Rust 1.78. Process ownership uses `process-wrap =8.2.2`
(declared Rust requirement 1.77), with Windows Job Objects / Unix process groups.
