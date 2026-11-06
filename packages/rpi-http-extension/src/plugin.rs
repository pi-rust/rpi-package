use crate::launch;
use rpi_plugin_sdk::{
    EventTag, FreeStringFn, RuntimeActionFn, RuntimeActionId, StablePluginEvent, StbString,
    StbStringRef,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicPtr, Ordering},
        Mutex, OnceLock,
    },
};
static ACTION: OnceLock<RuntimeActionFn> = OnceLock::new();
static FREE: OnceLock<FreeStringFn> = OnceLock::new();
static USER: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static STOP: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();
static THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
fn action(id: RuntimeActionId, args: Value) -> Option<Value> {
    let mut out = StbString::empty();
    let code = ACTION.get()?(
        id as u32,
        StbStringRef::from_str(&args.to_string()),
        &mut out,
        USER.load(Ordering::Acquire),
    );
    let result = if code == 0 {
        serde_json::from_str(&out.to_string_lossy()).ok()
    } else {
        None
    };
    if let Some(free) = FREE.get() {
        free(out);
    }
    result
}
fn flag(name: &str) -> Option<Value> {
    action(RuntimeActionId::GetCliFlag, json!({"name":name})).and_then(|v| v.get("value").cloned())
}
fn finish(code: i32) {
    action(
        RuntimeActionId::StdioLifecycle,
        json!({"op":"finish","exitCode":code}),
    );
}
fn text(value: Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(true) => Some("true".into()),
        _ => None,
    }
}

extern "C" fn start(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    let Ok(mut held) = THREAD.lock() else {
        return 1;
    };
    if held.is_some() {
        return 0;
    }
    for mode in ["webserver", "web", "server", "webui", "acp"] {
        if launch::enabled(flag(mode).as_ref()) {
            eprintln!("[rpi-http] Conflicting service mode");
            finish(2);
            return 0;
        }
    }
    if flag("http").as_ref().and_then(Value::as_str) == Some("help") {
        println!("{}", launch::HELP);
        finish(0);
        return 0;
    }
    let mut flags = BTreeMap::new();
    for name in [
        "http",
        "bind",
        "port",
        "node-data",
        "http-assets",
        "node-control-stdin",
    ] {
        if let Some(value) = flag(name).and_then(text) {
            flags.insert(name.into(), value);
        }
    }
    let options = match launch::from_flags(&flags) {
        Ok(v) => v,
        Err(error) => {
            eprintln!("[rpi-http] {error}");
            finish(2);
            return 0;
        }
    };
    let shutdown_stdin = options.control_capability.is_some()
        || launch::enabled(flag("http-shutdown-stdin").as_ref());
    let (stop, mut changes) = tokio::sync::watch::channel(false);
    let _ = STOP.set(stop);
    let thread = std::thread::Builder::new()
        .name("rpi-http-extension".into())
        .spawn(move || {
            let result = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(rpi_node_host::run_extension(
                    options,
                    async move {
                        if !*changes.borrow_and_update() {
                            let _ = changes.changed().await;
                        }
                    },
                    shutdown_stdin,
                )),
                Err(_) => Err("Cannot start HTTP extension runtime".into()),
            };
            let code = match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("[rpi-http] {error}");
                    1
                }
            };
            finish(code);
        });
    match thread {
        Ok(thread) => {
            *held = Some(thread);
            0
        }
        Err(_) => {
            finish(1);
            1
        }
    }
}
extern "C" fn stop(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if let Some(stop) = STOP.get() {
        stop.send_replace(true);
    }
    let thread = THREAD.lock().ok().and_then(|mut held| held.take());
    if let Some(thread) = thread {
        if thread.join().is_err() {
            return 1;
        }
    }
    0
}
fn register(api: &rpi_plugin_sdk::PluginApi) -> i32 {
    let _ = ACTION.set(api.runtime_action);
    let _ = FREE.set(api.free_string);
    USER.store(api.user_data, Ordering::Release);
    let Some(register_flag) = api.register_flag else {
        return 1;
    };
    if register_flag(
        StbStringRef::from_str("http"),
        StbStringRef::from_str("Start shared HTTP node; optional config path or help"),
    ) != 0
    {
        return 1;
    }
    for (name, description) in [
        ("bind", "HTTP listen IP"),
        ("port", "HTTP listen port"),
        ("node-data", "Node data directory"),
        ("http-assets", "Web assets directory"),
        (
            "node-control-stdin",
            "Read the private parent capability handoff from stdin",
        ),
        (
            "http-shutdown-stdin",
            "Stop the owned HTTP node on stdin EOF",
        ),
    ] {
        let _ = register_flag(
            StbStringRef::from_str(name),
            StbStringRef::from_str(description),
        );
    }
    if !launch::enabled(flag("http").as_ref()) {
        return 0;
    }
    if action(RuntimeActionId::StdioLifecycle, json!({"op":"status"})).is_none() {
        return 1;
    }
    let (Some(declare), Some(handler)) = (api.declare, api.register_event_handler) else {
        return 1;
    };
    if declare(StbStringRef::from_str(
        "{\"ownsStdio\":true,\"requiresAgent\":false}",
    )) != 0
    {
        return 1;
    }
    if handler(EventTag::SessionShutdown, stop, api.user_data) != 0 {
        return 1;
    }
    handler(EventTag::SessionStart, start, api.user_data)
}
rpi_plugin_sdk::export_plugin!(register);
