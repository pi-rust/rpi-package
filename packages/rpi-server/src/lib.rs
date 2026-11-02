#![forbid(unsafe_code)]

mod server;
mod transport;

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    EventTag, FreeStringFn, RuntimeActionFn, RuntimeActionId, StablePluginEvent, StableToolSchema,
    StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use server::{generate_token, launch_args, run_server, TcpServerHandle};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use transport::{ManagedConnection, TcpTransport};

const DEFAULT_TIMEOUT: u64 = 30;

// ── Data ──────────────────────────────────────────────────────────────────────

static SERVERS: OnceLock<Mutex<HashMap<String, Arc<TcpServerHandle>>>> = OnceLock::new();

fn servers() -> &'static Mutex<HashMap<String, Arc<TcpServerHandle>>> {
    SERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

// The host's `runtime_action` entry point + its opaque `user_data` (the host's
// `ActionBridge`), copied OUT of the vtable during `register`. The vtable is the
// host's to own — we must not retain the `api` pointer (that is a
// use-after-free: the host builds it on the register stack). `GetCliFlag` needs
// both the entry point and the `user_data`.
static RUNTIME_ACTION: OnceLock<RuntimeActionFn> = OnceLock::new();
static RUNTIME_USER_DATA: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
// The host's `free_string`, for reclaiming host-produced `out` strings.
static HOST_FREE_STRING: OnceLock<FreeStringFn> = OnceLock::new();

/// Call the host's `GetCliFlag` runtime action to read a parsed CLI flag value.
fn get_cli_flag(name: &str) -> Option<Value> {
    let runtime_action = *RUNTIME_ACTION.get()?;
    let user_data = RUNTIME_USER_DATA.load(Ordering::Acquire);
    if user_data.is_null() {
        return None;
    }

    let args = json!({"name": name});
    let args_str = args.to_string();
    let args_ref = StbStringRef::from_str(&args_str);
    let mut out = StbString::empty();

    let rc = runtime_action(
        RuntimeActionId::GetCliFlag as u32,
        args_ref,
        &mut out,
        user_data,
    );

    // The host may have produced `out` even on an error path; reclaim it via the
    // host's free so we don't leak the host allocation.
    let result_str = if rc == 0 {
        Some(out.to_string_lossy())
    } else {
        None
    };
    if let Some(free) = HOST_FREE_STRING.get() {
        free(out);
    }

    let result: Value = serde_json::from_str(&result_str?).ok()?;
    result.get("value").cloned()
}

static HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    receiver: mpsc::Receiver<Result<String, String>>,
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
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .ok_or("port must be an integer 0–65535")?,
        None => 9800,
        Some(_) => return Err("port must be an integer".into()),
    };

    let (executable, args) = launch_args(params)?;
    let no_token = params
        .get("noToken")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let token = if no_token {
        // Authentication disabled: any client may connect (trusted local use).
        String::new()
    } else {
        params
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(generate_token)
    };

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
            run_server(
                transport,
                &bind_clone,
                port,
                token_clone,
                exec_clone,
                args_clone,
            )
            .await
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

        runtime.block_on(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
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
    })
    .to_string())
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
    match params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("start")
    {
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
        other => Err(format!(
            "action must be one of: start, stop, status, list (got {other})"
        )),
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

/// Whether a `GetCliFlag` result means the switch is ON. `GetCliFlag` yields
/// `true` for a bare `--flag`, a string when `--flag=v`, and `null` when unset.
fn flag_is_on(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty() && !s.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

/// On SessionStart, check if --server CLI flag was passed and auto-start the TCP server.
extern "C" fn on_session_start(event: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if event.tag != EventTag::SessionStart {
        return 0;
    }

    // Check if --server flag was passed. `GetCliFlag` returns `{"value": null}`
    // for an *unset* flag, so a bare `is_none()` would wrongly treat that as
    // present — require an explicit truthy value.
    if !flag_is_on(get_cli_flag("server").as_ref()) {
        return 0;
    }

    // Read optional --port flag (default 9800)
    let port: u16 = match get_cli_flag("port") {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(9800),
        Some(Value::String(s)) => s.parse().unwrap_or(9800),
        _ => 9800,
    };

    // Read optional --bind flag (default 127.0.0.1)
    let bind = match get_cli_flag("bind") {
        Some(Value::String(s)) if !s.is_empty() => s,
        _ => "127.0.0.1".to_string(),
    };

    eprintln!("[rpi-server] --server detected, starting TCP server on {bind}:{port}");

    let token = match get_cli_flag("token") {
        Some(Value::String(s)) if !s.is_empty() => Some(s),
        _ => None,
    };
    let no_token = flag_is_on(get_cli_flag("no-token").as_ref());

    let params = json!({
        "bind": bind,
        "port": port,
        "token": token,
        "noToken": no_token,
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
    let subscribe = params
        .get("subscribe")
        .and_then(Value::as_bool)
        .unwrap_or(false);

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
            managed
                .connect_with_retry(&server.address)
                .await
                .map_err(|e| format!("failed to connect: {e}"))?;

            // Authenticate when the server requires a token.
            if !server.token.is_empty() {
                let auth = json!({
                    "jsonrpc": "2.0",
                    "id": 0,
                    "method": "authenticate",
                    "params": {"token": server.token},
                });
                managed
                    .send(auth)
                    .await
                    .map_err(|e| format!("auth send failed: {e}"))?;
                match tokio::time::timeout(Duration::from_secs(5), managed.recv()).await {
                    Ok(Ok(Some(response))) => {
                        if let Some(error) = response.get("error") {
                            return Err(format!("authentication failed: {error}"));
                        }
                    }
                    Ok(Ok(None)) => return Err("authentication failed: connection closed".into()),
                    Ok(Err(e)) => return Err(format!("authentication failed: {e}")),
                    Err(_) => return Err("authentication failed: timed out".into()),
                }
            }

            // Send request
            let request = json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": rpc_params,
                "id": 1,
            });
            managed
                .send(request)
                .await
                .map_err(|e| format!("send failed: {e}"))?;

            if subscribe {
                // Collect streaming events
                let mut events = Vec::new();
                let deadline = Instant::now() + wait;

                loop {
                    if cancelled_inner.load(Ordering::Acquire) {
                        break;
                    }
                    if Instant::now() >= deadline {
                        break;
                    }

                    match tokio::time::timeout(Duration::from_millis(100), managed.recv()).await {
                        Ok(Ok(Some(event))) => {
                            // Check for session_end before pushing
                            let is_end =
                                event.get("type").and_then(Value::as_str) == Some("session_end");
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
                })
                .to_string())
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
                            })
                            .to_string());
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
                runtime.block_on(async { server_tool(&p).await })
            });
        let _ = tx.send(result);
    });
    HANDLES.insert_with_cancel(
        Drive {
            receiver: rx,
            done: false,
        },
        cancelled,
    )
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
    HANDLES.insert_with_cancel(
        Drive {
            receiver: rx,
            done: false,
        },
        cancelled,
    )
}

extern "C" fn poll(
    handle: StepHandle,
    _: Option<ToolPartialCb>,
    _: *mut std::ffi::c_void,
) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    if drive.done {
        return StepResult::err(StbString::from_string(
            "handle polled after completion".into(),
        ));
    }
    if entry.cancelled.load(Ordering::Acquire) {
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
            StepResult::err(StbString::from_string(
                "worker stopped without result".into(),
            ))
        }
    }
}

extern "C" fn cancel(handle: StepHandle) {
    HANDLES.cancel(handle);
}

extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

use rpi_plugin_sdk::callbacks::free_string;

// ── Schemas ───────────────────────────────────────────────────────────────────

const SERVER_DESCRIPTION: &str = "Manage extension-local TCP JSONL listeners, not HTTP. Default start opens a listener and stores child launch defaults; status/stop require id. Child sessions start separately through rpc_client. Default is loopback with token authentication; non-loopback binds expose session control over unencrypted TCP. Results/logs contain secret tokens: do not publish them. Stop requests listener shutdown and terminates tracked children; session shutdown attempts cleanup. Timeout/cancellation does not undo side effects; inspect state before retrying. Explicitly stop sessions/listeners you create.";
const CLIENT_DESCRIPTION: &str = "Send JSON-RPC to a running server using its extension-local serverId; authentication is automatic. start_session creates a child, send writes a native command, stop_session terminates a child. method=subscribe with subscribe=true collects live events, not history. Results contain raw JSON-RPC responses: inspect nested errors even when the tool succeeds. Timeout/cancellation does not cancel remote work or imply rollback: do not blindly retry side-effecting methods. Clean up sessions with stop_session and listeners with rpc_server stop. Responses/events may contain secrets; redact before sharing.";

const SERVER_PARAMETERS: &str = r#"{
  "type":"object",
  "properties":{
    "action":{"type":"string","enum":["start","stop","status","list"],"default":"start","description":"Optional; defaults to start. start opens a listener; status reads one server; stop removes/stops one server and kills tracked children; list returns all registered server snapshots. Only start reads launch/network fields; stop/status require id."},
    "id":{"type":"string","description":"Non-blank extension-local server id returned by start/list; required only for stop/status. Not a child session id. No default; ignored for start/list."},
    "bind":{"type":"string","default":"127.0.0.1","description":"Start only. Non-blank TCP bind host; omitted/null defaults to 127.0.0.1. 0.0.0.0 exposes all IPv4 interfaces; TCP has no TLS. Prefer loopback; not an HTTP URL."},
    "port":{"type":"integer","minimum":0,"maximum":65535,"default":9800,"description":"Start only. TCP port, default 9800; 0 requests an OS-assigned port, but returned/stored address currently retains :0 and cannot be used by rpc_client to discover that port. Use a known available port for client calls. null is invalid."},
    "token":{"type":"string","description":"Start only. Secret authentication token; omitted/empty generates one. noToken=true overrides it. Included in status/results and server logs: do not echo, commit or share. Authentication does not encrypt TCP traffic."},
    "noToken":{"type":"boolean","default":false,"description":"Start only. Default false requires connection authentication; true disables it and ignores token, allowing any reachable client to control sessions/children. Use only on trusted local networks, preferably loopback."},
    "provider":{"type":"string","description":"Start only. Non-blank value forwarded as --provider to future children. Omitted/null: no flag, child CLI/config defaults apply (not necessarily the parent session's selection)."},
    "model":{"type":"string","description":"Start only. Non-blank value forwarded as --model; omitted/null: child CLI/config default. Does not change existing children."},
    "baseUrl":{"type":"string","description":"Start only. Non-blank provider endpoint forwarded as --base-url; omitted/null: child default. Not the TCP listener address. Only use trusted endpoints; child provider traffic may carry credentials/content."},
    "systemPrompt":{"type":"string","description":"Start only. Non-blank value forwarded verbatim as --system-prompt; omitted/null: no flag. Native CLI interprets it; no prompt expansion API is provided here. Values appear in child process arguments; avoid secrets."},
    "appendSystemPrompt":{"type":"array","maxItems":128,"items":{"type":"string","minLength":1,"description":"Non-empty native CLI value; forwarded verbatim."},"description":"Start only. Default [] (omit); up to 128 non-empty strings, each forwarded as --append-system-prompt. null is invalid. Stored for future children, not sent to existing sessions."},
    "thinking":{"type":"string","enum":["off","minimal","low","medium","high","xhigh","max"],"description":"Start only. Child thinking level forwarded as --thinking; omitted/null uses the child default."},
    "rpcMessageUpdates":{"type":"string","enum":["snapshot","compact-deltas"],"description":"Start only. Native RPC delta payload mode, forwarded as --rpc-message-updates=<mode> to future children. Omit to use the executable default and remain compatible with older hosts; null and any other value are invalid. Use snapshot only for legacy consumers; compact-deltas consumers must handle deltas and complete boundary messages."},
    "name":{"type":"string","description":"Start only. Non-blank child name forwarded as --name; omitted/null: no flag. Does not name the listener or select its registry id."},
    "session":{"type":"string","description":"Start only. Non-blank native session selector forwarded as --session; omitted/null emits no flag. Key presence conflicts with sessionId and with noSession=true, even if null. Not the server-generated session-N handle."},
    "sessionId":{"type":"string","description":"Start only. Non-blank native session id forwarded as --session-id; omitted/null emits no flag. Key presence conflicts with session and with noSession=true, even if null. Not rpc_client params.sessionId, which addresses a live child."},
    "sessionDir":{"type":"string","description":"Start only. Non-blank native session storage path forwarded as --session-dir; omitted/null: child default. Child may read/write files with host privileges; no sandbox or path validation is added."},
    "noSession":{"type":"boolean","default":false,"description":"Start only. true emits --no-session (disable native session persistence); omitted/null/false emits nothing. true conflicts with presence of session/sessionId. Does not disable creation of live RPC child sessions."},
    "tools":{"type":"array","maxItems":128,"items":{"type":"string","minLength":1,"description":"Non-empty native tool selector; avoid commas unless intended as CLI separators."},"description":"Start only. Default []; up to 128 non-empty strings joined by commas as one --tools value when nonempty. Native CLI resolves tool availability and conflicts; null invalid."},
    "excludeTools":{"type":"array","maxItems":128,"items":{"type":"string","minLength":1,"description":"Non-empty native tool selector; comma-separated CLI semantics apply."},"description":"Start only. Default []; up to 128 non-empty strings joined by commas as --exclude-tools when nonempty. Excludes child tools; null invalid; no server-side precedence resolution."},
    "noTools":{"type":"boolean","default":false,"description":"Start only. true forwards --no-tools; omitted/null/false emits nothing. Native CLI controls resulting tool set; does not restrict TCP management methods."},
    "noBuiltinTools":{"type":"boolean","default":false,"description":"Start only. true forwards --no-builtin-tools; omitted/null/false emits nothing. Child CLI determines interaction with tools/noTools."},
    "noSkills":{"type":"boolean","default":false,"description":"Start only. true forwards --no-skills; omitted/null/false emits nothing. Child CLI determines resource discovery and explicit skill handling."},
    "noPromptTemplates":{"type":"boolean","default":false,"description":"Start only. true forwards --no-prompt-templates; omitted/null/false emits nothing. Child CLI determines template loading."},
    "noContextFiles":{"type":"boolean","default":false,"description":"Start only. true forwards --no-context-files; omitted/null/false emits nothing. Child CLI determines context file loading; not filesystem isolation."},
    "noExtensions":{"type":"boolean","default":false,"description":"Start only. true forwards --no-extensions; omitted/null/false emits nothing. Child CLI resolves discovery and explicit extension options; not a sandbox."},
    "enablePiPackages":{"type":"boolean","default":false,"description":"Start only. true forwards --enable-pi-packages; omitted/null/false emits nothing (child defaults apply). Enable only trusted package sources; packages may execute code."},
    "extensionsDir":{"type":"array","maxItems":128,"items":{"type":"string","minLength":1,"description":"Non-empty trusted directory path, passed verbatim without validation."},"description":"Start only. Default []; up to 128 non-empty values, repeated --extensions-dir. null invalid. Child CLI loads resources with host privileges."},
    "extension":{"type":"array","maxItems":128,"items":{"type":"string","minLength":1,"description":"Non-empty trusted native extension specifier, passed verbatim."},"description":"Start only. Default []; up to 128 values, repeated --extension; null invalid. Extensions can execute code; no server-side validation or isolation."},
    "skill":{"type":"array","maxItems":128,"items":{"type":"string","minLength":1,"description":"Non-empty native skill resource specifier, passed verbatim."},"description":"Start only. Default []; up to 128 values, repeated --skill; null invalid. Child CLI resolves resources; only load trusted content."},
    "promptTemplate":{"type":"array","maxItems":128,"items":{"type":"string","minLength":1,"description":"Non-empty native prompt-template resource specifier, passed verbatim."},"description":"Start only. Default []; up to 128 values, repeated --prompt-template; null invalid. No server prompt API or hook; native child resolves templates."},
    "offline":{"type":"boolean","default":false,"description":"Start only. true forwards --offline; omitted/null/false emits nothing. Native CLI defines offline behavior; this is not a network firewall and does not disable the TCP listener."},
    "headless":{"type":"boolean","default":false,"description":"Start only. true forwards --headless; omitted/null/false emits nothing. Native CLI determines headless behavior; server itself always uses TCP JSONL."},
    "approve":{"type":"boolean","description":"Start only. Omitted: no approval flag, child CLI/config default applies. true emits --approve (automatic approval, potentially dangerous); false emits --no-approve. null invalid. Decide explicitly for unattended children: enabled tools/extensions may modify files or run commands with host privileges."}
  }
}"#;

const CLIENT_PARAMETERS: &str = r#"{
  "type":"object",
  "properties":{
    "serverId":{"type":"string","description":"Required non-blank id from rpc_server start/list in this extension process. Must still be running. Not a TCP address or child session id; no default. Connection and token authentication use its stored metadata."},
    "method":{"type":"string","description":"Required non-blank JSON-RPC method, no default. start_session spawns current executable using server launch defaults (params ignored); send writes params.command to child stdin (acknowledgement is not completion); stop_session kills/waits for a child. list_sessions, server_status, ping read state (server_status includes secret token). subscribe requires params.sessionId; use subscribe=true to collect events. Unknown methods return nested JSON-RPC errors, not necessarily tool errors."},
    "params":{"type":"object","default":{},"description":"Method-specific payload, default {}. send requires string sessionId (live child handle returned by start_session) and command (native JSON command); stop_session/subscribe require sessionId. start_session/list_sessions/server_status/ping ignore payload. Forwarded without command validation; send may cause file/process/network side effects under child privileges. Do not put the server token here; authentication is automatic."},
    "subscribe":{"type":"boolean","default":false,"description":"Default false returns the first full JSON-RPC response under result; inspect its error field. true collects responses/live notifications under events/eventCount. Use with method=subscribe and params.sessionId; this flag alone does not subscribe. No history/replay or completeness guarantee. Collection is time-limited, not count/byte-limited; timeout may return an error rather than partial events."},
    "timeoutSeconds":{"type":"integer","minimum":1,"maximum":300,"default":30,"description":"Wait budget in seconds, default 30 (omitted/null), range 1..300. Timeout/cancellation does not cancel or roll back remote commands or terminate children. Check state before retrying side-effecting methods."}
  },
  "required":["serverId","method"]
}"#;

// Shared by registration and tests: tests inspect exactly the schemas we register.
fn tool_schemas() -> [StableToolSchema; 2] {
    [
        StableToolSchema {
            name: StbString::from_string("rpc_server".into()),
            description: StbString::from_string(SERVER_DESCRIPTION.into()),
            parameters: StbString::from_string(SERVER_PARAMETERS.into()),
        },
        StableToolSchema {
            name: StbString::from_string("rpc_client".into()),
            description: StbString::from_string(CLIENT_DESCRIPTION.into()),
            parameters: StbString::from_string(CLIENT_PARAMETERS.into()),
        },
    ]
}

// ── Plugin entry ──────────────────────────────────────────────────────────────
rpi_plugin_sdk::export_plugin!(|api| {
    // Copy the host function pointers/handles OUT of the vtable during
    // register — the `api` pointer is only valid for this call.
    let _ = RUNTIME_ACTION.set(api.runtime_action);
    RUNTIME_USER_DATA.store(api.user_data, Ordering::Release);
    let _ = HOST_FREE_STRING.set(api.free_string);
    let Some(register) = api.register_tool else {
        return 1;
    };

    let [server_schema, client_schema] = tool_schemas().map(Box::new);

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
});

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
    fn rpc_message_updates_is_explicit_and_strict() {
        let (_, args) = launch_args(&json!({})).unwrap();
        assert!(!args
            .iter()
            .any(|arg| arg.starts_with("--rpc-message-updates")));
        for mode in ["snapshot", "compact-deltas"] {
            let (_, args) = launch_args(&json!({"rpcMessageUpdates":mode})).unwrap();
            assert_eq!(
                args,
                vec!["--mode", "rpc", &format!("--rpc-message-updates={mode}")]
            );
        }
        for invalid in [
            json!(""),
            json!("compact"),
            json!("SNAPSHOT"),
            json!(null),
            json!(true),
            json!(1),
            json!([]),
            json!({}),
        ] {
            assert_eq!(
                launch_args(&json!({"rpcMessageUpdates":invalid})).unwrap_err(),
                "rpcMessageUpdates must be snapshot or compact-deltas"
            );
        }
        let schema: Value = serde_json::from_str(SERVER_PARAMETERS).unwrap();
        assert_eq!(
            schema["properties"]["rpcMessageUpdates"]["enum"],
            json!(["snapshot", "compact-deltas"])
        );
        assert!(schema["properties"]["rpcMessageUpdates"]
            .get("default")
            .is_none());
    }

    #[test]
    fn schemas_are_valid_json() {
        assert!(serde_json::from_str::<Value>(SERVER_PARAMETERS).is_ok());
        assert!(serde_json::from_str::<Value>(CLIENT_PARAMETERS).is_ok());
    }

    #[test]
    fn registered_schemas_are_documented_and_keep_optional_fields() {
        let schemas = tool_schemas();
        for (schema, expected_name) in schemas.iter().zip(["rpc_server", "rpc_client"]) {
            assert_eq!(schema.name.to_string_lossy(), expected_name);
            assert!(!schema.description.to_string_lossy().trim().is_empty());
            let parameters: Value =
                serde_json::from_str(&schema.parameters.to_string_lossy()).unwrap();
            assert_eq!(parameters["type"], "object");
            // Do not close the object: host-injected optional __rpi must remain accepted.
            assert_ne!(parameters["additionalProperties"], false);
            for (name, field) in parameters["properties"].as_object().unwrap() {
                assert!(
                    field["description"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty()),
                    "{expected_name}.{name} needs guidance"
                );
                if field["type"] == "array" {
                    assert_eq!(field["maxItems"], 128);
                    assert_eq!(field["items"]["minLength"], 1);
                    assert!(field["items"]["description"].is_string());
                }
            }
            if expected_name == "rpc_server" {
                assert!(parameters.get("required").is_none());
                assert_eq!(parameters["properties"]["action"]["default"], "start");
                assert_eq!(
                    parameters["properties"]["action"]["enum"],
                    json!(["start", "stop", "status", "list"])
                );
            } else {
                assert_eq!(parameters["required"], json!(["serverId", "method"]));
            }
        }
        for schema in schemas {
            free_string(schema.name);
            free_string(schema.description);
            free_string(schema.parameters);
        }
    }

    #[test]
    fn registered_schemas_keep_defaults_and_limits() {
        let [server, client] = tool_schemas();
        let s: Value = serde_json::from_str(&server.parameters.to_string_lossy()).unwrap();
        let c: Value = serde_json::from_str(&client.parameters.to_string_lossy()).unwrap();
        let sp = &s["properties"];
        let cp = &c["properties"];
        assert_eq!(sp["bind"]["default"], "127.0.0.1");
        assert_eq!(sp["port"]["default"], 9800);
        assert_eq!(sp["port"]["minimum"], 0);
        assert_eq!(sp["port"]["maximum"], 65535);
        assert_eq!(sp["noToken"]["default"], false);
        assert!(sp["approve"].get("default").is_none());
        assert_eq!(cp["params"]["default"], json!({}));
        assert_eq!(cp["subscribe"]["default"], false);
        assert_eq!(cp["timeoutSeconds"]["default"], 30);
        assert_eq!(cp["timeoutSeconds"]["minimum"], 1);
        assert_eq!(cp["timeoutSeconds"]["maximum"], 300);
        assert_eq!(sp["headless"]["default"], false);
        assert_eq!(
            sp["thinking"]["enum"],
            json!(["off", "minimal", "low", "medium", "high", "xhigh", "max"])
        );
        for schema in [server, client] {
            free_string(schema.name);
            free_string(schema.description);
            free_string(schema.parameters);
        }
    }

    #[test]
    fn launch_options_keep_approval_and_session_conditions() {
        let (_, defaults) = launch_args(&json!({})).unwrap();
        assert!(!defaults
            .iter()
            .any(|arg| arg == "--approve" || arg == "--no-approve"));
        for (value, flag) in [(true, "--approve"), (false, "--no-approve")] {
            let (_, args) = launch_args(&json!({"approve": value})).unwrap();
            assert!(args.iter().any(|arg| arg == flag));
        }
        assert!(launch_args(&json!({"approve": null})).is_err());
        for params in [
            json!({"session": "s", "sessionId": "id"}),
            json!({"session": null, "sessionId": null}),
            json!({"noSession": true, "session": null}),
            json!({"noSession": true, "sessionId": null}),
        ] {
            assert!(launch_args(&params).is_err(), "{params}");
        }
    }

    #[test]
    fn generate_token_is_32_hex_chars() {
        let t = generate_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
