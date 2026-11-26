# Diagram IR — generating Mermaid from structured data

`scripts/render.py` turns a JSON document (the **IR**) into Mermaid source.
Same JSON in, same text out. The script does not render; run `scripts/check.sh` on the
output before use. Every branch below is covered by a real render in `tests/test_render.py`.

Use the IR when the diagram is already structured data (a graph, a timing table, a state
table, a sequence, a traceability table). Hand-author when there is no such source yet.

Presentational Mermaid (`classDef` / `style` coloring, `click` handlers, theme config) is
out of scope, except flowchart `status` colors below, which are fixed data. Post-process
the generated source if you need anything else. Icons are in scope only for
`architecture-beta` (`icon`).

## Families

Every document has `"diagram"` (family) and `"target"` (override with `--target`).
Any other pair is an error. Field-level schemas for the targets below live in this file.
Every other target (the `timeline` keyword, charts, kanban, packet, git, journey, the other
C4 diagrams, swimlane, agentflow, wardley, tree, cynefin, event model, railroad, use case,
zenuml, `info`) is specified in [ir-catalog.md](ir-catalog.md). The matrix is
[coverage.md](coverage.md).

| Family | Targets in this file |
|---|---|
| `graph` | `flowchart`, `mindmap`, `block`, `C4Context`, `C4Container`, `architecture-beta`, `erDiagram`, `classDiagram` |
| `timeline` | `gantt` (the `timeline` keyword is a different target; see ir-catalog) |
| `state-machine` | `stateDiagram-v2` |
| `sequence` | `sequenceDiagram` (`zenuml` is in ir-catalog) |
| `requirement-links` | `requirementDiagram` |

### `graph` — nodes, edges, optional grouping

```jsonc
{
  "diagram": "graph",
  "target": "flowchart",           // or "mindmap"
  "direction": "TD",                // flowchart only: TB/TD/BT/LR/RL, default TD
  "root": "n1",                     // mindmap only: the single root node id
  "groups": [{ "id": "stage-1", "label": "Stage 1" }],
  "nodes": [
    { "id": "1.1", "label": "Session rotation", "kind": "process", "group": "stage-1" }
  ],
  "edges": [
    { "from": "1.1", "to": "1.3", "kind": "dependency", "label": "" }
  ]
}
```

- Node `kind` (flowchart, ISO 5807 shapes): `terminator` (stadium), `process` (rect, default),
  `decision` (diamond), `io` (lean-r/parallelogram), `subprocess` (subproc), `document` (doc),
  `store` (cylinder), `connector` (circle).
- Node `status` (flowchart only, optional): `done`/`ready`/`blocked`/`pending`, each a fixed
  color via a generated `classDef`. Only statuses that appear get a `classDef`.
- Node `kind` (mindmap): `root` (auto-applied to the declared root), `circle`, `square`,
  `bang`, `cloud`, or omit for the default plain-text shape.
- Edge `kind` (flowchart): `normal`/`dependency` (`-->`), `conditional`/`weak` (`-.->`),
  `strong` (`==>`).
- `id`s may be anything (e.g. `"1.1"`); `render.py` sanitizes them to safe Mermaid
  identifiers internally and keeps the original as the visible label.
- Flowchart node labels flatten embedded newlines to a single space rather than `<br/>` — kept
  version-independent since `<br/>` inside the typed `@{shape: ...}` label syntax depends on
  HTML labels being enabled. Use `sequence` notes/messages (which do support `<br/>`) for
  multi-line prose.
- mindmap requires an explicit `"root"` and a tree. Every node is reachable from that root
  by one path. A second parent, a cycle, a duplicate id, an unreachable node, or an edge to
  an undeclared id is an error. Nothing is dropped.

### `graph` — `block`: nested/grouped composition

Reuses `nodes`/`edges`/`groups` exactly like flowchart (arbitrary `id`s, sanitized the same
way). An optional top-level `"columns"` (positive int) emits the `columns N` directive.

```jsonc
{
  "diagram": "graph", "target": "block", "columns": 3,
  "groups": [{ "id": "api", "label": "API" }],
  "nodes": [
    { "id": "db", "label": "Database", "kind": "store", "group": "api" },
    { "id": "start", "label": "Begin", "kind": "terminator" }
  ],
  "edges": [{ "from": "start", "to": "db", "label": "writes to" }]
}
```

- Node `kind`, reusing the flowchart ISO-shape names for consistency across the family:
  `process` (rect, default), `terminator` (stadium), `decision` (rhombus), `subprocess`
  (subroutine), `store` (cylinder), `connector` (circle), `io` (parallelogram).
- A group renders as `block:<id>["<label>"] ... end`; a node's `"group"` places it inside.
  Groups are flat (one level), matching flowchart's grouping — not the composite/nested
  blocks the raw Mermaid grammar also allows.
- Edges render as a plain arrow (`-->`), optionally with a quoted label
  (`A-- "label" -->B`). `kind` may be omitted, `normal`, or `dependency` (all the same
  arrow). Any other `kind` is an error. Both node and group ids are valid edge endpoints.

### `graph` — `C4Context` / `C4Container`: actors, systems, containers, boundaries

One serializer (`render_graph_c4`) handles both targets; the IR's own `"target"` field (not
just `--target`) picks which, since node/boundary kind vocabularies differ between them.

```jsonc
{
  "diagram": "graph", "target": "C4Context",              // or "C4Container"
  "title": "Banking System",
  "groups": [{ "id": "b0", "label": "Bank Boundary", "kind": "enterprise_boundary" }],
  "nodes": [
    { "id": "customerA", "label": "Customer A", "kind": "person",
      "description": "A bank customer", "group": "b0" },
    { "id": "sysAA", "label": "Banking System", "kind": "system", "description": "Core system" }
  ],
  "edges": [{ "from": "customerA", "to": "sysAA", "label": "Uses", "kind": "birel" }]
}
```

- Node `kind` (`C4Context`): `person`, `person_ext`, `system`, `system_db`, `system_queue`,
  `system_ext`, `system_db_ext`, `system_queue_ext`. `C4Container` additionally allows
  `container`, `container_db`, `container_queue`, `container_ext`, `container_db_ext`,
  `container_queue_ext`. `kind` is required — never guessed, since Person vs. System is a
  meaningful choice, not a default. Container-family kinds accept optional `"technology"`
  (rendered before `"description"`, matching `Container(alias, label, techn, descr)`); a
  `"description"` without a `"technology"` on a container kind is a render error rather than
  a silently reordered/dropped argument. Non-container kinds reject `"technology"` outright.
- Group `kind` (boundary): `enterprise_boundary`, `system_boundary`, `boundary` (all
  `C4Context`); `container_boundary`, `boundary` (`C4Container`). `kind: "boundary"` requires
  a `"boundary_type"` string (the macro's third positional arg). Groups nest via an optional
  `"group"` field pointing at a parent group id (unlike flowchart/block, boundaries can be
  arbitrarily deep).
- Edge `kind` (relationship macro), default `rel`: `rel` (`Rel`), `birel` (`BiRel`), `rel_up`/
  `rel_down`/`rel_left`/`rel_right` (`Rel_U`/`Rel_D`/`Rel_L`/`Rel_R`), `rel_back` (`Rel_Back`).
  Optional `"technology"` renders as the macro's 4th arg.
- Node/group/edge-endpoint `id`s must be bare identifiers (letters/digits/underscore) — C4's
  PlantUML-derived macro syntax doesn't tolerate arbitrary punctuation the way flowchart's
  sanitized ids do.
- `C4Component`, `C4Dynamic`, and `C4Deployment` reuse this document shape. Their kind lists
  and `rel_index` are in [ir-catalog.md](ir-catalog.md).

### `graph` — `architecture-beta`: services, groups, junctions, ports

```jsonc
{
  "diagram": "graph", "target": "architecture-beta",
  "groups": [{ "id": "api", "label": "API", "icon": "cloud" }],
  "nodes": [
    { "id": "db", "label": "Database", "icon": "database", "group": "api" },
    { "id": "server", "label": "Server", "icon": "server", "group": "api" },
    { "id": "j1", "kind": "junction" }
  ],
  "edges": [
    { "from": "db", "to": "server", "from_side": "L", "to_side": "R" },
    { "from": "server", "to": "j1", "from_side": "B", "to_side": "T", "arrow": "forward" }
  ]
}
```

- Node `kind`: `service` (default) or `junction`. A `junction` takes no `"label"`/`"icon"` —
  supplying either is a render error, not a silently dropped field.
- `"icon"` (services and groups) must be one of the five built-in icons (`cloud`, `database`,
  `disk`, `internet`, `server`) or a `"pack:icon-name"` reference to a registered icon pack
  (validated by shape — a bare unrecognized name is rejected rather than passed through and
  rendered as a missing icon). Omit `"icon"` for a plain box.
- `"group"` (nodes and groups) nests inside a declared parent group id; groups can nest
  arbitrarily deep via their own optional `"group"`.
- Edge `from_side`/`to_side` ∈ `T`/`B`/`L`/`R` (required — architecture-beta ports have no
  default side). Edge `"arrow"` ∈ `none` (`--`, default), `forward` (`-->`), `backward`
  (`<--`), `both` (`<-->`). `"from_group"`/`"to_group"` (bool) append the `{group}` modifier
  so the edge attaches to the service's enclosing group boundary instead of the service
  itself — group ids are never edge endpoints on their own, matching the underlying grammar.
- Node/group `id`s must be bare identifiers, matching the underlying `architecture-beta`
  grammar (no arbitrary-id sanitization, unlike flowchart/block).

### `graph` — `erDiagram`: entities, attributes, crow's-foot cardinality

```jsonc
{
  "diagram": "graph", "target": "erDiagram", "direction": "LR",
  "nodes": [
    { "id": "CUSTOMER", "members": ["string name PK", "string sector"] },
    { "id": "p", "label": "Customer Account" }
  ],
  "edges": [
    { "from": "CUSTOMER", "to": "p", "label": "has",
      "left": "exactly_one", "right": "zero_or_more", "identifying": true }
  ]
}
```

- Node `"id"` is the entity name; unicode/space/punctuation names are auto-quoted the same
  way flowchart sanitizes ids, but the *original* string is what's quoted (not an internal
  alias) since that's what Mermaid's ER grammar accepts directly. Optional `"label"` renders
  as a display alias (`id[label]`), matching Mermaid's alias syntax; edges still reference the
  entity by its bare `"id"`, never by the alias.
- `"members"` is a list of pre-formatted attribute-line strings (e.g. `"string email PK"`,
  `'int age "years"'`) rendered verbatim inside the entity's `{ }` block — attribute text is
  opaque content, like a gantt bar label or sequence note, not a re-validated sub-schema.
- Edge `"left"`/`"right"` (both required) ∈ `zero_or_one`, `exactly_one`, `zero_or_more`,
  `one_or_more` — the crow's-foot cardinality nearest the `from`/`to` entity respectively.
  Optional `"identifying"` (bool, default `true`) selects a solid (`--`, identifying) vs.
  dashed (`..`, non-identifying) relationship line.
- Optional top-level `"direction"` ∈ `TB`/`BT`/`LR`/`RL`.

### `graph` — `classDiagram`: classes, members, UML relationships

```jsonc
{
  "diagram": "graph", "target": "classDiagram",
  "nodes": [
    { "id": "Animal", "members": ["+int age", "+isMammal()"] },
    { "id": "Duck", "kind": "interface", "members": ["+String beakColor", "+swim()"] }
  ],
  "edges": [
    { "from": "Animal", "to": "Duck", "kind": "inheritance", "label": "implements" }
  ]
}
```

- Node `"id"` must be a bare identifier (letters, digits, underscore). Optional `"label"`
  renders a display label (`class Id["Label"]`).
- Optional node `"kind"` ∈ `interface`, `abstract`, `service`, `enumeration` renders the
  `<<Kind>>` annotation as the class block's first line; omit for a plain class.
- `"members"` is a list of pre-formatted attribute/method strings (e.g. `"+String owner"`,
  `"+deposit(amount) bool"`) rendered verbatim — same opaque-text treatment as erDiagram
  members and gantt bar labels.
- Edge `"kind"` (required — never defaulted, since inheritance vs. association is a meaningful
  UML choice) ∈ `inheritance` (`<|--`), `composition` (`*--`), `aggregation` (`o--`),
  `association` (`-->`), `link` (`--`), `dependency` (`..>`), `realization` (`..|>`),
  `link_dashed` (`..`). Optional `"from_card"`/`"to_card"` render quoted multiplicity text on
  either side of the arrow (e.g. `Customer "1" --> "*" Ticket`).
- Optional top-level `"direction"` ∈ `TB`/`BT`/`LR`/`RL`.

### `timeline` — sectioned bars with tags, milestones, and exclusions

```jsonc
{
  "diagram": "timeline",
  "target": "gantt",
  "dateFormat": "YYYY-MM-DDTHH:mm:ss",
  "axisFormat": "%m-%d %H:%M",
  "excludes": ["weekends"],          // optional: dates, weekday names, or "weekends"
  "sections": [
    { "name": "Stage 1", "bars": [
      { "id": "t1_1", "label": "1.1 (verified)", "start": "2026-08-09T10:00:00",
        "end": "2026-08-09T10:14:00", "tags": ["done"] },
      { "id": "t1_2", "label": "Checkpoint reached", "start": "2026-08-09T10:20:00",
        "end": "2026-08-09T10:20:00", "tags": ["milestone"] }
    ] }
  ]
}
```

`tags` is a list because a task can be both `crit` and `active` or `done`. Each tag ∈
`active`/`done`/`crit`/`milestone`; no duplicates; `active` and `done` are mutually
exclusive; `milestone` cannot combine with `active`/`done`. Every bar has an explicit
`start`/`end` in `dateFormat` (a milestone is `start == end`). The label must be non-empty
and must not contain `:`. Mermaid uses the first colon on the task line to start metadata;
a colon in the title crashes the renderer (`TypeError`) instead of naming a line. Colons
inside `start`/`end` values are fine.

### `state-machine` — states, composite states, pseudostates, notes

```jsonc
{
  "diagram": "state-machine",
  "target": "stateDiagram-v2",
  "direction": "LR",                 // optional: TB/BT/LR/RL
  "initial": "Draft",
  "final": ["Rejected"],
  "states": [
    { "id": "Draft", "label": "Awaiting review" },
    { "id": "Outer", "states": [{ "id": "Inner" }], "initial": "Inner", "final": ["Inner"],
      "transitions": [] },
    { "id": "gate", "kind": "choice" }
  ],
  "transitions": [{ "from": "Draft", "to": "Approved", "label": "user approves" }],
  "notes": [{ "state": "Draft", "side": "right", "text": "entry point" }]
}
```

- A state with a nested `"states"`/`"transitions"`/`"initial"`/`"final"` renders as a
  composite state (`state Outer { ... }`), recursively — a composite can contain another
  composite.
- State `kind` ∈ `choice`, `fork`, `join` renders `<<choice>>` / `<<fork>>` / `<<join>>`.
  Omit `kind` for an ordinary state. Any other `kind` is an error, including on a composite.
  A pseudostate cannot also carry `states`, `transitions`, `initial`, or `final`.
- `initial`/`final` generate the `[*] --> ...` / `... --> [*]` edges at whatever nesting level
  they appear.

### `sequence` — actors and a recursive step list

```jsonc
{
  "diagram": "sequence",
  "target": "sequenceDiagram",
  "actors": [{ "id": "User" }, { "id": "DB", "kind": "actor" }],
  "steps": [
    { "type": "activate", "actor": "DB" },
    { "type": "message", "from": "User", "to": "DB", "label": "query", "kind": "sync" },
    { "type": "loop", "label": "Every retry", "steps": [ /* nested steps */ ] },
    { "type": "alt", "branches": [
      { "label": "success", "steps": [ /* ... */ ] },
      { "label": "failure", "steps": [ /* ... */ ] }
    ] },
    { "type": "note", "position": "right_of", "actors": ["DB"], "text": "cache warm" },
    { "type": "deactivate", "actor": "DB" }
  ]
}
```

`steps` is a recursive list — each entry's `"type"` picks the shape:

| `type` | Fields | Renders |
|---|---|---|
| `message` (default) | `from`, `to`, `label`, `kind` | `A->>B: label` (arrow per `kind`: `sync`→`->>`, `async`→`-)`, `reply`→`-->>`, `cross`→`-x`) |
| `activate` / `deactivate` | `actor` | `activate A` / `deactivate A` |
| `note` | `position` (`right_of`/`left_of`/`over`), `actors`, `text` | `Note right of A: text` |
| `loop` / `opt` / `break` | `label`, `steps` | single-body block, e.g. `loop label ... end` |
| `rect` | `color`, `steps` | `rect <color> ... end` (background highlight) |
| `alt` / `par` / `critical` | `branches: [{label, steps}]` | multi-branch block using the right keyword per branch (`alt`/`else`, `par`/`and`, `critical`/`option`) |

Message/note text with embedded newlines is rendered with `<br/>` (Mermaid's documented line
break for sequence text), unlike flowchart node labels.

### `requirement-links` — traceability

```jsonc
{
  "diagram": "requirement-links",
  "target": "requirementDiagram",
  "direction": "LR",                 // optional: TB/BT/LR/RL
  "requirements": [
    { "id": "req_1_1", "text": "the system shall rotate tokens", "risk": "high",
      "verify_method": "test", "type": "requirement" }
  ],
  "elements": [{ "id": "session_module", "type": "component" }],
  "links": [{ "from": "session_module", "to": "req_1_1", "kind": "satisfies" }]
}
```

`type` ∈ the six SysML requirement types (default `requirement`); `risk` ∈
`low`/`medium`/`high`; `verify_method` ∈ `analysis`/`inspection`/`test`/`demonstration`; link
`kind` ∈ `contains`/`copies`/`derives`/`satisfies`/`verifies`/`refines`/`traces`. Requirement
and element `id`s must be bare identifiers (letters/digits/underscore) — Mermaid's
`requirementDiagram` grammar doesn't tolerate arbitrary punctuation the way flowchart node IDs
(sanitized automatically) do.

## Validation philosophy

`render.py` fails closed: an edge, link, or message to an undeclared node or actor, an
unknown `kind` / `tag` / `type` / `direction`, a mindmap that is not a tree, a gantt label
containing `:`, conflicting gantt tags, an unsupported `--backend`. It does not guess a
shape or drop a bad reference.
