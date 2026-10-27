//! Native AGY request identity, cumulative execution labels and generation defaults.
use super::{
    catalog,
    json::{entries, set, string},
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub fn fnv1a64_signed(input: &str) -> String {
    (input
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        }) as i64)
        .to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionContext {
    pub conversation_id: String,
    pub trajectory_id: String,
    pub numeric_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_claude: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_non_gemini_model: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_execution_id: Option<String>,
}
impl SessionContext {
    pub fn new(workspace_uri: &str, conversation_id: String, trajectory_id: String) -> Self {
        Self {
            conversation_id,
            trajectory_id,
            numeric_session_id: fnv1a64_signed(workspace_uri),
            used_claude: None,
            used_non_gemini_model: None,
            last_execution_id: None,
        }
    }
}
struct StoredSession {
    context: SessionContext,
    last_accessed_at: i64,
    last_request_timestamp: i64,
}
pub struct SessionStore {
    entries: IndexMap<String, StoredSession>,
    workspace_uri: String,
    ttl_ms: i64,
    max_entries: usize,
}
impl SessionStore {
    pub fn new(workspace_uri: String) -> Self {
        Self::with_limits(workspace_uri, 24 * 60 * 60 * 1000, 256)
    }
    pub fn with_limits(workspace_uri: String, ttl_ms: i64, max_entries: usize) -> Self {
        Self {
            entries: IndexMap::new(),
            workspace_uri,
            ttl_ms,
            max_entries,
        }
    }
    pub fn get_or_create(
        &mut self,
        key: &str,
        now: i64,
        next_id: &mut impl FnMut() -> String,
    ) -> &mut SessionContext {
        self.prune(now, key);
        let stored = self
            .entries
            .entry(key.into())
            .or_insert_with(|| StoredSession {
                context: SessionContext::new(&self.workspace_uri, next_id(), next_id()),
                last_accessed_at: now,
                last_request_timestamp: 0,
            });
        stored.last_accessed_at = now;
        &mut stored.context
    }
    pub fn begin_request(
        &mut self,
        key: &str,
        now: i64,
        next_id: &mut impl FnMut() -> String,
    ) -> (&mut SessionContext, i64) {
        self.get_or_create(key, now, next_id);
        let stored = self.entries.get_mut(key).unwrap();
        let timestamp = stored
            .last_accessed_at
            .max(stored.last_request_timestamp + 1);
        stored.last_request_timestamp = timestamp;
        (&mut stored.context, timestamp)
    }
    pub fn complete_execution(&mut self, key: &str, next_id: &mut impl FnMut() -> String) {
        if let Some(stored) = self.entries.get_mut(key) {
            stored.context.last_execution_id = Some(next_id());
        }
    }
    #[cfg(test)]
    pub fn has(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }
    #[cfg(test)]
    pub fn delete(&mut self, key: &str) {
        self.entries.shift_remove(key);
    }
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    fn prune(&mut self, now: i64, preserved: &str) {
        let expiry = now - self.ttl_ms;
        self.entries
            .retain(|key, stored| key == preserved || stored.last_accessed_at >= expiry);
        while self.entries.len() >= self.max_entries && !self.entries.contains_key(preserved) {
            let Some((key, _)) = self
                .entries
                .iter()
                .min_by_key(|(_, stored)| stored.last_accessed_at)
            else {
                break;
            };
            if key.is_empty() {
                break;
            }
            let key = key.clone();
            self.entries.shift_remove(&key);
        }
    }
}
pub fn model_enum(model: &str) -> Option<&'static str> {
    if let Some(native) = catalog::native(model) {
        return Some(&native.model);
    }
    match model.to_lowercase().as_str() {
        "gemini-3.1-pro-low" => Some("MODEL_PLACEHOLDER_M36"),
        "gemini-3.1-pro-high" => Some("MODEL_PLACEHOLDER_M37"),
        "gemini-pro-agent" => Some("MODEL_PLACEHOLDER_M16"),
        "gemini-3.6-flash-low" => Some("MODEL_PLACEHOLDER_M73"),
        "gemini-3.6-flash-medium" => Some("MODEL_PLACEHOLDER_M72"),
        "gemini-3.6-flash-high" => Some("MODEL_PLACEHOLDER_M71"),
        "gemini-3.7-flash-low" => Some("MODEL_PLACEHOLDER_M300"),
        "gemini-3.7-flash-medium" => Some("MODEL_PLACEHOLDER_M299"),
        "gemini-3.7-flash-high" => Some("MODEL_PLACEHOLDER_M298"),
        "claude-sonnet-4-6" => Some("MODEL_PLACEHOLDER_M35"),
        "claude-opus-4-6-thinking" => Some("MODEL_PLACEHOLDER_M26"),
        _ => None,
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepCountMode {
    #[default]
    Parts,
    Contents,
    /// The CLI identity: one step per content plus one per function response.
    Cli,
}
pub fn count_steps(payload: &Value, mode: StepCountMode) -> usize {
    let Some(contents) = payload["contents"].as_array() else {
        return 1;
    };
    if matches!(mode, StepCountMode::Contents) {
        return contents.len().max(1);
    }
    let mut parts = 0;
    let mut responses = 0;
    for content in contents {
        if let Some(values) = content.get("parts").and_then(Value::as_array) {
            parts += values.len();
            responses += values
                .iter()
                .filter(|part| part.get("functionResponse").is_some())
                .count();
        }
    }
    if matches!(mode, StepCountMode::Cli) {
        (contents.len() + responses).max(1)
    } else {
        parts.max(1)
    }
}
pub fn build(
    session: &mut SessionContext,
    payload: &Value,
    model: &str,
    timestamp: i64,
    mode: StepCountMode,
) -> Value {
    let previous = session
        .last_execution_id
        .as_deref()
        .filter(|id| !id.is_empty());
    let step = count_steps(payload, mode) + usize::from(previous.is_some());
    let lower = model.to_lowercase();
    let claude = session.used_claude == Some(true) || lower.starts_with("claude-");
    let non_gemini = session.used_non_gemini_model == Some(true)
        || lower.starts_with("claude-")
        || lower.starts_with("gpt-");
    session.used_claude = Some(claude);
    session.used_non_gemini_model = Some(non_gemini);
    let mut labels = Map::new();
    if let Some(previous) = previous {
        set(&mut labels, "last_execution_id", previous);
    }
    set(&mut labels, "last_step_index", step.to_string());
    if let Some(model) = model_enum(model) {
        set(&mut labels, "model_enum", model);
    }
    set(&mut labels, "trajectory_id", session.trajectory_id.clone());
    set(&mut labels, "used_claude", claude.to_string());
    set(&mut labels, "used_claude_conservative", claude.to_string());
    set(&mut labels, "used_non_gemini_model", non_gemini.to_string());
    json!({"requestId": format!("agent/{}/{timestamp}/{}/{}", session.conversation_id, session.trajectory_id, step+1),
        "sessionId": session.numeric_session_id, "labels": labels, "lastStepIndex": step})
}
fn order_payload(payload: &mut Value) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    let mut ordered = Map::new();
    for key in [
        "contents",
        "systemInstruction",
        "tools",
        "toolConfig",
        "labels",
        "generationConfig",
        "sessionId",
    ] {
        if let Some(value) = map.shift_remove(key) {
            set(&mut ordered, key, value);
        }
    }
    ordered.extend(std::mem::take(map));
    *payload = Value::Object(
        entries(&Value::Object(ordered))
            .into_iter()
            .map(|(key, value)| (key, value.clone()))
            .collect(),
    );
}
pub fn finalize(
    request: &mut Value,
    wire_model: &str,
    session: &mut SessionContext,
    timestamp: i64,
) -> String {
    if catalog::argon(&wire_model.to_lowercase()) {
        request.as_object_mut().unwrap().shift_remove("toolConfig");
    } else if request["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty())
    {
        request["toolConfig"] = json!({"functionCallingConfig":{"mode":"VALIDATED"}});
    }
    let metadata = build(session, request, wire_model, timestamp, StepCountMode::Cli);
    request["labels"] = metadata["labels"].clone();
    request["sessionId"] = metadata["sessionId"].clone();
    order_payload(request);
    metadata["requestId"].as_str().unwrap().to_owned()
}
pub fn session_key(context: &Value, options: &Value) -> String {
    if let Some(id) = options["sessionId"].as_str().filter(|id| !id.is_empty()) {
        return id.into();
    }
    context["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|message| message["role"] != "system"))
        .and_then(|message| message.get("timestamp"))
        .map(|timestamp| format!("message:{}", string(timestamp)))
        .unwrap_or_else(|| "__default__".into())
}
pub fn apply_generation(wire_model: &str, config: &mut Value) {
    let Some(native) = catalog::native(wire_model) else {
        return;
    };
    let map = config
        .as_object_mut()
        .expect("generation configuration must be an object");
    if !map.contains_key("maxOutputTokens") {
        if let Some(value) = map
            .get("max_output_tokens")
            .filter(|value| !value.is_null())
            .cloned()
            .or_else(|| native.max_output_tokens.map(Value::from))
        {
            set(map, "maxOutputTokens", value);
        }
    }
    map.shift_remove("max_output_tokens");
    if let Some(level) = native
        .thinking_level
        .filter(|_| catalog::argon(&wire_model.to_lowercase()) || native.supports_adaptive_thinking)
    {
        let mut thinking = Map::new();
        set(&mut thinking, "includeThoughts", true);
        if !catalog::argon(&wire_model.to_lowercase()) {
            set(&mut thinking, "thinkingBudget", 0);
        }
        if let Some(level) = ["LOW", "MEDIUM", "HIGH"].get(usize::from(level).wrapping_sub(1)) {
            set(&mut thinking, "thinkingLevel", *level);
        }
        set(map, "thinkingConfig", Value::Object(thinking));
    }
}
