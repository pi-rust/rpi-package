# rpi-durable-core

Protocol-neutral durable business-state primitives shared by `rpi-durable` and
`rpi-extcore`.

This crate owns:

- the canonical `durable-state` discriminator;
- `rpi-durable-journal` replay;
- the canonical typed workflow, run, tool and queue schema;
- the shared durable command vocabulary used by adapters;
- compatibility decoding for `rpi-durable-runtime`.

It deliberately does **not** own a session, storage backend, worker, scheduler,
or mutation writer. It provides model types and replay only; the `rpi-durable`
plugin applies commands, validates transitions and commits entries. New business
mutations must be sent to the `rpi-durable` plugin, which is the only writer of
canonical durable entries.
