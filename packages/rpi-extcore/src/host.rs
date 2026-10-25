use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{broadcast, mpsc, oneshot, Mutex, RwLock},
};

/// How many projected events a session keeps so a page reload can rebuild the
/// transcript without asking the native host for history.
const HISTORY_LIMIT: usize = 512;

async fn read_ready(
    reader: &mut (impl AsyncBufRead + Unpin),
    timeout: Duration,
) -> Result<Value, String> {
    let mut first_line = String::new();
    tokio::time::timeout(timeout, reader.take(1024 * 1024).read_line(&mut first_line))
        .await
        .map_err(|_| "native rpi host startup timed out".to_string())?
        .map_err(|error| format!("failed to read rpi ready frame: {error}"))?;
    let ready: Value = serde_json::from_str(first_line.trim())
        .map_err(|error| format!("invalid rpi ready frame: {error}"))?;
    if ready["type"] != "ready" || ready["protocol"] != "rpi" {
        return Err("native child did not advertise protocol rpi".into());
    }
    Ok(ready)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

type Pending = Arc<StdMutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>>;
type Worker = Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>;

struct Outbound {
    value: Value,
    pending_id: Option<String>,
}

// Removing on drop also covers a cancelled HTTP request and a full command queue.
struct PendingRequest {
    id: String,
    pending: Pending,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.pending
            .lock()
            .expect("pending requests")
            .remove(&self.id);
    }
}

async fn dispatch(
    commands: &mpsc::Sender<Outbound>,
    pending: &Pending,
    mut value: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let original_id = value.get("id").cloned();
    let id = uuid::Uuid::new_v4().to_string();
    value["id"] = json!(id);
    let (reply, receive) = oneshot::channel();
    pending
        .lock()
        .expect("pending requests")
        .insert(id.clone(), reply);
    let _request = PendingRequest {
        id: id.clone(),
        pending: pending.clone(),
    };
    let mut response = tokio::time::timeout(timeout, async {
        commands
            .send(Outbound {
                value,
                pending_id: Some(id),
            })
            .await
            .map_err(|_| "native rpi host command channel closed".to_string())?;
        receive
            .await
            .map_err(|_| "native rpi host response channel closed".to_string())?
    })
    .await
    .map_err(|_| "native rpi host response timed out".to_string())??;
    if let Some(id) = original_id {
        response["id"] = id;
    } else if let Some(object) = response.as_object_mut() {
        object.remove("id");
    }
    Ok(response)
}

async fn write_commands(
    mut stdin: impl AsyncWrite + Unpin,
    mut outbound: mpsc::Receiver<Outbound>,
    pending: Pending,
) -> Result<(), String> {
    while let Some(outbound) = outbound.recv().await {
        if outbound
            .pending_id
            .as_ref()
            .is_some_and(|id| !pending.lock().expect("pending requests").contains_key(id))
        {
            continue;
        }
        let mut line = outbound.value.to_string().into_bytes();
        line.push(b'\n');
        stdin
            .write_all(&line)
            .await
            .map_err(|error| format!("failed to write rpi command: {error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("failed to flush rpi command: {error}"))?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct HostSnapshot {
    pub ready: Value,
    pub status: String,
    pub last_error: Option<String>,
    pub latest_settlement: Option<Value>,
    pub pending_dialog: Option<Value>,
    pub pending_dialogs: Vec<Value>,
    pub title: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub last_seq: u64,
}

/// One projected event tagged with its session-local sequence number.
#[derive(Debug, Clone)]
pub struct BufferedEvent {
    pub seq: u64,
    pub value: Value,
}

/// Native launch settings independent of any client protocol.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub executable: PathBuf,
    pub project: PathBuf,
    pub session_id: String,
    pub session_dir: Option<PathBuf>,
    pub saved_session: Option<PathBuf>,
    pub fork_source: Option<String>,
    pub timings_dir: PathBuf,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub permission_gate: bool,
    pub bypass: bool,
}

pub struct HostSession {
    worker: Worker,
    shutdown_gate: Mutex<()>,
    ready: Value,
    runtime: Arc<HostRuntime>,
    run_gate: Arc<Mutex<()>>,
    cancellation: tokio::sync::watch::Sender<u64>,
    writer_abort: StdMutex<Option<tokio::task::AbortHandle>>,
    commands: mpsc::Sender<Outbound>,
}

// Shared with the reader and finalizer, without retaining the owning session.
struct HostRuntime {
    raw_events: broadcast::Sender<Value>,
    events: broadcast::Sender<BufferedEvent>,
    pending: Pending,
    state: Arc<RwLock<HostState>>,
    history: Arc<Mutex<VecDeque<BufferedEvent>>>,
    seq: Arc<AtomicU64>,
    timings: StdMutex<Option<crate::timing::ToolTimings>>,
    closed: Mutex<bool>,
    closed_signal: tokio::sync::watch::Sender<bool>,
}

impl HostRuntime {
    async fn terminate(self: Arc<Self>, worker: Worker) {
        let mut worker = worker.lock().await;
        if let Some(task) = worker.as_mut() {
            task.abort();
            let _ = task.await;
        }
        worker.take();
        self.finish().await;
    }

    async fn finish(&self) {
        let mut closed = self.closed.lock().await;
        if *closed {
            return;
        }
        self.finish_timings();
        publish(
            &self.events,
            &self.history,
            &self.state,
            &self.seq,
            json!({"type":"host_closed"}),
        )
        .await;
        *closed = true;
        self.closed_signal.send_replace(true);
        let _ = self.raw_events.send(json!({"type":"host_closed"}));
        self.fail_pending();
    }

    fn finish_timings(&self) {
        if let Some(timings) = self.timings.lock().expect("tool timings").as_mut() {
            timings.finish_active();
        }
    }

    fn fail_pending(&self) {
        for (_, reply) in self.pending.lock().expect("pending requests").drain() {
            let _ = reply.send(Err("native rpi host closed".into()));
        }
    }
}

#[derive(Debug)]
struct HostState {
    status: String,
    last_error: Option<String>,
    latest_settlement: Option<Value>,
    pending_dialogs: Vec<Value>,
    title: Option<String>,
    created_at: u64,
    updated_at: u64,
    last_seq: u64,
}

impl HostSession {
    /// Start a native host for a durable session id.
    ///
    /// `--session-id` makes the id authoritative instead of letting the child
    /// mint one, so a bridge restart can reopen the same transcript.
    pub async fn spawn(
        rpi_bin: PathBuf,
        project: PathBuf,
        session_dir: Option<PathBuf>,
        session_id: &str,
        timings_dir: PathBuf,
    ) -> Result<Arc<Self>, String> {
        Self::spawn_with_fork(rpi_bin, project, session_dir, session_id, None, timings_dir).await
    }

    /// Start a new native session by forking an existing durable session.
    pub async fn spawn_fork(
        rpi_bin: PathBuf,
        project: PathBuf,
        session_dir: Option<PathBuf>,
        requested_id: &str,
        source_id: &str,
        timings_dir: PathBuf,
    ) -> Result<Arc<Self>, String> {
        Self::spawn_with_fork(
            rpi_bin,
            project,
            session_dir,
            requested_id,
            Some(source_id),
            timings_dir,
        )
        .await
    }

    async fn spawn_with_fork(
        rpi_bin: PathBuf,
        project: PathBuf,
        session_dir: Option<PathBuf>,
        session_id: &str,
        fork_source: Option<&str>,
        timings_dir: PathBuf,
    ) -> Result<Arc<Self>, String> {
        Self::spawn_options(SessionOptions {
            executable: rpi_bin,
            project,
            session_id: session_id.to_owned(),
            session_dir,
            saved_session: None,
            fork_source: fork_source.map(str::to_owned),
            timings_dir,
            model: None,
            provider: None,
            permission_gate: false,
            bypass: false,
        })
        .await
    }

    pub async fn spawn_options(options: SessionOptions) -> Result<Arc<Self>, String> {
        let session_id = &options.session_id;
        let mut timings = crate::timing::ToolTimings::load(options.timings_dir.join(format!(
            "{}.json",
            options.fork_source.as_deref().unwrap_or(session_id)
        )))?;
        timings.set_session(session_id);
        let mut command = crate::process::background_command(&options.executable);
        command.arg("--mode").arg("rpc");
        if let Some(path) = &options.saved_session {
            command.arg("--session").arg(path);
        } else if let Some(source) = &options.fork_source {
            command.arg("--fork").arg(source);
        } else {
            command.arg("--session-id").arg(session_id);
        }
        if let Some(directory) = &options.session_dir {
            command.arg("--session-dir").arg(directory);
        }
        if let Some(model) = &options.model {
            command.arg("--model").arg(model);
        }
        if let Some(provider) = &options.provider {
            command.arg("--provider").arg(provider);
        }
        crate::process::configure_permission_gate(
            &mut command,
            options.bypass,
            !options.permission_gate,
        );
        let project = options.project;
        let rpi_bin = options.executable;
        let mut child = command
            .current_dir(project)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|error| format!("failed to start {}: {error}", rpi_bin.display()))?;
        let stdin = child.stdin.take().ok_or("rpi host stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("rpi host stdout unavailable")?;
        let mut reader = BufReader::new(stdout);
        let ready = read_ready(&mut reader, Duration::from_secs(15)).await?;

        let (raw_events, _) = broadcast::channel(16_384);
        let (events, _) = broadcast::channel(512);
        let (commands, outbound) = mpsc::channel::<Outbound>(32);
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let created_at = now_ms();
        let state = Arc::new(RwLock::new(HostState {
            status: "ready".into(),
            last_error: None,
            latest_settlement: None,
            pending_dialogs: Vec::new(),
            title: None,
            created_at,
            updated_at: created_at,
            last_seq: 0,
        }));
        let history: Arc<Mutex<VecDeque<BufferedEvent>>> = Arc::new(Mutex::new(VecDeque::new()));
        let seq = Arc::new(AtomicU64::new(0));
        let runtime = Arc::new(HostRuntime {
            raw_events: raw_events.clone(),
            events: events.clone(),
            pending: pending.clone(),
            state: state.clone(),
            history: history.clone(),
            seq: seq.clone(),
            timings: StdMutex::new(Some(timings)),
            closed: Mutex::new(false),
            closed_signal: tokio::sync::watch::channel(false).0,
        });
        let session = Arc::new(Self {
            worker: Arc::new(Mutex::new(None)),
            shutdown_gate: Mutex::new(()),
            ready: ready.clone(),
            runtime: runtime.clone(),
            run_gate: Arc::new(Mutex::new(())),
            cancellation: tokio::sync::watch::channel(0).0,
            writer_abort: StdMutex::new(None),
            commands,
        });

        let pending_writer = pending.clone();
        let (writer_failed, mut writer_failure) = oneshot::channel();
        let writer = tokio::spawn(async move {
            if let Err(error) = write_commands(stdin, outbound, pending_writer).await {
                let _ = writer_failed.send(error);
            }
        });

        *session.writer_abort.lock().expect("writer abort") = Some(writer.abort_handle());
        let pending_reader = pending.clone();
        let events_reader = events.clone();
        let state_reader = state.clone();
        let history_reader = history.clone();
        let seq_reader = seq.clone();
        let worker = tokio::spawn(async move {
            let mut lines = reader.lines();
            loop {
                let line = tokio::select! {
                    failure = &mut writer_failure => {
                        if let Ok(error) = failure {
                            publish(&events_reader, &history_reader, &state_reader, &seq_reader,
                                json!({"type":"host_error","error":error})).await;
                        }
                        break;
                    }
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => line,
                        _ => break,
                    }
                };
                let Ok(mut value) = serde_json::from_str::<Value>(&line) else {
                    publish(
                        &events_reader,
                        &history_reader,
                        &state_reader,
                        &seq_reader,
                        json!({"type":"host_error","error":"invalid JSON frame"}),
                    )
                    .await;
                    continue;
                };
                if value["type"] == "response" {
                    runtime
                        .timings
                        .lock()
                        .expect("tool timings")
                        .as_mut()
                        .unwrap()
                        .response(&mut value);
                    let _ = raw_events.send(value.clone());
                    if value["success"] == true
                        && value["data"]["accepted"] == true
                        && matches!(
                            value["command"].as_str(),
                            Some("prompt" | "compact" | "resume_pending")
                        )
                    {
                        publish(
                            &events_reader,
                            &history_reader,
                            &state_reader,
                            &seq_reader,
                            json!({"type":"operation_accepted", "command":value["command"]}),
                        )
                        .await;
                    }
                    let key = value.get("id").and_then(Value::as_str).unwrap_or_default();
                    if let Some(reply) =
                        pending_reader.lock().expect("pending requests").remove(key)
                    {
                        let _ = reply.send(Ok(value));
                    }
                } else {
                    runtime
                        .timings
                        .lock()
                        .expect("tool timings")
                        .as_mut()
                        .unwrap()
                        .event(&mut value);
                    let _ = raw_events.send(value.clone());
                    publish(
                        &events_reader,
                        &history_reader,
                        &state_reader,
                        &seq_reader,
                        value,
                    )
                    .await;
                }
            }
            // A separate finalizer survives cancellation of the reader task.
            let finish = tokio::spawn(async move { runtime.finish().await });
            let _ = finish.await;
            writer.abort();
            let _ = writer.await;
            let _ = child.kill().await;
        });

        *session.worker.lock().await = Some(worker);
        Ok(session)
    }

    async fn ensure_running(&self) -> Result<(), String> {
        if self.runtime.state.read().await.status == "closed" {
            Err("rpi backend stopped; restart the connection to restore saved history".into())
        } else {
            Ok(())
        }
    }

    pub fn try_run_lease(&self) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
        self.run_gate
            .clone()
            .try_lock_owned()
            .map_err(|_| "a prompt is already running".into())
    }
    pub async fn begin_prompt(&self, message: String) -> Result<crate::run::PromptRun, String> {
        let lease = self.try_run_lease()?;
        let events = self.subscribe_raw();
        let cancellation = self.cancellation.subscribe();
        let id = json!(uuid::Uuid::new_v4().to_string());
        self.notify(json!({"type":"prompt","id":id,"message":message}))
            .await?;
        Ok(crate::run::PromptRun::new(events, cancellation, id, lease))
    }
    pub async fn abort_run(&self) -> Result<(), String> {
        self.cancellation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        self.notify(json!({"type":"abort"})).await
    }

    pub fn subscribe_raw(&self) -> broadcast::Receiver<Value> {
        self.runtime.raw_events.subscribe()
    }

    pub async fn notify(&self, value: Value) -> Result<(), String> {
        self.ensure_running().await?;
        self.commands
            .send(Outbound {
                value,
                pending_id: None,
            })
            .await
            .map_err(|_| "native rpi host command channel closed".into())
    }

    pub async fn terminate(&self) {
        let _guard = self.shutdown_gate.lock().await;
        self.terminate_worker().await;
    }

    async fn terminate_worker(&self) {
        self.cancellation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        if let Some(abort) = self.writer_abort.lock().expect("writer abort").take() {
            abort.abort();
        }
        // This cleanup owns the join handle even if its caller is cancelled.
        // The shared worker lock also serializes concurrent cleanup attempts.
        let _ = tokio::spawn(self.runtime.clone().terminate(self.worker.clone())).await;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_connection(
        stdin: impl AsyncWrite + Unpin + Send + 'static,
        capacity: usize,
    ) -> (Arc<Self>, TestEvents) {
        Self::test_connection_with_ready(
            stdin,
            capacity,
            json!({
                "type": "ready",
                "protocol": "rpi",
                "supportedCommands": ["get_session_branch", "append_entry"]
            }),
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_connection_with_ready(
        stdin: impl AsyncWrite + Unpin + Send + 'static,
        capacity: usize,
        ready: Value,
    ) -> (Arc<Self>, TestEvents) {
        let (commands, outbound) = mpsc::channel(32);
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let (raw_events, _) = broadcast::channel(capacity);
        let writer = tokio::spawn(write_commands(stdin, outbound, pending.clone()));
        let abort = writer.abort_handle();
        let session = Arc::new(Self {
            worker: Arc::new(Mutex::new(None)),
            shutdown_gate: Mutex::new(()),
            ready,
            run_gate: Arc::new(Mutex::new(())),
            cancellation: tokio::sync::watch::channel(0).0,
            writer_abort: StdMutex::new(Some(abort)),
            commands,
            runtime: Arc::new(HostRuntime {
                raw_events,
                events: broadcast::channel(capacity).0,
                pending,
                state: Arc::new(RwLock::new(HostState {
                    status: "ready".into(),
                    last_error: None,
                    latest_settlement: None,
                    pending_dialogs: vec![],
                    title: None,
                    created_at: 0,
                    updated_at: 0,
                    last_seq: 0,
                })),
                history: Arc::new(Mutex::new(VecDeque::new())),
                seq: Arc::new(AtomicU64::new(0)),
                timings: StdMutex::new(None),
                closed: Mutex::new(false),
                closed_signal: tokio::sync::watch::channel(false).0,
            }),
        });
        (session.clone(), TestEvents(session))
    }

    pub fn ready(&self) -> Value {
        self.ready.clone()
    }

    /// Whether the native ready frame advertises an optional RPC command.
    /// Durable support requires an explicit capability advertisement so an old
    /// host cannot be crashed by an unknown optional command.
    pub fn supports_command(&self, command: &str) -> bool {
        self.ready
            .get("supportedCommands")
            .and_then(Value::as_array)
            .is_some_and(|commands| commands.iter().any(|value| value.as_str() == Some(command)))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BufferedEvent> {
        self.runtime.events.subscribe()
    }

    /// Replayable projection of everything this session has emitted.
    pub async fn history(&self, after: Option<u64>) -> Vec<BufferedEvent> {
        let history = self.runtime.history.lock().await;
        history
            .iter()
            .filter(|event| after.map_or(true, |after| event.seq > after))
            .cloned()
            .collect()
    }

    /// The first prompt becomes the sidebar title. Returns whether it was set.
    pub async fn set_title_if_empty(&self, title: &str) -> bool {
        let mut state = self.runtime.state.write().await;
        if state.title.is_some() {
            return false;
        }
        state.title = Some(title.to_string());
        state.updated_at = now_ms();
        true
    }

    /// Best-effort `stop` before the registry drops this session.
    pub async fn stop(&self) {
        let _ = self
            .commands
            .send(Outbound {
                value: json!({"type": "stop", "id": uuid::Uuid::new_v4().to_string()}),
                pending_id: None,
            })
            .await;
    }

    pub async fn shutdown(&self) {
        let _guard = self.shutdown_gate.lock().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), self.stop()).await;
        let mut worker = self.worker.lock().await;
        if let Some(task) = worker.as_mut() {
            if tokio::time::timeout(Duration::from_secs(3), &mut *task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        worker.take();
        drop(worker);
        self.terminate_worker().await;
    }

    pub async fn snapshot(&self) -> HostSnapshot {
        let state = self.runtime.state.read().await;
        HostSnapshot {
            ready: self.ready(),
            status: state.status.clone(),
            last_error: state.last_error.clone(),
            latest_settlement: state.latest_settlement.clone(),
            pending_dialog: state.pending_dialogs.first().cloned(),
            pending_dialogs: state.pending_dialogs.clone(),
            title: state.title.clone(),
            created_at: state.created_at,
            updated_at: state.updated_at,
            last_seq: state.last_seq,
        }
    }

    /// Read authoritative session state while no prompt lease is held.
    pub async fn state(&self) -> Result<Value, String> {
        let _prompt = self.try_run_lease()?;
        let response = self
            .send_timeout(
                json!({"type":"get_state"}),
                std::time::Duration::from_secs(15),
            )
            .await?;
        if response["success"] != true {
            return Err(format!(
                "open session: {}",
                response["error"].as_str().unwrap_or("invalid RPC response")
            ));
        }
        if !response["data"].is_object() {
            return Err("open session: invalid RPC state data".into());
        }
        Ok(response["data"].clone())
    }

    async fn runtime_command(&self, kind: &str, params: Value) -> Result<Value, String> {
        self.command(kind, params, Duration::from_secs(15)).await
    }
    /// The model catalog the child can switch to (`get_available_models`).
    pub async fn available_models(&self) -> Result<Value, String> {
        Ok(self
            .runtime_command("get_available_models", json!({}))
            .await?["models"]
            .clone())
    }

    /// Read the child's state without acquiring the prompt lock. Used by
    /// `session/set_config_option`, which must also work while a prompt runs
    /// (the locked [`Self::state`] would reject it).
    pub async fn state_unlocked(&self) -> Result<Value, String> {
        self.runtime_command("get_state", json!({})).await
    }

    /// Switch the child's active model (`set_model`).
    pub async fn set_model(&self, model_id: &str, provider: Option<&str>) -> Result<(), String> {
        let mut params = serde_json::Map::new();
        params.insert("modelId".to_string(), json!(model_id));
        // Keep the legacy field for older rpi RPC hosts; current hosts ignore it.
        params.insert("model".to_string(), json!(model_id));
        if let Some(provider) = provider {
            params.insert("provider".to_string(), json!(provider));
        }
        self.runtime_command("set_model", Value::Object(params))
            .await?;
        Ok(())
    }

    /// The thinking levels the current model supports (`get_available_thinking_levels`).
    pub async fn available_thinking_levels(&self) -> Result<Value, String> {
        Ok(self
            .runtime_command("get_available_thinking_levels", json!({}))
            .await?["levels"]
            .clone())
    }

    /// Switch the child's thinking level (`set_thinking_level`).
    pub async fn set_thinking_level(&self, level: &str) -> Result<(), String> {
        let mut params = serde_json::Map::new();
        params.insert("level".to_string(), json!(level));
        self.runtime_command("set_thinking_level", Value::Object(params))
            .await?;
        Ok(())
    }

    /// Read the complete currently selected native session branch.
    pub async fn session_branch(&self) -> Result<Vec<Value>, String> {
        let data = self
            .runtime_command("get_session_branch", json!({}))
            .await?;
        data.get("entries")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "get_session_branch: invalid entries data".into())
    }

    /// Append a protocol-neutral custom entry to the native session branch.
    pub async fn append_custom_entry(
        &self,
        custom_type: &str,
        data: Value,
    ) -> Result<Value, String> {
        if custom_type.trim().is_empty() {
            return Err("append_entry: custom type is required".into());
        }
        self.runtime_command(
            "append_entry",
            json!({"customType": custom_type, "data": data}),
        )
        .await
    }

    pub async fn send(&self, value: Value) -> Result<Value, String> {
        self.send_timeout(value, Duration::from_secs(300)).await
    }

    pub async fn send_timeout(&self, value: Value, timeout: Duration) -> Result<Value, String> {
        let mut closed = self.runtime.closed_signal.subscribe();
        self.ensure_running().await?;
        tokio::select! {
            biased;
            _ = closed.wait_for(|closed| *closed) => Err("native rpi host closed".into()),
            response = dispatch(&self.commands, &self.runtime.pending, value, timeout) => response,
        }
    }

    pub async fn command(
        &self,
        kind: &str,
        mut params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let object = params
            .as_object_mut()
            .ok_or_else(|| format!("{kind}: params must be an object"))?;
        object.insert("type".into(), json!(kind));
        let response = self.send_timeout(params, timeout).await?;
        if response["success"] != true {
            return Err(format!(
                "{kind}: {}",
                response["error"].as_str().unwrap_or("invalid RPC response")
            ));
        }
        Ok(response["data"].clone())
    }
}

impl Drop for HostSession {
    fn drop(&mut self) {
        if let Ok(worker) = self.worker.try_lock() {
            if let Some(task) = worker.as_ref() {
                task.abort();
            }
        }
        if let Some(writer) = self.writer_abort.lock().expect("writer abort").take() {
            writer.abort();
        }
        self.runtime.finish_timings();
        self.runtime.fail_pending();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(self.runtime.clone().terminate(self.worker.clone()));
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
pub struct TestEvents(Arc<HostSession>);
#[cfg(any(test, feature = "test-support"))]
impl TestEvents {
    pub fn send(&self, value: Value) -> Result<usize, broadcast::error::SendError<Value>> {
        let mut delivered = false;
        if value["type"] == "response" {
            if let Some(key) = value["id"].as_str() {
                if let Some(reply) = self.0.runtime.pending.lock().expect("pending").remove(key) {
                    delivered = reply.send(Ok(value.clone())).is_ok();
                }
            }
        }
        match self.0.runtime.raw_events.send(value) {
            Err(_) if delivered => Ok(0),
            result => result,
        }
    }
}

/// Tag an event with its session sequence, remember it for replay, update the
/// derived status, then fan it out to live subscribers.
async fn publish(
    events: &broadcast::Sender<BufferedEvent>,
    history: &Arc<Mutex<VecDeque<BufferedEvent>>>,
    state: &Arc<RwLock<HostState>>,
    seq: &AtomicU64,
    value: Value,
) {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    let next = seq.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut state = state.write().await;
        state.last_seq = next;
        state.updated_at = now_ms();
        match kind {
            "agent_start" | "operation_accepted" => state.status = "running".into(),
            "ui_dialog_request" => {
                state.status = "awaiting_input".into();
                if let Some(current) = state
                    .pending_dialogs
                    .iter_mut()
                    .find(|dialog| dialog.get("requestId") == value.get("requestId"))
                {
                    *current = value.clone();
                } else {
                    state.pending_dialogs.push(value.clone());
                }
            }
            "ui_dialog_closed" => {
                state
                    .pending_dialogs
                    .retain(|dialog| dialog.get("requestId") != value.get("requestId"));
                if state.pending_dialogs.is_empty() && state.status == "awaiting_input" {
                    state.status = "running".into();
                }
            }
            "agent_settled" => {
                state.status = "ready".into();
                state.pending_dialogs.clear();
                state.latest_settlement = Some(value.clone());
                state.last_error = value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "host_closed" => {
                state.status = "closed".into();
                state.pending_dialogs.clear();
            }
            "host_error" => {
                state.last_error = value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }
            _ => {}
        }
    }
    let buffered = BufferedEvent { seq: next, value };
    {
        let mut history = history.lock().await;
        if history.len() >= HISTORY_LIMIT {
            history.pop_front();
        }
        history.push_back(buffered.clone());
    }
    let _ = events.send(buffered);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn forced_close_and_shutdown_timeout_finalize_all_session_observers_once() {
        for graceful in [false, true] {
            let root = std::env::temp_dir().join(format!("rpi-close-{}", uuid::Uuid::new_v4()));
            let path = root.join("timings.json");
            let (stdin, peer) = tokio::io::duplex(4096);
            let (session, injector) = HostSession::test_connection(stdin, 16);
            drop(injector);
            // Simulate a host that ignores stop and keeps stdout open.
            let worker = tokio::spawn(std::future::pending());
            let worker_abort = worker.abort_handle();
            *session.worker.lock().await = Some(worker);
            let mut timings = crate::timing::ToolTimings::load(path.clone()).unwrap();
            timings.event(&mut json!({"type":"tool_execution_start","toolCallId":"interrupted"}));
            *session.runtime.timings.lock().unwrap() = Some(timings);
            publish(
                &session.runtime.events,
                &session.runtime.history,
                &session.runtime.state,
                &session.runtime.seq,
                json!({"type":"ui_dialog_request","requestId":"question"}),
            )
            .await;
            let mut raw = session.subscribe_raw();
            let mut projected = session.subscribe();
            let waiting = session.clone();
            let request =
                tokio::spawn(async move { waiting.send(json!({"type":"get_state"})).await });
            let mut reader = BufReader::new(peer);
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(session.runtime.pending.lock().unwrap().len(), 1);

            if graceful {
                session.shutdown().await;
            } else {
                session.terminate().await;
            }
            assert!(worker_abort.is_finished());
            assert!(tokio::time::timeout(Duration::from_secs(1), request)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .contains("closed"));
            assert_eq!(raw.recv().await.unwrap()["type"], "host_closed");
            let closed = projected.recv().await.unwrap();
            assert_eq!(closed.value["type"], "host_closed");
            assert_eq!(closed.seq, 2);
            let snapshot = session.snapshot().await;
            assert_eq!(snapshot.status, "closed");
            assert_eq!(snapshot.last_seq, closed.seq);
            assert!(snapshot.pending_dialogs.is_empty());
            assert!(session.runtime.pending.lock().unwrap().is_empty());
            let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert!(saved.get("interrupted").is_some());
            assert_eq!(session.history(Some(1)).await.len(), 1);
            assert!(session.send(json!({"type":"get_state"})).await.is_err());
            session.terminate().await;
            assert!(matches!(
                raw.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
            assert!(matches!(
                projected.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn dropping_the_owner_publishes_a_final_close_to_existing_subscribers() {
        let (stdin, _peer) = tokio::io::duplex(64);
        let (session, injector) = HostSession::test_connection(stdin, 16);
        drop(injector);
        let mut raw = session.subscribe_raw();
        let mut projected = session.subscribe();
        drop(session);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), raw.recv())
                .await
                .unwrap()
                .unwrap()["type"],
            "host_closed"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), projected.recv())
                .await
                .unwrap()
                .unwrap()
                .value["type"],
            "host_closed"
        );
    }

    #[tokio::test]
    async fn cancelled_shutdown_retains_the_worker_and_termination_still_finishes() {
        let (stdin, _peer) = tokio::io::duplex(4096);
        let (session, injector) = HostSession::test_connection(stdin, 16);
        drop(injector);
        let worker = tokio::spawn(std::future::pending());
        let abort = worker.abort_handle();
        *session.worker.lock().await = Some(worker);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), session.shutdown())
                .await
                .is_err()
        );
        assert!(session.worker.lock().await.is_some());
        assert!(!abort.is_finished());

        // Delay the finalizer midway through publication, then cancel its caller.
        let history = session.runtime.history.lock().await;
        let mut projected = session.subscribe();
        let mut raw = session.subscribe_raw();
        let closing = session.clone();
        let caller = tokio::spawn(async move { closing.terminate().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while session.snapshot().await.status != "closed" {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        caller.abort();
        let _ = caller.await;
        drop(history);
        assert!(abort.is_finished());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), projected.recv())
                .await
                .unwrap()
                .unwrap()
                .value["type"],
            "host_closed"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), raw.recv())
                .await
                .unwrap()
                .unwrap()["type"],
            "host_closed"
        );
    }

    #[tokio::test]
    async fn termination_survives_caller_cancellation_before_joining_the_worker() {
        let (stdin, _peer) = tokio::io::duplex(64);
        let (session, injector) = HostSession::test_connection(stdin, 16);
        drop(injector);
        let worker = tokio::spawn(std::future::pending());
        let abort = worker.abort_handle();
        *session.worker.lock().await = Some(worker);
        let guard = session.worker.lock().await;
        let mut cancellation = session.cancellation.subscribe();
        let mut projected = session.subscribe();
        let closing = session.clone();
        let caller = tokio::spawn(async move { closing.terminate().await });
        tokio::time::timeout(Duration::from_secs(1), cancellation.changed())
            .await
            .unwrap()
            .unwrap();
        caller.abort();
        let _ = caller.await;
        drop(guard);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), projected.recv())
                .await
                .unwrap()
                .unwrap()
                .value["type"],
            "host_closed"
        );
        assert!(abort.is_finished());
        assert_eq!(session.snapshot().await.status, "closed");
    }

    #[tokio::test]
    async fn dialog_closure_matches_the_current_request_and_restores_running_status() {
        let (events, _) = broadcast::channel(16);
        let history = Arc::new(Mutex::new(VecDeque::new()));
        let state = Arc::new(RwLock::new(HostState {
            status: "ready".into(),
            last_error: None,
            latest_settlement: None,
            pending_dialogs: Vec::new(),
            title: None,
            created_at: 0,
            updated_at: 0,
            last_seq: 0,
        }));
        let seq = AtomicU64::new(0);
        for id in ["old", "current"] {
            publish(
                &events,
                &history,
                &state,
                &seq,
                json!({"type":"ui_dialog_request","requestId":id}),
            )
            .await;
        }
        assert_eq!(state.read().await.pending_dialogs.len(), 2);
        assert_eq!(
            state.read().await.pending_dialogs.first().unwrap()["requestId"],
            "old"
        );
        publish(
            &events,
            &history,
            &state,
            &seq,
            json!({"type":"ui_dialog_closed","requestId":"unknown"}),
        )
        .await;
        assert_eq!(state.read().await.pending_dialogs.len(), 2);
        publish(
            &events,
            &history,
            &state,
            &seq,
            json!({"type":"ui_dialog_closed","requestId":"old"}),
        )
        .await;
        assert_eq!(
            state.read().await.pending_dialogs.first().unwrap()["requestId"],
            "current"
        );
        assert_eq!(state.read().await.status, "awaiting_input");
        publish(
            &events,
            &history,
            &state,
            &seq,
            json!({"type":"ui_dialog_closed","requestId":"current"}),
        )
        .await;
        assert!(state.read().await.pending_dialogs.is_empty());
        assert_eq!(state.read().await.status, "running");
        publish(
            &events,
            &history,
            &state,
            &seq,
            json!({"type":"agent_settled","outcome":"completed"}),
        )
        .await;
        publish(
            &events,
            &history,
            &state,
            &seq,
            json!({"type":"ui_dialog_closed","requestId":"current"}),
        )
        .await;
        assert_eq!(state.read().await.status, "ready");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn broken_stdin_closes_the_host_and_fails_requests_without_waiting_for_stdout() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("rpi-broken-pipe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let script = root.join("host");
        std::fs::write(&script, "#!/bin/sh\nexec 0<&-\nprintf '{\"type\":\"ready\",\"protocol\":\"rpi\"}\\n'\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let session = HostSession::spawn(script, root.clone(), None, "test", root.join("timings"))
            .await
            .unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            session.send(json!({"id":"test","type":"get_state"})),
        )
        .await
        .unwrap();
        assert!(response.unwrap_err().contains("closed"));
        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.status, "closed");
        assert!(snapshot.last_error.unwrap().contains("failed to write"));
        let history = session.history(None).await;
        assert_eq!(
            history.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(history[0].value["type"], "host_error");
        assert_eq!(history[1].value["type"], "host_closed");
        assert!(session.runtime.pending.lock().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn caller_ids_are_isolated_and_restored_even_for_null_and_missing_ids() {
        let (commands, mut outbound) = mpsc::channel::<Outbound>(8);
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let mut requests = Vec::new();
        for value in [
            json!({"id": "same", "type": "a"}),
            json!({"id": "same", "type": "b"}),
            json!({"id": null, "type": "c"}),
            json!({"type": "d"}),
        ] {
            let commands = commands.clone();
            let pending = pending.clone();
            requests.push(tokio::spawn(async move {
                dispatch(&commands, &pending, value, Duration::from_secs(1))
                    .await
                    .unwrap()
            }));
        }
        let mut frames = Vec::new();
        for _ in 0..4 {
            frames.push(outbound.recv().await.unwrap().value);
        }
        assert_eq!(pending.lock().unwrap().len(), 4);
        // Replies arrive in reverse order and must still reach their own caller.
        for frame in frames.into_iter().rev() {
            let reply = pending
                .lock()
                .unwrap()
                .remove(frame["id"].as_str().unwrap())
                .unwrap();
            reply
                .send(Ok(json!({"id": frame["id"], "command": frame["type"]})))
                .unwrap();
        }
        for (index, request) in requests.into_iter().enumerate() {
            let response = request.await.unwrap();
            assert_eq!(response["command"], ["a", "b", "c", "d"][index]);
            match index {
                0 | 1 => assert_eq!(response["id"], "same"),
                2 => assert_eq!(response.get("id"), Some(&Value::Null)),
                _ => assert!(response.get("id").is_none()),
            }
        }
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn queue_and_response_timeouts_and_cancellation_remove_pending_requests() {
        let (commands, mut outbound) = mpsc::channel::<Outbound>(1);
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        commands
            .send(Outbound {
                value: json!({"type":"stop"}),
                pending_id: None,
            })
            .await
            .unwrap();
        let error = dispatch(
            &commands,
            &pending,
            json!({"type":"get_state"}),
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();
        assert!(error.contains("timed out"));
        assert!(pending.lock().unwrap().is_empty());
        outbound.recv().await.unwrap();
        let error = dispatch(
            &commands,
            &pending,
            json!({"type":"get_state"}),
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();
        assert!(error.contains("timed out"));
        assert!(pending.lock().unwrap().is_empty());
        outbound.recv().await.unwrap();
        let request_pending = pending.clone();
        let request_commands = commands.clone();
        let task = tokio::spawn(async move {
            dispatch(
                &request_commands,
                &request_pending,
                json!({"type":"get_state"}),
                Duration::from_secs(300),
            )
            .await
        });
        outbound.recv().await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn writer_reports_broken_pipe_and_skips_expired_commands() {
        let (commands, outbound) = mpsc::channel(2);
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        commands
            .send(Outbound {
                value: json!({"type":"expired"}),
                pending_id: Some("expired".into()),
            })
            .await
            .unwrap();
        commands
            .send(Outbound {
                value: json!({"type":"stop"}),
                pending_id: None,
            })
            .await
            .unwrap();
        drop(commands);
        let mut bytes = Vec::new();
        write_commands(&mut bytes, outbound, pending.clone())
            .await
            .unwrap();
        assert_eq!(String::from_utf8(bytes).unwrap(), "{\"type\":\"stop\"}\n");
        let (commands, outbound) = mpsc::channel(1);
        commands
            .send(Outbound {
                value: json!({"type":"stop"}),
                pending_id: None,
            })
            .await
            .unwrap();
        let (writer, reader) = tokio::io::duplex(64);
        drop(reader);
        assert!(write_commands(writer, outbound, pending)
            .await
            .unwrap_err()
            .contains("failed to write"));
    }

    #[tokio::test]
    async fn ready_frames_are_validated_bounded_and_have_a_deadline() {
        let mut valid = &b"{\"type\":\"ready\",\"protocol\":\"rpi\"}\n"[..];
        assert!(read_ready(&mut valid, Duration::from_secs(1)).await.is_ok());
        let mut invalid = &b"{\"type\":\"ready\",\"protocol\":\"other\"}\n"[..];
        assert!(read_ready(&mut invalid, Duration::from_secs(1))
            .await
            .is_err());
        let large = vec![b'x'; 1024 * 1024 + 1];
        let mut oversized = large.as_slice();
        assert!(read_ready(&mut oversized, Duration::from_secs(1))
            .await
            .is_err());
        assert_eq!(oversized.len(), 1);
        let (silent, _writer) = tokio::io::duplex(64);
        let mut silent = BufReader::new(silent);
        assert!(read_ready(&mut silent, Duration::from_millis(20))
            .await
            .unwrap_err()
            .contains("timed out"));
    }
}
