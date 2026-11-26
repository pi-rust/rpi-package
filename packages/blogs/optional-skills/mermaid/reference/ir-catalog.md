# IR catalog — targets beyond the core families

Schemas for every `scripts/render.py` target that is not spelled out in [ir.md](ir.md).
The core families (flowchart, mindmap, block, C4 context/container, architecture, ER, class,
gantt, state, sequence, requirements) stay in that file. This file is the on-demand schema
for the rest. The coverage matrix is [coverage.md](coverage.md).

Every document is `{"diagram": "<family>", "target": "<keyword>", ...}`. Unknown fields that
the serializer does not read are ignored. Unknown `kind` / `type` values, missing required
fields, and undeclared endpoints raise. `render.py` does not render; run `scripts/check.sh`.

## `graph` — `swimlane-beta`

Same document as flowchart (`nodes`, `edges`, `groups`, `direction`, ISO `kind`, `status`).
The first line is `swimlane-beta <direction>` instead of `flowchart`. Groups become lanes.

## `graph` — `agentflow-beta`

```jsonc
{
  "diagram": "graph", "target": "agentflow-beta", "direction": "LR",
  "groups": [{ "id": "main", "label": "Main" }],
  "nodes": [
    { "id": "research", "label": "Research", "kind": "task", "group": "main" },
    { "id": "write", "label": "Write", "kind": "action" }
  ],
  "edges": [
    { "from": "research", "to": "write", "kind": "sequence", "label": "then" },
    { "from": "main", "to": "write", "kind": "reference" }
  ]
}
```

- Node `kind`: `task`, `tool`, `input`, `decision`, `refdoc`, `action`. Omit for a plain box.
- Edge `kind`: `sequence` (`-->`, default), `reference` (`-.-`), `failure` (`--x`).
- A group is a `flow id["label"] … end`. Nested groups use `"group"` for the parent flow id.
  Flow ids are valid edge endpoints.
- Not emitted: `@{ model, instruction, params, returns, connectorRef }`, the `connector`
  keyword, a `global` block, `@{ view: collapsed }`. Hand-author those.

## `graph` — `wardley-beta`

```jsonc
{
  "diagram": "graph", "target": "wardley-beta", "title": "Tea",
  "nodes": [
    { "id": "Business", "kind": "anchor", "visibility": 0.95, "evolution": 0.63 },
    { "id": "Cup of Tea", "kind": "component", "visibility": 0.79, "evolution": 0.61 }
  ],
  "edges": [{ "from": "Cup of Tea", "to": "Business" }]
}
```

- `kind` is `anchor` or `component`. `visibility` (Y) and `evolution` (X) are numbers in 0..1.
- `id` is the visible name. A bare identifier is unquoted; anything else is quoted.
- Not emitted: `evolution Genesis@… -> …`, `size`, label offsets, `(build)` / `(buy)` / `(inertia)`.

## `graph` — `C4Component`, `C4Dynamic`, `C4Deployment`

Same shape as the C4 documents in [ir.md](ir.md). Node and boundary vocabularies differ.

| `target` | Node `kind` | Group `kind` |
|---|---|---|
| `C4Component`, `C4Dynamic` | context kinds, plus container kinds, plus `component`, `component_db`, `component_queue`, `component_ext`, `component_db_ext`, `component_queue_ext` | `container_boundary`, `boundary` |
| `C4Deployment` | context kinds plus container kinds | `deployment_node`, `node`, `node_left`, `node_right` |

Container and component kinds take optional `technology` before `description`. Deployment
groups take optional `technology` (the node type) then `description`.

Edge `kind` adds `rel_index` on `C4Dynamic` only: integer `index`, no `technology`.
Renders `RelIndex(index, from, to, label)`.

Not emitted: sprites, tags, `UpdateRelStyle`, `UpdateElementStyle`.

## `timeline` — `timeline`

The keyword `timeline`, not a gantt chart. Provide `sections` or top-level `events`.

```jsonc
{
  "diagram": "timeline", "target": "timeline", "title": "History", "direction": "LR",
  "sections": [{ "name": "Early", "events": [
    { "period": "2002", "text": "LinkedIn" },
    { "period": "2004", "text": ["Facebook", "Google"] }
  ]}]
}
```

`direction` is `LR` or `TD` only. Title, section, period, and event text must not contain `:`.

## `sequence` — `zenuml`

```jsonc
{
  "diagram": "sequence", "target": "zenuml", "title": "Demo",
  "actors": [{ "id": "Alice", "kind": "actor" }, { "id": "John" }],
  "steps": [{ "type": "message", "from": "Alice", "to": "John", "label": "Hello" }]
}
```

- Actor `kind`: `actor` (`@Actor`), `participant` or omitted (bare name). No alias: `id` is the name.
- Only `type: "message"` is emitted (`A->B: text`). `if` / `while` / `try` / `par` / `opt` / `new` raise; hand-author them.

## `chart`

| `target` | Required | Notes |
|---|---|---|
| `pie` | `slices: [{label, value}]` | `value` > 0. Optional `title`, `showData`. |
| `xychart` | `xAxis: []`, and `bar` and/or `line` | Series length must match `xAxis`. Optional `title`, `orientation` `vertical` (default) or `horizontal`, `yAxis: {label, min, max}`. |
| `sankey` | `links: [{from, to, value}]` | CSV rows. Cells containing a comma are single-quoted. `value` ≥ 0. |
| `quadrantChart` | `xAxis: {left, right}`, `yAxis: {bottom, top}`, `quadrants` keys `1`–`4`, `points: [{label, x, y}]` | Point coordinates in 0..1. |
| `radar-beta` | `axes: [{id, label}]`, `curves: [{id, label, values}]` | Value count matches axes. Optional `title`, `min`, `max`. |
| `treemap-beta` | `nodes: [{label, value? , children?}]` | Indentation tree. A leaf needs `value` ≥ 0. |
| `venn-beta` | `sets` | A set is a string id or `{id, label}`. Optional `unions: [{sets, label}]` (2+ declared sets). Optional `title`. |

Donut holes, legend position, and themes are frontmatter config. They are not IR fields.

## `packet` — `packet`

```jsonc
{ "diagram": "packet", "target": "packet",
  "fields": [
    { "start": 0, "end": 15, "label": "Source Port" },
    { "bits": 16, "label": "Destination Port" }
  ] }
```

Each field is an absolute `start`/`end` (integers, `end` ≥ `start`) or a `bits` width (`+N`).
Absolute ranges must be contiguous: the next `start` equals the previous end + 1.

## `board` — `kanban`

```jsonc
{ "diagram": "board", "target": "kanban",
  "columns": [{ "id": "todo", "label": "To Do", "cards": [
    { "id": "t1", "label": "Write spec", "priority": "High", "assigned": "Ada", "ticket": "MC-1" }
  ]}] }
```

`priority` is `Very High`, `High`, `Low`, or `Very Low`. Labels must not contain `]`. Ids are bare identifiers and unique across columns and cards.

## `journey` — `journey`

```jsonc
{ "diagram": "journey", "target": "journey", "title": "My day",
  "sections": [{ "name": "Work", "tasks": [
    { "name": "Make tea", "score": 5, "actors": ["Me"] }
  ]}] }
```

`score` is an integer 1–5. Names and actor names must not contain `:`.

## `git` — `gitGraph`

```jsonc
{ "diagram": "git", "target": "gitGraph", "title": "Release",
  "ops": [
    { "type": "commit", "id": "c1", "tag": "v1" },
    { "type": "branch", "name": "develop" },
    { "type": "checkout", "name": "develop" },
    { "type": "merge", "name": "develop" }
  ] }
```

`type` is `commit`, `branch`, `checkout` (also accepts `switch`, emitted as `checkout`), or `merge`.
`cherry-pick` is not emitted. Optional `title` is YAML frontmatter.

## `tree`

`treeView-beta` nodes are `{name, children?}`. A node with children gets a trailing `/` when `name` does not already end in one. Names with spaces are quoted. Box-drawing input is hand-author only.

`ishikawa-beta` is `{effect, causes: [{label, children?}]}`. Effect and labels must not contain `:`.

## `cynefin` — `cynefin-beta`

```jsonc
{ "diagram": "cynefin", "target": "cynefin-beta", "title": "Incident",
  "domains": { "complex": ["Investigate"], "complicated": ["Analyze"] },
  "transitions": [{ "from": "complex", "to": "complicated", "label": "Pattern" }] }
```

Domain keys: `clear`, `complicated`, `complex`, `chaotic`, `confusion`. Empty domains are omitted.
A transition whose `from` equals `to` raises (Mermaid drops self-loops).

## `eventmodeling` — `eventmodeling`

```jsonc
{ "diagram": "eventmodeling", "target": "eventmodeling",
  "frames": [
    { "n": 1, "kind": "ui", "name": "CartUI" },
    { "n": 2, "kind": "cmd", "name": "AddItem" }
  ] }
```

Emits `tf NN kind name` with `n` zero-padded and unique. `kind` is `ui`, `cmd`, `evt`, `rmo`, or `pcr`.
`name` may contain dots (`Inventory.Changed`). Not emitted: `rf` rows, long kind names (`command`, `event`, …), data blocks, and link chains.

## `grammar` — `railroad-ebnf-beta`

```jsonc
{ "diagram": "grammar", "target": "railroad-ebnf-beta", "title": "Digit",
  "rules": ["digit = \"0\" | \"1\" ;"] }
```

Each rule is one line, contains `=`, and ends with `;`. This is verbatim EBNF, not a grammar AST.

## `usecase` — `usecase-beta`

```jsonc
{
  "diagram": "usecase", "target": "usecase-beta", "direction": "LR",
  "actors": [{ "id": "User", "label": "Customer" }],
  "usecases": [
    { "id": "Login", "label": "Sign in", "group": "bank" },
    { "id": "Audit", "label": "Audit trail" }
  ],
  "groups": [{ "id": "bank", "label": "Bank" }],
  "edges": [
    { "from": "User", "to": "Login", "label": "uses" },
    { "from": "Login", "to": "Audit", "kind": "include" }
  ]
}
```

- Ids are letters, digits, or underscore. A group is `systemBoundary id("label") … end`.
- Edge `kind`: `association` (default; `-->`, or `-- "label" -->`), `include` / `extend` (`A ..> : include B`, both ends use cases), `generalization` (`--|>`, two actors or two use cases).
- Not emitted: notes, JSON tables, stereotypes, actor `type` / `icon`, package boundaries.

## `info` — `info`

`{"diagram": "info", "target": "info"}` emits the single keyword `info`. Mermaid prints its version. No other fields are read.
