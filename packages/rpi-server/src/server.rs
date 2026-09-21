use crate::transport::{Connection, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::broadcast;
use tokio::sync::Mutex as TokioMutex;

const MAX_LINE: usize = 1_048_576;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub state: String,
    pub pid: Option<u32>,
    pub event_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub id: String,
    pub address: String,
    pub token: String,
    pub state: String,
    pub sessions: Vec<SessionInfo>,
}

// ── Session ───────────────────────────────────────────────────────────────────

pub struct Session {
    pub id: String,
    pub child: Child,
    pub stdin: ChildStdin,
    pub events: broadcast::Sender<Value>,
    pub event_count: u64,
    pub state: String,
}

impl Session {
    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            state: self.state.clone(),
            pid: self.child.id(),
            event_count: self.event_count,
        }
    }
}

// ── AppState ──────────────────────────────────────────────────────────────────

pub struct AppState {
    pub sessions: TokioMutex<HashMap<String, Arc<TokioMutex<Session>>>>,
    pub executable: PathBuf,
    pub default_args: Vec<String>,
    pub token: String,
    pub server_id: String,
    pub bound_address: TokioMutex<String>,
    pub running: AtomicBool,
}

impl AppState {
    pub fn new(
        executable: PathBuf,
        default_args: Vec<String>,
        token: String,
        server_id: String,
    ) -> Self {
        Self {
            sessions: TokioMutex::new(HashMap::new()),
            executable,
            default_args,
            token,
            server_id,
            bound_address: TokioMutex::new(String::new()),
            running: AtomicBool::new(true),
        }
    }

    pub async fn info(&self, address: &str, state: &str) -> ServerInfo {
        let sessions: Vec<SessionInfo> = {
            let sessions_lock = self.sessions.lock().await;
            let mut infos = Vec::new();
            for s in sessions_lock.values() {
                let guard = s.lock().await;
                infos.push(guard.info());
            }
            infos
        };
        ServerInfo {
            id: self.server_id.clone(),
            address: address.to_string(),
            token: self.token.clone(),
            state: state.to_string(),
            sessions,
        }
    }
}

// ── Token generation ──────────────────────────────────────────────────────────

pub fn generate_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let state = RandomState::new();
    let t1 = state.build_hasher().finish();
    let t2 = state.build_hasher().finish();
    format!("{t1:016x}{t2:016x}")
}

static NEXT_SERVER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_server_id() -> String {
    format!("rpi-rpc-{}", NEXT_SERVER_ID.fetch_add(1, Ordering::Relaxed))
}

pub fn next_session_id() -> String {
    format!("session-{}", NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed))
}

// ── Child process ─────────────────────────────────────────────────────────────

pub fn spawn_child(
    executable: &PathBuf,
    args: &[String],
    events_tx: broadcast::Sender<Value>,
) -> Result<(Child, ChildStdin), String> {
    let mut child = Command::new(executable)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start rpi: {e}"))?;

    let stdin = child.stdin.take().ok_or("stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("stdout unavailable")?;

    tokio::spawn(async move {
        let reader = tokio::io::BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.len() > MAX_LINE {
                let _ = events_tx.send(json!({"type": "error", "error": "line exceeds limit"}));
                continue;
            }
            match serde_json::from_str::<Value>(&line) {
                Ok(value) => {
                    let _ = events_tx.send(value);
                }
                Err(e) => {
                    let _ = events_tx.send(json!({"type": "error", "error": format!("invalid json: {e}")}));
                }
            }
        }
        let _ = events_tx.send(json!({"type": "session_end"}));
    });

    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[rpi stderr] {line}");
            }
        });
    }

    Ok((child, stdin))
}

// ── Param parsing ─────────────────────────────────────────────────────────────

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(format!("{key} must not be empty")),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn string_array(params: &Value, key: &str) -> Result<Vec<String>, String> {
    let Some(v) = params.get(key) else {
        return Ok(Vec::new());
    };
    let arr = v
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of strings"))?;
    if arr.len() > 128 {
        return Err(format!("{key} contains too many values"));
    }
    arr.iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{key} must contain non-empty strings"))
        })
        .collect()
}

fn bool_param(params: &Value, key: &str) -> Result<bool, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

fn push_option(
    args: &mut Vec<String>,
    params: &Value,
    key: &str,
    flag: &str,
) -> Result<(), String> {
    if let Some(v) = optional_string(params, key)? {
        args.push(flag.into());
        args.push(v);
    }
    Ok(())
}

fn push_switch(args: &mut Vec<String>, params: &Value, key: &str, flag: &str) -> Result<(), String> {
    if bool_param(params, key)? {
        args.push(flag.into());
    }
    Ok(())
}

fn push_repeated(
    args: &mut Vec<String>,
    params: &Value,
    key: &str,
    flag: &str,
) -> Result<(), String> {
    for v in string_array(params, key)? {
        args.push(flag.into());
        args.push(v);
    }
    Ok(())
}

pub fn launch_args(params: &Value) -> Result<(PathBuf, Vec<String>), String> {
    let executable = std::env::current_exe()
        .map_err(|e| format!("failed to locate rpi executable: {e}"))?;
    let mut args = vec!["--mode".into(), "rpc".into()];

    push_option(&mut args, params, "provider", "--provider")?;
    push_option(&mut args, params, "model", "--model")?;
    push_option(&mut args, params, "baseUrl", "--base-url")?;
    push_option(&mut args, params, "systemPrompt", "--system-prompt")?;
    push_option(&mut args, params, "thinking", "--thinking")?;
    push_option(&mut args, params, "name", "--name")?;
    push_option(&mut args, params, "session", "--session")?;
    push_option(&mut args, params, "sessionId", "--session-id")?;
    push_option(&mut args, params, "sessionDir", "--session-dir")?;

    for v in string_array(params, "appendSystemPrompt")? {
        args.push("--append-system-prompt".into());
        args.push(v);
    }

    let tools = string_array(params, "tools")?;
    if !tools.is_empty() {
        args.push("--tools".into());
        args.push(tools.join(","));
    }
    let excluded = string_array(params, "excludeTools")?;
    if !excluded.is_empty() {
        args.push("--exclude-tools".into());
        args.push(excluded.join(","));
    }

    push_switch(&mut args, params, "noSession", "--no-session")?;
    push_switch(&mut args, params, "noTools", "--no-tools")?;
    push_switch(&mut args, params, "noBuiltinTools", "--no-builtin-tools")?;
    push_switch(&mut args, params, "noSkills", "--no-skills")?;
    push_switch(&mut args, params, "noPromptTemplates", "--no-prompt-templates")?;
    push_switch(&mut args, params, "noContextFiles", "--no-context-files")?;
    push_switch(&mut args, params, "noExtensions", "--no-extensions")?;
    push_switch(&mut args, params, "enablePiPackages", "--enable-pi-packages")?;
    push_switch(&mut args, params, "offline", "--offline")?;
    push_switch(&mut args, params, "headless", "--headless")?;
    push_repeated(&mut args, params, "extensionsDir", "--extensions-dir")?;
    push_repeated(&mut args, params, "extension", "--extension")?;
    push_repeated(&mut args, params, "skill", "--skill")?;
    push_repeated(&mut args, params, "promptTemplate", "--prompt-template")?;

    if let Some(approve) = params.get("approve") {
        match approve.as_bool() {
            Some(true) => args.push("--approve".into()),
            Some(false) => args.push("--no-approve".into()),
            None => return Err("approve must be a boolean".into()),
        }
    }

    if bool_param(params, "noSession")?
        && (params.get("session").is_some() || params.get("sessionId").is_some())
    {
        return Err("noSession cannot be combined with session or sessionId".into());
    }
    if params.get("session").is_some() && params.get("sessionId").is_some() {
        return Err("session and sessionId are mutually exclusive".into());
    }

    Ok((executable, args))
}

// ── JSON-RPC message handling ─────────────────────────────────────────────────



/// Handle a single JSON-RPC request message, returning the response.
async fn handle_request(
    state: &Arc<AppState>,
    msg: &Value,
) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    let result = match method {
        "start_session" => handle_start_session(state).await,
        "send" => handle_send(state, &params).await,
        "stop_session" => handle_stop_session(state, &params).await,
        "list_sessions" => handle_list_sessions(state).await,
        "server_status" => handle_server_status(state).await,
        "subscribe" => {
            // Subscribe is handled separately in handle_connection
            Err("subscribe should be handled by connection handler".into())
        }
        "ping" => Ok(json!({"type": "pong"})),
        _ => Err(format!("unknown method: {method}")),
    };

    let response = match result {
        Ok(value) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": value,
        }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32000, "message": e},
        }),
    };

    Some(response)
}

async fn handle_start_session(state: &Arc<AppState>) -> Result<Value, String> {
    let session_id = next_session_id();
    let (events_tx, _) = broadcast::channel(256);
    let (child, stdin) = spawn_child(&state.executable, &state.default_args, events_tx.clone())?;
    let session = Arc::new(TokioMutex::new(Session {
        id: session_id.clone(),
        child,
        stdin,
        events: events_tx,
        event_count: 0,
        state: "running".into(),
    }));
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), Arc::clone(&session));
    let info = session.lock().await.info();
    Ok(json!({"sessionId": session_id, "session": info}))
}

async fn handle_send(state: &Arc<AppState>, params: &Value) -> Result<Value, String> {
    let sid = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or("sessionId is required")?
        .to_string();
    let cmd = params.get("command").ok_or("command is required")?;
    let session = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&sid)
            .ok_or(format!("session {sid} not found"))?
            .clone()
    };
    let mut line = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut sl = session.lock().await;
    sl.stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;
    sl.stdin
        .flush()
        .await
        .map_err(|e| format!("flush: {e}"))?;
    Ok(json!({"sessionId": sid, "status": "sent"}))
}

async fn handle_stop_session(state: &Arc<AppState>, params: &Value) -> Result<Value, String> {
    let sid = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or("sessionId is required")?
        .to_string();
    let session = {
        let mut sessions = state.sessions.lock().await;
        sessions
            .remove(&sid)
            .ok_or(format!("session {sid} not found"))?
    };
    let mut sl = session.lock().await;
    sl.state = "stopped".into();
    let _ = sl.child.kill().await;
    let _ = sl.child.wait().await;
    Ok(json!({"sessionId": sid, "status": "stopped"}))
}

async fn handle_list_sessions(state: &Arc<AppState>) -> Result<Value, String> {
    let sessions = state.sessions.lock().await;
    let mut list = Vec::new();
    for s in sessions.values() {
        list.push(s.lock().await.info());
    }
    Ok(json!(list))
}

async fn handle_server_status(state: &Arc<AppState>) -> Result<Value, String> {
    let address = state.bound_address.lock().await.clone();
    let info = state.info(&address, "running").await;
    Ok(json!(info))
}

static NEXT_SUB_ID: AtomicU64 = AtomicU64::new(1);

// ── Connection handler ────────────────────────────────────────────────────────

/// Handle a single client connection.
pub async fn handle_connection<C: Connection>(
    state: Arc<AppState>,
    mut conn: C,
) {
    let conn_id = conn.id().to_string();
    // Accepts requests only after `authenticate` when the server has a token.
    let mut authenticated = state.token.is_empty();
    // Map of subscription_id -> broadcast receiver
    let mut sub_receivers: HashMap<u64, broadcast::Receiver<Value>> = HashMap::new();
    let mut sub_sessions: HashMap<u64, Arc<TokioMutex<Session>>> = HashMap::new();

    loop {
        // Check for subscription events (non-blocking)
        for (sub_id, rx) in sub_receivers.iter_mut() {
            loop {
                match rx.try_recv() {
                    Ok(event) => {
                        let notification = json!({
                            "jsonrpc": "2.0",
                            "method": "event",
                            "params": {
                                "subscriptionId": sub_id,
                                "event": event,
                            }
                        });
                        if conn.send(notification).await.is_err() {
                            // Connection closed
                            return;
                        }
                        if event.get("type").and_then(Value::as_str) == Some("session_end") {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                        let notification = json!({
                            "jsonrpc": "2.0",
                            "method": "event",
                            "params": {
                                "subscriptionId": sub_id,
                                "event": {"type": "lagged", "skipped": n},
                            }
                        });
                        let _ = conn.send(notification).await;
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
                }
            }
        }

        // Read next message with a short timeout to allow subscription polling
        let recv_result = tokio::time::timeout(
            tokio::time::Duration::from_millis(50),
            conn.recv(),
        )
        .await;

        match recv_result {
            Ok(Ok(Some(msg))) => {
                // Check if it's a subscribe request — handle specially
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("");

                // Connection-level token gate. When the server was started with a
                // token, every connection must `authenticate` before it may issue
                // any other request (including `subscribe`).
                if !authenticated {
                    let id = msg.get("id").cloned();
                    if method == "authenticate" {
                        let provided = msg
                            .get("params")
                            .and_then(|p| p.get("token"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if provided == state.token {
                            authenticated = true;
                            let response = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {"authenticated": true},
                            });
                            if conn.send(response).await.is_err() {
                                break;
                            }
                        } else {
                            let response = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {"code": -32001, "message": "invalid token"},
                            });
                            let _ = conn.send(response).await;
                            // Drop the connection on a bad token.
                            break;
                        }
                    } else {
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {"code": -32001, "message": "authentication required"},
                        });
                        if conn.send(response).await.is_err() {
                            break;
                        }
                    }
                    continue;
                }

                if method == "subscribe" {
                    let id = msg.get("id").cloned();
                    let params = msg.get("params").cloned().unwrap_or(json!({}));
                    let sid = params
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();

                    let session = {
                        let sessions = state.sessions.lock().await;
                        sessions.get(&sid).cloned()
                    };

                    match session {
                        Some(session) => {
                            let sub_id = NEXT_SUB_ID.fetch_add(1, Ordering::Relaxed);
                            let rx = {
                                let sl = session.lock().await;
                                sl.events.subscribe()
                            };
                            sub_receivers.insert(sub_id, rx);
                            sub_sessions.insert(sub_id, session);

                            let response = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "subscriptionId": sub_id,
                                    "sessionId": sid,
                                    "status": "subscribed",
                                }
                            });
                            if conn.send(response).await.is_err() {
                                break;
                            }
                        }
                        None => {
                            let response = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {"code": -32000, "message": format!("session {sid} not found")},
                            });
                            if conn.send(response).await.is_err() {
                                break;
                            }
                        }
                    }
                } else {
                    // Normal request
                    if let Some(response) = handle_request(&state, &msg).await {
                        if conn.send(response).await.is_err() {
                            break;
                        }
                    }
                }
            }
            Ok(Ok(None)) => {
                // Connection closed
                break;
            }
            Ok(Err(e)) => {
                eprintln!("[{conn_id}] read error: {e}");
                break;
            }
            Err(_) => {
                // Timeout — just loop to check subscriptions
                continue;
            }
        }
    }

    // Cleanup: abort all subscription tasks
    for (_, session) in sub_sessions {
        let _ = session;
    }
}

// ── Server lifecycle ──────────────────────────────────────────────────────────

pub struct TcpServerHandle {
    pub id: String,
    pub address: String,
    pub token: String,
    pub state: Arc<AppState>,
    pub shutdown_tx: tokio::sync::watch::Sender<bool>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

impl TcpServerHandle {
    pub fn is_running(&self) -> bool {
        self.state.running.load(Ordering::Relaxed)
    }

    pub async fn stop(&self) -> Result<(), String> {
        self.state.running.store(false, Ordering::Relaxed);
        let _ = self.shutdown_tx.send(true);

        // Kill all child processes
        let sessions = self.state.sessions.lock().await;
        for (_, session) in sessions.iter() {
            let mut s = session.lock().await;
            let _ = s.child.kill().await;
            let _ = s.child.wait().await;
        }
        Ok(())
    }

    pub async fn info(&self) -> ServerInfo {
        let state_str = if self.is_running() {
            "running"
        } else {
            "stopped"
        };
        self.state.info(&self.address, state_str).await
    }
}

/// Run the TCP server — accepts connections and spawns handlers.
pub async fn run_server<T: Transport>(
    transport: Arc<T>,
    bind: &str,
    port: u16,
    token: String,
    executable: PathBuf,
    args: Vec<String>,
) -> Result<Arc<TcpServerHandle>, String> {
    let addr = format!("{bind}:{port}");
    transport.listen(&addr).await?;

    let server_id = next_server_id();
    let state = Arc::new(AppState::new(
        executable,
        args,
        token.clone(),
        server_id.clone(),
    ));
    *state.bound_address.lock().await = addr.clone();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let handle = Arc::new(TcpServerHandle {
        id: server_id,
        address: addr.clone(),
        token,
        state: Arc::clone(&state),
        shutdown_tx,
        shutdown_rx: shutdown_rx.clone(),
    });

    println!("rpi-server listening on {addr}");
    if handle.token.is_empty() {
        println!("token: (authentication disabled)");
    } else {
        println!("token: {}", handle.token);
    }

    // Accept loop
    let handle_clone = Arc::clone(&handle);
    let transport_clone = Arc::clone(&transport);
    tokio::spawn(async move {
        let mut shutdown_rx = handle_clone.shutdown_rx.clone();
        loop {
            tokio::select! {
                result = transport_clone.accept() => {
                    match result {
                        Ok(conn) => {
                            let state = Arc::clone(&handle_clone.state);
                            tokio::spawn(async move {
                                handle_connection(state, conn).await;
                            });
                        }
                        Err(e) => {
                            eprintln!("accept error: {e}");
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    break;
                }
            }
        }
    });

    Ok(handle)
}
