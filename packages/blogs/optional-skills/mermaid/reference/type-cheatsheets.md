# Per-type cheat-sheets — expectations to minimize render errors

Quick index of **opening keyword + minimal valid skeleton + top pitfalls** for every
Mermaid diagram type. This is a fast lookup, not the full spec.

When sources disagree, prefer live official Mermaid syntax, then
`reference/syntax/<type>.md`, then this file. Read one syntax file only when this
sheet lacks the construct or the keyword is `-beta`. Validate with `scripts/check.sh`
(it rejects Mermaid's error-placeholder SVG).

This sheet and most of `reference/syntax/` were taken at Mermaid 11.16.1.
`agentflow-beta` and `usecase-beta` are Mermaid 12; their skeletons are below and in
`reference/syntax/agentflow.md` and `reference/syntax/usecase.md`. Which types the IR
emits, and what it still leaves out, is [coverage.md](coverage.md).

---

## Universal rules (apply to most types)

- The **first non-empty line** is the diagram keyword. No text/code before it (an
  optional `---`…`---` YAML frontmatter block for `title:`/`config:` is allowed above it).
- **One statement per line.** Don't merge with `;` unless the type's doc shows it.
- **Quote labels** containing spaces, punctuation, parentheses, or reserved words:
  `A["Order (paid)"]`. Unquoted special chars are the #1 cause of errors.
- Use `<br>` for line breaks inside labels, not literal newlines.
- Node/actor **IDs** must be simple (letters/digits/underscore); put the pretty text
  in the label. `end` is reserved in flowcharts — capitalize or quote it.
- Comments: `%% comment` on its own line.
- Indentation is cosmetic except in indentation-driven types (mindmap, treemap,
  ishikawa, treeView) where it defines hierarchy — be consistent there.

---

## Flow & logic

### flowchart  (alias: `graph`)
```
flowchart TD
    A[Start] --> B{Decision?}
    B -->|Yes| C[Do thing]
    B -->|No| D[Stop]
```
- Direction: `TD`/`TB`, `LR`, `RL`, `BT`. Node shapes: `[]` rect, `()` round, `([])`
  stadium, `[[]]` subroutine, `{}` diamond, `(())` circle, `[/ /]` parallelogram.
- v11 typed shapes: `A@{ shape: rounded, label: "x" }`. Subgraphs: `subgraph title … end`.
- For program, system, or data-processing control flow, read
  [iso-5807-flowcharts.md](iso-5807-flowcharts.md): use semantic shapes and label every decision branch.
- Pitfall: unquoted `()`/`{}` in labels; using `end` as a bare node id.

### stateDiagram  (use `stateDiagram-v2`)
```
stateDiagram-v2
    [*] --> Still
    Still --> Moving
    Moving --> [*]
```
- `[*]` = start/end. Composite: `state Name { … }`. Notes: `note right of X: text`.
- Pitfall: prefer `-v2`; the legacy `stateDiagram` lacks many features.

### sequenceDiagram
```
sequenceDiagram
    participant A
    A->>B: Request
    B-->>A: Response
```
- Arrows: `->>` solid+arrow, `-->>` dashed, `-)` async. Blocks: `alt/else/end`,
  `loop/end`, `opt/end`, `par/and/end`. `activate/deactivate` or `->>+`/`->>-`.
- Pitfall: every `alt/loop/opt/par` needs a matching `end`.

### agentflow  (`agentflow-beta`, Mermaid 12)
```
agentflow-beta LR
    flow reviewer["Review Agent"]
        analyse["Analyse diff"]@{ shape: task }
        lint["run_linter"]@{ shape: tool }
        analyse --> lint
    end
```
- Shapes: `task`, `tool`, `input`, `decision`, `refdoc`, `action`.
- Edges: `-->` sequence, `-.-` reference, `--x` failure. Flow ids are endpoints.
- IR emits nodes, flows, and those edges. Hand-author `connector`, `global`, metadata
  (`model`, `instruction`), and `@{ view: "collapsed" }`. See `syntax/agentflow.md`.

### usecase  (`usecase-beta`, Mermaid 12)
```
usecase-beta
    direction LR
    actor Customer("Customer")
    systemBoundary orders("Order system")
        Checkout("Place order")
        Payment("Pay")
    end
    Customer --> Checkout
    Checkout ..> : include Payment
```
- Include/extend: `A ..> : include B` (space before the colon). Generalization: `A --|> B`.
- An undeclared endpoint becomes a use case. Actors must be declared with `actor`.
- IR does not emit notes, JSON tables, stereotypes, or actor icons. See `syntax/usecase.md`.

### zenuml  (alternative sequence renderer)
```
zenuml
    title Demo
    Alice->John: Hello
    John->Alice: Hi
```
- Distinct engine from `sequenceDiagram`; simpler arrows. Read the doc for `@Actor`,
  creation, and `if/while` blocks.

---

## Structure & data models

### classDiagram
```
classDiagram
    class Animal
    Animal <|-- Duck
    Animal : +int age
    Animal : +run()
```
- Relations: `<|--` inherit, `*--` composition, `o--` aggregation, `-->` association,
  `..>` dependency. Visibility: `+ - # ~`. Generics: `List~int~`.
- Pitfall: relationship arrows are direction-sensitive; `label` goes after `:`.

### erDiagram
```
erDiagram
    CUSTOMER ||--o{ ORDER : places
    ORDER ||--|{ LINE_ITEM : contains
```
- Cardinality (left–right): `|o` zero-or-one, `||` one, `}o` zero-or-many, `}|`
  one-or-many, `..`/`--` (dashed=non-identifying). Attributes in `ENTITY { type name }`.
- Pitfall: every relationship needs a `: label`. Entity names shouldn't contain spaces.

### requirementDiagram
```
requirementDiagram
    requirement test_req {
        id: 1
        text: the test text.
        risk: high
        verifymethod: test
    }
    element test_entity { type: simulation }
    test_entity - satisfies -> test_req
```
- Types: `requirement`, `functionalRequirement`, etc. Relations: `satisfies`,
  `traces`, `derives`, `refines`, `contains`, `copies`, `verifies`.
- Pitfall: `risk` ∈ {low,medium,high}; `verifymethod` ∈ {analysis,inspection,test,demonstration}.

### c4  (C4Context / C4Container / C4Component / C4Dynamic / C4Deployment)
```
C4Context
    title System Context
    Person(cust, "Customer", "desc")
    System(sys, "System", "desc")
    Rel(cust, sys, "uses")
```
- Boundaries: `Enterprise_Boundary(id,"name"){ … }`. Ext variants: `Person_Ext`, `System_Ext`.
- Pitfall: still evolving/experimental layout; keep relationships explicit with `Rel(...)`.

---

## Time & project

### gantt
```
gantt
    title A Gantt Diagram
    dateFormat YYYY-MM-DD
    section Section
        A task :a1, 2014-01-01, 30d
        Next   :after a1, 20d
```
- `dateFormat` is required for dated tasks. Task fields: `:id, start, duration`.
  Tags: `done`, `active`, `crit`, `milestone`. Relative: `after id`.
- Pitfall: missing `dateFormat`; durations need units (`30d`, `2w`). A colon inside
  the task title starts metadata early and crashes the renderer (`TypeError`).
  `Review: security :done, t1, ...` is invalid; keep the title free of `:`.

### timeline
```
timeline
    title History
    2002 : LinkedIn
    2004 : Facebook : Google
```
- `period : event [: event …]`. Multiple events per period on continuation lines
  starting with `:`. Sections via `section Name`.

### gitGraph
```
gitGraph
    commit
    branch develop
    checkout develop
    commit
    checkout main
    merge develop
```
- Keywords: `commit`, `branch`, `checkout`, `merge`, `cherry-pick`. Tag/id:
  `commit id: "x" tag: "v1"`. Pitfall: `merge <branch>` must reference an existing branch.

---

## Charts & quantities

### pie
```
pie title Pets
    "Dogs" : 386
    "Cats" : 85
```
- `showData` after `pie` to print values. Labels quoted; values are numbers.

### xychart  (older alias: `xychart-beta`)
```
xychart
    title "Sales"
    x-axis [jan, feb, mar]
    y-axis "Revenue" 4000 --> 11000
    bar [5000, 6000, 7500]
    line [5000, 6000, 7500]
```
- Pitfall: `bar`/`line` array length must match the `x-axis` categories.

### sankey  (older alias: `sankey-beta`)
```
sankey
    Source,Target,10
    A,B,5
```
- CSV rows: `source,target,value`. Quote names containing commas: `'Agri waste'`.
- Pitfall: it's data rows, not arrows — no `-->`.

### radar  (`radar-beta`)
```
radar-beta
    axis a["Math"], b["Science"], c["English"]
    curve x["Alice"]{85, 90, 80}
```
- Pitfall: each `curve`'s value count must equal the number of `axis` entries.

### quadrantChart
```
quadrantChart
    title Reach vs Engagement
    x-axis Low --> High
    y-axis Low --> High
    quadrant-1 Expand
    quadrant-2 Promote
    quadrant-3 Re-evaluate
    quadrant-4 Improve
    "Campaign A": [0.3, 0.6]
```
- Points are `[x, y]` with both in 0–1. All four `quadrant-N` labels expected.

### treemap  (`treemap-beta`)
```
treemap-beta
"Category A"
    "Item A1": 10
    "Item A2": 20
```
- Hierarchy by **indentation**; leaves have `: value`. Pitfall: inconsistent indent breaks nesting.

### venn  (`venn-beta`)
```
venn-beta
    title "Overlap"
    set Frontend
    set Backend
    union Frontend,Backend["APIs"]
```

---

## Visual / spatial / specialized

### architecture  (`architecture-beta`)
```
architecture-beta
    group api(cloud)[API]
    service db(database)[DB] in api
    service srv(server)[Server] in api
    db:L -- R:srv
```
- Built-in icons: `cloud`, `database`, `disk`, `internet`, `server`. Anything else is
  `pack:name` (for example `logos:aws-ec2`). A bare unknown name is not an icon.
- Edges specify sides: `id:L -- R:id2` (`T B L R`), arrows `-->`/`<--`. `junction` for 4-way splits.
- Pitfall: declare an id before an edge uses it. Group edges need the `{group}` modifier.
  A lexer failure is often an error-placeholder SVG while the CLI exits 0; `scripts/check.sh`
  rejects that SVG. Mermaid 11.16 rejected punctuation in bracket titles
  (`[ESP32-S3 firmware]`); Mermaid 12 accepts it. Keep the real spelling and validate.

### block  (older alias: `block-beta`)
```
block
    columns 3
    A B C
    block:group
        D E
    end
```
- `columns N` controls layout; `block:id … end` groups. Arrows with `blockArrowId<[" "]>(dir)`.

### packet  (older alias: `packet-beta`)
```
packet
    0-15: "Source Port"
    16-31: "Destination Port"
```
- Each row is a bit/byte range `start-end: "label"` (or single bit `0: "x"`).
- Pitfall: ranges must be contiguous and non-overlapping.

### kanban
```
kanban
    todo[To Do]
        t1[Write spec]
    doing[In Progress]
        t2[Build feature]
```
- Columns then indented task cards. Metadata: `t1[Task]@{ assigned: 'knsv', ticket: 'MC-1', priority: 'High' }`.
  Priority is `Very High`, `High`, `Low`, or `Very Low`.

### mindmap
```
mindmap
    root((center))
        Branch A
            Leaf
        Branch B
```
- Hierarchy by **indentation** only. Root shapes: `((circle))`, `[square]`, etc.
  Icons `::icon(fa fa-book)`. Pitfall: mixing tabs/spaces breaks the tree.

### journey  (user journey)
```
journey
    title My day
    section Work
        Make tea: 5: Me
        Do work: 1: Me, Cat
```
- Task line: `name: score(1–5): comma,separated,actors`.

### swimlanes  (`swimlane-beta`)
```
swimlane-beta LR
    subgraph Customer
        request[Request]
    end
    subgraph Support
        triage[Triage]
    end
    request --> triage
```
- Flowchart-like with lanes as `subgraph`s. Direction after the keyword.

### treeView  (`treeView-beta`)
```
treeView-beta
├── src/
│   ├── index.ts
│   └── utils.ts
└── README.md
```
- Renders a file/tree structure from ASCII tree glyphs. Keep the box-drawing chars intact.

---

## Thinking / modeling frameworks

### cynefin  (`cynefin-beta`)
```
cynefin-beta
    title Incident Response
    complex
        "Investigate root cause"
    complicated
        "Analyze"
```
- Domains: `clear`, `complicated`, `complex`, `chaotic`, `confusion`. Items indented, quoted.
  Mermaid ignores a self-loop; do not write `complex --> complex`.

### ishikawa  (`ishikawa-beta`, fishbone/cause-effect)
```
ishikawa-beta
    Blurry Photo
    Process
        Out of focus
    User
        Wrong settings
```
- First line = the effect/problem; then category headers with indented causes.

### eventmodeling
```
eventmodeling
    tf 01 ui CartUI
    tf 02 cmd AddItem
    tf 03 evt ItemAdded
```
- Timeframe rows `tf <n> <kind> <name>` — kinds like `ui`, `cmd`, `evt`, `rmo`. Read the doc.

### wardley  (`wardley-beta`)
```
wardley-beta
    title Value Chain
    anchor Business [0.95, 0.63]
    component Cup of Tea [0.79, 0.61]
    Business -> Cup of Tea
```
- Coordinates `[visibility, evolution]` in 0–1. `anchor`/`component` then `->` links.

### railroad  (`railroad-ebnf-beta`)
```
railroad-ebnf-beta
    title "Digit"
    digit = "0" | "1" | "2" ;
```
- EBNF grammar → railroad/syntax diagram. Each rule ends with `;`.
