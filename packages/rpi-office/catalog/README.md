# office tool catalog

`office-catalog.json` is generated from the upstream TypeBox catalog
[`@dieulc/pi-office-protocol`](https://github.com/dieuluucanh/pi-for-office/blob/main/packages/protocol/src/office-catalog.ts)
(`packages/protocol/src/office-catalog.ts`, version 0.5.1, catalog v3).

It is committed so the Rust bridge and the task-pane add-in derive the tool
surface from the same op ids and schemas: a pane advertises `excel.read_range`
in `hello.ops`, and this side validates it against exactly that catalog. Keep it
verbatim; do not hand-edit names, op ids, descriptions or schemas.

## What is preserved

- `host`, `op`, `name`, `label`, `description`, `promptSnippet`, `promptGuidelines`
- `parameters` — the TypeBox schema resolved to plain JSON Schema
- `since`, `legacyV1` (the legacy 0.2.x op set is derived from `legacyV1`)
- `catalogVersion`, `protocolVersion`, `hostLabels`, `legacyV1Ops`, `source*`

`promptSnippet`/`promptGuidelines` are kept for parity even though the pinned
`rpi-plugin-sdk` (0.3.20) has no `register_tool_prompt` slot yet; the tool
description still carries the model-facing guidance.

The bridge-only `document` target is **not** in the catalog: it is added to each
tool schema at registration time (`catalog::parameters_with_target`) and stripped
again by the server before the call reaches the pane.

## Regenerating

```sh
mkdir -p /tmp/office-catalog && cd /tmp/office-catalog
curl -sSL -H "Accept: application/vnd.github.raw" \
  https://api.github.com/repos/dieuluucanh/pi-for-office/contents/packages/protocol/src/office-catalog.ts \
  -o catalog.ts
printf '{"name":"gen","private":true,"type":"module"}\n' > package.json
npm install --silent typebox@^1.3.10 esbuild
cp catalog.ts gen/catalog.ts && cd gen
npx esbuild catalog.ts --bundle --format=esm --platform=node --outfile=catalog.mjs
node --input-type=module -e '
import { writeFileSync } from "node:fs";
const m = await import("./catalog.mjs");
const doc = {
  source: "https://github.com/dieuluucanh/pi-for-office packages/protocol/src/office-catalog.ts",
  sourcePackage: "@dieulc/pi-office-protocol",
  sourceVersion: "<package version>",
  catalogVersion: m.CATALOG_VERSION,
  protocolVersion: 3,
  generatedBy: "esbuild + node JSON.stringify of the upstream TypeBox catalog",
  hostLabels: m.HOST_APP_LABEL,
  legacyV1Ops: m.LEGACY_V1_OPS,
  entries: m.OFFICE_CATALOG.map((e) => ({
    host: e.host, op: e.op, name: e.name, label: e.label,
    description: e.description,
    ...(e.promptSnippet ? { promptSnippet: e.promptSnippet } : {}),
    ...(e.promptGuidelines ? { promptGuidelines: e.promptGuidelines } : {}),
    parameters: JSON.parse(JSON.stringify(e.parameters)),
    since: e.since,
    ...(e.legacyV1 ? { legacyV1: true } : {}),
  })),
};
writeFileSync("office-catalog.json", JSON.stringify(doc, null, 2) + "\n");
'
```

Copy the result to `packages/rpi-office/catalog/office-catalog.json`, then:

1. Update the trampoline list in `src/tools.rs` if op ids/names changed — the
   `registered_names_match_catalog` test fails until `EXECUTORS` covers the
   catalog exactly (33 entries today).
2. Update the counts asserted by `catalog::tests::catalog_matches_the_upstream_surface`
   and `README.md` if the catalog size changed.
3. Re-run `cargo test -p rpi-office`.

The add-in side must be updated to the same upstream version; a pane that
advertises an op this catalog does not know is dropped and counted
(`opsIgnoredCount`), and a pane that advertises fewer ops simply exposes fewer
tools.
