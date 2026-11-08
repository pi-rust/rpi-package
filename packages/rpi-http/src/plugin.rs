//! `rpi --http` entry point: run the API-only HTTP/SSE adapter from `rpi-httpcore`.
//!
//! The plugin keeps the existing ABI contract (`ownsStdio`, `SessionStart`,
//! `SessionShutdown` and `StdioLifecycle` exit reporting). It starts one
//! `rpi-extcore::service::Service` in its own runtime; that service does not
//! share live sessions with the Agent session that loaded the plugin.
use rpi_httpcore::config::{self, Launch};
use rpi_plugin_sdk::{
    EventTag, FreeStringFn, RuntimeActionFn, RuntimeActionId, StablePluginEvent, StbString,
    StbStringRef,
};
use serde_json::{json, Value};
use std::{
    io::Write,
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
fn flag_text(value: Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn flag_value(name: &str) -> Option<String> {
    flag(name).and_then(flag_text)
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

/// Forward host-parsed listener overrides so CLI values beat the config file.
fn append_service_flags(args: &mut Vec<String>) {
    args.extend(service_options(flag_value("bind"), flag_value("port")));
}

fn enabled(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !value.is_empty() && !value.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

/// Map the host `--http` value onto the existing `rpi-httpcore` CLI contract.
fn http_args(mode: Option<Value>) -> Vec<String> {
    match mode {
        Some(Value::String(mode)) if mode.eq_ignore_ascii_case("help") => vec!["--help".into()],
        Some(Value::String(mode)) if mode.eq_ignore_ascii_case("true") => Vec::new(),
        Some(Value::String(path)) => vec!["--config".into(), path],
        _ => Vec::new(),
    }
}

fn finish(code: i32) {
    action(
        RuntimeActionId::StdioLifecycle,
        json!({"op":"finish","exitCode":code}),
    );
}

fn report(info: rpi_httpcore::Listening) {
    println!("rpi HTTP API listening on {}", info.address);
    let _ = std::io::stdout().flush();
}

extern "C" fn start(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    let mut thread = THREAD.lock().unwrap();
    if thread.is_some() {
        return 0;
    }
    for other in ["webserver", "web", "server", "webui", "acp"] {
        if enabled(flag(other).as_ref()) {
            eprintln!("[rpi-http] --http cannot be combined with --{other}");
            finish(2);
            return 0;
        }
    }
    let mut args = http_args(flag("http"));
    append_service_flags(&mut args);
    let mut config = match config::from_args(args) {
        Ok(Launch::Run(config)) => *config,
        Ok(Launch::Help) => {
            println!("rpi --http[=CONFIG|help]\n{}", rpi_httpcore::config::HELP);
            finish(0);
            return 0;
        }
        Ok(Launch::Version) => {
            finish(0);
            return 0;
        }
        Err(error) => {
            eprintln!("[rpi-http] {error}");
            finish(2);
            return 0;
        }
    };
    // The native child host must be the same rpi binary that loaded the plugin.
    if config.rpi_bin == std::path::Path::new("rpi") {
        match std::env::current_exe() {
            Ok(path) => config.rpi_bin = path,
            Err(error) => {
                eprintln!("[rpi-http] {error}");
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
            Ok(runtime) => runtime.block_on(rpi_httpcore::serve_until(
                config,
                axum::Router::new(),
                report,
                async move {
                    if !*receiver.borrow_and_update() {
                        let _ = receiver.changed().await;
                    }
                },
            )),
            Err(error) => Err(error.into()),
        };
        let code = match result {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("[rpi-http] {error}");
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
            "http",
            "Start the API-only HTTP/SSE service; optional configuration path or help",
        ),
        (
            "bind",
            "HTTP listen address (overrides the HTTP configuration), used with --http",
        ),
        (
            "port",
            "HTTP listen port (overrides the HTTP configuration), used with --http",
        ),
    ] {
        if register_flag(
            StbStringRef::from_str(name),
            StbStringRef::from_str(description),
        ) != 0
        {
            return 1;
        }
    }
    if !enabled(flag("http").as_ref()) {
        return 0;
    }
    if action(RuntimeActionId::StdioLifecycle, json!({"op":"status"})).is_none() {
        return 1;
    }
    let (Some(declare), Some(handler)) = (api.declare, api.register_event_handler) else {
        return 1;
    };
    // The existing ABI claim: normal model configuration still applies, as it
    // does for `--webserver`; no `requiresAgent:false` bypass is requested.
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
    fn inactive_flags_do_not_start_a_service_in_rpc_children() {
        for value in [
            None,
            Some(Value::Null),
            Some(json!(false)),
            Some(json!("false")),
            Some(json!("")),
        ] {
            assert!(!enabled(value.as_ref()), "{value:?}");
        }
        for value in [
            json!(true),
            json!("true"),
            json!("help"),
            json!("/path/http.json"),
        ] {
            assert!(enabled(Some(&value)), "{value:?}");
        }
    }

    #[test]
    fn http_modes_reuse_the_existing_http_cli_contract() {
        assert!(http_args(None).is_empty());
        assert!(http_args(Some(json!(true))).is_empty());
        assert!(http_args(Some(json!("true"))).is_empty());
        assert_eq!(http_args(Some(json!("help"))), ["--help"]);
        assert_eq!(
            http_args(Some(json!("/path/http.json"))),
            ["--config", "/path/http.json"]
        );
        assert!(matches!(
            config::from_args(http_args(Some(json!("help")))).unwrap(),
            Launch::Help
        ));
        assert!(matches!(
            config::from_args(http_args(Some(json!("true")))).unwrap(),
            Launch::Run(_)
        ));
    }

    #[test]
    fn host_listener_flags_become_http_options() {
        assert_eq!(flag_text(json!("3031")).as_deref(), Some("3031"));
        assert_eq!(flag_text(json!(3031)).as_deref(), Some("3031"));
        assert_eq!(flag_text(json!(true)), None);
        assert_eq!(flag_text(json!("")), None);
        let options = service_options(Some("0.0.0.0".into()), Some("3031".into()));
        assert_eq!(options, ["--bind", "0.0.0.0", "--port", "3031"]);
        let Launch::Run(config) = config::from_args(options).unwrap() else {
            panic!("expected a run launch");
        };
        assert_eq!(config.bind, "0.0.0.0");
        assert_eq!(config.port, 3031);
        assert!(service_options(None, None).is_empty());
    }
}
