//! Live, read-only ACP child sessions. The session owner must start this watcher
//! before admitting native work and stop it before releasing its host.
use std::{collections::{HashMap, HashSet}, sync::Arc, time::Duration};

use rpi_extcore::{host::HostSession, message::MessageProjection};
use serde_json::{json, Value};
use tokio::{sync::broadcast, task::JoinHandle};

use crate::{protocol::Writer, session::translate_event};

const MAX_CHILDREN: usize = 32;
const MAX_ID: usize = 512;
const MAX_PARENT_TOOL_CALL_ID: usize = 1024;
const MAX_BYTES: usize = 256 * 1024;
const MAX_DIRECTED_MESSAGES: usize = 1024;
const OUTPUT_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn negotiated(params: &Value) -> bool {
    params["protocolVersion"].as_u64() == Some(2)
        || params
            .pointer("/clientCapabilities/subagents")
            .is_some_and(Value::is_object)
}

/// Owns the raw subscription and its only consumer. Never call Session::kill
/// from this task: that owner may itself be waiting for this task to finish.
pub(crate) struct Watcher {
    task: Option<JoinHandle<()>>,
    failure: Arc<std::sync::Mutex<Option<String>>>,
}

impl Watcher {
    pub(crate) fn start(rpc: Arc<HostSession>, writer: Arc<Writer>, root: String) -> Self {
        // Subscribe synchronously, not inside spawn: child_start cannot race us.
        let mut raw = rpc.subscribe_raw();
        let failure = Arc::new(std::sync::Mutex::new(None));
        let failed = failure.clone();
        let task = tokio::spawn(async move {
            let mut roster = Roster::new(root, writer.supports_v2());
            let result: Result<(), String> = async {
                loop {
                    let event = match raw.recv().await {
                        Ok(event) => event,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            return Err(format!("raw child subscription lagged by {n} events"));
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err("raw child subscription closed without host_closed".into());
                        }
                    };
                    let kind = event["type"].as_str().unwrap_or("");
                    if !matches!(
                        kind,
                        "child_start" | "child_event" | "child_end" | "host_closed"
                    ) {
                        continue;
                    }
                    let closed = kind == "host_closed";
                    for params in roster.event(&event)? {
                        let announced = (params["update"]["sessionUpdate"] == "subagent_update")
                            .then(|| params["update"]["sessionId"].as_str().map(str::to_owned))
                            .flatten();
                        tokio::time::timeout(
                            OUTPUT_TIMEOUT,
                            writer.notify("session/update", params),
                        )
                        .await
                        .map_err(|_| "child output stalled for two seconds".to_owned())?;
                        if writer.is_closed() {
                            return Err("ACP output closed while forwarding child events".into());
                        }
                        // Publication is a wire boundary: interactions may only
                        // discover this ID after its announcement was written.
                        if let Some(id) = announced {
                            writer.register_child(id);
                        }
                    }
                    if closed {
                        return Ok(());
                    }
                }
            }
            .await;
            if let Err(error) = result {
                // Publish the failure before termination wakes the parent run.
                // Backend shutdown is not a user-requested cancellation.
                *failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.clone());
                crate::diagln!(
                    "[rpi-acp] subagent forwarding failed: {error}; terminating native host"
                );
                rpc.terminate().await;
            }
        });
        Self { task: Some(task), failure }
    }

    pub(crate) fn failure(&self) -> Option<String> {
        self.failure.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) async fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[derive(Default)]
struct Child {
    parent: String,
    // Canonical first-start provenance from the native host, never tool JSON,
    // client input, or a child tool ID. None is immutable too.
    parent_tool_call_id: Option<String>,
    ended: bool,
    projection: MessageProjection,
    message_id: Option<String>,
    pending_text: Vec<Value>,
    // Retain only committed identities, not content. Fail rather than evict and
    // risk replaying an incoming entry as a new message.
    incoming_ids: HashSet<String>,
}

struct Roster {
    root: String,
    v2: bool,
    // Retain terminal IDs too: never reuse an association within a connection.
    children: HashMap<String, Child>,
}

fn id<'a>(event: &'a Value, key: &str) -> Result<&'a str, String> {
    event[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= MAX_ID)
        .ok_or_else(|| format!("invalid or overlong child {key}"))
}

fn envelope(session: &str, update: Value) -> Value {
    json!({"sessionId":session,"update":update})
}

fn state_updates(parent: &str, child: &str, state: Value, v2: bool) -> Vec<Value> {
    let mut updates = vec![envelope(
        parent,
        json!({
            "sessionUpdate":"subagent_update", "sessionId":child, "state":state,
        }),
    )];
    if v2 {
        let mut update = state;
        update["sessionUpdate"] = json!("state_update");
        updates.push(envelope(child, update));
    }
    updates
}

impl Roster {
    fn new(root: String, v2: bool) -> Self {
        Self {
            root,
            v2,
            children: HashMap::new(),
        }
    }

    fn event(&mut self, event: &Value) -> Result<Vec<Value>, String> {
        let mut updates = self.event_inner(event)?;
        // Native child IDs need only be unique inside their owning host. ACP
        // IDs must be unique across every root on this connection. The length
        // prefix makes this derivation unambiguous without rewriting native IDs.
        let root = &self.root;
        let scoped = |id: &str| format!("child:{}:{}:{}", root.len(), root, id);
        for params in &mut updates {
            if let Some(id) = params["sessionId"].as_str().filter(|id| *id != root) {
                params["sessionId"] = json!(scoped(id));
            }
            if params["update"]["sessionUpdate"] == "subagent_update" {
                let id = params["update"]["sessionId"].as_str().unwrap();
                if let Some(parent_tool_call_id) = self.children[id].parent_tool_call_id.as_deref() {
                    params["update"]["_meta"] = json!({"rpi/parentToolCallId":parent_tool_call_id});
                }
                let id = params["update"]["sessionId"].as_str().unwrap();
                params["update"]["sessionId"] = json!(scoped(id));
            }
            for field in ["senderSessionId", "recipientSessionId"] {
                if let Some(id) = params["update"][field].as_str().filter(|id| *id != root) {
                    params["update"][field] = json!(scoped(id));
                }
            }
        }
        Ok(updates)
    }

    fn event_inner(&mut self, event: &Value) -> Result<Vec<Value>, String> {
        let kind = event["type"].as_str().unwrap_or("");
        if kind == "host_closed" {
            let mut updates = Vec::new();
            for (child, entry) in &mut self.children {
                if !entry.ended {
                    updates.extend(state_updates(
                        &entry.parent,
                        child,
                        json!({"state":"unknown"}),
                        self.v2,
                    ));
                    entry.ended = true;
                }
            }
            return Ok(updates);
        }
        if !matches!(kind, "child_start" | "child_event" | "child_end") {
            return Ok(Vec::new());
        }
        if serde_json::to_vec(event).map_err(|e| e.to_string())?.len() > MAX_BYTES {
            return Err("child event exceeds 256 KiB".into());
        }
        let child = id(event, "sessionId")?;
        let parent = id(event, "parentSessionId")?;
        if let Some(alias) = event.get("childId") {
            if alias.as_str() != Some(child) {
                return Err("childId/sessionId mismatch".into());
            }
        }
        if kind == "child_start" {
            if child == self.root || child == parent || self.children.contains_key(child) {
                return Err("duplicate or cyclic child association".into());
            }
            if self.children.len() >= MAX_CHILDREN {
                return Err("child roster exceeds 32 IDs".into());
            }
            if parent != self.root && !self.children.get(parent).is_some_and(|c| !c.ended) {
                return Err("unknown or inactive child parent".into());
            }
            let parent_tool_call_id = match event.get("parentToolCallId") {
                None => None,
                Some(value) => Some(value.as_str().filter(|id| {
                    !id.is_empty()
                        && id.len() <= MAX_PARENT_TOOL_CALL_ID
                        && !id.bytes().any(|byte| byte.is_ascii_control())
                }).ok_or("invalid child parentToolCallId")?.to_owned()),
            };
            let mut update = json!({
                "sessionUpdate":"subagent_update", "sessionId":child,
                "capabilities":{}, "state":{"state":"running"},
            });
            for field in ["title", "description"] {
                if let Some(value) = event.get(field) {
                    if !value.is_string() && !value.is_null() {
                        return Err(format!("invalid child {field}"));
                    }
                    update[field] = value.clone();
                }
            }
            // Validate the entire announcement before committing any roster state.
            // All repeated starts are rejected above, including None -> Some.
            self.children.insert(
                child.to_owned(),
                Child {
                    parent: parent.to_owned(),
                    parent_tool_call_id,
                    ..Child::default()
                },
            );
            let mut updates = vec![envelope(parent, update)];
            if self.v2 {
                updates.push(envelope(
                    child,
                    json!({"sessionUpdate":"state_update","state":"running"}),
                ));
            }
            return Ok(updates);
        }
        let entry = self
            .children
            .get_mut(child)
            .ok_or("event for unknown child")?;
        if entry.parent != parent {
            return Err("child parent mismatch".into());
        }
        if entry.ended {
            return Err("event after child_end".into());
        }
        if kind == "child_end" {
            if !entry.pending_text.is_empty() {
                return Err("child ended with uncommitted text and no canonical ID".into());
            }
            let mut updates = Vec::new();
            if let Some(text) = event.pointer("/result/text").and_then(Value::as_str) {
                if text.len() > MAX_BYTES {
                    return Err("child final text exceeds 256 KiB".into());
                }
                if let Some(delta) = entry.projection.complete(text)? {
                    updates.push(envelope(
                        child,
                        text_update(delta, entry.message_id.as_deref(), self.v2)?,
                    ));
                }
            }
            let state = terminal_state(event, &entry.projection);
            updates.extend(state_updates(parent, child, state, self.v2));
            entry.ended = true;
            entry.projection = MessageProjection::default();
            entry.message_id = None;
            return Ok(updates);
        }
        let native = event
            .get("event")
            .filter(|e| e.is_object())
            .ok_or("missing nested child event")?;
        if let Some(session) = native.get("sessionId") {
            if session.as_str() != Some(child) {
                return Err("nested child event sessionId mismatch".into());
            }
        }
        let message_kind = native["type"].as_str().ok_or("missing child event type")?;
        validate_native_event(native, message_kind)?;
        if native["message"]["role"] == "user"
            && matches!(message_kind, "message_start" | "message_end")
        {
            // Only the committed snapshot is authoritative. Start/end are two
            // observations of ONE incoming entry, not appendable chunks. Keep
            // its identity entirely separate from assistant text/thought state.
            if message_kind == "message_start" {
                return Ok(Vec::new());
            }
            let message_id = native["messageId"]
                .as_str()
                .or_else(|| native["message"]["messageId"].as_str())
                .or_else(|| native["message"]["id"].as_str())
                .filter(|id| !id.trim().is_empty() && id.len() <= MAX_ID)
                .ok_or("committed child input has no valid canonical message ID")?;
            let content = match &native["message"]["content"] {
                Value::String(text) => json!([{"type":"text","text":text}]),
                Value::Array(blocks) if blocks.iter().all(|block| {
                    block.is_object() && block["type"] == "text" && block["text"].is_string()
                }) => Value::Array(blocks.clone()),
                _ => return Err("unsupported child input content; expected text blocks".into()),
            };
            if entry.incoming_ids.contains(message_id) {
                return Ok(Vec::new());
            }
            if entry.incoming_ids.len() >= MAX_DIRECTED_MESSAGES {
                return Err("child incoming message identity limit exceeded".into());
            }
            entry.incoming_ids.insert(message_id.to_owned());
            // We observe receipt in the child, not an outgoing parent transcript
            // entry. Do not invent senderMessageId or a second view. Both ACP
            // versions use the RFD's whole-content replacement upsert here.
            return Ok(vec![envelope(child, json!({
                "sessionUpdate":"session_message", "messageId":message_id,
                "senderSessionId":parent, "recipientSessionId":child,
                "content":content,
            }))]);
        }
        if message_kind == "message_start" {
            if !entry.pending_text.is_empty() {
                return Err("previous child message has no canonical ID".into());
            }
            entry.message_id = None;
        }
        if matches!(
            message_kind,
            "message_start" | "message_update" | "message_end"
        ) {
            if let Some(value) = native["message"]["messageId"]
                .as_str()
                .or_else(|| native["message"]["id"].as_str())
                .or_else(|| native["messageId"].as_str())
            {
                if value.trim().is_empty() || value.len() > MAX_ID {
                    return Err("invalid canonical child message ID".into());
                }
                if entry.message_id.as_deref().is_some_and(|id| id != value) {
                    return Err("canonical child message ID changed within message".into());
                }
                entry.message_id = Some(value.to_owned());
            }
        }
        let mut updates = Vec::new();
        let deltas = entry.projection.event(native)?;
        if self.v2 && entry.message_id.is_none() {
            // Native assigns canonical IDs at commit. Buffer bounded projected
            // deltas until then rather than inventing a streaming identity.
            entry.pending_text.extend(deltas);
        } else {
            for delta in entry.pending_text.drain(..).chain(deltas) {
                updates.push(envelope(
                    child,
                    text_update(delta, entry.message_id.as_deref(), self.v2)?,
                ));
            }
        }
        if message_kind == "message_end" && !entry.pending_text.is_empty() {
            return Err("committed child message has no canonical ID".into());
        }
        if entry
            .projection
            .text
            .len()
            .saturating_add(entry.projection.thinking.len())
            > MAX_BYTES
        {
            return Err("child message projection exceeds 256 KiB".into());
        }
        if !matches!(
            message_kind,
            "message_start" | "message_update" | "message_end"
        ) {
            if let Some(update) = translate_event(native) {
                let update = if self.v2 {
                    crate::v2::map_session_update(update)
                } else {
                    update
                };
                updates.push(envelope(child, update));
            }
        }
        Ok(updates)
    }
}

// This is deliberately an allowlist. A projection returning no text is not
// evidence that an unknown event (especially a request) was safely handled.
fn validate_native_event(event: &Value, kind: &str) -> Result<(), String> {
    match kind {
        "message_start" | "message_end" => {
            let message = &event["message"];
            // Native user messages use string content; assistant and tool
            // snapshots use block arrays. Do not reject the real input entry
            // just because the synthetic assistant fixture used arrays only.
            let content_valid = message["content"].is_array()
                || (message["role"] == "user" && message["content"].is_string());
            if !message.is_object()
                || !message["role"].is_string()
                || !content_valid
            {
                return Err("malformed child message snapshot".into());
            }
        }
        "message_update" => {
            let event_type = event["eventType"].as_str().ok_or("missing child message eventType")?;
            match event_type {
                "text_delta" | "thinking_delta" if event["delta"].is_string() => {}
                "text_start" | "text_end" | "thinking_start" | "thinking_end"
                | "toolcall_start" | "toolcall_delta" | "toolcall_end" | "start" | "done" | "error" => {}
                _ => return Err(format!("unsupported or malformed child message eventType: {event_type}")),
            }
            if let Some(message) = event.get("message") {
                if !message.is_object() || !message["content"].is_array() {
                    return Err("malformed child message snapshot".into());
                }
            }
        }
        "tool_execution_start" | "tool_execution_update" | "tool_execution_end" => {
            id(event, "toolCallId")?;
        }
        // Native lifecycle markers have no standalone ACP transcript projection.
        "agent_start" | "agent_end" | "turn_start" | "turn_end" | "agent_settled" => {}
        _ => return Err(format!("unsupported child event type: {kind}")),
    }
    Ok(())
}

fn text_update(delta: Value, message_id: Option<&str>, v2: bool) -> Result<Value, String> {
    let mut update = json!({
        "sessionUpdate": if delta["kind"] == "thinking" { "agent_thought_chunk" } else { "agent_message_chunk" },
        "content":{"type":"text","text":delta["text"]},
    });
    if let Some(id) = message_id {
        // Both text and thoughts use the supplied identity, never a synthetic suffix.
        update["messageId"] = json!(id);
    } else if v2 {
        return Err("v2 child text has no canonical committed message ID".into());
    }
    Ok(update)
}

fn terminal_state(event: &Value, projection: &MessageProjection) -> Value {
    let result = &event["result"];
    let error = event["error"]
        .as_str()
        .or_else(|| result["error"].as_str())
        .or(projection.error.as_deref());
    // Native cancellation currently arrives as one of these explicit errors;
    // other failures (including timeout/overflow) must not masquerade as cancel.
    let cancelled = result["status"] == "aborted"
        || error.is_some_and(|e| {
            matches!(
                e,
                "parent tool invocation cancelled" | "parent run ended" | "parent run cancelled"
            )
        });
    if cancelled {
        return json!({"state":"idle","stopReason":"cancelled"});
    }
    if event["status"] == "failed" || result["status"] == "failed" || error.is_some() {
        return json!({"state":"idle","stopReason":"error","error":{
            "code":-32000,"message":error.unwrap_or("native child failed"),
        }});
    }
    let reason = if result["turnLimited"] == true {
        Some("max_turn_requests")
    } else if projection.stop_reason.as_deref() == Some("length") {
        Some("max_tokens")
    } else if result["status"] == "completed" {
        Some("end_turn")
    } else {
        None
    };
    let mut state = json!({"state":"idle"});
    if let Some(reason) = reason {
        state["stopReason"] = json!(reason);
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    fn start(child: &str) -> Value {
        json!({"type":"child_start","parentSessionId":"root","sessionId":child,"childId":child})
    }
    fn nested(child: &str, event: Value) -> Value {
        json!({"type":"child_event","parentSessionId":"root","sessionId":child,"event":event})
    }
    fn correlated_start(child: &str, tool: &str) -> Value {
        let mut event = start(child);
        event["parentToolCallId"] = json!(tool);
        event
    }

    #[test]
    fn old_host_announcement_has_exact_legacy_bytes_and_no_inferred_parent() {
        for v2 in [false, true] {
            let mut r = Roster::new("root".into(), v2);
            let mut event = start("c");
            event["arguments"] = json!({"parentToolCallId":"untrusted"});
            event["_meta"] = json!({"rpi/parentToolCallId":"untrusted"});
            let updates = r.event(&event).unwrap();
            let mut expected = vec![json!({"sessionId":"root","update":{
                "sessionUpdate":"subagent_update","sessionId":"child:4:root:c",
                "capabilities":{},"state":{"state":"running"},
            }})];
            if v2 {
                expected.push(json!({"sessionId":"child:4:root:c","update":{
                    "sessionUpdate":"state_update","state":"running",
                }}));
            }
            assert_eq!(serde_json::to_vec(&updates).unwrap(), serde_json::to_vec(&expected).unwrap());
            assert!(r.children["c"].parent_tool_call_id.is_none());
        }
    }

    #[test]
    fn private_parent_provenance_is_scoped_only_on_associations_in_both_versions() {
        for v2 in [false, true] {
            let mut r = Roster::new("root".into(), v2);
            for (child, tool) in [("a", "root-tool-a"), ("b", "root-tool-b"), ("c", "root-tool-a")] {
                let updates = r.event(&correlated_start(child, tool)).unwrap();
                let association = &updates[0]["update"];
                assert_eq!(association["sessionId"], format!("child:4:root:{child}"));
                assert_eq!(association["_meta"], json!({"rpi/parentToolCallId":tool}));
                assert!(association.get("parentToolCallId").is_none());
                assert_eq!(association["capabilities"], json!({}));
                if v2 {
                    assert!(updates[1]["update"].get("_meta").is_none());
                    assert_eq!(crate::v2::update_envelope("root", association.clone()).unwrap(), updates[0]);
                }
            }
            let mut nested_start = correlated_start("nested", "actual-parent-child-tool");
            nested_start["parentSessionId"] = json!("a");
            let updates = r.event(&nested_start).unwrap();
            assert_eq!(updates[0]["sessionId"], "child:4:root:a");
            assert_eq!(updates[0]["update"]["_meta"]["rpi/parentToolCallId"], "actual-parent-child-tool");
            // A later event cannot override canonical start provenance.
            let mut end = json!({"type":"child_end","parentSessionId":"root","sessionId":"a",
                "parentToolCallId":"forged","result":{"status":"completed"}});
            let updates = r.event(&end).unwrap();
            assert_eq!(updates[0]["update"]["_meta"]["rpi/parentToolCallId"], "root-tool-a");
            end["sessionId"] = json!("b");
            end.as_object_mut().unwrap().remove("parentToolCallId");
            assert_eq!(r.event(&end).unwrap()[0]["update"]["_meta"]["rpi/parentToolCallId"], "root-tool-b");
            for params in r.event(&json!({"type":"host_closed"})).unwrap() {
                if params["update"]["sessionUpdate"] == "subagent_update" {
                    assert!(params["update"]["_meta"]["rpi/parentToolCallId"].is_string());
                }
            }
        }
    }

    #[test]
    fn invalid_parent_provenance_and_announcements_are_atomic() {
        let mut invalid = vec![Value::Null, json!(0), json!(true), json!([]), json!({}),
            json!(""), json!("x".repeat(1025)), json!("é".repeat(513))];
        for control in 0..=127_u8 {
            if control.is_ascii_control() {
                invalid.push(json!(format!("root{}tool", char::from(control))));
            }
        }
        for v2 in [false, true] {
            for value in &invalid {
                let mut r = Roster::new("root".into(), v2);
                let mut event = start("c");
                event["parentToolCallId"] = value.clone();
                assert!(r.event(&event).unwrap_err().contains("parentToolCallId"), "{value}");
                assert!(r.children.is_empty());
                r.event(&start("c")).unwrap();
            }
            for field in ["title", "description"] {
                let mut r = Roster::new("root".into(), v2);
                let mut event = correlated_start("c", "trusted");
                event[field] = json!(42);
                assert!(r.event(&event).is_err());
                assert!(r.children.is_empty());
                r.event(&start("c")).unwrap();
            }
            for tool in ["x".repeat(1024), "é".repeat(512)] {
                let mut r = Roster::new("root".into(), v2);
                assert_eq!(r.event(&correlated_start("c", &tool)).unwrap()[0]["update"]["_meta"]["rpi/parentToolCallId"], tool);
            }
        }
    }

    #[test]
    fn first_start_optional_provenance_is_immutable_even_across_duplicates() {
        for first in [None, Some("canonical")] {
            let mut r = Roster::new("root".into(), true);
            let initial = first.map_or_else(|| start("c"), |tool| correlated_start("c", tool));
            r.event(&initial).unwrap();
            for duplicate in [start("c"), correlated_start("c", "canonical"), correlated_start("c", "override")] {
                assert!(r.event(&duplicate).is_err());
                assert_eq!(r.children.len(), 1);
                assert_eq!(r.children["c"].parent, "root");
                assert_eq!(r.children["c"].parent_tool_call_id.as_deref(), first);
            }
            let updates = r.event(&json!({"type":"host_closed"})).unwrap();
            assert_eq!(updates[0]["update"]["_meta"]["rpi/parentToolCallId"].as_str(), first);
            assert_eq!(updates[0]["update"].get("_meta").is_some(), first.is_some());
        }
    }

    #[test]
    fn bounded_native_record_replay_projects_identical_private_metadata() {
        // Replay the same bounded native records through a fresh roster. This
        // proves projection determinism, not durable child load/history support.
        for v2 in [false, true] {
            let records = vec![correlated_start("c", "root-tool"),
                nested("c", json!({"type":"tool_execution_start","toolCallId":"child-tool","toolName":"read","args":{}})),
                json!({"type":"child_end","parentSessionId":"root","sessionId":"c","result":{"status":"completed"}})];
            let encoded = serde_json::to_vec(&records).unwrap();
            assert!(encoded.len() < MAX_BYTES);
            let replay: Vec<Value> = serde_json::from_slice(&encoded).unwrap();
            let project = |records: &[Value]| {
                let mut r = Roster::new("root".into(), v2);
                records.iter().flat_map(|record| r.event(record).unwrap()).collect::<Vec<_>>()
            };
            let live = project(&records);
            assert_eq!(live, project(&replay));
            for params in live {
                let update = &params["update"];
                if update["sessionUpdate"] == "subagent_update" {
                    assert_eq!(update["_meta"]["rpi/parentToolCallId"], "root-tool");
                } else {
                    assert!(update.get("_meta").is_none());
                }
            }
        }
    }

    #[test]
    fn negotiation_is_object_only_in_v1() {
        for value in [Value::Null, json!(false), json!(true), json!([]), json!(1)] {
            assert!(!negotiated(
                &json!({"protocolVersion":1,"clientCapabilities":{"subagents":value}})
            ));
        }
        assert!(negotiated(
            &json!({"protocolVersion":1,"clientCapabilities":{"subagents":{}}})
        ));
        assert!(negotiated(&json!({"protocolVersion":2})));
    }
    #[test]
    fn announcement_precedes_child_and_state_is_mirrored() {
        let mut r = Roster::new("root".into(), true);
        let updates = r.event(&start("child")).unwrap();
        assert_eq!(updates[0]["sessionId"], "root");
        assert_eq!(updates[0]["update"]["sessionId"], "child:4:root:child");
        assert_eq!(updates[0]["update"]["capabilities"], json!({}));
        assert_eq!(updates[1]["sessionId"], "child:4:root:child");
        let updates = r.event(&json!({"type":"host_closed"})).unwrap();
        assert_eq!(updates[0]["update"]["state"], json!({"state":"unknown"}));
        assert_eq!(updates[1]["update"]["state"], "unknown");
    }
    #[test]
    fn strict_roster_and_no_parent_forwarding() {
        let mut r = Roster::new("root".into(), false);
        assert!(r.event(&nested("missing", json!({}))).is_err());
        assert!(r
            .event(&json!({"type":"message_update","delta":"parent"}))
            .unwrap()
            .is_empty());
        r.event(&start("c")).unwrap();
        assert!(r.event(&start("c")).is_err());
        let mut wrong = nested("c", json!({}));
        wrong["parentSessionId"] = json!("other");
        assert!(r.event(&wrong).is_err());
        assert!(r.event(&start(&"x".repeat(MAX_ID + 1))).is_err());
        for n in 1..MAX_CHILDREN {
            r.event(&start(&format!("c{n}"))).unwrap();
        }
        assert!(r.event(&start("overflow")).is_err());
    }
    #[test]
    fn canonical_ids_and_final_snapshot_repair_before_idle() {
        let mut r = Roster::new("root".into(), true);
        r.event(&start("c")).unwrap();
        let delta = json!({"type":"message_update","eventType":"text_delta","delta":"hello", "messageId":"canonical"});
        let updates = r.event(&nested("c", delta)).unwrap();
        assert_eq!(updates[0]["update"]["messageId"], "canonical");
        let updates = r.event(&json!({"type":"child_end","sessionId":"c","parentSessionId":"root","status":"done","result":{"status":"completed","text":"hello world"}})).unwrap();
        assert_eq!(updates[0]["update"]["content"]["text"], " world");
        assert_eq!(updates[1]["update"]["state"]["stopReason"], "end_turn");
        assert_eq!(updates[2]["update"]["stopReason"], "end_turn");
        assert!(r.event(&nested("c", json!({}))).is_err());
    }
    #[test]
    fn missing_identity_and_byte_limits_fail_explicitly() {
        let mut r = Roster::new("root".into(), true);
        r.event(&start("c")).unwrap();
        let delta = json!({"type":"message_update","eventType":"text_delta","delta":"hello"});
        assert!(r.event(&nested("c", delta)).unwrap().is_empty());
        assert!(r
            .event(&nested("c", json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}})))
            .unwrap_err()
            .contains("canonical"));
        assert!(r
            .event(&nested("c", json!({"padding":"x".repeat(MAX_BYTES)})))
            .is_err());
    }
    #[test]
    fn buffered_text_uses_committed_identity_and_missing_end_id_fails() {
        let mut r = Roster::new("root".into(), true);
        r.event(&start("c")).unwrap();
        assert!(r.event(&nested("c", json!({"type":"message_update","eventType":"text_delta","delta":"hello"}))).unwrap().is_empty());
        let updates = r.event(&nested("c", json!({"type":"message_end","message":{"role":"assistant","id":"committed","content":[{"type":"text","text":"hello world"}]}}))).unwrap();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0]["update"]["content"]["text"], "hello");
        assert_eq!(updates[1]["update"]["content"]["text"], " world");
        assert!(updates.iter().all(|p| p["update"]["messageId"] == "committed"));
        let mut r = Roster::new("root".into(), true);
        r.event(&start("c")).unwrap();
        r.event(&nested("c", json!({"type":"message_update","eventType":"text_delta","delta":"hello"}))).unwrap();
        assert!(r.event(&json!({"type":"child_end","sessionId":"c","parentSessionId":"root"})).unwrap_err().contains("canonical"));
    }
    #[test]
    fn root_scoping_and_nested_parent_ordering() {
        let mut a = Roster::new("root".into(), true);
        let mut b = Roster::new("other".into(), true);
        let a_id = a.event(&start("c")).unwrap()[0]["update"]["sessionId"].clone();
        let mut other = start("c");
        other["parentSessionId"] = json!("other");
        let b_id = b.event(&other).unwrap()[0]["update"]["sessionId"].clone();
        assert_ne!(a_id, b_id);
        let updates = a.event(&json!({"type":"child_start","parentSessionId":"c","sessionId":"nested"})).unwrap();
        assert_eq!(updates[0]["sessionId"], a_id);
        assert_eq!(updates[0]["update"]["sessionId"], "child:4:root:nested");
        assert_eq!(updates[1]["sessionId"], updates[0]["update"]["sessionId"]);
    }
    #[test]
    fn malformed_unknown_and_cumulative_bytes_are_errors() {
        for event in [
            json!({"type":"future_event"}),
            json!({"type":"client_request","method":"fs/read_text_file"}),
            json!({"type":"message_update","eventType":"future_delta"}),
            json!({"type":"message_update","eventType":"text_delta","delta":42}),
            json!({"type":"message_end"}),
            json!({"type":"tool_execution_start"}),
            json!({"type":"message_update","eventType":"text_delta","delta":"x","sessionId":"wrong"}),
        ] {
            let mut r = Roster::new("root".into(), true);
            r.event(&start("c")).unwrap();
            assert!(r.event(&nested("c", event.clone())).is_err(), "{event}");
        }
        let mut r = Roster::new("root".into(), true);
        r.event(&start("c")).unwrap();
        let event = nested("c", json!({"type":"message_update","eventType":"text_delta","delta":"x".repeat(MAX_BYTES / 2)}));
        assert!(r.event(&event).unwrap().is_empty());
        assert!(r.event(&event).unwrap().is_empty());
        assert!(r.event(&event).unwrap_err().contains("256 KiB"));
    }
    fn user_event(kind: &str, mid: Option<&str>, content: Value) -> Value {
        let mut event = json!({"type":kind,"message":{
            "kind":"user","role":"user","content":content,"timestamp":1793317878830_u64,
        }});
        if let Some(mid) = mid {
            event["messageId"] = json!(mid);
        }
        event
    }

    #[test]
    fn native_input_is_one_committed_incoming_upsert_in_both_versions() {
        for v2 in [false, true] {
            let mut r = Roster::new("root".into(), v2);
            // Actual native shape: string content and top-level messageId.
            assert!(r.event(&nested("c", user_event("message_end", Some("input"), json!("task")))).is_err());
            let announcement = r.event(&start("c")).unwrap();
            assert_eq!(announcement[0]["update"]["capabilities"], json!({}));
            assert!(r.event(&nested("c", user_event("message_start", Some("input"), json!("draft")))).unwrap().is_empty());
            let commit = nested("c", user_event("message_end", Some("input"), json!("final task")));
            let updates = r.event(&commit).unwrap();
            assert_eq!(updates, vec![json!({"sessionId":"child:4:root:c","update":{
                "sessionUpdate":"session_message","messageId":"input",
                "senderSessionId":"root","recipientSessionId":"child:4:root:c",
                "content":[{"type":"text","text":"final task"}],
            }})]);
            assert!(r.event(&commit).unwrap().is_empty());
            assert!(r.children["c"].message_id.is_none());
            assert!(r.children["c"].projection.text.is_empty());
            assert!(r.children["c"].pending_text.is_empty());
            // Identical content under another actual ID is another entry.
            let next = r.event(&nested("c", user_event("message_end", Some("input-2"), json!("final task")))).unwrap();
            assert_eq!(next.len(), 1);
            assert_eq!(next[0]["update"]["messageId"], "input-2");
        }
    }

    #[test]
    fn input_preserves_blocks_and_scopes_nested_participants() {
        let mut r = Roster::new("root".into(), true);
        r.event(&start("c")).unwrap();
        r.event(&json!({"type":"child_start","parentSessionId":"c","sessionId":"nested"})).unwrap();
        let blocks = json!([{"type":"text","text":"one","_meta":{"test":true}}, {"type":"text","text":"two"}]);
        let mut event = nested("nested", user_event("message_end", Some("native-input"), blocks.clone()));
        event["parentSessionId"] = json!("c");
        let updates = r.event(&event).unwrap();
        assert_eq!(updates[0]["sessionId"], "child:4:root:nested");
        assert_eq!(updates[0]["update"]["senderSessionId"], "child:4:root:c");
        assert_eq!(updates[0]["update"]["recipientSessionId"], "child:4:root:nested");
        assert_eq!(updates[0]["update"]["content"], blocks);
        assert_eq!(updates[0]["update"]["messageId"], "native-input");
    }

    #[test]
    fn input_does_not_reuse_or_flush_assistant_identity_or_private_thoughts() {
        let mut r = Roster::new("root".into(), true);
        r.event(&start("c")).unwrap();
        r.event(&nested("c", json!({"type":"message_start","message":{"role":"assistant","content":[]}}))).unwrap();
        r.event(&nested("c", json!({"type":"message_update","eventType":"thinking_delta","delta":"private"}))).unwrap();
        let updates = r.event(&nested("c", user_event("message_end", Some("input"), json!("task")))).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0]["update"]["sessionUpdate"], "session_message");
        let updates = r.event(&nested("c", json!({"type":"message_end","messageId":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"answer"}]}}))).unwrap();
        assert!(updates.iter().all(|p| p["update"]["messageId"] == "assistant"));
        assert!(updates.iter().all(|p| p["update"]["sessionUpdate"] != "session_message"));
        let updates = r.event(&json!({"type":"child_end","parentSessionId":"root","sessionId":"c","result":{"status":"completed","text":"answer"}})).unwrap();
        // No genuine native directed-result provenance: omit child->parent view.
        assert!(updates.iter().all(|p| p["update"]["sessionUpdate"] != "session_message"));
    }

    #[test]
    fn input_requires_identity_valid_text_and_bounded_retention() {
        for mid in [None, Some(""), Some(" ")] {
            let mut r = Roster::new("root".into(), true);
            r.event(&start("c")).unwrap();
            assert!(r.event(&nested("c", user_event("message_end", mid, json!("task")))).unwrap_err().contains("canonical"));
        }
        for content in [json!([{"type":"text","text":42}]), json!([{"type":"thinking","thinking":"private"}]), json!(["text"])] {
            let mut r = Roster::new("root".into(), false);
            r.event(&start("c")).unwrap();
            assert!(r.event(&nested("c", user_event("message_end", Some("input"), content))).is_err());
        }
        let mut r = Roster::new("root".into(), true);
        r.event(&start("c")).unwrap();
        assert!(r.event(&nested("c", user_event("message_end", Some(&"i".repeat(MAX_ID + 1)), json!("task")))).is_err());
        assert!(r.event(&nested("c", user_event("message_end", Some("large"), json!("x".repeat(MAX_BYTES))))).is_err());
        for n in 0..MAX_DIRECTED_MESSAGES {
            r.event(&nested("c", user_event("message_end", Some(&format!("input-{n}")), json!("task")))).unwrap();
        }
        assert!(r.event(&nested("c", user_event("message_end", Some("overflow"), json!("task")))).unwrap_err().contains("limit"));
        assert!(r.event(&nested("c", user_event("message_end", Some("input-0"), json!("task")))).unwrap().is_empty());
    }

    #[test]
    fn terminal_reasons_follow_native_outcomes() {
        let p = MessageProjection::default();
        for (event, reason) in [
            (json!({"result":{"status":"aborted"}}), "cancelled"),
            (
                json!({"status":"failed","error":"parent run cancelled"}),
                "cancelled",
            ),
            (
                json!({"status":"failed","error":"child timed out after 1 seconds"}),
                "error",
            ),
            (
                json!({"result":{"status":"completed","turnLimited":true}}),
                "max_turn_requests",
            ),
        ] {
            assert_eq!(terminal_state(&event, &p)["stopReason"], reason);
        }
    }
}
