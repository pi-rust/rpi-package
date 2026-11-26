# ISO 5807 flowchart profile

ISO 5807:1985 defines documentation symbols and conventions for information-processing
flowcharts. Apply this profile whenever a Mermaid `flowchart` represents program, system, or
data-processing control flow. It does not prescribe sequence, ER, class, state, C4, or Gantt
diagrams.

> [!IMPORTANT]
> Mermaid is a rendering language, not an ISO-conformance tool. Use this profile's semantic
> shapes and conventions and validate the render. Keep the standard as an authoring constraint;
> do not mention it in reader-facing prose unless the user asks for it or it is material to the
> document's purpose.

## Symbol profile

| Meaning | Use in Mermaid | Convention |
|---|---|---|
| Start / end | `([Start])`, `([End])` or `shape: stadium` | One clear entry; explicit terminal exit(s). |
| Process | `[Perform operation]` or `shape: rect` | Imperative, action-oriented label. |
| Decision | `{Condition?}` or `shape: diamond` | State a question; label every outgoing branch. |
| Input / output | `[/Receive input/]` or `shape: lean-r` | Use only for data entering or leaving the process. |
| Predefined process | `[[Invoke named subprocess]]` or `shape: subproc` | Name the separately defined routine. |
| Document | `shape: doc` | Use when the artifact itself is a document. |
| Data store | `[(Store)]` or `shape: cyl` / `datastore` | Name persistent storage and show reads/writes clearly. |
| Connector | `((A))` or `shape: circle` | Use sparingly to avoid long or crossing connectors. |

Use legacy notation only when the live Mermaid documentation confirms it renders in the target
version; otherwise prefer the current typed shape syntax. See
`reference/syntax/flowchart.md` for authoritative syntax.

## Conventions

- Prefer a single top-to-bottom primary flow; use left-to-right only when it materially improves
  legibility. Arrowheads show direction.
- Keep one action or decision per symbol. Avoid overloaded labels and decorative shapes.
- Every decision has explicit, understandable branch labels. Make error and retry paths visible
  when they affect behavior.
- Avoid crossing lines. Split a dense chart, use sub-processes, or use connectors rather than
  creating visually ambiguous routes.
- Use a legend only when non-standard colors, styles, or symbols are unavoidable; never make
  color the sole carrier of meaning.
- For multiple-actor handoffs, prefer a Mermaid swimlane diagram if the target renderer supports
  it, or a `sequenceDiagram` when message order—not control flow—is the question.

Source: [ISO 5807:1985](https://www.iso.org/standard/11955.html).
