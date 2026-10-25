use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const SUPPORTED_COMMANDS: &[&str] = &[
    "get_state",
    "get_messages",
    "get_history",
    "get_session_branch",
    "append_entry",
    "get_stats",
    "get_queue",
    "queue_message",
    "cancel_queued",
    "clear_queue",
    "navigate",
    "prompt",
    "resume_pending",
    "compact",
    "abort",
    "get_available_models",
    "get_auth_status",
    "oauth",
    "set_api_key",
    "set_model",
    "get_available_thinking_levels",
    "set_thinking_level",
    "get_available_tools",
    "set_active_tools",
    "ui_dialog_response",
    "ping",
    "stop",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCommand {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(flatten)]
    pub payload: serde_json::Map<String, Value>,
}

pub fn capabilities() -> Value {
    json!({
        "protocol": "rpi",
        "piCompatibilityTarget": "1.2.0",
        "supportedCommands": SUPPORTED_COMMANDS,
        "optionalCommands": ["get_session_branch", "append_entry"],
        "supportedActions": ["history", "queue", "queue_status", "fork", "clone", "stats", "stats_breakdown", "history_mutation", "provider_auth", "oauth", "session_metadata", "projects", "create_project", "session_organization", "files", "git_diff", "durable", "durable_queue"],
        "optionalActions": ["durable", "durable_queue"],
        "semantics": {
            "promptResponse": "accepted",
            "settlementEvent": "agent_settled",
            "agentEndIsTerminal": false
        },
        "limits": {"maxConcurrentPrompts": 1},
        "parityComplete": false
    })
}

pub fn response(id: Option<Value>, command: &str, result: Result<Option<Value>, String>) -> Value {
    let mut value = json!({
        "type": "response",
        "command": command,
        "success": result.is_ok(),
    });
    if let Some(id) = id {
        value["id"] = id;
    }
    match result {
        Ok(Some(data)) => value["data"] = data,
        Ok(None) => {}
        Err(error) => value["error"] = json!(error),
    }
    value
}

pub fn command_id(command: &ApiCommand) -> Value {
    command
        .id
        .clone()
        .unwrap_or_else(|| json!(uuid::Uuid::new_v4().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_capability_driven() {
        let value = capabilities();
        assert_eq!(value["protocol"], "rpi");
        assert!(value["supportedCommands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "prompt"));
    }

    #[test]
    fn command_keeps_payload_fields() {
        let command: ApiCommand = serde_json::from_value(json!({
            "type": "prompt", "id": 1, "message": "hello"
        }))
        .unwrap();
        assert_eq!(command.kind, "prompt");
        assert_eq!(command.payload["message"], "hello");
    }
}
