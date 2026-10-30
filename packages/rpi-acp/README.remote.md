# Experimental remote ACP: HTTP/2 and WebSocket

This standalone bridge opt-in implements a **bounded ACP v1 remote profile**, not full RFD conformance or ACP v2 durability. Existing `rpi --acp` and bare `rpi-acp-bridge` remain stdio entrypoints. Building this source does not install or replace a binary.

Specification baseline: the official [Streamable HTTP & WebSocket transport RFD, pinned at `ccff4e7d`](https://github.com/agentclientprotocol/agent-client-protocol/blob/ccff4e7d/docs/rfds/streamable-http-websocket-transport.mdx), matching the cached `docs_rfds_streamable-http-websocket-transport.mdx` used for review. The RFD is a proposal; this implementation does not claim all of its reliability expectations.

## Start

```powershell
$env:CARGO_TARGET_DIR = 'C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review'
cargo build --locked -p rpi-acp --bin rpi-acp-bridge
# Supply a cryptographically random secret through the environment/secret manager.
# RPI_ACP_REMOTE_TOKEN must contain at least 32 printable ASCII bytes.
& "$env:CARGO_TARGET_DIR/debug/rpi-acp-bridge.exe" --experimental-http --bind 127.0.0.1:9231
# Alternatively, --experimental-remote starts a WebSocket-only listener.
```

The experimental switch must be the first argument. Both listeners use `/acp`, with mandatory `Authorization: Bearer <secret>` on every HTTP request/upgrade. `--allow-origin <ORIGIN>` permits one exact HTTP(S) origin without path, trailing slash, userinfo or wildcard. Absent Origin is allowed for native clients; browser Origins are otherwise denied. Standard browser WebSocket APIs cannot set this Authorization header.

`RPI_BIN` overrides the native executable; `RPI_ACP_SESSION_DIR` selects an absolute persistent transcript directory; `RPI_CODING_AGENT_DIR` selects native configuration/credentials. `RPI_ACP_NO_AUTO_REGISTER_ZED=1` disables automatic Zed registration.

## Exact HTTP profile

`--experimental-http` accepts HTTP/2 prior-knowledge cleartext requests and HTTP/1.1 WebSocket upgrades on the same `/acp` endpoint. Ordinary HTTP/1.1 requests return 505. Use an HTTP/2-capable client and keep all HTTP operations for one initialized connection on the **same TCP socket**; a connection ID is bound to that accepted socket's generation, not merely its peer address or bearer token.

- POST one JSON-RPC 2.0 object with `Content-Type: application/json`. First send `initialize` with a non-null string/integer ID and `protocolVersion: 1`, without connection/session headers. Successful initialization returns HTTP 200, JSON `result.connectionId`, and `Acp-Connection-Id`. Invalid initialization parameters return HTTP 200 with a JSON-RPC error and **no connection header**. ACP v2 initialization is explicitly rejected; it is not silently downgraded.
- Subsequent accepted POSTs return empty HTTP 202. Responses arrive asynchronously over GET SSE, correlated by JSON-RPC ID; 202 is not operation success. Request IDs cannot be reused within the connection.
- GET with `Accept: text/event-stream` and `Acp-Connection-Id` opens the connection stream. Add `Acp-Session-Id` for a session stream. Only one stream per scope is allowed (409 on duplicate). Opening-operation responses (`session/new`, `session/load`, `session/resume`, `session/fork`) use the connection stream; session updates, requests and other session-operation responses use the session stream.
- For saved history, open its session GET **before** posting `session/load`/`session/resume` with matching session header and body ID. GET validates/reserves history without starting a child or attaching it for agent operations. Claims are exclusive across connections (409); failed attachment releases the reservation even while its previous SSE response remains alive. Successful load attaches the saved session, including when its result does not repeat `sessionId`.
- `session/new` has no session header. `session/fork` requires a matching source-session header/body ID and an attached source; the new session ID is returned on the connection stream. Open a new session GET for that fork.
- DELETE terminates the **connection**, returning empty 202. Any `Acp-Session-Id` header is rejected with 400, including a known session. Missing/foreign socket connection identities cannot attach to a connection (404 for unknown/foreign IDs).
- SSE contains JSON in `data:` fields, without replay IDs. Batches, `Last-Event-ID`, and `Acp-Protocol-Version` are unsupported (501), not simulated resume/version negotiation. Malformed requests/scopes return 400; unsupported content type 415; missing SSE Accept 406. Authentication and Host/Origin checks run before normal transport processing.

## WebSocket profile

Connect to `ws://127.0.0.1:9231/acp` using an authenticated upgrade. The upgrade response carries a fresh `Acp-Connection-Id`; still send ACP v1 `initialize` first. Each text message is one JSON-RPC object, without stdio newline framing. Arrays, invalid JSON, repeated initialize and pre-initialize requests close the connection. Binary frames are ignored; ping/pong do not extend input-idle deadlines. Connection identity is diagnostic, not an authentication token.

## Security, lifecycle and bounds

A bearer holder has the bridge account's full agent privileges, filesystem/tools and native credentials. This is **not multi-tenant isolation**: no per-user session ACL, token issuance/rotation endpoint, OAuth or sandbox. No query/cookie authentication is implemented. Configured tokens are hashed; supplied credentials are hashed and compared using constant-time fixed-size digest comparison. Duplicate Authorization returns 401; invalid/duplicate Host or Origin returns 403.

Host must match the bound literal IP/port, or `localhost:PORT` on loopback. Forwarded headers are not trusted; public DNS Host names are not accepted directly. TLS is not implemented. Plain HTTP/WS exposes credentials and traffic on untrusted networks. **TLS/reverse-proxy operation is not integration-tested or guaranteed**; HTTP socket affinity and upgrade requirements must not be assumed to survive a proxy.

Each initialized connection owns its dispatcher and live children. SSE closure, underlying TCP EOF (even without any SSE GET), DELETE, shutdown, budget failure or deadline triggers cancellation/cleanup. Persisted native history survives; live runs do not. Reconnect with fresh initialize and explicit load/resume. There is no v2 resume guarantee, live event replay, guaranteed delivery, automatic retry or exactly-once execution. Avoid simultaneous transcript writers in other processes/transports; HTTP reservations are local to this listener.

Shared limits include 16 dispatchers, 1 MiB messages, 256 messages and 8 attempted session opens per connection; 32-item input queues, 64 KiB dispatcher output buffering, 10-second initial/write deadlines, 120-second input-idle deadline, one-hour lifetime and 30-second cleanup deadline before dispatcher abort. HTTP additionally bounds concurrent requests (64), aggregate header bytes (16 KiB), pending requests/server calls (32 each) and per-scope output queues (8). Overflow disconnects rather than silently dropping messages. These are transport limits, not total native/core memory bounds. Outbound activity does not refresh input idle.

## Verification

`tests/remote_http.rs` covers negotiation/error headers, security, socket-bound identity, stream exclusivity, strict connection DELETE, Unicode streaming, cancellation, GET-before-load without child startup, failed-attachment claim release, persisted replay, scoped fork and TCP EOF cleanup without SSE. `tests/remote_transport.rs` covers WebSocket security/framing, budgets, queue overload, native streaming/cancellation, EOF and persisted reconnect. Private `transport::tests` cover shortened initial/idle/lifetime deadlines.

```powershell
$env:CARGO_TARGET_DIR = 'C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review'
cargo build --locked -p rpi-acp --example remote_rpc
cargo test --locked -p rpi-acp --test remote_http --test remote_transport
cargo test --locked -p rpi-acp --lib transport::tests
```

The deterministic `examples/remote_rpc.rs` peer writes/reads completed native transcript messages; these tests are not real-host/model validation. Sustained outbound socket saturation, forced cleanup-abort recovery, TLS/proxy deployments, Linux/macOS runtime behavior and full RFD conformance remain unverified.
