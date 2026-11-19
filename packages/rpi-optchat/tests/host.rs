//! Host-facing behaviour, driven through the plugin ABI with a fake host.
//!
//! The unit tests in `src/` cover the memory model, the importers and the
//! command surface. This file covers the part that only runs against a host:
//! which runtime actions are called with what, what happens when the host
//! refuses or fails one, and whether a cached log still describes the branch it
//! was loaded from.
//!
//! The plugin keeps session state in process-wide statics, so every test holds
//! [`LOCK`] and starts a fresh session.

use rpi_optchat::rpi_plugin_register;
use rpi_plugin_sdk::{
    CommandHandlerFn, EventHandlerFn, EventTag, PluginApi, RuntimeActionId, StablePluginEvent,
    StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::sync::{Mutex, MutexGuard};

/// One plugin process, one test at a time: the plugin's state is static.
static LOCK: Mutex<()> = Mutex::new(());

static COMMAND: Mutex<Option<CommandHandlerFn>> = Mutex::new(None);
static EVENTS: Mutex<Vec<(EventTag, EventHandlerFn)>> = Mutex::new(Vec::new());

/// The fake branch: ordered `(seq, record)` pairs, exactly as a host reports them.
static BRANCH: Mutex<Vec<Value>> = Mutex::new(Vec::new());
/// Every `AppendEntry` the plugin made, in call order.
static APPENDED: Mutex<Vec<Value>> = Mutex::new(Vec::new());
/// Every `SetStatus`, in call order.
static STATUS: Mutex<Vec<Value>> = Mutex::new(Vec::new());
/// When set, `AppendEntry` fails, which must never lose or duplicate a message.
static APPEND_FAILS: Mutex<bool> = Mutex::new(false);
/// When false, the host answers `GetSessionBranch` without `filtered`, the way
/// a host that ignores the query would.
static HONORS_FILTER: Mutex<bool> = Mutex::new(true);
/// Every `GetSessionBranch` query, so the shape a test relies on is asserted.
static QUERIES: Mutex<Vec<Value>> = Mutex::new(Vec::new());

fn lock() -> MutexGuard<'static, ()> {
    let guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    *BRANCH.lock().unwrap() = Vec::new();
    *APPENDED.lock().unwrap() = Vec::new();
    *STATUS.lock().unwrap() = Vec::new();
    *QUERIES.lock().unwrap() = Vec::new();
    *APPEND_FAILS.lock().unwrap() = false;
    *HONORS_FILTER.lock().unwrap() = true;
    guard
}

extern "C" fn register_command(_: StbStringRef, _: StbStringRef, handler: CommandHandlerFn) -> i32 {
    *COMMAND.lock().unwrap() = Some(handler);
    0
}

extern "C" fn register_event(
    tag: EventTag,
    handler: EventHandlerFn,
    _: *mut std::ffi::c_void,
) -> i32 {
    EVENTS.lock().unwrap().push((tag, handler));
    0
}

// Serve the three actions the plugin uses, with the host's own query contract:
// a custom-type filter, a bounded page and `afterSeq` paging.
rpi_plugin_sdk::string_handler! {
fn runtime_action(action, args, out, _user_data) {
    let args: Value = serde_json::from_str(args).unwrap();
    // Action 23 is `GetExecutionContext`: the published SDK predates the variant
    // and the plugin asks for it by number, so the fake matches on the number too.
    if action == 23 {
        out.write(json!({"model": {"id": "fake-model", "provider": "fake", "api": "openai"},
                         "thinkingLevel": "off"}).to_string());
        return 0;
    }
    let id = RuntimeActionId::try_from(action).expect("plugin called an undeclared action");
    match id {
        RuntimeActionId::GetSessionBranch => {
            out.write(get_session_branch(&args).to_string());
        }
        RuntimeActionId::AppendEntry => {
            if *APPEND_FAILS.lock().unwrap() {
                // A rejecting host answers with a non-zero rc, not with a JSON error.
                out.write(json!({"error": "the host refused the entry"}).to_string());
                return 1;
            }
            out.write(append_entry(&args).to_string());
        }
        RuntimeActionId::SetStatus => {
            STATUS.lock().unwrap().push(args.clone());
            out.write(json!({"ok": true, "changed": true}).to_string());
        }
        other => panic!("unexpected runtime action: {other:?}"),
    }
    0
}
}

fn get_session_branch(args: &Value) -> Value {
    QUERIES.lock().unwrap().push(args.clone());
    let records = BRANCH.lock().unwrap().clone();
    if !*HONORS_FILTER.lock().unwrap() {
        return json!({"entries": records});
    }
    let types: Vec<String> = args
        .get("customTypes")
        .and_then(Value::as_array)
        .map(|types| {
            types
                .iter()
                .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let after = args.get("afterSeq").and_then(Value::as_u64).unwrap_or(0);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(200)
        .max(1) as usize;
    let newest_first = args.get("order").and_then(Value::as_str) == Some("newestFirst");
    let selected: Vec<Value> = records
        .iter()
        .filter(|record| {
            record.get("seq").and_then(Value::as_u64).unwrap_or(0) > after
                && types
                    .iter()
                    .any(|want| record.get("customType").and_then(Value::as_str) == Some(want))
        })
        .cloned()
        .collect();
    let page: Vec<Value> = if newest_first {
        selected.into_iter().rev().take(limit).collect()
    } else {
        selected.into_iter().take(limit).collect()
    };
    json!({"entries": page, "count": page.len(), "filtered": true})
}

fn append_entry(args: &Value) -> Value {
    APPENDED.lock().unwrap().push(args.clone());
    let mut branch = BRANCH.lock().unwrap();
    let seq = branch.len() as u64 + 1;
    branch.push(json!({
        "seq": seq,
        "type": "custom",
        "customType": args["customType"],
        "data": args["data"],
    }));
    let id = args["data"].get("i").cloned().unwrap_or(json!(seq));
    json!({"entryId": format!("entry-{seq}"), "seq": seq, "id": id})
}

fn api() -> PluginApi {
    let mut api = PluginApi {
        abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        runtime_action,
        free_string: rpi_plugin_sdk::callbacks::free_string,
        user_data: std::ptr::null_mut(),
        register_command: None,
        register_tool: None,
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
    };
    api.register_command = Some(register_command);
    api.register_event_handler = Some(register_event);
    api.register_entry_renderer = Some(register_entry_renderer);
    api.register_tool = Some(register_tool);
    api
}

/// A fresh plugin bound to the fake host, with no session state cached.
fn start() -> MutexGuard<'static, ()> {
    let guard = lock();
    *COMMAND.lock().unwrap() = None;
    *EVENTS.lock().unwrap() = Vec::new();
    *RENDERER.lock().unwrap() = None;
    // Profile mirroring must not write into the checkout, and a developer's
    // environment must not decide which store these tests read.
    let store = std::env::temp_dir().join(format!("optchat-host-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    std::env::set_var("RPI_OPTCHAT_STORE_DIR", &store);
    std::env::set_var("RPI_OPTCHAT_PROFILE", "");
    rpi_plugin_register(&api());
    emit(
        EventTag::SessionStart,
        StablePluginEvent::empty(EventTag::SessionStart),
    );
    guard
}

fn emit(tag: EventTag, event: StablePluginEvent) {
    let handlers: Vec<EventHandlerFn> = EVENTS
        .lock()
        .unwrap()
        .iter()
        .filter(|(registered, _)| *registered == tag)
        .map(|(_, handler)| *handler)
        .collect();
    assert!(!handlers.is_empty(), "the plugin did not register {tag:?}");
    for handler in handlers {
        handler(event, std::ptr::null_mut());
    }
}

fn command(args: &str) -> Value {
    let handler = COMMAND.lock().unwrap().expect("command registered");
    let input = json!({"command": "/optchat", "args": args}).to_string();
    let mut output = StbString::empty();
    let rc = handler(
        StbStringRef::from_str(&input),
        &mut output,
        std::ptr::null_mut(),
    );
    let text = output.to_string_lossy();
    rpi_plugin_sdk::callbacks::free_string(output);
    assert_eq!(rc, 0, "command handler failed for `{args}`");
    serde_json::from_str(&text).expect("command returned JSON")
}

fn text(args: &str) -> String {
    command(args)["text"]
        .as_str()
        .expect("command returned text")
        .to_string()
}

/// Seed the fake branch with memory entries, the way a resumed session holds them.
fn seed(entries: &[(u64, &str, &str)]) {
    let mut branch = BRANCH.lock().unwrap();
    for (i, kind, body) in entries {
        let seq = branch.len() as u64 + 1;
        branch.push(json!({
            "seq": seq,
            "type": "custom",
            "customType": "optchat-entry",
            "data": {
                "i": i,
                "kind": kind,
                "text": body,
                "date": "2026-02-03T04:05:06Z",
            },
        }));
    }
}

#[test]
fn a_host_that_ignores_the_custom_type_filter_is_refused() {
    let _guard = start();
    seed(&[(0, "user", "old message")]);
    *HONORS_FILTER.lock().unwrap() = false;

    // The plugin never falls back to reading the whole branch: an unfiltered
    // answer is treated as "this host cannot serve memory", not as memory.
    let answer = text("status");
    assert!(
        answer.contains("unavailable"),
        "an unfiltered branch must not become memory: {answer}"
    );
    assert!(
        !answer.contains("old message"),
        "the branch content must not surface as memory: {answer}"
    );
    // The refusal is a read, not a write: nothing was appended.
    assert!(APPENDED.lock().unwrap().is_empty());
}

#[test]
fn hydrate_reads_entries_nodes_view_and_settings_from_the_branch() {
    let _guard = start();
    seed(&[
        (0, "user", "why is the parser slow"),
        (1, "talk", "it re-reads the log"),
    ]);
    {
        let mut branch = BRANCH.lock().unwrap();
        let push = |branch: &mut Vec<Value>, custom_type: &str, data: Value| {
            let seq = branch.len() as u64 + 1;
            branch.push(json!({
                "seq": seq,
                "type": "custom",
                "customType": custom_type,
                "data": data,
            }));
        };
        push(
            &mut branch,
            "optchat-node",
            json!({"l": 1, "i": 0, "text": "a model written line"}),
        );
        push(&mut branch, "optchat-config", json!({"compact": true}));
        push(&mut branch, "optchat-profile", json!({"name": "work"}));
        push(
            &mut branch,
            "optchat-view",
            json!({"parts": [[1, 0]], "merging": false}),
        );
    }

    let status = text("status");
    assert!(status.contains("2 messages"), "{status}");
    assert!(status.contains("compaction is on"), "{status}");
    assert!(status.contains("profile `work`"), "{status}");
    // The saved `optchat-view` shape wins over a fresh fold.
    assert!(status.contains("1 view lines"), "{status}");
    let view = text("view 5");
    assert!(view.contains("a model written line"), "{view}");

    // The filter is the plugin's privacy contract, and paging stays bounded.
    for query in QUERIES.lock().unwrap().iter() {
        assert!(query["customTypes"].is_array(), "{query}");
        assert!(query["limit"].as_u64().unwrap_or(0) > 0, "{query}");
    }
}

#[test]
fn a_new_message_on_the_branch_reloads_the_log_instead_of_appending_past_it() {
    let _guard = start();
    seed(&[(0, "user", "first")]);
    assert!(text("status").contains("1 messages"));

    // Another writer (a fork, a second process, a tree move) added message 1.
    seed(&[(1, "talk", "second, appended elsewhere")]);
    let status = text("status");
    assert!(status.contains("2 messages"), "{status}");
    assert!(
        status.contains("(ids 0..2)"),
        "the cache must be replaced by the branch, not extended: {status}"
    );
}

#[test]
fn a_note_is_persisted_before_it_is_committed_and_keeps_its_id() {
    let _guard = start();
    let answer = text("note remember the bounded window");
    assert!(answer.contains("message 0"), "{answer}");

    let appended = APPENDED.lock().unwrap().clone();
    assert_eq!(appended.len(), 1, "{appended:?}");
    assert_eq!(appended[0]["customType"], "optchat-entry");
    assert_eq!(appended[0]["data"]["kind"], "note");
    assert_eq!(appended[0]["data"]["text"], "remember the bounded window");
    assert_eq!(appended[0]["data"]["i"], 0);
    assert!(text("status").contains("1 messages"));
}

#[test]
fn a_failed_append_leaves_no_phantom_message_and_the_next_capture_retries() {
    let _guard = start();
    *APPEND_FAILS.lock().unwrap() = true;
    let refused = text("note this will not persist");
    assert!(refused.contains("refused"), "{refused}");
    assert!(
        text("status").contains("is empty"),
        "a refused entry must not appear in the log"
    );
    assert!(APPENDED.lock().unwrap().is_empty());

    *APPEND_FAILS.lock().unwrap() = false;
    let accepted = text("note this persists");
    assert!(accepted.contains("message 0"), "{accepted}");
    assert_eq!(APPENDED.lock().unwrap().len(), 1);
    assert!(text("status").contains("1 messages"));
}

#[test]
fn a_message_end_is_captured_once_per_distinct_message() {
    let _guard = start();
    let user = json!({
        "kind": "user",
        "content": [{"type": "text", "text": "why is the parser slow"}],
        "timestamp": 1_700_000_000_000i64,
    });
    let event = || {
        StablePluginEvent::message(
            EventTag::MessageEnd,
            StbString::from_string(user.to_string()),
        )
    };
    emit(EventTag::MessageEnd, event());
    // The same message delivered twice — a prompt arrives through both
    // `before_agent_start` and `message_end` — is logged once.
    emit(EventTag::MessageEnd, event());
    assert_eq!(APPENDED.lock().unwrap().len(), 1);
    assert!(text("status").contains("1 messages"));

    let reply = json!({
        "kind": "assistant",
        "content": [{"type": "text", "text": "it re-reads the whole log"}],
        "timestamp": 1_700_000_030_000i64,
    });
    emit(
        EventTag::MessageEnd,
        StablePluginEvent::message(
            EventTag::MessageEnd,
            StbString::from_string(reply.to_string()),
        ),
    );
    assert_eq!(APPENDED.lock().unwrap().len(), 2);
    assert!(text("status").contains("2 messages"));
}

#[test]
fn a_custom_message_from_another_extension_is_not_memory() {
    let _guard = start();
    let custom = json!({
        "kind": "custom",
        "role": "goal",
        "content": [{"type": "text", "text": "another extension's state"}],
        "data": {"active": true},
    });
    emit(
        EventTag::MessageEnd,
        StablePluginEvent::message(
            EventTag::MessageEnd,
            StbString::from_string(custom.to_string()),
        ),
    );
    assert!(APPENDED.lock().unwrap().is_empty());
    assert!(text("status").contains("is empty"));
}

#[test]
fn the_footer_is_published_with_the_counts_and_cleared_when_the_session_ends() {
    let _guard = start();
    seed(&[(0, "user", "first")]);
    text("status");

    let published: Vec<Value> = STATUS.lock().unwrap().clone();
    let footer = published
        .iter()
        .find(|status| status["key"] == "optchat" && status["value"] != "")
        .expect("the footer was published");
    let line = footer["value"].as_str().unwrap();
    assert!(line.contains("1 messages"), "{line}");
    assert!(line.contains("0..1"), "{line}");

    STATUS.lock().unwrap().clear();
    emit(
        EventTag::SessionShutdown,
        StablePluginEvent::empty(EventTag::SessionShutdown),
    );
    let cleared: Vec<Value> = STATUS.lock().unwrap().clone();
    assert!(
        cleared
            .iter()
            .any(|status| status["key"] == "optchat" && status["value"] == ""),
        "a closed session must not leave a footer behind: {cleared:?}"
    );
}

#[test]
fn a_tree_move_drops_the_cache_so_the_new_branch_is_read() {
    let _guard = start();
    seed(&[(0, "user", "branch a")]);
    assert!(text("status").contains("1 messages"));

    // The host moves to another leaf: the branch now holds a different message.
    emit(
        EventTag::SessionBeforeTree,
        StablePluginEvent::empty(EventTag::SessionBeforeTree),
    );
    {
        let mut branch = BRANCH.lock().unwrap();
        branch.clear();
        branch.push(json!({
            "seq": 1,
            "type": "custom",
            "customType": "optchat-entry",
            "data": {"i": 0, "kind": "user", "text": "branch b", "date": "2026-02-03T04:05:06Z"},
        }));
    }
    emit(
        EventTag::SessionTree,
        StablePluginEvent::empty(EventTag::SessionTree),
    );

    let view = text("view 5");
    assert!(view.contains("branch b"), "{view}");
    assert!(!view.contains("branch a"), "{view}");
}

#[test]
fn an_entry_renderer_reply_covers_optchat_entries_only() {
    let _guard = start();
    let handler = RENDERER.lock().unwrap().expect("entry renderer registered");
    let render = |value: Value| {
        let mut output = StbString::empty();
        let rc = handler(
            StbStringRef::from_str(&value.to_string()),
            &mut output,
            std::ptr::null_mut(),
        );
        let text = output.to_string_lossy();
        rpi_plugin_sdk::callbacks::free_string(output);
        (rc, text)
    };
    let (rc, text) = render(json!({
        "customType": "optchat-node",
        "data": {"l": 1, "i": 0, "text": "a model written line"},
    }));
    assert_eq!(rc, 0);
    assert!(text.contains("OptChat summary 1+0"), "{text}");
    assert!(text.contains("a model written line"), "{text}");

    let (rc, text) = render(json!({"customType": "goal", "data": {"active": true}}));
    assert_eq!(rc, 1, "another extension's entry must fall through");
    assert!(text.is_empty(), "{text}");
}

static RENDERER: Mutex<Option<rpi_plugin_sdk::RenderFn>> = Mutex::new(None);

// The tools themselves are unit-tested in `src/`; here they only have to be
// accepted so registration continues to the command and the renderer.
extern "C" fn register_tool(
    _schema: *const rpi_plugin_sdk::StableToolSchema,
    _execute: rpi_plugin_sdk::ToolExecuteFn,
    _poll: rpi_plugin_sdk::ToolPollFn,
    _cancel: rpi_plugin_sdk::ToolCancelFn,
    _destroy: rpi_plugin_sdk::ToolDestroyFn,
    _free: rpi_plugin_sdk::FreeStringFn,
) -> i32 {
    0
}

extern "C" fn register_entry_renderer(
    name: StbStringRef,
    render: rpi_plugin_sdk::RenderFn,
    _free: rpi_plugin_sdk::FreeStringFn,
    _: *mut std::ffi::c_void,
) -> i32 {
    assert_eq!(unsafe { name.as_str() }, "optchat");
    *RENDERER.lock().unwrap() = Some(render);
    0
}
