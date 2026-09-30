use rpi_plugin_sdk::{
    register_entrypoint, EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId,
    StablePluginEvent, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult,
    ToolPartialCb,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const GOAL_FILE: &str = ".rpi/goals.json";
/// The custom-entry type the host's `CustomEntryContextMessageProjector` will
/// project into the model context (host wiring lands separately in pi-rust).
const GOAL_CUSTOM_TYPE: &str = "goal";
const MAX_TITLE_CHARS: usize = 500;
const MAX_NOTES_CHARS: usize = 20_000;

#[derive(Clone, Copy)]
struct Runtime {
    action: rpi_plugin_sdk::RuntimeActionFn,
    free_string: FreeStringFn,
    user_data: *mut c_void,
}
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
/// Last injected goal-state fingerprint (title + status + notes). Prevents
/// re-injecting the same status on every `TurnEnd` (transcript bloat).
static LAST_INJECTED: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn last_injected() -> &'static Mutex<Option<String>> {
    LAST_INJECTED.get_or_init(|| Mutex::new(None))
}

/// Call a host runtime action (e.g. `AppendEntry`). Safe from any thread,
/// including event handlers.
fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, String> {
    let runtime = *RUNTIME.get().ok_or("goal runtime is unavailable")?;
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

/// Append a **custom entry** (`customType: "goal"`) carrying the current goal
/// state. It does NOT drive a new run, so there is no feedback loop. Once the
/// host registers a `CustomEntryContextMessageProjector` for `"goal"`, this
/// entry is projected into the next turn's model context.
fn append_goal_custom_entry(goal: &Goal) -> Result<(), String> {
    let data = json!({
        "title": goal.title,
        "status": goal.status,
        "notes": goal.notes,
        "startedAt": goal.started_at,
        "updatedAt": goal.updated_at,
    });
    call_runtime(
        RuntimeActionId::AppendEntry,
        json!({"customType": GOAL_CUSTOM_TYPE, "data": data}),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Goal {
    title: String,
    status: String, // active | paused | complete
    notes: String,
    started_at: i64,
    updated_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Goals {
    active: Option<Goal>,
    archived: Vec<Goal>,
}

fn goals_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(GOAL_FILE)
}

fn read_goals() -> Goals {
    let path = goals_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return Goals::default();
    };
    serde_json::from_str(&text).unwrap_or_else(|_| Goals::default())
}

fn write_goals(goals: &Goals) -> Result<(), String> {
    let path = goals_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create goal directory: {e}"))?;
    }
    let tmp = path.with_file_name(format!("goals.json.tmp-{}", std::process::id()));
    let data = serde_json::to_string_pretty(goals).map_err(|e| format!("serialize goals: {e}"))?;
    fs::write(&tmp, data).map_err(|e| format!("write goals: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("commit goals: {e}"))?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Goal-state fingerprint used to dedupe injections.
fn fingerprint(goal: &Goal) -> String {
    format!("{}|{}|{}", goal.title, goal.status, goal.notes)
}

/// Mark the current active-goal status as already injected.
fn remember_injected(fingerprint: Option<String>) {
    let mut guard = last_injected().lock().unwrap_or_else(|p| p.into_inner());
    *guard = fingerprint;
}

/// Should we inject now? False when the same goal-state was already appended.
fn should_inject(fingerprint: Option<&str>) -> bool {
    let guard = last_injected().lock().unwrap_or_else(|p| p.into_inner());
    guard.as_deref() != fingerprint
}

// ---------------------------------------------------------------------------
// Tool actions
// ---------------------------------------------------------------------------

fn start_goal(params: &Value) -> Result<String, String> {
    let title = params
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or("goal title is required")?;
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "title must not exceed {MAX_TITLE_CHARS} characters"
        ));
    }
    let notes = params
        .get("notes")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if notes.chars().count() > MAX_NOTES_CHARS {
        return Err(format!(
            "notes must not exceed {MAX_NOTES_CHARS} characters"
        ));
    }
    let now = now_ms();
    let goal = Goal {
        title: title.to_string(),
        status: "active".to_string(),
        notes,
        started_at: now,
        updated_at: now,
    };
    let mut goals = read_goals();
    if let Some(old) = goals.active.take() {
        goals.archived.push(old);
    }
    goals.active = Some(goal.clone());
    write_goals(&goals)?;
    // Inject immediately so the goal is visible even before the next TurnEnd.
    let fp = fingerprint(&goal);
    if should_inject(Some(&fp)) {
        let _ = append_goal_custom_entry(&goal);
        remember_injected(Some(fp));
    }
    Ok(json!({
        "content": [{"type": "text", "text": format!(
            "## 🎯 Goal started\n\n**{}**\n\n{}\n\n_Injected into the next turn's context; the agent will keep advancing it._",
            goal.title,
            if goal.notes.trim().is_empty() { "_no notes_".to_string() } else { goal.notes.clone() }
        )}],
        "details": {"kind": "goal", "action": "start", "active": true, "markdown": true, "goal": {"title": goal.title, "status": "active"}}
    }).to_string())
}

fn require_active<'a>(goals: &'a Goals) -> Result<&'a Goal, String> {
    goals
        .active
        .as_ref()
        .ok_or_else(|| "no active goal — start one first".to_string())
}

fn update_goal(params: &Value) -> Result<String, String> {
    let mut goals = read_goals();
    let goal = require_active(&goals)?;
    let mut updated = goal.clone();
    if let Some(notes) = params.get("notes").and_then(Value::as_str) {
        let notes = notes.trim();
        if notes.chars().count() > MAX_NOTES_CHARS {
            return Err(format!(
                "notes must not exceed {MAX_NOTES_CHARS} characters"
            ));
        }
        updated.notes = notes.to_string();
    }
    if let Some(status) = params.get("status").and_then(Value::as_str) {
        if !["active", "paused"].contains(&status) {
            return Err("status must be \"active\" or \"paused\"".into());
        }
        updated.status = status.to_string();
    }
    updated.updated_at = now_ms();
    goals.active = Some(updated.clone());
    write_goals(&goals)?;
    // Always append a fresh entry so the host projector sees the new status.
    let _ = append_goal_custom_entry(&updated);
    remember_injected(Some(fingerprint(&updated)));
    Ok(json!({
        "content": [{"type": "text", "text": format!(
            "## 🎯 Goal updated\n\n**{}**  ({})\n\n{}",
            updated.title,
            updated.status,
            if updated.notes.trim().is_empty() { "_no notes_".to_string() } else { updated.notes.clone() }
        )}],
        "details": {"kind": "goal", "action": "update", "active": true, "markdown": true}
    }).to_string())
}

fn set_status(status: &str) -> Result<String, String> {
    let mut goals = read_goals();
    let goal = require_active(&goals)?;
    let mut updated = goal.clone();
    updated.status = status.to_string();
    updated.updated_at = now_ms();
    goals.active = Some(updated.clone());
    write_goals(&goals)?;
    // Always append a fresh entry so the host projector sees the new status
    // (it projects only the latest `goal` entry).
    let _ = append_goal_custom_entry(&updated);
    remember_injected(Some(fingerprint(&updated)));
    Ok(json!({
        "content": [{"type": "text", "text": format!(
            "## 🎯 Goal {}\n\n**{}**",
            if status == "paused" { "paused" } else { "resumed" },
            updated.title
        )}],
        "details": {"kind": "goal", "action": if status == "paused" { "pause" } else { "resume" }, "active": true, "markdown": true}
    }).to_string())
}

fn complete_goal() -> Result<String, String> {
    let mut goals = read_goals();
    let goal = require_active(&goals)?;
    let mut done = goal.clone();
    done.status = "complete".to_string();
    done.updated_at = now_ms();
    goals.archived.push(done.clone());
    goals.active = None;
    write_goals(&goals)?;
    // Append a `complete` entry so the host projector stops injecting.
    let _ = append_goal_custom_entry(&done);
    remember_injected(Some(fingerprint(&done)));
    Ok(json!({
        "content": [{"type": "text", "text": format!(
            "## ✅ Goal complete\n\n**{}**  \n\nArchived. The goal is no longer injected into context.",
            done.title
        )}],
        "details": {"kind": "goal", "action": "complete", "active": false, "markdown": true}
    }).to_string())
}

fn show_goal() -> Result<String, String> {
    let goals = read_goals();
    match &goals.active {
        Some(goal) => Ok(json!({
            "content": [{"type": "text", "text": format!(
                "## 🎯 Current goal\n\n**{}**  ({})\n\n{}",
                goal.title,
                goal.status,
                if goal.notes.trim().is_empty() { "_no notes_".to_string() } else { goal.notes.clone() }
            )}],
            "details": {"kind": "goal", "action": "show", "active": true, "markdown": true}
        }).to_string()),
        None => Ok(json!({
            "content": [{"type": "text", "text": "## 🎯 Goal\n\n_No active goal._ Start one with `goal start`.".to_string()}],
            "details": {"kind": "goal", "action": "show", "active": false, "markdown": true}
        }).to_string()),
    }
}

fn list_goals() -> Result<String, String> {
    let goals = read_goals();
    let mut lines = vec!["## 🎯 Goals".to_string()];
    match &goals.active {
        Some(goal) => lines.push(format!(
            "- **{}** ({}): {}",
            goal.title,
            goal.status,
            if goal.notes.trim().is_empty() {
                "_no notes_".to_string()
            } else {
                goal.notes.clone()
            }
        )),
        None => lines.push("_No active goal._".to_string()),
    }
    if !goals.archived.is_empty() {
        lines.push("".to_string());
        lines.push("### Archived".to_string());
        for goal in &goals.archived {
            lines.push(format!("- ~~{}~~", goal.title));
        }
    }
    Ok(json!({
        "content": [{"type": "text", "text": lines.join("\n")}],
        "details": {"kind": "goal", "action": "list", "active": goals.active.is_some(), "markdown": true}
    }).to_string())
}

fn goal(params: &Value) -> Result<String, String> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("show")
        .to_lowercase();
    match action.as_str() {
        "start" => start_goal(params),
        "update" => update_goal(params),
        "pause" => set_status("paused"),
        "resume" => set_status("active"),
        "complete" => complete_goal(),
        "show" | "status" => show_goal(),
        "list" => list_goals(),
        _ => Err(format!("unknown goal action: {action}")),
    }
}

// ---------------------------------------------------------------------------
// Tool drive (ABI)
// ---------------------------------------------------------------------------

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
        return StepResult::err(StbString::from_string("null goal handle".into()));
    }
    let drive = unsafe { &mut *(handle as *mut Drive) };
    if drive.cancelled {
        drive.done = true;
        return StepResult::err(StbString::from_string("goal cancelled".into()));
    }
    if drive.done {
        return StepResult::err(StbString::from_string(
            "goal polled after completion".into(),
        ));
    }
    drive.done = true;
    match goal(&drive.params) {
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

// ---------------------------------------------------------------------------
// TurnEnd handler — append the active goal as a custom entry
// ---------------------------------------------------------------------------

extern "C" fn on_turn_end(event: StablePluginEvent, _: *mut c_void) -> i32 {
    // TurnEnd carries no payload (empty event) in this ABI; ignore the struct.
    let _ = event;
    let Some(goal) = read_goals().active else {
        // No active goal — nothing to inject.
        remember_injected(None);
        return 0;
    };
    if goal.status == "paused" {
        remember_injected(Some(fingerprint(&goal)));
        return 0;
    }
    let fp = fingerprint(&goal);
    if !should_inject(Some(&fp)) {
        return 0;
    }
    match append_goal_custom_entry(&goal) {
        Ok(_) => {
            remember_injected(Some(fp));
        }
        Err(_) => {
            // Non-fatal: keep last-injected unset so a later TurnEnd retries.
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

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
                let rc = register_event(
                    EventTag::TurnEnd,
                    on_turn_end as EventHandlerFn,
                    std::ptr::null_mut(),
                );
                if rc != 0 {
                    return rc;
                }
            }
            let Some(register) = api.register_tool else {
                return 1;
            };
            let schema = Box::new(StableToolSchema {
                name: StbString::from_string("goal".into()),
                description: StbString::from_string(
                    "Codex-style goal mode: record one persistent, context-injected objective per project and let the agent advance it across turns. Actions: start, update, pause, resume, complete, show, list.".into(),
                ),
                parameters: StbString::from_string(
                    r#"{"type":"object","properties":{"action":{"type":"string","enum":["start","update","pause","resume","complete","show","list"]},"title":{"type":"string","description":"Goal title (required for start)"},"notes":{"type":"string","description":"Goal notes or progress update (start/update)"},"status":{"type":"string","enum":["active","paused"],"description":"Explicit status change (update)"}},"required":["action"]}"#.into(),
                ),
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
    fn rejects_empty_title() {
        assert!(start_goal(&json!({"action": "start"})).is_err());
        assert!(start_goal(&json!({"action": "start", "title": "  "})).is_err());
    }

    #[test]
    fn rejects_unknown_action() {
        assert!(goal(&json!({"action": "bogus"})).is_err());
    }

    #[test]
    fn fingerprint_changes_with_state() {
        let a = Goal {
            title: "t".into(),
            status: "active".into(),
            notes: "n".into(),
            started_at: 1,
            updated_at: 1,
        };
        let mut b = a.clone();
        b.notes = "n2".into();
        assert_ne!(fingerprint(&a), fingerprint(&b));
        let mut c = a.clone();
        c.status = "paused".into();
        assert_ne!(fingerprint(&a), fingerprint(&c));
        assert_eq!(fingerprint(&a), fingerprint(&a));
    }
}
