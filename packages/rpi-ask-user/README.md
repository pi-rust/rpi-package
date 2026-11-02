# rpi-ask-user

Interactive questions for workflows that need an explicit user decision.

The ABI v2 `ask_user` tool normalizes the native Pi shape (`question`,
`context`, option objects, `allowMultiple`, `allowFreeform`, `allowComment`,
`displayMode`, `timeout`, and `suggest`) as well as the older `questions[]`
alias. Each question keeps a stable `id`, optional `header`, context, and
option descriptions.

When the host exposes the rpi UI runtime action, the tool sends a unique
`requestId`/`toolCallId`, returns `Pending`, and waits for the TUI answer before
returning `Done`. Freeform questions use an input editor; questions with
options use a selector. Timeout and cancellation close the pending UI request.
The tool result contains only a readable answer summary and structured answer
details, never the transport JSON.

Hosts without an interactive UI receive the explicit error
`ask_user requires an interactive UI`. The registered `/ask_user` command is
also available for hosts that expose the native command selector directly:

```text
/ask_user {"question":"Which target?","options":["Linux","Windows"]}
```

## Arguments and defaults

Use this tool when a user decision is missing; do not infer an answer or treat
a UI request as approval. It requires an attached interactive UI and waits for
real user input. All schema fields remain optional for compatibility, but the
parser requires effective question text or confirmation text as described below.
Unknown top-level host metadata, including `__rpi`, is accepted.

| Argument | Meaning / default |
| --- | --- |
| `type` | `question` (default) or `confirm`. |
| `question` | In question mode, effective text must be nonblank and 1–500 characters **after trimming**. Supplies missing item text in `questions[]`. For confirm, fallback for absent/blank `summary`, without the question-mode length limit. |
| `questions` | Optional batch of 1–3 objects, shown sequentially instead of a single top-level question. Each uses its own `question`/`prompt`, then top-level `question`/`prompt`. |
| `options` | Optional array, at most 20 choices. Omitted/empty means an input editor; otherwise a selector. Top-level options are **not inherited** by batch items. |
| `id` | Optional trimmed answer identifier; default `q1` for a single question, `q1`/`q2`/`q3` per batch index. Top-level id is not inherited. |
| `context`, `header` | Optional trimmed background and heading. Batch items fall back to top-level values. |
| `allowMultiple` | Default `false`; enables multiple selector choices. |
| `allowFreeform` | If unset on both item and top level: `true` with no options, `false` with options. |
| `allowComment` | Default `false`; allows an extra comment. |
| `displayMode` | `overlay` or `inline`; omitted delegates presentation to the host. |
| `timeout` | Positive integer seconds per question; omitted means no plugin deadline. Timeout cancels the pending UI request and errors, not an implicit answer. |
| `suggest` | Optional trimmed input suggestion/placeholder, not a selected answer. |
| `summary` | In confirm mode, nonblank confirmation text (fallback `question`); no question-mode length limit. Ignored in question mode. |

Batch items inherit `header`, `context`, `allowMultiple`, `allowFreeform`,
`allowComment`, `displayMode`, `timeout`, and `suggest` from the top level unless
provided on the item. Blank strings are treated as absent for these text fields.
Options and ids never inherit. A batch item may omit `question` when effective
text is supplied by the top level, so it is not schema-required on every item.

Option objects should use `title` and optional string `description`. Both
strings and effective object titles must remain nonblank after trimming;
descriptions are trimmed and may be empty. The schema leaves `title` optional
so legacy title aliases can pass host validation before parsing (see below).
The model-facing schema advertises only canonical arguments and string/object
choices. Batch questions and choices are fully inline (no `$ref`/`$defs`), so
provider schema conversion preserves their shape. Runtime compatibility is
unchanged.

`type=confirm` uses only trimmed `summary` (fallback `question`) and a fixed
Yes/No selector with id `confirm`. It ignores `questions`, options, context,
header, UI flags, display mode, suggestion and timeout; there is no plugin
confirmation deadline. Cancellation still cancels the pending request and
returns an error. `summary` does not fall back to `prompt` in confirm mode.

```json
{
  "context": "Release build",
  "questions": [
    {"id":"target","question":"Which target?","options":[{"title":"Linux","description":"glibc"},{"title":"Windows"}]},
    {"id":"notes","question":"Any release notes?","suggest":"Optional notes"}
  ]
}
```

```json
{"type":"confirm","summary":"Deploy this release now?"}
```

The tool result contains a readable summary and `details.answers` entries with
`id`, `question`, `values`, `text`, and `confirmed`; there is no plugin cap on
answer text length. `confirmed` is null for input, and for any selector it is
true only when the first returned value equals `Yes` (case-insensitive). For
ordinary selections inspect `values`/`text`, not `confirmed` as a general
approval signal. A successful No response is not a tool error.

## Compatibility

The parser still accepts aliases (both top-level and item-level): `prompt` for
`question`, `questionId` for `id`, `allow_multiple`/`multiple` for `allowMultiple`,
`allow_freeform`/`allow_custom` for `allowFreeform`, `allow_comment` for
`allowComment`, and `placeholder` for `suggest`. Canonical usable values win;
item aliases are checked before top-level defaults. Blank question text aliases
fall through to the next usable text.

For option objects, the parser uses the **first string field** among `title`,
`label`, `text`, `value`, `name`, then trims it and rejects a blank title; a
blank `title` does not fall through to `label`. All aliases identify the same
visible and returned choice: `value` is not a hidden value. Extra object keys
are ignored. Legacy number/boolean/null and array choices are converted to
JSON text; arrays are not nested choice groups. Prefer canonical strings or
`{title, description}` objects; these legacy forms are not advertised in the
model-facing schema.

Install the published package with:

```text
rpi install rpi-ask-user
```
