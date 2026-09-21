mod server;
mod transport;

use rpi_plugin_sdk::{
    register_entrypoint, EventTag, FreeStringFn, PluginApiVt, RuntimeActionId, StablePluginEvent,
    StableToolSchema, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use server::{generate_token, launch_args, run_server, TcpServerHandle};
use transport::{ManagedConnection, TcpTransport};

const DEFAULT_TIMEOUT: u64 = 30;

// ── Data ──────────────────────────────────────────────────────────────────────

static SERVERS: OnceLock<Mutex<HashMap<String, Arc<TcpServerHandle>>>> = OnceLock::new();

fn servers() -> &'static Mutex<HashMap<String, Arc<TcpServerHandle>>> {
    SERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

// Store API vtable pointer for runtime_action calls (e.g., GetCliFlag)
static API_VT: AtomicPtr<PluginApiVt> = AtomicPtr::new(std::ptr::null_mut());

fn api_vt() -> &'static PluginApiVt {
    let ptr = API_VT.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "API vtable not initialized");
    unsafe { &*ptr }
}

/// Call host's GetCliFlag runtime action to read a CLI flag value
fn get_cli_flag(name: &str) -> Option<Value> {
    let api = api_vt();
    let runtime_action = api.runtime_action;

    let args = json!({"name": name});
    let args_str = args.to_string();
    let args_ref = StbStringRef::from_str(&args_str);
    let mut out = StbString::empty();

    let rc = runtime_action(
        RuntimeActionId::GetCliFlag as u32,
        args_ref,
        &mut out,
        std::ptr::null_mut(),
    );

    if rc != 0 {
        return None;
    }

    let result_str = out.to_string_lossy();
    let result: Value = serde_json::from_str(&result_str).ok()?;
    result.get("value").cloned()
}

struct Drive {
    receiver: mpsc::Receiver<Result<String, String>>,
    cancelled: Arc<AtomicBool>,
    done: bool,
}

// ── Param parsing ─────────────────────────────────────────────────────────────

fn string_param(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(format!("{key} must not be empty")),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn timeout_param(params: &Value) -> Result<Duration, String> {
    match params.get("timeoutSeconds") {
        None | Some(Value::Null) => Ok(Duration::from_secs(DEFAULT_TIMEOUT)),
        Some(v) => v
            .as_u64()
            .filter(|v| (1..=300).contains(v))
            .map(Duration::from_secs)
            .ok_or_else(|| "timeoutSeconds must be an integer from 1 to 300".into()),
    }
}

// ── Server lifecycle ──────────────────────────────────────────────────────────

fn do_start(params: &Value) -> Result<String, String> {
    let bind_addr = optional_string(params, "bind")?.unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = match params.get("port") {
        Some(Value::Number(n)) => n.as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .ok_or("port must be an integer 0–65535")?,
        None => 9800,
        Some(_) => return Err("port must be an integer".into()),
    };

    let (executable, args) = launch_args(params)?;
    let token = optional_string(params, "token")?.unwrap_or_else(generate_token);

    let exec_clone = executable.clone();
    let args_clone = args.clone();
    let token_clone = token.clone();
    let bind_clone = bind_addr.clone();

    let (ready_tx, ready_rx) = mpsc::channel();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("failed to create runtime: {e}")));
                return;
            }
        };

        let result = runtime.block_on(async {
            let transport = Arc::new(TcpTransport::new());
            run_server(transport, &bind_clone, port, token_clone, exec_clone, args_clone).await
        });

        match result {
            Ok(server) => {
                if let Ok(mut registry) = servers().lock() {
                    registry.insert(server.id.clone(), server.clone());
                }
                let _ = ready_tx.send(Ok(server));
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        }

        runtime.block_on(async { loop { tokio::time::sleep(Duration::from_secs(3600)).await; } });
    });

    let server = match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("timed out waiting for server startup".into()),
    };

    Ok(json!({
        "id": server.id,
        "address": server.address,
        "token": token,
        "state": "running",
        "protocol": "jsonl",
        "transport": "tcp",
    }).to_string())
}

fn do_stop(id: &str) -> Result<String, String> {
    let server = servers()
        .lock()
        .map_err(|_| "server registry poisoned")?
        .remove(id)
        .ok_or("RPC server not found")?;

    let (tx, rx) = mpsc::channel();
    let server_clone = Arc::clone(&server);
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(server_clone.stop());
        let _ = tx.send(result);
    });

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(json!({"id": id, "state": "stopped"}).to_string()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("timed out waiting for server stop".into()),
    }
}

async fn server_status(server: &Arc<TcpServerHandle>) -> Result<Value, String> {
    let info = server.info().await;
    Ok(json!({
        "id": info.id,
        "address": info.address,
        "token": info.token,
        "state": info.state,
        "protocol": "jsonl",
        "transport": "tcp",
        "sessions": info.sessions,
    }))
}

async fn do_status(id: &str) -> Result<String, String> {
    let server = servers()
        .lock()
        .map_err(|_| "server registry poisoned")?
        .get(id)
        .cloned()
        .ok_or("RPC server not found")?;
    Ok(server_status(&server).await?.to_string())
}

async fn do_list() -> Result<String, String> {
    let registry = servers().lock().map_err(|_| "server registry poisoned")?;
    let mut list: Vec<Value> = Vec::new();
    for server in registry.values() {
        list.push(server_status(server).await?);
    }
    Ok(json!(list).to_string())
}

async fn server_tool(params: &Value) -> Result<String, String> {
    match params.get("action").and_then(Value::as_str).unwrap_or("start") {
        "start" => do_start(params),
        "stop" => {
            let id = string_param(params, "id")?;
            do_stop(&id)
        }
        "status" => {
            let id = string_param(params, "id")?;
            do_status(&id).await
        }
        "list" => do_list().await,
        other => Err(format!("action must be one of: start, stop, status, list (got {other})")),
    }
}

fn stop_all_servers() {
    let ids: Vec<String> = match servers().lock() {
        Ok(registry) => registry.keys().cloned().collect(),
        Err(_) => return,
    };
    for id in ids {
        let _ = do_stop(&id);
    }
}

extern "C" fn on_session_shutdown(event: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if event.tag == EventTag::SessionShutdown {
        stop_all_servers();
    }
    0
}

/// On SessionStart, check if --server CLI flag was passed and auto-start the TCP server.
extern "C" fn on_session_start(event: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if event.tag != EventTag::SessionStart {
        return 0;
    }

    // Check if --server flag was passed
    let server_flag = get_cli_flag("server");
    if server_flag.is_none() {
        return 0;
    }

    // Read optional --port flag (default 9800)
    let port: u16 = match get_cli_flag("port") {
        Some(Value::Number(n)) => n.as_u64().and_then(|n| u16::try_from(n).ok()).unwrap_or(9800),
        Some(Value::String(s)) => s.parse().unwrap_or(9800),
        _ => 9800,
    };

    // Read optional --bind flag (default 127.0.0.1)
    let bind = match get_cli_flag("bind") {
        Some(Value::String(s)) if !s.is_empty() => s,
        _ => "127.0.0.1".to_string(),
    };

    eprintln!("[rpi-server] --server detected, starting TCP server on {bind}:{port}");

    let params = json!({
        "bind": bind,
        "port": port,
    });

    match do_start(&params) {
        Ok(info) => eprintln!("[rpi-server] TCP server started: {info}"),
        Err(e) => eprintln!("[rpi-server] failed to start: {e}"),
    }

    0
}

// ── Client ────────────────────────────────────────────────────────────────────

fn client_tool(params: &Value, cancelled: &AtomicBool) -> Result<String, String> {
    let server_id = string_param(params, "serverId")?;
    let method = string_param(params, "method")?;
    let rpc_params = params.get("params").cloned().unwrap_or(json!({}));

    let server = servers()
        .lock()
        .map_err(|_| "server registry poisoned")?
        .get(&server_id)
        .cloned()
        .ok_or("RPC server not found")?;

    if !server.is_running() {
        return Err("RPC server is not running".into());
    }

    let wait = timeout_param(params)?;
    let subscribe = params.get("subscribe").and_then(Value::as_bool).unwrap_or(false);

    let (tx, rx) = mpsc::channel();
    let cancelled_clone = Arc::new(AtomicBool::new(cancelled.load(Ordering::Acquire)));
    let cancelled_inner = Arc::clone(&cancelled_clone);

    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = runtime.block_on(async {
            let transport = Arc::new(TcpTransport::new());
            let managed = ManagedConnection::new(transport);

            // Connect with retry
            managed.connect_with_retry(&server.address).await
                .map_err(|e| format!("failed to connect: {e}"))?;

            // Send request
            let request = json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": rpc_params,
                "id": 1,
            });
            managed.send(request).await
                .map_err(|e| format!("send failed: {e}"))?;

            if subscribe {
                // Collect streaming events
                let mut events = Vec::new();
                let deadline = Instant::now() + wait;

                loop {
                    if cancelled_inner.load(Ordering::Acquire) { break; }
                    if Instant::now() >= deadline { break; }

                    match tokio::time::timeout(Duration::from_millis(100), managed.recv()).await {
                        Ok(Ok(Some(event))) => {
                            // Check for session_end before pushing
                            let is_end = event.get("type").and_then(Value::as_str) == Some("session_end");
                            events.push(event);
                            if is_end {
                                break;
                            }
                        }
                        Ok(Ok(None)) => break,
                        Ok(Err(e)) => return Err(format!("recv error: {e}")),
                        Err(_) => continue, // timeout, loop again
                    }
                }

                Ok(json!({
                    "serverId": server_id,
                    "method": method,
                    "events": events,
                    "eventCount": events.len(),
                }).to_string())
            } else {
                // Wait for response
                let deadline = Instant::now() + wait;
                loop {
                    if cancelled_inner.load(Ordering::Acquire) {
                        return Err("request cancelled".into());
                    }
                    if Instant::now() >= deadline {
                        return Err("request timed out".into());
                    }

                    match tokio::time::timeout(Duration::from_millis(100), managed.recv()).await {
                        Ok(Ok(Some(response))) => {
                            return Ok(json!({
                                "serverId": server_id,
                                "method": method,
                                "result": response,
                            }).to_string());
                        }
                        Ok(Ok(None)) => return Err("connection closed".into()),
                        Ok(Err(e)) => return Err(format!("recv error: {e}")),
                        Err(_) => continue, // timeout, loop again
                    }
                }
            }
        });

        let _ = tx.send(result);
    });

    let deadline = Instant::now() + wait;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err("request cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("request timed out".into());
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("worker stopped without result".into());
            }
        }
    }
}

// ── SDK integration ───────────────────────────────────────────────────────────

extern "C" fn execute_server(
    tool_call_id: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let _ = tool_call_id;
    let text = params.to_string_lossy();
    params.free_with(free);
    let cancelled = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = serde_json::from_str::<Value>(&text)
            .map_err(|e| format!("invalid parameters: {e}"))
            .and_then(|p| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async {
                    server_tool(&p).await
                })
            });
        let _ = tx.send(result);
    });
    Box::into_raw(Box::new(Drive { receiver: rx, cancelled, done: false })) as StepHandle
}

extern "C" fn execute_client(
    tool_call_id: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let _ = tool_call_id;
    let text = params.to_string_lossy();
    params.free_with(free);
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = Arc::clone(&cancelled);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = serde_json::from_str::<Value>(&text)
            .map_err(|e| format!("invalid parameters: {e}"))
            .and_then(|p| client_tool(&p, &cancelled_clone));
        let _ = tx.send(result);
    });
    Box::into_raw(Box::new(Drive { receiver: rx, cancelled, done: false })) as StepHandle
}

extern "C" fn poll(
    handle: StepHandle,
    _: Option<ToolPartialCb>,
    _: *mut std::ffi::c_void,
) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.done {
        return StepResult::err(StbString::from_string("handle polled after completion".into()));
    }
    if drive.cancelled.load(Ordering::Acquire) {
        drive.done = true;
        return StepResult::err(StbString::from_string("request cancelled".into()));
    }
    match drive.receiver.try_recv() {
        Ok(Ok(text)) => {
            drive.done = true;
            StepResult::done(StbString::from_string(
                json!({"content": [{"type": "text", "text": text}]}).to_string(),
            ))
        }
        Ok(Err(e)) => {
            drive.done = true;
            StepResult::err(StbString::from_string(e))
        }
        Err(mpsc::TryRecvError::Empty) => StepResult::pending(StbString::empty()),
        Err(mpsc::TryRecvError::Disconnected) => {
            drive.done = true;
            StepResult::err(StbString::from_string("worker stopped without result".into()))
        }
    }
}

extern "C" fn cancel(handle: StepHandle) {
    if !handle.is_null() {
        unsafe { (&*(handle as *mut Drive)).cancelled.store(true, Ordering::Release); }
    }
}

extern "C" fn destroy(handle: StepHandle) {
    if !handle.is_null() {
        unsafe {
            let drive = Box::from_raw(handle as *mut Drive);
            drive.cancelled.store(true, Ordering::Release);
        }
    }
}

extern "C" fn free_string(s: StbString) {
    if !s.is_empty() && !s.ptr.is_null() {
        unsafe {
            let bytes = std::slice::from_raw_parts(s.ptr as *const u8, s.len);
            let _ = Box::from_raw(bytes as *const [u8] as *mut [u8]);
        }
    }
}

// ── Schemas ───────────────────────────────────────────────────────────────────

const SERVER_PARAMETERS: &str = r#"{
  "type":"object",
  "properties":{
    "action":{"type":"string","enum":["start","stop","status","list"]},
    "id":{"type":"string"},
    "bind":{"type":"string"},
    "port":{"type":"integer","minimum":0,"maximum":65535},
    "token":{"type":"string"},
    "provider":{"type":"string"},
    "model":{"type":"string"},
    "baseUrl":{"type":"string"},
    "systemPrompt":{"type":"string"},
    "appendSystemPrompt":{"type":"array","items":{"type":"string"}},
    "thinking":{"type":"string","enum":["off","minimal","low","medium","high","xhigh","max"]},
    "name":{"type":"string"},
    "session":{"type":"string"},
    "sessionId":{"type":"string"},
    "sessionDir":{"type":"string"},
    "noSession":{"type":"boolean"},
    "tools":{"type":"array","items":{"type":"string"}},
    "excludeTools":{"type":"array","items":{"type":"string"}},
    "noTools":{"type":"boolean"},
    "noBuiltinTools":{"type":"boolean"},
    "noSkills":{"type":"boolean"},
    "noPromptTemplates":{"type":"boolean"},
    "noContextFiles":{"type":"boolean"},
    "noExtensions":{"type":"boolean"},
    "enablePiPackages":{"type":"boolean"},
    "extensionsDir":{"type":"array","items":{"type":"string"}},
    "extension":{"type":"array","items":{"type":"string"}},
    "skill":{"type":"array","items":{"type":"string"}},
    "promptTemplate":{"type":"array","items":{"type":"string"}},
    "offline":{"type":"boolean"},
    "approve":{"type":"boolean"}
  }
}"#;

const CLIENT_PARAMETERS: &str = r#"{
  "type":"object",
  "properties":{
    "serverId":{"type":"string"},
    "method":{"type":"string"},
    "params":{"type":"object"},
    "subscribe":{"type":"boolean","description":"Enable streaming subscription"},
    "timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}
  },
  "required":["serverId","method"]
}"#;

// ── Plugin entry ──────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn rpi_plugin_register_v2(api: *const PluginApiVt, abi: u32) -> i32 {
    // Save API vtable for runtime_action calls (e.g., GetCliFlag)
    API_VT.store(api as *mut PluginApiVt, Ordering::Release);

    register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };

        let server_schema = Box::new(StableToolSchema {
            name: StbString::from_string("rpc_server".into()),
            description: StbString::from_string(
                "Start and manage a TCP JSONL server with streaming subscriptions.".into(),
            ),
            parameters: StbString::from_string(SERVER_PARAMETERS.into()),
        });

        let client_schema = Box::new(StableToolSchema {
            name: StbString::from_string("rpc_client".into()),
            description: StbString::from_string(
                "Send a JSON-RPC request to a running TCP server. Set subscribe=true for streaming responses.".into(),
            ),
            parameters: StbString::from_string(CLIENT_PARAMETERS.into()),
        });

        let first = register(
            &*server_schema,
            execute_server,
            poll,
            cancel,
            destroy,
            free_string,
        );

        let second = if first == 0 {
            register(
                &*client_schema,
                execute_client,
                poll,
                cancel,
                destroy,
                free_string,
            )
        } else {
            first
        };

        let third = if second == 0 {
            api.register_event_handler
                .map(|register| {
                    register(
                        EventTag::SessionShutdown,
                        on_session_shutdown,
                        std::ptr::null_mut(),
                    )
                })
                .unwrap_or(0)
        } else {
            second
        };

        // Register SessionStart handler to auto-start server when --server flag is passed
        let fourth = if third == 0 {
            api.register_event_handler
                .map(|register| {
                    register(
                        EventTag::SessionStart,
                        on_session_start,
                        std::ptr::null_mut(),
                    )
                })
                .unwrap_or(0)
        } else {
            third
        };

        drop(server_schema);
        drop(client_schema);
        fourth
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_args_basic() {
        let (_, args) = launch_args(&json!({
            "provider": "anthropic",
            "model": "claude-3",
            "noSession": true,
        }))
        .unwrap();
        assert_eq!(&args[0..2], ["--mode", "rpc"]);
        assert!(args.windows(2).any(|w| w == ["--provider", "anthropic"]));
        assert!(args.windows(2).any(|w| w == ["--model", "claude-3"]));
        assert!(args.contains(&"--no-session".to_string()));
    }

    #[test]
    fn schemas_are_valid_json() {
        assert!(serde_json::from_str::<Value>(SERVER_PARAMETERS).is_ok());
        assert!(serde_json::from_str::<Value>(CLIENT_PARAMETERS).is_ok());
    }

    #[test]
    fn generate_token_is_32_hex_chars() {
        let t = generate_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
