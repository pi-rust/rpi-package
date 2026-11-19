//! rpi-office — drive Excel, Word and PowerPoint through the pi-for-office
//! task-pane add-in.
//!
//! This is the rpi port of the upstream Pi extension
//! `@dieulc/pi-office-bridge` (MIT, pi-for-office). It runs a loopback
//! WebSocket server inside the rpi process so the unchanged add-in can attach,
//! registers one `office_<host>_<op>` tool per catalog op, proxies each call to
//! the pane that runs Office.js, and streams pane-initiated replies back.
//!
//! The server is **off by default**: a session never claims the port until the
//! user runs `/office connect` (or passes `--office-bridge-autostart`), so
//! several sessions can coexist without silently fighting over 38617.
//!
//! Requires `rpi-plugin-sdk` 0.25 (ABI 4, `struct_size` 192): the host actions
//! and slots this extension relies on (`Abort`, `register_plugin_lifecycle`,
//! `register_tool_prompt`) were appended after the older 0.3.x line. On a host
//! with a shorter vtable the SDK reports those slots as `None` and the plugin
//! degrades: no `client-abort`, no prompt snippets, and no reload-safe join.
// Every ABI shape this extension touches goes through an SDK macro
// (`event_handler!`, `string_handler!`, `execute_handler!`, `call_action!`,
// `stb_text!`), which keeps the raw-pointer contract inside the SDK. `forbid`
// rather than `deny` so a future `#[allow(unsafe_code)]` cannot quietly reopen
// the door. The entrypoint the macro below emits takes a raw `*const PluginApi`
// by ABI definition, which is why the pointer-deref lint is allowed here; the
// dereference itself happens inside the SDK's macro.
#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod active_tools;
pub mod bridge;
pub mod catalog;
pub mod commands;
pub mod config;
pub mod lifecycle;
pub mod pane_replies;
pub mod protocol;
pub mod server;
pub mod tools;

use active_tools::PaneCapability;
use bridge::{HealthMeta, Pane};
use lifecycle::{BridgeSnapshot, Controller};
use pane_replies::{format_document_tag, PanePromptEntry, PanePromptTracker};
use rpi_plugin_sdk::{
    BeforeAgentStartFn, CommandHandlerFn, EventHandlerFn, EventTag, FreeStringFn, PluginApi,
    RuntimeActionId, StbStringRef,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Name advertised in `welcome` and `/health` (upstream compatibility).
const SERVER_NAME: &str = "pi-office-bridge";
/// This extension's own version, reported to the pane.
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Footer status key owned by this extension.
const STATUS_KEY: &str = "office-bridge";
/// Deltas are coalesced into ~frame-sized batches.
const DELTA_FLUSH_MS: u64 = 60;

/* ── host API storage ──────────────────────────────────────────────── */

static API: OnceLock<Mutex<Option<PluginApi>>> = OnceLock::new();

fn api_store() -> &'static Mutex<Option<PluginApi>> {
    API.get_or_init(|| Mutex::new(None))
}

/// Resolve the host function table, rebinding `user_data` to the callback's
/// owner (a native reload may present a different owner than registration).
fn api(owner: *mut c_void) -> Option<PluginApi> {
    let mut api = {
        let stored = api_store()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        (*stored)?
    };
    if !owner.is_null() {
        api.user_data = owner;
    }
    Some(api)
}

fn call_action(api: PluginApi, action: RuntimeActionId, args: Value) -> Result<Value, String> {
    let encoded = args.to_string();
    // `call_action!` owns the allocate / call / read / free sequence, so this
    // crate never touches a raw ABI pointer for a runtime action.
    let (rc, text) = rpi_plugin_sdk::call_action!(api, action, &encoded);
    if rc != 0 {
        return Err(if text.trim().is_empty() {
            format!("runtime action {action:?} failed")
        } else {
            text
        });
    }
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&text).map_err(|error| format!("invalid runtime response: {error}"))
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn cli_flag(api: PluginApi, name: &str) -> Option<Value> {
    call_action(api, RuntimeActionId::GetCliFlag, json!({"name": name}))
        .ok()
        .and_then(|value| value.get("value").cloned())
        .filter(|value| !value.is_null())
}

fn pi_version() -> Option<String> {
    env_value("RPI_VERSION").or_else(|| env_value("PI_VERSION"))
}

fn health_meta(port: u16) -> HealthMeta {
    HealthMeta {
        service: SERVER_NAME.into(),
        server_version: SERVER_VERSION.into(),
        pi_version: pi_version(),
        port,
        started_at: protocol::now_ms(),
    }
}

/* ── plugin state ──────────────────────────────────────────────────── */

#[derive(Default)]
struct DeltaState {
    pane_id: Option<String>,
    prompt_id: Option<String>,
    buffer: String,
}

impl DeltaState {
    fn clear(&mut self) {
        self.pane_id = None;
        self.prompt_id = None;
        self.buffer.clear();
    }
}

struct State {
    controller: Controller,
    /// Pane-prompt correlation; the key is the pane id replies route to.
    prompts: PanePromptTracker<String>,
    delta: DeltaState,
    port: u16,
    autostart: bool,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| {
        Mutex::new(State {
            controller: Controller::new(protocol::BRIDGE_DEFAULT_PORT),
            prompts: PanePromptTracker::default(),
            delta: DeltaState::default(),
            port: protocol::BRIDGE_DEFAULT_PORT,
            autostart: false,
        })
    })
}

fn with_state<R>(body: impl FnOnce(&mut State) -> R) -> R {
    let mut state = state().lock().unwrap_or_else(|poison| poison.into_inner());
    body(&mut state)
}

fn server_lookup() -> Option<Arc<server::Server>> {
    with_state(|state| state.controller.server().cloned())
}

fn panes_lookup() -> Vec<Pane> {
    with_state(|state| state.controller.panes())
}

/* ── worker: the only thread that calls back into the host ─────────── */

enum Job {
    UserMessage {
        pane: Pane,
        text: String,
    },
    PanesChanged,
    Delta {
        pane_id: String,
        prompt_id: String,
        text: String,
    },
    Flush,
    Final {
        pane_id: String,
        prompt_id: String,
        text: String,
        message_id: Option<String>,
    },
    ToolActivity {
        pane_id: String,
        tool: String,
        status: String,
        summary: Option<String>,
    },
    /// A pane asked to abort the run answering its prompt.
    Abort {
        prompt_id: Option<String>,
    },
    /// `finish_shutdown` asked the worker to exit; reply once it has stopped.
    Shutdown(Sender<()>),
}

struct Worker {
    tx: Sender<Job>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

static WORKER: OnceLock<Worker> = OnceLock::new();

fn worker_state() -> &'static Worker {
    WORKER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let join = std::thread::Builder::new()
            .name("rpi-office-worker".into())
            .spawn(move || run_worker(rx))
            .expect("spawn rpi-office worker");
        Worker {
            tx,
            join: Mutex::new(Some(join)),
        }
    })
}

fn worker() -> &'static Sender<Job> {
    &worker_state().tx
}

/// Stop and join the worker so no plugin code can run after `finish_shutdown`.
fn shutdown_worker() -> bool {
    let Some(worker) = WORKER.get() else {
        return true;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    if worker.tx.send(Job::Shutdown(tx)).is_ok() {
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }
    let handle = worker
        .join
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    if let Some(handle) = handle {
        let _ = handle.join();
    }
    true
}

fn run_worker(rx: Receiver<Job>) {
    loop {
        match rx.recv_timeout(Duration::from_millis(DELTA_FLUSH_MS)) {
            Ok(Job::UserMessage { pane, text }) => inject_pane_prompt(&pane, &text),
            Ok(Job::PanesChanged) => {
                if let Some(api) = api(std::ptr::null_mut()) {
                    reconcile_tools(api);
                    refresh_status(api);
                }
            }
            Ok(Job::Delta {
                pane_id,
                prompt_id,
                text,
            }) => buffer_delta(&pane_id, &prompt_id, &text),
            Ok(Job::Flush) => flush_deltas(),
            Ok(Job::Final {
                pane_id,
                prompt_id,
                text,
                message_id,
            }) => {
                flush_deltas();
                push_to_pane(
                    &pane_id,
                    protocol::agent_final(&text, Some(&prompt_id), message_id.as_deref()),
                );
                if let Some(api) = api(std::ptr::null_mut()) {
                    refresh_status(api);
                }
            }
            Ok(Job::ToolActivity {
                pane_id,
                tool,
                status,
                summary,
            }) => push_to_pane(
                &pane_id,
                protocol::tool_activity(&tool, &status, summary.as_deref()),
            ),
            Ok(Job::Abort { prompt_id }) => abort_run(prompt_id.as_deref()),
            Ok(Job::Shutdown(done)) => {
                flush_deltas();
                let _ = done.send(());
                break;
            }
            Err(RecvTimeoutError::Timeout) => flush_deltas(),
            Err(RecvTimeoutError::Disconnected) => {
                flush_deltas();
                break;
            }
        }
    }
}

/// Inject a pane prompt into the shared conversation.
fn inject_pane_prompt(pane: &Pane, text: &str) {
    let Some(api) = api(std::ptr::null_mut()) else {
        return;
    };
    let prompt_id = protocol::next_call_id("prompt");
    let sent_text = format!(
        "{}\n{}",
        format_document_tag(
            pane.host.as_str(),
            pane.document_name.as_deref(),
            pane.document_id.as_deref()
        ),
        text
    );
    with_state(|state| {
        state.prompts.enqueue(PanePromptEntry {
            pane: pane.pane_id.clone(),
            prompt_id: prompt_id.clone(),
            sent_text: sent_text.clone(),
        })
    });
    // followUp: delivered immediately when idle (and starts a turn), queued
    // until the current turn settles when busy.
    let result = call_action(
        api,
        RuntimeActionId::SendMessage,
        json!({"text": sent_text, "triggerTurn": true, "deliverAs": "followUp"}),
    );
    if let Err(error) = result {
        eprintln!("[rpi-office] could not inject the pane prompt: {error}");
    }
    refresh_status(api);
}

/// Abort the in-flight run on behalf of a pane's Stop button.
///
/// The host's `Abort` is cooperative, so late stream events may still arrive;
/// the local reply binding is cleared by `on_abort_request` before this runs.
/// The response's `aborted` flag is only logged: on this host it is `true` even
/// for a second call on the same run, so it must not drive any control flow.
fn abort_run(prompt_id: Option<&str>) {
    let Some(api) = api(std::ptr::null_mut()) else {
        return;
    };
    match call_action(api, RuntimeActionId::Abort, json!({})) {
        Ok(value) => {
            eprintln!(
                "[rpi-office] abort requested (prompt {}) → aborted={} run={} steer={} followUp={}",
                prompt_id.unwrap_or("-"),
                value["aborted"],
                value["runId"].as_str().unwrap_or("-"),
                value["steer"].as_array().map(Vec::len).unwrap_or(0),
                value["followUp"].as_array().map(Vec::len).unwrap_or(0),
            );
        }
        Err(error) => eprintln!(
            "[rpi-office] abort failed (prompt {}): {error}",
            prompt_id.unwrap_or("-")
        ),
    }
    refresh_status(api);
}

fn buffer_delta(pane_id: &str, prompt_id: &str, text: &str) {
    with_state(|state| {
        let switched = state.delta.pane_id.as_deref() != Some(pane_id)
            || state.delta.prompt_id.as_deref() != Some(prompt_id);
        if switched {
            state.delta.clear();
            state.delta.pane_id = Some(pane_id.to_string());
            state.delta.prompt_id = Some(prompt_id.to_string());
        }
        state.delta.buffer.push_str(text);
    });
}

fn flush_deltas() {
    let pending = with_state(|state| {
        if state.delta.buffer.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut state.delta.buffer);
        Some((
            state.delta.pane_id.clone(),
            state.delta.prompt_id.clone(),
            text,
        ))
    });
    let Some((Some(pane_id), prompt_id, text)) = pending else {
        return;
    };
    push_to_pane(
        &pane_id,
        protocol::agent_delta(&text, prompt_id.as_deref(), None),
    );
}

fn push_to_pane(pane_id: &str, message: Value) {
    if let Some(server) = server_lookup() {
        let _ = server.push_to_pane(pane_id, message);
    }
}

/* ── active tools + status line ────────────────────────────────────── */

fn active_tool_names(api: PluginApi) -> Result<Vec<String>, String> {
    let value = call_action(api, RuntimeActionId::GetActiveTools, json!({}))?;
    value
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .ok_or_else(|| "runtime did not return an active tool list".to_string())
}

/// Reconcile the host's active tool set with the bridge lifecycle and panes.
fn reconcile_tools(api: PluginApi) {
    let Ok(current) = active_tool_names(api) else {
        return;
    };
    let (enabled, panes) = with_state(|state| {
        (
            state.controller.is_on(),
            state
                .controller
                .panes()
                .into_iter()
                .map(|pane| PaneCapability {
                    host: pane.host,
                    ops: pane.ops.clone(),
                })
                .collect::<Vec<_>>(),
        )
    });
    let next = active_tools::reconcile_bridge_tool_activation(&current, enabled, &panes);
    if next != current {
        if let Err(error) =
            call_action(api, RuntimeActionId::SetActiveTools, json!({"tools": next}))
        {
            eprintln!("[rpi-office] could not update the active tools: {error}");
        }
    }
}

/// Compact footer status line (`office: Excel · "Book" attached (bridge :38617)`).
fn status_line(snapshot: &BridgeSnapshot, panes: &[Pane]) -> Option<String> {
    match snapshot.state {
        lifecycle::LifecycleState::Off => None,
        lifecycle::LifecycleState::Starting => {
            Some(format!("office bridge starting on :{}…", snapshot.port))
        }
        lifecycle::LifecycleState::Closing => Some("office bridge closing…".into()),
        lifecycle::LifecycleState::On => {
            let port = snapshot.actual_port.unwrap_or(snapshot.port);
            if panes.is_empty() {
                return Some(format!("office bridge on :{port} — no app attached"));
            }
            let mut counts: Vec<(String, usize)> = Vec::new();
            for pane in panes {
                let label = pane.host.label().to_string();
                match counts.iter_mut().find(|(name, _)| name == &label) {
                    Some((_, count)) => *count += 1,
                    None => counts.push((label, 1)),
                }
            }
            let labels = counts
                .into_iter()
                .map(|(label, count)| {
                    if count > 1 {
                        format!("{label}×{count}")
                    } else {
                        label
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("office: {labels} attached (bridge :{port})"))
        }
    }
}

fn refresh_status(api: PluginApi) {
    let (snapshot, panes) =
        with_state(|state| (state.controller.snapshot(), state.controller.panes()));
    let line = status_line(&snapshot, &panes);
    let mut args = json!({"key": STATUS_KEY});
    if let Some(line) = line {
        args["value"] = json!(line);
    }
    if let Err(error) = call_action(api, RuntimeActionId::SetStatus, args) {
        eprintln!("[rpi-office] could not update the status line: {error}");
    }
}

/* ── server hooks (never call the host inline: they enqueue) ────────── */

struct PluginHooks;

impl server::Hooks for PluginHooks {
    fn on_user_message(&self, text: &str, pane: &Pane) {
        let _ = worker().send(Job::UserMessage {
            pane: pane.clone(),
            text: text.to_string(),
        });
    }

    fn on_abort_request(&self, prompt_id: Option<&str>, pane: &Pane) {
        // Release the local reply binding first so a late delta or final cannot
        // land in the pane's stopped buffer, then ask the host to abort the run
        // (`client-abort`, backed by `RuntimeActionId::Abort`).
        let matched = with_state(|state| {
            if let Some(active) = state.prompts.active() {
                let same_pane = active.pane == pane.pane_id;
                let same_prompt = prompt_id
                    .map(|prompt_id| prompt_id == active.prompt_id)
                    .unwrap_or(true);
                if same_pane && same_prompt {
                    state.prompts.complete();
                    state.delta.clear();
                    return true;
                }
            }
            false
        });
        if matched {
            let _ = worker().send(Job::Abort {
                prompt_id: prompt_id.map(str::to_owned),
            });
        }
    }

    fn on_panes_changed(&self, _panes: &[Pane]) {
        let _ = worker().send(Job::PanesChanged);
    }

    fn on_log(&self, message: &str) {
        eprintln!("[rpi-office] {message}");
    }
}

fn hooks() -> Arc<dyn server::Hooks> {
    static HOOKS: OnceLock<Arc<PluginHooks>> = OnceLock::new();
    HOOKS.get_or_init(|| Arc::new(PluginHooks)).clone() as Arc<dyn server::Hooks>
}

/* ── lifecycle helpers shared by commands and events ───────────────── */

fn connect_bridge(port: Option<u16>, owner: *mut c_void) -> Result<BridgeSnapshot, String> {
    let snapshot = with_state(|state| state.controller.connect(port, &health_meta, hooks()))?;
    if let Some(api) = api(owner) {
        reconcile_tools(api);
        refresh_status(api);
    }
    Ok(snapshot)
}

fn close_bridge(owner: *mut c_void) -> Result<BridgeSnapshot, String> {
    let snapshot = with_state(|state| {
        state.prompts.clear();
        state.delta.clear();
        state.controller.close()
    })?;
    if let Some(api) = api(owner) {
        reconcile_tools(api);
        refresh_status(api);
    }
    Ok(snapshot)
}

/* ── content helpers ───────────────────────────────────────────────── */

/// Flatten a message content payload to display text.
fn flatten_text_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.trim().to_string(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part["type"].as_str() {
                Some("text") | Some("thinking") => part["text"].as_str(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
        _ => String::new(),
    }
}

/// Extract a `text_delta` chunk from a `message_update` payload.
fn delta_text(payload: &Value) -> Option<String> {
    let update = payload
        .get("assistantMessageEvent")
        .or_else(|| payload.get("assistant_message_event"))
        .unwrap_or(payload);
    if update["type"].as_str() != Some("text_delta") {
        return None;
    }
    update["delta"]
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// One-line summary of a non-office tool call for the pane UI.
fn summarize_tool_args(tool: &str, args: &Value) -> String {
    if matches!(tool, "bash" | "powershell") {
        if let Some(command) = args["command"].as_str() {
            let one_line = command.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut chars = one_line.chars();
            let short: String = chars.by_ref().take(120).collect();
            return if chars.next().is_some() {
                format!("{short}…")
            } else {
                short
            };
        }
    }
    if matches!(tool, "read" | "write" | "edit") {
        if let Some(path) = args["path"].as_str() {
            return path.to_string();
        }
    }
    tool.to_string()
}

/// Stable per-turn pane-awareness block appended to the system prompt.
fn pane_context_block(panes: &[Pane]) -> Option<String> {
    if panes.is_empty() {
        return None;
    }
    let lines: Vec<String> = panes
        .iter()
        .map(|pane| {
            let mut marks: Vec<&str> = Vec::new();
            if pane.focused {
                marks.push("focused");
            }
            if pane.ops.is_none() {
                marks.push("legacy client (v1 ops)");
            }
            let suffix = if marks.is_empty() {
                String::new()
            } else {
                format!(" ({})", marks.join(", "))
            };
            format!(
                "- {} · \"{}\" — id `{}`{suffix}",
                pane.host.label(),
                pane.document_label(),
                pane.document_ref()
            )
        })
        .collect();
    Some(format!(
        "\n\n## Attached Office documents\n{lines}\n\n\
Every office_* tool accepts an optional `document` argument: a document id from the list above, \
a unique document name, or \"active\" (the default) for the focused document. When several \
documents of one host are open and none is focused, calls without a document target fail with \
the candidate list — pass the target explicitly instead of guessing. A user's `#name` mention \
selects that document. Call office_list_documents if this list may be stale.\n\n\
These tools edit the attached documents directly — use them, and never emit HTML for Word. \
Formatting support depends on the host and the APIs it exposes: each tool's schema lists exactly \
the properties it accepts. If a property is not in the schema, say it is not available rather \
than claiming support, and call office_word_get_capabilities when a request depends on a Word \
feature you are unsure about.",
        lines = lines.join("\n")
    ))
}

/* ── event handlers ────────────────────────────────────────────────── */

/// One-shot session initialization.
///
/// `SessionStart` is the natural hook, but non-interactive (`--print`) runs
/// dispatch `AgentStart`/`TurnStart`/`BeforeAgentStart` and **never**
/// `SessionStart` (measured on `rpi 0.24.0`). Both entry points share this
/// function so a print-mode session still resolves flags, reconciles the tool
/// set and honours `--office-bridge-autostart`.
fn ensure_initialized(api: PluginApi, owner: *mut c_void) {
    static INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if INITIALIZED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let port = config::resolve_port(cli_flag(api, config::FLAG_PORT).as_ref(), &env_value);
    let autostart =
        config::resolve_autostart(cli_flag(api, config::FLAG_AUTOSTART).as_ref(), &env_value);
    with_state(|state| {
        state.port = port;
        state.autostart = autostart;
        if state.controller.snapshot().state == lifecycle::LifecycleState::Off {
            state.controller = Controller::new(port);
        }
    });
    // Always reconcile: with the bridge off this removes every office tool from
    // the active set, so an offline session does not advertise Office support.
    reconcile_tools(api);
    if autostart
        && with_state(|state| state.controller.snapshot().state == lifecycle::LifecycleState::Off)
    {
        match connect_bridge(Some(port), owner) {
            Ok(snapshot) => eprintln!(
                "[rpi-office] bridge listening on ws://127.0.0.1:{}",
                snapshot.actual_port.unwrap_or(snapshot.port)
            ),
            Err(error) => eprintln!("[rpi-office] autostart failed: {error}"),
        }
    }
    refresh_status(api);
}

rpi_plugin_sdk::event_handler! {
fn on_session_start(_event, owner) {
    if let Some(api) = api(owner) {
        ensure_initialized(api, owner);
    }
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_agent_start(_event, owner) {
    if let Some(api) = api(owner) {
        ensure_initialized(api, owner);
    }
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_session_shutdown(_event, owner) {
    let result = with_state(|state| {
        state.prompts.clear();
        state.delta.clear();
        state.controller.close()
    });
    if let Err(error) = result {
        eprintln!("[rpi-office] shutdown close failed: {error}");
    }
    if let Some(api) = api(owner) {
        let _ = call_action(api, RuntimeActionId::SetStatus, json!({"key": STATUS_KEY}));
    }
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_message_start(event, _owner) {
    let Ok(message) = serde_json::from_str::<Value>(&event.message_text().unwrap_or_default())
    else {
        return 0;
    };
    if message["role"].as_str() != Some("user") {
        return 0;
    }
    let text = flatten_text_content(&message["content"]);
    if text.is_empty() {
        return 0;
    }
    with_state(|state| {
        let _ = state.prompts.bind_delivered_user_message(&text);
    });
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_message_update(event, _owner) {
    let payload: Value =
        serde_json::from_str(&event.message_text().unwrap_or_default()).unwrap_or(Value::Null);
    let Some(text) = delta_text(&payload) else {
        return 0;
    };
    let active = with_state(|state| {
        state.prompts.active().map(|entry| {
            (
                entry.pane.clone(),
                entry.prompt_id.clone(),
            )
        })
    });
    let Some((pane_id, prompt_id)) = active else {
        return 0;
    };
    // A v2 pane cannot render a delta; the final reply is all it gets.
    let protocol_version = panes_lookup()
        .into_iter()
        .find(|pane| pane.pane_id == pane_id)
        .map(|pane| pane.protocol_version)
        .unwrap_or(0);
    if protocol_version < protocol::BRIDGE_PROTOCOL_VERSION {
        return 0;
    }
    let _ = worker().send(Job::Delta {
        pane_id,
        prompt_id,
        text,
    });
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_message_end(event, owner) {
    let Ok(message) = serde_json::from_str::<Value>(&event.message_text().unwrap_or_default())
    else {
        return 0;
    };
    if message["role"].as_str() != Some("assistant") {
        return 0;
    }
    let text = flatten_text_content(&message["content"]);
    let message_id = message["id"].as_str().map(str::to_owned);
    let active = with_state(|state| {
        let entry = state.prompts.complete();
        state.delta.clear();
        entry
    });
    let Some(entry) = active else {
        return 0;
    };
    let _ = worker().send(Job::Flush);
    if let Some(api) = api(owner) {
        refresh_status(api);
    }
    if text.is_empty() {
        return 0;
    }
    let _ = worker().send(Job::Final {
        pane_id: entry.pane,
        prompt_id: entry.prompt_id,
        text,
        message_id,
    });
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_agent_settled(_event, _owner) {
    // An aborted/settled turn that produced no final must not leave a pane
    // bound to the next, unrelated turn.
    with_state(|state| {
        state.prompts.complete();
        state.delta.clear();
    });
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_tool_execution_start(event, _owner) {
    let Some(call) = event.tool_call() else { return 0 };
    if catalog::catalog().host_for_tool(&call.tool_name).is_some()
        || call.tool_name == active_tools::OFFICE_LIST_TOOL
    {
        return 0;
    }
    let args: Value = serde_json::from_str(&call.params).unwrap_or(Value::Null);
    let pane_id = with_state(|state| state.prompts.active().map(|entry| entry.pane.clone()));
    if let Some(pane_id) = pane_id {
        let _ = worker().send(Job::ToolActivity {
            pane_id,
            tool: call.tool_name.clone(),
            status: "start".into(),
            summary: Some(summarize_tool_args(&call.tool_name, &args)),
        });
    }
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_tool_execution_end(event, _owner) {
    let Some(result) = event.tool_result() else { return 0 };
    if catalog::catalog().host_for_tool(&result.tool_name).is_some()
        || result.tool_name == active_tools::OFFICE_LIST_TOOL
    {
        return 0;
    }
    let pane_id = with_state(|state| state.prompts.active().map(|entry| entry.pane.clone()));
    if let Some(pane_id) = pane_id {
        let (status, summary) = if result.is_error {
            ("error", Some(result.result.clone()))
        } else {
            ("end", None)
        };
        let _ = worker().send(Job::ToolActivity {
            pane_id,
            tool: result.tool_name,
            status: status.into(),
            summary,
        });
    }
    0
}
}

/* ── system prompt transformer ─────────────────────────────────────── */

rpi_plugin_sdk::string_handler! {
fn before_agent_start(input, out, _owner) {
    let env: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let current = env["systemPrompt"].as_str().unwrap_or_default();
    let panes = panes_lookup();
    let Some(block) = pane_context_block(&panes) else {
        return 0;
    };
    // The response replaces the prompt, so append to the current value.
    out.write(json!({"systemPrompt": format!("{current}{block}")}).to_string());
    0
}
}

/* ── plugin lifecycle (reload / shutdown safety) ────────────────────── */

/// `begin_shutdown`: non-blocking half. Ask the hub to close panes and exit,
/// drop prompt correlation, and refuse to start new work.
extern "C" fn plugin_begin_shutdown(_user_data: *mut c_void) {
    with_state(|state| {
        state.prompts.clear();
        state.delta.clear();
        state.controller.begin_shutdown();
    });
}

/// `finish_shutdown`: blocking half. Join both plugin threads, then report 0 so
/// the host may unload the library; returning nonzero would keep it resident.
extern "C" fn plugin_finish_shutdown(_user_data: *mut c_void) -> i32 {
    with_state(|state| state.controller.finish_shutdown());
    let _ = shutdown_worker();
    if let Some(api) = api(std::ptr::null_mut()) {
        let _ = call_action(api, RuntimeActionId::SetStatus, json!({"key": STATUS_KEY}));
    }
    0
}

/* ── commands ──────────────────────────────────────────────────────── */

fn command_text(raw: &str, owner: *mut c_void) -> String {
    let result = (|| -> Result<String, String> {
        let envelope: Value =
            serde_json::from_str(raw).map_err(|error| format!("Invalid command: {error}"))?;
        let args = envelope["args"].as_str().unwrap_or_default();
        match commands::parse_office_command(args) {
            commands::OfficeCommand::Help => Ok(commands::office_command_help()),
            commands::OfficeCommand::Error(message) => Err(message),
            commands::OfficeCommand::Status => {
                let (snapshot, panes, autostart) = with_state(|state| {
                    (
                        state.controller.snapshot(),
                        state.controller.panes(),
                        state.autostart,
                    )
                });
                Ok(commands::status_text(&commands::StatusInput {
                    snapshot: &snapshot,
                    panes: &panes,
                    autostart,
                }))
            }
            commands::OfficeCommand::Connect { port } => {
                let snapshot = connect_bridge(port, owner)?;
                Ok(format!(
                    "Office bridge listening on ws://127.0.0.1:{}. Open the Pi for Office add-in, or run /office status.",
                    snapshot.actual_port.unwrap_or(snapshot.port)
                ))
            }
            commands::OfficeCommand::Close => {
                close_bridge(owner)?;
                Ok("Office bridge closed. The add-in is disconnected; run /office connect to resume."
                    .to_string())
            }
            commands::OfficeCommand::Restart { port } => {
                let snapshot = with_state(|state| {
                    state.prompts.clear();
                    state.delta.clear();
                    state.controller.restart(port, &health_meta, hooks())
                })?;
                if let Some(api) = api(owner) {
                    reconcile_tools(api);
                    refresh_status(api);
                }
                Ok(format!(
                    "Office bridge restarted on ws://127.0.0.1:{}.",
                    snapshot.actual_port.unwrap_or(snapshot.port)
                ))
            }
        }
    })();
    result.unwrap_or_else(|error| format!("Unable to run /office: {error}"))
}

rpi_plugin_sdk::string_handler! {
fn office_command(input, out, owner) {
    let text = command_text(input, owner);
    out.write(json!({"kind":"message","text":text,"markdown":true}).to_string());
    0
}
}

rpi_plugin_sdk::string_handler! {
fn office_tools_command(_input, out, owner) {
    let active = api(owner)
        .and_then(|api| active_tool_names(api).ok())
        .unwrap_or_default();
    out.write(
        json!({
            "kind": "message",
            "text": commands::tools_text(&active),
            "markdown": true,
        })
        .to_string(),
    );
    0
}
}

/* ── registration ──────────────────────────────────────────────────── */

fn register(api: &PluginApi) -> i32 {
    *api_store()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(*api);
    tools::install(server_lookup, panes_lookup);

    if let Some(register_flag) = api.register_flag {
        let _ = register_flag(
            StbStringRef::from_str(config::FLAG_PORT),
            StbStringRef::from_str("Port for the pi-for-office bridge server (default 38617)"),
        );
        let _ = register_flag(
            StbStringRef::from_str(config::FLAG_AUTOSTART),
            StbStringRef::from_str(
                "Start the pi-for-office bridge when the session opens (default: off; also RPI_OFFICE_BRIDGE_AUTOSTART=1)",
            ),
        );
    }

    let Some(register_command) = api.register_command else {
        return 1;
    };
    let rc = register_command(
        StbStringRef::from_str("office"),
        StbStringRef::from_str(
            "Control the pi-for-office bridge: status, connect, close or restart it",
        ),
        office_command as CommandHandlerFn,
    );
    if rc != 0 {
        return rc;
    }
    let rc = register_command(
        StbStringRef::from_str("office-tools"),
        StbStringRef::from_str("List the office tools registered by this extension"),
        office_tools_command as CommandHandlerFn,
    );
    if rc != 0 {
        return rc;
    }

    let Some(register_event) = api.register_event_handler else {
        return 1;
    };
    let handlers: [(EventTag, EventHandlerFn); 9] = [
        (EventTag::SessionStart, on_session_start as EventHandlerFn),
        (EventTag::AgentStart, on_agent_start as EventHandlerFn),
        (
            EventTag::SessionShutdown,
            on_session_shutdown as EventHandlerFn,
        ),
        (EventTag::MessageStart, on_message_start as EventHandlerFn),
        (EventTag::MessageUpdate, on_message_update as EventHandlerFn),
        (EventTag::MessageEnd, on_message_end as EventHandlerFn),
        (EventTag::AgentSettled, on_agent_settled as EventHandlerFn),
        (
            EventTag::ToolExecutionStart,
            on_tool_execution_start as EventHandlerFn,
        ),
        (
            EventTag::ToolExecutionEnd,
            on_tool_execution_end as EventHandlerFn,
        ),
    ];
    for (tag, handler) in handlers {
        let rc = register_event(tag, handler, api.user_data);
        if rc != 0 {
            return rc;
        }
    }

    if let Some(register_lifecycle) = api.register_plugin_lifecycle {
        let rc = register_lifecycle(plugin_begin_shutdown, plugin_finish_shutdown, api.user_data);
        if rc != 0 {
            return rc;
        }
    }

    if let Some(register_before_agent_start) = api.register_before_agent_start {
        let rc = register_before_agent_start(
            before_agent_start as BeforeAgentStartFn,
            rpi_plugin_sdk::callbacks::free_string as FreeStringFn,
            api.user_data,
        );
        if rc != 0 {
            return rc;
        }
    }

    tools::register(api)
}

rpi_plugin_sdk::export_plugin!(register);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Host;
    use rpi_plugin_sdk::StbString;

    #[test]
    fn flattens_message_content_shapes() {
        assert_eq!(flatten_text_content(&json!("hi")), "hi");
        assert_eq!(
            flatten_text_content(&json!([
                {"type": "text", "text": "a"},
                {"type": "toolCall", "text": "ignored"},
                {"type": "thinking", "text": " b "}
            ])),
            "a\n b".trim().to_string()
        );
        assert_eq!(flatten_text_content(&json!(42)), "");
    }

    #[test]
    fn extracts_only_text_deltas() {
        assert_eq!(
            delta_text(&json!({"type": "text_delta", "delta": "chunk"})).as_deref(),
            Some("chunk")
        );
        assert_eq!(
            delta_text(&json!({"assistantMessageEvent": {"type": "text_delta", "delta": "x"}}))
                .as_deref(),
            Some("x")
        );
        assert!(delta_text(&json!({"type": "thinking_delta", "delta": "x"})).is_none());
        assert!(delta_text(&json!({"type": "text_delta", "delta": ""})).is_none());
        assert!(delta_text(&json!(null)).is_none());
    }

    #[test]
    fn summarizes_tool_arguments() {
        assert_eq!(
            summarize_tool_args("bash", &json!({"command": "  ls   -la  "})),
            "ls -la"
        );
        assert_eq!(
            summarize_tool_args("read", &json!({"path": "src/lib.rs"})),
            "src/lib.rs"
        );
        assert_eq!(summarize_tool_args("websearch", &json!({})), "websearch");
        let long = summarize_tool_args("bash", &json!({"command": "x".repeat(500)}));
        assert!(long.ends_with('…'));
        assert_eq!(long.chars().count(), 121);
    }

    #[test]
    fn status_line_is_compact_and_cleared_when_off() {
        let snapshot = BridgeSnapshot {
            state: lifecycle::LifecycleState::Off,
            port: 38617,
            actual_port: None,
            panes: 0,
            last_error: None,
            started_at: None,
        };
        assert!(status_line(&snapshot, &[]).is_none());

        let snapshot = BridgeSnapshot {
            state: lifecycle::LifecycleState::On,
            port: 38617,
            actual_port: Some(38617),
            panes: 0,
            last_error: None,
            started_at: Some(1),
        };
        assert_eq!(
            status_line(&snapshot, &[]).as_deref(),
            Some("office bridge on :38617 — no app attached")
        );
    }

    #[test]
    fn pane_context_block_lists_documents_and_rules() {
        assert!(pane_context_block(&[]).is_none());
        let pane = bridge::Pane {
            conn: 1,
            host: Host::Excel,
            pane_id: "pane-1".into(),
            client_name: "add-in".into(),
            protocol_version: 3,
            connected_at: 1,
            last_seen: 1,
            model: None,
            provider: None,
            ops: Some(vec!["excel.read_range".into()]),
            catalog_version: Some(3),
            ops_ignored: 0,
            document_id: Some("url_sha256:abc".into()),
            document_name: Some("Book1.xlsx".into()),
            document_source: "document.url".into(),
            target_id: "url_sha256:abc".into(),
            focused: true,
            focus_updated_at: 1,
            visible: true,
        };
        let block = pane_context_block(&[pane]).expect("block");
        assert!(block.contains("## Attached Office documents"));
        assert!(block.contains("Excel · \"Book1.xlsx\" — id `url_sha256:abc` (focused)"));
        assert!(block.contains("never emit HTML for Word"));
        assert!(block.contains("office_word_get_capabilities"));
    }

    #[test]
    fn registered_registrars_are_required() {
        let api = PluginApi {
            abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
            struct_size: std::mem::size_of::<PluginApi>() as u32,
            runtime_action: unimplemented_action,
            free_string: rpi_plugin_sdk::callbacks::free_string,
            user_data: std::ptr::null_mut(),
            register_tool: None,
            register_command: None,
            register_shortcut: None,
            register_flag: None,
            register_provider: None,
            register_message_renderer: None,
            register_markdown_transformer: None,
            register_entry_renderer: None,
            register_event_handler: None,
            register_resources_discover: None,
            dispatch_event: None,
            declare: None,
            register_before_agent_start: None,
            register_oauth_provider: None,
            register_streaming_provider: None,
            register_tool_with_wake: None,
            register_plugin_lifecycle: None,
            register_tool_prompt: None,
            register_oauth_provider_cancellable: None,
            register_tool_result: None,
            register_autocomplete_provider: None,
            register_command_completions: None,
            register_argument_suggester: None,
        };
        // No command registrar on this host: registration refuses to load.
        assert_eq!(register(&api), 1);
    }

    extern "C" fn unimplemented_action(
        _action: u32,
        _args: StbStringRef,
        _out: *mut StbString,
        _user_data: *mut c_void,
    ) -> i32 {
        1
    }
}
