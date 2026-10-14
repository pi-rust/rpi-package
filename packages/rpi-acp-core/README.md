# rpi-acp-core

Shared ACP v1 server, session translation, history and stdio lifecycle for
`rpi-acp` and `rpi-webui`. This library has no exported plugin ABI entrypoint.
Users install the extensions; they do not install this crate separately.

`register_plugin` supplies the ordinary ACP/Zed plugin lifecycle.
`register_embedded` claims stdio for an embedding extension without Zed setup.
Standalone clients can run `protocol::run` directly.
