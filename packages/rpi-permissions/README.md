# rpi-permissions

Native-compatible permission policy storage for tool-call workflows. Rules use
`Tool(pattern)` entries in `.pi/permissions.json` (project-local) or
`~/.pi/agent/permissions.json` (global fallback). `deny` rules are evaluated
first; when an allow list exists for a tool, unmatched calls are denied.

Use `permissions` with `check`, `grant`, `revoke`, and `list` actions. The
package exposes the policy decision; the rpi host must wire its
`before_tool_call` hook to enforce the decision before running the target tool.
