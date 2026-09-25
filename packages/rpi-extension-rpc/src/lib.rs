use rpi_plugin_sdk::{
    register_entrypoint_unified, EventTag, FreeStringFn, PluginApi, StablePluginEvent, StableToolSchema,
    StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const MAX_BYTES: usize = 1_048_576;
const DEFAULT_TIMEOUT: u64 = 30;

struct Drive {
    receiver: Receiver<Result<String, String>>,
    cancelled: Arc<AtomicBool>,
    done: bool,
}

#[derive(Clone, Copy)]
enum Kind {
    Client,
    Server,
}

struct RpcProcess {
    child: Child,
    stdin: ChildStdin,
    records: Receiver<Result<Value, String>>,
    stderr: Arc<Mutex<String>>,
}

struct RpcServer {
    id: String,
    executable: String,
    args: Vec<String>,
    process: Mutex<RpcProcess>,
}

static SERVERS: OnceLock<Mutex<HashMap<String, Arc<RpcServer>>>> = OnceLock::new();
static NEXT_SERVER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn servers() -> &'static Mutex<HashMap<String, Arc<RpcServer>>> {
    SERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn string_param(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Err(format!("{key} must not be empty")),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn string_array(params: &Value, key: &str) -> Result<Vec<String>, String> {
    let Some(value) = params.get(key) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of strings"))?;
    if values.len() > 128 {
        return Err(format!("{key} contains too many values"));
    }
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{key} must contain non-empty strings"))
        })
        .collect()
}

fn bool_param(params: &Value, key: &str) -> Result<bool, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

fn timeout(params: &Value) -> Result<Duration, String> {
    match params.get("timeoutSeconds") {
        None | Some(Value::Null) => Ok(Duration::from_secs(DEFAULT_TIMEOUT)),
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=300).contains(value))
            .map(Duration::from_secs)
            .ok_or_else(|| "timeoutSeconds must be an integer from 1 to 300".to_string()),
    }
}

fn push_option(
    args: &mut Vec<String>,
    params: &Value,
    key: &str,
    flag: &str,
) -> Result<(), String> {
    if let Some(value) = optional_string(params, key)? {
        args.push(flag.to_string());
        args.push(value);
    }
    Ok(())
}

fn push_switch(
    args: &mut Vec<String>,
    params: &Value,
    key: &str,
    flag: &str,
) -> Result<(), String> {
    if bool_param(params, key)? {
        args.push(flag.to_string());
    }
    Ok(())
}

fn push_repeated(
    args: &mut Vec<String>,
    params: &Value,
    key: &str,
    flag: &str,
) -> Result<(), String> {
    for value in string_array(params, key)? {
        args.push(flag.to_string());
        args.push(value);
    }
    Ok(())
}

fn launch_command(params: &Value) -> Result<(PathBuf, Vec<String>), String> {
    let executable = std::env::current_exe()
        .map_err(|err| format!("failed to locate the current rpi executable: {err}"))?;
    let mut args = vec!["--mode".to_string(), "rpc".to_string()];
    push_option(&mut args, params, "provider", "--provider")?;
    push_option(&mut args, params, "model", "--model")?;
    push_option(&mut args, params, "baseUrl", "--base-url")?;
    push_option(&mut args, params, "systemPrompt", "--system-prompt")?;
    push_option(&mut args, params, "thinking", "--thinking")?;
    push_option(&mut args, params, "name", "--name")?;
    push_option(&mut args, params, "session", "--session")?;
    push_option(&mut args, params, "sessionId", "--session-id")?;
    push_option(&mut args, params, "sessionDir", "--session-dir")?;

    for value in string_array(params, "appendSystemPrompt")? {
        args.push("--append-system-prompt".into());
        args.push(value);
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
    push_switch(
        &mut args,
        params,
        "noPromptTemplates",
        "--no-prompt-templates",
    )?;
    push_switch(&mut args, params, "noContextFiles", "--no-context-files")?;
    push_switch(&mut args, params, "noExtensions", "--no-extensions")?;
    push_switch(
        &mut args,
        params,
        "enablePiPackages",
        "--enable-pi-packages",
    )?;
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

fn read_stdout(stdout: ChildStdout, sender: mpsc::Sender<Result<Value, String>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut bytes = Vec::new();
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                let _ = sender.send(Err(format!("failed reading rpi RPC output: {err}")));
                break;
            }
        }
        if bytes.len() > MAX_BYTES + 2 {
            let _ = sender.send(Err("rpi RPC record exceeds the 1 MiB limit".into()));
            break;
        }
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        if bytes.is_empty() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(&bytes)
            .map_err(|err| format!("rpi RPC returned invalid JSONL: {err}"));
        if sender.send(value).is_err() {
            break;
        }
    }
}

fn launch_process(params: &Value) -> Result<RpcServer, String> {
    let (executable, args) = launch_command(params)?;
    let mut child = Command::new(&executable)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start rpi --mode rpc: {err}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "rpi RPC stdin is unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "rpi RPC stdout is unavailable".to_string())?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "rpi RPC stderr is unavailable".to_string())?;
    let (sender, records) = mpsc::channel();
    thread::spawn(move || read_stdout(stdout, sender));

    let stderr = Arc::new(Mutex::new(String::new()));
    let stderr_thread = Arc::clone(&stderr);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr_pipe
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes);
        if bytes.len() > MAX_BYTES {
            bytes.truncate(MAX_BYTES);
        }
        if let Ok(mut output) = stderr_thread.lock() {
            *output = String::from_utf8_lossy(&bytes).trim().to_string();
        }
    });

    let id = format!("rpi-rpc-{}", NEXT_SERVER_ID.fetch_add(1, Ordering::Relaxed));
    let mut process = RpcProcess {
        child,
        stdin,
        records,
        stderr,
    };
    let startup_deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < startup_deadline {
        match process.child.try_wait() {
            Ok(Some(status)) => {
                thread::sleep(Duration::from_millis(10));
                let stderr = process
                    .stderr
                    .lock()
                    .map(|value| value.clone())
                    .unwrap_or_default();
                let suffix = if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                };
                return Err(format!(
                    "rpi --mode rpc exited during startup with {status}{suffix}"
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(err) => return Err(format!("failed to inspect rpi RPC process: {err}")),
        }
    }

    Ok(RpcServer {
        id,
        executable: executable.to_string_lossy().into_owned(),
        args,
        process: Mutex::new(process),
    })
}

fn process_state(process: &mut RpcProcess) -> Result<Value, String> {
    match process.child.try_wait() {
        Ok(None) => Ok(json!({"state":"running"})),
        Ok(Some(status)) => Ok(json!({
            "state":"exited",
            "exitCode":status.code(),
            "stderr":process.stderr.lock().map(|value| value.clone()).unwrap_or_default(),
        })),
        Err(err) => Err(format!("failed to inspect rpi RPC process: {err}")),
    }
}

fn start_server(params: &Value) -> Result<String, String> {
    let server = Arc::new(launch_process(params)?);
    let response = json!({
        "id":server.id,
        "state":"running",
        "executable":server.executable,
        "args":server.args,
    });
    servers()
        .lock()
        .map_err(|_| "RPC server registry is poisoned")?
        .insert(server.id.clone(), server);
    Ok(response.to_string())
}

fn server_status(server: &RpcServer) -> Result<Value, String> {
    let mut process = server
        .process
        .lock()
        .map_err(|_| "RPC process lock is poisoned")?;
    let mut state = process_state(&mut process)?;
    let object = state
        .as_object_mut()
        .ok_or_else(|| "invalid RPC process state".to_string())?;
    object.insert("id".into(), Value::String(server.id.clone()));
    object.insert(
        "executable".into(),
        Value::String(server.executable.clone()),
    );
    object.insert(
        "args".into(),
        Value::Array(server.args.iter().cloned().map(Value::String).collect()),
    );
    Ok(state)
}

fn server(params: &Value) -> Result<String, String> {
    match params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("start")
    {
        "start" => start_server(params),
        "list" => {
            let values = servers()
                .lock()
                .map_err(|_| "RPC server registry is poisoned")?
                .values()
                .map(|server| server_status(server))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(values).to_string())
        }
        "status" => {
            let id = string_param(params, "id")?;
            let item = servers()
                .lock()
                .map_err(|_| "RPC server registry is poisoned")?
                .get(&id)
                .cloned()
                .ok_or_else(|| "RPC server not found".to_string())?;
            Ok(server_status(&item)?.to_string())
        }
        "stop" => {
            let id = string_param(params, "id")?;
            let item = servers()
                .lock()
                .map_err(|_| "RPC server registry is poisoned")?
                .remove(&id)
                .ok_or_else(|| "RPC server not found".to_string())?;
            let mut process = item
                .process
                .lock()
                .map_err(|_| "RPC process lock is poisoned")?;
            if process
                .child
                .try_wait()
                .map_err(|err| format!("failed to inspect rpi RPC process: {err}"))?
                .is_none()
            {
                process
                    .child
                    .kill()
                    .map_err(|err| format!("failed to stop rpi RPC process: {err}"))?;
                let _ = process.child.wait();
            }
            Ok(json!({"id":id,"state":"stopped"}).to_string())
        }
        _ => Err("action must be one of: start, status, list, stop".into()),
    }
}

fn stop_all_servers() {
    let items = match servers().lock() {
        Ok(mut registry) => registry
            .drain()
            .map(|(_, server)| server)
            .collect::<Vec<_>>(),
        Err(_) => return,
    };
    for item in items {
        if let Ok(mut process) = item.process.lock() {
            if process.child.try_wait().ok().flatten().is_none() {
                let _ = process.child.kill();
                let _ = process.child.wait();
            }
        }
    }
}

extern "C" fn on_session_shutdown(event: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if event.tag == EventTag::SessionShutdown {
        stop_all_servers();
    }
    0
}

fn prepare_command(params: &Value, tool_call_id: &str) -> Result<(Value, Value, String), String> {
    let mut command = params
        .get("command")
        .cloned()
        .ok_or_else(|| "command is required".to_string())?;
    let object = command
        .as_object_mut()
        .ok_or_else(|| "command must be a JSON object".to_string())?;
    let command_type = object
        .get("type")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "command.type is required".to_string())?;
    let request_id = match object.get("id") {
        Some(Value::String(value)) if !value.is_empty() => Value::String(value.clone()),
        Some(_) => return Err("command.id must be a non-empty string".into()),
        None => {
            let id = if tool_call_id.is_empty() {
                format!(
                    "rpi-request-{}",
                    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
                )
            } else {
                tool_call_id.to_string()
            };
            let id = Value::String(id);
            object.insert("id".into(), id.clone());
            id
        }
    };
    let line = serde_json::to_string(&command)
        .map_err(|err| format!("command is not valid JSON: {err}"))?;
    if line.len() > MAX_BYTES {
        return Err("command exceeds the 1 MiB limit".into());
    }
    Ok((command, request_id, command_type))
}

fn process_ended_error(process: &mut RpcProcess) -> String {
    let status = process.child.try_wait().ok().flatten();
    let stderr = process
        .stderr
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    let mut message = match status {
        Some(status) => format!("rpi --mode rpc exited with {status}"),
        None => "rpi RPC output closed before a matching response".to_string(),
    };
    if !stderr.is_empty() {
        message.push_str(": ");
        message.push_str(&stderr);
    }
    message
}

fn client(params: &Value, tool_call_id: &str, cancelled: &AtomicBool) -> Result<String, String> {
    let server_id = string_param(params, "serverId")?;
    let (command, request_id, command_type) = prepare_command(params, tool_call_id)?;
    let wait = timeout(params)?;
    let item = servers()
        .lock()
        .map_err(|_| "RPC server registry is poisoned")?
        .get(&server_id)
        .cloned()
        .ok_or_else(|| "RPC server not found".to_string())?;
    let mut process = item
        .process
        .lock()
        .map_err(|_| "RPC process lock is poisoned")?;

    if let Some(status) = process
        .child
        .try_wait()
        .map_err(|err| format!("failed to inspect rpi RPC process: {err}"))?
    {
        return Err(format!("rpi --mode rpc is not running ({status})"));
    }

    let line = serde_json::to_string(&command)
        .map_err(|err| format!("command is not valid JSON: {err}"))?;
    process
        .stdin
        .write_all(line.as_bytes())
        .and_then(|_| process.stdin.write_all(b"\n"))
        .and_then(|_| process.stdin.flush())
        .map_err(|err| format!("failed writing rpi RPC command: {err}"))?;

    let deadline = Instant::now() + wait;
    let mut events = Vec::new();
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err("rpi RPC request cancelled".into());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(format!("rpi RPC command {command_type} timed out"));
        }
        let slice = (deadline - now).min(Duration::from_millis(25));
        match process.records.recv_timeout(slice) {
            Ok(Ok(record)) => {
                let is_response = record.get("type").and_then(Value::as_str) == Some("response")
                    && record.get("id") == Some(&request_id);
                if is_response {
                    return Ok(json!({
                        "serverId":server_id,
                        "command":command,
                        "response":record,
                        "events":events,
                    })
                    .to_string());
                }
                events.push(record);
            }
            Ok(Err(err)) => return Err(err),
            Err(RecvTimeoutError::Timeout) => {
                if process.child.try_wait().ok().flatten().is_some() {
                    return Err(process_ended_error(&mut process));
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(process_ended_error(&mut process));
            }
        }
    }
}

fn execute_with_kind(
    kind: Kind,
    tool_call_id: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let tool_call_id = unsafe { tool_call_id.as_str() }.to_owned();
    let text = params.to_string_lossy();
    params.free_with(free);
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = serde_json::from_str::<Value>(&text)
            .map_err(|err| format!("invalid tool parameters: {err}"))
            .and_then(|params| match kind {
                Kind::Client => client(&params, &tool_call_id, &worker_cancelled),
                Kind::Server => server(&params),
            });
        let _ = sender.send(result);
    });
    Box::into_raw(Box::new(Drive {
        receiver,
        cancelled,
        done: false,
    })) as StepHandle
}

extern "C" fn execute_client(
    tool_call_id: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    execute_with_kind(Kind::Client, tool_call_id, params, free)
}

extern "C" fn execute_server(
    tool_call_id: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    execute_with_kind(Kind::Server, tool_call_id, params, free)
}

extern "C" fn poll(
    handle: StepHandle,
    _: Option<ToolPartialCb>,
    _: *mut std::ffi::c_void,
) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null RPC handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.done {
        return StepResult::err(StbString::from_string(
            "RPC handle polled after completion".into(),
        ));
    }
    if drive.cancelled.load(Ordering::Acquire) {
        drive.done = true;
        return StepResult::err(StbString::from_string("RPC request cancelled".into()));
    }
    match drive.receiver.try_recv() {
        Ok(Ok(text)) => {
            drive.done = true;
            StepResult::done(StbString::from_string(
                json!({"content":[{"type":"text","text":text}]}).to_string(),
            ))
        }
        Ok(Err(err)) => {
            drive.done = true;
            StepResult::err(StbString::from_string(err))
        }
        Err(TryRecvError::Empty) => StepResult::pending(StbString::empty()),
        Err(TryRecvError::Disconnected) => {
            drive.done = true;
            StepResult::err(StbString::from_string(
                "RPC worker stopped without a result".into(),
            ))
        }
    }
}

extern "C" fn cancel(handle: StepHandle) {
    if !handle.is_null() {
        unsafe {
            (&*(handle as *mut Drive))
                .cancelled
                .store(true, Ordering::Release);
        }
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

const CLIENT_PARAMETERS: &str = r#"{
  "type":"object",
  "properties":{
    "serverId":{"type":"string"},
    "command":{"type":"object","description":"Native rpi RPC command: {id?, type, ...}"},
    "timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}
  },
  "required":["serverId","command"]
}"#;

const SERVER_PARAMETERS: &str = r#"{
  "type":"object",
  "properties":{
    "action":{"type":"string","enum":["start","status","list","stop"]},
    "id":{"type":"string"},
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

#[no_mangle]
pub extern "C" fn rpi_plugin_register(api: *const PluginApi) -> i32 {
    unsafe {
        register_entrypoint_unified(api, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let client_schema = Box::new(StableToolSchema {
            name: StbString::from_string("extension_rpc_client".into()),
            description: StbString::from_string(
                "Send a native JSONL command to a managed rpi --mode rpc process.".into(),
            ),
            parameters: StbString::from_string(CLIENT_PARAMETERS.into()),
        });
        let server_schema = Box::new(StableToolSchema {
            name: StbString::from_string("extension_rpc_server".into()),
            description: StbString::from_string(
                "Start and manage a persistent rpi --mode rpc process using rpi CLI options."
                    .into(),
            ),
            parameters: StbString::from_string(SERVER_PARAMETERS.into()),
        });
        let first = register(
            &*client_schema,
            execute_client,
            poll,
            cancel,
            destroy,
            free_string,
        );
        let second = if first == 0 {
            register(
                &*server_schema,
                execute_server,
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
        drop(client_schema);
        drop(server_schema);
        third
    })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_uses_reserved_rpi_mode_and_options() {
        let (_, args) = launch_command(&json!({
            "provider":"openai",
            "model":"gpt-5",
            "thinking":"high",
            "name":"rpc-test",
            "sessionId":"session-1",
            "tools":["read","bash"],
            "extensionsDir":["/tmp/extensions"],
            "approve":true
        }))
        .unwrap();
        assert_eq!(&args[0..2], ["--mode", "rpc"]);
        assert!(args.windows(2).any(|pair| pair == ["--provider", "openai"]));
        assert!(args.windows(2).any(|pair| pair == ["--model", "gpt-5"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--session-id", "session-1"]));
        assert!(args.windows(2).any(|pair| pair == ["--tools", "read,bash"]));
        assert!(args.iter().any(|value| value == "--approve"));
    }

    #[test]
    fn tool_call_id_becomes_rpc_request_id() {
        let (command, id, kind) =
            prepare_command(&json!({"command":{"type":"get_state"}}), "tool-call-42").unwrap();
        assert_eq!(id, "tool-call-42");
        assert_eq!(command["id"], "tool-call-42");
        assert_eq!(kind, "get_state");
    }

    #[test]
    fn rejects_conflicting_session_options() {
        let err = launch_command(&json!({
            "noSession":true,
            "session":"existing"
        }))
        .unwrap_err();
        assert!(err.contains("cannot be combined"));
    }

    #[test]
    fn schemas_are_valid_json() {
        assert!(serde_json::from_str::<Value>(CLIENT_PARAMETERS).is_ok());
        assert!(serde_json::from_str::<Value>(SERVER_PARAMETERS).is_ok());
    }
}
