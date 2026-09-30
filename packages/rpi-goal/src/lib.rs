use rpi_plugin_sdk::{
    register_entrypoint, CommandHandlerFn, EventHandlerFn, EventTag, FreeStringFn, PluginApi,
    RuntimeActionId, StablePluginEvent, StableToolSchema, StbString, StbStringRef, StepHandle,
    StepResult, ToolPartialCb,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const GOAL_FILE: &str = ".rpi/goals.json";
/// The custom-message type stored in the generic AgentMessage payload.
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
/// including event handlers and command handlers.
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
    /// Turns completed while the goal was active. Drives the `max_turns` budget.
    #[serde(default)]
    turns_used: u32,
    /// Turn budget. `None` = unlimited.
    #[serde(default)]
    max_turns: Option<u32>,
    /// Absolute epoch-ms deadline. `None` = unlimited.
    #[serde(default)]
    deadline_ms: Option<i64>,
}

impl Goal {
    /// Human-readable budget line, e.g. `turns 3/20 · time 12m left`.
    fn budget_summary(&self, now: i64) -> String {
        let mut parts = Vec::new();
        match self.max_turns {
            Some(max) => parts.push(format!("turns {}/{}", self.turns_used.min(max), max)),
            None => parts.push(format!("turns {} (unlimited)", self.turns_used)),
        }
        match self.deadline_ms {
            Some(deadline) => {
                let remaining = deadline - now;
                if remaining <= 0 {
                    parts.push("time is up".to_string());
                } else {
                    parts.push(format!("{} left", format_duration_ms(remaining)));
                }
            }
            None => parts.push("time unlimited".to_string()),
        }
        parts.join(" · ")
    }

    /// Static budget hint injected into the model context (no live counters, so
    /// the message stays fingerprint-stable across turns).
    fn budget_hint(&self) -> String {
        match (self.max_turns, self.deadline_ms) {
            (None, None) => String::new(),
            (max, deadline) => {
                let mut parts = Vec::new();
                if let Some(max) = max {
                    parts.push(format!("{max} turns"));
                }
                if let Some(deadline) = deadline {
                    let remaining = deadline - self.started_at;
                    parts.push(format_duration_ms(remaining.max(0)));
                }
                format!(" (budget: {})", parts.join(" / "))
            }
        }
    }
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

/// Goal-state fingerprint used to dedupe injections. Deliberately excludes the
/// live counters (`turns_used`) so a running budget does not re-inject each turn.
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
// Budget parsing / formatting
// ---------------------------------------------------------------------------

/// Parse a duration spec into milliseconds. Accepts bare numbers (minutes) and
/// unit suffixes `s`/`m`/`h`/`d`, combinable (`90s`, `30m`, `2h`, `1h30m`).
fn parse_duration_ms(spec: &str) -> Result<i64, String> {
    let spec = spec.trim().to_lowercase();
    if spec.is_empty() {
        return Err("empty duration".into());
    }
    if let Ok(minutes) = spec.parse::<i64>() {
        return Ok(minutes.saturating_mul(60_000));
    }
    let mut total: i64 = 0;
    let mut number = String::new();
    let mut saw_unit = false;
    for ch in spec.chars() {
        if ch.is_ascii_digit() {
            number.push(ch);
            continue;
        }
        let unit = match ch {
            's' => 1_000,
            'm' => 60_000,
            'h' => 3_600_000,
            'd' => 86_400_000,
            other => return Err(format!("unknown duration unit `{other}` (use s/m/h/d)")),
        };
        let value: i64 = number
            .parse()
            .map_err(|_| format!("invalid duration `{spec}`"))?;
        total = total.saturating_add(value.saturating_mul(unit));
        number.clear();
        saw_unit = true;
    }
    if !number.is_empty() {
        return Err(format!("duration `{spec}` is missing a unit (s/m/h/d)"));
    }
    if !saw_unit {
        return Err(format!("invalid duration `{spec}`"));
    }
    Ok(total)
}

/// Compact duration rendering: `2h5m`, `12m30s`, `45s`.
fn format_duration_ms(ms: i64) -> String {
    let secs = (ms.max(0) / 1000) as u64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

// ---------------------------------------------------------------------------
// Context injection
// ---------------------------------------------------------------------------

fn append_custom_message(goal: &Goal, text: String) -> Result<(), String> {
    let message = json!({
        "kind": "custom",
        "role": "custom",
        "content": [{"type": "text", "text": text}],
        "data": {
            "customType": GOAL_CUSTOM_TYPE,
            "display": true,
            "details": {
                "title": goal.title,
                "status": goal.status,
                "notes": goal.notes,
                "turnsUsed": goal.turns_used,
                "maxTurns": goal.max_turns,
                "deadlineMs": goal.deadline_ms,
                "startedAt": goal.started_at,
                "updatedAt": goal.updated_at,
            }
        },
        "timestamp": goal.updated_at,
    });
    call_runtime(RuntimeActionId::AppendEntry, json!({"message": message}))?;
    Ok(())
}

/// Append the goal's current state as a generic custom message. The host stores
/// it as an ordinary message entry and the generic context builder forwards it
/// to the model; no goal-specific host projector is required.
fn append_goal_custom_message(goal: &Goal) -> Result<(), String> {
    let tag = match goal.status.as_str() {
        "paused" => "⏸️ Paused goal",
        "complete" => "✅ Goal completed",
        _ => "🎯 Active goal",
    };
    let notes = goal.notes.trim();
    let hint = goal.budget_hint();
    let text = if notes.is_empty() {
        format!("{tag}: {}.{hint}", goal.title)
    } else {
        format!("{tag}: {} — {}.{hint}", goal.title, notes)
    };
    append_custom_message(goal, text)
}

/// Announce that a budget was exhausted and the goal auto-paused.
fn append_budget_message(goal: &Goal, reason: &str) -> Result<(), String> {
    let text = format!(
        "⏸️ Goal paused — {reason}: {}. Resume with `goal resume` or `/goal resume`.",
        goal.title
    );
    append_custom_message(goal, text)
}

// ---------------------------------------------------------------------------
// Core operations (shared by the `goal` tool and the `/goal` command)
// ---------------------------------------------------------------------------

fn require_active(goals: &Goals) -> Result<Goal, String> {
    goals
        .active
        .clone()
        .ok_or_else(|| "no active goal — start one first".to_string())
}

fn validate_title(title: &str) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("goal title is required".into());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "title must not exceed {MAX_TITLE_CHARS} characters"
        ));
    }
    Ok(title.to_string())
}

fn validate_notes(notes: &str) -> Result<String, String> {
    let notes = notes.trim();
    if notes.chars().count() > MAX_NOTES_CHARS {
        return Err(format!(
            "notes must not exceed {MAX_NOTES_CHARS} characters"
        ));
    }
    Ok(notes.to_string())
}

/// Persist `goal` as the active goal and inject it into the model context.
fn persist_and_inject(goals: &Goals, goal: &Goal) -> Result<(), String> {
    let mut goals = goals.clone();
    goals.active = Some(goal.clone());
    write_goals(&goals)?;
    let _ = append_goal_custom_message(goal);
    remember_injected(Some(fingerprint(goal)));
    Ok(())
}

fn op_start(
    title: &str,
    notes: &str,
    max_turns: Option<u32>,
    deadline_ms: Option<i64>,
) -> Result<Goal, String> {
    let title = validate_title(title)?;
    let notes = validate_notes(notes)?;
    let now = now_ms();
    let goal = Goal {
        title,
        status: "active".into(),
        notes,
        started_at: now,
        updated_at: now,
        turns_used: 0,
        max_turns,
        deadline_ms,
    };
    let mut goals = read_goals();
    if let Some(old) = goals.active.take() {
        goals.archived.push(old);
    }
    persist_and_inject(&goals, &goal)?;
    Ok(goal)
}

fn op_update(notes: Option<&str>, status: Option<&str>) -> Result<Goal, String> {
    let goals = read_goals();
    let mut updated = require_active(&goals)?;
    if let Some(notes) = notes {
        updated.notes = validate_notes(notes)?;
    }
    if let Some(status) = status {
        if !["active", "paused"].contains(&status) {
            return Err("status must be \"active\" or \"paused\"".into());
        }
        updated.status = status.to_string();
    }
    updated.updated_at = now_ms();
    persist_and_inject(&goals, &updated)?;
    Ok(updated)
}

fn op_set_status(status: &str) -> Result<Goal, String> {
    op_update(None, Some(status))
}

/// Merge budget changes into `goal`. `None` leaves a field untouched;
/// `Some(None)` clears it; `Some(Some(v))` sets it (a relative ms value for
/// the deadline, resolved against `now`).
fn apply_limits(
    goal: &Goal,
    now: i64,
    max_turns: Option<Option<u32>>,
    deadline_ms: Option<Option<i64>>,
) -> Goal {
    let mut updated = goal.clone();
    if let Some(max_turns) = max_turns {
        updated.max_turns = max_turns.filter(|n| *n > 0);
    }
    if let Some(deadline_ms) = deadline_ms {
        updated.deadline_ms = match deadline_ms {
            Some(ms) if ms > 0 => Some(now.saturating_add(ms)),
            _ => None,
        };
    }
    updated.updated_at = now;
    updated
}

fn op_set_limits(
    max_turns: Option<Option<u32>>,
    deadline_ms: Option<Option<i64>>,
) -> Result<Goal, String> {
    let goals = read_goals();
    let active = require_active(&goals)?;
    let updated = apply_limits(&active, now_ms(), max_turns, deadline_ms);
    persist_and_inject(&goals, &updated)?;
    Ok(updated)
}

fn op_complete() -> Result<Goal, String> {
    let mut goals = read_goals();
    let mut done = require_active(&goals)?;
    done.status = "complete".into();
    done.updated_at = now_ms();
    goals.archived.push(done.clone());
    goals.active = None;
    write_goals(&goals)?;
    let _ = append_goal_custom_message(&done);
    remember_injected(Some(fingerprint(&done)));
    Ok(done)
}

// ---------------------------------------------------------------------------
// Rendering (tool = markdown, command = plain text)
// ---------------------------------------------------------------------------

fn render_status_text(goal: Option<&Goal>) -> String {
    let now = now_ms();
    match goal {
        Some(goal) => {
            let notes = if goal.notes.trim().is_empty() {
                "(no notes)".to_string()
            } else {
                goal.notes.clone()
            };
            format!(
                "🎯 {}\nstatus: {}\nnotes: {}\n{}",
                goal.title,
                goal.status,
                notes,
                goal.budget_summary(now)
            )
        }
        None => "🎯 No active goal. Start one with `/goal start <title>`.".to_string(),
    }
}

fn render_list_text(goals: &Goals) -> String {
    let now = now_ms();
    let mut lines = vec!["🎯 Goals".to_string()];
    match &goals.active {
        Some(goal) => lines.push(format!(
            "• {} ({}) — {}",
            goal.title,
            goal.status,
            goal.budget_summary(now)
        )),
        None => lines.push("• (no active goal)".to_string()),
    }
    if !goals.archived.is_empty() {
        lines.push(format!("archived: {}", goals.archived.len()));
        for goal in &goals.archived {
            lines.push(format!("  – {}", goal.title));
        }
    }
    lines.join("\n")
}

fn goal_summary_json(goal: &Goal) -> Value {
    json!({
        "title": goal.title,
        "status": goal.status,
        "notes": goal.notes,
        "turnsUsed": goal.turns_used,
        "maxTurns": goal.max_turns,
        "deadlineMs": goal.deadline_ms,
        "startedAt": goal.started_at,
        "updatedAt": goal.updated_at,
    })
}

fn tool_envelope(action: &str, text: String, active: bool, goal: Option<&Goal>) -> String {
    json!({
        "content": [{"type": "text", "text": text}],
        "details": {
            "kind": "goal",
            "action": action,
            "active": active,
            "markdown": true,
            "goal": goal.map(goal_summary_json),
        }
    })
    .to_string()
}

fn show_goal() -> Result<String, String> {
    let goals = read_goals();
    let text = match &goals.active {
        Some(goal) => format!(
            "## 🎯 Current goal\n\n**{}**  ({})\n\n{}\n\n{}",
            goal.title,
            goal.status,
            if goal.notes.trim().is_empty() {
                "_no notes_".to_string()
            } else {
                goal.notes.clone()
            },
            goal.budget_summary(now_ms())
        ),
        None => "## 🎯 Goal\n\n_No active goal._ Start one with `goal start`.".to_string(),
    };
    Ok(tool_envelope(
        "show",
        text,
        goals.active.is_some(),
        goals.active.as_ref(),
    ))
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
    Ok(tool_envelope(
        "list",
        lines.join("\n"),
        goals.active.is_some(),
        goals.active.as_ref(),
    ))
}

// ---------------------------------------------------------------------------
// Tool actions
// ---------------------------------------------------------------------------

fn param_u32(params: &Value, key: &str) -> Option<u32> {
    params.get(key).and_then(Value::as_u64).map(|v| v as u32)
}

fn start_goal(params: &Value) -> Result<String, String> {
    let title = params
        .get("title")
        .and_then(Value::as_str)
        .ok_or("goal title is required")?;
    let notes = params.get("notes").and_then(Value::as_str).unwrap_or("");
    let max_turns = param_u32(params, "maxTurns").filter(|n| *n > 0);
    let deadline_ms = param_u32(params, "maxMinutes")
        .filter(|n| *n > 0)
        .map(|minutes| now_ms().saturating_add((minutes as i64) * 60_000));
    let goal = op_start(title, notes, max_turns, deadline_ms)?;
    let text = format!(
        "## 🎯 Goal started\n\n**{}**\n\n{}\n\n_{}_",
        goal.title,
        if goal.notes.trim().is_empty() {
            "_no notes_".to_string()
        } else {
            goal.notes.clone()
        },
        goal.budget_summary(now_ms())
    );
    Ok(tool_envelope("start", text, true, Some(&goal)))
}

fn update_goal(params: &Value) -> Result<String, String> {
    let notes = params.get("notes").and_then(Value::as_str);
    let status = params.get("status").and_then(Value::as_str);
    if notes.is_none() && status.is_none() {
        return Err("update requires `notes` and/or `status`".into());
    }
    let goal = op_update(notes, status)?;
    let text = format!(
        "## 🎯 Goal updated\n\n**{}**  ({})\n\n{}\n\n{}",
        goal.title,
        goal.status,
        if goal.notes.trim().is_empty() {
            "_no notes_".to_string()
        } else {
            goal.notes.clone()
        },
        goal.budget_summary(now_ms())
    );
    Ok(tool_envelope("update", text, true, Some(&goal)))
}

fn status_goal(status: &str) -> Result<String, String> {
    let goal = op_set_status(status)?;
    let verb = if status == "paused" {
        "paused"
    } else {
        "resumed"
    };
    let text = format!("## 🎯 Goal {verb}\n\n**{}**", goal.title);
    Ok(tool_envelope(
        if status == "paused" {
            "pause"
        } else {
            "resume"
        },
        text,
        true,
        Some(&goal),
    ))
}

fn complete_goal() -> Result<String, String> {
    let goal = op_complete()?;
    let text = format!(
        "## ✅ Goal complete\n\n**{}**  \n\nArchived. The goal is no longer injected into context.",
        goal.title
    );
    Ok(tool_envelope("complete", text, false, Some(&goal)))
}

fn limit_goal(params: &Value) -> Result<String, String> {
    let max_turns = params
        .get("maxTurns")
        .and_then(Value::as_u64)
        .map(|v| (v as u32).max(1));
    let max_minutes = params
        .get("maxMinutes")
        .and_then(Value::as_u64)
        .map(|v| (v as i64) * 60_000);
    if max_turns.is_none() && max_minutes.is_none() {
        return Err("limit requires `maxTurns` and/or `maxMinutes`".into());
    }
    // `Some(None)` clears a budget, `None` leaves it untouched.
    let goal = op_set_limits(max_turns.map(Some), max_minutes.map(Some))?;
    let text = format!(
        "## 🎯 Goal budget updated\n\n**{}**\n\n{}",
        goal.title,
        goal.budget_summary(now_ms())
    );
    Ok(tool_envelope("limit", text, true, Some(&goal)))
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
        "pause" => status_goal("paused"),
        "resume" => status_goal("active"),
        "complete" => complete_goal(),
        "limit" => limit_goal(params),
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
// TurnEnd handler — count turns, enforce budgets, inject the active goal
// ---------------------------------------------------------------------------

extern "C" fn on_turn_end(event: StablePluginEvent, _: *mut c_void) -> i32 {
    // TurnEnd carries no payload (empty event) in this ABI; ignore the struct.
    let _ = event;
    let mut goals = read_goals();
    let Some(mut goal) = goals.active.clone() else {
        // No active goal — nothing to inject.
        remember_injected(None);
        return 0;
    };
    if goal.status != "active" {
        remember_injected(Some(fingerprint(&goal)));
        return 0;
    }

    // Count this turn against the budget.
    goal.turns_used = goal.turns_used.saturating_add(1);
    goal.updated_at = now_ms();

    let exhausted = match goal.max_turns {
        Some(max) if goal.turns_used >= max => Some(format!("turn budget reached ({max})")),
        _ => match goal.deadline_ms {
            Some(deadline) if goal.updated_at >= deadline => Some("time budget reached".into()),
            _ => None,
        },
    };

    if let Some(reason) = exhausted {
        goal.status = "paused".into();
        goals.active = Some(goal.clone());
        let _ = write_goals(&goals);
        let _ = append_budget_message(&goal, &reason);
        remember_injected(Some(fingerprint(&goal)));
        return 0;
    }

    goals.active = Some(goal.clone());
    let _ = write_goals(&goals);

    let fp = fingerprint(&goal);
    if !should_inject(Some(&fp)) {
        return 0;
    }
    match append_goal_custom_message(&goal) {
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
// `/goal` slash command
// ---------------------------------------------------------------------------

fn command_output(out: *mut StbString, value: Value) -> i32 {
    if out.is_null() {
        return 1;
    }
    unsafe { *out = StbString::from_string(value.to_string()) };
    0
}

/// Extract `title` / `--notes` from a free-form string.
fn split_title_notes(rest: &str) -> (String, Option<String>) {
    let mut title_parts: Vec<String> = Vec::new();
    let mut notes: Option<String> = None;
    let mut tokens = rest.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        if token == "--notes" || token == "-n" {
            let mut collected = Vec::new();
            while let Some(next) = tokens.peek() {
                if next.starts_with("--") {
                    break;
                }
                collected.push(tokens.next().unwrap().to_string());
            }
            notes = Some(collected.join(" "));
            continue;
        }
        title_parts.push(token.to_string());
    }
    (title_parts.join(" "), notes)
}

const COMMAND_HELP: &str = "\
/goal                     show status
/goal start <title>       start a goal (--notes \"...\" optional)
/goal <title...>          shorthand for start
/goal pause | resume      pause / resume the active goal
/goal stop                complete and archive the goal
/goal turns <N>           max turns (0 clears)
/goal time <dur>          max duration, e.g. 30m / 2h / 90s / 1h30m (0 clears)
/goal limit <N> [<dur>]   set both budgets
/goal clear               clear both budgets
/goal list                list active + archived goals
/goal help                this help";

fn run_command(args: &str) -> String {
    let args = args.trim();
    let (head, rest) = match args.split_once(char::is_whitespace) {
        Some((head, rest)) => (head.to_lowercase(), rest.trim()),
        None => (args.to_lowercase(), ""),
    };

    let result: Result<String, String> = match head.as_str() {
        "" | "status" | "show" => {
            let goals = read_goals();
            Ok(render_status_text(goals.active.as_ref()))
        }
        "list" => Ok(render_list_text(&read_goals())),
        "help" => Ok(COMMAND_HELP.to_string()),
        "start" => {
            let (title, notes) = split_title_notes(rest);
            op_start(&title, notes.as_deref().unwrap_or(""), None, None).map(|goal| {
                format!(
                    "🎯 Goal started: {}\n{}",
                    goal.title,
                    goal.budget_summary(now_ms())
                )
            })
        }
        "pause" => op_set_status("paused").map(|goal| format!("⏸️ Paused: {}", goal.title)),
        "resume" => op_set_status("active").map(|goal| format!("▶️ Resumed: {}", goal.title)),
        "stop" | "done" | "complete" => {
            op_complete().map(|goal| format!("✅ Completed: {}", goal.title))
        }
        "turns" => match rest.parse::<u32>() {
            Ok(0) => op_set_limits(Some(None), None)
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(now_ms()))),
            Ok(n) => op_set_limits(Some(Some(n)), None)
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(now_ms()))),
            Err(_) => Err(format!("`{rest}` is not a turn count")),
        },
        "time" => match parse_duration_ms(rest) {
            Ok(0) => op_set_limits(None, Some(None))
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(now_ms()))),
            Ok(ms) => op_set_limits(None, Some(Some(ms)))
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(now_ms()))),
            Err(error) => Err(error),
        },
        "limit" => {
            let mut parts = rest.split_whitespace();
            let turns = parts.next();
            let time = parts.next();
            if turns.is_none() && time.is_none() {
                return "Usage: /goal limit <turns> [duration]".to_string();
            }
            let max_turns = match turns {
                Some(token) => match token.parse::<u32>() {
                    Ok(0) => Some(None),
                    Ok(n) => Some(Some(n)),
                    Err(_) => return format!("`{token}` is not a turn count"),
                },
                None => None,
            };
            let deadline = match time {
                Some(token) => match parse_duration_ms(token) {
                    Ok(0) => Some(None),
                    Ok(ms) => Some(Some(ms)),
                    Err(error) => return error,
                },
                None => None,
            };
            op_set_limits(max_turns, deadline)
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(now_ms())))
        }
        "clear" => op_set_limits(Some(None), Some(None))
            .map(|goal| format!("Budget cleared: {}", goal.budget_summary(now_ms()))),
        // Unknown verb → treat the whole input as a title (shorthand start).
        _ => {
            let (title, notes) = split_title_notes(args);
            op_start(&title, notes.as_deref().unwrap_or(""), None, None)
                .map(|goal| format!("🎯 Goal started: {}", goal.title))
        }
    };

    match result {
        Ok(text) => text,
        Err(error) => format!("⚠️ {error}"),
    }
}

extern "C" fn goal_command(args_json: StbStringRef, out: *mut StbString, _: *mut c_void) -> i32 {
    let raw = unsafe { args_json.as_str().to_owned() };
    let envelope: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let args = envelope
        .get("args")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let text = run_command(args);
    command_output(out, json!({"kind": "message", "text": text}))
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
            if let Some(register_command) = api.register_command {
                let name = StbStringRef::from_str("goal");
                let description =
                    StbStringRef::from_str("Show, start, pause, stop, or budget the active goal");
                let _ = register_command(name, description, goal_command as CommandHandlerFn);
            }
            let Some(register) = api.register_tool else {
                return 1;
            };
            let schema = Box::new(StableToolSchema {
                name: StbString::from_string("goal".into()),
                description: StbString::from_string(
                    "Codex-style goal mode: record one persistent, context-injected objective per project and let the agent advance it across turns. Actions: start, update, pause, resume, complete, limit, show, list.".into(),
                ),
                parameters: StbString::from_string(
                    r#"{"type":"object","properties":{"action":{"type":"string","enum":["start","update","pause","resume","complete","limit","show","list"]},"title":{"type":"string","description":"Goal title (required for start)"},"notes":{"type":"string","description":"Goal notes or progress update (start/update)"},"status":{"type":"string","enum":["active","paused"],"description":"Explicit status change (update)"},"maxTurns":{"type":"integer","minimum":1,"description":"Optional turn budget (start/limit)"},"maxMinutes":{"type":"integer","minimum":1,"description":"Optional wall-clock budget in minutes (start/limit)"}},"required":["action"]}"#.into(),
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

    fn goal_with(status: &str) -> Goal {
        Goal {
            title: "t".into(),
            status: status.into(),
            notes: "n".into(),
            started_at: 1,
            updated_at: 1,
            turns_used: 0,
            max_turns: None,
            deadline_ms: None,
        }
    }

    #[test]
    fn rejects_empty_title() {
        assert!(validate_title("  ").is_err());
        assert!(validate_title("ok").is_ok());
    }

    #[test]
    fn rejects_unknown_action() {
        assert!(goal(&json!({"action": "bogus"})).is_err());
    }

    #[test]
    fn fingerprint_changes_with_state() {
        let a = goal_with("active");
        let mut b = a.clone();
        b.notes = "n2".into();
        assert_ne!(fingerprint(&a), fingerprint(&b));
        let mut c = a.clone();
        c.status = "paused".into();
        assert_ne!(fingerprint(&a), fingerprint(&c));
        assert_eq!(fingerprint(&a), fingerprint(&a));
    }

    #[test]
    fn fingerprint_ignores_turn_counter() {
        let a = goal_with("active");
        let mut b = a.clone();
        b.turns_used = 7;
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration_ms("30m").unwrap(), 1_800_000);
        assert_eq!(parse_duration_ms("2h").unwrap(), 7_200_000);
        assert_eq!(parse_duration_ms("90s").unwrap(), 90_000);
        assert_eq!(parse_duration_ms("1h30m").unwrap(), 5_400_000);
        assert_eq!(parse_duration_ms("5").unwrap(), 300_000); // bare = minutes
        assert_eq!(parse_duration_ms("0").unwrap(), 0);
        assert!(parse_duration_ms("10x").is_err());
        assert!(parse_duration_ms("").is_err());
        assert!(parse_duration_ms("h").is_err());
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration_ms(90_000), "1m30s");
        assert_eq!(format_duration_ms(45_000), "45s");
        assert_eq!(format_duration_ms(7_500_000), "2h5m");
        assert_eq!(format_duration_ms(-1), "0s");
    }

    #[test]
    fn budget_summary_reports_limits_and_remaining() {
        let mut goal = goal_with("active");
        goal.turns_used = 3;
        goal.max_turns = Some(20);
        goal.deadline_ms = Some(1_000_000);
        let summary = goal.budget_summary(940_000);
        assert!(summary.contains("turns 3/20"), "{summary}");
        assert!(summary.contains("1m0s left"), "{summary}");

        let unlimited = goal_with("active");
        let summary = unlimited.budget_summary(0);
        assert!(summary.contains("unlimited"));
    }

    #[test]
    fn budget_hint_is_empty_without_limits() {
        assert!(goal_with("active").budget_hint().is_empty());
    }

    #[test]
    fn splits_title_and_notes() {
        let (title, notes) = split_title_notes("ship it --notes fast and safe");
        assert_eq!(title, "ship it");
        assert_eq!(notes.as_deref(), Some("fast and safe"));

        let (title, notes) = split_title_notes("just a title");
        assert_eq!(title, "just a title");
        assert!(notes.is_none());
    }

    #[test]
    fn status_text_without_goal_is_actionable() {
        assert!(render_status_text(None).contains("/goal start"));
    }

    #[test]
    fn apply_limits_distinguishes_clear_from_untouched() {
        let mut base = goal_with("active");
        base.max_turns = Some(10);
        base.deadline_ms = Some(5_000);

        // Only turns given → deadline preserved, turns replaced.
        let updated = apply_limits(&base, 1_000, Some(Some(20)), None);
        assert_eq!(updated.max_turns, Some(20));
        assert_eq!(updated.deadline_ms, Some(5_000));

        // Explicit clear → that field dropped, the other preserved.
        let updated = apply_limits(&base, 1_000, Some(None), None);
        assert_eq!(updated.max_turns, None);
        assert_eq!(updated.deadline_ms, Some(5_000));

        // Relative deadline resolves against `now`.
        let updated = apply_limits(&base, 1_000, None, Some(Some(3_000)));
        assert_eq!(updated.deadline_ms, Some(4_000));
        assert_eq!(updated.max_turns, Some(10));

        // Nothing passed → unchanged.
        let updated = apply_limits(&base, 1_000, None, None);
        assert_eq!(updated.max_turns, Some(10));
        assert_eq!(updated.deadline_ms, Some(5_000));
    }
}
