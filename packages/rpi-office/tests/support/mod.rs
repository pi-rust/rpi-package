//! Shared fake host for the plugin-level integration tests.
//!
//! Provides a `PluginApi` whose registrars record what the plugin registers, a
//! runtime-action double for the actions this extension uses, and helpers to
//! invoke the registered command/event/prompt callbacks. Each test binary gets
//! its own copy (and therefore its own statics), which is what keeps the
//! plugin's process-wide state isolated between test binaries.
#![allow(dead_code)]

use rpi_plugin_sdk::{
    AutocompleteProviderFn, BeforeAgentStartFn, CommandArgumentCompletionFn, CommandHandlerFn,
    EventHandlerFn, EventTag, FreeStringFn, PluginApi, PluginBeginShutdownFn,
    PluginFinishShutdownFn, RuntimeActionId, StablePluginEvent, StableToolSchema, StbString,
    StbStringRef, ToolCancelFn, ToolDestroyFn, ToolExecuteFn, ToolPollFn,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub const ABI: u32 = rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED;

/// A registered tool and its ABI callbacks.
#[derive(Clone, Copy)]
pub struct RegisteredTool {
    pub execute: ToolExecuteFn,
    pub poll: ToolPollFn,
    pub cancel: ToolCancelFn,
    pub destroy: ToolDestroyFn,
}

static TOOLS: Mutex<Vec<(String, String, Value, RegisteredTool)>> = Mutex::new(Vec::new());
static EVENTS: Mutex<Vec<(EventTag, EventHandlerFn)>> = Mutex::new(Vec::new());
static COMMANDS: Mutex<Vec<(String, CommandHandlerFn)>> = Mutex::new(Vec::new());
static FLAGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PROMPT: Mutex<Option<BeforeAgentStartFn>> = Mutex::new(None);
static ACTIVE: Mutex<Vec<String>> = Mutex::new(Vec::new());
static STATUS: Mutex<Option<String>> = Mutex::new(None);
static SENT: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PROMPTS: Mutex<Vec<(String, Value)>> = Mutex::new(Vec::new());
static LIFECYCLE: Mutex<Option<(PluginBeginShutdownFn, PluginFinishShutdownFn)>> = Mutex::new(None);
static ABORTS: AtomicU64 = AtomicU64::new(0);
static AUTOCOMPLETE: Mutex<Option<AutocompleteProviderFn>> = Mutex::new(None);
static COMMAND_COMPLETIONS: Mutex<Vec<(String, CommandArgumentCompletionFn)>> =
    Mutex::new(Vec::new());

/// Copy a host-owned string lent to a registration callback.
fn borrowed_text(value: &StbString) -> String {
    // SAFETY: the host keeps the schema strings alive for this call.
    unsafe { value.to_string_lossy() }
}

extern "C" fn register_tool(
    schema: *const StableToolSchema,
    execute: ToolExecuteFn,
    poll: ToolPollFn,
    cancel: ToolCancelFn,
    destroy: ToolDestroyFn,
    _free: FreeStringFn,
) -> i32 {
    // SAFETY: this test harness plays the host, so it dereferences the schema
    // pointer the plugin just handed over. Nothing else here needs `unsafe`:
    // the strings are copied out with the SDK's own reading macro.
    let schema = unsafe { &*schema };
    let name = borrowed_text(&schema.name);
    let description = borrowed_text(&schema.description);
    let parameters: Value =
        serde_json::from_str(&borrowed_text(&schema.parameters)).expect("valid tool schema");
    TOOLS.lock().unwrap().push((
        name.clone(),
        description,
        parameters,
        RegisteredTool {
            execute,
            poll,
            cancel,
            destroy,
        },
    ));
    // A real host activates every registered tool by default.
    ACTIVE.lock().unwrap().push(name);
    0
}

extern "C" fn register_event(
    tag: EventTag,
    handler: EventHandlerFn,
    _user_data: *mut std::ffi::c_void,
) -> i32 {
    EVENTS.lock().unwrap().push((tag, handler));
    0
}

extern "C" fn register_command(
    name: StbStringRef,
    _description: StbStringRef,
    handler: CommandHandlerFn,
) -> i32 {
    // SAFETY: the host owns the string for the duration of the call.
    COMMANDS
        .lock()
        .unwrap()
        .push((unsafe { name.as_str() }.to_string(), handler));
    0
}

extern "C" fn register_flag(name: StbStringRef, _description: StbStringRef) -> i32 {
    // SAFETY: the host owns the string for the duration of the call.
    FLAGS
        .lock()
        .unwrap()
        .push(unsafe { name.as_str() }.to_string());
    0
}

extern "C" fn register_tool_prompt(name: StbStringRef, metadata: StbStringRef) -> i32 {
    // SAFETY: the host owns both strings for the duration of the call.
    let name = unsafe { name.as_str() }.to_string();
    let metadata: Value =
        serde_json::from_str(unsafe { metadata.as_str() }).expect("prompt metadata is JSON");
    PROMPTS.lock().unwrap().push((name, metadata));
    0
}

extern "C" fn register_plugin_lifecycle(
    begin: PluginBeginShutdownFn,
    finish: PluginFinishShutdownFn,
    _user_data: *mut std::ffi::c_void,
) -> i32 {
    *LIFECYCLE.lock().unwrap() = Some((begin, finish));
    0
}

extern "C" fn register_autocomplete_provider(
    handler: AutocompleteProviderFn,
    _free: FreeStringFn,
    _user_data: *mut std::ffi::c_void,
) -> i32 {
    *AUTOCOMPLETE.lock().unwrap() = Some(handler);
    0
}

extern "C" fn register_command_completions(
    name: StbStringRef,
    handler: CommandArgumentCompletionFn,
    _free: FreeStringFn,
    _user_data: *mut std::ffi::c_void,
) -> i32 {
    // SAFETY: the host owns the string for the duration of the call.
    COMMAND_COMPLETIONS
        .lock()
        .unwrap()
        .push((unsafe { name.as_str() }.to_string(), handler));
    0
}

extern "C" fn register_before_agent_start(
    handler: BeforeAgentStartFn,
    _free: FreeStringFn,
    _user_data: *mut std::ffi::c_void,
) -> i32 {
    *PROMPT.lock().unwrap() = Some(handler);
    0
}

rpi_plugin_sdk::string_handler! {
fn runtime_action(action, args, out, _user_data) {
    let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    let id = RuntimeActionId::try_from(action).expect("known runtime action");
    match id {
        RuntimeActionId::GetCliFlag => {
            out.write(json!({"value": Value::Null}).to_string());
        }
        RuntimeActionId::GetActiveTools => {
            let tools = ACTIVE.lock().unwrap().clone();
            out.write(json!({"tools": tools}).to_string());
        }
        RuntimeActionId::SetActiveTools => {
            let tools: Vec<String> = args["tools"]
                .as_array()
                .expect("tools array")
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect();
            *ACTIVE.lock().unwrap() = tools;
            out.write(json!({"ok": true}).to_string());
        }
        RuntimeActionId::SetStatus => {
            *STATUS.lock().unwrap() = args.get("value").and_then(Value::as_str).map(str::to_owned);
            out.write(json!({"ok": true}).to_string());
        }
        RuntimeActionId::Abort => {
            ABORTS.fetch_add(1, Ordering::SeqCst);
            out.write(
                json!({"aborted": true, "runId": "run-test", "steer": [], "followUp": []})
                    .to_string(),
            );
        }
        RuntimeActionId::SendMessage | RuntimeActionId::SendUserMessage => {
            SENT
                .lock()
                .unwrap()
                .push(args["text"].as_str().unwrap_or_default().to_string());
            out.write(json!({"status": "queued"}).to_string());
        }
        other => panic!("unexpected runtime action: {other:?}"),
    }
    0
}
}

/// The fake host API.
pub fn api() -> PluginApi {
    PluginApi {
        abi_version: ABI,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        free_string: rpi_plugin_sdk::callbacks::free_string,
        register_tool: Some(register_tool),
        register_command: Some(register_command),
        register_shortcut: None,
        register_flag: Some(register_flag),
        register_provider: None,
        register_message_renderer: None,
        register_markdown_transformer: None,
        register_entry_renderer: None,
        register_event_handler: Some(register_event),
        register_resources_discover: None,
        runtime_action,
        dispatch_event: None,
        user_data: std::ptr::null_mut(),
        declare: None,
        register_before_agent_start: Some(register_before_agent_start),
        register_oauth_provider: None,
        register_streaming_provider: None,
        register_tool_with_wake: None,
        register_plugin_lifecycle: Some(register_plugin_lifecycle),
        register_tool_prompt: Some(register_tool_prompt),
        register_oauth_provider_cancellable: None,
        register_tool_result: None,
        register_autocomplete_provider: Some(register_autocomplete_provider),
        register_command_completions: Some(register_command_completions),
        register_argument_suggester: None,
    }
}

/// Register the plugin against the fake host.
pub fn register_plugin() {
    assert_eq!(rpi_office::rpi_plugin_register(&api()), 0);
}

/// `(name, description, parameters, callbacks)` for every registered tool.
pub fn tools() -> Vec<(String, String, Value, RegisteredTool)> {
    TOOLS.lock().unwrap().clone()
}

pub fn tool(name: &str) -> Option<(String, Value, RegisteredTool)> {
    TOOLS
        .lock()
        .unwrap()
        .iter()
        .find(|(registered, _, _, _)| registered == name)
        .map(|(_, description, parameters, callbacks)| {
            (description.clone(), parameters.clone(), *callbacks)
        })
}

/// Registered `(name, handler)` command pairs.
pub fn commands() -> Vec<(String, CommandHandlerFn)> {
    COMMANDS.lock().unwrap().clone()
}

pub fn flags() -> Vec<String> {
    FLAGS.lock().unwrap().clone()
}

pub fn active_tools() -> Vec<String> {
    ACTIVE.lock().unwrap().clone()
}

pub fn status() -> Option<String> {
    STATUS.lock().unwrap().clone()
}

/// `(tool name, metadata)` pairs registered through `register_tool_prompt`.
pub fn prompts() -> Vec<(String, Value)> {
    PROMPTS.lock().unwrap().clone()
}

pub fn lifecycle_hooks() -> Option<(PluginBeginShutdownFn, PluginFinishShutdownFn)> {
    *LIFECYCLE.lock().unwrap()
}

/// Whether an autocomplete provider was registered.
pub fn autocomplete_registered() -> bool {
    AUTOCOMPLETE.lock().unwrap().is_some()
}

/// Registered `(command name, handler)` argument-completion pairs.
pub fn command_completions() -> Vec<(String, CommandArgumentCompletionFn)> {
    COMMAND_COMPLETIONS.lock().unwrap().clone()
}

/// Invoke the autocomplete provider with a fake editor query; returns `None`
/// when the provider declined (return code 1 / empty output).
pub fn run_autocomplete(text: &str, line_prefix: &str) -> Option<Value> {
    let handler = AUTOCOMPLETE
        .lock()
        .unwrap()
        .expect("autocomplete registered");
    let query = json!({
        "text": text,
        "cursor": line_prefix.len(),
        "line": line_prefix,
        "linePrefix": line_prefix,
    })
    .to_string();
    let mut out = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&query),
        &mut out,
        std::ptr::null_mut(),
    );
    // SAFETY: the provider produced `out`; read then free it exactly once.
    let text = unsafe { out.to_string_lossy() };
    unsafe { rpi_plugin_sdk::callbacks::free_string(out) };
    if rc != 0 || text.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&text).ok()
}

/// Invoke a command's argument completions; returns `None` when it declines.
pub fn run_command_completions(command: &str, prefix: &str) -> Option<Value> {
    let handler = COMMAND_COMPLETIONS
        .lock()
        .unwrap()
        .iter()
        .find(|(name, _)| name == command)
        .map(|(_, handler)| *handler)
        .unwrap_or_else(|| panic!("no argument completions for /{command}"));
    let args = json!({"argumentPrefix": prefix}).to_string();
    let mut out = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&args),
        &mut out,
        std::ptr::null_mut(),
    );
    // SAFETY: the handler produced `out`; read then free it exactly once.
    let text = unsafe { out.to_string_lossy() };
    unsafe { rpi_plugin_sdk::callbacks::free_string(out) };
    if rc != 0 || text.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&text).ok()
}

/// How many `RuntimeActionId::Abort` calls the plugin made.
pub fn abort_count() -> u64 {
    ABORTS.load(Ordering::SeqCst)
}

/// Invoke the registered `begin_shutdown` hook (non-blocking half).
pub fn run_begin_shutdown() {
    let (begin, _) = lifecycle_hooks().expect("lifecycle hooks registered");
    begin(std::ptr::null_mut());
}

/// Invoke the registered `finish_shutdown` hook and return its status.
pub fn run_finish_shutdown() -> i32 {
    let (_, finish) = lifecycle_hooks().expect("lifecycle hooks registered");
    finish(std::ptr::null_mut())
}

/// Texts injected into the session via `SendMessage`/`SendUserMessage`.
pub fn sent_messages() -> Vec<String> {
    SENT.lock().unwrap().clone()
}

/// Invoke a registered slash command and return its JSON response.
pub fn run_command(name: &str, args: &str) -> Value {
    let handler = COMMANDS
        .lock()
        .unwrap()
        .iter()
        .find(|(registered, _)| registered == name)
        .map(|(_, handler)| *handler)
        .unwrap_or_else(|| panic!("command /{name} is not registered"));
    let input = json!({"command": name, "args": args}).to_string();
    let mut out = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&input),
        &mut out,
        std::ptr::null_mut(),
    );
    // SAFETY: the host produced `out`; read then free it exactly once.
    let text = unsafe { out.to_string_lossy() };
    unsafe { rpi_plugin_sdk::callbacks::free_string(out) };
    assert_eq!(rc, 0);
    serde_json::from_str(&text).expect("command returns JSON")
}

/// Dispatch an event to the handler registered for `tag`.
pub fn run_event(tag: EventTag, event: StablePluginEvent) -> i32 {
    let handler = EVENTS
        .lock()
        .unwrap()
        .iter()
        .find(|(registered, _)| *registered == tag)
        .map(|(_, handler)| *handler)
        .unwrap_or_else(|| panic!("no handler for {tag:?}"));
    handler(event, std::ptr::null_mut())
}

/// Dispatch an empty-payload event (SessionStart, AgentSettled, …).
pub fn run_empty_event(tag: EventTag) {
    let rc = run_event(tag, StablePluginEvent::empty(tag));
    assert_eq!(rc, 0, "event handler {tag:?} failed");
}

/// Dispatch a message-carrying event (MessageStart/Update/End).
pub fn run_message_event(tag: EventTag, payload: Value) {
    let text = payload.to_string();
    let event = StablePluginEvent::message(tag, StbString::from_string(text));
    let rc = run_event(tag, event);
    assert_eq!(rc, 0, "event handler {tag:?} failed");
}

/// Run the registered `before_agent_start` transformer.
pub fn run_before_agent_start(system_prompt: &str) -> Option<String> {
    let handler = PROMPT
        .lock()
        .unwrap()
        .expect("prompt transformer registered");
    let input = json!({"systemPrompt": system_prompt, "imageCount": 0}).to_string();
    let mut out = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&input),
        &mut out,
        std::ptr::null_mut(),
    );
    // SAFETY: the host produced `out`; read then free it exactly once.
    let text = unsafe { out.to_string_lossy() };
    unsafe { rpi_plugin_sdk::callbacks::free_string(out) };
    assert_eq!(rc, 0);
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// `GET /health` against the live bridge.
pub fn health(port: u16) -> Value {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .expect("write");
    let mut raw = String::new();
    let _ = stream.read_to_string(&mut raw);
    let (_, body) = raw.split_once("\r\n\r\n").expect("body");
    serde_json::from_str(body).expect("json body")
}

/// An unused loopback port (bound and released).
pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    listener.local_addr().unwrap().port()
}

/// Wait until `predicate` holds, or fail after ~3 s.
pub fn wait_until(what: &str, mut predicate: impl FnMut() -> bool) {
    for _ in 0..150 {
        if predicate() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}
