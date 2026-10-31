# rpi-plan-mode

CodeX-like planning mode for the rpi Rust agent.

## Usage

```text
/plan                  # show status
/plan start            # enter planning mode
/plan <request>        # enter planning mode and submit a planning request
/plan show             # show the current or last saved plan
/plan finalize         # report whether a plan is stored (does not approve/execute it)
/plan exit             # leave planning mode
```

The model can also enter the mode directly with the `plan_mode_start` tool
when a request is complex enough to require repository exploration. This is a
read-only design and review/approval workflow, **not an orchestrator plan DAG**,
agent dispatcher, or execution scheduler. Use `todo` for routine task tracking.
While active, the exact tool allowlist is `read`, `bash`, `docs`, and
`plan_mode_complete`. The extension restricts tool names, not shell commands:
use `bash` only for non-mutating inspection; it is not a read-only sandbox.
Do not edit files or implement changes during planning.

### Model tool parameters

- `plan_mode_start`: accepts optional string `request`. Leading/trailing
  whitespace is removed; omitted, empty, or blank text means no request.
  The text appears in the tool response; unlike `/plan <request>`, this tool
  does not submit a separate user message. Re-entering an already active mode
  keeps it active rather than starting a new plan.
- `plan_mode_complete`: requires string `plan` and active plan mode. Supply a
  complete Markdown design with scope, affected files, concrete implementation
  steps, resolved decisions, risks, and validation/acceptance checks. Runtime
  trims surrounding whitespace, rejects blank text, and limits the **trimmed**
  plan to 50,000 Unicode characters (not bytes). The schema uses a nonblank
  pattern and describes the trimmed limit rather than incorrectly applying
  `maxLength` to untrimmed input.
- Supply only `request` to start and `plan` to complete. Missing/non-string
  `request` is treated as no request by runtime, but model calls should use the
  declared string type.

Submitting a plan saves it and leaves planning mode; it neither grants user
approval nor starts implementation. Respect any required approval before
making changes. `/plan finalize` only reports that a plan is stored; it is not
an approval gate or an execution command.

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
Pi's plan-mode progress convention. Entering from inactive mode with
`/plan start` or `plan_mode_start` clears the previous in-memory plan; entering
again while active does not reset it. Use `/plan show` to inspect the current
or last completed plan. Progress markers are display metadata, not scheduled
execution nodes.

The updated interactive host uses one plan panel for start, completion, `/plan`
commands, and restored entries. It renders Markdown headings, lists, and tables
without showing the raw tool arguments. Long plans have a 16-row preview;
Ctrl+T expands the full plan, and `/plan show` explicitly opens it in full.
An empty inactive plan is labelled Idle rather than Ready.

## Developer schema compatibility

Both schemas use top-level `additionalProperties: true` to tolerate host-injected
`__rpi` metadata of any shape and other extra fields; the reserved field is not
exposed as a model property. A shared runtime handler selects completion whenever
a `plan` key is present, irrespective of its value or the tool name. Keep start
calls to `request` and completion calls to `plan`; runtime dispatch is unchanged.

This follows the upstream `pi-plan-mode` state model: planning state is
owned by the extension and persisted in session entries, while the host owns
the session lifecycle and UI.
