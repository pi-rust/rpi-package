#![forbid(unsafe_code)]

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId, StablePluginEvent,
    StableToolSchema, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

const MAX_PLAN_CHARS: usize = 50_000;
const PLAN_TOOL: &str = "plan_mode_complete";
const START_TOOL: &str = "plan_mode_start";
const PLAN_ENTRY: &str = "plan-mode";

// Reuse the SDK's host-context lifetime and thread-safety contract.
type Runtime = PluginApi;

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
    let rc = (runtime.runtime_action)(
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
    let Ok(value) = call_runtime(
        RuntimeActionId::GetSessionBranch,
        json!({"customTypes": [PLAN_ENTRY]}),
    ) else {
        return;
    };
    if value.get("filtered").and_then(Value::as_bool) != Some(true) {
        return;
    }
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
        lines.push(format!("- {marker} {}. {}", item.number, item.text));
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
            "Planning",
            "Explore first, then call `plan_mode_complete` when the plan is ready.",
        )
    } else if plan.is_some_and(|plan| !plan.trim().is_empty()) {
        (
            "Ready",
            "Use `/plan start` or `plan_mode_start` to create a new plan.",
        )
    } else {
        ("Idle", "Use `/plan start` to begin planning.")
    };
    let body = match plan {
        Some(plan) if !plan.trim().is_empty() => plan.trim().to_string(),
        _ => "_No completed plan yet._".to_string(),
    };
    format!("**Plan · {status}**\n\n{body}\n\n{hint}")
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
    markdown_plan(guard.plan.as_deref(), guard.active)
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
        "content": [{"type": "text", "text": markdown_plan(Some(plan), false)}],
        "details": {"kind": "plan", "plan": plan, "outline": plan_outline(plan), "active": false, "markdown": true}
    })
    .to_string())
}

fn complete_schema() -> StableToolSchema {
    StableToolSchema {
        name: StbString::from_string(PLAN_TOOL.into()),
        description: StbString::from_string("Submit a decision-ready Markdown design for user review and leave active plan mode, restoring the previous tool set. Only valid while plan mode is active. Include scope, concrete implementation steps/files, important decisions, risks, and validation/acceptance checks. This saves a plan; it does not approve or execute changes, dispatch agents, or create an orchestrator plan DAG. Do not implement during planning; respect any required user approval after submission.".into()),
        parameters: StbString::from_string(json!({
            "type": "object",
            "additionalProperties": true,
            "required": ["plan"],
            "properties": {
                "plan": {"type": "string", "pattern": "\\S", "description": format!("Complete implementation-ready Markdown plan, including validation and acceptance checks. Leading/trailing whitespace is removed; the trimmed plan must contain 1 to {MAX_PLAN_CHARS} Unicode characters.")}
            }
        }).to_string()),
    }
}

fn start_schema() -> StableToolSchema {
    StableToolSchema {
        name: StbString::from_string(START_TOOL.into()),
        description: StbString::from_string("Enter read-only design/review mode for a complex change requiring exploration and an implementation-ready plan, not for routine task tracking (use todo). While active, only read, bash, docs, and plan_mode_complete are enabled: use bash solely for non-mutating inspection, not edits or implementation. Finish with plan_mode_complete. This is a planning/approval workflow, not an orchestrator DAG or execution scheduler; entering it does not itself request or grant approval. Calling again while active keeps the current mode. Supply request here; submit the plan with plan_mode_complete.".into()),
        parameters: StbString::from_string(json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "request": {"type": "string", "description": "Optional planning context, trimmed; omitted, empty, or whitespace-only means no request. Included in the start response, not submitted as a separate user message. No plan field should be supplied to start."}
            }
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
        return StepResult::err(StbString::from_string("null plan handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    if entry.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
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
    HANDLES.cancel(handle);
}

extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

use rpi_plugin_sdk::callbacks::free_string;

fn command_output(out: &mut rpi_plugin_sdk::callbacks::Output<'_>, value: Value) -> i32 {
    if !out.is_available() {
        return 1;
    }
    out.write(value.to_string());
    0
}

rpi_plugin_sdk::string_handler! {
fn plan_command(args_json, out, _user_data) {
    let raw = args_json.to_owned();
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
    let mut output = json!({"kind": "message", "text": result});
    if !result.starts_with("Unable to") {
        let guard = state().lock().unwrap_or_else(|p| p.into_inner());
        output["details"] = json!({"kind":"plan","plan":guard.plan,"active":guard.active,"expanded":first == "show"});
    }
    command_output(out, output)
}
}
rpi_plugin_sdk::export_plugin!(|api| {
    if let Some(register_event) = api.register_event_handler {
        for tag in [EventTag::SessionStart, EventTag::SessionTree] {
            if register_event(tag, handle_event as EventHandlerFn, std::ptr::null_mut()) != 0 {
                return 1;
            }
        }
    }
    let Some(register) = api.register_tool else {
        return 1;
    };
    let _ = RUNTIME.set(*api);
    let schema = Box::new(complete_schema());
    let rc = register(&*schema, execute, poll, cancel, destroy, free_string);
    drop(schema);
    if rc != 0 {
        return rc;
    }
    let start_schema = Box::new(start_schema());
    let start_rc = register(&*start_schema, execute, poll, cancel, destroy, free_string);
    drop(start_schema);
    if start_rc != 0 {
        return start_rc;
    }
    if let Some(register_command) = api.register_command {
        let name = StbStringRef::from_str("plan");
        let description = StbStringRef::from_str("Enter, inspect, or exit CodeX-like plan mode");
        let _ = register_command(name, description, plan_command);
    }
    0
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_schemas_are_json_objects_with_runtime_parameter_rules() {
        for (tool, name) in [(start_schema(), START_TOOL), (complete_schema(), PLAN_TOOL)] {
            // Exercise the constructors used for actual registration.
            assert_eq!(tool.name.to_string_lossy(), name);
            let schema: Value = serde_json::from_str(&tool.parameters.to_string_lossy()).unwrap();
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["additionalProperties"], true);
            let props = schema["properties"].as_object().unwrap();
            assert!(!props.contains_key("__rpi"));
            if name == START_TOOL {
                assert!(schema.get("required").is_none());
                assert_eq!(props["request"]["type"], "string");
                assert!(props["request"].get("minLength").is_none());
                assert!(!props.contains_key("plan"));
            } else {
                assert_eq!(schema["required"], json!(["plan"]));
                assert_eq!(props["plan"]["type"], "string");
                assert_eq!(props["plan"]["pattern"], "\\S");
                // Runtime bounds the trimmed text, not the raw JSON string.
                assert!(props["plan"].get("maxLength").is_none());
                assert!(props["plan"]["description"]
                    .as_str()
                    .unwrap()
                    .contains(&MAX_PLAN_CHARS.to_string()));
            }
            assert!(tool.description.to_string_lossy().contains("DAG"));
        }
    }

    #[test]
    fn completion_rejects_missing_wrong_type_blank_and_trimmed_oversize_plans() {
        for input in [
            json!({}),
            json!({"plan":null}),
            json!({"plan":42}),
            json!({"plan": {"steps": []}}),
            json!({"plan":" \n\t"}),
        ] {
            assert_eq!(
                complete_plan(&input).unwrap_err(),
                "plan must be a non-empty string"
            );
        }
        // Multibyte characters count as characters rather than bytes. Validation
        // precedes the active-mode check and does not access runtime/global state.
        let long = format!("  {}  ", "界".repeat(MAX_PLAN_CHARS + 1));
        assert_eq!(
            complete_plan(&json!({"plan":long,"__rpi":{"trace":[{"id":1}]}})).unwrap_err(),
            format!("plan must not exceed {MAX_PLAN_CHARS} characters")
        );
    }

    #[test]
    fn renders_empty_status() {
        assert!(markdown_plan(None, false).contains("No completed plan"));
        assert!(markdown_plan(None, false).contains("Idle"));
        assert!(!markdown_plan(None, false).contains("Ready"));
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
- ○ 1. Add the parser
- ○ 2. Update the UI"
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
        assert!(items[0].completed);
        assert!(items[1].completed);
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

    #[test]
    fn complete_display_preserves_sections_and_non_numbered_plans() {
        let plan =
            "## Approach\n\n- Inspect source\n- Update UI\n\n## Tests\n\nRun the regression suite";
        let rendered = markdown_plan(Some(plan), false);
        assert!(rendered.contains(plan));
        assert_eq!(rendered.matches("Ready").count(), 1);
        assert!(!rendered.contains("Next:"));
    }

    static BRANCH: Mutex<Value> = Mutex::new(Value::Null);

    rpi_plugin_sdk::string_handler! {
    fn branch_runtime(action, args, out, _user_data) {
        let args: Value = serde_json::from_str(args).unwrap();
        assert_eq!(action, RuntimeActionId::GetSessionBranch as u32);
        assert_eq!(args, json!({"customTypes": ["plan-mode"]}));
        out.write(BRANCH.lock().unwrap().to_string());
        0
    }
    }

    #[test]
    fn filtered_branch_restores_latest_plan_and_rejects_unconfirmed_replies() {
        let _ = RUNTIME.set(PluginApi {
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
        });
        *BRANCH.lock().unwrap() = json!({"filtered":true,"entries":[
            {"type":"custom","customType":"plan-mode","data":{"active":true,"plan":"older"}},
            {"type":"custom","customType":"goal-state","data":{"active":true,"plan":"unrelated"}},
            {"type":"custom","customType":"plan-mode","data":{"active":false,"plan":"latest"}}
        ]});
        restore_state();
        assert_eq!(state().lock().unwrap().plan.as_deref(), Some("latest"));
        assert!(!state().lock().unwrap().active);
        for filtered in [Value::Null, json!(false), json!("true")] {
            *BRANCH.lock().unwrap() = json!({"filtered":filtered,"entries":[
                {"type":"custom","customType":"plan-mode","data":{"active":true,"plan":"wrong"}}
            ]});
            restore_state();
            assert_eq!(state().lock().unwrap().plan.as_deref(), Some("latest"));
            assert!(!state().lock().unwrap().active);
        }
        *BRANCH.lock().unwrap() = json!({"filtered":true,"entries":[
            {"type":"custom","customType":"plan-mode","data":{"active":true,"plan":null}}
        ]});
        restore_state();
        assert!(state().lock().unwrap().active);
        assert!(state().lock().unwrap().plan.is_none());
        state().lock().unwrap().active = false;
    }
}
