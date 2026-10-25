//! Protocol-neutral durable run semantics.
//!
//! [`DurableCoordinator`] folds projected native lifecycle events into a
//! bounded, serializable ledger. It does not execute or replay tools. A host
//! integration can persist [`DurableState::entry`] in the native session
//! branch and expose the same state through HTTP, ACP or another adapter.
use crate::host::HostSession;
use futures_util::{stream, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Native custom-entry discriminator for the durable runtime ledger.
pub const ENTRY_TYPE: &str = "rpi-durable-runtime";
/// Maximum number of retained queue items.
pub const MAX_QUEUE_ITEMS: usize = 32;
/// Maximum number of retained tool checkpoints per run.
pub const MAX_TOOL_ENTRIES: usize = 64;
/// Maximum number of durable state changes retained for reconnect replay.
pub const MAX_CHANGE_ENTRIES: usize = 128;
/// Maximum length of persisted tool and queue summaries.
pub const MAX_SUMMARY_CHARS: usize = 1_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
/// A durable state change published after native append succeeds.
pub struct DurableChange {
    pub revision: u64,
    pub state: DurableState,
}

pub enum DurableStreamItem {
    Change(DurableChange),
    ResyncRequired { dropped: u64 },
}

pub struct DurableSubscription {
    pub ready: DurableState,
    pub replay: Vec<DurableChange>,
    pub receiver: tokio::sync::broadcast::Receiver<DurableChange>,
    pub gap: bool,
    pub after: Option<u64>,
}

pub fn replay_changes(
    replay: Vec<DurableChange>,
    receiver: tokio::sync::broadcast::Receiver<DurableChange>,
    after: Option<u64>,
) -> impl Stream<Item = DurableStreamItem> {
    let cursor = replay
        .last()
        .map(|change| change.revision)
        .unwrap_or_default()
        .max(after.unwrap_or_default());
    stream::iter(replay.into_iter().map(DurableStreamItem::Change)).chain(stream::unfold(
        (receiver, cursor),
        |(mut receiver, cursor)| async move {
            loop {
                match receiver.recv().await {
                    Ok(change) if change.revision <= cursor => continue,
                    Ok(change) => {
                        let cursor = change.revision;
                        return Some((DurableStreamItem::Change(change), (receiver, cursor)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                        return Some((
                            DurableStreamItem::ResyncRequired { dropped },
                            (receiver, cursor),
                        ))
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    ))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Serializable durable runtime state for one native session branch.
pub struct DurableState {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub next_run_id: u64,
    #[serde(default)]
    pub next_queue_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunCheckpoint>,
    #[serde(default)]
    pub queue: Vec<QueueItem>,
}

impl DurableState {
    /// Recover the newest valid runtime entry from branch records.
    pub fn from_branch(entries: &[Value]) -> Self {
        entries
            .iter()
            .filter(|entry| {
                entry.get("type").and_then(Value::as_str) == Some("custom")
                    && entry.get("customType").and_then(Value::as_str) == Some(ENTRY_TYPE)
            })
            .filter_map(|entry| entry.get("data"))
            .filter_map(|data| serde_json::from_value(data.clone()).ok())
            .next_back()
            .unwrap_or_default()
    }

    /// Encode this state as the payload accepted by the native append action.
    pub fn entry(&self) -> Value {
        json!({"customType": ENTRY_TYPE, "data": self})
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Lifecycle state of an agent run.
pub enum RunStatus {
    Queued,
    Running,
    Settled,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Lifecycle state of one tool execution.
pub enum ToolStatus {
    Started,
    Completed,
    Failed,
    Interrupted,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Bounded, replay-safe metadata for one tool execution.
pub struct ToolCheckpoint {
    pub call_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub params: String,
    pub status: ToolStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Persisted run metadata and its tool journal.
pub struct RunCheckpoint {
    pub id: String,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recovery_of: String,
    #[serde(default)]
    pub tools: Vec<ToolCheckpoint>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// State of a manually coordinated durable work item.
pub enum QueueStatus {
    Queued,
    Claimed,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A protocol-neutral mutation exposed by durable control-plane adapters.
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
/// A branch-scoped work item; this crate does not run it automatically.
pub struct QueueItem {
    pub id: String,
    pub text: String,
    pub status: QueueStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Validation and conflict errors from durable state transitions.
pub enum DurableError {
    Invalid(String),
    NotFound(String),
    Conflict(String),
    Limit(String),
}

impl std::fmt::Display for DurableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Limit(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for DurableError {}

pub struct DurableCoordinator {
    state: DurableState,
}

impl DurableCoordinator {
    /// Create a coordinator from recovered branch state.
    pub fn new(state: DurableState) -> Self {
        Self { state }
    }
    /// Inspect the current state without mutating it.
    pub fn state(&self) -> &DurableState {
        &self.state
    }
    pub fn into_state(self) -> DurableState {
        self.state
    }

    /// Apply one protocol-neutral control-plane mutation.
    pub fn apply(&mut self, mutation: DurableMutation) -> Result<Value, DurableError> {
        match mutation {
            DurableMutation::StartRun => Ok(json!({
                "changed": true,
                "run": self.start_run().clone(),
            })),
            DurableMutation::SettleRun => Ok(json!({"changed": self.settle_run()})),
            DurableMutation::RecoverRun => Ok(json!({
                "changed": true,
                "run": self.recover_run()?.clone(),
            })),
            DurableMutation::Enqueue { text } => Ok(json!({
                "changed": true,
                "item": self.enqueue(&text)?.clone(),
            })),
            DurableMutation::Claim { id } => Ok(json!({
                "changed": self.claim(&id)?,
                "itemId": id,
            })),
            DurableMutation::Finish { id, status, note } => Ok(json!({
                "changed": self.finish(&id, status, &note)?,
                "itemId": id,
            })),
            DurableMutation::Cancel { id, note } => Ok(json!({
                "changed": self.cancel(&id, &note)?,
                "itemId": id,
            })),
        }
    }

    /// Start a fresh run, retaining the previous id as recovery context when relevant.
    pub fn start_run(&mut self) -> &RunCheckpoint {
        let recovery_of = self
            .state
            .run
            .as_ref()
            .filter(|run| matches!(run.status, RunStatus::Queued | RunStatus::Running))
            .map(|run| run.id.clone())
            .unwrap_or_default();
        self.state.next_run_id = self.state.next_run_id.saturating_add(1);
        self.state.run = Some(RunCheckpoint {
            id: format!("run-{}", self.state.next_run_id),
            status: RunStatus::Running,
            outcome: String::new(),
            recovery_of,
            tools: Vec::new(),
        });
        self.bump();
        self.state.run.as_ref().expect("run created")
    }

    pub fn settle_run(&mut self) -> bool {
        let Some(run) = self.state.run.as_mut() else {
            return false;
        };
        if run.status != RunStatus::Running {
            return false;
        }
        run.status = RunStatus::Settled;
        run.outcome = "settled".into();
        for tool in &mut run.tools {
            if tool.status == ToolStatus::Started {
                tool.status = ToolStatus::Interrupted;
                tool.result = "run settled without a durable tool result".into();
            }
        }
        self.bump();
        true
    }

    pub fn recover_interrupted(&mut self) -> bool {
        let Some(run) = self.state.run.as_mut() else {
            return false;
        };
        if run.status != RunStatus::Running {
            return false;
        }
        run.status = RunStatus::Interrupted;
        run.outcome = "interrupted".into();
        for tool in &mut run.tools {
            if tool.status == ToolStatus::Started {
                tool.status = ToolStatus::Interrupted;
                if tool.result.is_empty() {
                    tool.result = "execution did not reach a durable result".into();
                }
            }
        }
        self.bump();
        true
    }

    /// Create a queued recovery point without replaying any tool.
    pub fn recover_run(&mut self) -> Result<&RunCheckpoint, DurableError> {
        let previous = self.state.run.as_ref().ok_or_else(|| {
            DurableError::NotFound("no durable run is available to recover".into())
        })?;
        if previous.status != RunStatus::Interrupted {
            return Err(DurableError::Conflict(format!(
                "run is {:?}; only an interrupted run can be recovered",
                previous.status
            )));
        }
        let previous_id = previous.id.clone();
        let tools = previous.tools.clone();
        self.state.next_run_id = self.state.next_run_id.saturating_add(1);
        self.state.run = Some(RunCheckpoint {
            id: format!("run-{}", self.state.next_run_id),
            status: RunStatus::Queued,
            outcome: String::new(),
            recovery_of: previous_id,
            tools,
        });
        self.bump();
        Ok(self.state.run.as_ref().expect("run created"))
    }

    pub fn tool_started(
        &mut self,
        call_id: &str,
        name: &str,
        params: &str,
    ) -> Result<bool, DurableError> {
        if call_id.trim().is_empty() || name.trim().is_empty() {
            return Err(DurableError::Invalid(
                "tool call id and name are required".into(),
            ));
        }
        if self.state.run.is_none() {
            self.start_run();
        }
        if self
            .state
            .run
            .as_ref()
            .is_some_and(|run| run.status != RunStatus::Running)
        {
            return Ok(false);
        }
        let run = self.state.run.as_mut().expect("run exists");
        if run.tools.iter().any(|tool| tool.call_id == call_id) {
            return Ok(false);
        }
        run.tools.push(ToolCheckpoint {
            call_id: summary(call_id),
            name: summary(name),
            params: summary(params),
            status: ToolStatus::Started,
            result: String::new(),
        });
        trim_tools(run);
        self.bump();
        Ok(true)
    }

    pub fn tool_finished(
        &mut self,
        call_id: &str,
        name: &str,
        result: &str,
        is_error: bool,
    ) -> Result<bool, DurableError> {
        if call_id.trim().is_empty() || name.trim().is_empty() {
            return Err(DurableError::Invalid(
                "tool call id and name are required".into(),
            ));
        }
        if self.state.run.is_none() {
            self.start_run();
        }
        let id = summary(call_id);
        let existing = self
            .state
            .run
            .as_ref()
            .and_then(|run| run.tools.iter().position(|tool| tool.call_id == id));
        if let Some(index) = existing {
            let run = self.state.run.as_mut().expect("run exists");
            let tool = &mut run.tools[index];
            if matches!(
                tool.status,
                ToolStatus::Completed
                    | ToolStatus::Failed
                    | ToolStatus::Interrupted
                    | ToolStatus::Unknown
            ) {
                return Ok(false);
            }
            tool.status = if is_error {
                ToolStatus::Failed
            } else {
                ToolStatus::Completed
            };
            tool.result = summary(result);
            self.bump();
            return Ok(true);
        }
        let run = self.state.run.as_mut().expect("run exists");
        run.tools.push(ToolCheckpoint {
            call_id: id,
            name: summary(name),
            params: String::new(),
            status: ToolStatus::Unknown,
            result: summary(result),
        });
        trim_tools(run);
        self.bump();
        Ok(true)
    }

    /// Add a manually coordinated work item to the bounded queue.
    pub fn enqueue(&mut self, text: &str) -> Result<&QueueItem, DurableError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(DurableError::Invalid("queue item text is required".into()));
        }
        if self.state.queue.len() >= MAX_QUEUE_ITEMS {
            return Err(DurableError::Limit(format!(
                "queue cannot contain more than {MAX_QUEUE_ITEMS} items"
            )));
        }
        self.state.next_queue_id = self.state.next_queue_id.saturating_add(1);
        self.state.queue.push(QueueItem {
            id: format!("item-{}", self.state.next_queue_id),
            text: summary(text),
            status: QueueStatus::Queued,
            note: String::new(),
        });
        self.bump();
        Ok(self.state.queue.last().expect("queue item created"))
    }

    pub fn claim(&mut self, id: &str) -> Result<bool, DurableError> {
        let item = self.item_mut(id)?;
        if item.status != QueueStatus::Queued {
            return Err(DurableError::Conflict(format!(
                "queue item {id} is not queued"
            )));
        }
        item.status = QueueStatus::Claimed;
        self.bump();
        Ok(true)
    }

    pub fn finish(
        &mut self,
        id: &str,
        status: QueueStatus,
        note: &str,
    ) -> Result<bool, DurableError> {
        if !matches!(status, QueueStatus::Completed | QueueStatus::Failed) {
            return Err(DurableError::Invalid(
                "queue finish status must be completed or failed".into(),
            ));
        }
        let item = self.item_mut(id)?;
        if item.status != QueueStatus::Claimed {
            return Err(DurableError::Conflict(format!(
                "queue item {id} is not claimed"
            )));
        }
        item.status = status;
        item.note = summary(note);
        self.bump();
        Ok(true)
    }

    pub fn cancel(&mut self, id: &str, note: &str) -> Result<bool, DurableError> {
        let item = self.item_mut(id)?;
        if !matches!(item.status, QueueStatus::Queued | QueueStatus::Claimed) {
            return Err(DurableError::Conflict(format!(
                "queue item {id} cannot be cancelled"
            )));
        }
        item.status = QueueStatus::Cancelled;
        item.note = if note.trim().is_empty() {
            "cancelled".into()
        } else {
            summary(note)
        };
        self.bump();
        Ok(true)
    }

    /// Observe a projected native event. Unknown event types are ignored.
    /// Fold one projected native lifecycle event into the durable state.
    pub fn observe(&mut self, event: &Value) -> Result<bool, DurableError> {
        match event.get("type").and_then(Value::as_str) {
            Some("agent_start") => {
                self.start_run();
                Ok(true)
            }
            Some("tool_execution_start") => self.tool_started(
                &field_text(event, "toolCallId"),
                &field_text(event, "toolName"),
                &field_text(event, "args"),
            ),
            Some("tool_execution_end") | Some("tool_result") => self.tool_finished(
                &field_text(event, "toolCallId"),
                &field_text(event, "toolName"),
                &field_text(event, "result"),
                event
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ),
            Some("agent_settled") => Ok(self.settle_run()),
            Some("host_closed") | Some("session_end") => Ok(self.recover_interrupted()),
            _ => Ok(false),
        }
    }

    fn item_mut(&mut self, id: &str) -> Result<&mut QueueItem, DurableError> {
        self.state
            .queue
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| DurableError::NotFound(format!("queue item not found: {id}")))
    }
    fn bump(&mut self) {
        self.state.revision = self.state.revision.saturating_add(1);
    }
}

/// Branch-backed durable runtime shared by protocol adapters.
///
/// The runtime owns the coordinator and the native branch persistence boundary.
/// Adapters can use it for queries, mutations and lifecycle observations without
/// duplicating rollback or append handling.
pub struct DurableRuntime {
    session: Arc<HostSession>,
    coordinator: Arc<Mutex<DurableCoordinator>>,
    changes: tokio::sync::broadcast::Sender<DurableChange>,
    history: Arc<Mutex<VecDeque<DurableChange>>>,
}

impl DurableRuntime {
    /// Load durable state from the native branch and persist interrupted-run
    /// recovery. This is the strict variant used by the shared Service.
    pub async fn attach(session: Arc<HostSession>) -> Result<Arc<Self>, String> {
        Self::load(session, true).await
    }

    /// Try to attach durable support without making it a prerequisite for a
    /// normal native session. Unsupported optional RPCs return `None`.
    pub async fn try_attach(session: Arc<HostSession>) -> Result<Option<Arc<Self>>, String> {
        if !session.supports_command("get_session_branch")
            || !session.supports_command("append_entry")
        {
            return Ok(None);
        }
        let entries = match session.session_branch().await {
            Ok(entries) => entries,
            Err(_) => return Ok(None),
        };
        let mut coordinator = DurableCoordinator::new(DurableState::from_branch(&entries));
        if coordinator.recover_interrupted() {
            let data = state_data(coordinator.state())?;
            if session.append_custom_entry(ENTRY_TYPE, data).await.is_err() {
                return Ok(None);
            }
        }
        Ok(Some(Self::new(session, coordinator.into_state())))
    }

    /// Create an empty runtime for deterministic adapter tests.
    pub fn empty(session: Arc<HostSession>) -> Arc<Self> {
        Self::new(session, DurableState::default())
    }

    fn new(session: Arc<HostSession>, state: DurableState) -> Arc<Self> {
        let (changes, _) = tokio::sync::broadcast::channel(MAX_CHANGE_ENTRIES);
        Arc::new(Self {
            session,
            coordinator: Arc::new(Mutex::new(DurableCoordinator::new(state))),
            changes,
            history: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_CHANGE_ENTRIES))),
        })
    }

    async fn load(session: Arc<HostSession>, strict_recovery: bool) -> Result<Arc<Self>, String> {
        let entries = session.session_branch().await?;
        let mut coordinator = DurableCoordinator::new(DurableState::from_branch(&entries));
        if coordinator.recover_interrupted() {
            let data = state_data(coordinator.state())?;
            if let Err(error) = session.append_custom_entry(ENTRY_TYPE, data).await {
                if strict_recovery {
                    return Err(error);
                }
            }
        }
        Ok(Self::new(session, coordinator.into_state()))
    }

    /// Access the coordinator for compatibility with existing core callers.
    pub fn coordinator(&self) -> Arc<Mutex<DurableCoordinator>> {
        self.coordinator.clone()
    }

    pub async fn state(&self) -> DurableState {
        self.coordinator.lock().await.state().clone()
    }

    /// Subscribe before taking the replay snapshot so changes cannot be lost.
    pub async fn subscribe_changes(&self, after: Option<u64>) -> DurableSubscription {
        let receiver = self.changes.subscribe();
        let ready = self.state().await;
        let history = self.history.lock().await;
        let replay = history
            .iter()
            .filter(|change| after.map_or(true, |cursor| change.revision > cursor))
            .cloned()
            .collect::<Vec<_>>();
        let latest = ready.revision;
        let first = history.front().map(|change| change.revision);
        let gap = after.is_some_and(|cursor| {
            cursor < latest && first.map_or(true, |first| cursor.saturating_add(1) < first)
        });
        DurableSubscription {
            ready,
            replay,
            receiver,
            gap,
            after,
        }
    }

    async fn publish(&self, state: DurableState) {
        if state.revision == 0 {
            return;
        }
        let change = DurableChange {
            revision: state.revision,
            state,
        };
        {
            let mut history = self.history.lock().await;
            history.push_back(change.clone());
            while history.len() > MAX_CHANGE_ENTRIES {
                history.pop_front();
            }
        }
        let _ = self.changes.send(change);
    }

    /// Apply and persist a mutation, rolling back if the native append fails.
    pub async fn update<F, T>(&self, update: F) -> Result<T, String>
    where
        F: FnOnce(&mut DurableCoordinator) -> Result<T, DurableError>,
    {
        let mut coordinator = self.coordinator.lock().await;
        let before = coordinator.state().clone();
        let revision = before.revision;
        let value = update(&mut coordinator).map_err(|error| error.to_string())?;
        if coordinator.state().revision == revision {
            return Ok(value);
        }
        let data = state_data(coordinator.state())?;
        if let Err(error) = self.session.append_custom_entry(ENTRY_TYPE, data).await {
            *coordinator = DurableCoordinator::new(before);
            return Err(error);
        }
        let state = coordinator.state().clone();
        self.publish(state).await;
        Ok(value)
    }

    pub async fn mutate(&self, mutation: DurableMutation) -> Result<Value, String> {
        self.update(|coordinator| coordinator.apply(mutation)).await
    }

    /// Fold one native lifecycle event and persist the resulting state.
    pub async fn observe(&self, event: &Value) -> Result<bool, String> {
        let is_closed = matches!(
            event.get("type").and_then(Value::as_str),
            Some("host_closed")
        );
        let mut coordinator = self.coordinator.lock().await;
        let before = coordinator.state().clone();
        let changed = coordinator
            .observe(event)
            .map_err(|error| error.to_string())?;
        if !changed || is_closed {
            return Ok(changed);
        }
        let data = match state_data(coordinator.state()) {
            Ok(data) => data,
            Err(error) => {
                *coordinator = DurableCoordinator::new(before);
                return Err(error);
            }
        };
        if let Err(error) = self.session.append_custom_entry(ENTRY_TYPE, data).await {
            *coordinator = DurableCoordinator::new(before);
            return Err(error);
        }
        self.publish(coordinator.state().clone()).await;
        Ok(true)
    }

    /// Spawn the branch observer owned by the caller that created it.
    pub fn spawn_observer(self: &Arc<Self>) -> JoinHandle<()> {
        let runtime = self.clone();
        let mut events = self.session.subscribe_raw();
        tokio::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                let is_closed = event.get("type").and_then(Value::as_str) == Some("host_closed");
                let _ = runtime.observe(&event).await;
                if is_closed {
                    break;
                }
            }
        })
    }
}

fn state_data(state: &DurableState) -> Result<Value, String> {
    state
        .entry()
        .get("data")
        .cloned()
        .ok_or_else(|| "durable entry has no data".to_string())
}

fn trim_tools(run: &mut RunCheckpoint) {
    if run.tools.len() > MAX_TOOL_ENTRIES {
        run.tools.drain(..run.tools.len() - MAX_TOOL_ENTRIES);
    }
}
fn field_text(event: &Value, field: &str) -> String {
    match event.get(field) {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn summary(value: &str) -> String {
    let original_len = value.chars().count();
    let mut value: String = value.chars().take(MAX_SUMMARY_CHARS).collect();
    if original_len > MAX_SUMMARY_CHARS {
        value.push('…');
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_publishes_replayable_changes_after_append() {
        use futures_util::StreamExt;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let (writer, peer) = tokio::io::duplex(4096);
        let (session, injector) = HostSession::test_connection(writer, 16);
        let fixture = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let data = if request["type"] == "get_session_branch" {
                    json!({"entries": []})
                } else {
                    json!({"entryId": "entry-durable"})
                };
                injector
                    .send(json!({
                        "type": "response",
                        "id": request["id"],
                        "success": true,
                        "data": data
                    }))
                    .unwrap();
            }
        });

        let runtime = DurableRuntime::try_attach(session.clone())
            .await
            .unwrap()
            .unwrap();
        let mut subscription = runtime.subscribe_changes(None).await;
        runtime.mutate(DurableMutation::StartRun).await.unwrap();
        let changes = replay_changes(
            subscription.replay,
            subscription.receiver,
            subscription.after,
        );
        futures_util::pin_mut!(changes);
        let item = changes.next().await.unwrap();
        match item {
            DurableStreamItem::Change(change) => {
                assert_eq!(change.revision, 1);
                assert_eq!(change.state.run.unwrap().status, RunStatus::Running);
            }
            DurableStreamItem::ResyncRequired { .. } => panic!("unexpected replay gap"),
        }
        subscription = runtime.subscribe_changes(Some(1)).await;
        assert!(!subscription.gap);
        assert!(subscription.replay.is_empty());

        fixture.abort();
        session.shutdown().await;
    }

    #[tokio::test]
    async fn runtime_rolls_back_when_native_append_fails() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let (writer, peer) = tokio::io::duplex(4096);
        let (session, injector) = HostSession::test_connection(writer, 16);
        let fixture = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let response = if request["type"] == "get_session_branch" {
                    json!({
                        "type": "response",
                        "id": request["id"],
                        "success": true,
                        "data": {"entries": []}
                    })
                } else {
                    json!({
                        "type": "response",
                        "id": request["id"],
                        "success": false,
                        "error": "append failed"
                    })
                };
                injector.send(response).unwrap();
            }
        });

        let runtime = DurableRuntime::attach(session.clone()).await.unwrap();
        assert!(runtime.mutate(DurableMutation::StartRun).await.is_err());
        assert!(runtime.state().await.run.is_none());

        fixture.abort();
        session.shutdown().await;
    }

    #[tokio::test]
    async fn try_attach_returns_none_when_branch_rpc_is_unavailable() {
        let (writer, _peer) = tokio::io::duplex(4096);
        let (session, _injector) = HostSession::test_connection_with_ready(
            writer,
            16,
            json!({"type":"ready","protocol":"rpi","supportedCommands":[]}),
        );

        assert!(DurableRuntime::try_attach(session.clone())
            .await
            .unwrap()
            .is_none());
        session.shutdown().await;
    }

    #[test]
    fn mutation_application_is_shared_by_adapters() {
        let mut coordinator = DurableCoordinator::new(DurableState::default());
        let result = coordinator.apply(DurableMutation::StartRun).unwrap();
        assert_eq!(result["changed"], true);
        assert_eq!(result["run"]["status"], "running");
    }

    #[test]
    fn branch_recovery_uses_newest_valid_entry() {
        let old = json!({"type":"custom","customType":ENTRY_TYPE,"data":{"revision":1}});
        let current = json!({
            "type":"custom",
            "customType":ENTRY_TYPE,
            "data":{"revision":2}
        });
        let state = DurableState::from_branch(&[old, json!({"type":"message"}), current]);
        assert_eq!(state.revision, 2);
    }

    #[test]
    fn lifecycle_is_idempotent_and_recovery_is_safe() {
        let mut coordinator = DurableCoordinator::new(DurableState::default());
        coordinator.observe(&json!({"type":"agent_start"})).unwrap();
        coordinator.observe(&json!({"type":"tool_execution_start","toolCallId":"call-1","toolName":"shell","args":"{}"})).unwrap();
        coordinator.observe(&json!({"type":"tool_execution_end","toolCallId":"call-1","toolName":"shell","result":"ok"})).unwrap();
        assert!(!coordinator.observe(&json!({"type":"tool_execution_end","toolCallId":"call-1","toolName":"shell","result":"different","isError":true})).unwrap());
        coordinator.observe(&json!({"type":"host_closed"})).unwrap();
        assert_eq!(
            coordinator.state().run.as_ref().unwrap().status,
            RunStatus::Interrupted
        );
        assert_eq!(
            coordinator.state().run.as_ref().unwrap().tools[0].status,
            ToolStatus::Completed
        );
        coordinator.recover_run().unwrap();
        assert_eq!(
            coordinator.state().run.as_ref().unwrap().status,
            RunStatus::Queued
        );
    }

    #[test]
    fn queue_requires_claim_before_terminal_transition() {
        let mut coordinator = DurableCoordinator::new(DurableState::default());
        coordinator.enqueue("run tests").unwrap();
        assert!(coordinator
            .finish("item-1", QueueStatus::Completed, "")
            .is_err());
        coordinator.claim("item-1").unwrap();
        coordinator
            .finish("item-1", QueueStatus::Completed, "passed")
            .unwrap();
        assert!(coordinator.claim("item-1").is_err());
    }

    #[test]
    fn tool_journal_is_bounded() {
        let mut coordinator = DurableCoordinator::new(DurableState::default());
        coordinator.start_run();
        for index in 0..(MAX_TOOL_ENTRIES + 2) {
            let id = format!("call-{index}");
            coordinator.tool_started(&id, "tool", "{}").unwrap();
        }
        let tools = &coordinator.state().run.as_ref().unwrap().tools;
        assert_eq!(tools.len(), MAX_TOOL_ENTRIES);
        assert_eq!(tools[0].call_id, "call-2");
    }
}
