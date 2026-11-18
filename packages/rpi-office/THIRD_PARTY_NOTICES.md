# Third-party notices

## pi-for-office (MIT)

`rpi-office` is a Rust port of the bridge half of
[dieuluucanh/pi-for-office](https://github.com/dieuluucanh/pi-for-office) (MIT),
which is itself a fork of pi-for-excel (© tmustier, MIT).

Ported material:

- `packages/protocol/src/protocol.ts` — wire protocol v3, validation rules,
  capability list, error wording → `src/protocol.rs`
- `packages/protocol/src/office-catalog.ts` — the op catalog (committed verbatim
  as `catalog/office-catalog.json`)
- `packages/bridge-extension/src/bridge-server.ts` — pane registry, document
  routing, pending-call correlation, `/health` surface → `src/bridge.rs`,
  `src/server.rs`
- `packages/bridge-extension/src/lifecycle.ts` — manual lifecycle controller →
  `src/lifecycle.rs`
- `packages/bridge-extension/src/active-tools.ts` — tool reconciliation →
  `src/active_tools.rs`
- `packages/bridge-extension/src/pane-replies.ts` — prompt correlation →
  `src/pane_replies.rs`
- `packages/bridge-extension/src/commands.ts` — `/office` grammar and help →
  `src/commands.rs`
- `packages/bridge-extension/src/config.ts` — port/autostart precedence →
  `src/config.rs`

The Office.js task-pane add-in is **not** vendored here; it stays a separate
upstream artifact.

### MIT license text

Copyright (c) pi-for-office contributors, and pi-for-excel contributors
(© tmustier)

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
