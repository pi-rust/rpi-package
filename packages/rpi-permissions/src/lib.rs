#![forbid(unsafe_code)]

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    FreeStringFn, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

static HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    params: Value,
    done: bool,
}
#[derive(Default, Clone, Debug)]
struct Rules {
    allow: Vec<String>,
    deny: Vec<String>,
}

fn project_root(p: &Value) -> PathBuf {
    p.get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn local_path(p: &Value) -> PathBuf {
    project_root(p).join(".pi").join("permissions.json")
}

fn global_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("RPI_AGENT_DIR") {
        return PathBuf::from(dir).join("permissions.json");
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".pi").join("agent").join("permissions.json")
}

fn parse_rules(value: Value) -> Result<Rules, String> {
    let root = value.get("permissions").unwrap_or(&value);
    if let Some(obj) = root.as_object() {
        let strings = |key: &str| {
            obj.get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        return Ok(Rules {
            allow: strings("allow"),
            deny: strings("deny"),
        });
    }
    // Compatibility with the first rpi release, which stored capability
    // objects as a top-level array.
    if let Some(items) = value.as_array() {
        return Ok(Rules {
            allow: items
                .iter()
                .filter(|r| r.get("effect").and_then(Value::as_str) == Some("allow"))
                .filter_map(|r| r.get("capability").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect(),
            deny: Vec::new(),
        });
    }
    Err("permissions must be an object with allow/deny arrays".into())
}

fn load(path: &PathBuf) -> Result<Rules, String> {
    if !path.exists() {
        return Ok(Rules::default());
    }
    let text = fs::read_to_string(path).map_err(|e| format!("read permissions: {e}"))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("parse permissions: {e}"))?;
    parse_rules(value)
}

fn save(path: &PathBuf, rules: &Rules) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create permission directory: {e}"))?;
    }
    let text = serde_json::to_string_pretty(
        &json!({"permissions": {"allow": rules.allow, "deny": rules.deny}}),
    )
    .map_err(|e| e.to_string())?;
    write_store_atomically(path, &text)
}

/// Write `text` to `path` via a temp sibling + rename.
///
/// `fs::write` truncates and then writes in place, so a shorter document written
/// over a longer one leaves the previous tail behind, and two rpi sessions
/// sharing a project can interleave their writes. Either way the rule file
/// becomes unparsable. `rename` replaces atomically on Unix and on Windows
/// (`MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`), so a reader only ever sees a
/// whole document.
fn write_store_atomically(path: &PathBuf, text: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "permissions.json".to_string());
    let tmp = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
    fs::write(&tmp, text).map_err(|e| format!("write permissions: {e}"))?;
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("commit permissions: {error}"));
    }
    Ok(())
}

fn split_rule(rule: &str) -> (&str, &str) {
    if let Some(open) = rule.find('(') {
        if rule.ends_with(')') && open > 0 {
            return (&rule[..open], &rule[open + 1..rule.len() - 1]);
        }
    }
    ("*", rule)
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let (mut pi, mut vi, mut star, mut mark) = (0usize, 0usize, None, 0usize);
    let p = pattern.as_bytes();
    let v = value.as_bytes();
    while vi < v.len() {
        if pi < p.len() && (p[pi] == v[vi]) {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = vi;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

fn rule_matches(rule: &str, tool: &str, target: &str) -> bool {
    let (rule_tool, pattern) = split_rule(rule);
    (rule_tool == "*" || rule_tool.eq_ignore_ascii_case(tool)) && glob_matches(pattern, target)
}
fn permissions(p: &Value) -> Result<String, String> {
    let local = local_path(p);
    let file = if local.exists() {
        local.clone()
    } else {
        global_path()
    };
    let mut rules = load(&file)?;
    let action = p.get("action").and_then(Value::as_str).unwrap_or("list");
    match action {
        "list" => Ok(
            json!({"action":"list","rules":{"allow":rules.allow,"deny":rules.deny},"path":file})
                .to_string(),
        ),
        "check" => {
            let tool = p.get("tool").and_then(Value::as_str).unwrap_or("*");
            let target = p
                .get("value")
                .or_else(|| p.get("capability"))
                .and_then(Value::as_str)
                .ok_or("value (or capability) is required")?;
            let denied = rules.deny.iter().any(|r| rule_matches(r, tool, target));
            let has_allow_for_tool = rules
                .allow
                .iter()
                .any(|r| split_rule(r).0 == "*" || split_rule(r).0.eq_ignore_ascii_case(tool));
            let allowed = !denied
                && (!has_allow_for_tool
                    || rules.allow.iter().any(|r| rule_matches(r, tool, target)));
            Ok(json!({"action":"check","tool":tool,"value":target,"allowed":allowed,"reason":if denied{"deny rule"}else if !allowed{"not in allow rules"}else{"allowed"},"default":"allow-unless-allow-rules-exist"}).to_string())
        }
        "grant" | "revoke" => {
            let capability = p
                .get("rule")
                .or_else(|| p.get("capability"))
                .and_then(Value::as_str)
                .ok_or("capability is required")?
                .trim();
            if capability.is_empty() || capability.len() > 200 {
                return Err("capability must contain 1-200 characters".into());
            }
            let effect = p.get("effect").and_then(Value::as_str).unwrap_or("allow");
            if effect != "allow" && effect != "deny" {
                return Err("effect must be allow or deny".into());
            }
            rules.allow.retain(|r| r != capability);
            rules.deny.retain(|r| r != capability);
            if action == "grant" {
                if effect == "allow" {
                    rules.allow.push(capability.to_owned());
                } else {
                    rules.deny.push(capability.to_owned());
                }
            }
            save(&local, &rules)?;
            Ok(
                json!({"action":action,"rule":capability,"effect":effect,"allowed":action=="grant"})
                    .to_string(),
            )
        }
        _ => Err("action must be one of: check, grant, revoke, list".into()),
    }
}
extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    HANDLES.insert(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        done: false,
    })
}
extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null permissions handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut d = entry.lock();
    if entry.cancelled.load(Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("permissions cancelled".into()));
    }
    if d.done {
        return StepResult::err(StbString::from_string(
            "permissions polled after completion".into(),
        ));
    }
    d.done = true;
    match permissions(&d.params) {
        Ok(t) => StepResult::done(StbString::from_string(
            json!({"content":[{"type":"text","text":t}]}).to_string(),
        )),
        Err(e) => StepResult::err(StbString::from_string(e)),
    }
}
extern "C" fn cancel(h: StepHandle) {
    HANDLES.cancel(h);
}
extern "C" fn destroy(h: StepHandle) {
    drop(HANDLES.remove(h));
}
const TOOL_DESCRIPTION: &str = "Inspect tool-call permission rules with list/check before an operation. Only with explicit user authorization, use grant/revoke to persist project policy in <cwd>/.pi/permissions.json. grant effect=deny ADDS a deny rule; revoke DELETES the exact rule from both lists. Never self-authorize or remove a denial to bypass a rejected operation. Deny rules win; this tool reports policy and does not execute or enforce the target call. Returns JSON text: list includes full rules/path (no output cap); check includes allowed/reason and defaults to allow unless applicable allow rules exist. Mutation results describe the action, not effective permission: allowed=true means grant, EVEN for effect=deny; use check to evaluate the resulting policy.";
const TOOL_PARAMETERS: &str = r#"{
  "type": "object",
  "description": "Inspect policy or explicitly authorized project-local rule changes. Unknown top-level host metadata (including __rpi) is accepted.",
  "properties": {
    "action": {"type":"string","enum":["check","grant","revoke","list"],"description":"Required action: list reads effective rules; check evaluates a target without running it; grant adds/moves an exact rule into effect's list; revoke removes that exact rule from BOTH lists regardless of effect. Mutations persist project-local policy."},
    "tool": {"type":"string","default":"*","description":"For check: native tool name (for example Bash, Read, Write, Edit); case-insensitive match. Omitted means *. Ignored by list/grant/revoke."},
    "value": {"type":"string","description":"Required for check unless capability is supplied: literal command or path to evaluate, not a rule expression. Takes precedence over capability; check never executes it."},
    "rule": {"type":"string","description":"Required for grant/revoke unless capability is supplied. Exact rule text, trimmed, 1-200 UTF-8 bytes after trimming; for example Bash(git push *). Only * is a wildcard; a bare pattern matches any tool. Takes precedence over capability; revoke deletes exact text, not matching rules."},
    "capability": {"type":"string","description":"Legacy fallback for check's value or grant/revoke's rule; ignored when the corresponding primary field is present. Uses that field's limits and meaning."},
    "effect": {"type":"string","enum":["allow","deny"],"default":"allow","description":"For grant: allow adds an allow rule; deny ADDS a deny rule (not removal). Both remove the same exact rule from either list first. For revoke: validated but does not choose a list; both lists are cleared of the rule. Ignored by check/list."},
    "cwd": {"type":"string","description":"Project directory for .pi/permissions.json; omitted or empty uses the plugin process working directory. Reads use the project file if present, otherwise RPI_AGENT_DIR/permissions.json or ~/.pi/agent/permissions.json. grant/revoke always write the project file (creating directories), seeding it from fallback rules if needed; never write global policy."}
  },
  "required": ["action"],
  "additionalProperties": true
}"#;

fn tool_schema() -> StableToolSchema {
    StableToolSchema {
        name: StbString::from_string("permissions".into()),
        description: StbString::from_string(TOOL_DESCRIPTION.into()),
        parameters: StbString::from_string(TOOL_PARAMETERS.into()),
    }
}

use rpi_plugin_sdk::callbacks::free_string;
rpi_plugin_sdk::export_plugin!(|api| {
    let Some(register) = api.register_tool else {
        return 1;
    };
    let schema = Box::new(tool_schema());
    let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
    drop(schema);
    rc
});
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_schema_documents_policy_without_requiring_optional_fields() {
        // The registration uses this same constructor, not a test-only schema copy.
        let schema = tool_schema();
        assert_eq!(schema.name.to_string_lossy(), "permissions");
        let description = schema.description.to_string_lossy();
        assert!(description.contains("grant effect=deny ADDS"));
        assert!(description.contains("revoke DELETES"));
        assert!(description.contains("Never self-authorize"));
        let params: Value = serde_json::from_str(&schema.parameters.to_string_lossy()).unwrap();
        assert_eq!(params["type"], "object");
        assert_eq!(params["required"], json!(["action"]));
        assert_eq!(params["additionalProperties"], true); // Includes host __rpi.
        let properties = params["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 7);
        for property in properties.values() {
            assert!(!property["description"].as_str().unwrap().is_empty());
        }
        assert_eq!(
            properties["action"]["enum"],
            json!(["check", "grant", "revoke", "list"])
        );
        assert_eq!(properties["effect"]["default"], "allow");
        assert_eq!(properties["tool"]["default"], "*");
        assert!(properties["cwd"]["description"]
            .as_str()
            .unwrap()
            .contains("never write global"));
        assert!(properties["rule"]["description"]
            .as_str()
            .unwrap()
            .contains("UTF-8 bytes"));
        schema.name.free_with(Some(free_string));
        schema.description.free_with(Some(free_string));
        schema.parameters.free_with(Some(free_string));
    }

    #[test]
    fn grant_deny_adds_and_revoke_removes_from_project_store() {
        let dir =
            std::env::temp_dir().join(format!("rpi-permissions-metadata-{}", std::process::id()));
        let path = dir.join(".pi").join("permissions.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"permissions":{"allow":[],"deny":[]}}"#).unwrap();
        permissions(&json!({"action":"grant","cwd":dir,"rule":"Bash(git push *)","effect":"deny"}))
            .unwrap();
        let stored = load(&path).unwrap();
        assert_eq!(stored.deny, vec!["Bash(git push *)"]);
        assert!(stored.allow.is_empty());
        let check: Value = serde_json::from_str(
            &permissions(
                &json!({"action":"check","cwd":dir,"tool":"Bash","value":"git push origin main"}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(check["allowed"], false);
        permissions(
            &json!({"action":"revoke","cwd":dir,"rule":"Bash(git push *)","effect":"allow"}),
        )
        .unwrap();
        let stored = load(&path).unwrap();
        assert!(stored.deny.is_empty());
        assert!(stored.allow.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn default_allows_without_allow_list() {
        let out = permissions(&json!({"action":"check","tool":"Bash","value":"rm -rf /"})).unwrap();
        // With no allow list, native pi-permissions permits unspecified calls;
        // an explicit deny rule is what blocks a dangerous operation.
        assert!(out.contains("true"));
    }

    #[test]
    fn deny_rule_wins_over_allow() {
        let dir = std::env::temp_dir().join(format!("rpi-permissions-{}", std::process::id()));
        let allow = dir.join(".pi").join("permissions.json");
        fs::create_dir_all(allow.parent().unwrap()).unwrap();
        fs::write(
            &allow,
            r#"{"permissions":{"allow":["Bash(git *)"],"deny":["Bash(git push *)"]}}"#,
        )
        .unwrap();
        let out = permissions(
            &json!({"action":"check","cwd":dir,"tool":"Bash","value":"git push origin main"}),
        )
        .unwrap();
        assert!(out.contains("false"));
        fs::remove_dir_all(dir).ok();
    }
}
