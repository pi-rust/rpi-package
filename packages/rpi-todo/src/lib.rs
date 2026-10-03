use rpi_plugin_sdk::{
    register_entrypoint, EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId,
    StablePluginEvent, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult,
    ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

const TOOL: &str = "todo";

#[derive(Clone, Copy)]
struct Runtime {
    action: rpi_plugin_sdk::RuntimeActionFn,
    free_string: FreeStringFn,
    user_data: *mut c_void,
}
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

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
}

fn state() -> &'static Mutex<TodoState> {
    TODOS.get_or_init(|| Mutex::new(TodoState::default()))
}

fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, String> {
    let runtime = *RUNTIME.get().ok_or("todo runtime is unavailable")?;
    let mut out = StbString::empty();
    let input = StbStringRef::from_str(&args.to_string());
    let rc = (runtime.action)(id.into(), input, &mut out, runtime.user_data);
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
    let mut next_id = value.get("nextId").and_then(Value::as_u64).unwrap_or(1);
    for item in todos {
        let id = item.get("id").and_then(Value::as_u64)?;
        let text = item.get("text").and_then(Value::as_str)?.to_string();
        result.push(Todo {
            id,
            text,
            done: item.get("done").and_then(Value::as_bool).unwrap_or(false),
        });
        next_id = next_id.max(id.saturating_add(1));
    }
    Some(TodoState {
        todos: result,
        next_id,
    })
}

fn state_json(state: &TodoState) -> Value {
    json!({"todos": state.todos.iter().map(|todo| json!({"id":todo.id,"text":todo.text,"done":todo.done})).collect::<Vec<_>>(), "nextId": state.next_id})
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

extern "C" fn handle_event(event: StablePluginEvent, _: *mut c_void) -> i32 {
    match event.tag {
        EventTag::SessionStart | EventTag::SessionTree => restore_from_branch(),
        EventTag::ToolResult => unsafe {
            let payload = event.payload.tool_result;
            if payload.tool_name.to_string_lossy() == TOOL {
                if let Ok(result) = serde_json::from_str::<Value>(&payload.result.to_string_lossy())
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

fn render(state: &TodoState, include_done: bool) -> String {
    if state.todos.is_empty() {
        return "No todos".into();
    }
    let done = state.todos.iter().filter(|todo| todo.done).count();
    let mut lines = vec![format!("{done}/{} completed", state.todos.len())];
    for todo in state.todos.iter().filter(|todo| include_done || !todo.done) {
        lines.push(format!(
            "[{}] #{}: {}",
            if todo.done { "x" } else { " " },
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
                id: guard.next_id.max(1),
                text: text.to_string(),
                done: false,
            };
            guard.next_id = todo.id.saturating_add(1);
            guard.todos.push(todo.clone());
            Ok(result(
                action,
                &guard,
                format!("Added todo #{}: {}", todo.id, todo.text),
                None,
            ))
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
                    format!("Todo #{id} not found"),
                    Some("todo not found"),
                ));
            };
            guard.todos[index].done = !guard.todos[index].done;
            let done = guard.todos[index].done;
            Ok(result(
                action,
                &guard,
                format!(
                    "Todo #{id} {}",
                    if done { "completed" } else { "uncompleted" }
                ),
                None,
            ))
        }
        "clear" => {
            let count = guard.todos.len();
            guard.todos.clear();
            guard.next_id = 1;
            Ok(result(
                action,
                &guard,
                format!("Cleared {count} todos"),
                None,
            ))
        }
        _ => Err("action must be one of: list, add, toggle, clear".into()),
    }
}

struct Drive {
    params: Value,
    done: bool,
    cancelled: bool,
}
extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    Box::into_raw(Box::new(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        done: false,
        cancelled: false,
    })) as StepHandle
}
extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null todo handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled {
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
    if !handle.is_null() {
        unsafe {
            (*(handle as *mut Drive)).cancelled = true;
        }
    }
}
extern "C" fn destroy(handle: StepHandle) {
    if !handle.is_null() {
        unsafe {
            drop(Box::from_raw(handle as *mut Drive));
        }
    }
}
extern "C" fn free_string(value: StbString) {
    if !value.is_empty() && !value.ptr.is_null() {
        unsafe {
            let slice = std::slice::from_raw_parts(value.ptr as *const u8, value.len);
            let _ = Box::from_raw(slice as *const [u8] as *mut [u8]);
        }
    }
}

extern "C" fn command(args: StbStringRef, out: *mut StbString, _: *mut c_void) -> i32 {
    let text = render(&state().lock().unwrap_or_else(|p| p.into_inner()), true);
    if out.is_null() {
        return 1;
    }
    let _ = args;
    unsafe {
        *out = StbString::from_string(json!({"kind":"message","text":text}).to_string());
    }
    0
}

#[no_mangle]
pub extern "C" fn rpi_plugin_register(api: *const PluginApi) -> i32 {
    unsafe {
        register_entrypoint(api, |api| {
            let _ = RUNTIME.set(Runtime {
                action: api.runtime_action,
                free_string: api.free_string,
                user_data: api.user_data,
            });
            if let Some(register_event) = api.register_event_handler {
                for tag in [
                    EventTag::SessionStart,
                    EventTag::SessionTree,
                    EventTag::ToolResult,
                ] {
                    if register_event(tag, handle_event as EventHandlerFn, std::ptr::null_mut())
                        != 0
                    {
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
            description: StbString::from_string("Manage a session todo list. Actions: list, add (text), toggle (id), clear".into()),
            parameters: StbString::from_string(r#"{"type":"object","properties":{"action":{"type":"string","enum":["list","add","toggle","clear"]},"text":{"type":"string"},"id":{"type":"integer"},"includeDone":{"type":"boolean"}},"required":["action"]}"#.into()),
        });
            let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
            drop(schema);
            rc
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restores_state_from_details() {
        let state =
            parse_todos(&json!({"todos":[{"id":1,"text":"a","done":true}],"nextId":2})).unwrap();
        assert!(state.todos[0].done);
        assert_eq!(state.next_id, 2);
    }
    #[test]
    fn rejects_unknown_action() {
        assert!(todo(&json!({"action":"remove"})).is_err());
    }
}
