//! Optional host entry point: `rpi --webserver`, sharing the standalone server.
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

/// A flag carrying a value: `--port 3031` and `--port=3031` both yield a string.
fn flag_value(name: &str) -> Option<String> {
    flag(name).and_then(flag_text)
}

/// Normalize a parsed host flag into the text form of a `--flag VALUE` option.
fn flag_text(value: Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Append options the host CLI already parsed, so `rpi --webserver --port N`
/// behaves like `rpi-web --port N` (CLI overrides every config file).
fn append_service_flags(args: &mut Vec<String>) {
    args.extend(service_options(flag_value("bind"), flag_value("port")));
}

fn service_options(bind: Option<String>, port: Option<String>) -> Vec<String> {
    let mut args = Vec::new();
    for (name, value) in [("bind", bind), ("port", port)] {
        if let Some(value) = value {
            args.push(format!("--{name}"));
            args.push(value);
        }
    }
    args
}

fn enabled(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !value.is_empty() && !value.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

fn service_mode(primary: Option<Value>, legacy: Option<Value>) -> Result<Option<Value>, String> {
    let primary = primary.filter(|value| enabled(Some(value)));
    let legacy = legacy.filter(|value| enabled(Some(value)));
    match (primary, legacy) {
        (Some(primary), Some(legacy)) if primary != legacy => {
            Err("--webserver and --web specify different values; use only one entry point".into())
        }
        (Some(value), _) | (_, Some(value)) => Ok(Some(value)),
        _ => Ok(None),
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
            eprintln!("[rpi-web] --webserver (--web) cannot be combined with --{other}");
            finish(2);
            return 0;
        }
    }
    let mode = match service_mode(flag("webserver"), flag("web")) {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("[rpi-web] {error}");
            finish(2);
            return 0;
        }
    };
    let args = match mode {
        Some(Value::String(mode)) if mode.eq_ignore_ascii_case("true") => vec![],
        Some(Value::String(mode)) if mode == "init" => vec!["--init".into()],
        Some(Value::String(mode)) if mode == "help" => vec!["--help".into()],
        Some(Value::String(path)) => vec!["--config".into(), path],
        _ => vec![],
    };
    let mut args = args;
    append_service_flags(&mut args);
    let mut config = match config::from_args(args) {
        Ok(Launch::Run(config)) => *config,
        Ok(Launch::Initialized(path)) => {
            println!("Created {}", path.display());
            finish(0);
            return 0;
        }
        Ok(Launch::Help) => {
            println!(
                "rpi --webserver[=CONFIG|init|help]\nAlias: --web\n{}",
                config::HELP
            );
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
    // Children receive neither service flag, preventing recursive Web servers.
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
    for (name, description) in [
        (
            "webserver",
            "Start Web server; optional configuration path, init or help",
        ),
        ("web", "Compatibility alias for --webserver"),
    ] {
        if register_flag(
            StbStringRef::from_str(name),
            StbStringRef::from_str(description),
        ) != 0
        {
            return 1;
        }
    }
    if let Some(register_flag) = api.register_flag {
        let _ = register_flag(
            StbStringRef::from_str("port"),
            StbStringRef::from_str(
                "Web server listen port (overrides web.json), used with --webserver",
            ),
        );
        let _ = register_flag(
            StbStringRef::from_str("bind"),
            StbStringRef::from_str(
                "Web server listen address (overrides web.json), used with --webserver",
            ),
        );
    }
    if !enabled(flag("webserver").as_ref()) && !enabled(flag("web").as_ref()) {
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
    fn service_entry_points_preserve_modes_and_reject_ambiguous_configuration() {
        for value in [
            json!(true),
            json!("true"),
            json!("init"),
            json!("help"),
            json!("/path/web.json"),
        ] {
            assert_eq!(
                service_mode(Some(value.clone()), None).unwrap(),
                Some(value.clone())
            );
            assert_eq!(
                service_mode(None, Some(value.clone())).unwrap(),
                Some(value.clone())
            );
            assert_eq!(
                service_mode(Some(json!(false)), Some(value.clone())).unwrap(),
                Some(value)
            );
        }
        assert_eq!(service_mode(None, None).unwrap(), None);
        assert_eq!(
            service_mode(Some(json!(false)), Some(json!("false"))).unwrap(),
            None
        );
        assert_eq!(
            service_mode(Some(json!("init")), Some(json!("init"))).unwrap(),
            Some(json!("init"))
        );
        assert!(service_mode(Some(json!("init")), Some(json!("other.json"))).is_err());
    }

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

    #[test]
    fn host_service_flags_become_web_options() {
        assert_eq!(flag_text(json!("3031")).as_deref(), Some("3031"));
        assert_eq!(flag_text(json!(3031)).as_deref(), Some("3031"));
        assert_eq!(flag_text(json!(true)), None);
        assert_eq!(flag_text(json!(null)), None);
        assert_eq!(flag_text(json!("")), None);
        let options = service_options(Some("0.0.0.0".into()), Some("3031".into()));
        assert_eq!(options, ["--bind", "0.0.0.0", "--port", "3031"]);
        // The forwarded options parse as ordinary Web CLI overrides.
        let (bind, port) = match crate::config::from_args(options).unwrap() {
            crate::config::Launch::Run(config) => (config.bind, config.port),
            other => panic!("expected a run launch, got {other:?}"),
        };
        assert_eq!(bind, "0.0.0.0");
        assert_eq!(port, 3031);
        assert!(service_options(None, None).is_empty());
    }
}
