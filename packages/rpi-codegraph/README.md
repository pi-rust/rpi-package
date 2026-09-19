# rpi-codegraph

A Rust rpi plugin that exposes the upstream CodeGraph index through focused
native tools. It does not scan and dump the whole workspace on every request;
it starts `codegraph serve --mcp --path <project>` and proxies bounded MCP
queries to the local `.codegraph` SQLite index.

## Requirements

Install the upstream CLI and initialize each project first:

```bash
npm install -g @colbymchenry/codegraph
cd /path/to/project
codegraph init -i
```

On Windows the plugin discovers `codegraph` through PowerShell, so npm and
Scoop installations do not need a hardcoded path.

## Tools

The plugin registers:

- `codegraph_search` — symbol search, locations only
- `codegraph_node` — one symbol or one indexed source file
- `codegraph_callers` / `codegraph_callees` — call relationships
- `codegraph_impact` — bounded dependency impact
- `codegraph_explore` — relevant source grouped by file
- `codegraph_files` — indexed file tree with filters
- `codegraph_status` — index health and pending sync state

Every tool accepts an optional absolute `projectPath`; otherwise it uses the
agent's current working directory. Responses are capped at 25,000 characters,
and sessions time out after 20 seconds to avoid context bloat or a stuck child
process.

If a project is not indexed, use the built-in `read`, `grep`, and `find` tools
instead of repeatedly retrying CodeGraph.
