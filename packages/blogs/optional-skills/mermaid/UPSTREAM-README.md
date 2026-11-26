# Mermaid Skill

An [Agent Skills](https://agentskills.io/specification)-compatible skill for authoring,
validating, and deterministically generating [Mermaid](https://mermaid.js.org/) diagrams. Built
for Claude Code, and portable to any tool that reads the open `SKILL.md` format.

- **Cached syntax, checked against the renderer.** Per-type syntax lives in
  `mermaid/reference/syntax/` (version stamp in `mermaid/reference/diagram-types.md`).
  `scripts/check.sh` uses the installed Mermaid CLI, which may be newer than that stamp.
  Escalate to live docs or `scripts/refresh.sh` when a type is missing from the cache.
- **Never presented unverified.** Every diagram is render-validated (via an MCP render tool or
  the bundled `scripts/check.sh`) before it's shown, including detection of Mermaid's silent
  error-placeholder SVG.
- **Deterministic generation from structured data.** When a diagram's content is already
  structured data on disk (a dependency graph, a timing ledger, a state machine, a sequence, a
  requirement-traceability table), `mermaid/scripts/render.py` generates the exact Mermaid source
  from a small JSON intermediate representation (IR) instead of hand-authoring it — see
  [`mermaid/reference/ir.md`](mermaid/reference/ir.md) and
  [`mermaid/reference/ir-catalog.md`](mermaid/reference/ir-catalog.md). The matrix of every
  open-source diagram type, its IR, its tests, and remaining gaps is
  [`mermaid/reference/coverage.md`](mermaid/reference/coverage.md).

## Install

The [`mermaid/`](mermaid/) folder in this repository *is* the Agent Skill package — copy it
as-is into your tool's Agent Skills directory (e.g. `~/.claude/skills/mermaid/`,
`~/.agents/skills/mermaid/` — see the [Agent Skills spec](https://agentskills.io/specification)
for every supported tool's path). Nothing outside `mermaid/` (this README, the license, CI) is
part of the installed skill.

## Testing

```bash
cd mermaid && python3 -m pytest tests/ -q
```

Every positive-case test in `tests/test_render.py` is a real render through `scripts/check.sh`
(an installed `mmdc`, otherwise `npx -y @mermaid-js/mermaid-cli@12.0.0`), not just a string match.
GitHub Actions installs that same pin on Node 22. Mermaid 11 does not register `agentflow-beta`.

## License

MIT — see [LICENSE](LICENSE).
