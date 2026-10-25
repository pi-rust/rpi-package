# rpi-http

HTTP/SSE protocol adapter for `rpi-extcore`. It provides a reusable Axum router and an API-only executable. It contains no frontend resources and does not depend on the plugin SDK.

## Standalone service

From the repository root, with the native host installed:

```bash
cargo run --locked -p rpi-http -- --project /path/to/project --rpi-bin /path/to/rpi --port 3030
```

This serves `/api/v1`, including projects, sessions, commands, snapshots, SSE, model/provider operations and file/Git previews. `/` and `/assets/*` return 404. Use `rpi --webserver` from the `rpi-web` extension when you want the bundled browser workspace.

`--help` lists the supported options. `--port 0` selects an available port and startup output shows the actual address. An explicit JSON file is optional:

```json
{
  "project": ".",
  "rpiBin": "/absolute/path/to/rpi",
  "port": 3030,
  "registry": "state/sessions.json",
  "sessionDir": "history"
}
```

```bash
cargo run --locked -p rpi-http -- --config /path/to/http.json --port 3031
```

JSON paths are relative to the file, except a bare executable name, which is resolved on PATH. CLI options override JSON regardless of their order. This new standalone entrypoint loads only an explicitly selected configuration file; the existing `rpi-web` global/project configuration discovery remains in Web.

## Authentication and compatibility

Use `RPI_HTTP_PASSWORD` for access authentication and `RPI_HTTP_ALLOWED_HOSTS` for allowed Host authorities. Both fall back to their legacy `RPI_WEB_*` names. The Web entrypoint keeps `RPI_WEB_*` precedence.

The existing `/api/v1` payloads, cookie authentication, `X-Rpi-Web: 1` mutation marker, SSE event names/IDs and `Last-Event-ID` behavior are preserved. Log in with `POST /api/v1/auth`, retain its cookie, and include the mutation marker on POST/DELETE requests. Model API credentials remain separate and are handled by the native host.

For compatibility the default registry remains `~/.rpi-web/sessions.json`. Run one backend writer per registry. Separate standalone instances can select their own `--registry` paths.

The durable control plane is available for an opened session at
`GET /api/v1/sessions/{id}/durable` and
`POST /api/v1/sessions/{id}/durable`. Durable changes can be streamed from
`GET /api/v1/sessions/{id}/durable/events`; it emits `durable_ready` for a new
subscriber and `durable_change` events with the persisted state revision. Use
`Last-Event-ID` or `?after=` to reconnect. A `resync_required` event means the
bounded replay window no longer covers the requested revision.

The POST body selects a coordinator operation without duplicating its state
machine:

```json
{"operation":"enqueue","text":"run tests"}
{"operation":"claim","itemId":"item-1"}
{"operation":"finish","itemId":"item-1","status":"completed","note":"passed"}
```

Supported operations are `start_run`, `settle_run`, `recover_run`, `enqueue`,
`claim`, `finish` and `cancel`. Durable is an optional native capability: if
`rpi --mode rpc` does not provide branch operations, ordinary sessions still
work and these endpoints return `503 durable_unavailable`. Tool side effects
are never replayed, and the queue API is coordination only; it does not start a
background scheduler.

## Embed the API

`router(core, &config, shutdown_receiver)` builds an API router around an existing `Arc<rpi_extcore::service::Service>`. Supply the actual bound port in the configuration so Host checks use the advertised endpoint. Rust callers and HTTP clients then observe the same service state.

`serve_until(config, frontend_router, on_listen, stop)` owns a listener and a core instance with bounded shutdown; Web uses this to combine the API with its own frontend routes. `run(config)` starts the API-only service and stops on Ctrl+C.

## Verification

```bash
cargo test --locked -p rpi-extcore -p rpi-http
python packages/rpi-http/tests/native_smoke.py --http /path/to/rpi-http --rpi /path/to/rpi
```

The native smoke test exercises authentication, metadata, snapshots, SSE, restart recovery and the absence of frontend routes without model calls.
