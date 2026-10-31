# rpi-codegraph

A Rust rpi plugin that exposes the upstream CodeGraph index through focused
native tools. Every call (including `codegraph_status`) first runs
`codegraph init <project>` if `.codegraph/` is absent, then runs
`codegraph sync <project>`. These steps can scan the workspace and **write the
project index**. It then starts `codegraph serve --mcp --path <project>` and
proxies a query to the local index; successful response text is bounded.

## Requirements

Install the upstream CLI (available on PATH) and select an accessible project
directory. A previously initialized project is **not** required:

```bash
npm install -g @colbymchenry/codegraph
```

The plugin automatically initializes and synchronizes the index on each call.
Do not use these tools when writing index files is not permitted; use direct
`read`, `grep` or `find` instead.

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
- `codegraph_status` — index health and pending state **after** init/sync;
  not a read-only view of pre-sync state

## Parameters and result boundaries

Every tool accepts an optional absolute `projectPath` pointing to an accessible
directory; omitted or blank uses the process current working directory. The
schema accepts extra host metadata such as `__rpi`. Optional fields remain
optional; the plugin forwards parameters without injecting schema defaults or
clamping numeric values. Actual resolution, normalization, caps and support
for optional selectors depend on the installed upstream CodeGraph version.

| Tool | Required in schema | Optional controls / registered default hints |
| --- | --- | --- |
| `codegraph_search` | `query`: symbol name/partial name | `kind`: one of the listed symbol kinds, omitted means all; `limit`: 10 |
| `codegraph_callers` / `codegraph_callees` | `symbol`: definition name | `file`: definition path/suffix for ambiguity; `limit`: 20 |
| `codegraph_impact` | `symbol`: definition name | `file`: definition path/suffix; `depth`: 2 dependency levels |
| `codegraph_explore` | `query`: question or symbol/file/code terms | `maxFiles`: registered hint 8; omitted upstream default can differ (the locally inspected upstream uses 12) |
| `codegraph_node` | none structurally; supply **symbol or file** in use | `includeCode`: false; `file`/`line` disambiguate symbol mode; file mode uses `offset`/`limit`, `symbolsOnly`: false |
| `codegraph_files` | none | `path`: directory; `pattern`: glob; `format`: tree/flat/grouped (tree); `includeMetadata`: true; `maxDepth`: unlimited if omitted |
| `codegraph_status` | none | `projectPath` only |

For node file mode, omit `symbol` and provide a file path or basename. Source is
line-numbered; `offset` is a 1-based starting line (omitted: beginning), `limit`
requests a maximum line count (the locally inspected upstream defaults to 2,000
lines, with a separate character budget; 2,000 is not a hard line cap).
`symbolsOnly=true` requests an outline/dependents rather than source. In symbol
mode `includeCode=true` requests source; containers may return member outlines,
and `line` selects a definition, not a source read offset. Older upstream
versions may not support file mode or definition selectors: use direct reads
rather than assuming those parameters were honored.

Successful response text is truncated at a Unicode-safe boundary to at most
25,000 characters **plus** a truncation marker, possibly ending at an earlier
newline. This bounds returned context, not total index work or process memory.
The **subsequent MCP session** (handshake and query together) has a 20-second
response deadline. CLI discovery, automatic init/sync and process startup are
outside that deadline; init/sync are blocking operations without that timeout
or cancellation checks. Do not describe the whole call as having a 20-second
limit. The MCP child is killed/waited after the session operation.

Indexed file listings are not complete filesystem inventories. Relationships
are best-effort static results, not exhaustive runtime paths, a compiler check
or proof of impact completeness. Sync cannot prevent concurrent edits or
unsupported-language gaps. If results are stale, empty, ambiguous, truncated,
or the CLI/index is unavailable, narrow queries or verify using `read`, `grep`
and `find`; do not repeatedly retry an unavailable index. For truncated node
file source, resume with an explicit later `offset` and smaller `limit`.

## Metadata regression tests

Unit tests inspect the actual `tool_specs()` used by registration, parse every
schema as JSON, and check object/property shape, required versus optional
fields, host metadata compatibility, default hints, enums and concise shared
CLI/index-write/truncation/fallback guidance. Detailed timeout boundaries stay
in this README rather than being repeated in every tool description. They do
not invoke CodeGraph or change index behavior.
