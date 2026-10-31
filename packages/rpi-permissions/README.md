# rpi-permissions

Native-compatible permission policy storage for tool-call workflows. Rules use
`Tool(pattern)` entries in `.pi/permissions.json` (project-local) or
`~/.pi/agent/permissions.json` (global fallback). `deny` rules are evaluated
first; when an allow list exists for a tool, unmatched calls are denied.

Use `permissions` with `check`, `grant`, `revoke`, and `list` actions. The
package exposes the policy decision; the rpi host must wire its
`before_tool_call` hook to enforce the decision before running the target tool.

## Tool arguments and safety

Inspect policy with `list`/`check` before an operation. Never use this tool to
self-authorize, remove a denial, or otherwise bypass a rejected operation.
Policy changes require explicit user authorization.

| Argument | Meaning / default |
| --- | --- |
| `action` | Required: `list`, `check`, `grant`, or `revoke`. The parser's legacy omission default is `list`, but the registered schema requires an explicit action. |
| `tool` | For `check`, native tool name such as `Bash` or `Read`; case-insensitive, defaults to `*`. Ignored by other actions. |
| `value` | Required for `check` unless `capability` is supplied: literal command/path, never executed. |
| `rule` | Required for `grant`/`revoke` unless `capability` is supplied. Trimmed, 1–200 **UTF-8 bytes**. `Tool(pattern)` matches that tool; a bare pattern matches any tool. Only `*` is a wildcard. |
| `capability` | Legacy fallback for `value` on check or `rule` on mutation. A present primary field takes precedence (even if invalid). |
| `effect` | `allow` (default) or `deny`. `grant` adds the rule to that list after removing the same exact text from both lists. `grant` with `effect=deny` **adds a deny rule**, not a revocation. `revoke` deletes the exact rule from **both** lists regardless of effect; supplied effect must still be valid. Ignored by check/list. |
| `cwd` | Project directory; omitted/empty uses process working directory. A relative `cwd` is resolved relative to the process working directory; check's `value` is matched literally, not path-resolved. |

Reads use `<cwd>/.pi/permissions.json` when present, otherwise
`$RPI_AGENT_DIR/permissions.json` when set, otherwise
`~/.pi/agent/permissions.json` (`USERPROFILE`, then `HOME`). Missing files
mean empty rules. `grant` and `revoke` **always persist to the project file**,
creating parent directories as needed. If no project file exists, mutation
starts from global fallback rules and writes that resulting policy locally;
it does not edit the global file. Top-level host metadata such as `__rpi`
is accepted; optional fields remain optional.

`check` evaluates deny rules first. Without allow rules for the requested tool,
unspecified calls are allowed; with an allow list, unmatched calls are denied.
Neither `check` nor `list` changes policy. `revoke` is exact-text deletion,
not wildcard deletion of matching rules. The mutation response's legacy
`allowed` field only means `action == "grant"`; it is **not** a policy decision
(for example it is true for a newly added deny rule). Use `check` to evaluate.

```json
{"action":"check","tool":"Bash","value":"git push origin main","cwd":"/project"}
```

After explicit user authorization only:

```json
{"action":"grant","rule":"Bash(git push *)","effect":"deny","cwd":"/project"}
{"action":"revoke","rule":"Bash(git push *)","cwd":"/project"}
```
