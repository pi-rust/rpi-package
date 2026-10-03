# rpi-plan-mode

CodeX-like planning mode for the rpi Rust agent.

## Usage

```text
/plan                  # show status
/plan start            # enter planning mode
/plan <request>        # enter planning mode and submit a planning request
/plan show             # show the current or last saved plan
/plan finalize         # confirm the session plan
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

The `plan_mode_complete` tool stores the latest plan in a `plan-mode` session
entry, returns a compact plain-text outline, and restores the tool set that was
active before planning started. The current session branch is the source of
truth, so fork, tree navigation, and resume restore the matching plan state.

Numbered steps are shown with progress markers. The extension understands
`[DONE:1]` markers and checkbox forms such as `- [x] 2. Run tests`, matching
Pi's plan-mode progress convention. A new `/plan start` replaces the in-memory plan; the
the current branch plan remains available through `/plan show`.

This follows the upstream `pi-plan-mode` state model: planning state is
owned by the extension and persisted in session entries, while the host owns
the session lifecycle and UI.
