#![forbid(unsafe_code)]

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId, StableToolSchema,
    StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

const TOOL: &str = "todo";

// Reuse the SDK's host-context lifetime and thread-safety contract.
type Runtime = PluginApi;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static TODOS: OnceLock<Mutex<TodoState>> = OnceLock::new();

#[derive(Clone, Debug, Default)]
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

fn state() -> &'static Mutex<TodoState> {
    TODOS.get_or_init(|| Mutex::new(TodoState::default()))
}

fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, String> {
    let runtime = *RUNTIME.get().ok_or("todo runtime is unavailable")?;
    let mut out = StbString::empty();
    let input_json = args.to_string();
    let input = StbStringRef::from_str(&input_json);
    let rc = (runtime.runtime_action)(id.into(), input, &mut out, runtime.user_data);
    let text = out.to_string_lossy();
    (runtime.free_string)(out);
    if rc != 0 {
        return Err(if text.is_empty() {
            "runtime action failed".into()
        } else {
            text
        });
    }
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&text).map_err(|e| format!("invalid runtime response: {e}"))
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

fn restore_from_branch() {
    let Ok(value) = call_runtime(RuntimeActionId::GetSessionBranch, json!({})) else {
        return;
    };
    let mut restored = TodoState::default();
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        for entry in entries {
            let Some(message) = entry.get("message") else {
                continue;
            };
            if message.get("role").and_then(Value::as_str) != Some("toolResult")
                || message.get("toolName").and_then(Value::as_str) != Some(TOOL)
            {
                continue;
            }
            if let Some(next) = parse_todos(message.get("details").unwrap_or(&Value::Null)) {
                restored = next;
            }
        }
    }
    *state().lock().unwrap_or_else(|p| p.into_inner()) = restored;
}

rpi_plugin_sdk::event_handler! {
fn handle_event(event, _user_data) {
    match event.tag() {
        EventTag::SessionStart | EventTag::SessionTree => restore_from_branch(),
        EventTag::ToolResult => {
            let Some(payload) = event.tool_result() else { return 0; };
            if payload.tool_name == TOOL {
                if let Ok(result) = serde_json::from_str::<Value>(&payload.result)
                {
                    if let Some(next) = parse_todos(result.get("details").unwrap_or(&Value::Null)) {
                        *state().lock().unwrap_or_else(|p| p.into_inner()) = next;
                    }
                }
            }
        },
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
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    let mut guard = state().lock().unwrap_or_else(|p| p.into_inner());
    match action {
        "list" => Ok(result(
            action,
            &guard,
            render(
                &guard,
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
                    &guard,
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
            guard.todos.push(todo.clone());
            Ok(result(action, &guard, render(&guard, true), None))
        }
        "toggle" => {
            let id = params
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("id required for toggle")?;
            let Some(index) = guard.todos.iter().position(|todo| todo.id == id) else {
                return Ok(result(
                    action,
                    &guard,
                    format!(
                        "Todo #{id} not found; call `todo` with action `list` and use the returned zero-based id"
                    ),
                    Some("todo not found"),
                ));
            };
            guard.todos[index].done = !guard.todos[index].done;
            guard.todos[index].in_progress = false;
            Ok(result(action, &guard, render(&guard, true), None))
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
                return Ok(result(action, &guard, text, Some("todo not found")));
            };
            item.done = status == "completed";
            item.in_progress = status == "in_progress";
            Ok(result(action, &guard, render(&guard, true), None))
        }
        "clear" => {
            guard.todos.clear();
            guard.next_id = 0;
            Ok(result(action, &guard, render(&guard, true), None))
        }
        _ => Err("action must be one of: list, add, update, toggle, clear".into()),
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
    let text = render(&state().lock().unwrap_or_else(|p| p.into_inner()), true);
    if !out.is_available() {
        return 1;
    }
    let _ = args;
    out.write(json!({"kind":"message","text":text}).to_string());
    0
}
}
rpi_plugin_sdk::export_plugin!(|api| {
    let _ = RUNTIME.set(*api);
    if let Some(register_event) = api.register_event_handler {
        for tag in [
            EventTag::SessionStart,
            EventTag::SessionTree,
            EventTag::ToolResult,
        ] {
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
    let schema = Box::new(StableToolSchema {
    name: StbString::from_string(TOOL.into()),
    description: StbString::from_string("Manage one session task list. Task IDs are zero-based: the first added task is #0. Always call list first and use the exact returned id for update/toggle; an id is not an array position after tasks are removed or cleared. Actions: list, add (text), update (id, status: pending/in_progress/completed), toggle (id), clear.".into()),
    parameters: StbString::from_string(r#"{"type":"object","properties":{"action":{"type":"string","enum":["list","add","update","toggle","clear"]},"text":{"type":"string"},"id":{"type":"integer","minimum":0,"description":"Zero-based task ID returned by list/add; the first task is 0."},"status":{"type":"string","enum":["pending","in_progress","completed"]},"includeDone":{"type":"boolean"}},"required":["action"]}"#.into()),
});
    let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
    drop(schema);
    rc
});

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(todo(&json!({"action":"remove"})).is_err());
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
}
