# rpi-durable-core

Protocol-neutral durable business-state primitives shared by `rpi-durable` and
`rpi-extcore`.

This crate owns:

- the canonical `durable-state` discriminator;
- strict current-version `rpi-durable-journal` snapshot/delta replay;
- the canonical typed workflow, run, tool and queue schema;
- the shared durable command vocabulary used by adapters.

Only native `custom` entries with `customType: "durable-state"`, a non-empty
native `id`, format `rpi-durable-journal` and version `1` are decoded. Raw state
payloads and the former `rpi-durable-runtime` discriminator are not supported.
Use `DurableState::try_from_branch` to surface malformed data; there is no
lossy `from_branch` decoder or raw-state `entry()` writer helper.

Replay starts at the newest valid, self-contained snapshot (whose state revision
must match its record). The input must already be filtered to `durable-state`:
every native envelope must have the exact type/discriminator, a non-empty string
`id` and a `data` field. Envelope errors always fail, even before the newest
snapshot. Only obsolete malformed payload prefixes may be discarded; malformed
payloads after that snapshot fail closed. Each suffix delta
must increment the revision exactly once and reference the preceding native
entry via `baseEntryId`; missing mutation paths, ambiguous tool updates and
invalid collection bounds are errors. Journal change fields follow the plugin's
current persisted schema, including snake_case fields such as `step_id` and
`call_key` (distinct from the camelCase native branch-query parameters).
The public state/checkpoint structs retain their existing serde field defaults;
strict journal replay does not introduce a stricter public state schema.

It deliberately does **not** own a session, storage backend, worker, scheduler,
or mutation writer. It provides model types and replay only; the `rpi-durable`
plugin applies commands, validates transitions and commits entries. New business
mutations must be sent to the `rpi-durable` plugin, which is the only writer of
canonical durable entries.
