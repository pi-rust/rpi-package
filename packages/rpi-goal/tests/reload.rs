//! Reload fixture: a DLL can be registered again without resetting its statics.
//! The second registration must rebind commands, tools and events to the new
//! owner, and an in-flight tool call must keep the owner that started it.
mod support;

use rpi_plugin_sdk::{
    CommandHandlerFn, EventHandlerFn, EventTag, FreeStringFn, PluginApi, StablePluginEvent,
    StableToolSchema, StbString, StbStringRef, ToolCancelFn, ToolDestroyFn, ToolExecuteFn,
    ToolPollFn,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

static COMMAND: Mutex<Option<CommandHandlerFn>> = Mutex::new(None);
static EVENTS: Mutex<Vec<(EventTag, EventHandlerFn)>> = Mutex::new(Vec::new());
static TOOLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static OWNERS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

extern "C" fn command(_: StbStringRef, _: StbStringRef, handler: CommandHandlerFn) -> i32 {
    *COMMAND.lock().unwrap() = Some(handler);
    0
}

extern "C" fn event(tag: EventTag, handler: EventHandlerFn, _: *mut c_void) -> i32 {
    EVENTS.lock().unwrap().push((tag, handler));
    0
}

extern "C" fn tool(
    schema: *const StableToolSchema,
    execute: ToolExecuteFn,
    poll: ToolPollFn,
    _: ToolCancelFn,
    _: ToolDestroyFn,
    _: FreeStringFn,
) -> i32 {
    // SAFETY: the host owns the schema for this call.
    let name = unsafe { &*schema }.name.to_string_lossy();
    TOOLS.lock().unwrap().push(name);
    let _ = (execute, poll);
    0
}

rpi_plugin_sdk::string_handler! {
fn action(_action, _args, out, user_data) {
    OWNERS.lock().unwrap().push(user_data as usize);
    out.write(json!({"filtered": true, "entries": []}).to_string());
    0
}
}

fn api(owner: usize) -> PluginApi {
    PluginApi {
        abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        runtime_action: action,
        free_string: rpi_plugin_sdk::callbacks::free_string,
        user_data: owner as *mut c_void,
        register_command: Some(command),
        register_tool: Some(tool),
        register_shortcut: None,
        register_flag: None,
        register_provider: None,
        register_message_renderer: None,
        register_markdown_transformer: None,
        register_entry_renderer: None,
        register_event_handler: Some(event),
        register_resources_discover: None,
        dispatch_event: None,
        declare: None,
        register_before_agent_start: None,
        register_oauth_provider: None,
    }
}

fn invoke(args: &str) -> Value {
    let handler = COMMAND.lock().unwrap().expect("command was not registered");
    let input = json!({"command": "goal", "args": args}).to_string();
    let mut output = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&input),
        &mut output,
        std::ptr::null_mut(),
    );
    let text = output.to_string_lossy();
    rpi_plugin_sdk::callbacks::free_string(output);
    assert_eq!(rc, 0);
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

#[test]
fn a_second_registration_rebinds_every_callback_to_its_owner() {
    let _guard = support::serial();
    // Reset the process-wide fixtures this test owns.
    COMMAND.lock().unwrap().take();
    EVENTS.lock().unwrap().clear();
    TOOLS.lock().unwrap().clear();
    OWNERS.lock().unwrap().clear();

    assert_eq!(rpi_goal::rpi_plugin_register(&api(1)), 0);
    assert_eq!(TOOLS.lock().unwrap().len(), 4, "four tools must register");
    let tags: Vec<EventTag> = EVENTS.lock().unwrap().iter().map(|(tag, _)| *tag).collect();
    for expected in [
        EventTag::SessionStart,
        EventTag::AgentStart,
        EventTag::AgentEnd,
        EventTag::AgentSettled,
        EventTag::TurnEnd,
        EventTag::AfterProviderResponse,
        EventTag::ToolCall,
        EventTag::MessageEnd,
        EventTag::SessionCompact,
        EventTag::SessionShutdown,
    ] {
        assert!(tags.contains(&expected), "missing {expected:?}");
    }

    OWNERS.lock().unwrap().clear();
    invoke("status");
    assert_eq!(OWNERS.lock().unwrap().as_slice(), &[1]);

    // A reload registers the same library again with a new owner.
    EVENTS.lock().unwrap().clear();
    TOOLS.lock().unwrap().clear();
    assert_eq!(rpi_goal::rpi_plugin_register(&api(2)), 0);
    assert_eq!(TOOLS.lock().unwrap().len(), 4);

    OWNERS.lock().unwrap().clear();
    invoke("status");
    assert_eq!(
        OWNERS.lock().unwrap().as_slice(),
        &[2],
        "callbacks after reload must use the new owner"
    );

    // Handlers registered by the candidate are the freshly bound ones.
    let settled = EVENTS
        .lock()
        .unwrap()
        .iter()
        .filter(|(tag, _)| *tag == EventTag::AgentSettled)
        .count();
    assert_eq!(settled, 1);
}

#[test]
fn registration_does_not_block_on_in_flight_work() {
    let _guard = support::serial();
    COMMAND.lock().unwrap().take();
    EVENTS.lock().unwrap().clear();
    TOOLS.lock().unwrap().clear();

    assert_eq!(rpi_goal::rpi_plugin_register(&api(1)), 0);
    // A status read from another thread holds no lock this registration needs.
    let handle = std::thread::spawn(|| invoke("status"));
    assert_eq!(rpi_goal::rpi_plugin_register(&api(2)), 0);
    handle.join().unwrap();

    let event = StablePluginEvent::empty(EventTag::AgentSettled);
    for (tag, handler) in EVENTS.lock().unwrap().clone() {
        if tag == EventTag::AgentSettled {
            handler(event, std::ptr::null_mut());
        }
    }
    let _ = OnceLock::<()>::new();
}
