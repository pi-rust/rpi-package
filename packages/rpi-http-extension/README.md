# rpi-http-extension

Native `rpi-plugin-sdk` extension entry point for the existing API-only HTTP/SSE
service. The cdylib registers `rpi --http`; the `rpi-http-service` executable
uses the same `rpi-http` configuration and server. It contains no frontend, no
project/workspace owner, no independent node runtime and no Work component/UI
protocol.

## Usage

```bash
rpi --http --bind 127.0.0.1 --port 3030
rpi --http=/path/to/http.json
rpi --http=help
```

`--http` starts the `rpi-http` standalone contract: `/api/v1`, SSE, cookies,
Host/Origin checks and listener lifecycle. Optional `--bind`/`--port` override
the JSON configuration. Authentication comes from `RPI_HTTP_PASSWORD` (or the
legacy `RPI_WEB_PASSWORD`), and allowed Host authorities from
`RPI_HTTP_ALLOWED_HOSTS`; this extension does not add a second authentication or
binding layer. See [rpi-http](../rpi-http/README.md) for the complete server,
configuration file, endpoint and security contract.

`rpi --http` conflicts with `--webserver`, `--web`, `--server`, `--webui` and
`--acp`, because each of those owns a service lifecycle of its own.

The standalone binary is the same API-only service without the plugin host:

```bash
rpi-http-service --project /path/to/project --rpi-bin /path/to/rpi --port 3030
```

## Lifecycle and ABI

The extension uses the existing rpi plugin ABI only:

- it declares `{"ownsStdio":true}` and does not request `requiresAgent:false`;
  normal rpi model configuration is still required, exactly as for the existing
  `--webserver` entry point;
- `SessionStart` starts the service on a dedicated runtime, `SessionShutdown`
  signals it to stop and joins the worker, and service exit is reported through the
  existing `StdioLifecycle` action;
- the native child host defaults to the running `rpi` executable. No synthetic
  credential, private control channel or extra stdin protocol is introduced.

## Session boundary

The plugin starts one `rpi-extcore::service::Service` inside the process that
loaded the plugin, but that service owns its own live session registry and native
children. It does **not** attach to, share or take over the Agent session that
loaded the extension: the Agent session and the HTTP service do not share
in-memory runs, dialogs or event streams merely because they live in the same
process. One service instance owns each live registry, and one writer per
registry file is still required. Sharing live sessions across processes would
require clients to connect to a single service instance; it is not provided by
this extension.

## Distribution

The package keeps the workspace repository identity and the standard prebuilt
extension metadata (`package.metadata.rpi.prebuilt`) for the supported platforms.
It is published as an ordinary rpi prebuilt extension; no Work UI or native
component bundle is required.

## Verification

`cargo test -p rpi-http-extension -p rpi-http --lib` covers flag adaptation and
the existing HTTP behavior. For a real installed-host ABI smoke, build the library,
set `RPI_NATIVE_BIN` and `RPI_HTTP_EXTENSION`, and run
`node packages/rpi-http-extension/tests/native-smoke.cjs`.
The smoke uses isolated registry/model configuration, checks authenticated API-only
access and the absence of Work routes, makes no model calls, and terminates only
its test process. It does not treat stdin EOF as a service shutdown command.
