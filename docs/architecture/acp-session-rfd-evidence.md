# ACP experimental session RFD evidence

## Pinned official sources

Repository: `agentclientprotocol/agent-client-protocol`, revision
`1142c83bbfa9407f9f264d1e290ffafcd54eb79e`.

- [Client-set titles](https://github.com/agentclientprotocol/agent-client-protocol/blob/1142c83bbfa9407f9f264d1e290ffafcd54eb79e/docs/rfds/session-set-title.mdx)
- [Archive/unarchive](https://github.com/agentclientprotocol/agent-client-protocol/blob/1142c83bbfa9407f9f264d1e290ffafcd54eb79e/docs/rfds/session-archive.mdx)
- [Fork](https://github.com/agentclientprotocol/agent-client-protocol/blob/1142c83bbfa9407f9f264d1e290ffafcd54eb79e/docs/rfds/session-fork.mdx)
- [Unstable v1 schema](https://github.com/agentclientprotocol/agent-client-protocol/blob/1142c83bbfa9407f9f264d1e290ffafcd54eb79e/schema/v1/schema.unstable.json)

Local source cache inspected: `D:/Temp/acp-session-rfd-evidence/`:
`docs_rfds_session-set-title.mdx`, `docs_rfds_session-archive.mdx`,
`docs_rfds_session-fork.mdx`, `schema_v1_schema.unstable.json`.
These proposals are experimental, not a claim of stable ACP support or an
upstream SDK/client release. The RFDs are normative evidence for these proposed
wire contracts; the stable schema alone does not establish their semantics.

## Private opt-in policy (not an official client capability)

The adapter requires the exact revision string at
`clientCapabilities._meta["rpi/unstable-session-rfds"]` for v1 or
`capabilities._meta["rpi/unstable-session-rfds"]` for v2 initialization.
This is **private rpi adapter policy**. The RFDs define agent capabilities; they
do not specify this client negotiation key. A missing or different revision
must not advertise or enable the RFD methods. Ordinary clients remain opted out.

Opted-in advertisement is `agentCapabilities.sessionCapabilities` in v1 and
`capabilities.session` in v2, with `{}` for `fork`, `setTitle`, and `archive`.
The shared `archive` capability covers both archive and unarchive, all three
list filters, and a boolean `archived` on each listed session.

## Implementation mapping

- `packages/rpi-acp/src/session_extensions.rs`: wire validation, normalized
  title response, loaded-session notifications, archive-close ordering, fork
  activation and private negotiation.
- `packages/rpi-extcore/src/session_metadata.rs`: supplemental organization
  metadata; bounded 16 KiB reads (including a growth-race bound), atomic
  replacement, serialized per-scope/per-ID read-modify-persist, filtered paging.
- `packages/rpi-extcore/src/session_fork.rs`: native-authoritative UUID and fork
  branch, staging, shutdown before publication, source preservation and cleanup.
- `protocol.rs`: RFD dispatch shares per-ID exclusion with load/resume/close/
  delete; checks existence under that lock. Unknown/deleted archive, unarchive,
  title and fork requests map to `-32002`. List overlays title/archive metadata
  before selecting pages; cursor binds cwd and archive filter.

Archive first cancels and closes an active native session, then commits archive
metadata. Unarchive never opens a child. Repeating either operation succeeds.
Deletion removes native histories, so orphan supplemental records cannot restore
or list deleted sessions. Listing defaults to `exclude`; `include` and `only`
are supported only after opt-in. Invalid values return `-32602`.

A persisted client title overlays native/generated titles on every list read;
subsequent native transcript changes cannot overwrite this supplemental choice.
This adapter currently has no automatic-title notification forwarding path.
Metadata is not a replacement transcript database. Like Registry, the store
requires a single writer service per filesystem scope; its per-ID locks do not
claim cross-process locking. Use `MetadataStore::update` for concurrent mutations;
`write` is a low-level full replacement, not a merge operation.

Native fork staging receives model/provider, workspace roots and permission
policy. ACP fork reopens the native fork and reapplies live source model,
thinking and compaction settings. Staging permission bypass now uses the same
process policy as ordinary ACP children rather than re-parsing argv. Forks are
active on return; no prompt is used to manufacture a fork. Unpublished staging
is owned by the helper task, including when its caller disappears; adapter
opening guards own reopened children and discard failed unregistered forks.

## Actual verification (2026-10-29)

Isolated package target: `C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review`.
Native candidate: `C:/Users/bigfish/.cargo/target/rpi-acp-native-review/debug/rpi.exe`.
No binaries were installed and no services were upgraded.

Passed:

- Core metadata tests: bounded oversized reads, concurrent mutations preserving
  both title/archive fields (50 races), replacement/restart, pre-page filtering
  and cursor binding (3 tests).
- ACP RFD unit tests: explicit negotiation/advertisement and title validation
  (2 tests).
- Ignored real-native helper test explicitly enabled with `RPI_FORK_TEST_BIN`:
  authoritative native fork UUID, parent identity, source byte preservation,
  persisted rediscovery/reopen, failed-native-start staging cleanup.
- `tests/session_rfd_protocol.py` ACP v1 real bridge + isolated real native:
  opt-out gates, title validation/normalization, unknown `-32002`, source prompt,
  fork immediately prompted without resume, inherited context/model/roots,
  unchanged source bytes, archive during a held real provider request with
  prompt settlement before archive success, closed-session rejection,
  idempotency, default/include/only filters, concurrent title/archive updates,
  restart/title/archive discovery, unarchive without loading, resumed fork
  prompt, 107 sessions over three pages, cwd/filter cursor rejection and real
  deletion excluded from all views. Four local model calls.

**Not passed:** the same v2 end-to-end run fails at its first real source prompt
with `-32603: native prompt settled without canonical user insertion`. This is
not a session-RFD pass and is not hidden by accepting an error response. v2
initialization/opt-out/title/unknown-session checks execute before that failure;
remaining v2 lifecycle/fork/paging/deletion checks are blocked. Prompt/native
insertion handling is outside this change's permitted protocol scope. Rerun
against the corrected isolated candidate before claiming v2 acceptance.

Artifacts: `D:/Temp/acp-session-rfd-run2/result.json`, `v1/frames.json`,
`v2/frames.json`, and per-version `model-inputs.json` and native histories.
The first run exposed a Windows extended-length-path spelling difference in a
test comparison; comparison was corrected to filesystem identity (`samefile`),
not by removing the workspace-roots assertion.

```sh
cargo build --locked -p rpi-acp --bin rpi-acp-bridge --target-dir C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review
cargo test --locked -p rpi-extcore session_metadata --target-dir C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review
cargo test --locked -p rpi-acp --lib session_extensions --target-dir C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review
RPI_FORK_TEST_BIN=C:/Users/bigfish/.cargo/target/rpi-acp-native-review/debug/rpi.exe cargo test --locked -p rpi-extcore native_fork_is_authoritative --target-dir C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review -- --ignored --nocapture
python packages/rpi-acp/tests/session_rfd_protocol.py --bridge C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review/debug/rpi-acp-bridge.exe --native C:/Users/bigfish/.cargo/target/rpi-acp-native-review/debug/rpi.exe --artifacts D:/Temp/acp-session-rfd-run2
```

The helper's expected invalid-source native error is intentional negative-test
output, not a successful-fork failure. The acceptance test's 105 additional
paging histories are explicitly constructed native-format fixtures; its source,
fork, prompt, archive cancellation and restart flows use actual native children.
No fake native RPC peer substitutes for those flows.
