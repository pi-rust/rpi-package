//! Scriptable fake host for the goal extension.
//!
//! Each integration-test binary gets its own process, so these statics are
//! per-binary. Tests inside one binary run in parallel by default; call
//! [`serial`] first so the shared goal state and the background worker stay
//! deterministic.
#![allow(dead_code)]

use rpi_plugin_sdk::{
    CommandHandlerFn, EventHandlerFn, EventTag, FreeStringFn, PluginApi, StablePluginEvent,
    StableToolSchema, StbString, StbStringRef, ToolCancelFn, ToolDestroyFn, ToolExecuteFn,
    ToolPollFn,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Call {
    pub action: String,
    pub args: Value,
}

static STATE: Mutex<Option<Value>> = Mutex::new(None);
static CALLS: Mutex<Vec<Call>> = Mutex::new(Vec::new());
static CHANGED: Condvar = Condvar::new();
static SEND_STATUS: Mutex<String> = Mutex::new(String::new());
static ACTIVE_TOOLS: Mutex<Option<Vec<String>>> = Mutex::new(None);
static HANDLERS: Mutex<Vec<(EventTag, EventHandlerFn)>> = Mutex::new(Vec::new());
static COMMAND: Mutex<Option<CommandHandlerFn>> = Mutex::new(None);
static TOOLS: Mutex<BTreeMap<String, ToolFns>> = Mutex::new(BTreeMap::new());
static SERVED: OnceLock<()> = OnceLock::new();
/// Model the host's deferred commit: `AppendEntry` succeeds but the entry only
/// reaches the branch when the test commits it.
static DEFER_APPENDS: Mutex<bool> = Mutex::new(false);
static STAGED: Mutex<Option<Value>> = Mutex::new(None);
/// Blocking `SendMessage`: simulates a run that stays in flight so a test can
/// fire events while the continuation is still active.
static SEND_BLOCKS: Mutex<bool> = Mutex::new(false);
static SEND_RELEASED: Mutex<bool> = Mutex::new(false);
static SEND_CV: Condvar = Condvar::new();

#[derive(Clone, Copy)]
struct ToolFns {
    execute: ToolExecuteFn,
    poll: ToolPollFn,
    cancel: ToolCancelFn,
    destroy: ToolDestroyFn,
}

/// Serialize tests that share the fake host and the extension's process-wide
/// worker.
pub fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    reset();
    guard
}

pub fn reset() {
    // The worker may still be dispatching for the previous test. Drain it until
    // the host sees no new activity, then clear twice so no queued write can
    // resurrect the old goal.
    for _ in 0..40 {
        let before = calls().len();
        std::thread::sleep(Duration::from_millis(25));
        if calls().len() == before {
            break;
        }
    }
    let _ = rpi_goal::state::flush();
    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = None;
    rpi_goal::dispatch::wake();
    std::thread::sleep(Duration::from_millis(40));
    let _ = rpi_goal::state::flush();
    // The extension keeps its newest intent until the branch confirms it; a new
    // test must start from the branch alone.
    rpi_goal::state::discard_desired();
    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = None;
    *DEFER_APPENDS.lock().unwrap_or_else(|p| p.into_inner()) = false;
    *STAGED.lock().unwrap_or_else(|p| p.into_inner()) = None;
    CALLS.lock().unwrap_or_else(|p| p.into_inner()).clear();
    *SEND_STATUS.lock().unwrap_or_else(|p| p.into_inner()) = "completed".into();
    *ACTIVE_TOOLS.lock().unwrap_or_else(|p| p.into_inner()) = None;
    *SEND_BLOCKS.lock().unwrap_or_else(|p| p.into_inner()) = false;
    *SEND_RELEASED.lock().unwrap_or_else(|p| p.into_inner()) = true;
    SEND_CV.notify_all();
    rpi_goal::inject::discard();
}

/// Keep every `SendMessage` call in flight until [`release_sends`].
pub fn block_sends(block: bool) {
    *SEND_BLOCKS.lock().unwrap_or_else(|p| p.into_inner()) = block;
    let mut released = SEND_RELEASED.lock().unwrap_or_else(|p| p.into_inner());
    *released = !block;
    SEND_CV.notify_all();
}

pub fn release_sends() {
    *SEND_RELEASED.lock().unwrap_or_else(|p| p.into_inner()) = true;
    SEND_CV.notify_all();
}

pub fn set_active_tools(tools: &[&str]) {
    *ACTIVE_TOOLS.lock().unwrap_or_else(|p| p.into_inner()) =
        Some(tools.iter().map(|tool| tool.to_string()).collect());
}

/// Reply status for the next `SendMessage` calls: `completed`, `aborted`,
/// `error`, or `queued`.
pub fn set_send_status(status: &str) {
    *SEND_STATUS.lock().unwrap_or_else(|p| p.into_inner()) = status.to_string();
}

pub fn set_goal(state: Value) {
    rpi_goal::state::discard_desired();
    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = Some(state);
    CALLS.lock().unwrap_or_else(|p| p.into_inner()).clear();
}

pub fn goal_state() -> Option<Value> {
    STATE.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn active_goal() -> Option<Value> {
    let state = goal_state()?;
    let active = state.get("active")?.clone();
    if active.is_null() {
        None
    } else {
        Some(active)
    }
}

pub fn status() -> String {
    active_goal()
        .and_then(|goal| {
            goal.get("status")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

pub fn calls() -> Vec<Call> {
    CALLS.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn calls_named(action: &str) -> Vec<Value> {
    calls()
        .into_iter()
        .filter(|call| call.action == action)
        .map(|call| call.args)
        .collect()
}

/// `SendMessage` calls whose prompt mentions `marker`. Tests use unique titles so
/// a dispatch that leaks from a neighbouring test cannot change the count.
pub fn sends_mentioning(marker: &str) -> Vec<Value> {
    calls_named("SendMessage")
        .into_iter()
        .filter(|call| {
            call.get("text")
                .and_then(|text| text.as_str())
                .map(|text| text.contains(marker))
                .unwrap_or(false)
        })
        .collect()
}

/// Wait until at least `count` sends mention `marker`.
pub fn wait_sends_mentioning(marker: &str, count: usize, timeout: Duration) -> Vec<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let matched = sends_mentioning(marker);
        if matched.len() >= count || Instant::now() >= deadline {
            return matched;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until at least `count` calls of `action` were observed.
pub fn wait_for(action: &str, count: usize, timeout: Duration) -> Vec<Value> {
    let deadline = Instant::now() + timeout;
    let mut calls = CALLS.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        let matched: Vec<Value> = calls
            .iter()
            .filter(|call| call.action == action)
            .map(|call| call.args.clone())
            .collect();
        if matched.len() >= count {
            return matched;
        }
        let now = Instant::now();
        if now >= deadline {
            return matched;
        }
        let (guard, _) = CHANGED
            .wait_timeout(calls, deadline - now)
            .unwrap_or_else(|p| p.into_inner());
        calls = guard;
    }
}

/// Give the worker a moment to (not) do something. Used for negative assertions.
pub fn settle_pause() {
    std::thread::sleep(Duration::from_millis(120));
}

// ---------------------------------------------------------------------------
// Host callbacks
// ---------------------------------------------------------------------------

extern "C" fn register_command(_: StbStringRef, _: StbStringRef, handler: CommandHandlerFn) -> i32 {
    *COMMAND.lock().unwrap_or_else(|p| p.into_inner()) = Some(handler);
    0
}

extern "C" fn register_event(tag: EventTag, handler: EventHandlerFn, _: *mut c_void) -> i32 {
    HANDLERS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((tag, handler));
    0
}

extern "C" fn register_tool(
    schema: *const StableToolSchema,
    execute: ToolExecuteFn,
    poll: ToolPollFn,
    cancel: ToolCancelFn,
    destroy: ToolDestroyFn,
    _: FreeStringFn,
) -> i32 {
    // SAFETY: the host owns the schema for the duration of this call and the
    // plugin keeps the name alive in its own registration; copying the name here
    // is an ordinary read.
    let schema = unsafe { &*schema };
    let name = schema.name.to_string_lossy();
    TOOLS.lock().unwrap_or_else(|p| p.into_inner()).insert(
        name,
        ToolFns {
            execute,
            poll,
            cancel,
            destroy,
        },
    );
    0
}

rpi_plugin_sdk::string_handler! {
fn runtime_action(action, args, out, _user_data) {
    let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    let name = action_name(action);
    record(&name, args.clone());
    let result = match name.as_str() {
        "GetSessionBranch" => {
            let state = STATE.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let entries = match state {
                Some(state) => vec![json!({"type":"custom","customType":"goal-state","data":state})],
                None => Vec::new(),
            };
            json!({"filtered": true, "entries": entries, "count": entries.len()})
        }
        "AppendEntry" => {
            if args.get("customType").and_then(Value::as_str) == Some("goal-state") {
                let data = args.get("data").cloned().unwrap_or(Value::Null);
                if *DEFER_APPENDS.lock().unwrap_or_else(|p| p.into_inner()) {
                    *STAGED.lock().unwrap_or_else(|p| p.into_inner()) = Some(data);
                } else {
                    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = Some(data);
                }
            }
            json!({"entryId": "fixture-entry"})
        }
        "SendMessage" => {
            let blocks = *SEND_BLOCKS.lock().unwrap_or_else(|p| p.into_inner());
            if blocks {
                let mut released = SEND_RELEASED.lock().unwrap_or_else(|p| p.into_inner());
                while !*released {
                    let (guard, timeout) = SEND_CV
                        .wait_timeout(released, Duration::from_secs(5))
                        .unwrap_or_else(|p| p.into_inner());
                    released = guard;
                    if timeout.timed_out() {
                        break;
                    }
                }
            }
            let status = SEND_STATUS.lock().unwrap_or_else(|p| p.into_inner()).clone();
            match status.as_str() {
                "error" => json!({"status": "error", "error": "provider failed"}),
                other => json!({"status": other}),
            }
        }
        "GetActiveTools" => {
            let tools = ACTIVE_TOOLS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
                .unwrap_or_else(|| {
                    vec![
                        "goal".to_string(),
                        "goal_complete".to_string(),
                        "goal_blocked".to_string(),
                        "goal_wait".to_string(),
                    ]
                });
            json!({"tools": tools})
        }
        "SetStatus" => json!({"ok": true, "changed": true}),
        _ => json!({}),
    };
    out.write(result.to_string());
    0
}
}

fn action_name(action: u32) -> String {
    match action {
        0 => "SendMessage",
        1 => "SendUserMessage",
        2 => "AppendEntry",
        4 => "GetActiveTools",
        18 => "SetStatus",
        20 => "GetSessionBranch",
        _ => return format!("action-{action}"),
    }
    .to_string()
}

fn record(name: &str, args: Value) {
    CALLS.lock().unwrap_or_else(|p| p.into_inner()).push(Call {
        action: name.to_string(),
        args,
    });
    CHANGED.notify_all();
}

// ---------------------------------------------------------------------------
// Plugin lifecycle
// ---------------------------------------------------------------------------

pub fn api() -> PluginApi {
    PluginApi {
        abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        runtime_action,
        free_string: rpi_plugin_sdk::callbacks::free_string,
        user_data: std::ptr::null_mut(),
        register_command: Some(register_command),
        register_tool: Some(register_tool),
        register_shortcut: None,
        register_flag: None,
        register_provider: None,
        register_message_renderer: None,
        register_markdown_transformer: None,
        register_entry_renderer: None,
        register_event_handler: Some(register_event),
        register_resources_discover: None,
        dispatch_event: None,
        declare: None,
        register_before_agent_start: None,
        register_oauth_provider: None,
    }
}

/// Install the extension against the fake host exactly once per test binary.
pub fn install() -> PluginApi {
    SERVED.get_or_init(|| {
        let api = api();
        assert_eq!(rpi_goal::rpi_plugin_register(&api), 0);
    });
    api()
}

/// Fire a payload-less event at every handler registered for `tag`.
pub fn fire(tag: EventTag) {
    let handlers: Vec<EventHandlerFn> = HANDLERS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|(registered, _)| *registered == tag)
        .map(|(_, handler)| *handler)
        .collect();
    for handler in handlers {
        let event = StablePluginEvent::empty(tag);
        handler(event, std::ptr::null_mut());
    }
}

/// Fire a message event carrying a JSON payload (the host sends the serialized
/// `AgentMessage` for `MessageStart`/`MessageUpdate`/`MessageEnd`).
pub fn fire_message(tag: EventTag, payload: &str) {
    let handlers: Vec<EventHandlerFn> = HANDLERS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|(registered, _)| *registered == tag)
        .map(|(_, handler)| *handler)
        .collect();
    let owned = StbString::from_string(payload.to_string());
    for handler in handlers {
        let event = StablePluginEvent::message(tag, owned);
        handler(event, std::ptr::null_mut());
    }
    // The host frees one payload allocation after the fan-out.
    owned.free_with(Some(rpi_plugin_sdk::callbacks::free_string));
}

pub fn fire_settled() {
    fire(EventTag::AgentSettled);
}

pub fn fire_turn_end() {
    fire(EventTag::TurnEnd);
}

/// Invoke the `/goal` command handler with `args`.
pub fn command(args: &str) -> Value {
    let handler = COMMAND
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .expect("/goal command was not registered");
    let input = json!({"command": "goal", "args": args}).to_string();
    let mut output = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&input),
        &mut output,
        std::ptr::null_mut(),
    );
    let text = output.to_string_lossy();
    rpi_plugin_sdk::callbacks::free_string(output);
    assert_eq!(rc, 0, "/goal handler failed");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// Drive one registered tool through execute/poll and return its JSON envelope.
pub fn tool(name: &str, params: Value) -> Result<Value, String> {
    let fns = TOOLS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(name)
        .copied()
        .unwrap_or_else(|| panic!("tool `{name}` was not registered"));
    let handle = (fns.execute)(
        StbStringRef::from_str("call"),
        StbString::from_string(params.to_string()),
        Some(rpi_plugin_sdk::callbacks::free_string),
    );
    let result = (fns.poll)(handle, None, std::ptr::null_mut());
    // SAFETY: the union arm is selected by the tag the plugin set, and the plugin
    // allocated exactly one string there. Reading it once and freeing it mirrors
    // what the host does.
    let (is_error, owned) = match result.tag {
        rpi_plugin_sdk::StepResultTag::Done => (false, unsafe { result.payload.done.result }),
        rpi_plugin_sdk::StepResultTag::Err => (true, unsafe { result.payload.err.message }),
        rpi_plugin_sdk::StepResultTag::Pending => (false, StbString::empty()),
    };
    let text = owned.to_string_lossy();
    owned.free_with(Some(rpi_plugin_sdk::callbacks::free_string));
    (fns.destroy)(handle);
    if is_error {
        return Err(text);
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

pub fn tool_names() -> Vec<String> {
    TOOLS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .keys()
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Settings sandbox
// ---------------------------------------------------------------------------

/// Point `RPI_CODING_AGENT_DIR` at a temporary directory holding `goal.json`.
pub struct SettingsDir {
    previous: Option<std::ffi::OsString>,
    pub dir: PathBuf,
}

impl SettingsDir {
    pub fn new(contents: Option<Value>) -> Self {
        static COUNTER: Mutex<u64> = Mutex::new(0);
        let mut counter = COUNTER.lock().unwrap_or_else(|p| p.into_inner());
        *counter += 1;
        let dir =
            std::env::temp_dir().join(format!("rpi-goal-test-{}-{}", std::process::id(), *counter));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(contents) = contents {
            std::fs::write(dir.join("goal.json"), contents.to_string()).unwrap();
        }
        let previous = std::env::var_os("RPI_CODING_AGENT_DIR");
        std::env::set_var("RPI_CODING_AGENT_DIR", &dir);
        Self { previous, dir }
    }

    pub fn with_limits(automatic_turns: Option<u32>, no_progress_turns: Option<u32>) -> Self {
        Self::new(Some(json!({
            "continuationLimits": {
                "automaticTurns": automatic_turns,
                "noProgressTurns": no_progress_turns,
            }
        })))
    }
}

impl Drop for SettingsDir {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => std::env::set_var("RPI_CODING_AGENT_DIR", previous),
            None => std::env::remove_var("RPI_CODING_AGENT_DIR"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------------------
// Goal fixtures
// ---------------------------------------------------------------------------

pub fn active_goal_state(id: &str, title: &str) -> Value {
    json!({
        "version": 2,
        "active": {
            "id": id,
            "title": title,
            "status": "active",
            "notes": "",
            "startedAt": rpi_goal::state::now_ms(),
            "updatedAt": rpi_goal::state::now_ms(),
            "iteration": 0,
            "turnsUsed": 0,
            "maxTurns": null,
            "deadlineMs": null,
            "tokenBudget": null,
            "tokensUsed": 0,
            "baselineTokens": 0,
            "automaticTurns": 0,
            "noProgressCount": 0,
            "lastOutputFingerprint": null,
            "safetyPauseCause": null,
            "activeMs": 0,
            "activeSince": rpi_goal::state::now_ms(),
            "waiting": null,
            "lastOutcome": null
        },
        "archived": []
    })
}

/// Defer `goal-state` commits, like a host that only commits an entry written
/// during a run after that turn ends.
pub fn defer_appends(defer: bool) {
    *DEFER_APPENDS.lock().unwrap_or_else(|p| p.into_inner()) = defer;
}

/// Commit whatever the deferred `AppendEntry` staged.
pub fn commit_staged() {
    let staged = STAGED.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(staged) = staged {
        *STATE.lock().unwrap_or_else(|p| p.into_inner()) = Some(staged);
    }
}

/// The branch as the host has committed it (bypasses the extension).
pub fn committed_goal() -> Option<Value> {
    goal_state()
}

/// Short pause for negative assertions (the worker must *not* act).
pub fn sleep(millis: u64) {
    std::thread::sleep(Duration::from_millis(millis));
}
