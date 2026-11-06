//! Native-only ACP ownership. Persisted data never contains executable commands or credentials.
use super::{LocalError, alias_name, local};
use super::{WorkspaceError, WorkspaceEventStream, unsupported};
use crate::runtime::{AcpClient, RuntimeError, RuntimeTransport};
use async_stream::try_stream;
use futures_util::StreamExt;
use rpi_node_api::AgentDescriptor;
use rpi_node_api::api::{
    AgentBackendOption, SessionAgentBinding, SessionMutation, SessionPreferences, SessionSummary,
    Snapshot, WireEvent,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{broadcast, oneshot, watch};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Profile {
    option: AgentBackendOption,
    executable: Option<PathBuf>,
    args: Vec<String>,
}

fn executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(windows)]
    {
        // npm also installs extensionless shell wrappers. They are not directly
        // executable by CreateProcess; advertise only native .exe entry points.
        path.extension()
            .and_then(|s| s.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn resolve(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    let candidates = if path.is_absolute() {
        vec![path.to_owned()]
    } else if path.components().count() == 1 {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|p| p.is_absolute())
            .map(|p| p.join(path))
            .collect()
    } else {
        return None;
    };
    #[cfg(windows)]
    {
        for candidate in candidates {
            if executable(&candidate) {
                return Some(candidate);
            }
            if candidate.extension().is_none() {
                let exe = candidate.with_extension("exe");
                if executable(&exe) {
                    return Some(exe);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        candidates
            .into_iter()
            .find(|candidate| executable(candidate))
    }
}

fn profile(id: &str) -> Result<Profile, WorkspaceError> {
    let (name, default, args) = match id {
        "claude" => ("Claude", "claude-agent-acp", vec![]),
        "codex" => ("Codex", "codex-acp", vec![]),
        "opencode" => ("OpenCode", "opencode", vec!["acp".to_owned()]),
        _ => return Err(unsupported("unknown agent backend", id)),
    };
    let prefix = format!("RPI_WORK_{}_ACP", id.to_ascii_uppercase());
    let program = std::env::var(format!("{prefix}_BIN")).unwrap_or_else(|_| default.into());
    let parsed = match std::env::var(format!("{prefix}_ARGS")) {
        Ok(value) => serde_json::from_str::<Vec<String>>(&value),
        Err(_) => Ok(args),
    };
    let (args, executable, reason) = match parsed {
        Ok(args) => {
            let found = resolve(&program);
            let reason = found.is_none().then(|| format!("Executable {program:?} not found on PATH (or not a safe native executable); set {prefix}_BIN and {prefix}_ARGS"));
            (args, found, reason)
        }
        Err(_) => (
            vec![],
            None,
            Some(format!("{prefix}_ARGS must be a JSON string array")),
        ),
    };
    Ok(Profile {
        option: AgentBackendOption {
            id: id.into(),
            name: name.into(),
            available: executable.is_some(),
            reason,
        },
        executable,
        args,
    })
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    generation: u64,
    binding: SessionAgentBinding,
    summary: SessionSummary,
    cwd: PathBuf,
    #[serde(default)]
    journal: Vec<WireEvent>,
    #[serde(default)]
    assistant: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sidecar {
    schema_version: u32,
    records: HashMap<String, Record>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SidecarRef<'a> {
    schema_version: u32,
    records: &'a HashMap<String, Record>,
}

struct Runtime {
    generation: u64,
    last_checkpoint: std::time::Instant,
    sender: broadcast::Sender<WireEvent>,
    terminal: Option<String>,
    completion: watch::Sender<u64>,
    permissions: HashMap<String, Value>,
}
struct Store {
    path: PathBuf,
    records: HashMap<String, Record>,
    runtime: HashMap<String, Runtime>,
}
impl Store {
    fn save(&self, records: &HashMap<String, Record>) -> Result<(), WorkspaceError> {
        local((|| {
            let tmp = self.path.with_extension("json.tmp");
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&serde_json::to_vec(&SidecarRef {
                schema_version: 1,
                records,
            })?)?;
            file.sync_all()?;
            std::fs::rename(tmp, &self.path)?;
            Ok(())
        })())
    }
    fn change<T>(
        &mut self,
        id: &str,
        update: impl FnOnce(&mut Record) -> T,
    ) -> Result<T, WorkspaceError> {
        let mut records = self.records.clone();
        let record = records.get_mut(id).ok_or_else(|| {
            WorkspaceError::Local(LocalError::NotFound(format!("ACP session {id}")))
        })?;
        let result = update(record);
        self.save(&records)?;
        self.records = records;
        Ok(result)
    }
    // The production LocalRuntime holds the data-directory single-writer lock.
    // This mutex serializes journal transactions, not independent runtime instances.
    fn transaction(
        &mut self,
        id: &str,
        update: impl FnOnce(&mut Record) -> Vec<WireEvent>,
    ) -> Result<(), WorkspaceError> {
        let events = self.change(id, |record| {
            let events = update(record);
            events
                .into_iter()
                .map(|mut event| {
                    let seq = record.summary.last_seq + 1;
                    event.seq = Some(seq);
                    event.id = Some(format!("{id}:{seq}"));
                    event.data["sessionId"] = json!(id);
                    record.summary.last_seq = seq;
                    record.summary.updated_at = now();
                    record.journal.push(event.clone());
                    event
                })
                .collect::<Vec<_>>()
        })?;
        if let Some(runtime) = self.runtime.get(id) {
            for event in events {
                let _ = runtime.sender.send(event);
            }
        }
        Ok(())
    }
    fn append(&mut self, id: &str, mut event: WireEvent) -> Result<(), WorkspaceError> {
        if event.event_type == "message_update" && event.data["eventType"] == "text_delta" {
            // Text deltas may arrive hundreds of times a second. Keep the live
            // journal exact, but checkpoint partial text at most four times/sec;
            // all permission/tool/turn transitions below remain immediately durable.
            let record = self.records.get_mut(id).ok_or_else(|| {
                WorkspaceError::Local(LocalError::NotFound(format!("ACP session {id}")))
            })?;
            record
                .assistant
                .push_str(event.data["delta"].as_str().unwrap_or_default());
            let seq = record.summary.last_seq + 1;
            event.seq = Some(seq);
            event.id = Some(format!("{id}:{seq}"));
            event.data["sessionId"] = json!(id);
            record.summary.last_seq = seq;
            record.summary.updated_at = now();
            record.journal.push(event.clone());
            if self.runtime.get(id).is_some_and(|runtime| {
                runtime.last_checkpoint.elapsed() >= Duration::from_millis(250)
            }) {
                self.save(&self.records)?;
                if let Some(runtime) = self.runtime.get_mut(id) {
                    runtime.last_checkpoint = std::time::Instant::now();
                }
            }
            if let Some(runtime) = self.runtime.get(id) {
                let _ = runtime.sender.send(event);
            }
            return Ok(());
        }
        self.transaction(id, |_| vec![event])
    }
    fn emit(&mut self, id: &str, kind: &str, data: Value) -> Result<(), WorkspaceError> {
        self.append(id, frame(kind, data))
    }
    fn settle(&mut self, id: &str, event: WireEvent) -> Result<(), WorkspaceError> {
        self.transaction(id, |r| {
            let mut events = Vec::new();
            if !r.assistant.is_empty() {
                events.push(frame("message_end", json!({"type":"message_end", "message":{"role":"assistant","content":std::mem::take(&mut r.assistant),"entryId":format!("{id}:assistant:{}",r.summary.last_seq+1)}})));
            }
            r.summary.status = "idle".into();
            r.summary.latest_settlement = Some(event.data.clone());
            events.push(event);
            events
        })?;
        if let Some(runtime) = self.runtime.get(id) {
            runtime.completion.send_modify(|version| *version += 1);
        }
        Ok(())
    }
    fn recover_interrupted(&mut self) -> Result<(), WorkspaceError> {
        let interrupted: Vec<_> = self
            .records
            .iter()
            .filter(|(_, r)| r.summary.status == "running" || !r.assistant.is_empty())
            .map(|(id, _)| id.clone())
            .collect();
        for id in interrupted {
            let reason = "ACP turn interrupted by workspace restart";
            // Use the normal settlement transaction so checkpointed partial text
            // becomes canonical history before prepare can clear the buffer.
            self.settle(
                &id,
                frame(
                    "agent_settled",
                    json!({"type":"agent_settled", "outcome":"failed", "error":reason}),
                ),
            )?;
            self.change(&id, |record| {
                record.summary.status = "offline".into();
                record.summary.error = Some(reason.into());
            })?;
        }
        Ok(())
    }
    fn fail(&mut self, id: &str, generation: u64, reason: &str) {
        if !self
            .runtime
            .get(id)
            .is_some_and(|r| r.generation == generation && r.terminal.is_none())
        {
            return;
        }
        if self
            .records
            .get(id)
            .is_some_and(|r| r.summary.status == "running" || !r.assistant.is_empty())
        {
            let _ = self.settle(
                id,
                WireEvent {
                    id: None,
                    seq: None,
                    event_type: "agent_settled".into(),
                    data: json!({"type":"agent_settled", "outcome":"failed", "error":reason}),
                },
            );
        }
        // Wake subscribers even when an idle process exits without a final frame.
        let _ = self.emit(
            id,
            "connection_offline",
            json!({"type":"connection_offline","error":reason}),
        );
        let _ = self.change(id, |r| {
            r.summary.status = "offline".into();
            r.summary.error = Some(reason.to_owned());
        });
        // Disk may be unavailable. Runtime truth must still reflect the reaped child.
        if let Some(record) = self.records.get_mut(id) {
            record.summary.status = "offline".into();
            record.summary.error = Some(reason.to_owned());
            record.assistant.clear();
        }
        if let Some(runtime) = self.runtime.get_mut(id) {
            runtime.terminal = Some(reason.to_owned());
            runtime.permissions.clear();
            runtime.completion.send_modify(|version| *version += 1);
            // Never depend on a successful journal write to wake subscribers.
            let _ = runtime.sender.send(frame(
                "connection_offline",
                json!({"type":"connection_offline","sessionId":id,"error":reason}),
            ));
        }
    }
}
fn frame(kind: &str, data: Value) -> WireEvent {
    WireEvent {
        id: None,
        seq: None,
        event_type: kind.into(),
        data,
    }
}
fn now() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
fn lock_store(store: &Mutex<Store>) -> Result<std::sync::MutexGuard<'_, Store>, WorkspaceError> {
    local(store.lock().map_err(|_| LocalError::Poisoned))
}

struct Live {
    client: AcpClient,
    external: String,
    generation: u64,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    prompt: Arc<tokio::sync::Mutex<()>>,
}
pub(super) struct Manager {
    store: Arc<Mutex<Store>>,
    live: Mutex<HashMap<String, Arc<Live>>>,
    start: tokio::sync::Mutex<()>,
}
impl Manager {
    pub(super) fn new(data: &Path) -> Result<Self, LocalError> {
        let path = data.join("rpc/agent-sessions.json");
        let records: HashMap<String, Record> = match std::fs::read(&path) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes)?;
                if value.get("schemaVersion").is_some() {
                    let sidecar: Sidecar = serde_json::from_value(value)?;
                    if sidecar.schema_version != 1 {
                        return Err(LocalError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Unsupported ACP sidecar schema",
                        )));
                    }
                    sidecar.records
                } else {
                    serde_json::from_value(value)?
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.into()),
        };
        let mut store = Store {
            path,
            records,
            runtime: HashMap::new(),
        };
        // Inspect persisted status before marking loaded sessions offline. Recovery
        // must be durable (or fail startup) before any snapshots are exposed.
        store.recover_interrupted().map_err(|error| match error {
            WorkspaceError::Local(error) => error,
            error @ WorkspaceError::Runtime(_) => LocalError::Initialization(error.to_string()),
        })?;
        for record in store.records.values_mut() {
            record.summary.status = "offline".into();
        }
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            live: Mutex::new(HashMap::new()),
            start: tokio::sync::Mutex::new(()),
        })
    }
    pub(super) fn contains(&self, id: &str) -> Result<bool, WorkspaceError> {
        Ok(lock_store(&self.store)?.records.contains_key(id))
    }
    pub(super) fn summaries(&self) -> Result<Vec<SessionSummary>, WorkspaceError> {
        Ok(lock_store(&self.store)?
            .records
            .values()
            .map(|r| r.summary.clone())
            .collect())
    }
    pub(super) fn summary(&self, id: &str) -> Result<SessionSummary, WorkspaceError> {
        lock_store(&self.store)?
            .records
            .get(id)
            .map(|r| r.summary.clone())
            .ok_or_else(|| WorkspaceError::Local(LocalError::NotFound(id.into())))
    }
    pub(super) fn binding(&self, id: &str) -> Result<SessionAgentBinding, WorkspaceError> {
        lock_store(&self.store)?
            .records
            .get(id)
            .map(|r| r.binding.clone())
            .ok_or_else(|| WorkspaceError::Local(LocalError::NotFound(id.into())))
    }
    pub(super) fn snapshot(&self, id: &str) -> Result<Snapshot, WorkspaceError> {
        let store = lock_store(&self.store)?;
        let record = store
            .records
            .get(id)
            .ok_or_else(|| WorkspaceError::Local(LocalError::NotFound(id.into())))?;
        let messages: Vec<_> = record
            .journal
            .iter()
            .filter(|e| e.event_type == "message_end")
            .filter_map(|e| e.data.get("message").cloned())
            .collect();
        let pending_dialogs = local(
            record
                .journal
                .iter()
                .filter(|event| event.event_type == "ui_dialog_request")
                .filter(|event| {
                    store.runtime.get(id).is_some_and(|runtime| {
                        runtime.terminal.is_none()
                            && event.data["requestId"]
                                .as_str()
                                .is_some_and(|key| runtime.permissions.contains_key(key))
                    })
                })
                .map(rpi_node_api::api::events::dialog_from_wire)
                .collect::<Result<Vec<_>, _>>()
                .map_err(LocalError::from),
        )?;
        let pending_dialogs: Vec<_> = pending_dialogs.into_iter().flatten().collect();
        let events = record
            .journal
            .iter()
            .map(|e| json!({"seq": e.seq, "event": e.data}))
            .collect::<Vec<_>>();
        local(serde_json::from_value(json!({"sessionId":id, "status":record.summary.status, "pendingDialogs":pending_dialogs, "total":messages.len(), "messages":messages, "lastSeq":record.summary.last_seq, "events":events, "latestSettlement":record.summary.latest_settlement})).map_err(LocalError::from))
    }
    pub(super) fn rename(&self, id: &str, title: &str) -> Result<SessionMutation, WorkspaceError> {
        let title = local(alias_name(title))?;
        lock_store(&self.store)?.change(id, |r| {
            r.summary.title = Some(title.clone());
            r.summary.updated_at = now();
        })?;
        Ok(SessionMutation {
            session_id: id.into(),
            source_session_id: None,
            title: Some(title),
            status: None,
        })
    }
    pub(super) fn preferences(
        &self,
        id: &str,
        archived: Option<bool>,
        pinned: Option<bool>,
    ) -> Result<SessionPreferences, WorkspaceError> {
        lock_store(&self.store)?.change(id, |r| {
            if let Some(value) = archived {
                r.summary.archived = value;
            }
            if let Some(value) = pinned {
                r.summary.pinned = value;
            }
            SessionPreferences {
                session_id: id.into(),
                archived: r.summary.archived,
                pinned: r.summary.pinned,
            }
        })
    }
    pub(super) fn backends() -> Result<Vec<AgentBackendOption>, WorkspaceError> {
        let mut options = vec![AgentBackendOption {
            id: "rpi".into(),
            name: "RPI".into(),
            available: true,
            reason: None,
        }];
        for id in ["claude", "codex", "opencode"] {
            options.push(profile(id)?.option);
        }
        Ok(options)
    }
    pub(super) async fn create(
        &self,
        project: Option<String>,
        cwd: PathBuf,
        backend: &str,
    ) -> Result<String, WorkspaceError> {
        // No RPI registry entry or placeholder is created for ACP sessions.
        let id = format!(
            "acp-{}-{}-{}",
            std::process::id(),
            now(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let record = Record {
            generation: 0,
            binding: SessionAgentBinding {
                backend: backend.into(),
                external_session_id: None,
            },
            cwd,
            summary: SessionSummary {
                session_id: id.clone(),
                project_id: project,
                title: None,
                status: "offline".into(),
                created_at: now(),
                updated_at: now(),
                last_seq: 0,
                error: None,
                pending_resume: false,
                latest_settlement: None,
                archived: false,
                pinned: false,
            },
            journal: vec![],
            assistant: String::new(),
        };
        profile(backend)?;
        {
            let mut store = lock_store(&self.store)?;
            let mut records = store.records.clone();
            records.insert(id.clone(), record);
            store.save(&records)?;
            store.records = records;
        }
        let mut rollback = CreationGuard {
            store: self.store.clone(),
            id: Some(id.clone()),
        };
        if let Err(error) = self.prepare(&id, true).await {
            let mut store = lock_store(&self.store)?;
            let mut records = store.records.clone();
            records.remove(&id);
            store.save(&records)?;
            store.records = records;
            return Err(error);
        }
        rollback.id = None;
        Ok(id)
    }
    fn mapped_client(&self, id: &str, live: Arc<Live>) -> PreparedAgent {
        PreparedAgent {
            id: id.into(),
            live,
            store: self.store.clone(),
        }
    }
    pub(super) async fn prepare(
        &self,
        id: &str,
        new: bool,
    ) -> Result<PreparedAgent, WorkspaceError> {
        let _start = self.start.lock().await;
        let existing = local(self.live.lock().map_err(|_| LocalError::Poisoned))?
            .get(id)
            .cloned();
        if let Some(live) = existing {
            if lock_store(&self.store)?
                .runtime
                .get(id)
                .is_some_and(|r| r.terminal.is_none())
            {
                return Ok(self.mapped_client(id, live));
            }
            live.client.stop().await?;
            if let Some(task) = live.task.lock().await.take() {
                let _ = task.await;
            }
            local(self.live.lock().map_err(|_| LocalError::Poisoned))?.remove(id);
        }
        let record = lock_store(&self.store)?
            .records
            .get(id)
            .cloned()
            .ok_or_else(|| WorkspaceError::Local(LocalError::NotFound(id.into())))?;
        let client = spawn_client(&record).await?;
        let stream: WorkspaceEventStream = Box::pin(
            client
                .subscribe_events()?
                .map(|event| event.map_err(WorkspaceError::from)),
        ); // MUST precede new/load, including synchronous replay notifications.
        // Persist before accepting permission cards: process restarts must never reuse IDs.
        let generation = lock_store(&self.store)?.change(id, |r| {
            r.generation = r.generation.saturating_add(1);
            r.generation
        })?;
        let (sender, _) = broadcast::channel(4096);
        lock_store(&self.store)?.runtime.insert(
            id.into(),
            Runtime {
                generation,
                last_checkpoint: std::time::Instant::now(),
                sender,
                terminal: None,
                completion: watch::channel(0).0,
                permissions: HashMap::new(),
            },
        );
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(consume(
            stream,
            rx,
            self.store.clone(),
            id.into(),
            generation,
            client.clone(),
            !new,
        ));
        let mut cleanup = PreparationGuard {
            client: Some(client.clone()),
            task: task.abort_handle(),
        };
        let result = restore_session(&client, &record, id, new).await;
        let external = match result {
            Ok(external) => external,
            Err(error) => {
                drop(tx);
                let _ = client.stop().await;
                let _ = task.await;
                return Err(error);
            }
        };
        let saved = lock_store(&self.store)?.change(id, |r| {
            r.binding.external_session_id = Some(external.clone());
            r.summary.status = "idle".into();
            r.summary.error = None;
            r.assistant.clear();
        });
        if let Err(error) = saved {
            drop(tx);
            let _ = client.stop().await;
            let _ = task.await;
            return Err(error);
        }
        // Persisted history is canonical: load replay is not a new assistant turn.
        close_historical_permissions(&mut *lock_store(&self.store)?, id)?;
        let (ready, ready_rx) = oneshot::channel();
        let _ = tx.send((external.clone(), ready));
        ready_rx.await.map_err(|_| {
            WorkspaceError::Runtime(RuntimeError::Acp("ACP restore stream failed".into()))
        })?;
        cleanup.client = None;
        let live = Arc::new(Live {
            client,
            external,
            generation,
            task: tokio::sync::Mutex::new(Some(task)),
            prompt: Arc::new(tokio::sync::Mutex::new(())),
        });
        local(self.live.lock().map_err(|_| LocalError::Poisoned))?.insert(id.into(), live.clone());
        Ok(self.mapped_client(id, live))
    }
    pub(super) async fn delete(&self, id: &str) -> Result<(), WorkspaceError> {
        let _start = self.start.lock().await;
        let live = local(self.live.lock().map_err(|_| LocalError::Poisoned))?.remove(id);
        if let Some(live) = live {
            live.client.stop().await?;
            if let Some(task) = live.task.lock().await.take() {
                let _ = task.await;
            }
        }
        let mut store = lock_store(&self.store)?;
        let mut records = store.records.clone();
        records.remove(id);
        store.save(&records)?;
        store.records = records;
        store.runtime.remove(id);
        Ok(())
    }
    pub(super) async fn shutdown(&self) {
        let _start = self.start.lock().await;
        let lives: Vec<_> = self
            .live
            .lock()
            .map(|mut l| l.drain().map(|(_, l)| l).collect())
            .unwrap_or_default();
        for live in lives {
            let _ = live.client.stop().await;
            if let Some(task) = live.task.lock().await.take() {
                let _ = task.await;
            }
        }
    }
}

async fn spawn_client(record: &Record) -> Result<AcpClient, WorkspaceError> {
    let profile = profile(&record.binding.backend)?;
    let executable = profile.executable.ok_or_else(|| {
        WorkspaceError::Runtime(RuntimeError::Acp(
            profile
                .option
                .reason
                .unwrap_or_else(|| "Agent unavailable".into()),
        ))
    })?;
    AcpClient::spawn(executable, &profile.args, &record.cwd)
        .await
        .map_err(WorkspaceError::from)
}

async fn restore_session(
    client: &AcpClient,
    record: &Record,
    id: &str,
    new: bool,
) -> Result<String, WorkspaceError> {
    let cwd = record.cwd.to_string_lossy();
    if new {
        client
            .new_session(cwd.to_string())
            .await
            .map_err(WorkspaceError::from)
    } else if let Some(external) = record.binding.external_session_id.as_deref() {
        let descriptor = client.descriptor();
        let response = if descriptor.capabilities.session_load {
            client
                .load_session(external, &cwd)
                .await
                .map_err(WorkspaceError::from)
        } else if descriptor.capabilities.session_resume {
            client
                .resume_session(external, &cwd)
                .await
                .map_err(WorkspaceError::from)
        } else {
            Err(WorkspaceError::Runtime(RuntimeError::Acp(format!(
                "Cannot restore {id}: backend does not support session/load or session/resume"
            ))))
        };
        response.map(|v| v["sessionId"].as_str().unwrap_or(external).to_owned())
    } else {
        Err(WorkspaceError::Runtime(RuntimeError::Acp(format!(
            "Cannot restore {id}: missing external session ID"
        ))))
    }
}

async fn consume(
    stream: WorkspaceEventStream,
    binding: oneshot::Receiver<(String, oneshot::Sender<()>)>,
    store: Arc<Mutex<Store>>,
    id: String,
    generation: u64,
    client: AcpClient,
    restoring: bool,
) {
    let reason = consume_events(
        stream,
        binding,
        store.clone(),
        id.clone(),
        generation,
        restoring,
        &client,
    )
    .await;
    let _ = client.stop().await;
    if let Ok(mut store) = store.lock() {
        store.fail(&id, generation, &reason);
    }
}

async fn consume_events(
    mut stream: WorkspaceEventStream,
    mut binding: oneshot::Receiver<(String, oneshot::Sender<()>)>,
    store: Arc<Mutex<Store>>,
    id: String,
    generation: u64,
    restoring: bool,
    client: &AcpClient,
) -> String {
    let mut buffered = Vec::new();
    let mut replaying = restoring;
    let (external, ready) = loop {
        tokio::select! {
            result = &mut binding => match result { Ok(id) => break id, Err(_) => return "ACP initialization cancelled".into() },
            event = stream.next() => match event {
                Some(Ok(event)) => {
                    if replaying {
                        if let Err(error) = suppress_replay_event(client, &event, &mut replaying).await {
                            return error.to_string();
                        }
                    } else { buffered.push(event); }
                }
                Some(Err(error)) => return error.to_string(),
                None => return "ACP stream closed during initialization".into(),
            }
        }
    };
    // The reader emits this explicit marker at the load/resume response, in wire
    // order. Pending/cooperative yielding is NOT evidence that replay is drained.
    while replaying {
        match stream.next().await {
            Some(Ok(event)) => {
                if let Err(error) = suppress_replay_event(client, &event, &mut replaying).await {
                    return error.to_string();
                }
            }
            Some(Err(error)) => return error.to_string(),
            None => return "ACP stream closed during restore".into(),
        }
    }
    let _ = ready.send(());
    let mut events = futures_util::stream::iter(buffered.into_iter().map(Ok)).chain(stream);
    while let Some(event) = events.next().await {
        let result = match event {
            Ok(mut event) => {
                if event.data.get("sessionId").and_then(Value::as_str) != Some(&external)
                    && !(event.event_type == "ui_dialog_closed"
                        && event.data.get("sessionId").is_none())
                {
                    continue;
                }
                if event.data["eventType"] == "thinking_delta"
                    || event.event_type.contains("thought")
                {
                    continue;
                }
                let mut store = match lock_store(&store) {
                    Ok(store) => store,
                    Err(error) => return error.to_string(),
                };
                if !store
                    .runtime
                    .get(&id)
                    .is_some_and(|r| r.generation == generation && r.terminal.is_none())
                {
                    return "ACP generation replaced".into();
                }
                if !map_permission(&mut store, &id, generation, &mut event) {
                    continue;
                }
                event.data["sessionId"] = json!(id);
                if event.event_type == "agent_settled" {
                    store.settle(&id, event)
                } else {
                    store.append(&id, event)
                }
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            return error.to_string();
        }
    }
    "ACP event stream closed".into()
}

fn journal_terminal(
    state: &Mutex<Store>,
    id: &str,
    generation: u64,
) -> Result<Option<String>, WorkspaceError> {
    let store = lock_store(state)?;
    Ok(match store.runtime.get(id) {
        Some(r) if r.generation == generation => r.terminal.clone(),
        _ => Some("ACP session replaced or deleted".into()),
    })
}

/// Bound access to a prepared ACP session owned by this workspace.
///
/// The handle shares live runtime/journal state; it is not a client projector or
/// a general raw-command bypass. Every operation retains the original binding
/// and generation checks. Authentication remains the caller adapter's job.
#[derive(Clone)]
pub struct PreparedAgent {
    id: String,
    live: Arc<Live>,
    store: Arc<Mutex<Store>>,
}
impl PreparedAgent {
    fn validate(&self, id: &str) -> Result<(), WorkspaceError> {
        if id != self.id {
            return Err(WorkspaceError::Runtime(RuntimeError::Acp(
                "Cross-session ACP operation rejected".into(),
            )));
        }
        let store = lock_store(&self.store)?;
        match store.runtime.get(id) {
            Some(r) if r.generation == self.live.generation && r.terminal.is_none() => Ok(()),
            _ => Err(WorkspaceError::Runtime(RuntimeError::ChildExited(
                "Stale or stopped ACP session".into(),
            ))),
        }
    }
    fn stream(&self, after: u64) -> Result<WorkspaceEventStream, WorkspaceError> {
        self.validate(&self.id)?;
        // Append and subscribe share one mutex: no replay/live handoff gap.
        let store = lock_store(&self.store)?;
        let replay: Vec<_> = store.records[&self.id]
            .journal
            .iter()
            .filter(|e| e.seq.is_some_and(|s| s > after))
            .cloned()
            .collect();
        let mut receiver = store.runtime[&self.id].sender.subscribe();
        let state = self.store.clone();
        let id = self.id.clone();
        let generation = self.live.generation;
        let stream = try_stream! {
            for event in replay { yield event; }
            loop {
                let terminal = journal_terminal(&state, &id, generation)?;
                if let Some(reason) = terminal {
                    while let Ok(event) = receiver.try_recv() { yield event; }
                    Err(WorkspaceError::Runtime(RuntimeError::ChildExited(reason)))?;
                }
                match receiver.recv().await {
                    Ok(event) => yield event,
                    Err(error) => Err(WorkspaceError::Runtime(RuntimeError::Acp(format!("ACP journal subscriber: {error}"))))?,
                }
            }
        };
        Ok(Box::pin(stream))
    }
}
impl PreparedAgent {
    #[must_use]
    pub fn descriptor(&self) -> AgentDescriptor {
        self.live.client.descriptor()
    }
    #[must_use]
    pub fn supports_command(&self, command: &str) -> bool {
        matches!(command, "prompt" | "abort" | "cancel")
    }
    #[must_use]
    pub fn supports_config_option(&self, id: &str, config: &str) -> bool {
        self.validate(id).is_ok()
            && RuntimeTransport::supports_config_option(
                &self.live.client,
                &self.live.external,
                config,
            )
    }
    #[must_use]
    pub fn session_config_options(&self, id: &str) -> Vec<Value> {
        if self.validate(id).is_ok() {
            self.live.client.session_config_options(&self.live.external)
        } else {
            vec![]
        }
    }
    // Keep validation lazy: constructing an operation must not inspect state yet.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn open_session(&self, _cwd: &str) -> Result<String, WorkspaceError> {
        self.validate(&self.id)?;
        Ok(self.id.clone())
    }
    // Keep validation lazy: constructing an operation must not inspect state yet.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn load_session(&self, id: &str, _cwd: &str) -> Result<Value, WorkspaceError> {
        self.validate(id)?;
        Ok(json!({"sessionId":self.id}))
    }
    pub async fn resume_session(&self, id: &str, cwd: &str) -> Result<Value, WorkspaceError> {
        self.load_session(id, cwd).await
    }
    pub async fn stop(&self) -> Result<(), WorkspaceError> {
        self.validate(&self.id)?;
        self.live.client.stop().await.map_err(WorkspaceError::from)
    }
    /// Read the current owner journal, never response metadata supplied by a client.
    pub(super) fn pending_dialog(
        &self,
        request_id: &str,
    ) -> Result<rpi_node_api::api::PendingDialog, WorkspaceError> {
        let store = lock_store(&self.store)?;
        let invalid = || {
            WorkspaceError::Local(LocalError::InvalidInput(
                "stale or foreign ACP dialog request".into(),
            ))
        };
        let runtime = store
            .runtime
            .get(&self.id)
            .filter(|runtime| {
                runtime.generation == self.live.generation && runtime.terminal.is_none()
            })
            .ok_or_else(invalid)?;
        if !runtime.permissions.contains_key(request_id) {
            return Err(invalid());
        }
        let event = store
            .records
            .get(&self.id)
            .ok_or_else(invalid)?
            .journal
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.event_type.as_str(),
                    "ui_dialog_request" | "ui_dialog_closed"
                ) && event.data["requestId"].as_str() == Some(request_id)
            })
            .filter(|event| event.event_type == "ui_dialog_request")
            .ok_or_else(invalid)?;
        let dialog = rpi_node_api::api::events::dialog_from_wire(event)
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        if dialog.response_id.as_ref().and_then(Value::as_str) != Some(request_id) {
            return Err(invalid());
        }
        Ok(dialog)
    }
    pub async fn respond_to_agent_request(
        &self,
        request_id: Value,
        result: Value,
    ) -> Result<(), WorkspaceError> {
        self.validate(&self.id)?;
        let original = {
            let store = lock_store(&self.store)?;
            request_id
                .as_str()
                .and_then(|key| {
                    store
                        .runtime
                        .get(&self.id)
                        .filter(|runtime| {
                            runtime.generation == self.live.generation && runtime.terminal.is_none()
                        })
                        .and_then(|runtime| runtime.permissions.get(key))
                })
                .cloned()
                .ok_or_else(|| {
                    WorkspaceError::Runtime(RuntimeError::Acp(
                        "Stale or foreign ACP permission".into(),
                    ))
                })?
        };
        self.live
            .client
            .respond_to_agent_request(original, result)
            .await
            .map_err(WorkspaceError::from)
    }
    pub async fn set_config_option(
        &self,
        id: &str,
        config: &str,
        value: Value,
    ) -> Result<Value, WorkspaceError> {
        self.validate(id)?;
        self.live
            .client
            .set_config_option(&self.live.external, config, value)
            .await
            .map_err(WorkspaceError::from)
    }
    pub async fn request(
        &self,
        id: &str,
        command: &str,
        payload: Value,
    ) -> Result<Value, WorkspaceError> {
        self.validate(id)?;
        match command {
            "prompt" => {
                let guard = self.live.prompt.clone().try_lock_owned().map_err(|_| {
                    WorkspaceError::Runtime(RuntimeError::Acp(
                        "ACP session already has an active prompt".into(),
                    ))
                })?;
                let mut completion = {
                    let mut store = lock_store(&self.store)?;
                    if store.records[id].summary.status == "running" {
                        return Err(WorkspaceError::Runtime(RuntimeError::Acp(
                            "ACP session still has an active turn".into(),
                        )));
                    }
                    let completion = store.runtime[id].completion.subscribe();
                    store.transaction(id, |r| {
                        r.summary.status = "running".into();
                        r.assistant.clear();
                        vec![frame("agent_start", json!({"type":"agent_start"})),
                            frame("message_end", json!({"type":"message_end","message":{"role":"user","content":payload["message"],"entryId":format!("{id}:user:{}",r.summary.last_seq+1)}}))]
                    })?;
                    completion
                };
                let live = self.live.clone();
                let store = self.store.clone();
                let id = self.id.clone();
                let message = payload["message"].as_str().unwrap_or_default().to_owned();
                // Own the concrete Send future, never the UI's ?Send transport future.
                let task = tokio::spawn(async move {
                    let _guard = guard;
                    let result = live
                        .client
                        .prompt(&live.external, &message)
                        .await
                        .map_err(WorkspaceError::from);
                    match result {
                        Ok(value) => {
                            completion.changed().await.map_err(|_| {
                                WorkspaceError::Runtime(RuntimeError::ChildExited(
                                    "ACP completion closed".into(),
                                ))
                            })?;
                            if let Some(reason) = journal_terminal(&store, &id, live.generation)? {
                                return Err(WorkspaceError::Runtime(RuntimeError::ChildExited(
                                    reason,
                                )));
                            }
                            Ok(value)
                        }
                        Err(error) => {
                            let _ = live.client.stop().await;
                            lock_store(&store)?.fail(&id, live.generation, &error.to_string());
                            Err(error)
                        }
                    }
                });
                task.await.map_err(|error| {
                    WorkspaceError::Runtime(RuntimeError::Process(error.to_string()))
                })?
            }
            "abort" | "cancel" => self
                .live
                .client
                .cancel(&self.live.external)
                .await
                .map_err(WorkspaceError::from),
            _ => Err(unsupported(command, "acp")),
        }
    }
    // Keep validation lazy: constructing an operation must not inspect state yet.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn subscribe(
        &self,
        id: &str,
        after: u64,
    ) -> Result<WorkspaceEventStream, WorkspaceError> {
        self.validate(id)?;
        self.stream(after)
    }
}

struct CreationGuard {
    store: Arc<Mutex<Store>>,
    id: Option<String>,
}
impl Drop for CreationGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            if let Ok(mut store) = self.store.lock() {
                let mut records = store.records.clone();
                records.remove(&id);
                if store.save(&records).is_ok() {
                    store.records = records;
                }
                store.runtime.remove(&id);
            }
        }
    }
}
struct PreparationGuard {
    client: Option<AcpClient>,
    task: tokio::task::AbortHandle,
}
impl Drop for PreparationGuard {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            self.task.abort();
            tokio::spawn(async move {
                let _ = client.stop().await;
            });
        }
    }
}

fn close_historical_permissions(store: &mut Store, id: &str) -> Result<(), WorkspaceError> {
    let mut open = std::collections::HashSet::new();
    for event in &store.records[id].journal {
        if let Some(key) = event.data["requestId"].as_str() {
            if event.event_type == "ui_dialog_request" {
                open.insert(key.to_owned());
            }
            if event.event_type == "ui_dialog_closed" {
                open.remove(key);
            }
        }
    }
    store.transaction(id, |_| {
        open.into_iter()
            .map(|key| {
                frame(
                    "ui_dialog_closed",
                    json!({"type":"ui_dialog_closed","requestId":key}),
                )
            })
            .collect()
    })
}

fn map_permission(store: &mut Store, id: &str, generation: u64, event: &mut WireEvent) -> bool {
    let runtime = store.runtime.get_mut(id).expect("validated runtime");
    if event.event_type == "ui_dialog_request" {
        let original = event.data["jsonRpcRequestId"].clone();
        let key = format!(
            "{id}:{generation}:permission:{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        runtime.permissions.insert(key.clone(), original);
        event.data["requestId"] = json!(key);
        event.data["jsonRpcRequestId"] = json!(key);
    } else if event.event_type == "ui_dialog_closed" {
        let original_id = &event.data["jsonRpcRequestId"];
        let key = runtime
            .permissions
            .iter()
            .find_map(|(key, original)| (original == original_id).then(|| key.clone()));
        let Some(key) = key else {
            return false;
        };
        runtime.permissions.remove(&key);
        event.data["requestId"] = json!(key);
    }
    true
}

async fn suppress_replay_event(
    client: &AcpClient,
    event: &WireEvent,
    replaying: &mut bool,
) -> Result<(), WorkspaceError> {
    if event.event_type == "acp_session_restored" {
        *replaying = false;
    } else {
        // Resolve permission requests even before load returns, otherwise a
        // backend waiting for permission and the loader would wait on each other.
        cancel_restored_permission(client, event).await?;
    }
    Ok(())
}

async fn cancel_restored_permission(
    client: &AcpClient,
    event: &WireEvent,
) -> Result<(), WorkspaceError> {
    if event.event_type == "ui_dialog_request" {
        client
            .respond_to_agent_request(
                event.data["jsonRpcRequestId"].clone(),
                json!({"outcome":{"outcome":"cancelled"}}),
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rpi-acp-recovery-{}-{}-{}",
                std::process::id(),
                now(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(path.join("rpc")).unwrap();
            Self(path)
        }

        fn write_record(&self, status: &str, assistant: &str) {
            // Construct persisted records directly: no profile lookup, environment
            // overrides, adapter process, or model is needed for startup recovery.
            let record: Record = serde_json::from_value(json!({
                "binding": {"backend":"codex"},
                "cwd": self.0,
                "summary": {
                    "sessionId":"test", "projectId":null, "title":null,
                    "status":status, "createdAt":1, "updatedAt":1, "lastSeq":0
                },
                "assistant": assistant
            }))
            .unwrap();
            let records = HashMap::from([("test".into(), record)]);
            std::fs::write(
                self.0.join("rpc/agent-sessions.json"),
                serde_json::to_vec(&SidecarRef {
                    schema_version: 1,
                    records: &records,
                })
                .unwrap(),
            )
            .unwrap();
        }

        fn persisted(&self) -> Record {
            let sidecar: Sidecar = serde_json::from_slice(
                &std::fs::read(self.0.join("rpc/agent-sessions.json")).unwrap(),
            )
            .unwrap();
            sidecar.records["test"].clone()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn startup_settles_checkpointed_partial_once() {
        // Nonempty assistant buffers also indicate interruption if status was
        // already changed or came from an older sidecar.
        for status in ["running", "idle", "offline"] {
            let fixture = Fixture::new();
            fixture.write_record(status, "checkpointed partial output");
            let manager = Manager::new(&fixture.0).unwrap();
            let snapshot = manager.snapshot("test").unwrap();
            assert_eq!(snapshot.total, 1);
            assert_eq!(snapshot.messages[0]["role"], "assistant");
            assert_eq!(
                snapshot.messages[0]["content"],
                "checkpointed partial output"
            );
            assert_eq!(snapshot.last_seq, 2);
            let settlement = snapshot.latest_settlement.as_ref().unwrap();
            assert_eq!(settlement["outcome"], "failed");
            assert!(
                settlement["error"]
                    .as_str()
                    .unwrap()
                    .contains("interrupted")
            );
            assert_eq!(snapshot.events[0].event["type"], "message_end");
            assert_eq!(snapshot.events[1].event["type"], "agent_settled");
            let recovered = fixture.persisted();
            assert_eq!(recovered.assistant, "");
            assert_eq!(recovered.summary.status, "offline");
            assert_eq!(
                recovered.summary.error.as_deref(),
                settlement["error"].as_str()
            );
            assert_eq!(manager.summary("test").unwrap(), recovered.summary);
            drop(manager);

            let restarted = Manager::new(&fixture.0).unwrap();
            let second = restarted.snapshot("test").unwrap();
            assert_eq!(second.messages, snapshot.messages);
            assert_eq!(second.total, snapshot.total);
            assert_eq!(second.last_seq, snapshot.last_seq);
            assert_eq!(second.events, snapshot.events);
            assert_eq!(second.latest_settlement, snapshot.latest_settlement);
            assert_eq!(restarted.summary("test").unwrap().status, "offline");
            assert_eq!(fixture.persisted().journal.len(), 2);
        }
    }

    #[test]
    fn startup_settles_running_turn_without_text_as_failed() {
        let fixture = Fixture::new();
        fixture.write_record("running", "");
        let manager = Manager::new(&fixture.0).unwrap();
        let snapshot = manager.snapshot("test").unwrap();
        assert_eq!(snapshot.messages, Vec::<Value>::new());
        assert_eq!(snapshot.last_seq, 1);
        assert_eq!(snapshot.latest_settlement.unwrap()["outcome"], "failed");
        assert_eq!(fixture.persisted().summary.status, "offline");
    }

    #[test]
    fn startup_keeps_idle_and_offline_records_unsettled() {
        for status in ["idle", "offline"] {
            let fixture = Fixture::new();
            fixture.write_record(status, "");
            // Ordinary startup must not require a write or invent a failed turn.
            std::fs::create_dir(fixture.0.join("rpc/agent-sessions.json.tmp")).unwrap();
            let manager = Manager::new(&fixture.0).unwrap();
            let snapshot = manager.snapshot("test").unwrap();
            assert_eq!(snapshot.messages, Vec::<Value>::new());
            assert_eq!(
                snapshot.events,
                Vec::<rpi_node_api::api::SnapshotEvent>::new()
            );
            assert!(snapshot.latest_settlement.is_none());
            assert_eq!(snapshot.last_seq, 0);
            let summary = manager.summary("test").unwrap();
            assert_eq!(summary.status, "offline");
            assert!(summary.error.is_none());
            assert_eq!(fixture.persisted().summary.status, status);
        }
    }

    #[test]
    fn startup_propagates_recovery_write_error_and_can_retry() {
        let fixture = Fixture::new();
        fixture.write_record("running", "partial");
        let blocked = fixture.0.join("rpc/agent-sessions.json.tmp");
        std::fs::create_dir(&blocked).unwrap();
        assert!(matches!(Manager::new(&fixture.0), Err(LocalError::Io(_))));
        let original = fixture.persisted();
        assert_eq!(original.assistant, "partial");
        assert_eq!(original.summary.status, "running");
        assert_eq!(original.journal, Vec::<WireEvent>::new());
        std::fs::remove_dir(blocked).unwrap();
        let manager = Manager::new(&fixture.0).unwrap();
        assert_eq!(manager.snapshot("test").unwrap().messages.len(), 1);
        assert_eq!(manager.summary("test").unwrap().status, "offline");
    }

    #[cfg(windows)]
    #[test]
    fn windows_executable_accepts_only_native_exe_files() {
        let fixture = Fixture::new();
        for (name, expected) in [
            ("adapter.exe", true),
            ("adapter.EXE", true),
            ("adapter.cmd", false),
            ("adapter.bat", false),
            ("adapter.ps1", false),
            ("adapter.js", false),
            ("npm", false),
        ] {
            let path = fixture.0.join(name);
            std::fs::write(&path, "npm wrapper fixture").unwrap();
            assert_eq!(executable(&path), expected, "{name}");
        }
        let directory = fixture.0.join("directory.exe");
        std::fs::create_dir(&directory).unwrap();
        assert!(!executable(&directory));
        assert!(!executable(&fixture.0.join("missing.exe")));
    }
}
