# rpi-background-tasks

A tracked process extension inspired by `pi-background-tasks`. The single
`background_task` tool supports `start`, `status`, `list`, and `cancel`
operations. Output is redirected to a per-task log under the system temporary
directory, and every child remains tracked in the plugin process.
