//! Shared, protocol-neutral durable state primitives.
//!
//! This crate deliberately owns no session, storage, worker, or network code.
//! It owns only the canonical entry discriminator, the bounded projection model,
//! and replay of `rpi-durable` journal records.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const ENTRY_TYPE: &str = "durable-state";
pub const LEGACY_ENTRY_TYPE: &str = "rpi-durable-runtime";
pub const JOURNAL_FORMAT: &str = "rpi-durable-journal";
pub const JOURNAL_VERSION: u64 = 1;
pub const MAX_QUEUE_ITEMS: usize = 32;
pub const MAX_TOOL_ENTRIES: usize = 64;
pub const MAX_SUMMARY_CHARS: usize = 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DurableMutation {
    StartRun,
    SettleRun,
    RecoverRun,
    Enqueue {
        text: String,
    },
    Claim {
        id: String,
    },
    Finish {
        id: String,
        status: QueueStatus,
        note: String,
    },
    Cancel {
        id: String,
        note: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub name: String,
    pub status: WorkflowStatus,
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Running,
    Paused,
    Completed,
    Failed,
}

impl WorkflowStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: String,
    pub title: String,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
}

impl StepStatus {
    pub fn marker(&self) -> &'static str {
        match self {
            Self::Pending => " ",
            Self::InProgress => "•",
            Self::Completed => "✓",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableState {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub next_run_id: u64,
    #[serde(default)]
    pub next_queue_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<Workflow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunCheckpoint>,
    #[serde(default)]
    pub queue: Vec<QueueItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Settled,
    Failed,
    Interrupted,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Settled => "settled",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Started,
    Completed,
    Failed,
    Interrupted,
    Unknown,
}

impl ToolStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCheckpoint {
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_key: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub params: String,
    pub status: ToolStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCheckpoint {
    pub id: String,
    pub status: RunStatus,
    #[serde(default)]
    pub started_revision: u64,
    #[serde(default)]
    pub updated_revision: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recovery_of: String,
    #[serde(default)]
    pub tools: Vec<ToolCheckpoint>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueStatus {
    Queued,
    Claimed,
    Completed,
    Failed,
    Cancelled,
}

impl QueueStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub id: String,
    pub text: String,
    pub status: QueueStatus,
    #[serde(default)]
    pub created_revision: u64,
    #[serde(default)]
    pub updated_revision: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl DurableState {
    pub fn entry(&self) -> Value {
        json!({"customType": ENTRY_TYPE, "data": self})
    }
    /// Decode a branch and fail on malformed journal or state data.
    pub fn try_from_branch(entries: &[Value]) -> Result<Self, String> {
        replay_entries(entries)?
            .map(|value| {
                serde_json::from_value(value)
                    .map_err(|error| format!("decode durable state: {error}"))
            })
            .transpose()
            .map(|state| state.unwrap_or_default())
    }

    /// Decode a branch for compatibility callers that intentionally treat an
    /// absent or malformed durable projection as the empty state. Runtime
    /// attachment and refresh paths should use `try_from_branch` instead.
    pub fn from_branch(entries: &[Value]) -> Self {
        replay_entries(entries)
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }
}

pub fn replay_entries(entries: &[Value]) -> Result<Option<Value>, String> {
    let mut state: Option<Value> = None;
    for entry in entries {
        let custom_type = entry.get("customType").and_then(Value::as_str);
        if custom_type != Some(ENTRY_TYPE) && custom_type != Some(LEGACY_ENTRY_TYPE) {
            continue;
        }
        let Some(data) = entry.get("data") else {
            continue;
        };
        if data.get("format").and_then(Value::as_str) == Some(JOURNAL_FORMAT) {
            let version = data.get("version").and_then(Value::as_u64).unwrap_or(0);
            if version > JOURNAL_VERSION {
                return Err(format!("unsupported durable journal version: {version}"));
            }
            if let Some(snapshot) = data.get("state") {
                state = Some(snapshot.clone());
            }
            if let Some(changes) = data.get("changes").and_then(Value::as_array) {
                let current = state.get_or_insert_with(|| json!({}));
                for change in changes {
                    apply_change(current, change)?;
                }
            }
            if let Some(revision) = data.get("revision").and_then(Value::as_u64) {
                if let Some(object) = state.as_mut().and_then(Value::as_object_mut) {
                    object.insert("revision".into(), json!(revision));
                }
            }
        } else if data.is_object() {
            state = Some(data.clone());
        }
    }
    Ok(state)
}

fn apply_change(state: &mut Value, change: &Value) -> Result<(), String> {
    let Some(op) = change.get("op").and_then(Value::as_str) else {
        return Err("durable journal change has no op".into());
    };
    let object = state
        .as_object_mut()
        .ok_or_else(|| "durable journal state is not an object".to_string())?;
    match op {
        "setWorkflow" => set(
            object,
            "workflow",
            change.get("workflow").cloned().unwrap_or(Value::Null),
        ),
        "setCounters" => {
            set(
                object,
                "nextRunId",
                change.get("nextRunId").cloned().unwrap_or(json!(0)),
            );
            set(
                object,
                "nextQueueId",
                change.get("nextQueueId").cloned().unwrap_or(json!(0)),
            );
        }
        "setRun" => set(
            object,
            "run",
            change.get("run").cloned().unwrap_or(Value::Null),
        ),
        "setTools" => {
            let run = object.entry("run").or_insert_with(|| json!({}));
            run.as_object_mut()
                .ok_or_else(|| "durable run is not an object".to_string())?
                .insert(
                    "tools".into(),
                    change.get("tools").cloned().unwrap_or(json!([])),
                );
        }
        "setQueue" => set(
            object,
            "queue",
            change.get("queue").cloned().unwrap_or(json!([])),
        ),
        "setWorkflowMeta" => {
            if let Some(workflow) = object.get_mut("workflow").and_then(Value::as_object_mut) {
                for key in ["name", "status", "reason"] {
                    if let Some(value) = change.get(key) {
                        workflow.insert(key.into(), value.clone());
                    }
                }
            }
        }
        "setRunMeta" => {
            if let Some(run) = object.get_mut("run").and_then(Value::as_object_mut) {
                for key in [
                    "id",
                    "status",
                    "startedRevision",
                    "updatedRevision",
                    "outcome",
                    "recoveryOf",
                ] {
                    if let Some(value) = change.get(key) {
                        run.insert(key.into(), value.clone());
                    }
                }
            }
        }
        "updateStep" | "appendTool" | "updateTool" | "appendQueue" | "updateQueue" => {
            apply_collection_change(object, op, change)?
        }
        other => return Err(format!("unsupported durable journal operation: {other}")),
    }
    if let Some(revision) = change.get("revision").and_then(Value::as_u64) {
        object.insert("revision".into(), json!(revision));
    }
    Ok(())
}

fn set(object: &mut Map<String, Value>, key: &str, value: Value) {
    object.insert(key.into(), value);
}

fn apply_collection_change(
    object: &mut Map<String, Value>,
    op: &str,
    change: &Value,
) -> Result<(), String> {
    match op {
        "updateStep" => {
            let workflow = object
                .get_mut("workflow")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| "durable workflow is not an object".to_string())?;
            let steps = workflow
                .get_mut("steps")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| "durable workflow steps are not an array".to_string())?;
            let id = change
                .get("stepId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(step) = steps
                .iter_mut()
                .find(|step| step.get("id").and_then(Value::as_str) == Some(id))
                .and_then(Value::as_object_mut)
            {
                if let Some(status) = change.get("status") {
                    step.insert("status".into(), status.clone());
                }
                if let Some(note) = change.get("note") {
                    step.insert("note".into(), note.clone());
                }
            }
        }
        "appendTool" | "appendQueue" => {
            let (container, key, value_key) = if op == "appendTool" {
                ("run", "tools", "tool")
            } else {
                ("root", "queue", "item")
            };
            let items = if container == "root" {
                object.entry(key).or_insert_with(|| json!([]))
            } else {
                object
                    .entry(container)
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .ok_or_else(|| format!("durable {container} is not an object"))?
                    .entry(key)
                    .or_insert_with(|| json!([]))
            };
            items
                .as_array_mut()
                .ok_or_else(|| format!("durable {key} is not an array"))?
                .push(change.get(value_key).cloned().unwrap_or(Value::Null));
        }
        "updateTool" => {
            let run = object
                .get_mut("run")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| "durable run is not an object".to_string())?;
            let tools = run
                .get_mut("tools")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| "durable run tools are not an array".to_string())?;
            let id = change
                .get("callId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(tool) = tools
                .iter_mut()
                .find(|tool| tool.get("callId").and_then(Value::as_str) == Some(id))
                .and_then(Value::as_object_mut)
            {
                for key in ["callKey", "status", "result"] {
                    if let Some(value) = change.get(key) {
                        tool.insert(key.into(), value.clone());
                    }
                }
            }
        }
        "updateQueue" => {
            let items = object
                .get_mut("queue")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| "durable queue is not an array".to_string())?;
            let id = change
                .get("item")
                .and_then(|item| item.get("id"))
                .and_then(Value::as_str);
            if let Some(item) = items
                .iter_mut()
                .find(|item| id.is_some() && item.get("id").and_then(Value::as_str) == id)
            {
                *item = change.get("item").cloned().unwrap_or(Value::Null);
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

pub fn is_canonical_entry(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("custom")
        && entry.get("customType").and_then(Value::as_str) == Some(ENTRY_TYPE)
}
pub fn is_legacy_entry(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("custom")
        && entry.get("customType").and_then(Value::as_str) == Some(LEGACY_ENTRY_TYPE)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replays_snapshot_and_delta_entries() {
        let entries = vec![
            json!({"type":"custom","customType":ENTRY_TYPE,"data":{"format":JOURNAL_FORMAT,"version":JOURNAL_VERSION,"kind":"snapshot","revision":1,"state":{"revision":1,"queue":[]}}}),
            json!({"type":"custom","customType":ENTRY_TYPE,"data":{"format":JOURNAL_FORMAT,"version":JOURNAL_VERSION,"kind":"delta","revision":2,"changes":[{"op":"appendQueue","item":{"id":"item-1","text":"compile","status":"queued"}}]}}),
        ];
        let state = DurableState::from_branch(&entries);
        assert_eq!(state.revision, 2);
        assert_eq!(state.queue[0].text, "compile");
    }
    #[test]
    fn accepts_legacy_runtime_snapshot_without_writing_it() {
        let entries = vec![
            json!({"type":"custom","customType":LEGACY_ENTRY_TYPE,"data":{"revision":4,"queue":[]}}),
        ];
        assert_eq!(DurableState::from_branch(&entries).revision, 4);
        assert!(is_legacy_entry(&entries[0]));
    }
    #[test]
    fn malformed_journal_fails_closed() {
        let entries = vec![
            json!({"type":"custom","customType":ENTRY_TYPE,"data":{"format":JOURNAL_FORMAT,"version":JOURNAL_VERSION,"changes":[{"revision":2}]}}),
        ];
        let error = replay_entries(&entries).unwrap_err();
        assert!(error.contains("has no op"));
        assert!(DurableState::try_from_branch(&entries).is_err());
        assert_eq!(DurableState::from_branch(&entries), DurableState::default());
    }
    #[test]
    fn entry_classification_is_strict_and_migration_is_read_only() {
        let canonical = json!({"type":"custom","customType":ENTRY_TYPE});
        let legacy = json!({"type":"custom","customType":LEGACY_ENTRY_TYPE});
        let message = json!({"type":"message","customType":ENTRY_TYPE});
        assert!(is_canonical_entry(&canonical));
        assert!(is_legacy_entry(&legacy));
        assert!(!is_canonical_entry(&legacy));
        assert!(!is_legacy_entry(&message));
        assert_eq!(DurableState::default().entry()["customType"], ENTRY_TYPE);
    }
}
