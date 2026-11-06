//! Shared node payloads. Wire field names and defaults are compatibility contracts.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Health {
    #[serde(default = "default_health_ok")]
    pub ok: bool,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
}

fn default_health_ok() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Capabilities {
    pub protocol: String,
    #[serde(default, rename = "piCompatibilityTarget")]
    pub pi_compatibility_target: Option<String>,
    #[serde(default, rename = "supportedCommands")]
    pub supported_commands: Vec<String>,
    #[serde(default, rename = "supportedActions")]
    pub supported_actions: Vec<String>,
    #[serde(default)]
    pub semantics: Option<Value>,
    #[serde(default)]
    pub limits: Option<Value>,
    #[serde(default, rename = "parityComplete")]
    pub parity_complete: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AuthState {
    pub required: bool,
    pub authenticated: bool,
}

/// A short-lived, single-use credential. Intentionally not `Debug`.
#[derive(Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct QrAuthorization {
    pub ticket: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: u64,
    #[serde(rename = "nodeId")]
    pub node_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ProjectCreation {
    pub project: Project,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ProjectFolder {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ProjectFolders {
    pub root: String,
    pub path: String,
    pub folders: Vec<ProjectFolder>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct FileListing {
    pub path: String,
    pub entries: Vec<FileEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct FileContent {
    pub path: String,
    pub content: String,
}

/// Maximum encoded project raster size; decoding/rendering is the caller's concern.
pub const PROJECT_IMAGE_MAX_BYTES: usize = 20 * 1024 * 1024;

/// Binary raster content, independent of HTTP and JSON transport encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectImage {
    pub bytes: Vec<u8>,
    pub media_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ChangedFile {
    pub path: String,
    #[serde(rename = "originalPath")]
    pub original_path: Option<String>,
    pub status: String,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct GitStatus {
    #[serde(rename = "isRepository")]
    pub is_repository: bool,
    pub branch: Option<String>,
    #[serde(rename = "repositoryRoot")]
    pub repository_root: Option<String>,
    #[serde(rename = "worktreePath")]
    pub worktree_path: Option<String>,
    pub files: Vec<ChangedFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Diff {
    pub path: String,
    pub patch: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct QueueItem {
    #[serde(rename = "entryId")]
    pub entry_id: String,
    pub queue: String,
    pub text: String,
    pub status: Option<String>,
    #[serde(rename = "completedSeq")]
    pub completed_seq: Option<u64>,
    pub message: Option<Value>,
    #[serde(rename = "runId")]
    pub run_id: Option<String>,
    pub seq: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct QueueSnapshot {
    pub items: Vec<QueueItem>,
    pub count: u64,
}

/// Presentation data only; unknown fields (including questions[]) remain available
/// to consumers so an unsupported request cannot silently become a different one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiDialogPayload {
    pub purpose: Option<String>,
    pub question: Option<String>,
    pub message: Option<String>,
    pub context: Option<String>,
    pub kind: Option<String>,
    pub options: Vec<UiDialogOption>,
    pub allow_freeform: bool,
    pub allow_multiple: bool,
    pub raw: Value,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiDialogOption {
    pub title: Option<String>,
    pub value: Option<Value>,
    pub permission: Option<String>,
    pub description: Option<String>,
    pub raw: Value,
}

impl<'de> Deserialize<'de> for UiDialogPayload {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        if !raw.is_object() {
            return Err(serde::de::Error::custom("dialog UI must be an object"));
        }
        let text = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_owned);
        let flag = |keys: &[&str]| {
            keys.iter()
                .any(|key| raw.get(*key).and_then(Value::as_bool) == Some(true))
        };
        let options = raw
            .get("options")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .cloned()
                    .map(serde_json::from_value)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(serde::de::Error::custom)?
            .unwrap_or_default();
        Ok(Self {
            purpose: text("purpose"),
            question: text("question"),
            message: text("message"),
            context: text("context"),
            kind: text("kind"),
            options,
            allow_freeform: flag(&["allowFreeform", "allow_freeform", "allow_custom"]),
            allow_multiple: flag(&["allowMultiple", "allow_multiple", "multiple"]),
            raw,
        })
    }
}

impl<'de> Deserialize<'de> for UiDialogOption {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        let text = |keys: &[&str]| {
            keys.iter()
                .find_map(|key| raw.get(*key).and_then(Value::as_str).map(str::to_owned))
        };
        Ok(Self {
            title: raw
                .as_str()
                .map(str::to_owned)
                .or_else(|| text(&["title", "label", "text", "value", "name"])),
            value: raw.get("value").cloned(),
            permission: text(&["permission"]),
            description: text(&["description"]),
            raw,
        })
    }
}

impl Serialize for UiDialogPayload {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.raw.is_object()
            && serde_json::from_value::<Self>(self.raw.clone()).is_ok_and(|parsed| &parsed == self)
        {
            return self.raw.serialize(serializer);
        }
        let mut raw = self.raw.as_object().cloned().unwrap_or_default();
        for (key, value) in [
            ("purpose", &self.purpose),
            ("question", &self.question),
            ("message", &self.message),
            ("context", &self.context),
            ("kind", &self.kind),
        ] {
            if let Some(value) = value {
                raw.insert(key.into(), json!(value));
            }
        }
        if !self.options.is_empty() {
            raw.insert("options".into(), json!(self.options));
        }
        if self.allow_freeform {
            raw.insert("allowFreeform".into(), json!(true));
        }
        if self.allow_multiple {
            raw.insert("allowMultiple".into(), json!(true));
        }
        raw.serialize(serializer)
    }
}

impl Serialize for UiDialogOption {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serde_json::from_value::<Self>(self.raw.clone()).is_ok_and(|parsed| &parsed == self) {
            return self.raw.serialize(serializer);
        }
        let mut raw = self.raw.as_object().cloned().unwrap_or_default();
        for (key, value) in [
            ("title", &self.title),
            ("permission", &self.permission),
            ("description", &self.description),
        ] {
            if let Some(value) = value {
                raw.insert(key.into(), json!(value));
            }
        }
        if let Some(value) = &self.value {
            raw.insert("value".into(), value.clone());
        }
        raw.serialize(serializer)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct QueueMutation {
    pub accepted: Option<bool>,
    #[serde(rename = "entryId")]
    pub entry_id: Option<String>,
    pub queue: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct QueueCancellation {
    #[serde(rename = "entryId")]
    pub entry_id: String,
    pub outcome: String,
}
/// An agent profile offered by the workspace. Native availability never implies authentication.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentBackendOption {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub reason: Option<String>,
}

/// Stable workspace identity is independent of the agent's opaque session identity.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionAgentBinding {
    pub backend: String,
    pub external_session_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct SessionSummary {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "projectId")]
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
    #[serde(rename = "lastSeq")]
    pub last_seq: u64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default, rename = "pendingResume")]
    pub pending_resume: bool,
    #[serde(default, rename = "latestSettlement")]
    pub latest_settlement: Option<Value>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct SessionPreferences {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub archived: bool,
    pub pinned: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct RuntimeState {
    #[serde(rename = "modelId")]
    pub model_id: Option<String>,
    pub provider: Option<String>,
    #[serde(rename = "thinkingLevel")]
    pub thinking_level: Option<String>,
    #[serde(rename = "activeTools", default)]
    pub active_tools: Vec<String>,
    pub context: Option<Value>,
    pub queue: Option<Value>,
    pub stats: Option<Value>,
    #[serde(rename = "activeOperation")]
    pub active_operation: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ModelDescriptor {
    pub id: String,
    pub name: String,
    pub provider: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(rename = "contextWindow", default)]
    pub context_window: Option<u64>,
    #[serde(rename = "maxTokens", default)]
    pub max_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AuthStatusProvider {
    pub provider: String,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub oauth: bool,
    #[serde(rename = "oauthAvailable", default)]
    pub oauth_available: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Default)]
pub struct AuthStatus {
    #[serde(default)]
    pub providers: Vec<AuthStatusProvider>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ToolCatalog {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(rename = "activeTools", default)]
    pub active_tools: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct CompactionEntry {
    pub id: Option<String>,
    pub seq: Option<u64>,
    pub summary: Option<String>,
    #[serde(rename = "tokensBefore")]
    pub tokens_before: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct SessionBranch {
    #[serde(default)]
    pub entries: Vec<Value>,
    #[serde(default)]
    pub count: u64,
}

impl SessionBranch {
    /// Extract only public compaction summaries. Message entries, including
    /// assistant thinking, are deliberately never returned to the UI.
    #[must_use]
    pub fn latest_compaction(&self) -> Option<CompactionEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("compaction"))
            .map(|entry| CompactionEntry {
                id: entry.get("id").and_then(Value::as_str).map(str::to_owned),
                seq: entry.get("seq").and_then(Value::as_u64),
                summary: entry
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                tokens_before: entry.get("tokensBefore").and_then(Value::as_u64),
            })
            .max_by_key(|entry| entry.seq.unwrap_or_default())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct History {
    #[serde(default)]
    pub records: Vec<Value>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub limit: u64,
    #[serde(rename = "afterSeq", default)]
    pub after_seq: Option<u64>,
    #[serde(rename = "beforeSeq", default)]
    pub before_seq: Option<u64>,
    #[serde(rename = "firstSeq", default)]
    pub first_seq: Option<u64>,
    #[serde(rename = "lastSeq", default)]
    pub last_seq: Option<u64>,
    #[serde(rename = "hasMore", default)]
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct SessionMutation {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "sourceSessionId")]
    pub source_session_id: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct SnapshotEvent {
    pub seq: u64,
    pub event: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct HostResponse<T> {
    #[serde(rename = "type")]
    pub response_type: String,
    pub id: Option<Value>,
    pub command: String,
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct PendingDialog {
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: Option<String>,
    #[serde(rename = "responseId")]
    pub response_id: Option<Value>,
    pub ui: UiDialogPayload,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Snapshot {
    /// Persisted branch leaf at the snapshot boundary, for pinned timeline reads.
    #[serde(default, rename = "leafId")]
    pub leaf_id: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// Host state at the snapshot boundary, before the incremental `events`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, rename = "lastSeq")]
    pub last_seq: u64,
    #[serde(default, rename = "pendingResume")]
    pub pending_resume: bool,
    #[serde(default, rename = "pendingDialog")]
    pub pending_dialog: Option<PendingDialog>,
    #[serde(default, rename = "pendingDialogs")]
    pub pending_dialogs: Vec<PendingDialog>,
    #[serde(default, rename = "latestSettlement")]
    pub latest_settlement: Option<Value>,
    #[serde(default, rename = "firstId")]
    pub first_id: Option<String>,
    #[serde(default, rename = "lastId")]
    pub last_id: Option<String>,
    #[serde(default, rename = "hasMore", alias = "has_more")]
    pub has_more: bool,
    #[serde(default)]
    pub events: Vec<SnapshotEvent>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct WireEvent {
    pub id: Option<String>,
    pub seq: Option<u64>,
    pub event_type: String,
    pub data: Value,
}
