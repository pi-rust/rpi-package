//! `rpi-permission-gate` — confirm dangerous tool calls through the host's
//! generic extension UI dialog.
//!
//! It is a **generic policy plugin**: it subscribes to the pi `on("tool_call")`
//! event (wired by the host's `before_tool_call` hook), and for `bash` / `write`
//! / `edit` asks the user through the `UiDialog` runtime action (17). The host
//! answers that from wherever it has a UI — the interactive TUI, or the
//! `--mode rpc` UI bridge that an ACP client such as `rpi-acp` serves.
//!
//! Off by default. Enable with `--permission-gate` or `RPI_PERMISSION_GATE=1`.
//! `--bypass` overrides both activation methods for this process.
//! When a tool is denied, the handler returns `EVENT_HANDLER_ABORT`, which the
//! host's `before_tool_call` turns into a blocked tool (an error result the
//! model sees). If the dialog cannot be answered (no UI), it **blocks** — the
//! safe default, mirroring the upstream Pi permission-gate example.

use rpi_plugin_sdk::{
    register_entrypoint, EventTag, FreeStringFn, PluginApi, RuntimeActionFn, RuntimeActionId,
    StbString, StbStringRef, EVENT_HANDLER_ABORT, EVENT_HANDLER_CONTINUE,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// CLI flag that turns gating on.
const GATE_FLAG: &str = "permission-gate";
const BYPASS_FLAG: &str = "bypass";
/// Env equivalent (useful when the flag cannot be passed).
const GATE_ENV: &str = "RPI_PERMISSION_GATE";

#[derive(Clone, Copy)]
struct HostRuntime {
    action: RuntimeActionFn,
    free: FreeStringFn,
    user_data: *mut std::ffi::c_void,
}
// SAFETY: the host warrants the runtime-action fn + its opaque `user_data` are
// safe to call from any thread (the same contract every host-runtime plugin
// relies on, e.g. rpi-ask-user / rpi-goal).
unsafe impl Send for HostRuntime {}
unsafe impl Sync for HostRuntime {}

static RUNTIME: OnceLock<HostRuntime> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static REQUEST_SEQ: AtomicU64 = AtomicU64::new(0);

fn get_cli_flag(name: &str) -> Option<Value> {
    let runtime = *RUNTIME.get()?;
    let args = json!({"name": name}).to_string();
    let mut out = StbString::empty();
    let rc = (runtime.action)(
        RuntimeActionId::GetCliFlag as u32,
        StbStringRef::from_str(&args),
        &mut out,
        runtime.user_data,
    );
    let text = if rc == 0 {
        Some(out.to_string_lossy())
    } else {
        None
    };
    (runtime.free)(out);
    let value: Value = serde_json::from_str(&text?).ok()?;
    value.get("value").cloned()
}

fn flag_is_on(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(on)) => *on,
        Some(Value::String(text)) => !text.is_empty() && !text.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            let value = value.trim();
            !value.is_empty() && !value.eq_ignore_ascii_case("false") && value != "0"
        })
        .unwrap_or(false)
}

fn gate_enabled(bypass: bool, flag: bool, environment: bool) -> bool {
    !bypass && (flag || environment)
}

fn next_request_id() -> String {
    let seq = REQUEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("gate-{}-{seq}-{nanos}", std::process::id())
}

fn invoke_ui_dialog(runtime: &HostRuntime, args: &Value) -> Result<Value, String> {
    let input = args.to_string();
    let mut out = StbString::empty();
    let rc = (runtime.action)(
        RuntimeActionId::UiDialog as u32,
        StbStringRef::from_str(&input),
        &mut out,
        runtime.user_data,
    );
    let text = out.to_string_lossy();
    (runtime.free)(out);
    if rc != 0 {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(error) = value.get("error").and_then(Value::as_str) {
                return Err(error.to_owned());
            }
        }
        return Err(if text.is_empty() {
            format!("UI dialog failed with status {rc}")
        } else {
            text
        });
    }
    serde_json::from_str(&text).map_err(|error| format!("UI dialog returned invalid JSON: {error}"))
}

/// The `UiDialog` payload for one tool call. It must satisfy the SDK's dialogs
/// contract (`purpose: "permission"` + an explicit `permission` decision per
/// option) or the rpc/ACP bridges answer it as cancelled instead of asking.
fn permission_ui(tool_name: &str, args: &Value) -> Value {
    json!({
        "kind": "confirm",
        "purpose": "permission",
        "id": "permission-gate",
        "question": format!("Allow {tool_name}?"),
        "message": summarize(tool_name, args),
        "options": [{"title": "Yes", "permission": "allow_once"}, {"title": "No", "permission": "reject_once"}],
    })
}

/// Ask the user to confirm one tool call. `Ok(true)` = allowed.
fn confirm(
    runtime: &HostRuntime,
    tool_call_id: &str,
    tool_name: &str,
    args: &Value,
) -> Result<bool, String> {
    let request_id = next_request_id();
    let open = json!({
        "op": "open",
        "requestId": request_id,
        "toolCallId": tool_call_id,
        "ui": permission_ui(tool_name, args),
    });
    invoke_ui_dialog(runtime, &open)?;

    // The plugin-side dialog protocol is open → poll → answer. Sleep briefly
    // between polls; the host's UI bridge delivers the answer asynchronously.
    loop {
        let response = invoke_ui_dialog(runtime, &json!({"op": "poll", "requestId": request_id}))?;
        match response
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
        {
            "answered" => {
                let answer = response.get("answer").cloned().unwrap_or(Value::Null);
                let allowed = answer_values(&answer)
                    .first()
                    .map(|value| value.eq_ignore_ascii_case("yes"))
                    .unwrap_or(false);
                return Ok(allowed);
            }
            "cancelled" => return Ok(false),
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn answer_values(answer: &Value) -> Vec<String> {
    if let Some(values) = answer.get("values").and_then(Value::as_array) {
        return values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect();
    }
    if let Some(value) = answer.get("value").and_then(Value::as_str) {
        return vec![value.to_string()];
    }
    Vec::new()
}

/// A short human summary of what the call will do.
fn summarize(tool_name: &str, args: &Value) -> String {
    match tool_name {
        "bash" => args
            .get("command")
            .and_then(Value::as_str)
            .map(|command| truncate(command, 400))
            .unwrap_or_else(|| "run a shell command".into()),
        "write" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or("file");
            let bytes = args
                .get("content")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or(0);
            format!("write {path} ({bytes} bytes)")
        }
        // `background` spawns a shell exactly like `bash` does, so it needs the
        // same summary: the command is what the user is approving. Without this
        // arm the gate would show only the tool name, which is useless when the
        // whole point is "which command runs unattended".
        "background" => {
            let Some(command) = args.get("command").and_then(Value::as_str) else {
                return "run a shell command in the background".into();
            };
            let command = truncate(command, 400);
            match args.get("label").and_then(Value::as_str) {
                Some(label) if !label.is_empty() => {
                    format!("{command} (background: {label})")
                }
                _ => format!("{command} (background)"),
            }
        }
        "edit" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or("file");
            let edits = args
                .get("edits")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            format!("edit {path} ({edits} replacement(s))")
        }
        other => other.to_string(),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

/// Whether a tool call must be confirmed before it runs.
///
/// These are the tools that spawn a process or mutate a file. `background`
/// belongs here because it spawns a shell exactly like `bash` does — leaving it
/// out made it a silent way to run the same command `bash` would have prompted
/// for. `jobs` is deliberately absent: it only inspects or kills jobs the user
/// already approved.
fn is_gated_tool(tool_name: &str) -> bool {
    matches!(tool_name, "bash" | "write" | "edit" | "background")
}

rpi_plugin_sdk::event_handler! {
/// pi `on("tool_call")`: confirm dangerous tools before they run, and veto
/// (`EVENT_HANDLER_ABORT`) when the user declines or no UI can answer.
fn on_tool_call(event, _data) {
    if !ENABLED.load(Ordering::Acquire) {
        return EVENT_HANDLER_CONTINUE;
    }
    let Some(call) = event.tool_call() else {
        return EVENT_HANDLER_CONTINUE;
    };
    if !is_gated_tool(&call.tool_name) {
        return EVENT_HANDLER_CONTINUE;
    }
    let Some(runtime) = RUNTIME.get().copied() else {
        // No host runtime captured — fail closed.
        return EVENT_HANDLER_ABORT;
    };
    let args: Value = serde_json::from_str(&call.params).unwrap_or(Value::Null);

    match confirm(&runtime, &call.tool_call_id, &call.tool_name, &args) {
        Ok(true) => EVENT_HANDLER_CONTINUE,
        Ok(false) => {
            eprintln!("[rpi-permission-gate] {} blocked by user", call.tool_name);
            EVENT_HANDLER_ABORT
        }
        Err(error) => {
            eprintln!("[rpi-permission-gate] {error}; blocking {} by default", call.tool_name);
            EVENT_HANDLER_ABORT
        }
    }
}
}

#[no_mangle]
// The host resolves this symbol and calls it with the unified-ABI signature; the
// raw pointer is validated by `register_entrypoint` before use.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn rpi_plugin_register(api: *const PluginApi) -> i32 {
    unsafe {
        register_entrypoint(api, |api| {
            // Capture the host callbacks first so the flag can be read during
            // register (the `PluginApi` pointer is only valid for this call).
            let _ = RUNTIME.set(HostRuntime {
                action: api.runtime_action,
                free: api.free_string,
                user_data: api.user_data,
            });

            if let Some(register_flag) = api.register_flag {
                let _ = register_flag(
                    StbStringRef::from_str(BYPASS_FLAG),
                    StbStringRef::from_str(
                        "Skip permission-gate approvals for this process and ACP sessions",
                    ),
                );
                let _ = register_flag(
                    StbStringRef::from_str(GATE_FLAG),
                    StbStringRef::from_str(
                        "Confirm dangerous bash/write/edit tool calls via the host UI dialog",
                    ),
                );
            }

            // Off unless explicitly enabled — a gate can hang a headless run
            // whose client cannot answer UI dialogs.
            if !gate_enabled(
                flag_is_on(get_cli_flag(BYPASS_FLAG).as_ref()),
                flag_is_on(get_cli_flag(GATE_FLAG).as_ref()),
                env_flag(GATE_ENV),
            ) {
                return 0;
            }
            ENABLED.store(true, Ordering::Release);

            match api.register_event_handler {
                Some(register) => register(EventTag::ToolCall, on_tool_call, std::ptr::null_mut()),
                None => 0,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_overrides_both_gate_activation_methods() {
        for flag in [false, true] {
            for environment in [false, true] {
                assert!(!gate_enabled(true, flag, environment));
                assert_eq!(gate_enabled(false, flag, environment), flag || environment);
            }
        }
    }

    #[test]
    fn summarize_covers_gated_tools() {
        assert_eq!(
            summarize("bash", &json!({"command": "rm -rf /"})),
            "rm -rf /"
        );
        assert_eq!(
            summarize("write", &json!({"path": "a.txt", "content": "hi"})),
            "write a.txt (2 bytes)"
        );
        assert_eq!(
            summarize(
                "edit",
                &json!({"path": "a.rs", "edits": [{"oldText":"a","newText":"b"}]})
            ),
            "edit a.rs (1 replacement(s))"
        );
        // `background` runs the same shell as `bash`, so the prompt has to show
        // the command — the tool name alone tells the user nothing about what
        // is about to run unattended.
        assert_eq!(
            summarize("background", &json!({"command": "npm run dev"})),
            "npm run dev (background)"
        );
        assert_eq!(
            summarize(
                "background",
                &json!({"command": "npm run dev", "label": "dev server"})
            ),
            "npm run dev (background: dev server)"
        );
        assert_eq!(
            summarize("background", &json!({})),
            "run a shell command in the background"
        );
    }

    /// The gate must cover exactly the tools that run a shell or touch files.
    ///
    /// A tool that spawns a process but is missing from this list is a way to
    /// bypass the prompt entirely — which is what `background` was before.
    #[test]
    fn gated_tool_set_includes_background() {
        for gated in ["bash", "write", "edit", "background"] {
            assert!(is_gated_tool(gated), "{gated} must require confirmation");
        }
        for ungated in ["read", "glob", "grep", "jobs"] {
            assert!(!is_gated_tool(ungated), "{ungated} must not be gated");
        }
    }

    #[test]
    fn answer_values_reads_yes_no_conventions() {
        assert_eq!(answer_values(&json!({"values": ["Yes"]})), vec!["Yes"]);
        assert_eq!(answer_values(&json!({"value": "No"})), vec!["No"]);
        assert!(answer_values(&json!({})).is_empty());
    }

    /// The ACP bridge only asks the client when the dialog satisfies the SDK's
    /// dialogs contract; otherwise it answers "cancelled" and every gated tool
    /// is silently blocked. Guard that shape here.
    #[test]
    fn permission_ui_satisfies_the_dialogs_contract() {
        let ui = permission_ui("bash", &json!({"command": "rm -rf /"}));
        assert!(rpi_plugin_sdk::dialogs::validate(&ui, Some("tool-1")).is_ok());
        let options = rpi_plugin_sdk::dialogs::permission_options(&ui).unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].permission, "allow_once");
        assert_eq!(options[1].permission, "reject_once");
        // The allow title must be the one `confirm` treats as approval.
        assert!(options[0].title.eq_ignore_ascii_case("yes"));
    }

    #[test]
    fn env_flag_truthiness() {
        // Not set.
        assert!(!env_flag("RPI_PERMISSION_GATE_TEST_UNSET"));
    }
}
