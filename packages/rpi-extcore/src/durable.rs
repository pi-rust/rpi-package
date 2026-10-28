//! Protocol-neutral durable projection semantics.
//!
//! The pure state types remain available to protocol adapters, but this module
//! is no longer a durable writer. `rpi-durable` owns the canonical
//! `durable-state`/`rpi-durable-journal` entries; this crate only replays them,
//! forwards mutations through the registered extension command, and publishes
//! an in-memory projection.
use crate::host::HostSession;
use futures_util::{stream, Stream, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Canonical native custom-entry discriminator owned by `rpi-durable`.
pub const ENTRY_TYPE: &str = rpi_durable_core::ENTRY_TYPE;
/// Legacy discriminator retained for ACP event compatibility; durable replay
/// itself only reads the canonical entry type.
pub const LEGACY_ENTRY_TYPE: &str = rpi_durable_core::LEGACY_ENTRY_TYPE;
/// Maximum number of durable state changes retained for reconnect replay.
pub const MAX_CHANGE_ENTRIES: usize = 128;

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
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    ))
}

pub use rpi_durable_core::{
    DurableMutation, DurableState, QueueItem, QueueStatus, RunCheckpoint, RunStatus,
    ToolCheckpoint, ToolStatus, MAX_QUEUE_ITEMS, MAX_SUMMARY_CHARS, MAX_TOOL_ENTRIES,
};

/// Branch-backed durable projection shared by protocol adapters.

///
/// The runtime never appends a durable entry. Mutations are sent to the
/// registered `rpi-durable` command and state is reloaded from the native
/// branch after the command commits.
pub struct DurableRuntime {
    session: Arc<HostSession>,
    projection: Arc<Mutex<DurableState>>,
    refresh_lock: Mutex<()>,
    changes: tokio::sync::broadcast::Sender<DurableChange>,
    history: Arc<Mutex<VecDeque<DurableChange>>>,
}

impl DurableRuntime {
    /// Load the canonical projection from the native branch. Interrupted-run
    /// recovery is owned by the native harness or `rpi-durable`, never here.
    pub async fn attach(session: Arc<HostSession>) -> Result<Arc<Self>, String> {
        Self::load(session, true).await
    }

    /// Load a projection when the host exposes branch reads and extension
    /// commands. Ordinary sessions remain usable when either capability is absent.
    pub async fn try_attach(session: Arc<HostSession>) -> Result<Option<Arc<Self>>, String> {
        if !session.supports_command("get_session_branch")
            || !session.supports_command("execute_command")
        {
            return Ok(None);
        }
        let (entries, _filtered) = match session.branch_entries_of_type(ENTRY_TYPE).await {
            Ok(reply) => reply,
            Err(_) => return Ok(None),
        };
        let runtime = Self::new(
            session,
            DurableState::try_from_branch(&entries)
                .map_err(|error| format!("durable branch replay: {error}"))?,
        );
        Ok(Some(runtime))
    }

    /// Create an empty runtime for deterministic adapter tests.
    pub fn empty(session: Arc<HostSession>) -> Arc<Self> {
        Self::new(session, DurableState::default())
    }

    fn new(session: Arc<HostSession>, state: DurableState) -> Arc<Self> {
        let (changes, _) = tokio::sync::broadcast::channel(MAX_CHANGE_ENTRIES);
        Arc::new(Self {
            session,
            projection: Arc::new(Mutex::new(state)),
            refresh_lock: Mutex::new(()),
            changes,
            history: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_CHANGE_ENTRIES))),
        })
    }

    async fn load(session: Arc<HostSession>, _strict_recovery: bool) -> Result<Arc<Self>, String> {
        let (entries, _filtered) = session.branch_entries_of_type(ENTRY_TYPE).await?;
        let state = DurableState::try_from_branch(&entries)?;
        Ok(Self::new(session, state))
    }

    pub async fn state(&self) -> DurableState {
        self.projection.lock().await.clone()
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

    /// Forward a business mutation to `rpi-durable`, the only durable writer.
    pub async fn mutate(&self, mutation: DurableMutation) -> Result<Value, String> {
        let command = mutation_command(&mutation);
        let result = self
            .session
            .execute_extension_command("durable", command)
            .await?;
        if let Some(error) = result.get("error").and_then(Value::as_str).or_else(|| {
            result
                .get("details")
                .and_then(|details| details.get("error"))
                .and_then(Value::as_str)
        }) {
            return Err(error.to_string());
        }
        self.refresh_branch().await?;
        Ok(result)
    }

    pub async fn refresh_branch(&self) -> Result<bool, String> {
        let _refresh = self.refresh_lock.lock().await;
        let (entries, _filtered) = self.session.branch_entries_of_type(ENTRY_TYPE).await?;
        let next = DurableState::try_from_branch(&entries)?;
        let mut projection = self.projection.lock().await;
        if *projection == next {
            return Ok(false);
        }
        *projection = next.clone();
        drop(projection);
        self.publish(next).await;
        Ok(true)
    }
}

fn mutation_command(mutation: &DurableMutation) -> Value {
    match mutation {
        DurableMutation::StartRun => json!({"action": "start_run"}),
        DurableMutation::SettleRun => json!({"action": "settle_run"}),
        DurableMutation::RecoverRun => json!({"action": "recover_run"}),
        DurableMutation::Enqueue { text } => json!({"action": "enqueue", "text": text}),
        DurableMutation::Claim { id } => json!({"action": "claim", "id": id}),

        DurableMutation::Finish { id, status, note } => {
            json!({"action": "finish", "id": id, "status": status, "note": note})
        }
        DurableMutation::Cancel { id, note } => {
            json!({"action": "cancel", "id": id, "note": note})
        }
    }
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
            let mut started = false;
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let data = match request["type"].as_str() {
                    Some("get_session_branch") if started => json!({
                        "filtered": true,
                        "entries": [{"seq":7,"type":"custom","customType":ENTRY_TYPE,"data":{
                            "revision":1,"nextRunId":1,
                            "run":{"id":"run-1","status":"running","tools":[]},
                            "queue":[]
                        }}]
                    }),
                    Some("get_session_branch") => json!({"filtered":true,"entries":[]}),
                    Some("execute_command") => {
                        started = true;
                        json!({"changed":true,"run":{"id":"run-1","status":"running","tools":[]}})
                    }
                    _ => json!({"entryId": "entry-durable"}),
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
        assert!(!runtime.refresh_branch().await.unwrap());
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
}
