//! Optional host entry point: `rpi --web`, sharing the standalone server.
use crate::config::{self, Launch};
use rpi_plugin_sdk::{
    EventTag, FreeStringFn, RuntimeActionFn, RuntimeActionId, StablePluginEvent, StbString,
    StbStringRef,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicPtr, Ordering},
    Mutex, OnceLock,
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
    let value = if code == 0 {
        serde_json::from_str(&out.to_string_lossy()).ok()
    } else {
        None
    };
    if let Some(free) = FREE.get() {
        free(out);
    }
    value
}

fn flag(name: &str) -> Option<Value> {
    action(RuntimeActionId::GetCliFlag, json!({"name":name})).and_then(|v| v.get("value").cloned())
}

fn enabled(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !value.is_empty() && !value.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

fn finish(code: i32) {
    action(
        RuntimeActionId::StdioLifecycle,
        json!({"op":"finish","exitCode":code}),
    );
}

extern "C" fn start(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    let mut thread = THREAD.lock().unwrap();
    if thread.is_some() {
        return 0;
    }
    for other in ["acp", "webui", "server"] {
        if enabled(flag(other).as_ref()) {
            eprintln!("[rpi-web] --web cannot be combined with --{other}");
            finish(2);
            return 0;
        }
    }
    let args = match flag("web") {
        Some(Value::String(mode)) if mode.eq_ignore_ascii_case("true") => vec![],
        Some(Value::String(mode)) if mode == "init" => vec!["--init".into()],
        Some(Value::String(mode)) if mode == "help" => vec!["--help".into()],
        Some(Value::String(path)) => vec!["--config".into(), path],
        _ => vec![],
    };
    let mut config = match config::from_args(args) {
        Ok(Launch::Run(config)) => *config,
        Ok(Launch::Initialized(path)) => {
            println!("Created {}", path.display());
            finish(0);
            return 0;
        }
        Ok(Launch::Help) => {
            println!("rpi --web[=CONFIG|init|help]\n{}", config::HELP);
            finish(0);
            return 0;
        }
        Ok(Launch::Version) => {
            finish(0);
            return 0;
        }
        Err(error) => {
            eprintln!("[rpi-web] {error}");
            finish(2);
            return 0;
        }
    };
    // Children use this exact host, and receive no --web flag, preventing recursion.
    if config.rpi_bin == std::path::Path::new("rpi") {
        match std::env::current_exe() {
            Ok(path) => config.rpi_bin = path,
            Err(error) => {
                eprintln!("[rpi-web] {error}");
                finish(2);
                return 0;
            }
        }
    }
    let (stop, mut receiver) = tokio::sync::watch::channel(false);
    let _ = STOP.set(stop);
    *thread = Some(std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build();
        let result = match runtime {
            Ok(runtime) => runtime.block_on(crate::server::run_until(config, async move {
                if !*receiver.borrow_and_update() {
                    let _ = receiver.changed().await;
                }
            })),
            Err(error) => Err(error.into()),
        };
        let code = match result {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("[rpi-web] {error}");
                1
            }
        };
        finish(code);
    }));
    0
}

extern "C" fn stop(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if let Some(stop) = STOP.get() {
        let _ = stop.send(true);
    }
    if let Some(thread) = THREAD.lock().unwrap().take() {
        let _ = thread.join();
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
        StbStringRef::from_str("web"),
        StbStringRef::from_str("Start Web workspace; optional configuration path, init or help"),
    ) != 0
    {
        return 1;
    }
    if !enabled(flag("web").as_ref()) {
        return 0;
    }
    if action(RuntimeActionId::StdioLifecycle, json!({"op":"status"})).is_none() {
        return 1;
    }
    let (Some(declare), Some(handler)) = (api.declare, api.register_event_handler) else {
        return 1;
    };
    if declare(StbStringRef::from_str("{\"ownsStdio\":true}")) != 0 {
        return 1;
    }
    if handler(EventTag::SessionShutdown, stop, api.user_data) != 0 {
        return 1;
    }
    handler(EventTag::SessionStart, start, api.user_data)
}

rpi_plugin_sdk::export_plugin!(register);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inactive_flags_do_not_start_a_server_in_rpc_children() {
        for value in [
            None,
            Some(Value::Null),
            Some(json!(false)),
            Some(json!("false")),
            Some(json!("")),
        ] {
            assert!(!enabled(value.as_ref()));
        }
        for value in [json!(true), json!("init"), json!("/path/web.json")] {
            assert!(enabled(Some(&value)));
        }
    }
}
