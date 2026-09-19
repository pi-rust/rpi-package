use jsonrpsee::server::{RpcModule, Server, ServerHandle};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
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
                Ok(value) => { let _ = events_tx.send(value); }
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
    let Some(v) = params.get(key) else { return Ok(Vec::new()); };
    let arr = v.as_array().ok_or_else(|| format!("{key} must be an array of strings"))?;
    if arr.len() > 128 { return Err(format!("{key} contains too many values")); }
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

fn push_option(args: &mut Vec<String>, params: &Value, key: &str, flag: &str) -> Result<(), String> {
    if let Some(v) = optional_string(params, key)? {
        args.push(flag.into());
        args.push(v);
    }
    Ok(())
}

fn push_switch(args: &mut Vec<String>, params: &Value, key: &str, flag: &str) -> Result<(), String> {
    if bool_param(params, key)? { args.push(flag.into()); }
    Ok(())
}

fn push_repeated(args: &mut Vec<String>, params: &Value, key: &str, flag: &str) -> Result<(), String> {
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

// ── Error helper ──────────────────────────────────────────────────────────────

fn rpc_err(code: i32, msg: &str) -> jsonrpsee::types::ErrorObjectOwned {
    jsonrpsee::types::ErrorObjectOwned::owned(code, msg.to_string(), None::<()>)
}

// ── RPC Module Builder ────────────────────────────────────────────────────────

pub async fn build_rpc_module(state: Arc<AppState>) -> Result<RpcModule<Arc<AppState>>, String> {
    let mut module = RpcModule::new(state);

    // ── start_session ─────────────────────────────────────────────────────
    module.register_async_method("start_session", |_params, ctx, _ext| async move {
        let session_id = next_session_id();
        let (events_tx, _) = broadcast::channel(256);
        let (child, stdin) = spawn_child(&ctx.executable, &ctx.default_args, events_tx.clone())
            .map_err(|e| rpc_err(-32000, &e))?;
        let session = Arc::new(tokio::sync::Mutex::new(Session {
            id: session_id.clone(), child, stdin, events: events_tx, event_count: 0, state: "running".into(),
        }));
        ctx.sessions.lock().await
            .insert(session_id.clone(), Arc::clone(&session));
        let info = session.lock().await.info();
        Ok::<Value, jsonrpsee::types::ErrorObjectOwned>(json!({"sessionId": session_id, "session": info}))
    }).map_err(|e| e.to_string())?;

    // ── send ──────────────────────────────────────────────────────────────
    module.register_async_method("send", |params, ctx, _ext| async move {
        let pv: Value = params.parse().unwrap_or(json!({}));
        let sid = pv.get("sessionId").and_then(Value::as_str)
            .ok_or_else(|| rpc_err(-32602, "sessionId is required"))?.to_string();
        let cmd = pv.get("command").ok_or_else(|| rpc_err(-32602, "command is required"))?;
        let session = {
            let sessions = ctx.sessions.lock().await;
            sessions.get(&sid)
                .ok_or_else(|| rpc_err(-32000, &format!("session {sid} not found")))?.clone()
        };
        let mut line = serde_json::to_string(cmd).map_err(|e| rpc_err(-32000, &e.to_string()))?;
        line.push('\n');
        let mut sl = session.lock().await;
        sl.stdin.write_all(line.as_bytes()).await.map_err(|e| rpc_err(-32000, &format!("write: {e}")))?;
        sl.stdin.flush().await.map_err(|e| rpc_err(-32000, &format!("flush: {e}")))?;
        Ok::<Value, jsonrpsee::types::ErrorObjectOwned>(json!({"sessionId": sid, "status": "sent"}))
    }).map_err(|e| e.to_string())?;

    // ── stop_session ──────────────────────────────────────────────────────
    module.register_async_method("stop_session", |params, ctx, _ext| async move {
        let pv: Value = params.parse().unwrap_or(json!({}));
        let sid = pv.get("sessionId").and_then(Value::as_str)
            .ok_or_else(|| rpc_err(-32602, "sessionId is required"))?.to_string();
        let session = {
            let mut sessions = ctx.sessions.lock().await;
            sessions.remove(&sid).ok_or_else(|| rpc_err(-32000, &format!("session {sid} not found")))?
        };
        let mut sl = session.lock().await;
        sl.state = "stopped".into();
        let _ = sl.child.kill().await;
        let _ = sl.child.wait().await;
        Ok::<Value, jsonrpsee::types::ErrorObjectOwned>(json!({"sessionId": sid, "status": "stopped"}))
    }).map_err(|e| e.to_string())?;

    // ── list_sessions ─────────────────────────────────────────────────────
    module.register_async_method("list_sessions", |_params, ctx, _ext| async move {
        let sessions = ctx.sessions.lock().await;
        let mut list = Vec::new();
        for s in sessions.values() {
            list.push(s.lock().await.info());
        }
        Ok::<Value, jsonrpsee::types::ErrorObjectOwned>(json!(list))
    }).map_err(|e| e.to_string())?;

    // ── server_status ─────────────────────────────────────────────────────
    module.register_async_method("server_status", |_params, ctx, _ext| async move {
        let address = ctx.bound_address.lock().await.clone();
        let info = ctx.info(&address, "running").await;
        Ok::<Value, jsonrpsee::types::ErrorObjectOwned>(json!(info))
    }).map_err(|e| e.to_string())?;

    // ── Subscription: subscribe (streaming events) ────────────────────────
    // jsonrpsee 0.24 register_subscription API:
    // register_subscription(subscribe_method, notif_method, unsubscribe_method, callback)
    // callback: Fn(Params<'static>, PendingSubscriptionSink, Arc<Context>, Extensions) -> Fut
    module.register_subscription(
        "subscribe",
        "subscribe",
        "unsubscribe",
        |params: jsonrpsee::types::Params<'static>, pending, ctx, _ext| async move {
            let pv: Value = params.parse().unwrap_or(json!({}));
            let sid = pv.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string();
            let session = {
                let sessions = ctx.sessions.lock().await;
                match sessions.get(&sid) {
                    Some(s) => s.clone(),
                    None => {
                        let _ = pending.reject(rpc_err(-32000, &format!("session {sid} not found"))).await;
                        return Ok(());
                    }
                }
            };
            let sink = match pending.accept().await {
                Ok(s) => s,
                Err(_) => return Ok(()),
            };
            let mut rx = {
                let sl = session.lock().await;
                sl.events.subscribe()
            };
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        {
                            let mut s = session.lock().await;
                            s.event_count += 1;
                        }
                        let msg = jsonrpsee::server::SubscriptionMessage::from_json(&event)
                            .map_err(|e| rpc_err(-32000, &format!("serialize: {e}")))?;
                        if sink.send(msg).await.is_err() {
                            break;
                        }
                        if event.get("type").and_then(Value::as_str) == Some("session_end") {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        let lag = json!({"type": "lagged", "skipped": n});
                        let msg = jsonrpsee::server::SubscriptionMessage::from_json(&lag)
                            .map_err(|e| rpc_err(-32000, &format!("serialize: {e}")))?;
                        if sink.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            Ok(())
        },
    ).map_err(|e| e.to_string())?;

    Ok(module)
}

// ── Server lifecycle ──────────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct RpcServerHandle {
    pub id: String,
    pub address: String,
    pub token: String,
    pub handle: tokio::sync::Mutex<Option<ServerHandle>>,
    pub state: Arc<AppState>,
}

impl RpcServerHandle {
    pub fn is_running(&self) -> bool {
        self.handle.try_lock().map(|h| h.is_some()).unwrap_or(false)
    }

    pub async fn stop(&self) -> Result<(), String> {
        // Kill all child processes first
        {
            let sessions = self.state.sessions.lock().await;
            for (_, session) in sessions.iter() {
                let mut s = session.lock().await;
                let _ = s.child.kill().await;
                let _ = s.child.wait().await;
            }
        }
        if let Some(h) = self.handle.lock().await.take() {
            let _ = h.stop();
        }
        Ok(())
    }

    pub async fn info(&self) -> ServerInfo {
        let state_str = if self.is_running() { "running" } else { "stopped" };
        self.state.info(&self.address, state_str).await
    }
}

pub async fn start_rpc_server(
    bind: &str,
    port: u16,
    token: String,
    executable: PathBuf,
    args: Vec<String>,
) -> Result<Arc<RpcServerHandle>, String> {
    let addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .map_err(|e| format!("invalid bind address: {e}"))?;

    let server_id = next_server_id();
    let state = Arc::new(AppState::new(executable, args, token.clone(), server_id.clone()));
    let module = build_rpc_module(Arc::clone(&state)).await?;

    let server = Server::builder()
        .build(addr)
        .await
        .map_err(|e| format!("failed to start server: {e}"))?;

    let actual_addr = server.local_addr().map_err(|e| format!("failed to get address: {e}"))?;
    *state.bound_address.lock().await = actual_addr.to_string();
    let handle = server.start(module);

    println!("rpi-server listening on ws://{actual_addr}");
    println!("token: {token}");

    Ok(Arc::new(RpcServerHandle {
        id: server_id,
        address: actual_addr.to_string(),
        token,
        handle: tokio::sync::Mutex::new(Some(handle)),
        state,
    }))
}
