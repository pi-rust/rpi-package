# rpi-node-launcher

Native parent composition without an Agent/workspace owner. Starts an explicitly
selected rpi cdylib via the existing plugin ABI, claims the child process tree,
reads a bounded ready frame, and supplies a private Control handoff. The capability
travels only over the parent's stdin pipe; `--node-control-stdin` selects that
handoff without changing the inherited Agent environment. Keep it out of WebViews, persisted connection settings and Agent
children. EOF requests ordinary shutdown; an owned Windows job / Unix process
group bounds fallback cleanup.

`packages::Store` downloads a reviewed Bundle's native provider using an isolated
HTTPS transport, validates exact bytes and SHA-256, writes immutable content paths,
then atomically selects a version for next startup. No native file is overwritten
in place. Selection is validated again before loading. A failed transfer preserves
the prior selected provider and never grants native execution through UI install.

Tests cover incomplete/oversized ready frames, corrupt/truncated package transfer,
immutable updates, TLS download without node credentials, and an opt-in actual
native ABI load from the downloaded file. The latter needs RPI_TEST_NATIVE_DLL,
RPI_TEST_RPI_BIN and RPI_TEST_RPC_BIN pointing at isolated build outputs.
