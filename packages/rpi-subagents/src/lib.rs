mod kit;

use crate::kit::{optional_string, string_param};
use serde_json::Value;

fn delegate_task(params: &Value) -> Result<String, String> {
    let task = string_param(params, "task")?;
    if task.trim().is_empty() || task.len() > 20_000 {
        return Err("task must contain 1-20000 characters".into());
    }
    let role = optional_string(params, "role", "worker");
    let max_turns = params
        .get("maxTurns")
        .and_then(Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 32);
    let timeout = params
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(300)
        .clamp(10, 1800);
    Ok(serde_json::json!({
        "kind": "rpi-delegation",
        "task": task,
        "role": role,
        "maxTurns": max_turns,
        "timeoutSeconds": timeout,
        "recursive": false,
    })
    .to_string())
}

export_single_tool_plugin!(
    delegate_task,
    "delegate_task",
    "Create a bounded, non-recursive subagent delegation envelope.",
    r#"{"type":"object","properties":{"task":{"type":"string","maxLength":20000},"role":{"type":"string"},"maxTurns":{"type":"integer","minimum":1,"maximum":32},"timeoutSeconds":{"type":"integer","minimum":10,"maximum":1800}},"required":["task"]}"#
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_delegation_limits() {
        let value = serde_json::json!({"task":"review","maxTurns":999,"timeoutSeconds":99999});
        let result: Value = serde_json::from_str(&delegate_task(&value).unwrap()).unwrap();
        assert_eq!(result["maxTurns"], 32);
        assert_eq!(result["timeoutSeconds"], 1800);
        assert_eq!(result["recursive"], false);
    }
}
