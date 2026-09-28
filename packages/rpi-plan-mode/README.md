# rpi-plan-mode

CodeX-like planning mode for the rpi Rust agent.

## Usage

```text
/plan                  # show status
/plan start            # enter planning mode
/plan <request>        # enter planning mode and submit a planning request
/plan show             # show the current or last saved plan
/plan finalize         # show the saved plan path
/plan exit             # leave planning mode
```

The model can also enter the mode directly with the `plan_mode_start` tool
when a request is complex enough to require repository exploration. While
planning mode is active, the extension restricts the active tools to read-only
exploration plus `plan_mode_complete`.

```json
{
  "plan": "1. Inspect ...\n2. Implement ...\n3. Test ..."
}
```

The `plan_mode_complete` tool stores the latest plan in the current project's
`.rpi/PLAN.md`, returns it as Markdown, and restores the tool set that was active
before planning started. A new `/plan start` replaces the in-memory plan; the
last saved plan remains available through `/plan show`.

This is intentionally the first Rust implementation of the upstream
`pi-plan-mode` workflow. It keeps the important safety boundary and explicit
completion tool while leaving the richer selector/settings UI to a later
iteration.
