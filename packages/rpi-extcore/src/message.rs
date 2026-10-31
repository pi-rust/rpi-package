use serde_json::{json, Value};

/// One live assistant message for snapshot-oriented consumers. This supplements
/// only the projected/replay feed; raw native frames and nested events stay intact.
/// Structural snapshots always replace local state, including nonempty initial
/// content and final corrections. This is not a second transcript/history store.
#[derive(Debug, Default)]
pub(crate) struct LiveMessageProjection {
    message: Option<Value>,
    message_id: Option<String>,
    tool_json: std::collections::HashMap<usize, String>,
}

impl LiveMessageProjection {
    pub(crate) fn project(&mut self, event: &mut Value) {
        match event["type"].as_str().unwrap_or("") {
            "message_start" | "message_update" | "message_end" => {
                let index = event["contentIndex"]
                    .as_u64()
                    .or_else(|| event["assistantMessageEvent"]["contentIndex"].as_u64())
                    .and_then(|i| usize::try_from(i).ok());
                let kind = event["eventType"]
                    .as_str()
                    .or_else(|| event["assistantMessageEvent"]["type"].as_str())
                    .unwrap_or("");
                if event["type"] == "message_start" {
                    *self = Self::default();
                }
                if let Some(raw_id) = event.get("messageId") {
                    let Ok(id) = validated_message_id(raw_id) else {
                        return;
                    };
                    if self
                        .message_id
                        .as_deref()
                        .is_some_and(|current| current != id)
                        || (self.message.is_some() && self.message_id.is_none())
                    {
                        // A delayed frame must not overwrite a newer message's
                        // snapshot or append its compact delta to that message.
                        return;
                    }
                    self.message_id = Some(id.to_owned());
                }
                if let Some(index) = index {
                    if kind == "toolcall_start" || kind == "toolcall_end" {
                        self.tool_json.remove(&index);
                    }
                    if kind == "toolcall_delta" {
                        if let Some(delta) = event["delta"].as_str() {
                            // Only track slots established by native snapshots.
                            let known = self
                                .message
                                .as_ref()
                                .and_then(|message| message["content"].as_array())
                                .is_some_and(|blocks| {
                                    blocks
                                        .get(index)
                                        .is_some_and(|block| block["type"] == "toolCall")
                                });
                            if known {
                                self.tool_json.entry(index).or_default().push_str(delta);
                            }
                        }
                    }
                }
                if event["message"]["role"] == "assistant" {
                    self.message = Some(event["message"].clone());
                    if event["type"] == "message_end" {
                        *self = Self::default();
                    }
                    return;
                }
                if event["type"] != "message_update" || !event["message"].is_null() {
                    return;
                }
                let (block_type, field) = match kind {
                    "text_delta" => ("text", "text"),
                    "thinking_delta" => ("thinking", "thinking"),
                    "toolcall_delta" => ("toolCall", "arguments"),
                    _ => return,
                };
                let Some(index) = index else { return };
                let Some(delta) = event["delta"].as_str() else {
                    return;
                };
                let Some(message) = self.message.as_mut() else {
                    return;
                };
                // Do not allocate from an untrusted index or guess block layout;
                // native start/structural events provide the authoritative shape.
                let Some(block) = message["content"]
                    .as_array_mut()
                    .and_then(|blocks| blocks.get_mut(index))
                else {
                    return;
                };
                if block["type"] != block_type {
                    return;
                }
                if block_type == "toolCall" {
                    for (source, target) in [("id", "id"), ("toolName", "name")] {
                        if let Some(value) = event["assistantMessageEvent"][source].as_str() {
                            block[target] = json!(value);
                        }
                    }
                    // Incomplete JSON is not a final argument value. Keep the
                    // latest native snapshot until the raw stream parses, and
                    // always reconcile with toolcall_end/message_end afterward.
                    if let Some(arguments) = self
                        .tool_json
                        .get(&index)
                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                    {
                        block[field] = arguments;
                    }
                } else {
                    let Some(text) = block[field].as_str() else {
                        return;
                    };
                    let mut text = text.to_owned();
                    text.push_str(delta);
                    block[field] = json!(text);
                }
                event["message"] = message.clone();
            }
            "agent_start" | "retry_scheduled" | "agent_settled" | "host_closed" => {
                *self = Self::default()
            }
            _ => {}
        }
    }
}

/// Reconcile incremental text with authoritative native snapshots. Reconcile with cumulative backend snapshots so a
/// dropped delta or an early RPC completion cannot silently truncate a reply.
#[derive(Default)]
pub struct MessageProjection {
    pub text: String,
    pub thinking: String,
    pub stop_reason: Option<String>,
    pub error: Option<String>,
    pub repaired: usize,
    /// Validated native session entry ID for the current assistant message.
    /// Public to retain construction with `..Default::default()` outside core.
    pub message_id: Option<String>,
    /// Configure before observing a stream; legacy hosts leave this false.
    pub message_ids_required: bool,
}

impl MessageProjection {
    pub fn require_message_ids(&mut self, required: bool) {
        self.message_ids_required = required;
    }

    fn identify(&mut self, event: &Value, start: bool) -> Result<(), String> {
        let incoming = event
            .get("messageId")
            .map(validated_message_id)
            .transpose()?;
        if self.message_ids_required && incoming.is_none() {
            return Err("rpi assistant message is missing its native messageId".into());
        }
        if !start {
            if let Some(id) = incoming {
                if self
                    .message_id
                    .as_deref()
                    .is_some_and(|current| current != id)
                    || (self.message_id.is_none()
                        && (!self.text.is_empty() || !self.thinking.is_empty()))
                {
                    return Err("rpi assistant messageId changed within a message".into());
                }
            }
        }
        if start {
            *self = Self {
                repaired: self.repaired,
                message_ids_required: self.message_ids_required,
                ..Self::default()
            };
        }
        if let Some(id) = incoming {
            self.message_id = Some(id.to_owned());
        }
        Ok(())
    }

    fn with_identity(&self, mut update: Value) -> Value {
        if let Some(id) = &self.message_id {
            update["messageId"] = json!(id);
        }
        update
    }

    pub fn event(&mut self, event: &Value) -> Result<Vec<Value>, String> {
        let kind = event["type"].as_str().unwrap_or("");
        let message = &event["message"];
        if matches!(kind, "message_start" | "message_update" | "message_end")
            && (message["role"] == "assistant" || kind == "message_update")
        {
            // Validate before mutating text/thinking or producing any output.
            self.identify(event, kind == "message_start")?;
        }
        if matches!(kind, "message_start" | "message_update" | "message_end")
            && message["role"] == "assistant"
        {
            if kind == "message_end" {
                self.stop_reason = message["stopReason"].as_str().map(str::to_owned);
                self.error = message["errorMessage"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
            if let Some(blocks) = message["content"].as_array() {
                let mut updates = Vec::new();
                for (block_type, field, variant, sent) in [
                    ("thinking", "thinking", "thinking", &mut self.thinking),
                    ("text", "text", "text", &mut self.text),
                ] {
                    let parts: Vec<&str> = blocks
                        .iter()
                        .filter(|b| b["type"] == block_type)
                        .filter_map(|b| b[field].as_str())
                        .collect();
                    if !parts.is_empty() {
                        if let Some(update) = append_snapshot(sent, &parts.concat(), variant)? {
                            if kind == "message_end" {
                                self.repaired += update["text"].as_str().unwrap_or("").len();
                            }
                            updates.push(update);
                        }
                    }
                }
                // Older RPC senders may omit snapshots. Fall back only when the
                // delta's content kind is absent, never append it twice.
                let delta_type = match event["eventType"].as_str() {
                    Some("text_delta") => "text",
                    Some("thinking_delta") => "thinking",
                    _ => "",
                };
                if kind != "message_end" && !blocks.iter().any(|b| b["type"] == delta_type) {
                    if let Some(update) = self.delta(event) {
                        updates.push(update);
                    }
                }
                return Ok(updates
                    .into_iter()
                    .map(|update| self.with_identity(update))
                    .collect());
            }
        }
        Ok(self.delta(event).into_iter().collect())
    }

    fn delta(&mut self, event: &Value) -> Option<Value> {
        let update = if event["type"] == "message_update" {
            let kind = match event["eventType"].as_str() {
                Some("text_delta") => "text",
                Some("thinking_delta") => "thinking",
                _ => return None,
            };
            let text = event["delta"].as_str().filter(|text| !text.is_empty())?;
            Some(json!({"kind":kind,"text":text}))
        } else {
            None
        };
        if let Some(update) = &update {
            let sent = match update["kind"].as_str() {
                Some("text") => Some(&mut self.text),
                Some("thinking") => Some(&mut self.thinking),
                _ => None,
            };
            if let Some(sent) = sent {
                sent.push_str(update["text"].as_str().unwrap_or(""));
            }
        }
        update.map(|update| self.with_identity(update))
    }

    pub fn complete(&mut self, final_text: &str) -> Result<Option<Value>, String> {
        // Aborted/failed runs before their first token legitimately have no
        // assistant state and an empty finalText. Never invent an ID for them.
        if self.message_ids_required && !final_text.is_empty() && self.message_id.is_none() {
            return Err("rpi finalText has no validated assistant messageId".into());
        }
        let update = append_snapshot(&mut self.text, final_text, "text")?;
        if let Some(update) = &update {
            self.repaired += update["text"].as_str().unwrap_or("").len();
        }
        Ok(update.map(|update| self.with_identity(update)))
    }
}

fn validated_message_id(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        .ok_or_else(|| {
            "rpi assistant messageId must be nonempty, at most 256 bytes and contain no controls"
                .into()
        })
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    include!("message_identity_tests.rs");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<Value> {
        serde_json::from_str(include_str!("../tests/fixtures/compact-deltas.json")).unwrap()
    }

    #[test]
    fn compact_and_snapshot_streams_preserve_initial_content_without_double_append() {
        let mut live = LiveMessageProjection::default();
        let mut compact = MessageProjection::default();
        let mut snapshot = MessageProjection::default();
        for raw in fixture() {
            let mut projected = raw.clone();
            live.project(&mut projected);
            // This is also an offline snapshot-mode fixture: each text/thinking
            // delta carries its cumulative message, as older native hosts do.
            assert_eq!(
                compact.event(&raw).unwrap(),
                snapshot.event(&projected).unwrap()
            );
            if let Some(nested) = raw.get("assistantMessageEvent") {
                assert_eq!(&projected["assistantMessageEvent"], nested);
                assert!(projected["assistantMessageEvent"].get("partial").is_none());
            }
            for field in ["type", "eventType", "contentIndex", "delta"] {
                assert_eq!(raw.get(field), projected.get(field));
            }
            if raw["eventType"] == "thinking_delta" {
                assert_eq!(projected["message"]["content"][0]["thinking"], "初想🌊");
                assert_eq!(projected["message"]["content"][1]["text"], "Hello");
            }
            if raw["eventType"] == "text_delta" {
                assert!(projected["message"]["content"][1]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("Hello world"));
            }
            if raw["type"] == "message_end" {
                assert_eq!(raw, projected);
            }
            if raw["eventType"] == "toolcall_delta" {
                assert_eq!(
                    projected["message"]["content"][2]["arguments"],
                    json!({"path":"file"})
                );
            }
        }
        assert_eq!(compact.text, "Hello world! Saved.");
        assert_eq!(compact.thinking, "初想🌊好了");
        assert_eq!(compact.stop_reason.as_deref(), Some("stop"));
        assert!(compact.complete("Hello world! Saved.").unwrap().is_none());
        assert!(compact.complete("different final text").is_err());
    }

    #[test]
    fn live_snapshots_replace_state_and_final_message_is_authoritative() {
        let mut live = LiveMessageProjection::default();
        for mut event in fixture().into_iter().take(3) {
            live.project(&mut event);
        }
        // A structural snapshot is authoritative, not another chunk to append.
        let mut correction = json!({"type":"message_update","eventType":"text_end","message":{"role":"assistant","content":[{"type":"text","text":"replacement"}]}});
        live.project(&mut correction);
        let mut delta =
            json!({"type":"message_update","eventType":"text_delta","contentIndex":0,"delta":"!"});
        live.project(&mut delta);
        assert_eq!(delta["message"]["content"][0]["text"], "replacement!");
        let mut end = json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"final correction"}]}});
        let expected = end.clone();
        live.project(&mut end);
        assert_eq!(end, expected);
        assert!(live.message.is_none());
        let mut orphan = json!({"type":"message_update","eventType":"text_delta","contentIndex":0,"delta":"late"});
        live.project(&mut orphan);
        assert!(orphan.get("message").is_none());
    }

    #[test]
    fn compact_tool_identity_and_complete_arguments_update_without_guessing_partial_json() {
        let mut live = LiveMessageProjection::default();
        let mut start = json!({"type":"message_start","message":{"role":"assistant","content":[{"type":"toolCall","id":"","name":"","arguments":{"path":"pre"}}]}});
        live.project(&mut start);
        let mut first = json!({"type":"message_update","eventType":"toolcall_delta","contentIndex":0,"delta":"{\"path\":\"prefix","assistantMessageEvent":{"id":"call","toolName":"read"}});
        live.project(&mut first);
        assert_eq!(first["message"]["content"][0]["id"], "call");
        assert_eq!(first["message"]["content"][0]["name"], "read");
        assert_eq!(
            first["message"]["content"][0]["arguments"],
            json!({"path":"pre"})
        );
        let mut last = json!({"type":"message_update","eventType":"toolcall_delta","contentIndex":0,"delta":"\"}"});
        live.project(&mut last);
        assert_eq!(
            last["message"]["content"][0]["arguments"],
            json!({"path":"prefix"})
        );
        let mut end = json!({"type":"message_end","message":{"role":"assistant","stopReason":"aborted","content":[{"type":"toolCall","id":"final","name":"read","arguments":{"path":"corrected"}}]}});
        let expected = end.clone();
        live.project(&mut end);
        assert_eq!(end, expected);
        assert!(live.tool_json.is_empty());
        assert!(live.message.is_none());
        live.project(&mut start);
        let mut reused = json!({"type":"message_update","eventType":"toolcall_delta","contentIndex":0,"delta":"{\"other\":1}"});
        live.project(&mut reused);
        assert_eq!(
            reused["message"]["content"][0]["arguments"],
            json!({"other":1})
        );
    }

    #[test]
    fn live_projection_never_guesses_missing_or_mismatched_content_indexes() {
        let mut live = LiveMessageProjection::default();
        live.project(&mut fixture().remove(0));
        for index in [
            json!(null),
            json!(-1),
            json!(u64::MAX),
            json!("1"),
            json!(0),
        ] {
            let mut event = json!({"type":"message_update","eventType":"text_delta","contentIndex":index,"delta":"bad"});
            let expected = event.clone();
            live.project(&mut event);
            assert_eq!(event, expected);
        }
    }
}

fn append_snapshot(
    sent: &mut String,
    complete: &str,
    variant: &str,
) -> Result<Option<Value>, String> {
    if let Some(suffix) = complete.strip_prefix(sent.as_str()) {
        if suffix.is_empty() {
            return Ok(None);
        }
        let update = json!({"kind":variant,"text":suffix});
        *sent = complete.to_owned();
        return Ok(Some(update));
    }
    // A delayed partial snapshot must not replace or duplicate newer text.
    if sent.starts_with(complete) {
        return Ok(None);
    }
    Err("rpi final message differs from the streamed text; reload this session to recover the saved reply".into())
}
