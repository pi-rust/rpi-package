use rpi_plugin_sdk::{
    register_entrypoint, FreeStringFn, PluginApiVt, StableToolSchema, StbString, StbStringRef,
    StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt;

const MAX_BYTES: usize = 1_048_576;
const DEFAULT_TIMEOUT: u64 = 30;

struct Drive {
    params: Value,
    kind: Kind,
    cancelled: AtomicBool,
    done: bool,
}

#[derive(Clone, Copy)]
enum Kind {
    Client,
    Server,
}

struct RpcServer {
    id: String,
    address: String,
    stop: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
}

static SERVERS: OnceLock<Mutex<HashMap<String, RpcServer>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn servers() -> &'static Mutex<HashMap<String, RpcServer>> {
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

fn timeout(params: &Value) -> Duration {
    Duration::from_secs(
        params
            .get("timeoutSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT)
            .clamp(1, 300),
    )
}

fn argv(params: &Value) -> Result<(String, Vec<String>), String> {
    let program = string_param(params, "program")?;
    if program.len() > 4096 {
        return Err("program path is too long".into());
    }
    let args = params
        .get("args")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "args must contain strings".to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    if args.len() > 128 || args.iter().any(|arg| arg.len() > 32_768) {
        return Err("args exceed the supported size".into());
    }
    Ok((program, args))
}

fn request_line(params: &Value) -> Result<String, String> {
    let request = params
        .get("request")
        .cloned()
        .or_else(|| params.get("message").cloned())
        .ok_or_else(|| "request is required".to_string())?;
    let line =
        serde_json::to_string(&request).map_err(|err| format!("request is not JSON: {err}"))?;
    if line.len() > MAX_BYTES {
        return Err("request exceeds the 1 MiB limit".into());
    }
    Ok(line)
}

fn run_process(params: &Value, request: &str) -> Result<Value, String> {
    let (program, args) = argv(params)?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start RPC process: {err}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(request.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .map_err(|err| format!("failed to write RPC request: {err}"))?;
    }
    let status = child
        .wait_timeout(timeout(params))
        .map_err(|err| format!("failed waiting for RPC process: {err}"))?;
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("RPC process timed out".into());
    }
    let mut stdout = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        pipe.take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut stdout)
            .map_err(|err| format!("failed reading RPC response: {err}"))?;
    }
    let truncated = stdout.len() > MAX_BYTES;
    if truncated {
        stdout.truncate(MAX_BYTES);
    }
    let text = String::from_utf8_lossy(&stdout);
    let mut records = Vec::new();
    for raw in text.split('\n').filter(|line| !line.trim().is_empty()) {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        records.push(
            serde_json::from_str::<Value>(raw)
                .map_err(|err| format!("RPC process returned invalid JSONL: {err}"))?,
        );
    }
    Ok(json!({
        "records": records,
        "exitCode": status.and_then(|value| value.code()),
        "truncated": truncated,
    }))
}

fn client(params: &Value) -> Result<String, String> {
    let request = request_line(params)?;
    if let Some(address) = params.get("address").and_then(Value::as_str) {
        return tcp_client(address, &request, timeout(params));
    }
    Ok(run_process(params, &request)?.to_string())
}

fn tcp_client(address: &str, request: &str, timeout: Duration) -> Result<String, String> {
    let socket = address
        .parse::<std::net::SocketAddr>()
        .map_err(|err| format!("address must be host:port: {err}"))?;
    if !matches!(socket.ip(), IpAddr::V4(ip) if ip.is_loopback())
        && !matches!(socket.ip(), IpAddr::V6(ip) if ip.is_loopback())
    {
        return Err("RPC client only allows loopback server addresses".into());
    }
    let mut stream = TcpStream::connect_timeout(&socket, timeout)
        .map_err(|err| format!("failed to connect to RPC server: {err}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|err| format!("failed to configure RPC client: {err}"))?;
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|err| format!("failed to write RPC request: {err}"))?;
    let mut output = Vec::new();
    stream
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|err| format!("failed to read RPC server response: {err}"))?;
    let truncated = output.len() > MAX_BYTES;
    if truncated {
        output.truncate(MAX_BYTES);
    }
    let records = String::from_utf8_lossy(&output)
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line.strip_suffix('\r').unwrap_or(line)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("RPC server returned invalid JSONL: {err}"))?;
    Ok(json!({"records": records, "truncated": truncated}).to_string())
}

fn handle_connection(mut stream: TcpStream, params: &Value) {
    let _ = stream.set_read_timeout(Some(timeout(params)));
    let mut request = String::new();
    let cloned = match stream.try_clone() {
        Ok(cloned) => cloned,
        Err(_) => return,
    };
    let mut reader = BufReader::new(cloned);
    if reader.read_line(&mut request).is_err() || request.len() > MAX_BYTES + 2 {
        let _ = stream.write_all(b"{\"error\":\"invalid request\"}\n");
        return;
    }
    let request = request.trim_end_matches(['\n', '\r']);
    let value = match serde_json::from_str::<Value>(request) {
        Ok(value) => value,
        Err(err) => {
            let _ = stream.write_all(
                json!({"error": format!("invalid JSON: {err}")})
                    .to_string()
                    .as_bytes(),
            );
            let _ = stream.write_all(b"\n");
            return;
        }
    };
    match run_process(params, &value.to_string()) {
        Ok(result) => {
            if let Some(records) = result.get("records").and_then(Value::as_array) {
                for record in records {
                    let _ = stream.write_all(record.to_string().as_bytes());
                    let _ = stream.write_all(b"\n");
                }
            } else {
                let _ = stream.write_all(result.to_string().as_bytes());
                let _ = stream.write_all(b"\n");
            }
        }
        Err(err) => {
            let _ = stream.write_all(json!({"error": err}).to_string().as_bytes());
            let _ = stream.write_all(b"\n");
        }
    }
}

fn start_server(params: &Value) -> Result<String, String> {
    let (program, _) = argv(params)?;
    let _ = program;
    let port = params.get("port").and_then(Value::as_u64).unwrap_or(0);
    if port > u16::MAX as u64 {
        return Err("port must be between 0 and 65535".into());
    }
    let listener = TcpListener::bind(("127.0.0.1", port as u16))
        .map_err(|err| format!("failed to bind RPC server: {err}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("failed to configure RPC server: {err}"))?;
    let address = listener
        .local_addr()
        .map_err(|err| format!("failed to read RPC server address: {err}"))?
        .to_string();
    let id = format!("rpi-rpc-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));
    let stop_thread = Arc::clone(&stop);
    let running_thread = Arc::clone(&running);
    let child_params = params.clone();
    thread::spawn(move || {
        while !stop_thread.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let params = child_params.clone();
                    thread::spawn(move || handle_connection(stream, &params));
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25))
                }
                Err(_) => break,
            }
        }
        running_thread.store(false, Ordering::Release);
    });
    servers()
        .lock()
        .map_err(|_| "server registry is poisoned")?
        .insert(
            id.clone(),
            RpcServer {
                id: id.clone(),
                address: address.clone(),
                stop,
                running,
            },
        );
    Ok(json!({"id": id, "address": address, "state": "running"}).to_string())
}

fn server(params: &Value) -> Result<String, String> {
    match params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("start")
    {
        "start" => start_server(params),
        "list" => {
            let values = servers().lock().map_err(|_| "server registry is poisoned")?.values().map(|server| json!({"id":server.id,"address":server.address,"state":if server.running.load(Ordering::Acquire) {"running"} else {"stopped"}})).collect::<Vec<_>>();
            Ok(Value::Array(values).to_string())
        }
        "status" => {
            let id = string_param(params, "id")?;
            let registry = servers()
                .lock()
                .map_err(|_| "server registry is poisoned")?;
            let item = registry
                .get(&id)
                .ok_or_else(|| "RPC server not found".to_string())?;
            Ok(json!({"id":item.id,"address":item.address,"state":if item.running.load(Ordering::Acquire) {"running"} else {"stopped"}}).to_string())
        }
        "stop" => {
            let id = string_param(params, "id")?;
            let item = servers()
                .lock()
                .map_err(|_| "server registry is poisoned")?
                .remove(&id)
                .ok_or_else(|| "RPC server not found".to_string())?;
            item.stop.store(true, Ordering::Release);
            Ok(json!({"id":id,"state":"stopping"}).to_string())
        }
        _ => Err("action must be one of: start, status, list, stop".into()),
    }
}

fn execute_with_kind(
    kind: Kind,
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    Box::into_raw(Box::new(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        kind,
        cancelled: AtomicBool::new(false),
        done: false,
    })) as StepHandle
}

extern "C" fn execute_client(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    execute_with_kind(Kind::Client, StbStringRef::empty(), params, free)
}
extern "C" fn execute_server(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    execute_with_kind(Kind::Server, StbStringRef::empty(), params, free)
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
    if drive.cancelled.load(Ordering::Acquire) {
        return StepResult::err(StbString::from_string("RPC request cancelled".into()));
    }
    if drive.done {
        return StepResult::err(StbString::from_string(
            "RPC handle polled after completion".into(),
        ));
    }
    drive.done = true;
    let result = match drive.kind {
        Kind::Client => client(&drive.params),
        Kind::Server => server(&drive.params),
    };
    match result {
        Ok(text) => StepResult::done(StbString::from_string(
            json!({"content":[{"type":"text","text":text}]}).to_string(),
        )),
        Err(err) => StepResult::err(StbString::from_string(err)),
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
            drop(Box::from_raw(handle as *mut Drive));
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

#[no_mangle]
pub extern "C" fn rpi_plugin_register(api: *const PluginApiVt, abi: u32) -> i32 {
    register_entrypoint(api, abi, |api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let client_schema = Box::new(StableToolSchema { name: StbString::from_string("extension_rpc_client".into()), description: StbString::from_string("Send one bounded JSONL RPC request to an extension process or localhost RPC server.".into()), parameters: StbString::from_string(r#"{"type":"object","properties":{"program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"address":{"type":"string"},"request":{},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}},"required":["request"]}"#.into()) });
        let server_schema = Box::new(StableToolSchema { name: StbString::from_string("extension_rpc_server".into()), description: StbString::from_string("Manage a localhost JSONL RPC server that forwards requests to an extension process.".into()), parameters: StbString::from_string(r#"{"type":"object","properties":{"action":{"type":"string","enum":["start","status","list","stop"]},"program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"port":{"type":"integer","minimum":0,"maximum":65535},"id":{"type":"string"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}}}"#.into()) });
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
        drop(client_schema);
        drop(server_schema);
        second
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_single_jsonl_record() {
        let value = json!({"request":{"type":"get_state","message":"a\nb"}});
        let line = request_line(&value).unwrap();
        assert!(!line.contains('\n'));
        assert!(serde_json::from_str::<Value>(&line).is_ok());
    }

    #[test]
    fn rejects_shell_like_invalid_args() {
        let err = argv(&json!({"program":"rpi","args":[1]})).unwrap_err();
        assert!(err.contains("strings"));
    }
}
