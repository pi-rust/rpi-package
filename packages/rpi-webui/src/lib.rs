//! Start an existing Open WebUI installation. Agent sessions stay in `rpi-acp`.
#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod launcher;
mod openwebui;

use rpi_plugin_sdk::{EventTag, PluginApi, RuntimeActionId, StbString, StbStringRef};
use std::sync::{mpsc, Mutex, OnceLock};

static API: OnceLock<PluginApi> = OnceLock::new();
type Worker = (mpsc::Sender<()>, std::thread::JoinHandle<()>);
static WORKER: Mutex<Option<Worker>> = Mutex::new(None);

fn cli_flag(name: &str) -> Option<serde_json::Value> {
    let api = API.get()?;
    let action = api.runtime_action;
    let mut out = StbString::empty();
    let rc = action(
        RuntimeActionId::GetCliFlag as u32,
        StbStringRef::from_str(&serde_json::json!({"name": name}).to_string()),
        &mut out,
        api.user_data,
    );
    let value = out.to_string_lossy();
    (api.free_string)(out);
    if rc != 0 {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&value) else {
        return None;
    };
    Some(value.get("value").unwrap_or(&value).clone())
}

fn enabled_flag() -> bool {
    cli_flag("webui")
        .is_some_and(|v| v == true || v.as_str().is_some_and(|s| !matches!(s, "false" | "0")))
}

fn start() -> Result<(), String> {
    if launcher::env_bool("RPI_WEBUI_CHILD", false) {
        return Ok(());
    }
    let mut worker = WORKER.lock().unwrap_or_else(|p| p.into_inner());
    if worker
        .as_ref()
        .is_some_and(|(_, thread)| !thread.is_finished())
    {
        return Ok(());
    }
    if let Some((_, thread)) = worker.take() {
        let _ = thread.join();
    }
    let config = launcher::Config::from_options(cli_flag)?;
    let (stop, receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        if let Err(error) = launcher::run(config, receiver) {
            eprintln!("[rpi-webui] {error}");
        }
    });
    *worker = Some((stop, thread));
    Ok(())
}

fn stop() {
    if let Some((stop, thread)) = WORKER.lock().unwrap_or_else(|p| p.into_inner()).take() {
        let _ = stop.send(());
        let _ = thread.join();
    }
}

rpi_plugin_sdk::event_handler! {
fn event(event, _user_data) {
    match event.tag() {
        EventTag::SessionShutdown => stop(),
        EventTag::SessionStart if enabled_flag() => {
            if let Err(error) = start() { eprintln!("[rpi-webui] {error}"); }
        },
        EventTag::BeforeTuiStart if launcher::env_bool("RPI_WEBUI_AUTOSTART", false) => {
            if let Err(error) = start() { eprintln!("[rpi-webui] {error}"); }
        },
        _ => {},
    }
    0
}
}

rpi_plugin_sdk::export_plugin!(|api| {
    let _ = API.set(*api);
    if cli_flag("webui-acp") == Some(serde_json::Value::Bool(true)) {
        return rpi_acp_core::register_embedded(api);
    }
    let Some(register_flag) = api.register_flag else {
        return 1;
    };
    for (name, description) in [
        (
            "webui",
            "Launch and connect a UI adapter (default: openwebui)",
        ),
        ("webui-url", "Connect to an existing Open WebUI URL"),
        (
            "webui-workspace",
            "Agent workspace (default: current directory)",
        ),
        (
            "webui-permission",
            "Tool permission mode: ask (default) or bypass",
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
    let Some(register) = api.register_event_handler else {
        return 1;
    };
    for tag in [
        EventTag::SessionStart,
        EventTag::BeforeTuiStart,
        EventTag::SessionShutdown,
    ] {
        if register(tag, event, std::ptr::null_mut()) != 0 {
            return 1;
        }
    }
    0
});
