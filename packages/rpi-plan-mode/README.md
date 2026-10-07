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
entry, returns the complete Markdown plan with an outline in result details, and restores the tool set that was
active before planning started. The current session branch is the source of
truth, so fork, tree navigation, and resume restore the matching plan state.

The structured outline includes progress markers. The extension understands
`[DONE:1]` markers and checkbox forms such as `- [x] 2. Run tests`, matching
Pi's plan-mode progress convention. A new `/plan start` replaces the in-memory plan; the
current branch plan is available through `/plan show`.

The updated interactive host uses one plan panel for start, completion, `/plan`
commands, and restored entries. It renders Markdown headings, lists, and tables
without showing the raw tool arguments. Long plans have a 16-row preview;
Ctrl+T expands the full plan, and `/plan show` explicitly opens it in full.
An empty inactive plan is labelled Idle rather than Ready.

This follows the upstream `pi-plan-mode` state model: planning state is
owned by the extension and persisted in session entries, while the host owns
the session lifecycle and UI.
