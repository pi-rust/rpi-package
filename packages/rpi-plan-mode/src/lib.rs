use rpi_plugin_sdk::{
    register_entrypoint_unified, FreeStringFn, PluginApi, RuntimeActionId, StableToolSchema,
    StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const MAX_PLAN_CHARS: usize = 50_000;
const PLAN_TOOL: &str = "plan_mode_complete";
const START_TOOL: &str = "plan_mode_start";

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

fn planning_tools() -> Vec<String> {
    vec![
        "read".to_string(),
        "bash".to_string(),
        "docs".to_string(),
        PLAN_TOOL.to_string(),
    ]
}

fn plan_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".rpi")
        .join("PLAN.md")
}

fn save_plan(plan: &str) -> Result<PathBuf, String> {
    let path = plan_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create plan directory: {e}"))?;
    }
    let tmp = path.with_file_name(format!("PLAN.md.tmp-{}", std::process::id()));
    fs::write(&tmp, format!("# Implementation Plan\n\n{plan}\n"))
        .map_err(|e| format!("write plan: {e}"))?;
    if let Err(error) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("commit plan: {error}"));
    }
    Ok(path)
}

fn saved_plan() -> Option<String> {
    fs::read_to_string(plan_path()).ok().and_then(|text| {
        text.strip_prefix("# Implementation Plan\n\n")
            .map(|plan| plan.trim().to_string())
            .filter(|plan| !plan.is_empty())
    })
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
    format!(
        "## Plan Mode\n\n**Status:** {status}\n\n### Implementation plan\n\n{body}\n\n---\n\n_{hint}_"
    )
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
    Ok(true)
}

fn plan_status() -> String {
    let guard = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let saved = saved_plan();
    let stored = guard.plan.as_deref().or(saved.as_deref());
    let path = plan_path();
    let location = if path.exists() {
        format!("Saved plan: `{}`", path.display())
    } else {
        "Saved plan: _none_".to_string()
    };
    format!(
        "{}\n\n{}\n\n{}",
        markdown_plan(stored, guard.active),
        location,
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
    Ok(json!({
        "content": [{"type": "text", "text": format!(
            "## Plan Mode\n\n**Status:** 🟡 Planning\n\n{}\n\n---\n\n_The model must inspect the repository without editing files, then submit the complete plan with `plan_mode_complete`._",
            request.map(|value| format!("### Request\n\n> {value}" )).unwrap_or_else(|| "_No request supplied._".to_string())
        )}],
        "details": {"kind": "plan", "active": true, "markdown": true}
    }).to_string())
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
    let path = save_plan(plan)?;
    {
        let mut guard = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.plan = Some(plan.to_string());
    }
    deactivate()?;
    Ok(json!({
        "content": [{"type": "text", "text": format!("{}\n\n**Saved to:** `{}`\n\n**Next:** Plan Mode is complete; normal tool access has been restored.", markdown_plan(Some(plan), false), path.display())}],
        "details": {"kind": "plan", "plan": plan, "path": path, "active": false, "markdown": true}
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
            let path = plan_path();
            if path.exists() {
                format!(
                    "Plan saved at `{}`. Use `/plan show` to review it.",
                    path.display()
                )
            } else {
                "No saved plan yet. Complete plan mode with `plan_mode_complete` first.".to_string()
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
        register_entrypoint_unified(api, |api| {
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
}
