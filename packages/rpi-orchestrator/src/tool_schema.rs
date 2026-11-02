//! Model-facing metadata, shared by registration and contract tests.
use rpi_plugin_sdk::StableToolSchema;
use serde_json::{json, Value};

fn worker_properties() -> Value {
    json!({
        "task": {"type":"string", "pattern":"\\S", "description":"Self-contained instructions: goal, exact workspace/files allowed, constraints, deliverables and acceptance checks. No parent conversation is copied. At most 20000 UTF-8 bytes (plan nodes trim before checking; subagent checks the original string)."},
        "role": {"type":"string", "description":"Label only; does not restrict tools or change the child prompt. Standalone subagent default: worker."},
        "provider": {"type":"string", "description":"Usually omit both provider and model to inherit the parent session binding (best effort). An explicit nonblank provider disables inheritance of BOTH fields; specify a compatible model too when needed. Use only a known authenticated provider. Blank strings count as omitted."},
        "model": {"type":"string", "description":"Usually omit with provider. Any explicit nonblank model disables inheritance of BOTH fields; a model-only override is resolved by the child CLI, not paired with the parent provider. Prefer an explicit compatible provider/model pair. Missing host binding falls back to child defaults."},
        "maxTurns": {"type":"integer", "minimum":1, "maximum":32, "default":8, "description":"Hard child turn ceiling, 1..32; default 8."},
        "timeoutSeconds": {"type":"integer", "minimum":10, "maximum":1800, "default":300, "description":"Child wall-clock budget in seconds, 10..1800; default 300. Each node verification gets a separate budget of this length."}
    })
}

fn nonblank_string() -> Value {
    json!({"type":"string", "pattern":"\\S"})
}

fn string_list() -> Value {
    json!({"type":"array", "items":nonblank_string()})
}

fn node_schema() -> Value {
    let mut properties = worker_properties();
    properties["id"] = json!({"type":"string", "pattern":"\\S", "description":"Unique node id; trimmed. Dependencies must name existing ids and form an acyclic graph."});
    properties["deps"] = json!({"type":"array", "items":nonblank_string(), "default":[], "description":"Prerequisite node ids; trimmed and deduplicated. All must be done before this node runs."});
    properties["expectFile"] = json!({"type":"string", "description":"Optional nonblank file path (trimmed), absolute or relative to the shared process cwd. After successful child exit the path must exist (directories also pass); existence only, not proof that this run created or changed it."});
    properties["verify"] = json!({"type":"string", "description":"Optional nonblank shell command (trimmed) run in shared cwd after child success and expectFile check. Must exit 0; use focused acceptance checks, not untrusted commands. Not a sandbox."});
    json!({"type":"object", "properties":properties, "required":["id","task"], "additionalProperties":true})
}

fn parameters(name: &str) -> Value {
    match name {
        "subagent" => {
            json!({"type":"object", "properties":worker_properties(), "required":["task"], "additionalProperties":true})
        }
        "plan" => json!({
            "type":"object", "additionalProperties":true,
            "properties":{
                "action":{"type":"string", "enum":["set","update","show"], "default":"show"},
                "nodes":{"type":"array", "minItems":1, "items":node_schema(), "description":"action=set: complete replacement graph; at least one node. There is no runtime node-count cap."},
                "addNodes":{"type":"array", "items":node_schema(), "description":"action=update: append new ids, before removals and dependency edits. Empty array is allowed."},
                "removeIds":{"type":"array", "items":nonblank_string(), "description":"action=update: remove ids (trimmed); unknown ids are ignored. Repair remaining dependencies in the same update."},
                "setDeps":{"type":"array", "items":{"type":"object", "properties":{"id":{"type":"string", "description":"Exact existing node id; unlike creation this field is NOT trimmed."}, "deps":string_list()}, "required":["id","deps"], "additionalProperties":true}, "description":"action=update: replace dependencies for existing ids; empty deps clears them. Dependency ids are trimmed and deduplicated."},
                "reset":{"type":"array", "items":nonblank_string(), "description":"action=update: re-arm existing ids to pending and clear their results. Also reset skipped dependents when retrying; this is not automatic."}
            },
            "allOf":[{"if":{"properties":{"action":{"const":"set"}},"required":["action"]},"then":{"required":["nodes"]}}]
        }),
        "dispatch" => {
            json!({"type":"object", "additionalProperties":true, "properties":{"maxParallel":{"type":"integer", "minimum":1, "maximum":16, "default":4, "description":"Concurrent children within this wave, 1..16; default 4. All nodes ready at wave start are attempted, in batches if needed."}}})
        }
        _ => unreachable!("unknown orchestrator tool"),
    }
}

pub(super) fn tool_schema(name: &str) -> StableToolSchema {
    let description = match name {
        "subagent" => "Run one bounded task in a fresh rpi subprocess, not the host-managed in-memory delegate_task. No parent conversation is copied; pass scope, file ownership, constraints and acceptance criteria in task. The child shares cwd/files/environment (no isolated worktree or sandbox), so avoid concurrent writes to the same files. Returns bounded output, not proof of task completion; inspect and verify it. Children cannot call subagent/plan/dispatch. Prefer delegate_task for host-managed delegation on the current execution binding, or plan + dispatch for dependency scheduling.",
        "plan" => "Author the session-scoped in-memory execution DAG, NOT plan_mode's read-only design/approval workflow. set replaces nodes; update applies addNodes, removeIds, setDeps, reset in that order and validates atomically; show (default) inspects. Only edit between dispatch waves. Nodes need self-contained tasks and non-overlapping write ownership in the shared workspace. Declare expectFile/verify acceptance checks: only dispatch can mark completion, and exit 0 alone can include a turn-limited incomplete answer. Session switches clear this graph; it is not a durable queue.",
        "dispatch" => "Execute one ready wave of the current plan using bounded rpi child processes. Call plan(set) first. All pending nodes whose dependencies are done at wave start run in batches up to maxParallel; newly unblocked nodes wait for the next call. Overlapping dispatch/edit calls are rejected. Inspect results and acceptance checks before the next wave; failed dependencies cause skipped nodes, and retries require explicit plan(update, reset). Shared files are not isolated: parallelize only independent work. Returns bounded per-node summaries and the updated graph.",
        _ => unreachable!("unknown orchestrator tool"),
    };
    crate::kit::schema(name, description, &parameters(name).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Node, MAX_TASK_BYTES};

    fn registered(name: &str) -> Value {
        let schema = tool_schema(name);
        assert_eq!(schema.name.to_string_lossy(), name);
        assert!(!schema.description.to_string_lossy().is_empty());
        serde_json::from_str(&schema.parameters.to_string_lossy()).unwrap()
    }

    #[test]
    fn registered_schemas_are_json_and_allow_host_context() {
        for name in ["subagent", "plan", "dispatch"] {
            let schema = registered(name);
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["additionalProperties"], true); // includes __rpi
            assert!(schema["properties"].is_object());
        }
        assert_eq!(registered("subagent")["required"], json!(["task"]));
        let parallel = &registered("dispatch")["properties"]["maxParallel"];
        assert_eq!(
            (
                parallel["minimum"].as_u64(),
                parallel["maximum"].as_u64(),
                parallel["default"].as_u64()
            ),
            (Some(1), Some(16), Some(4))
        );
    }

    #[test]
    fn plan_nested_items_and_conditional_requirement_match_parser() {
        let schema = registered("plan");
        let props = &schema["properties"];
        assert_eq!(props["nodes"]["minItems"], 1);
        assert_eq!(props["nodes"]["items"], props["addNodes"]["items"]);
        assert_eq!(props["nodes"]["items"]["required"], json!(["id", "task"]));
        let edit = &props["setDeps"]["items"];
        assert_eq!(edit["required"], json!(["id", "deps"]));
        assert_eq!(edit["properties"]["deps"]["items"]["type"], "string");
        assert_eq!(schema["allOf"][0]["then"]["required"], json!(["nodes"]));
        assert_eq!(props["action"]["default"], "show");
        assert!(Graph::from_nodes_value(&json!([])).is_err());
        assert!(Node::from_value(&json!({"id":"a"})).is_err());
        assert!(
            Graph::from_nodes_value(&json!([{"id":"a","task":"x","deps":["missing"]}])).is_err()
        );
    }

    #[test]
    fn node_defaults_bounds_and_utf8_rules_match_registered_metadata() {
        let schema = registered("plan");
        let fields = &schema["properties"]["nodes"]["items"]["properties"];
        let node = Node::from_value(&json!({"id":" a ","task":" x ","deps":["b"," b "]})).unwrap();
        assert_eq!(node.id, "a");
        assert_eq!(node.task, "x");
        assert_eq!(node.deps, vec!["b"]);
        assert_eq!(Some(node.max_turns), fields["maxTurns"]["default"].as_u64());
        assert_eq!(
            Some(node.timeout_secs),
            fields["timeoutSeconds"]["default"].as_u64()
        );
        let clamped =
            Node::from_value(&json!({"id":"a","task":"x","maxTurns":99,"timeoutSeconds":0}))
                .unwrap();
        assert_eq!(
            Some(clamped.max_turns),
            fields["maxTurns"]["maximum"].as_u64()
        );
        assert_eq!(
            Some(clamped.timeout_secs),
            fields["timeoutSeconds"]["minimum"].as_u64()
        );
        assert!(
            Node::from_value(&json!({"id":"a","task":"界".repeat(MAX_TASK_BYTES / 3 + 1)}))
                .is_err()
        );
        // Bounds apply to trimmed node tasks, not the raw JSON string length.
        assert!(Node::from_value(
            &json!({"id":"a","task":format!("{}x", " ".repeat(MAX_TASK_BYTES))})
        )
        .is_ok());
    }
}
