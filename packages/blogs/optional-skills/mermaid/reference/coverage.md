# Mermaid type coverage

Catalog of diagram types an agent can author with this skill. The type list is the Mermaid
open-source syntax set (`docs/syntax` on mermaid-js, published at
[mermaid.ai/open-source](https://mermaid.ai/open-source/)) plus `info`, which ships in
`packages/mermaid/src/diagrams` and has no syntax page. `common` and `error` are not types.
`examples.md` is a sample index, not a type.

Keyword in the first column is what `render.py` emits when an IR exists. Older aliases that
still render (`graph`, `xychart-beta`, `sankey-beta`, `packet-beta`, `block-beta`,
`stateDiagram`, `treemap` without `-beta`) are noted in the cheatsheet, not as extra rows.

Skill docs: [type-cheatsheets.md](type-cheatsheets.md) has a skeleton for every row.
[syntax/](syntax/) is the cached full page (Mermaid 11.16.1) except `agentflow.md` and
`usecase.md`, which are short Mermaid 12 notes. IR schemas: [ir.md](ir.md) and
[ir-catalog.md](ir-catalog.md). Tests live in `tests/test_render.py` and call `scripts/check.sh`.

CI and the `npx` fallback pin `@mermaid-js/mermaid-cli@12.0.0` with Mermaid `12.0.0`
on Node 22. `agentflow-beta` and `usecase-beta` are not registered in Mermaid 11; that
CLI reports `UnknownDiagramError` and is not the version these tests claim. No type is
demoted for that reason.

| Type | Skill docs | IR | Tests | Gaps remaining |
|---|---|---|---|---|
| `flowchart` (`graph`) | cheatsheet, `syntax/flowchart.md`, ISO 5807 | `graph` / `flowchart` | `GraphFlowchartTests` | Expanded shape names beyond the ISO set; `click` / theme |
| `sequenceDiagram` | cheatsheet, `syntax/sequenceDiagram.md` | `sequence` / `sequenceDiagram` | `SequenceTests` | Actor boxes, menus, creation/destruction, sequence numbers; ids are not aliased |
| `classDiagram` | cheatsheet, `syntax/classDiagram.md` | `graph` / `classDiagram` | `GraphClassTests` | Namespaces, cardinality on relations, `cssClass` |
| `stateDiagram-v2` | cheatsheet, `syntax/stateDiagram.md` | `state-machine` / `stateDiagram-v2` | `StateMachineTests` | Legacy `stateDiagram`; concurrency (`--`) |
| `erDiagram` | cheatsheet, `syntax/entityRelationshipDiagram.md` | `graph` / `erDiagram` | `GraphErTests` | Attributes as keys beyond the emitted forms |
| `gantt` | cheatsheet, `syntax/gantt.md` | `timeline` / `gantt` | `TimelineGanttTests` | `until`, `excludes` weekends only as emitted, tick intervals |
| `journey` | cheatsheet, `syntax/userJourney.md` | `journey` / `journey` | `CatalogRenderTests` | — |
| `timeline` | cheatsheet, `syntax/timeline.md` | `timeline` / `timeline` | `CatalogRenderTests` | — |
| `pie` | cheatsheet, `syntax/pie.md` | `chart` / `pie` | `CatalogRenderTests` | Donut / highlight / legend are frontmatter, not IR |
| `quadrantChart` | cheatsheet, `syntax/quadrantChart.md` | `chart` / `quadrantChart` | `CatalogRenderTests` | Point styling |
| `xychart` | cheatsheet, `syntax/xyChart.md` | `chart` / `xychart` | `CatalogRenderTests` | `line` and `bar` together are supported; chart config is not |
| `requirementDiagram` | cheatsheet, `syntax/requirementDiagram.md` | `requirement-links` / `requirementDiagram` | `RequirementLinksTests` | — |
| `architecture-beta` | cheatsheet, `syntax/architecture.md` | `graph` / `architecture-beta` | `GraphArchitectureTests` | Group-edge `{group}` modifier, edge labels |
| `mindmap` | cheatsheet, `syntax/mindmap.md` | `graph` / `mindmap` | `GraphMindmapTests` | `::icon()` decorations |
| `ishikawa-beta` | cheatsheet, `syntax/ishikawa.md` | `tree` / `ishikawa-beta` | `CatalogRenderTests` | — |
| `kanban` | cheatsheet, `syntax/kanban.md` | `board` / `kanban` | `CatalogRenderTests` | `ticketBaseUrl` frontmatter |
| `gitGraph` | cheatsheet, `syntax/gitgraph.md` | `git` / `gitGraph` | `CatalogRenderTests` | `cherry-pick`; branch order options |
| `C4Context` | cheatsheet, `syntax/c4.md` | `graph` / `C4Context` | `GraphC4Tests` | Sprites, tags, `UpdateElementStyle`, `UpdateRelStyle` |
| `C4Container` | same | `graph` / `C4Container` | `GraphC4Tests` | same |
| `C4Component` | same | `graph` / `C4Component` | `DispatchTests` | same |
| `C4Dynamic` | same | `graph` / `C4Dynamic` | `DispatchTests` (`RelIndex`) | same; relationship index only |
| `C4Deployment` | same | `graph` / `C4Deployment` | `DispatchTests` | same; deployment node type is `technology` |
| `sankey` | cheatsheet, `syntax/sankey.md` | `chart` / `sankey` | `CatalogRenderTests` | — |
| `packet` | cheatsheet, `syntax/packet.md` | `packet` / `packet` | `CatalogRenderTests` | Byte-vs-bit display config |
| `block` | cheatsheet, `syntax/block.md` | `graph` / `block` | `GraphBlockTests` | Nested `block` groups, `blockArrow`, columns only as a top-level int |
| `eventmodeling` | cheatsheet, `syntax/eventmodeling.md` | `eventmodeling` / `eventmodeling` | `CatalogRenderTests` | `rf` rows, long kind names, data blocks, link chains |
| `treeView-beta` | cheatsheet, `syntax/treeView.md` | `tree` / `treeView-beta` | `CatalogRenderTests` | Box-drawing input, `icon()`, `##` annotations, `:::` classes |
| `radar-beta` | cheatsheet, `syntax/radar.md` | `chart` / `radar-beta` | `CatalogRenderTests` | Per-axis range, curve options beyond min/max |
| `treemap-beta` | cheatsheet, `syntax/treemap.md` | `chart` / `treemap-beta` | `CatalogRenderTests` | Class styles on leaves |
| `usecase-beta` | cheatsheet, `syntax/usecase.md` | `usecase` / `usecase-beta` | `CatalogRenderTests` | Notes, JSON tables, stereotypes, actor type/icon, package boundaries |
| `venn-beta` | cheatsheet, `syntax/venn.md` | `chart` / `venn-beta` | `CatalogRenderTests` | — |
| `wardley-beta` | cheatsheet, `syntax/wardley.md` | `graph` / `wardley-beta` | `CatalogRenderTests` | Evolution axis, `size`, label offsets, `(build)`/`(buy)`/`(inertia)` |
| `cynefin-beta` | cheatsheet, `syntax/cynefin.md` | `cynefin` / `cynefin-beta` | `CatalogRenderTests` | Self-loops are rejected because Mermaid ignores them |
| `railroad-ebnf-beta` | cheatsheet, `syntax/railroad.md` | `grammar` / `railroad-ebnf-beta` | `CatalogRenderTests` | Rules are verbatim strings, not a grammar AST |
| `swimlane-beta` | cheatsheet, `syntax/swimlanes.md` | `graph` / `swimlane-beta` | `CatalogRenderTests` | Lane-only features beyond flowchart subgraphs |
| `agentflow-beta` | cheatsheet, `syntax/agentflow.md` | `graph` / `agentflow-beta` | `CatalogRenderTests` | Metadata, `connector`, `global`, collapsed `view` |
| `zenuml` | cheatsheet, `syntax/zenuml.md` | `sequence` / `zenuml` | `CatalogRenderTests` | `if` / `while` / `try` / `par` / `opt` / `new` / aliases. External diagram; `mmdc` loads it |
| `info` | cheatsheet only (no upstream syntax page) | `info` / `info` | `CatalogRenderTests` | Prints the renderer version; nothing to configure |

Hand-author anything in the gaps column. Prefer IR for every other row when the content is already structured data.
