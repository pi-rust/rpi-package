//! Public ABI fixture: keep the module loaded while registering fresh owners.
//! The fake host independently validates owners, as native ActionBridge does.
use rpi_plugin_sdk::*;
use serde_json::json;
use std::{
    cell::Cell,
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Condvar, Mutex,
    },
    time::Duration,
};

type Tool = (ToolExecuteFn, ToolPollFn, ToolDestroyFn);
static COMMAND: Mutex<Option<CommandHandlerFn>> = Mutex::new(None);
static EVENT: Mutex<Option<(EventHandlerFn, usize)>> = Mutex::new(None);
static TOOL: Mutex<Option<Tool>> = Mutex::new(None);
static CALLS: Mutex<Vec<(usize, u32)>> = Mutex::new(Vec::new());
// Zero permits both owners (a candidate has registered, but not been swapped).
static LIVE: AtomicUsize = AtomicUsize::new(1);
static NEST: AtomicBool = AtomicBool::new(false);
static REJECT_EVENT: AtomicBool = AtomicBool::new(false);
static HOLD: Mutex<(bool, bool, bool)> = Mutex::new((false, false, false));
static CHANGED: Condvar = Condvar::new();
thread_local! { static INVOCATION: Cell<Option<usize>> = const { Cell::new(None) }; }

extern "C" fn command(_: StbStringRef, _: StbStringRef, handler: CommandHandlerFn) -> i32 {
    *COMMAND.lock().unwrap() = Some(handler);
    0
}
extern "C" fn event(tag: EventTag, handler: EventHandlerFn, data: *mut c_void) -> i32 {
    assert_eq!(tag, EventTag::TurnEnd);
    if REJECT_EVENT.swap(false, Ordering::SeqCst) {
        return 7;
    }
    *EVENT.lock().unwrap() = Some((handler, data as usize));
    0
}
extern "C" fn tool(
    _: *const StableToolSchema,
    execute: ToolExecuteFn,
    poll: ToolPollFn,
    _: ToolCancelFn,
    destroy: ToolDestroyFn,
    _: FreeStringFn,
) -> i32 {
    *TOOL.lock().unwrap() = Some((execute, poll, destroy));
    0
}
rpi_plugin_sdk::string_handler! {
fn action(action, _args, out, user_data) {
    // Native uses its invocation-scoped bridge for synchronous tool callbacks.
    let owner = INVOCATION.with(Cell::get).unwrap_or(user_data as usize);
    CALLS.lock().unwrap().push((owner, action));
    if LIVE.load(Ordering::SeqCst) != 0 && owner != LIVE.load(Ordering::SeqCst) {
        out.write(json!({"error":"expired runtime handle"}).to_string());
        return 1;
    }
    if action == RuntimeActionId::GetSessionBranch as u32 {
        let mut hold = HOLD.lock().unwrap();
        if hold.0 {
            hold.1 = true;
            CHANGED.notify_all();
            let (next, timeout) = CHANGED.wait_timeout_while(hold, Duration::from_secs(5), |h| !h.2).unwrap();
            assert!(!timeout.timed_out(), "fixture branch was never released");
            hold = next;
        }
        drop(hold);
        if owner == 1 && NEST.swap(false, Ordering::SeqCst) {
            assert!(!invoke(2, "show").contains("expired"));
        }
        out.write(json!({"filtered":true,"entries":[]}).to_string());
    } else {
        out.write(json!({"entryId":"fixture-entry"}).to_string());
    }
    CHANGED.notify_all();
    0
}
}
fn api(owner: usize) -> PluginApi {
    PluginApi {
        abi_version: RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        free_string: rpi_plugin_sdk::callbacks::free_string,
        runtime_action: action,
        user_data: owner as *mut c_void,
        register_tool: Some(tool),
        register_command: Some(command),
        register_event_handler: Some(event),
        register_shortcut: None,
        register_flag: None,
        register_provider: None,
        register_message_renderer: None,
        register_markdown_transformer: None,
        register_entry_renderer: None,
        register_resources_discover: None,
        dispatch_event: None,
        declare: None,
        register_before_agent_start: None,
        register_oauth_provider: None,
    }
}
fn invoke(owner: usize, args: &str) -> String {
    let callback = COMMAND.lock().unwrap().unwrap();
    let mut out = StbString::empty();
    let input = json!({"args":args}).to_string();
    let rc = callback(
        StbStringRef::from_str(&input),
        &mut out,
        owner as *mut c_void,
    );
    assert_eq!(rc, 0);
    let text = out.to_string_lossy();
    rpi_plugin_sdk::callbacks::free_string(out);
    text
}
fn drive(execute: ToolExecuteFn) -> StepHandle {
    execute(
        StbStringRef::from_str("call"),
        StbString::from_string(r#"{"action":"show"}"#.into()),
        Some(rpi_plugin_sdk::callbacks::free_string),
    )
}
fn await_worker(owner: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !CALLS
        .lock()
        .unwrap()
        .contains(&(owner, RuntimeActionId::SendUserMessage as u32))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "goal worker did not run"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn reload_preserves_explicit_callback_owners_and_pins_in_flight_work() {
    // Exact original regression: old owner expires while the DLL stays loaded.
    assert_eq!(rpi_goal::rpi_plugin_register(&api(1)), 0);
    assert!(!invoke(1, "show").contains("expired"));
    let old_event = EVENT.lock().unwrap().unwrap();
    let (execute, poll, destroy) = TOOL.lock().unwrap().unwrap();
    let old_drive = drive(execute);
    LIVE.store(2, Ordering::SeqCst);
    assert_eq!(rpi_goal::rpi_plugin_register(&api(2)), 0);
    CALLS.lock().unwrap().clear();
    assert!(!invoke(2, "show").contains("expired runtime handle"));
    let current_event = EVENT.lock().unwrap().unwrap();
    assert_eq!(
        current_event.1, 2,
        "event registration must retain its host owner"
    );
    (current_event.0)(
        StablePluginEvent::empty(EventTag::TurnEnd),
        current_event.1 as *mut c_void,
    );
    let new_drive = drive(execute);
    let result = poll(new_drive, None, std::ptr::null_mut());
    assert_eq!(result.tag, StepResultTag::Done);
    rpi_plugin_sdk::callbacks::free_step_result(result);
    destroy(new_drive);
    assert_eq!(
        CALLS
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.0)
            .collect::<Vec<_>>(),
        vec![2, 2, 2]
    );
    // Legacy hosts may supply null command context; keep the latest fallback.
    assert!(!invoke(0, "show").contains("expired"));
    CALLS.lock().unwrap().clear();
    (old_event.0)(
        StablePluginEvent::empty(EventTag::TurnEnd),
        old_event.1 as *mut c_void,
    );
    let result = poll(old_drive, None, std::ptr::null_mut());
    assert_eq!(result.tag, StepResultTag::Err);
    rpi_plugin_sdk::callbacks::free_step_result(result);
    destroy(old_drive);
    assert!(invoke(1, "show").contains("expired"));
    assert!(
        CALLS.lock().unwrap().iter().all(|c| c.0 == 1),
        "old work must not use owner 2"
    );

    // Registration succeeds, but native rejects the candidate and keeps A.
    // Its command and event must still use A, not reject it as a stale plugin.
    LIVE.store(1, Ordering::SeqCst);
    CALLS.lock().unwrap().clear();
    assert!(!invoke(1, "show").contains("expired"));
    (old_event.0)(
        StablePluginEvent::empty(EventTag::TurnEnd),
        old_event.1 as *mut c_void,
    );
    // Tools invoked AFTER candidate registration have native invocation routing.
    let after_candidate = drive(execute);
    INVOCATION.with(|scope| scope.set(Some(1)));
    let result = poll(after_candidate, None, std::ptr::null_mut());
    INVOCATION.with(|scope| scope.set(None));
    assert_eq!(result.tag, StepResultTag::Done);
    rpi_plugin_sdk::callbacks::free_step_result(result);
    destroy(after_candidate);
    assert!(CALLS.lock().unwrap().iter().all(|c| c.0 == 1));

    // A registrar can also fail before candidate construction completes.
    REJECT_EVENT.store(true, Ordering::SeqCst);
    assert_eq!(rpi_goal::rpi_plugin_register(&api(3)), 7);
    assert!(!invoke(1, "show").contains("expired"));
    (old_event.0)(
        StablePluginEvent::empty(EventTag::TurnEnd),
        old_event.1 as *mut c_void,
    );
    assert!(CALLS.lock().unwrap().iter().all(|c| c.0 == 1));

    // A blocked callback must retain A across registration, including its
    // deferred SendUserMessage worker. No API lock may block registration.
    LIVE.store(0, Ordering::SeqCst);
    assert_eq!(rpi_goal::rpi_plugin_register(&api(1)), 0);
    CALLS.lock().unwrap().clear();
    *HOLD.lock().unwrap() = (true, false, false);
    let thread = std::thread::spawn(|| invoke(1, "start pinned owner"));
    let (hold, timeout) = CHANGED
        .wait_timeout_while(HOLD.lock().unwrap(), Duration::from_secs(3), |h| !h.1)
        .unwrap();
    assert!(!timeout.timed_out());
    drop(hold);
    assert_eq!(rpi_goal::rpi_plugin_register(&api(2)), 0);
    HOLD.lock().unwrap().2 = true;
    CHANGED.notify_all();
    assert!(thread.join().unwrap().contains("Goal started"));
    await_worker(1);
    assert!(CALLS.lock().unwrap().iter().all(|c| c.0 == 1));
    *HOLD.lock().unwrap() = (false, false, false);

    // Reentrant B command resolves its explicit owner, not outer A's TLS;
    // after it returns, A's append and worker still use A.
    CALLS.lock().unwrap().clear();
    NEST.store(true, Ordering::SeqCst);
    assert!(invoke(1, "start reentrant owner").contains("Goal started"));
    await_worker(1);
    let calls = CALLS.lock().unwrap();
    assert_eq!(calls[0], (1, RuntimeActionId::GetSessionBranch as u32));
    assert_eq!(calls[1], (2, RuntimeActionId::GetSessionBranch as u32));
    assert!(calls[2..].iter().all(|c| c.0 == 1));
}
