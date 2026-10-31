//! The `plan` tool: author and edit the task DAG.
//!
//! Scheduling lives in `dispatch`; this tool only changes the graph's *shape*
//! (and can re-arm node state). Every result carries the graph in `details` so
//! it can be replayed from the session branch.
//!
//! There is deliberately **no** way for the model to mark a node `done`/`failed`:
//! completion is only ever written by `dispatch`, so the plan cannot be
//! "completed" by hand. `reset` is the one state edit the model keeps.

use crate::control::Control;
use crate::graph::{Graph, Node, Status};
use crate::kit::{result_with_details, Progress};
use crate::lock_graph;
use serde_json::Value;

pub fn plan(params: &Value, control: &Control, _progress: &Progress) -> Result<String, String> {
    control.check()?;
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("show");
    let mut guard = lock_graph(params);
    if matches!(action, "set" | "update") && guard.is_running() {
        return Err(
            "dispatch is running: wait for the wave to finish before editing the plan".into(),
        );
    }
    let graph = guard.graph_mut();
    match action {
        "set" => {
            let nodes = params
                .get("nodes")
                .ok_or("`nodes` is required for action=set")?;
            *graph = Graph::from_nodes_value(nodes)?;
        }
        "update" => {
            let mut next = graph.clone();
            apply_update(&mut next, params)?;
            next.validate()?;
            *graph = next;
        }
        "show" => {}
        other => {
            return Err(format!(
                "unknown plan action `{other}` (expected set, update, show)"
            ))
        }
    }
    let result = result_with_details(&graph.render(), graph.to_details());
    if matches!(action, "set" | "update") {
        guard.advance_generation();
    }
    Ok(result)
}

fn apply_update(graph: &mut Graph, params: &Value) -> Result<(), String> {
    if let Some(add) = params.get("addNodes") {
        let array = add.as_array().ok_or("`addNodes` must be an array")?;
        for item in array {
            let node = Node::from_value(item)?;
            if graph.get(&node.id).is_some() {
                return Err(format!("node `{}` already exists", node.id));
            }
            graph.nodes.push(node);
        }
    }
    if let Some(remove) = params.get("removeIds") {
        let array = remove.as_array().ok_or("`removeIds` must be an array")?;
        for item in array {
            let id = item
                .as_str()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .ok_or("`removeIds` must contain non-empty strings")?;
            graph.nodes.retain(|node| node.id != id);
        }
    }
    if let Some(set_deps) = params.get("setDeps") {
        let array = set_deps.as_array().ok_or("`setDeps` must be an array")?;
        for item in array {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .ok_or("each `setDeps` entry needs an `id`")?;
            let deps = item
                .get("deps")
                .and_then(Value::as_array)
                .ok_or("each `setDeps` entry needs a `deps` array")?;
            let mut parsed = Vec::with_capacity(deps.len());
            for dep in deps {
                let dep = dep
                    .as_str()
                    .map(str::trim)
                    .filter(|dep| !dep.is_empty())
                    .ok_or("`deps` must contain non-empty strings")?
                    .to_string();
                // Match Node::from_value: duplicate edges are one dependency,
                // otherwise the cycle detector's indegree can never reach zero.
                if !parsed.contains(&dep) {
                    parsed.push(dep);
                }
            }
            let node = graph
                .get_mut(id)
                .ok_or_else(|| format!("node `{id}` not found"))?;
            node.deps = parsed;
        }
    }
    if let Some(reset) = params.get("reset") {
        let array = reset.as_array().ok_or("`reset` must be an array")?;
        for item in array {
            let id = item
                .as_str()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .ok_or("`reset` must contain non-empty strings")?;
            let node = graph
                .get_mut(id)
                .ok_or_else(|| format!("node `{id}` not found"))?;
            node.status = Status::Pending;
            node.result = None;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seeded() -> Graph {
        Graph::from_nodes_value(&json!([
            {"id": "a", "task": "x"},
            {"id": "b", "task": "y", "deps": ["a"]}
        ]))
        .unwrap()
    }

    #[test]
    fn update_can_add_remove_and_reset() {
        let mut g = seeded();
        apply_update(
            &mut g,
            &json!({"addNodes": [{"id": "c", "task": "z", "deps": ["b"]}]}),
        )
        .unwrap();
        assert_eq!(g.nodes.len(), 3);

        apply_update(&mut g, &json!({"setDeps": [{"id": "b", "deps": []}]})).unwrap();
        assert!(g.get("b").unwrap().deps.is_empty());

        apply_update(&mut g, &json!({"reset": ["a"]})).unwrap();
        assert_eq!(g.get("a").unwrap().status, Status::Pending);

        apply_update(&mut g, &json!({"removeIds": ["c"]})).unwrap();
        assert!(g.get("c").is_none());
    }

    #[test]
    fn dependency_updates_accept_duplicate_edges_like_plan_creation() {
        let mut graph = seeded();
        apply_update(
            &mut graph,
            &json!({"setDeps":[{"id":"b","deps":["a", " a "]}]}),
        )
        .unwrap();
        graph
            .validate()
            .expect("duplicate dependency edges are not a cycle");
        assert_eq!(graph.get("b").unwrap().deps, vec!["a"]);
        assert_eq!(graph.ready(), vec!["a"]);
        graph.get_mut("a").unwrap().status = Status::Done;
        assert_eq!(graph.ready(), vec!["b"]);
    }

    #[test]
    fn dependency_edits_require_nested_id_and_deps_array() {
        for entry in [
            json!({"deps": []}),
            json!({"id": "b"}),
            json!({"id": "b", "deps": "a"}),
            json!({"id": "b", "deps": [null]}),
            json!({"id": "b", "deps": ["  "]}),
            json!({"id": " b ", "deps": []}), // exact id lookup, no trim
        ] {
            assert!(apply_update(&mut seeded(), &json!({"setDeps": [entry]})).is_err());
        }
        let mut graph = seeded();
        apply_update(
            &mut graph,
            &json!({"addNodes": [], "setDeps": [{"id": "b", "deps": []}], "__rpi": {"trace": [1]}}),
        )
        .unwrap();
        graph.validate().unwrap();
        assert!(graph.get("b").unwrap().deps.is_empty());
    }

    #[test]
    fn update_cannot_mark_nodes_done() {
        // Completion is dispatch-only: there is no `status` action, so a stray
        // `status` field is ignored and cannot hand-mark a node done.
        let mut g = seeded();
        apply_update(&mut g, &json!({"status": [{"id": "a", "status": "done"}]})).unwrap();
        assert_eq!(g.get("a").unwrap().status, Status::Pending);
    }

    #[test]
    fn update_rejects_unknown_dep_and_cycle() {
        let mut g = seeded();
        // A missing dep is caught by validate(), not apply_update().
        apply_update(
            &mut g,
            &json!({"setDeps": [{"id": "b", "deps": ["ghost"]}]}),
        )
        .unwrap();
        assert!(g.validate().unwrap_err().contains("unknown"));

        let mut g = seeded();
        apply_update(&mut g, &json!({"setDeps": [{"id": "a", "deps": ["b"]}]})).unwrap();
        assert!(g.validate().unwrap_err().contains("cycle"));
    }
}
