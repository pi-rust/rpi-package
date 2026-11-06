# rpi-components

Versioned, transport-neutral **custom UI packages** and a node-owned capability
registry. This crate does not start another agent/session owner and does not
define or replace rpi's native extension ABI.

Packages contain a manifest, complete custom HTML documents and a content digest.
Settings panels are loaded only when opened. The same package contract is used
for bundled and downloaded panels. `Host` validates and persists packages using
atomic publication without overwriting existing identities. Requested capability
names are separate from explicit installation grants. A host capability's local
owner restriction cannot be weakened by a package manifest.

The client runs each document in `sandbox="allow-scripts"` without same-origin
access. The shared bridge applies a restrictive CSP, establishes a source-checked
MessageChannel, bounds requests, forwards declared actions, supplies theme tokens
and closes channels when the frame is removed. Host callbacks must validate their
arguments and implement policy; UI checks are not authorization.

The SHA-256 is a content identity/integrity check, not publisher authentication.
Installing a user-supplied package requires explicit review of its identity and
grants. Downloads must not carry node cookies or credentials.

Current v1 intentionally supports self-contained HTML entry documents and the
`settings` slot. An optional native runtime descriptor supplies per-platform
HTTPS URLs, exact sizes and hashes. rpi-node-launcher separately downloads and
stages native code after explicit trust; a selected provider is loaded through
the existing rpi ABI on next startup. This never happens as a side effect of UI
installation. Publisher signatures, rollback UI and additional slots are outside
the v1 protocol. An installed UI package is never reported as a loaded native
extension.

Run `cargo test -p rpi-components` for persistence, validation and capability tests.
