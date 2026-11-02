#![forbid(unsafe_code)]
// The SDK's safe export macro owns the raw-pointer ABI boundary.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId, StableToolSchema,
    StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

const TOOL: &str = "todo";
const TODO_ENTRY: &str = "todo-state";

// Reuse the SDK's host-context lifetime and thread-safety contract.
type Runtime = PluginApi;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static TODOS: OnceLock<Mutex<Store>> = OnceLock::new();
// Serialize branch recovery and candidate commits, without holding the state
// mutex during host calls. Only explicit session reloads may clear a blocked store.
static COMMIT_GATE: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct Store {
    committed: TodoState,
    recovery: Recovery,
}

#[derive(Default)]
enum Recovery {
    #[default]
    Uninitialized,
    Ready,
    Blocked(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TodoState {
    todos: Vec<Todo>,
    next_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Todo {
    id: u64,
    text: String,
    done: bool,
    in_progress: bool,
}

fn state() -> &'static Mutex<Store> {
    TODOS.get_or_init(|| Mutex::new(Store::default()))
}

#[derive(Debug)]
enum RuntimeFailure {
    Rejected(String),
    InvalidResponse(String),
}

impl RuntimeFailure {
    fn message(self) -> String {
        match self {
            Self::Rejected(message) | Self::InvalidResponse(message) => message,
        }
    }
}

fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, RuntimeFailure> {
    let runtime = *RUNTIME
        .get()
        .ok_or_else(|| RuntimeFailure::Rejected("todo runtime is unavailable".into()))?;
    let mut out = StbString::empty();
    let input_json = args.to_string();
    let input = StbStringRef::from_str(&input_json);
    let rc = (runtime.runtime_action)(id.into(), input, &mut out, runtime.user_data);
    let text = out.to_string_lossy();
    (runtime.free_string)(out);
    if rc != 0 {
        return Err(RuntimeFailure::Rejected(if text.is_empty() {
            "runtime action failed".into()
        } else {
            text
        }));
    }
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&text)
            .map_err(|e| RuntimeFailure::InvalidResponse(format!("invalid runtime response: {e}")))
    }
}

fn parse_todos(value: &Value) -> Option<TodoState> {
    let todos = value.get("todos")?.as_array()?;
    let mut result = Vec::new();
    let mut next_id = value.get("nextId").and_then(Value::as_u64).unwrap_or(0);
    for item in todos {
        let id = item.get("id").and_then(Value::as_u64)?;
        let text = item.get("text").and_then(Value::as_str)?.to_string();
        result.push(Todo {
            id,
            text,
            done: item
                .get("done")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| item.get("status").and_then(Value::as_str) == Some("completed")),
            in_progress: item.get("status").and_then(Value::as_str) == Some("in_progress"),
        });
        next_id = next_id.max(id.saturating_add(1));
    }
    Some(TodoState {
        todos: result,
        next_id,
    })
}

fn state_json(state: &TodoState) -> Value {
    json!({"todos": state.todos.iter().map(|todo| json!({"id":todo.id,"text":todo.text,"done":todo.done,"status":if todo.done {"completed"} else if todo.in_progress {"in_progress"} else {"pending"}})).collect::<Vec<_>>(), "nextId": state.next_id})
}

// Caller holds COMMIT_GATE. A failed restore must not expose state from the
// previous branch or allow an implicit retry on the next tool invocation.
fn restore_from_branch() -> Result<(), String> {
    let restored = (|| {
        let value = call_runtime(
            RuntimeActionId::GetSessionBranch,
            json!({"customTypes": [TODO_ENTRY], "order": "newestFirst", "limit": 1}),
        )
        .map_err(RuntimeFailure::message)?;
        if value.get("filtered").and_then(Value::as_bool) != Some(true) {
            return Err("branch response did not confirm filtered: true".to_string());
        }
        let entries = value
            .get("entries")
            .and_then(Value::as_array)
            .ok_or("branch response has no entries array")?;
        if entries.len() > 1 {
            return Err("branch response exceeded snapshot limit".to_string());
        }
        let Some(entry) = entries.first() else {
            return Ok(TodoState::default());
        };
        if entry.get("type").and_then(Value::as_str) != Some("custom")
            || entry.get("customType").and_then(Value::as_str) != Some(TODO_ENTRY)
        {
            return Err("branch response contains an unexpected entry".to_string());
        }
        parse_todos(entry.get("data").unwrap_or(&Value::Null))
            .ok_or_else(|| "invalid todo-state snapshot".to_string())
    })();
    let mut store = state().lock().unwrap_or_else(|p| p.into_inner());
    match restored {
        Ok(restored) => {
            store.committed = restored;
            store.recovery = Recovery::Ready;
            Ok(())
        }
        Err(error) => {
            let error = format!(
                "todo restore failed: {error}; explicitly reload the session before retrying"
            );
            store.recovery = Recovery::Blocked(error.clone());
            Err(error)
        }
    }
}

// Caller holds COMMIT_GATE. Navigation need not dispatch SessionTree, so every
// unblocked use reads the latest self-contained snapshot on the current branch.
fn current_state() -> Result<TodoState, String> {
    {
        let store = state().lock().unwrap_or_else(|p| p.into_inner());
        if let Recovery::Blocked(error) = &store.recovery {
            return Err(error.clone());
        }
    }
    restore_from_branch()?;
    Ok(state()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .committed
        .clone())
}

rpi_plugin_sdk::event_handler! {
fn handle_event(event, _user_data) {
    match event.tag() {
        EventTag::SessionStart | EventTag::SessionTree => {
            let _gate = COMMIT_GATE.lock().unwrap_or_else(|p| p.into_inner());
            if restore_from_branch().is_err() {
                return 1;
            }
        }
        _ => {}
    }
    0
}
}

fn render(state: &TodoState, include_done: bool) -> String {
    if state.todos.is_empty() {
        return "No todos".into();
    }
    let done = state.todos.iter().filter(|todo| todo.done).count();
    let mut lines = vec![format!("{done}/{} completed", state.todos.len())];
    for todo in state.todos.iter().filter(|todo| include_done || !todo.done) {
        lines.push(format!(
            "[{}] #{}: {}",
            if todo.done {
                "✓"
            } else if todo.in_progress {
                "•"
            } else {
                " "
            },
            todo.id,
            todo.text
        ));
    }
    lines.join("\n")
}

fn result(action: &str, state: &TodoState, text: String, error: Option<&str>) -> String {
    let mut details = state_json(state);
    details["action"] = json!(action);
    if let Some(error) = error {
        details["error"] = json!(error);
    }
    json!({"content":[{"type":"text","text":text}],"details":details}).to_string()
}

fn todo(params: &Value) -> Result<String, String> {
    let _gate = COMMIT_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let committed = current_state()?;
    let mut next = committed.clone();
    let response = candidate(params, &mut next)?;
    if next == committed {
        return Ok(response);
    }
    let acknowledgement = call_runtime(
        RuntimeActionId::AppendEntry,
        json!({"customType": TODO_ENTRY, "data": state_json(&next)}),
    );
    match acknowledgement {
        Ok(value)
            if value
                .get("entryId")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty()) => {}
        // A nonzero host return is a rejected append, not a commit. Keep the
        // previously committed state readable and do not consume an ID.
        Err(RuntimeFailure::Rejected(error)) => {
            return Err(format!("todo persistence failed: {error}"));
        }
        // A successful host call with no usable identity may already have
        // appended. Block reads and writes until an explicit branch reload.
        unknown => {
            let reason = match unknown {
                Err(error) => error.message(),
                Ok(_) => "AppendEntry returned no usable entryId".into(),
            };
            let error = format!("todo append acknowledgement is unknown: {reason}; explicitly reload the session before retrying");
            state().lock().unwrap_or_else(|p| p.into_inner()).recovery =
                Recovery::Blocked(error.clone());
            return Err(error);
        }
    }
    state().lock().unwrap_or_else(|p| p.into_inner()).committed = next;
    Ok(response)
}

// Pure candidate construction: neither invalid input nor unchanged state writes
// a snapshot. The caller publishes only after an acknowledged append.
fn candidate(params: &Value, guard: &mut TodoState) -> Result<String, String> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    match action {
        "list" => Ok(result(
            action,
            guard,
            render(
                guard,
                params
                    .get("includeDone")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ),
            None,
        )),
        "add" => {
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if text.is_empty() {
                return Ok(result(
                    action,
                    guard,
                    "Error: text required for add".into(),
                    Some("text required"),
                ));
            }
            let todo = Todo {
                id: guard.next_id,
                text: text.to_string(),
                done: false,
                in_progress: false,
            };
            guard.next_id = todo.id.saturating_add(1);
            guard.todos.push(todo);
            Ok(result(action, guard, render(guard, true), None))
        }
        "toggle" => {
            let id = params
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id required for toggle")?;
            let Some(index) = guard.todos.iter().position(|todo| todo.id == id) else {
                return Ok(result(
                    action,
                    guard,
                    format!(
                        "Todo #{id} not found; call `todo` with action `list` and use the returned zero-based id"
                    ),
                    Some("todo not found"),
                ));
            };
            guard.todos[index].done = !guard.todos[index].done;
            guard.todos[index].in_progress = false;
            Ok(result(action, guard, render(guard, true), None))
        }
        "update" => {
            let id = params
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id required for update")?;
            let status = params
                .get("status")
                .and_then(Value::as_str)
                .ok_or("status required for update")?;
            if !matches!(status, "pending" | "in_progress" | "completed") {
                return Err("status must be pending, in_progress, or completed".into());
            }
            let Some(item) = guard.todos.iter_mut().find(|todo| todo.id == id) else {
                let text = format!(
                    "Todo #{id} not found; call `todo` with action `list` and use the returned zero-based id"
                );
                return Ok(result(action, guard, text, Some("todo not found")));
            };
            item.done = status == "completed";
            item.in_progress = status == "in_progress";
            Ok(result(action, guard, render(guard, true), None))
        }
        "clear" => {
            guard.todos.clear();
            guard.next_id = 0;
            Ok(result(action, guard, render(guard, true), None))
        }
        _ => Err("action must be one of: list, add, update, toggle, clear".into()),
    }
}

fn tool_schema() -> StableToolSchema {
    StableToolSchema {
        name: StbString::from_string(TOOL.into()),
        description: StbString::from_string("Track actionable tasks on the current session branch, not a project backlog or execution DAG. List first (includeDone=true to see completed tasks), then use returned IDs, not array positions. Add concrete tasks with acceptance checks; mark completed only after those checks pass. Prefer update for explicit status; toggle flips completion and clears in_progress. clear removes every task and resets IDs to 0. Use plan_mode_start/complete for read-only design and review, not todo bookkeeping. Omitting action lists unfinished tasks.".into()),
        parameters: StbString::from_string(json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "action": {"type": "string", "enum": ["list", "add", "update", "toggle", "clear"], "default": "list", "description": "Operation. clear deletes all tasks, including completed ones, and resets the next ID."},
                "text": {"type": "string", "pattern": "\\S", "description": "Required for add: concrete task and acceptance check. Trimmed before storage; whitespace-only text is rejected. Ignored by other actions."},
                "id": {"type": "integer", "minimum": 0, "maximum": u64::MAX, "description": "Required for update/toggle: exact ID returned by list/add. IDs start at 0; clear resets them. Not an array index."},
                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "Required for update. completed means the task's acceptance checks have passed. Ignored by other actions."},
                "includeDone": {"type": "boolean", "default": false, "description": "For list only: include completed tasks in displayed text. Result details always contain the full state."}
            },
            "allOf": [
                {"if": {"properties": {"action": {"const": "add"}}, "required": ["action"]}, "then": {"required": ["text"]}},
                {"if": {"properties": {"action": {"enum": ["update", "toggle"]}}, "required": ["action"]}, "then": {"required": ["id"]}},
                {"if": {"properties": {"action": {"const": "update"}}, "required": ["action"]}, "then": {"required": ["status"]}}
            ]
        }).to_string()),
    }
}

static HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    params: Value,
    done: bool,
}
extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    HANDLES.insert(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        done: false,
    })
}
extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null todo handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    if entry.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("todo cancelled".into()));
    }
    if drive.done {
        return StepResult::err(StbString::from_string(
            "todo polled after completion".into(),
        ));
    }
    drive.done = true;
    match todo(&drive.params) {
        Ok(value) => StepResult::done(StbString::from_string(value)),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}
extern "C" fn cancel(handle: StepHandle) {
    HANDLES.cancel(handle);
}
extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}
use rpi_plugin_sdk::callbacks::free_string;

rpi_plugin_sdk::string_handler! {
fn command(args, out, _user_data) {
    if !out.is_available() {
        return 1;
    }
    let _ = args;
    let _gate = COMMIT_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let response = match current_state() {
        Ok(state) => json!({"kind":"message","text":render(&state, true)}),
        Err(error) => json!({"kind":"message","text":format!("Error: {error}"),"error":error}),
    };
    // Nonzero callback results discard the payload in the host. Report a
    // visible error message instead of replacing it with generic commandfailed.
    out.write(response.to_string());
    0
}
}
rpi_plugin_sdk::export_plugin!(|api| {
    let _ = RUNTIME.set(*api);
    if let Some(register_event) = api.register_event_handler {
        for tag in [EventTag::SessionStart, EventTag::SessionTree] {
            if register_event(tag, handle_event as EventHandlerFn, std::ptr::null_mut()) != 0 {
                return 1;
            }
        }
    }
    if let Some(register_command) = api.register_command {
        let _ = register_command(
            StbStringRef::from_str("todos"),
            StbStringRef::from_str("Show todos on the current session branch"),
            command,
        );
    }
    let Some(register) = api.register_tool else {
        return 1;
    };
    let schema = Box::new(tool_schema());
    let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
    drop(schema);
    rc
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_schema_has_defaults_and_conditional_requirements() {
        // Parse the same schema constructor used by register_tool, not a fixture.
        let tool = tool_schema();
        assert_eq!(tool.name.to_string_lossy(), TOOL);
        let schema: Value = serde_json::from_str(&tool.parameters.to_string_lossy()).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], true);
        assert!(schema["properties"].get("__rpi").is_none());
        assert!(schema.get("required").is_none());
        let props = &schema["properties"];
        assert_eq!(props["action"]["default"], "list");
        assert_eq!(props["includeDone"]["default"], false);
        assert_eq!(props["text"]["pattern"], "\\S");
        assert_eq!(props["id"]["minimum"], 0);
        assert_eq!(props["id"]["maximum"], json!(u64::MAX));
        assert_eq!(
            props["status"]["enum"],
            json!(["pending", "in_progress", "completed"])
        );
        let rules = schema["allOf"].as_array().unwrap();
        assert_eq!(rules.len(), 3);
        for rule in rules {
            assert_eq!(rule["if"]["required"], json!(["action"]));
            assert!(rule["if"]["properties"]["action"].is_object());
            assert!(rule["then"]["required"].is_array());
        }
        assert_eq!(rules[0]["if"]["properties"]["action"]["const"], "add");
        assert_eq!(rules[0]["then"]["required"], json!(["text"]));
        assert_eq!(
            rules[1]["if"]["properties"]["action"]["enum"],
            json!(["update", "toggle"])
        );
        assert_eq!(rules[1]["then"]["required"], json!(["id"]));
        assert_eq!(rules[2]["if"]["properties"]["action"]["const"], "update");
        assert_eq!(rules[2]["then"]["required"], json!(["status"]));
    }

    #[test]
    fn parameter_rules_match_candidate_defaults_and_metadata_tolerance() {
        let mut state = TodoState::default();
        let listed: Value = serde_json::from_str(
            &candidate(
                &json!({"__rpi": {"nested": [1, {"trace": true}]}}),
                &mut state,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(listed["details"]["action"], "list");
        candidate(
            &json!({"action": "add", "text": "  Verify tests pass  ", "__rpi": {"trace": [1, 2]}}),
            &mut state,
        )
        .unwrap();
        assert_eq!(state.todos[0].text, "Verify tests pass");
        let before = state.clone();
        for input in [
            json!({"action":"add"}),
            json!({"action":"add","text":" \n\t"}),
        ] {
            let result: Value =
                serde_json::from_str(&candidate(&input, &mut state).unwrap()).unwrap();
            assert_eq!(result["details"]["error"], "text required");
        }
        for input in [
            json!({"action":"toggle"}),
            json!({"action":"toggle","id":-1}),
            json!({"action":"toggle","id":0.5}),
            json!({"action":"update","id":0}),
            json!({"action":"update","id":0,"status":"done"}),
        ] {
            assert!(candidate(&input, &mut state).is_err());
        }
        assert_eq!(state, before);
        candidate(
            &json!({"action":"update","id":0,"status":"completed"}),
            &mut state,
        )
        .unwrap();
        let hidden = candidate(&json!({}), &mut state).unwrap();
        let shown = candidate(&json!({"includeDone":true}), &mut state).unwrap();
        let hidden: Value = serde_json::from_str(&hidden).unwrap();
        let shown: Value = serde_json::from_str(&shown).unwrap();
        assert!(!hidden["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("#0"));
        assert!(shown["content"][0]["text"].as_str().unwrap().contains("#0"));
        assert_eq!(hidden["details"]["todos"], shown["details"]["todos"]);
        candidate(&json!({"action":"toggle","id":0}), &mut state).unwrap();
        assert!(!state.todos[0].done);
        assert!(!state.todos[0].in_progress);
        candidate(&json!({"action":"clear"}), &mut state).unwrap();
        assert_eq!(state, TodoState::default());
    }

    #[test]
    fn new_state_starts_at_zero() {
        let state = parse_todos(&json!({"todos":[]})).unwrap();
        assert_eq!(state.next_id, 0);
    }

    #[test]
    fn restores_state_from_details() {
        let state =
            parse_todos(&json!({"todos":[{"id":0,"text":"a","done":true}],"nextId":1})).unwrap();
        assert!(state.todos[0].done);
        assert_eq!(state.next_id, 1);
    }
    #[test]
    fn rejects_unknown_action() {
        assert!(candidate(&json!({"action":"remove"}), &mut TodoState::default()).is_err());
    }
    #[test]
    fn renders_and_restores_all_task_states() {
        let state = parse_todos(&json!({"todos":[
            {"id":0,"text":"Pending task","done":false},
            {"id":1,"text":"Working task","status":"in_progress"},
            {"id":2,"text":"Finished task","status":"completed"}
        ]}))
        .unwrap();
        let text = render(&state, true);
        assert!(text.contains("1/3 completed"));
        assert!(text.contains("[ ] #0: Pending task"));
        assert!(text.contains("[•] #1: Working task"));
        assert!(text.contains("[✓] #2: Finished task"));
        assert_eq!(state_json(&state)["todos"][2]["done"], true);
        assert_eq!(state_json(&state)["todos"][1]["status"], "in_progress");
    }

    #[test]
    fn candidates_preserve_actions_ids_and_noops_without_a_runtime() {
        let mut state = TodoState::default();
        let empty = state.clone();
        for params in [
            json!({"action":"list"}),
            json!({"action":"clear"}),
            json!({"action":"add","text":"  "}),
        ] {
            candidate(&params, &mut state).unwrap();
            assert_eq!(state, empty);
        }
        let added: Value = serde_json::from_str(
            &candidate(&json!({"action":"add","text":" first "}), &mut state).unwrap(),
        )
        .unwrap();
        assert_eq!(added["content"][0]["type"], "text");
        assert_eq!(
            added["details"],
            json!({"todos":[{"id":0,"text":"first","done":false,"status":"pending"}],"nextId":1,"action":"add"})
        );
        let saved = state.clone();
        for params in [
            json!({"action":"update","id":0,"status":"pending"}),
            json!({"action":"update","id":99,"status":"completed"}),
            json!({"action":"toggle","id":99}),
        ] {
            candidate(&params, &mut state).unwrap();
            assert_eq!(state, saved);
        }
        for params in [
            json!({"action":"update","id":0,"status":"bad"}),
            json!({"action":"update","id":0}),
            json!({"action":"toggle"}),
            json!({"action":"remove"}),
        ] {
            assert!(candidate(&params, &mut state).is_err());
            assert_eq!(state, saved);
        }
        candidate(
            &json!({"action":"update","id":0,"status":"in_progress"}),
            &mut state,
        )
        .unwrap();
        assert!(state.todos[0].in_progress);
        candidate(&json!({"action":"toggle","id":0}), &mut state).unwrap();
        assert!(state.todos[0].done);
        assert!(!state.todos[0].in_progress);
        let listed: Value =
            serde_json::from_str(&candidate(&json!({"action":"list"}), &mut state).unwrap())
                .unwrap();
        assert!(!listed["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("#0:"));
        assert_eq!(listed["details"]["todos"].as_array().unwrap().len(), 1);
        candidate(
            &json!({"action":"update","id":0,"status":"pending"}),
            &mut state,
        )
        .unwrap();
        candidate(&json!({"action":"add","text":"second"}), &mut state).unwrap();
        assert_eq!(state.todos[1].id, 1);
        candidate(&json!({"action":"clear"}), &mut state).unwrap();
        assert_eq!(state, empty);
    }

    struct MockHost {
        branch: Value,
        // Simulate a host violating the filtered/bounded response contract.
        branch_response: Option<Value>,
        restore_fails: bool,
        append_fails: bool,
        ack: String,
        reads: usize,
        appends: Vec<Value>,
        pause_append: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    }

    impl Default for MockHost {
        fn default() -> Self {
            Self {
                branch: json!({"filtered":true,"entries":[]}),
                branch_response: None,
                restore_fails: false,
                append_fails: false,
                ack: json!({"entryId":"todo-snapshot"}).to_string(),
                reads: 0,
                appends: Vec::new(),
                pause_append: None,
            }
        }
    }

    static HOST: OnceLock<Mutex<MockHost>> = OnceLock::new();

    fn host() -> &'static Mutex<MockHost> {
        HOST.get_or_init(|| Mutex::new(MockHost::default()))
    }

    rpi_plugin_sdk::string_handler! {
    fn branch_runtime(action, args, out, _user_data) {
        // Every host call must run under the separate gate and never under the
        // state mutex (the host is allowed to take its own locks).
        assert!(COMMIT_GATE.try_lock().is_err());
        assert!(state().try_lock().is_ok());
        let args: Value = serde_json::from_str(args).unwrap();
        let mut host = host().lock().unwrap();
        match RuntimeActionId::try_from(action).unwrap() {
            RuntimeActionId::GetSessionBranch => {
                assert_eq!(args, json!({"customTypes": ["todo-state"], "order":"newestFirst", "limit":1}));
                host.reads += 1;
                if host.restore_fails {
                    out.write("mock restore failure".into());
                    return 1;
                }
                let response = host.branch_response.clone().unwrap_or_else(|| {
                    let mut response = host.branch.clone();
                    if let Some(entries) = response.get_mut("entries").and_then(Value::as_array_mut) {
                        // Model native filtering before newest-first ordering/limit.
                        entries.retain(|entry| entry["type"] == "custom" && entry["customType"] == TODO_ENTRY);
                        entries.reverse();
                        entries.truncate(1);
                    }
                    response
                });
                out.write(response.to_string());
            }
            RuntimeActionId::AppendEntry => {
                assert_eq!(args["customType"], "todo-state");
                host.appends.push(args.clone());
                if host.append_fails {
                    out.write("mock append failure".into());
                    return 1;
                }
                host.branch["entries"].as_array_mut().unwrap().push(json!({
                    "type":"custom","customType":"todo-state","data":args["data"]
                }));
                out.write(host.ack.clone());
                let pause = host.pause_append.take();
                drop(host);
                if let Some((started, resume)) = pause {
                    started.send(()).unwrap();
                    resume.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
                }
            }
            other => panic!("unexpected action: {other:?}"),
        }
        0
    }
    }

    fn reload(tag: EventTag) -> i32 {
        handle_event(
            rpi_plugin_sdk::StablePluginEvent::empty(tag),
            std::ptr::null_mut(),
        )
    }

    fn invoke(params: Value, expected: rpi_plugin_sdk::StepResultTag) {
        let handle = execute(
            StbStringRef::from_str("call"),
            StbString::from_string(params.to_string()),
            Some(free_string),
        );
        let result = poll(handle, None, std::ptr::null_mut());
        assert_eq!(result.tag, expected);
        // This is a live result from this SDK instance; the SDK macro copies
        // the tagged payload and reclaims its allocation at the ABI boundary.
        match rpi_plugin_sdk::take_step_result!(result) {
            rpi_plugin_sdk::callbacks::Step::Done(text) => {
                let response: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(response["content"][0]["type"], "text");
                assert_eq!(response["details"]["action"], params["action"]);
                assert!(response["details"]["todos"].is_array());
                assert!(response["details"]["nextId"].is_u64());
            }
            rpi_plugin_sdk::callbacks::Step::Err(error) => assert!(!error.is_empty()),
            rpi_plugin_sdk::callbacks::Step::Pending(_) => panic!("todo must settle synchronously"),
        }
        destroy(handle);
    }

    fn details(params: Value) -> Value {
        serde_json::from_str::<Value>(&todo(&params).unwrap()).unwrap()["details"].clone()
    }

    fn append_count() -> usize {
        host().lock().unwrap().appends.len()
    }

    fn command_text(expect_error: bool) -> String {
        let mut out = StbString::empty();
        assert_eq!(
            command(StbStringRef::from_str(""), &mut out, std::ptr::null_mut()),
            0
        );
        let response: Value = serde_json::from_str(&out.to_string_lossy()).unwrap();
        free_string(out);
        assert_eq!(response["kind"], "message");
        assert_eq!(response["error"].is_string(), expect_error);
        if expect_error {
            assert!(response["text"].as_str().unwrap().starts_with("Error: "));
        }
        response["text"].as_str().unwrap().to_owned()
    }

    // All tests touching the FFI statics live in this one serial test. Other
    // tests exercise pure candidates and parsing, so parallel cargo tests are safe.
    #[test]
    fn ffi_commits_recovery_failures_and_serialization() {
        use rpi_plugin_sdk::StepResultTag::{Done, Err as Failed};
        assert!(RUNTIME
            .set(PluginApi {
                abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
                struct_size: std::mem::size_of::<PluginApi>() as u32,
                runtime_action: branch_runtime,
                free_string,
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
            })
            .is_ok());

        // No SessionStart and no completion event: --print's first mutation
        // lazily restores, appends, and only then returns Done/publishes ID 0.
        invoke(json!({"action":"add","text":"first"}), Done);
        assert_eq!(host().lock().unwrap().reads, 1);
        assert_eq!(append_count(), 1);
        assert_eq!(host().lock().unwrap().appends[0]["data"]["nextId"], 1);
        assert_eq!(details(json!({"action":"list"}))["todos"][0]["id"], 0);
        let reads = host().lock().unwrap().reads;
        for params in [
            json!({"action":"list"}),
            json!({"action":"add","text":" "}),
            json!({"action":"update","id":0,"status":"pending"}),
            json!({"action":"toggle","id":99}),
        ] {
            invoke(params, Done);
        }
        for params in [
            json!({"action":"update","id":0,"status":"bad"}),
            json!({"action":"toggle"}),
            json!({"action":"remove"}),
        ] {
            invoke(params, Failed);
        }
        assert_eq!(host().lock().unwrap().reads, reads + 7);
        assert_eq!(append_count(), 1);
        invoke(
            json!({"action":"update","id":0,"status":"in_progress"}),
            Done,
        );
        invoke(json!({"action":"toggle","id":0}), Done);
        assert_eq!(append_count(), 3);
        assert_eq!(
            details(json!({"action":"list"}))["todos"][0]["status"],
            "completed"
        );

        // Rejected append cannot advance memory or nextId; list stays usable.
        let committed = state().lock().unwrap().committed.clone();
        host().lock().unwrap().append_fails = true;
        invoke(json!({"action":"add","text":"not committed"}), Failed);
        invoke(json!({"action":"clear"}), Failed);
        assert_eq!(state().lock().unwrap().committed, committed);
        assert_eq!(details(json!({"action":"list"}))["nextId"], 1);
        host().lock().unwrap().append_fails = false;
        invoke(json!({"action":"add","text":"second"}), Done);
        assert_eq!(details(json!({"action":"list"}))["todos"][1]["id"], 1);

        // Navigation may omit SessionTree entirely. Commands and mutations must
        // follow A rather than copying the cached A+B snapshot onto that branch.
        let branch_ab = host().lock().unwrap().branch.clone();
        let branch_a = json!({"filtered":true,"entries":[branch_ab["entries"][0]]});
        host().lock().unwrap().branch = branch_a.clone();
        let text = command_text(false);
        assert!(text.contains("first"));
        assert!(!text.contains("second"));
        host().lock().unwrap().branch = branch_ab.clone();
        assert!(command_text(false).contains("second"));
        // Keep cached A+B, then navigate directly into a mutation on A.
        host().lock().unwrap().branch = branch_a;
        invoke(json!({"action":"add","text":"replacement"}), Done);
        let latest = host().lock().unwrap().appends.last().unwrap()["data"].clone();
        assert_eq!(latest["nextId"], 2);
        assert_eq!(latest["todos"].as_array().unwrap().len(), 2);
        assert_eq!(latest["todos"][0]["text"], "first");
        assert_eq!(latest["todos"][1]["text"], "replacement");
        assert_eq!(latest["todos"][1]["id"], 1);
        host().lock().unwrap().branch = branch_ab;
        assert!(command_text(false).contains("second"));
        invoke(json!({"action":"toggle","id":1}), Done);
        let restored = details(json!({"action":"list"}));
        assert_eq!(restored["todos"][1]["text"], "second");
        assert_eq!(restored["todos"][1]["status"], "completed");

        // A successful append with an invalid/unknown acknowledgement may
        // already be durable. Neither list nor another write may implicitly retry.
        for ack in [
            "",
            "{",
            "null",
            "{}",
            r#"{"entryId":null}"#,
            r#"{"entryId":7}"#,
            r#"{"entryId":""}"#,
            r#"{"entryId":"  "}"#,
        ] {
            host().lock().unwrap().ack = ack.into();
            let old = state().lock().unwrap().committed.clone();
            invoke(json!({"action":"add","text":"uncertain"}), Failed);
            assert_eq!(state().lock().unwrap().committed, old);
            let writes = append_count();
            let reads = host().lock().unwrap().reads;
            assert!(todo(&json!({"action":"list"}))
                .unwrap_err()
                .contains("acknowledgement is unknown"));
            invoke(json!({"action":"add","text":"must not retry"}), Failed);
            assert!(command_text(true).contains("acknowledgement is unknown"));
            assert_eq!(append_count(), writes);
            assert_eq!(host().lock().unwrap().reads, reads);
            host().lock().unwrap().ack = json!({"entryId":"recovered"}).to_string();
            assert_eq!(reload(EventTag::SessionTree), 0);
            assert_eq!(details(json!({"action":"list"}))["nextId"], old.next_id + 1);
        }

        let snapshot =
            json!({"todos":[{"id":7,"text":"Recovered","status":"in_progress"}],"nextId":11});
        host().lock().unwrap().branch = json!({"filtered":true,"entries":[
            {"type":"custom","customType":"other","data":{}},
            {"type":"custom","customType":"todo-state","data":{}},
            {"type":"custom","customType":"todo-state","data":snapshot},
            {"type":"custom","customType":"other","data":{}}
        ]});
        assert_eq!(reload(EventTag::SessionStart), 0);
        assert_eq!(details(json!({"action":"list"}))["nextId"], 11);
        assert!(state().lock().unwrap().committed.todos[0].in_progress);
        invoke(json!({"action":"add","text":"eleven"}), Done);
        assert_eq!(details(json!({"action":"list"}))["todos"][1]["id"], 11);

        // A fresh plugin instance lazily recovers a nonempty branch on list or
        // command use, without appending or needing a lifecycle event.
        let reads = host().lock().unwrap().reads;
        let writes = append_count();
        *state().lock().unwrap() = Store::default();
        invoke(json!({"action":"list"}), Done);
        assert_eq!(details(json!({"action":"list"}))["nextId"], 12);
        *state().lock().unwrap() = Store::default();
        assert!(command_text(false).contains("[•] #7: Recovered"));
        assert_eq!(host().lock().unwrap().reads, reads + 3);
        assert_eq!(append_count(), writes);

        // Bad restore envelopes and malformed snapshots fail closed, retain
        // but never expose stale memory, and require an explicit reload.
        let old = state().lock().unwrap().committed.clone();
        for branch in [
            json!({"entries":[]}),
            json!({"filtered":false,"entries":[]}),
            json!({"filtered":"true","entries":[]}),
            json!({"filtered":true}),
            json!({"filtered":true,"entries":{}}),
            json!({"filtered":true,"entries":[{}]}),
            json!({"filtered":true,"entries":[{"type":"message"}]}),
            json!({"filtered":true,"entries":[{"type":"custom","customType":"other","data":{"todos":[]}}]}),
            json!({"filtered":true,"entries":[{"type":"custom","customType":"todo-state","data":{"todos":[]}},{"type":"custom","customType":"todo-state","data":{"todos":[]}}]}),
            json!({"filtered":true,"entries":[{"type":"custom","customType":"todo-state","data":{"todos":[{}]}}]}),
        ] {
            host().lock().unwrap().branch_response = Some(branch);
            assert_eq!(reload(EventTag::SessionTree), 1);
            assert_eq!(state().lock().unwrap().committed, old);
            let reads = host().lock().unwrap().reads;
            let writes = append_count();
            assert!(todo(&json!({"action":"list"}))
                .unwrap_err()
                .contains("restore failed"));
            invoke(json!({"action":"add","text":"blocked"}), Failed);
            assert!(command_text(true).contains("restore failed"));
            assert_eq!(host().lock().unwrap().reads, reads);
            assert_eq!(append_count(), writes);
        }
        host().lock().unwrap().branch_response = None;
        host().lock().unwrap().restore_fails = true;
        assert_eq!(reload(EventTag::SessionStart), 1);
        assert!(todo(&json!({"action":"list"}))
            .unwrap_err()
            .contains("mock restore failure"));
        host().lock().unwrap().restore_fails = false;
        host().lock().unwrap().branch = json!({"filtered":true,"entries":[]});
        assert_eq!(reload(EventTag::SessionTree), 0);
        invoke(json!({"action":"clear"}), Done);
        assert_eq!(details(json!({"action":"list"}))["nextId"], 0);

        // Lazy recovery failure is also latched; changing the host response
        // alone cannot turn the next tool call into an implicit recovery.
        *state().lock().unwrap() = Store::default();
        host().lock().unwrap().restore_fails = true;
        invoke(json!({"action":"add","text":"blocked first use"}), Failed);
        let reads = host().lock().unwrap().reads;
        host().lock().unwrap().restore_fails = false;
        invoke(json!({"action":"list"}), Failed);
        assert_eq!(host().lock().unwrap().reads, reads);
        assert_eq!(reload(EventTag::SessionStart), 0);

        // First command use has the same latch, while preserving the detailed
        // error in a host-supported message response with a successful ABI rc.
        *state().lock().unwrap() = Store::default();
        host().lock().unwrap().restore_fails = true;
        assert!(command_text(true).contains("mock restore failure"));
        let reads = host().lock().unwrap().reads;
        host().lock().unwrap().restore_fails = false;
        assert!(command_text(true).contains("mock restore failure"));
        invoke(json!({"action":"add","text":"still blocked"}), Failed);
        assert_eq!(host().lock().unwrap().reads, reads);
        assert_eq!(reload(EventTag::SessionStart), 0);

        // Hold an append acknowledgement in flight: memory must still be old.
        // A competing mutation must build from the first acknowledged commit.
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        host().lock().unwrap().pause_append = Some((started_tx, resume_rx));
        let reads = host().lock().unwrap().reads;
        let writes = append_count();
        let first = std::thread::spawn(|| invoke(json!({"action":"add","text":"one"}), Done));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(state().lock().unwrap().committed.next_id, 0);
        let second = std::thread::spawn(|| invoke(json!({"action":"add","text":"two"}), Done));
        resume_tx.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        // Check counts only after both workers settle, never against a racing
        // in-flight query. No timing/sleep assumptions are needed.
        assert_eq!(host().lock().unwrap().reads, reads + 2);
        assert_eq!(append_count(), writes + 2);
        let restored = details(json!({"action":"list"}));
        assert_eq!(restored["nextId"], 2);
        assert_eq!(restored["todos"][0]["text"], "one");
        assert_eq!(restored["todos"][1]["text"], "two");
        assert_eq!(
            host().lock().unwrap().appends.last().unwrap()["data"]["todos"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        // Tree recovery also waits for the commit, and cannot be overwritten by
        // a stale in-flight publish after it has selected a different branch.
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        host().lock().unwrap().pause_append = Some((started_tx, resume_rx));
        let writer =
            std::thread::spawn(|| invoke(json!({"action":"add","text":"old branch"}), Done));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        host().lock().unwrap().branch = json!({"filtered":true,"entries":[{"type":"custom","customType":"todo-state","data":snapshot}]});
        let reader = std::thread::spawn(|| reload(EventTag::SessionTree));
        resume_tx.send(()).unwrap();
        writer.join().unwrap();
        assert_eq!(reader.join().unwrap(), 0);
        assert_eq!(details(json!({"action":"list"}))["nextId"], 11);
        assert_eq!(state().lock().unwrap().committed.todos.len(), 1);
        invoke(json!({"action":"clear"}), Done);
        let writes = append_count();
        let reads = host().lock().unwrap().reads;
        invoke(json!({"action":"clear"}), Done);
        invoke(json!({"action":"list"}), Done);
        assert_eq!(append_count(), writes);
        assert_eq!(host().lock().unwrap().reads, reads + 2);
    }
}
