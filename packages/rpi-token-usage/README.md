# rpi-token-usage

Token visibility for rpi. The `token_count` tool estimates tokens for a text
using a deterministic character heuristic. The package also registers a
`token-usage` message renderer that accepts a JSON payload containing
`usage.input`, `usage.output`, and optional cache fields, returning a compact
TUI line such as `tokens: in 1.2k | out 320 | total 1.5k`.

Hosts can invoke the renderer for a custom message payload with
`customType: "token-usage"`; the tool works in every host that supports normal
plugin tools.
