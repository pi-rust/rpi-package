# rpi-http-component

Remote-access control UI and action contract, composed with a node's existing
owner. The custom layout, state presentation and request validation live here;
the consuming application only binds `RemoteAccessOwner` and mounts a generic
component panel.

`register()` supplies the same UI bundle used by downloaded packages. The status
snapshot excludes passwords, authorization tickets and model credentials. Read
status is available to authenticated remote clients. Changing sharing policy
requires the local owner, enforced by the host capability registry.

Export a distributable UI bundle:

```sh
cargo run -p rpi-http-component --bin export-component > component.json
```

This UI package is composed with rpi-http-extension's real `rpi --http` entry.
The HTTP adapter and owner live in shared native packages. Local status and
policy calls use a private parent control listener; public clients can read
status but cannot change host sharing policy or issue owner authorization.

The dedicated panel also edits listener IP/port, public origin and a new node
password. Configuration is staged for next startup and never restarts the live
owner. The node stores only the password verifier; read status contains no
password material. Form actions use the component bridge without granting
sandboxed form navigation or direct network access to the document.

Pass a native descriptor JSON file to export-component to produce a combined
UI/native install bundle. The downloader verifies its platform file separately;
UI installation is never a native code execution grant.
