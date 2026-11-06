# rpi-http-extension

Native `rpi-plugin-sdk` extension entry point for one multi-project node owner.
The cdylib registers `rpi --http`; the independent `rpi-node-service` executable
uses the same host. Node ownership, HTTP adaptation and custom component UI live
in reusable packages rather than Work UI.

The extension uses the existing plugin ABI and declares
`{"ownsStdio":true,"requiresAgent":false}`. Model-free service bootstrap requires
the generic host startup support for `requiresAgent:false`; older rpi versions
can load the ABI but still require model configuration before SessionStart.
No synthetic API key is a production workaround. Ordinary Agent extensions and
stdio owners that omit the declaration keep the existing startup path.

`rpi --extension <library> --http --bind 127.0.0.1 --port 13031` requires an explicit
`RPI_HTTP_PASSWORD` or `RPI_WORK_HTTP_PASSWORD`. `--http=<configuration.json>`
accepts dataDir, rpiBin, bind, port, assets and passwordEnv. Values are parsed
without changing the process environment. `--http-shutdown-stdin` enables explicit
parent supervision by stdin EOF; ordinary inherited stdin does not stop a node.

A supervising native parent selects the `--node-control-stdin` flag
and writes one bounded JSON capability frame to the private stdin pipe. The
64-hex-character capability and the supervision flag are never inherited by Agent
children. The extension then binds a second random loopback port over the **same**
owner and publishes its credential-free controlAddress in the ready frame. The
parent sends the capability in `x-rpi-node-control`; public HTTP never accepts it.
The handoff also enables supervised EOF shutdown without an extra CLI flag.
The control listener has no browser assets and authenticates even health reads.
Public sharing revocation does not disable local control, stop sessions, or
revive previous public cookies. EOF shutdown drains both listeners before the
workspace and its exclusive data lock are released.

The control capability must remain in the native parent, never argv, browser
storage, component HTML, exported connection settings or logs. A UI package
being installed does not imply that a native library was downloaded or loaded.
