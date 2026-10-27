use rpi_plugin_sdk::{
    callbacks::free_string, EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId,
    StablePluginEvent, StableToolSchema, StbString, StbStringRef, StepResultTag, ToolCancelFn,
    ToolDestroyFn, ToolExecuteFn, ToolPartialCb, ToolPollFn,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy)]
struct ToolFns {
    execute: ToolExecuteFn,
    poll: ToolPollFn,
    destroy: ToolDestroyFn,
    free: FreeStringFn,
}
#[derive(Default)]
struct Host {
    entries: Vec<Value>,
    next_id: u32,
    fail_read: bool,
    fail_append: bool,
    missing_ack: bool,
    invalid_json_ack: bool,
    tool: Option<ToolFns>,
    events: Vec<(EventTag, EventHandlerFn)>,
}
static HOST: OnceLock<Mutex<Host>> = OnceLock::new();
fn host() -> &'static Mutex<Host> {
    HOST.get_or_init(|| Mutex::new(Host::default()))
}

extern "C" fn register_tool(
    _: *const StableToolSchema,
    execute: ToolExecuteFn,
    poll: ToolPollFn,
    _: ToolCancelFn,
    destroy: ToolDestroyFn,
    free: FreeStringFn,
) -> i32 {
    host().lock().unwrap().tool = Some(ToolFns {
        execute,
        poll,
        destroy,
        free,
    });
    0
}
extern "C" fn register_command(
    _: StbStringRef,
    _: StbStringRef,
    _: rpi_plugin_sdk::CommandHandlerFn,
) -> i32 {
    0
}
extern "C" fn register_event(tag: EventTag, handler: EventHandlerFn, _: *mut c_void) -> i32 {
    host().lock().unwrap().events.push((tag, handler));
    0
}
extern "C" fn runtime(
    action: u32,
    input: StbStringRef,
    out: *mut StbString,
    _: *mut c_void,
) -> i32 {
    // The host owns this output allocation and the plugin frees it through `free_string`.
    let args = StbString {
        ptr: input.ptr as *mut _,
        len: input.len,
    }
    .to_string_lossy();
    let value: Value = serde_json::from_str(&args).unwrap();
    let mut host = host().lock().unwrap();
    let mut invalid_json_ack = false;
    let (rc, result) = match RuntimeActionId::try_from(action).unwrap() {
        RuntimeActionId::GetSessionBranch if host.fail_read => (1, json!({"error":"read failed"})),
        RuntimeActionId::GetSessionBranch => (0, json!({"entries":host.entries})),
        RuntimeActionId::AppendEntry if host.fail_append => {
            host.fail_append = false;
            (1, json!({"error":"append failed"}))
        }
        RuntimeActionId::AppendEntry => {
            host.next_id += 1;
            let id = format!("entry-{}", host.next_id);
            host.entries.push(
                json!({"id":id,"type":"custom","customType":"durable-state","data":value["data"]}),
            );
            invalid_json_ack = std::mem::take(&mut host.invalid_json_ack);
            let result = if host.missing_ack {
                host.missing_ack = false;
                json!({})
            } else {
                json!({"entryId":id})
            };
            (0, result)
        }
        other => panic!("unexpected runtime action: {other:?}"),
    };
    // SAFETY: the plugin supplies a live output slot for this synchronous call.
    // Ownership of the allocation transfers to it with the host free callback.
    unsafe {
        *out = StbString::from_string(if invalid_json_ack {
            "not-json".into()
        } else {
            result.to_string()
        })
    };
    rc
}

fn dispatch(event: StablePluginEvent) -> i32 {
    let handlers = host().lock().unwrap().events.clone();
    handlers
        .into_iter()
        .filter(|(tag, _)| *tag == event.tag)
        .map(|(_, handler)| handler(event, std::ptr::null_mut()))
        .find(|rc| *rc != 0)
        .unwrap_or(0)
}
fn empty(tag: EventTag) -> i32 {
    dispatch(StablePluginEvent::empty(tag))
}
fn action(params: Value) -> Result<Value, String> {
    let tool = host().lock().unwrap().tool.unwrap();
    let handle = (tool.execute)(
        StbStringRef::from_str("contract"),
        StbString::from_string(params.to_string()),
        Some(free_string),
    );
    let result = (tool.poll)(handle, None::<ToolPartialCb>, std::ptr::null_mut());
    // SAFETY: each branch reads only the union member selected by its tag;
    // the plugin's free callback releases that owned string exactly once.
    let answer = match result.tag {
        StepResultTag::Done => unsafe {
            let value = result.done_payload().result;
            let text = value.to_string_lossy();
            (tool.free)(value);
            Ok(serde_json::from_str::<Value>(&text).unwrap())
        },
        StepResultTag::Err => unsafe {
            let value = result.err_payload().message;
            let text = value.to_string_lossy();
            (tool.free)(value);
            Err(text)
        },
        StepResultTag::Pending => panic!("durable tool unexpectedly pending"),
    };
    (tool.destroy)(handle);
    answer
}
fn legacy() -> Value {
    json!({"id":"legacy","type":"custom","customType":"durable-state","data":{"workflow":null}})
}

#[test]
fn runtime_contract_is_sequential_and_fail_closed() {
    let api = PluginApi {
        abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        free_string,
        register_tool: Some(register_tool),
        register_command: Some(register_command),
        register_shortcut: None,
        register_flag: None,
        register_provider: None,
        register_message_renderer: None,
        register_markdown_transformer: None,
        register_entry_renderer: None,
        register_event_handler: Some(register_event),
        register_resources_discover: None,
        runtime_action: runtime,
        dispatch_event: None,
        user_data: std::ptr::null_mut(),
        declare: None,
        register_before_agent_start: None,
        register_oauth_provider: None,
    };
    assert_eq!(rpi_durable::rpi_plugin_register(&api), 0);

    let mut old = legacy();
    old["data"]["workflow"] = json!({
        "name":"legacy loaded before SessionStart",
        "status":"running",
        "steps":[{"id":"s","title":"S","status":"pending"}]
    });
    host().lock().unwrap().entries = vec![old];
    assert_eq!(
        action(json!({"action":"status"})).unwrap()["details"]["workflow"]["name"],
        "legacy loaded before SessionStart"
    );
    assert_eq!(host().lock().unwrap().entries.len(), 1);
    assert_eq!(empty(EventTag::SessionTree), 0);
    action(json!({"action":"enqueue","text":"persist"})).unwrap();

    host().lock().unwrap().fail_read = true;
    assert_ne!(empty(EventTag::SessionTree), 0);
    let before_blocked = host().lock().unwrap().entries.clone();
    let blocked = action(json!({"action":"enqueue","text":"blocked"})).unwrap();
    assert!(blocked["details"]["error"].is_string());
    assert_eq!(host().lock().unwrap().entries, before_blocked);
    host().lock().unwrap().fail_read = false;
    host().lock().unwrap().entries.clear();
    assert_eq!(empty(EventTag::SessionTree), 0);

    let before_failure = action(json!({"action":"status"})).unwrap();
    let before_failure_entries = host().lock().unwrap().entries.clone();
    host().lock().unwrap().fail_append = true;
    assert!(action(json!({"action":"enqueue","text":"retry"})).is_err());
    assert_eq!(action(json!({"action":"status"})).unwrap(), before_failure);
    assert_eq!(host().lock().unwrap().entries, before_failure_entries);
    action(json!({"action":"enqueue","text":"retry"})).unwrap();
    host().lock().unwrap().missing_ack = true;
    assert!(action(json!({"action":"enqueue","text":"ack"})).is_err());
    let acknowledged_unknown = host().lock().unwrap().entries.clone();
    let no_retry = action(json!({"action":"enqueue","text":"no blind retry"})).unwrap();
    assert!(no_retry["details"]["error"].is_string());
    assert_eq!(host().lock().unwrap().entries, acknowledged_unknown);
    assert_eq!(empty(EventTag::SessionTree), 0);
    let recovered = action(json!({"action":"status"})).unwrap();
    assert_eq!(recovered["details"]["queue"][1]["text"], "ack");
    action(json!({"action":"enqueue","text":"after reconciliation"})).unwrap();
    host().lock().unwrap().invalid_json_ack = true;
    assert!(action(json!({"action":"enqueue","text":"committed with malformed ack"})).is_err());
    let malformed_ack_entries = host().lock().unwrap().entries.clone();
    let blocked =
        action(json!({"action":"enqueue","text":"must block after malformed ack"})).unwrap();
    assert!(blocked["details"]["error"].is_string());
    assert_eq!(host().lock().unwrap().entries, malformed_ack_entries);
    assert_eq!(empty(EventTag::SessionTree), 0);
    let recovered = action(json!({"action":"status"})).unwrap();
    assert_eq!(
        recovered["details"]["queue"][3]["text"],
        "committed with malformed ack"
    );

    let mut corrupt = host().lock().unwrap().entries.clone();
    corrupt.last_mut().unwrap()["data"]["version"] = json!(999);
    host().lock().unwrap().entries = corrupt;
    assert_ne!(empty(EventTag::SessionTree), 0);
    let corrupt_failed = action(json!({"action":"enqueue","text":"corrupt"})).unwrap();
    assert!(corrupt_failed["details"]["error"].is_string());

    host().lock().unwrap().entries = vec![legacy()];
    assert_eq!(empty(EventTag::SessionTree), 0);
    action(json!({"action":"enqueue","text":"upgrade"})).unwrap();
    let mut downgrade = host().lock().unwrap().entries.clone();
    downgrade.push(legacy());
    host().lock().unwrap().entries = downgrade;
    assert_ne!(empty(EventTag::SessionTree), 0);
    host().lock().unwrap().entries.clear();
    assert_eq!(empty(EventTag::SessionTree), 0);
}
