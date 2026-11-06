# rpi-node-host

Native composition of one locked node owner, its component registry and protocol
entrances. The same Arc<LocalWorkspace> backs public HTTP and private local
control; neither entrance creates another registry. Work UI consumes a client.

The guard retains the data lock until listener operations drain and workspace
children stop. A public listener failure is recoverable through private control
when supervised by the rpi extension; ordinary public headless startup still
fails explicitly. Headless startup retains its explicit-password/EOF opt-in
behavior. Configuration and sharing preference files belong to the node.

The remote-access component exposes local-only sharing, authorization and saved
listener configuration. IP, port, public URL and a password verifier are atomically
saved for next startup, leaving current connections and work alone. Password input
is never returned in status. Explicit launch overrides take precedence over saved
configuration, then environment defaults. Invalid listener settings fail closed
on public HTTP and can be repaired locally without opening a public bypass.
