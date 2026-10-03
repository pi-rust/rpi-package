use rpi_plugin_sdk::{
    register_entrypoint, EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId,
    StablePluginEvent, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult,
    ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

const MAX_PLAN_CHARS: usize = 50_000;
const PLAN_TOOL: &str = "plan_mode_complete";
const START_TOOL: &str = "plan_mode_start";
const PLAN_ENTRY: &str = "plan-mode";

#[derive(Clone, Copy)]
struct Runtime {
    action: rpi_plugin_sdk::RuntimeActionFn,
    free_string: FreeStringFn,
    user_data: *mut c_void,
}

unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static STATE: OnceLock<Mutex<PlanState>> = OnceLock::new();

struct PlanState {
    active: bool,
    plan: Option<String>,
    previous_tools: Option<Vec<String>>,
}

fn state() -> &'static Mutex<PlanState> {
    STATE.get_or_init(|| {
        Mutex::new(PlanState {
            active: false,
            plan: None,
            previous_tools: None,
        })
    })
}

fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, String> {
    let runtime = *RUNTIME.get().ok_or("plan mode runtime is unavailable")?;
    let input = args.to_string();
    let mut output = StbString::empty();
    let rc = (runtime.action)(
        id as u32,
        StbStringRef::from_str(&input),
        &mut output,
        runtime.user_data,
    );
    let text = output.to_string_lossy();
    (runtime.free_string)(output);
    if rc != 0 {
        return Err(if text.is_empty() {
            format!("runtime action {id:?} failed")
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

fn current_tools() -> Result<Vec<String>, String> {
    let value = call_runtime(RuntimeActionId::GetActiveTools, json!({}))?;
    value
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| "runtime did not return an active tool list".to_string())
        .and_then(|tools| {
            tools
                .iter()
                .map(|tool| {
                    tool.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "active tool list contains a non-string value".to_string())
                })
                .collect()
        })
}

fn set_tools(tools: &[String]) -> Result<(), String> {
    call_runtime(RuntimeActionId::SetActiveTools, json!({"tools": tools}))?;
    Ok(())
}

fn persist_state(active: bool, plan: Option<&str>) {
    let _ = call_runtime(
        RuntimeActionId::AppendEntry,
        json!({"customType": PLAN_ENTRY, "data": {"active": active, "plan": plan}}),
    );
}

fn restore_state() {
    let Ok(value) = call_runtime(RuntimeActionId::GetSessionBranch, json!({})) else {
        return;
    };
    let mut restored = None;
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        for entry in entries {
            if entry.get("type").and_then(Value::as_str) == Some("custom")
                && entry.get("customType").and_then(Value::as_str) == Some(PLAN_ENTRY)
            {
                restored = entry.get("data").cloned();
            }
        }
    }
    if let Some(data) = restored {
        let mut guard = state().lock().unwrap_or_else(|p| p.into_inner());
        guard.active = data.get("active").and_then(Value::as_bool).unwrap_or(false);
        guard.plan = data.get("plan").and_then(Value::as_str).map(str::to_owned);
    }
}

extern "C" fn handle_event(event: StablePluginEvent, _: *mut c_void) -> i32 {
    if matches!(event.tag, EventTag::SessionStart | EventTag::SessionTree) {
        restore_state();
    }
    0
}

fn planning_tools() -> Vec<String> {
    vec![
        "read".to_string(),
        "bash".to_string(),
        "docs".to_string(),
        PLAN_TOOL.to_string(),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanItem {
    number: usize,
    text: String,
    completed: bool,
}

fn parse_plan_items(plan: &str) -> Vec<PlanItem> {
    let mut items: Vec<PlanItem> = Vec::new();
    let mut done = std::collections::HashSet::new();
    for line in plan.lines() {
        let line = line.trim();
        if let Some(index) = line
            .strip_prefix("[DONE:")
            .and_then(|value| value.strip_suffix(']'))
        {
            if let Ok(index) = index.trim().parse::<usize>() {
                done.insert(index);
            }
        }
    }
    for line in plan.lines() {
        let line = line.trim();
        if line.starts_with("[DONE:") {
            continue;
        }

        let mut candidate = line;
        let checked = if let Some(value) = candidate.strip_prefix("- [x] ") {
            candidate = value;
            true
        } else if let Some(value) = candidate.strip_prefix("- [X] ") {
            candidate = value;
            true
        } else if let Some(value) = candidate.strip_prefix("- [ ] ") {
            candidate = value;
            false
        } else {
            false
        };
        let Some((number, text)) = candidate.split_once('.') else {
            continue;
        };
        let Ok(number) = number.trim().parse::<usize>() else {
            continue;
        };
        let text = text.trim();
        if !text.is_empty() {
            let completed = checked || done.contains(&number);
            if let Some(existing) = items.iter_mut().find(|item| item.number == number) {
                existing.completed |= completed;
            } else {
                items.push(PlanItem {
                    number,
                    text: text.to_string(),
                    completed,
                });
            }
        }
    }
    items
}

fn plan_outline(plan: &str) -> String {
    let items = parse_plan_items(plan);
    if items.is_empty() {
        return "(Plan saved; use `/plan show` to view the full plan.)".to_string();
    }
    let total = items.len();
    let completed = items.iter().filter(|item| item.completed).count();
    let mut lines = vec![format!("Progress: {completed}/{total} complete")];
    for item in items.iter().take(12) {
        let marker = if item.completed { '✓' } else { '○' };
        lines.push(format!("{marker} {}. {}", item.number, item.text));
    }
    if total > 12 {
        lines.push("... (use `/plan show` to view the remaining steps)".to_string());
    }
    lines.join(
        "
",
    )
}

fn markdown_plan(plan: Option<&str>, active: bool) -> String {
    let (status, hint) = if active {
        (
            "🟡 Planning",
            "Explore first, then call `plan_mode_complete` when the plan is ready.",
        )
    } else {
        (
            "✅ Ready",
            "Use `/plan start` or `plan_mode_start` to create a new plan.",
        )
    };
    let body = match plan {
        Some(plan) if !plan.trim().is_empty() => plan.trim().to_string(),
        _ => "_No completed plan yet._".to_string(),
    };
    format!("## Plan\n\n**{status}**\n\n{body}\n\n> {hint}")
}

fn activate() -> Result<bool, String> {
    let mut guard = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.active {
        return Ok(false);
    }
    let previous = current_tools()?;
    set_tools(&planning_tools())?;
    guard.previous_tools = Some(previous);
    guard.active = true;
    guard.plan = None;
    drop(guard);
    persist_state(true, None);
    Ok(true)
}

fn deactivate() -> Result<bool, String> {
    let mut guard = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !guard.active {
        return Ok(false);
    }
    if let Some(previous) = guard.previous_tools.take() {
        set_tools(&previous)?;
    }
    guard.active = false;
    let plan = guard.plan.clone();
    drop(guard);
    persist_state(false, plan.as_deref());
    Ok(true)
}

fn plan_status() -> String {
    let guard = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    format!(
        "{}

Stored in the current session.

{}",
        markdown_plan(guard.plan.as_deref(), guard.active),
        if guard.active {
            "Next: continue inspecting the repository, then call `plan_mode_complete`."
        } else {
            "Next: use `/plan start` or `plan_mode_start` to begin planning."
        }
    )
}

fn start_plan(raw: &Value) -> Result<String, String> {
    let request = raw
        .get("request")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|request| !request.is_empty());
    activate()?;
    let message = request
        .map(|value| format!("Plan mode started for: {value}"))
        .unwrap_or_else(|| {
            "Plan mode started. Inspect the repository, then call `plan_mode_complete`.".to_string()
        });
    Ok(json!({
        "content": [{"type": "text", "text": message}],
        "details": {"kind": "plan", "active": true}
    })
    .to_string())
}

fn complete_plan(raw: &Value) -> Result<String, String> {
    let plan = raw
        .get("plan")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|plan| !plan.is_empty())
        .ok_or("plan must be a non-empty string")?;
    if plan.chars().count() > MAX_PLAN_CHARS {
        return Err(format!("plan must not exceed {MAX_PLAN_CHARS} characters"));
    }
    {
        let guard = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !guard.active {
            return Err("plan_mode_complete is only available while plan mode is active".into());
        }
    }
    {
        let mut guard = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.plan = Some(plan.to_string());
    }
    deactivate()?;
    Ok(json!({
        "content": [{"type": "text", "text": format!(
            "Plan complete.

{}

Normal tool access restored.",
            plan_outline(plan)
        )}],
        "details": {"kind": "plan", "plan": plan, "active": false}
    })
    .to_string())
}

struct Drive {
    params: Value,
    cancelled: bool,
    done: bool,
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
        cancelled: false,
        done: false,
    })) as StepHandle
}

extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null plan handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled {
        drive.done = true;
        return StepResult::err(StbString::from_string("plan cancelled".into()));
    }
    if drive.done {
        return StepResult::err(StbString::from_string(
            "plan polled after completion".into(),
        ));
    }
    drive.done = true;
    let result = if drive.params.get("plan").is_some() {
        complete_plan(&drive.params)
    } else {
        start_plan(&drive.params)
    };
    match result {
        Ok(value) => StepResult::done(StbString::from_string(value)),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}

extern "C" fn cancel(handle: StepHandle) {
    if !handle.is_null() {
        unsafe {
            (&mut *(handle as *mut Drive)).cancelled = true;
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

fn command_output(out: *mut StbString, value: Value) -> i32 {
    if out.is_null() {
        return 1;
    }
    unsafe { *out = StbString::from_string(value.to_string()) };
    0
}

extern "C" fn plan_command(args_json: StbStringRef, out: *mut StbString, _: *mut c_void) -> i32 {
    let raw = unsafe { args_json.as_str().to_owned() };
    let envelope: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let args = envelope
        .get("args")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let first = args.split_whitespace().next().unwrap_or("").to_lowercase();
    let result = match first.as_str() {
        "start" => match activate() {
            Ok(true) => {
                "Plan mode enabled. Inspect the codebase and call `plan_mode_complete` when ready."
                    .to_string()
            }
            Ok(false) => "Plan mode is already active.".to_string(),
            Err(error) => format!("Unable to enter plan mode: {error}"),
        },
        "show" | "status" | "" => plan_status(),
        "finalize" => {
            if state()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .plan
                .is_some()
            {
                "Plan is stored in the current session. Use `/plan show` to review it.".to_string()
            } else {
                "No plan yet. Complete plan mode with `plan_mode_complete` first.".to_string()
            }
        }
        "exit" | "off" => match deactivate() {
            Ok(true) => "Plan mode disabled. Full tool access restored.".to_string(),
            Ok(false) => "Plan mode is not active.".to_string(),
            Err(error) => format!("Unable to leave plan mode: {error}"),
        },
        _ => match activate() {
            Ok(_) => {
                let prompt = format!(
                    "You are now in Plan mode. Do not edit files or implement changes.\n\nUser request: {args}\n\nInspect the repository, resolve important decisions, and call `plan_mode_complete` with a complete implementation-ready Markdown plan when finished."
                );
                if let Err(error) =
                    call_runtime(RuntimeActionId::SendUserMessage, json!({"text": prompt}))
                {
                    let _ = deactivate();
                    format!("Unable to start planning request: {error}")
                } else {
                    "Plan mode started and the planning request was submitted.".to_string()
                }
            }
            Err(error) => format!("Unable to enter plan mode: {error}"),
        },
    };
    command_output(out, json!({"kind": "message", "text": result}))
}

#[no_mangle]
pub extern "C" fn rpi_plugin_register(api: *const PluginApi) -> i32 {
    unsafe {
        register_entrypoint(api, |api| {
            if let Some(register_event) = api.register_event_handler {
                for tag in [EventTag::SessionStart, EventTag::SessionTree] {
                    if register_event(tag, handle_event as EventHandlerFn, std::ptr::null_mut())
                        != 0
                    {
                        return 1;
                    }
                }
            }
            let Some(register) = api.register_tool else {
                return 1;
            };
            let _ = RUNTIME.set(Runtime {
                action: api.runtime_action,
                free_string: api.free_string,
                user_data: api.user_data,
            });
            let schema = Box::new(StableToolSchema {
                name: StbString::from_string(PLAN_TOOL.into()),
                description: StbString::from_string(
                    "Submit a complete implementation-ready plan and leave plan mode. Only available while /plan mode is active.".into(),
                ),
                parameters: StbString::from_string(format!(
                    r#"{{"type":"object","required":["plan"],"properties":{{"plan":{{"type":"string","minLength":1,"maxLength":{MAX_PLAN_CHARS},"description":"The complete decision-ready implementation plan in Markdown."}}}}}}"#
                )),
            });
            let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
            drop(schema);
            if rc != 0 {
                return rc;
            }
            let start_schema = Box::new(StableToolSchema {
                name: StbString::from_string(START_TOOL.into()),
                description: StbString::from_string(
                    "Enter Plan mode before making a complex change. Use this when the request needs repository exploration and an implementation plan.".into(),
                ),
                parameters: StbString::from_string(
                    r#"{"type":"object","properties":{"request":{"type":"string","description":"Optional user request to plan."}}}"#.into(),
                ),
            });
            let start_rc = register(&*start_schema, execute, poll, cancel, destroy, free_string);
            drop(start_schema);
            if start_rc != 0 {
                return start_rc;
            }
            if let Some(register_command) = api.register_command {
                let name = StbStringRef::from_str("plan");
                let description =
                    StbStringRef::from_str("Enter, inspect, or exit CodeX-like plan mode");
                let _ = register_command(name, description, plan_command);
            }
            0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_empty_status() {
        assert!(markdown_plan(None, false).contains("No completed plan"));
    }

    #[test]
    fn rejects_empty_plan() {
        assert!(complete_plan(&json!({"plan": ""})).is_err());
    }

    #[test]
    fn extracts_numbered_outline_without_markdown() {
        let outline = plan_outline("## Plan\n\n1. Add the parser\n2. Update the UI\n\nDetails");
        assert_eq!(
            outline,
            "Progress: 0/2 complete
○ 1. Add the parser
○ 2. Update the UI"
        );
    }

    #[test]
    fn marks_done_steps_from_tags_and_checkboxes() {
        let items = parse_plan_items(
            "1. First
2. Second
[DONE:1]
- [x] 2. Second",
        );
        assert_eq!(items[0].completed, true);
        assert_eq!(items[1].completed, true);
        assert!(plan_outline(
            "1. First
2. Second
[DONE:1]"
        )
        .contains("Progress: 1/2 complete"));
    }

    #[test]
    fn limits_long_outline() {
        let plan = (1..=13)
            .map(|index| format!("{index}. Step {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let outline = plan_outline(&plan);
        assert!(outline.contains("... (use `/plan show` to view the remaining steps)"));
        assert_eq!(outline.lines().count(), 14);
    }
}
