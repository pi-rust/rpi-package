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
    process::Command,
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

pub struct HostSession {
    worker: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    ready: Value,
    events: broadcast::Sender<BufferedEvent>,
    commands: mpsc::Sender<Outbound>,
    pending: Pending,
    state: Arc<RwLock<HostState>>,
    history: Arc<Mutex<VecDeque<BufferedEvent>>>,
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
        let mut timings = crate::timing::ToolTimings::load(
            timings_dir.join(format!("{}.json", fork_source.unwrap_or(session_id))),
        )?;
        timings.set_session(session_id);
        let mut command = Command::new(&rpi_bin);
        command.arg("--mode").arg("rpc");
        if let Some(source) = fork_source {
            command.arg("--fork").arg(source);
        } else {
            command.arg("--session-id").arg(session_id);
        }
        if let Some(dir) = session_dir {
            command.arg("--session-dir").arg(dir);
        }
        let mut child = command
            .current_dir(project)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("failed to start {}: {error}", rpi_bin.display()))?;
        let stdin = child.stdin.take().ok_or("rpi host stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("rpi host stdout unavailable")?;
        let mut reader = BufReader::new(stdout);
        let ready = read_ready(&mut reader, Duration::from_secs(15)).await?;

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
        let session = Arc::new(Self {
            worker: StdMutex::new(None),
            ready: ready.clone(),
            events: events.clone(),
            commands,
            pending: pending.clone(),
            state: state.clone(),
            history: history.clone(),
        });

        let pending_writer = pending.clone();
        let (writer_failed, mut writer_failure) = oneshot::channel();
        let writer = tokio::spawn(async move {
            if let Err(error) = write_commands(stdin, outbound, pending_writer).await {
                let _ = writer_failed.send(error);
            }
        });

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
                    timings.response(&mut value);
                    timings.response(&mut value);
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
                    timings.event(&mut value);
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
            timings.finish_active();
            {
                let mut state = state_reader.write().await;
                state.status = "closed".into();
                state.updated_at = now_ms();
            }
            publish(
                &events_reader,
                &history_reader,
                &state_reader,
                &seq_reader,
                json!({"type":"host_closed"}),
            )
            .await;
            writer.abort();
            let _ = writer.await;
            let _ = child.kill().await;
            let mut pending = pending_reader.lock().expect("pending requests");
            for (_, reply) in pending.drain() {
                let _ = reply.send(Err("native rpi host closed".into()));
            }
        });

        *session.worker.lock().expect("host worker") = Some(worker);
        Ok(session)
    }

    pub fn ready(&self) -> Value {
        self.ready.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BufferedEvent> {
        self.events.subscribe()
    }

    /// Replayable projection of everything this session has emitted.
    pub async fn history(&self, after: Option<u64>) -> Vec<BufferedEvent> {
        let history = self.history.lock().await;
        history
            .iter()
            .filter(|event| after.map_or(true, |after| event.seq > after))
            .cloned()
            .collect()
    }

    /// The first prompt becomes the sidebar title. Returns whether it was set.
    pub async fn set_title_if_empty(&self, title: &str) -> bool {
        let mut state = self.state.write().await;
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
        let _ = tokio::time::timeout(Duration::from_secs(1), self.stop()).await;
        let worker = self.worker.lock().expect("host worker").take();
        if let Some(mut worker) = worker {
            if tokio::time::timeout(Duration::from_secs(3), &mut worker)
                .await
                .is_err()
            {
                worker.abort();
                let _ = worker.await;
            }
        }
    }

    pub async fn snapshot(&self) -> HostSnapshot {
        let state = self.state.read().await;
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

    pub async fn send(&self, value: Value) -> Result<Value, String> {
        dispatch(
            &self.commands,
            &self.pending,
            value,
            Duration::from_secs(300),
        )
        .await
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
        assert!(session.pending.lock().unwrap().is_empty());
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
