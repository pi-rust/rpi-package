use crate::protocol::{run, ServerOptions};
use rpi_plugin_sdk::{
    EventTag, FreeStringFn, RuntimeActionFn, RuntimeActionId, StablePluginEvent, StbString,
    StbStringRef,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

const MCP_FLAG: &str = "mcp";

static RUNTIME_ACTION: OnceLock<RuntimeActionFn> = OnceLock::new();
static RUNTIME_USER_DATA: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static HOST_FREE_STRING: OnceLock<FreeStringFn> = OnceLock::new();
static MCP_ENABLED: AtomicBool = AtomicBool::new(false);
static SERVER_STOP: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();
static SERVER_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

fn action(id: RuntimeActionId, args: Value) -> Option<Value> {
    let action = *RUNTIME_ACTION.get()?;
    let mut out = StbString::empty();
    let rc = action(
        id as u32,
        StbStringRef::from_str(&args.to_string()),
        &mut out,
        RUNTIME_USER_DATA.load(Ordering::Acquire),
    );
    let value = if rc == 0 {
        serde_json::from_str(&out.to_string_lossy()).ok()
    } else {
        None
    };
    if let Some(free) = HOST_FREE_STRING.get() {
        free(out);
    }
    value
}

fn flag(name: &str) -> Option<Value> {
    action(RuntimeActionId::GetCliFlag, json!({"name":name}))
        .and_then(|value| value.get("value").cloned())
}

fn flag_is_on(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => {
            !value.trim().is_empty() && !value.eq_ignore_ascii_case("false")
        }
        _ => false,
    }
}

fn lifecycle(op: &str, code: i32) {
    let _ = action(
        RuntimeActionId::StdioLifecycle,
        json!({"op":op,"exitCode":code}),
    );
}

fn get_arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix(&format!("{name}=")).map(String::from))
        })
}

fn get_flag(name: &str) -> bool {
    std::env::args().any(|arg| arg == name)
}

extern "C" fn start(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    let mut thread = SERVER_THREAD.lock().unwrap();
    if thread.is_some() || !MCP_ENABLED.load(Ordering::Acquire) {
        return 0;
    }
    // Stdio ownership is already ours here, so refusing a conflicting adapter
    // can end the host instead of silently leaving two servers on one pipe.
    for other in ["acp", "http", "webserver", "web", "server", "webui"] {
        if flag_is_on(flag(other).as_ref()) {
            eprintln!("[rpi-mcp] --mcp cannot be combined with --{other}");
            lifecycle("finish", 2);
            return 0;
        }
    }
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let _ = SERVER_STOP.set(stop);
    let options = ServerOptions {
        cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        model: get_arg("--model"),
        provider: get_arg("--provider"),
        bypass: get_flag("--bypass"),
        session_dir: get_arg("--session-dir").map(Into::into),
    };
    *thread = Some(thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("[rpi-mcp] failed to create runtime: {error}");
                lifecycle("finish", 1);
                return;
            }
        };
        let (input, pending) = match take_stdin() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[rpi-mcp] failed to take stdin: {error}");
                lifecycle("finish", 1);
                return;
            }
        };
        let code = runtime.block_on(run(input, pending, receiver, options));
        lifecycle("finish", code);
    }));
    0
}

extern "C" fn stop(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if let Some(stop) = SERVER_STOP.get() {
        let _ = stop.send(true);
    }
    if let Some(thread) = SERVER_THREAD.lock().unwrap().take() {
        let _ = thread.join();
    }
    0
}

fn take_stdin() -> Result<(rpi_plugin_sdk::stdio::StdinLines, Option<String>), String> {
    let mut input = rpi_plugin_sdk::stdio::StdinLines::default();
    let pending = match input.poll_line().map_err(|error| error.to_string())? {
        rpi_plugin_sdk::stdio::InputLine::Line(line) => Some(line),
        rpi_plugin_sdk::stdio::InputLine::Pending | rpi_plugin_sdk::stdio::InputLine::Eof => None,
    };
    Ok((input, pending))
}

pub fn register(api: &rpi_plugin_sdk::PluginApi) -> i32 {
    let _ = RUNTIME_ACTION.set(api.runtime_action);
    RUNTIME_USER_DATA.store(api.user_data, Ordering::Release);
    let _ = HOST_FREE_STRING.set(api.free_string);

    if let Some(register_flag) = api.register_flag {
        if register_flag(
            StbStringRef::from_str(MCP_FLAG),
            StbStringRef::from_str("Run rpi as an MCP stdio server"),
        ) != 0
        {
            return 1;
        }
    }

    if !flag_is_on(flag(MCP_FLAG).as_ref()) {
        return 0;
    }
    let Some(declare) = api.declare else {
        eprintln!("[rpi-mcp] host does not support ownsStdio");
        return 1;
    };
    if declare(StbStringRef::from_str(r#"{"ownsStdio":true}"#)) != 0 {
        eprintln!("[rpi-mcp] host rejected stdio ownership");
        return 1;
    }
    MCP_ENABLED.store(true, Ordering::Release);
    let Some(register_handler) = api.register_event_handler else {
        return 1;
    };
    if register_handler(EventTag::SessionShutdown, stop, api.user_data) != 0 {
        return 1;
    }
    register_handler(EventTag::SessionStart, start, api.user_data)
}
