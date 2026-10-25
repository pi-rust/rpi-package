//! `rpi-acp` — run `rpi` as an external **Agent Client Protocol** agent.
//!
//! Zed (and any other ACP client) launches `rpi --acp` and speaks ACP v1
//! JSON-RPC over the process's stdin/stdout. This extension makes that work:
//!
//! 1. During `register` (which the host runs *before* it decides whether to
//!    read piped stdin) it reads the `--acp` flag and, when set, `declare`s
//!    `{"ownsStdio": true}`. The host then defers `read_piped_stdin` and runs
//!    its headless `Rpc` lifecycle, leaving this process's stdio to us.
//! 2. On `SessionStart` it runs the one-shot actions (Zed registration or the
//!    `--setup` credential flow) or starts the ACP server ([`protocol::run`]) on a
//!    dedicated thread. The server handles `initialize` / `session/new` /
//!    `session/prompt` / `session/cancel` / `session/load` / `session/resume`
//!    / `session/durable`.
//! 3. Each ACP session is backed by a child `rpi --mode rpc` process rooted at
//!    the session's `cwd`; its event stream is translated into ACP
//!    `session/update` notifications (see [`session`]).
//!
//! The host waits for the exclusive stdio owner to finish. EOF reports the
//! server's exit code through the generic StdioLifecycle action; SessionShutdown
//! stops and joins the server thread before the plugin is unloaded.

#![forbid(unsafe_code)]
// The SDK's `export_plugin!` macro expands to the `pub extern "C" fn
// rpi_plugin_register(*const PluginApi)` entrypoint and validates/dereferences
// the pointer via `register_entrypoint`; clippy attributes that generated deref
// here. The ABI signature is fixed, so the lint is not actionable.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

// `pub` so the standalone `rpi-acp` binary (`src/main.rs`) can run the same ACP
// server the extension embeds.
pub mod auth;
pub mod diag;
mod history;
pub mod protocol;
mod questions;
pub mod session;
pub mod zed;

use rpi_plugin_sdk::{
    EventTag, FreeStringFn, RuntimeActionFn, RuntimeActionId, StablePluginEvent, StbString,
    StbStringRef,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

/// The CLI flag that turns an ordinary `rpi` invocation into an ACP agent.
const ACP_FLAG: &str = "acp";
const BYPASS_FLAG: &str = "bypass";
/// One-shot: register rpi in Zed's settings, then exit.
const INSTALL_ZED_FLAG: &str = "install-zed";
/// One-shot: print the Zed settings snippet, then exit.
const PRINT_ZED_FLAG: &str = "print-zed";
/// One-shot: run the interactive credential setup that backs ACP's `terminal`
/// auth method, then exit `0` only once the provider is verified usable.
const SETUP_FLAG: &str = "setup";

// Host callbacks captured during `register`. The `PluginApi` pointer is only
// valid for that call, so the function pointers + the opaque `user_data` (the
// host's action bridge) are copied out here, exactly like `rpi-server` /
// `rpi-im-message`. They are used to read the parsed CLI flag before the host
// decides whether to drain stdin.
static RUNTIME_ACTION: OnceLock<RuntimeActionFn> = OnceLock::new();
static RUNTIME_USER_DATA: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static HOST_FREE_STRING: OnceLock<FreeStringFn> = OnceLock::new();

/// Set once `--acp` was seen during register.
static ACP_ENABLED: AtomicBool = AtomicBool::new(false);
/// Set once `--install-zed` was seen: register and exit instead of serving.
static INSTALL_ZED: AtomicBool = AtomicBool::new(false);
/// Set once `--print-zed` was seen: print the snippet and exit.
static PRINT_ZED: AtomicBool = AtomicBool::new(false);
/// Set once `--setup` was seen: run credential setup and exit instead of serving.
static SETUP: AtomicBool = AtomicBool::new(false);
/// Guards against starting the server twice (SessionStart can fire on reload).
static SERVER_STARTED: AtomicBool = AtomicBool::new(false);
static SERVER_STOP: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();
static SERVER_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

fn lifecycle_action(op: &str, code: i32) -> bool {
    let Some(action) = RUNTIME_ACTION.get() else {
        return false;
    };
    let args = json!({"op": op, "exitCode": code}).to_string();
    let mut out = StbString::empty();
    let rc = action(
        RuntimeActionId::StdioLifecycle as u32,
        StbStringRef::from_str(&args),
        &mut out,
        RUNTIME_USER_DATA.load(Ordering::Acquire),
    );
    if let Some(free) = HOST_FREE_STRING.get() {
        free(out);
    }
    rc == 0
}

extern "C" fn on_session_shutdown(_: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if let Some(stop) = SERVER_STOP.get() {
        let _ = stop.send(true);
    }
    if let Some(thread) = SERVER_THREAD.lock().unwrap().take() {
        let _ = thread.join();
    }
    0
}

/// Read a parsed CLI flag through the host's `GetCliFlag` runtime action.
fn get_cli_flag(name: &str) -> Option<Value> {
    let runtime_action = *RUNTIME_ACTION.get()?;
    let user_data = RUNTIME_USER_DATA.load(Ordering::Acquire);
    if user_data.is_null() {
        return None;
    }

    let args = json!({"name": name}).to_string();
    let args_ref = StbStringRef::from_str(&args);
    let mut out = StbString::empty();

    let rc = runtime_action(
        RuntimeActionId::GetCliFlag as u32,
        args_ref,
        &mut out,
        user_data,
    );

    let result_str = if rc == 0 {
        Some(out.to_string_lossy())
    } else {
        None
    };
    if let Some(free) = HOST_FREE_STRING.get() {
        free(out);
    }

    let value: Value = serde_json::from_str(&result_str?).ok()?;
    value.get("value").cloned()
}

/// Whether a `GetCliFlag` result means the switch is ON. A bare `--flag` yields
/// `true`; `--flag=value` yields a string; unset yields `null`.
fn flag_is_on(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(on)) => *on,
        Some(Value::String(text)) => !text.is_empty() && !text.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

/// `SessionStart`: run the one-shot Zed actions, or bring up the ACP server.
///
/// Dispatched by the host after it has loaded extensions and (because of the
/// `ownsStdio` claim) decided to run the headless lifecycle with stdio left to us.
///
/// Make Zed able to launch rpi without the user hand-editing `settings.json`.
///
/// Only reached under `--acp`, i.e. when the user has explicitly asked for Zed
/// integration, and a no-op once `agent_servers.rpi` exists (so a Zed-launched
/// child, which is already registered, never writes). The edit preserves the
/// file's comments and formatting and is backed up — see [`zed`].
///
/// `RPI_ACP_NO_AUTO_REGISTER_ZED=1` refuses the write and prints the snippet
/// instead, for users who keep their editor config under version control.
fn ensure_zed_registration() {
    let Some(path) = zed::settings_path() else {
        return;
    };
    let registration = zed::Registration::detect();
    if env_flag("RPI_ACP_NO_AUTO_REGISTER_ZED") {
        let registered = std::fs::read_to_string(&path)
            .map(|text| matches!(zed::ensure_registered(&text, &registration), Ok(None)))
            .unwrap_or(true);
        if !registered {
            diagln!(
                "[rpi-acp] rpi is not registered in {}. Add this to that file:\n{}",
                path.display(),
                registration.snippet()
            );
        }
        return;
    }
    match zed::register_file(&path, &registration, false) {
        Ok(zed::Outcome::AlreadyPresent) => {}
        Ok(zed::Outcome::Created) => {
            diagln!("[rpi-acp] registered rpi in {}", path.display());
        }
        Ok(zed::Outcome::Updated { .. }) => {
            diagln!(
                "[rpi-acp] added rpi to {} (backup: settings.json.rpi-bak)",
                path.display()
            );
        }
        // Never fail an ACP start over this: a user can always edit by hand.
        Err(error) => diagln!("[rpi-acp] could not update {}: {error}", path.display()),
    }
}

/// Report a one-shot action's status to the host, which then exits with it.
fn finish_with(code: i32) -> i32 {
    if !lifecycle_action("finish", code) {
        diagln!("[rpi-acp] failed to finish the host stdio lifecycle");
    }
    0
}

/// Treat unset/empty/`0`/`false` as off, anything else as on.
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

extern "C" fn on_session_start(event: StablePluginEvent, _: *mut std::ffi::c_void) -> i32 {
    if event.tag != EventTag::SessionStart {
        return 0;
    }
    // `--install-zed` / `--print-zed` / `--setup` are one-shot: do the work, hand
    // the host a status code, and let it exit. They take precedence over `--acp`,
    // which is what lets an ACP client's terminal-auth invocation
    // (`rpi --acp --setup`) run setup instead of a second server.
    if SETUP.load(Ordering::Acquire) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        return finish_with(auth::run(&args));
    }
    if INSTALL_ZED.load(Ordering::Acquire) || PRINT_ZED.load(Ordering::Acquire) {
        let write = INSTALL_ZED.load(Ordering::Acquire);
        return finish_with(zed::run_default_action(write));
    }
    if !ACP_ENABLED.load(Ordering::Acquire) {
        return 0;
    }
    if SERVER_STARTED.swap(true, Ordering::AcqRel) {
        return 0;
    }

    // Take stdin *here*, synchronously, before spawning the server: a terminal
    // cannot carry ACP, and the host has already parked in its headless
    // lifecycle by now. Failing later (inside the server thread) would print the
    // host's "running in headless mode; Ctrl+C to stop" line and look like a
    // hang; failing here prints one actionable message and exits.
    let (input, pending) = match protocol::acp_stdin() {
        Ok(taken) => taken,
        Err(error) => {
            diagln!("[rpi-acp] {error}");
            diagln!(
                "[rpi-acp] --acp speaks ACP on stdin, so it needs a client (Zed) on the \
                 other end of a pipe."
            );
            diagln!("[rpi-acp] To register rpi in Zed from a terminal, run: rpi --install-zed");
            return finish_with(2);
        }
    };

    diagln!("[rpi-acp] ACP stdio server starting");
    diagln!(
        "[rpi-acp] pid {} = {} (cwd {})",
        std::process::id(),
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "?".into()),
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "?".into()),
    );
    ensure_zed_registration();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let _ = SERVER_STOP.set(stop);
    let server = thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                diagln!("[rpi-acp] failed to create tokio runtime: {error}");
                lifecycle_action("finish", 2);
                return;
            }
        };

        let code = runtime.block_on(protocol::run(receiver, input, pending));
        if !lifecycle_action("finish", code) {
            diagln!("[rpi-acp] failed to finish the host stdio lifecycle");
        }
    });
    *SERVER_THREAD.lock().unwrap() = Some(server);

    0
}

fn register_plugin(api: &rpi_plugin_sdk::PluginApi) -> i32 {
    let _ = RUNTIME_ACTION.set(api.runtime_action);
    RUNTIME_USER_DATA.store(api.user_data, Ordering::Release);
    let _ = HOST_FREE_STRING.set(api.free_string);

    // Document the flags. They are always readable via `GetCliFlag` (unknown
    // long flags are retained by the parser), but registering them puts them in
    // the host's flag list/help.
    if let Some(register_flag) = api.register_flag {
        let _ = register_flag(
            StbStringRef::from_str(ACP_FLAG),
            StbStringRef::from_str("Run as an ACP (Agent Client Protocol) stdio agent"),
        );
        let _ = register_flag(
            StbStringRef::from_str(BYPASS_FLAG),
            StbStringRef::from_str(
                "Skip permission-gate approvals for this process and ACP sessions",
            ),
        );
        let _ = register_flag(
            StbStringRef::from_str(INSTALL_ZED_FLAG),
            StbStringRef::from_str("Register rpi in Zed's settings.json, then exit"),
        );
        let _ = register_flag(
            StbStringRef::from_str(PRINT_ZED_FLAG),
            StbStringRef::from_str("Print the Zed settings snippet, then exit"),
        );
        let _ = register_flag(
            StbStringRef::from_str(SETUP_FLAG),
            StbStringRef::from_str("Set up provider credentials, then exit"),
        );
    }

    // Decide NOW, during register: the host only learns whether to defer
    // `read_piped_stdin` from the `ownsStdio` declaration, which must be in the
    // registry before `build()` returns.
    let acp = flag_is_on(get_cli_flag(ACP_FLAG).as_ref());
    session::set_permission_bypass(flag_is_on(get_cli_flag(BYPASS_FLAG).as_ref()));
    let install = flag_is_on(get_cli_flag(INSTALL_ZED_FLAG).as_ref());
    let print = flag_is_on(get_cli_flag(PRINT_ZED_FLAG).as_ref());
    let setup = flag_is_on(get_cli_flag(SETUP_FLAG).as_ref());
    if !acp && !install && !print && !setup {
        return 0;
    }

    if !lifecycle_action("status", 0) {
        diagln!("[rpi-acp] host must support the StdioLifecycle runtime action");
        return 1;
    }
    let Some(declare) = api.declare else {
        diagln!(
            "[rpi-acp] this rpi host does not support the `declare` ABI; \
             cannot claim stdio (upgrade rpi)"
        );
        return 1;
    };
    let payload = json!({"ownsStdio": true}).to_string();
    if declare(StbStringRef::from_str(&payload)) != 0 {
        diagln!("[rpi-acp] host rejected the `ownsStdio` declaration");
        return 1;
    }

    // One-shot actions own the headless lifecycle without starting an ACP server.
    INSTALL_ZED.store(install, Ordering::Release);
    PRINT_ZED.store(print, Ordering::Release);
    SETUP.store(setup, Ordering::Release);
    ACP_ENABLED.store(acp && !install && !print && !setup, Ordering::Release);

    if let Some(register) = api.register_event_handler {
        let rc = register(
            EventTag::SessionShutdown,
            on_session_shutdown,
            api.user_data,
        );
        if rc != 0 {
            return rc;
        }
        let rc = register(
            EventTag::SessionStart,
            on_session_start,
            std::ptr::null_mut(),
        );
        if rc != 0 {
            return rc;
        }
    }

    0
}

rpi_plugin_sdk::export_plugin!(register_plugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_is_on_handles_bool_string_and_null() {
        assert!(flag_is_on(Some(&json!(true))));
        assert!(flag_is_on(Some(&json!("true"))));
        assert!(flag_is_on(Some(&json!("yes"))));
        assert!(!flag_is_on(Some(&json!(false))));
        assert!(!flag_is_on(Some(&json!("false"))));
        assert!(!flag_is_on(Some(&json!(""))));
        assert!(!flag_is_on(Some(&Value::Null)));
        assert!(!flag_is_on(None));
    }
}
