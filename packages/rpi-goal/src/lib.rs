#![forbid(unsafe_code)]

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    CommandHandlerFn, EventHandlerFn, EventTag, FreeStringFn, PluginApi, RuntimeActionId,
    StablePluginEvent, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult,
    ToolPartialCb,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const GOAL_FILE: &str = ".rpi/goals.json";
/// State is stored as a custom entry in the current session branch. Unlike the
/// legacy project file, this cannot leak an active goal into another session.
const GOAL_STATE_ENTRY: &str = "goal-state";
/// The custom-message type stored in the generic AgentMessage payload.
const GOAL_CUSTOM_TYPE: &str = "goal";
const MAX_TITLE_CHARS: usize = 500;
const MAX_NOTES_CHARS: usize = 20_000;

// Reuse the SDK's host-context lifetime and thread-safety contract.
type Runtime = PluginApi;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
/// Last injected goal-state fingerprint (title + status + notes). Prevents
/// re-injecting the same status on every `TurnEnd` (transcript bloat).
static LAST_INJECTED: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn last_injected() -> &'static Mutex<Option<String>> {
    LAST_INJECTED.get_or_init(|| Mutex::new(None))
}

/// A goal notice queued by a state change that happened **inside a tool batch**
/// (the `goal` tool). Appending it immediately would place a custom message
/// between an `assistant(tool_calls)` message and its `toolResult`, which strict
/// OpenAI-compatible providers reject with HTTP 400 ("An assistant message with
/// 'tool_calls' must be followed by tool messages…"). The notice is flushed at a
/// safe boundary instead (TurnEnd, or the end of the `/goal` command handler).
struct PendingInjection {
    revision: u64,
    goal: Goal,
    budget_reason: Option<String>,
}

static PENDING_INJECT: OnceLock<Mutex<Option<PendingInjection>>> = OnceLock::new();
static PENDING_STATE: OnceLock<Mutex<Option<Goals>>> = OnceLock::new();
// Tests and older hosts may not implement GetSessionBranch. Live hosts return
// an `entries` array, which enables session-scoped state; otherwise retain the
// file fallback for compatibility.
static SESSION_BRANCH_SUPPORTED: AtomicBool = AtomicBool::new(true);
// Accessed under the queue mutex, including while its pending slot is empty
// because a flush has taken the notice into an in-flight host callback.
static INJECTION_REVISION: AtomicU64 = AtomicU64::new(0);

fn pending_inject() -> &'static Mutex<Option<PendingInjection>> {
    PENDING_INJECT.get_or_init(|| Mutex::new(None))
}

fn pending_state() -> &'static Mutex<Option<Goals>> {
    PENDING_STATE.get_or_init(|| Mutex::new(None))
}

fn queue_state(goals: &Goals) {
    *pending_state().lock().unwrap_or_else(|p| p.into_inner()) = Some(goals.clone());
}

fn queue_inject(goal: &Goal) {
    queue_notice(goal, None);
}

fn queue_notice(goal: &Goal, budget_reason: Option<String>) {
    let mut pending = pending_inject().lock().unwrap_or_else(|p| p.into_inner());
    let revision = INJECTION_REVISION
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    *pending = Some(PendingInjection {
        revision,
        goal: goal.clone(),
        budget_reason,
    });
}

fn flush_pending_inject() {
    flush_pending_state();
    flush_pending_inject_with(|pending| match &pending.budget_reason {
        Some(reason) => append_budget_message(&pending.goal, reason),
        None => append_goal_custom_message(&pending.goal),
    });
}

fn flush_pending_inject_with(inject: impl FnOnce(&PendingInjection) -> Result<(), String>) {
    let pending = pending_inject()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    if let Some(pending) = pending {
        let result = inject(&pending);
        let mut queued = pending_inject().lock().unwrap_or_else(|p| p.into_inner());
        // A newer notice may have been queued AND delivered while this host
        // call was in flight. An empty slot alone cannot distinguish that
        // from this notice still being current.
        if INJECTION_REVISION.load(Ordering::Relaxed) != pending.revision {
            return;
        }
        if result.is_ok() {
            remember_injected(Some(fingerprint(&pending.goal)));
        } else {
            // Retry at a later safe boundary, but never overwrite a newer
            // goal change queued while the host callback was in flight.
            queued.get_or_insert(pending);
        }
    }
}

/// Environment markers meaning "this process is a subagent spawned by an
/// orchestrator". Such a process shares the parent's project directory — and
/// therefore its `.rpi/goals.json` — so its turns must not be counted against
/// the parent's goal budget.
///
/// `RPI_ORCHESTRATOR_SUBAGENT` is rpi-orchestrator's marker; the `LANGFUSE_*` one
/// is the SDK's cross-repo "I am a subagent" contract
/// ([`rpi_plugin_sdk::SUBAGENT_PARENT_TRACE_ID_ENV`]).
const ORCHESTRATOR_SUBAGENT_ENV: &str = "RPI_ORCHESTRATOR_SUBAGENT";

fn is_subagent_from(orchestrator_marker: bool, langfuse_parent: bool) -> bool {
    orchestrator_marker || langfuse_parent
}

static PROCESS_IS_SUBAGENT: AtomicBool = AtomicBool::new(false);

// Capture inherited markers at plugin initialization. Langfuse also publishes
// its parent trace marker in the parent process during a run, for future
// children to inherit; reading that marker on TurnEnd misclassifies the parent.
fn capture_process_role() {
    PROCESS_IS_SUBAGENT.store(
        is_subagent_from(
            std::env::var_os(ORCHESTRATOR_SUBAGENT_ENV).is_some(),
            std::env::var_os(rpi_plugin_sdk::SUBAGENT_PARENT_TRACE_ID_ENV).is_some(),
        ),
        Ordering::Relaxed,
    );
}

fn is_subagent() -> bool {
    PROCESS_IS_SUBAGENT.load(Ordering::Relaxed)
}

/// Call a host runtime action (e.g. `AppendEntry`). Safe from any thread,
/// including event handlers and command handlers.
fn call_runtime(id: RuntimeActionId, args: Value) -> Result<Value, String> {
    let runtime = *RUNTIME.get().ok_or("goal runtime is unavailable")?;
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

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Goal {
    title: String,
    status: String, // active | paused | complete
    notes: String,
    #[serde(alias = "started_at")]
    started_at: i64,
    #[serde(alias = "updated_at")]
    updated_at: i64,
    /// Turns completed while the goal was active. Drives the `max_turns` budget.
    #[serde(default, alias = "turns_used")]
    turns_used: u32,
    /// Turn budget. `None` = unlimited.
    #[serde(default, alias = "max_turns")]
    max_turns: Option<u32>,
    /// Absolute epoch-ms deadline. `None` = unlimited.
    #[serde(default, alias = "deadline_ms")]
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

fn flush_pending_state() {
    let state = pending_state()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    let Some(goals) = state else {
        return;
    };
    if write_goals(&goals).is_err() {
        *pending_state().lock().unwrap_or_else(|p| p.into_inner()) = Some(goals);
    }
}

fn read_goals() -> Goals {
    if RUNTIME.get().is_some() && SESSION_BRANCH_SUPPORTED.load(Ordering::Relaxed) {
        if let Some(goals) = pending_state()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return goals;
        }
        if let Ok(value) = call_runtime(RuntimeActionId::GetSessionBranch, json!({})) {
            if let Some(entries) = value.get("entries").and_then(Value::as_array) {
                SESSION_BRANCH_SUPPORTED.store(true, Ordering::Relaxed);
                for entry in entries.iter().rev() {
                    if entry.get("type").and_then(Value::as_str) == Some("custom")
                        && entry.get("customType").and_then(Value::as_str) == Some(GOAL_STATE_ENTRY)
                    {
                        if let Some(data) = entry.get("data") {
                            return serde_json::from_value(data.clone()).unwrap_or_default();
                        }
                    }
                }
                return Goals::default();
            }
        }
        SESSION_BRANCH_SUPPORTED.store(false, Ordering::Relaxed);
    }

    let path = goals_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return Goals::default();
    };
    serde_json::from_str(&text).unwrap_or_else(|_| Goals::default())
}

fn write_goals(goals: &Goals) -> Result<(), String> {
    if RUNTIME.get().is_some() && SESSION_BRANCH_SUPPORTED.load(Ordering::Relaxed) {
        call_runtime(
            RuntimeActionId::AppendEntry,
            json!({"customType": GOAL_STATE_ENTRY, "data": goals}),
        )?;
        return Ok(());
    }
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

/// Persist `goal` as the active goal and queue its context injection.
///
/// The notice is appended at the next safe boundary (`flush_pending_inject`),
/// never inline: a state change from the `goal` tool runs inside a tool batch,
/// where an immediate append would split `tool_calls` from its `toolResult`.
fn persist_and_queue(goals: &Goals, goal: &Goal) -> Result<(), String> {
    let mut goals = goals.clone();
    goals.active = Some(goal.clone());
    if RUNTIME.get().is_some() && SESSION_BRANCH_SUPPORTED.load(Ordering::Relaxed) {
        queue_state(&goals);
    } else {
        write_goals(&goals)?;
    }
    queue_inject(goal);
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
    persist_and_queue(&goals, &goal)?;
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
    persist_and_queue(&goals, &updated)?;
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
    persist_and_queue(&goals, &updated)?;
    Ok(updated)
}

fn op_complete() -> Result<Goal, String> {
    let mut goals = read_goals();
    let mut done = require_active(&goals)?;
    done.status = "complete".into();
    done.updated_at = now_ms();
    goals.archived.push(done.clone());
    goals.active = None;
    if RUNTIME.get().is_some() && SESSION_BRANCH_SUPPORTED.load(Ordering::Relaxed) {
        queue_state(&goals);
    } else {
        write_goals(&goals)?;
    }
    queue_inject(&done);
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
        return StepResult::err(StbString::from_string("null goal handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    if entry.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
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
    HANDLES.cancel(handle);
}

extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}

use rpi_plugin_sdk::callbacks::free_string;

// ---------------------------------------------------------------------------
// TurnEnd handler — count turns, enforce budgets, inject the active goal
// ---------------------------------------------------------------------------

extern "C" fn on_turn_end(event: StablePluginEvent, _: *mut c_void) -> i32 {
    // TurnEnd carries no payload (empty event) in this ABI; ignore the struct.
    let _ = event;
    // A subagent process shares the parent's cwd and therefore its
    // `.rpi/goals.json`; counting its turns would inflate `turnsUsed` and could
    // pause the orchestrator early. Skip accounting and injection entirely.
    if is_subagent() {
        return 0;
    }
    // Safe boundary: flush any notice queued during this turn's tool batch.
    flush_pending_inject();
    let mut goals = read_goals();
    let Some(mut goal) = goals.active.clone() else {
        // No active goal — nothing to inject.
        remember_injected(None);
        return 0;
    };
    if goal.status != "active" {
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
        // This paused state supersedes an active notice whose append failed.
        // Keep the reason too, so a retry still explains why work stopped.
        queue_notice(&goal, Some(reason));
        flush_pending_inject();
        return 0;
    }

    goals.active = Some(goal.clone());
    let _ = write_goals(&goals);

    let fp = fingerprint(&goal);
    if pending_inject()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some()
        || !should_inject(Some(&fp))
    {
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

fn command_output(out: &mut rpi_plugin_sdk::callbacks::Output<'_>, value: Value) -> i32 {
    if !out.is_available() {
        return 1;
    }
    out.write(value.to_string());
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

const START_NEXT_STEP_HINT: &str =
    "ℹ️ Goal recorded; the first work run is starting in the background.";

fn start_from_command(title: &str, notes: &str) -> Result<String, String> {
    let goal = op_start(title, notes, None, None)?;
    // Persist the session state and notice before starting the run. The full
    // SendUserMessage call itself must not run on the TUI command thread.
    flush_pending_state();
    flush_pending_inject();
    let prompt = if goal.notes.trim().is_empty() {
        format!("Work on the active goal: {}", goal.title)
    } else {
        format!(
            "Work on the active goal: {}\nNotes: {}",
            goal.title, goal.notes
        )
    };
    // A full SendUserMessage waits for the agent run to finish. Calling it
    // inline deadlocks the TUI, leaving the editor populated and unresponsive.
    std::thread::Builder::new()
        .name("rpi-goal-start".into())
        .spawn(move || {
            if let Err(error) =
                call_runtime(RuntimeActionId::SendUserMessage, json!({"text": prompt}))
            {
                eprintln!("rpi-goal: failed to start goal run: {error}");
            }
        })
        .map_err(|error| format!("unable to start goal worker: {error}"))?;
    Ok(format!(
        "🎯 Goal started: {}\n{}\n{}",
        goal.title,
        goal.budget_summary(now_ms()),
        START_NEXT_STEP_HINT
    ))
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
            start_from_command(&title, notes.as_deref().unwrap_or(""))
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
            start_from_command(&title, notes.as_deref().unwrap_or(""))
        }
    };

    match result {
        Ok(text) => text,
        Err(error) => format!("⚠️ {error}"),
    }
}

rpi_plugin_sdk::string_handler! {
fn goal_command(args_json, out, _user_data) {
    let raw = args_json.to_owned();
    let envelope: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let args = envelope
        .get("args")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let text = run_command(args);
    // The command runs outside any tool batch, so injecting right here is safe —
    // and necessary: the goal must be in context before the next prompt.
    flush_pending_inject();
    command_output(out, json!({"kind": "message", "text": text}))
}
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------
rpi_plugin_sdk::export_plugin!(|api| {
    capture_process_role();
    let _ = RUNTIME.set(*api);
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
});

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

    #[test]
    fn subagent_markers_skip_turn_accounting() {
        assert!(is_subagent_from(true, false));
        assert!(is_subagent_from(false, true));
        assert!(!is_subagent_from(false, false));
    }

    #[test]
    fn start_confirmation_mentions_background_work() {
        assert!(START_NEXT_STEP_HINT.contains("background"));
        assert!(START_NEXT_STEP_HINT.contains("Goal recorded"));
    }

    // -----------------------------------------------------------------------
    // Review follow-ups: the two tests below cover the *file state* and the
    // *event-handler path*, which the boolean-only test above does not.
    // -----------------------------------------------------------------------

    /// Serializes everything below: they mutate process-global state (cwd, env,
    /// the `RUNTIME` vtable, the injected-fingerprint latch).
    static GLOBAL_STATE_LOCK: Mutex<()> = Mutex::new(());

    struct ProcessRoleGuard {
        markers: Vec<(&'static str, Option<std::ffi::OsString>)>,
        previous_role: bool,
    }

    impl ProcessRoleGuard {
        fn parent() -> Self {
            let markers = [
                ORCHESTRATOR_SUBAGENT_ENV,
                rpi_plugin_sdk::SUBAGENT_PARENT_TRACE_ID_ENV,
            ]
            .into_iter()
            .map(|name| {
                let value = std::env::var_os(name);
                std::env::remove_var(name);
                (name, value)
            })
            .collect();
            let previous_role = is_subagent();
            capture_process_role();
            Self {
                markers,
                previous_role,
            }
        }
    }

    impl Drop for ProcessRoleGuard {
        fn drop(&mut self) {
            for (name, value) in &self.markers {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            PROCESS_IS_SUBAGENT.store(self.previous_role, Ordering::Relaxed);
        }
    }

    #[test]
    fn parent_keeps_counting_and_flushing_after_langfuse_publishes_trace() {
        let _serial = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _cwd = isolated_cwd("parent-trace");
        let _role = ProcessRoleGuard::parent();
        install_recording_runtime();
        remember_injected(None);
        APPENDED.lock().unwrap().clear();
        op_start("Parent budget", "", Some(1), None).unwrap();

        // BeforeAgentStart publishes this for future children after plugins load.
        std::env::set_var(rpi_plugin_sdk::SUBAGENT_PARENT_TRACE_ID_ENV, "parent-trace");
        turn_end();

        let goal = read_goals().active.unwrap();
        assert_eq!(goal.turns_used, 1);
        assert_eq!(goal.status, "paused");
        assert!(!APPENDED.lock().unwrap().is_empty());
        assert!(pending_inject().lock().unwrap().is_none());
    }

    #[test]
    fn child_role_survives_langfuse_clearing_inherited_trace() {
        let _serial = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _cwd = isolated_cwd("child-trace");
        let _role = ProcessRoleGuard::parent();
        std::env::set_var(
            rpi_plugin_sdk::SUBAGENT_PARENT_TRACE_ID_ENV,
            "inherited-trace",
        );
        capture_process_role();
        op_start("Parent budget", "", Some(1), None).unwrap();
        std::env::remove_var(rpi_plugin_sdk::SUBAGENT_PARENT_TRACE_ID_ENV);
        turn_end();
        let goal = read_goals().active.unwrap();
        assert_eq!(goal.turns_used, 0);
        assert_eq!(goal.status, "active");
    }

    /// Run with the process cwd pointed at a fresh temp directory, restoring it
    /// afterwards. `goals_path()` is cwd-relative, so this is the only way to
    /// exercise the real read/write path without touching the developer's project.
    struct CwdGuard {
        previous: PathBuf,
        dir: PathBuf,
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn isolated_cwd(tag: &str) -> CwdGuard {
        let dir = std::env::temp_dir().join(format!("rpi-goal-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp cwd");
        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&dir).expect("enter temp cwd");
        CwdGuard { previous, dir }
    }

    fn turn_end() {
        on_turn_end(
            StablePluginEvent::empty(EventTag::TurnEnd),
            std::ptr::null_mut(),
        );
    }

    /// Regression: a subagent process shares the parent's cwd — and therefore
    /// its `.rpi/goals.json` — so its `TurnEnd` used to increment the parent's
    /// `turnsUsed`. A wave of subagents could exhaust any `/goal turns N` budget
    /// on its own. The parent must keep counting; subagents under *either* marker
    /// must not.
    #[test]
    fn subagent_turns_do_not_count_against_the_parent_goal() {
        let _serial = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _cwd = isolated_cwd("subagent-budget");
        let _role = ProcessRoleGuard::parent();
        remember_injected(None);

        // The parent records a goal (this happens inside a tool batch).
        op_start("Ship it", "", None, None).expect("goal starts");

        let turns_used = || {
            read_goals()
                .active
                .expect("goal is still active")
                .turns_used
        };

        // A normal parent turn counts, and lands in the real file.
        turn_end();
        assert_eq!(turns_used(), 1, "the parent's own turn must count");

        // Subagents in the same cwd run several turns each. Neither marker may
        // add to the parent's budget.
        for marker in [
            ORCHESTRATOR_SUBAGENT_ENV,
            rpi_plugin_sdk::SUBAGENT_PARENT_TRACE_ID_ENV,
        ] {
            std::env::set_var(marker, "1");
            capture_process_role();
            for _ in 0..5 {
                turn_end();
            }
            std::env::remove_var(marker);
        }
        assert_eq!(
            turns_used(),
            1,
            "a subagent's turns leaked into the parent's goal budget"
        );

        // The parent still counts once the markers are gone.
        capture_process_role();
        turn_end();
        assert_eq!(turns_used(), 2);
    }

    /// Records the `AppendEntry` payloads the plugin asks the host for, in order.
    static APPENDED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static FAIL_APPENDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    rpi_plugin_sdk::string_handler! {
    fn recording_runtime_action(action, args, out, _user_data) {
        if action == RuntimeActionId::AppendEntry as u32 {
            if FAIL_APPENDS.fetch_update(Ordering::SeqCst, Ordering::SeqCst,
                |remaining| remaining.checked_sub(1)).is_ok() {
                return 1;
            }
            APPENDED
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(args.to_owned());
        }
        let _ = out;
        0
    }
    }

    /// Install the recording runtime. `RUNTIME` is a `OnceLock`, so the first
    /// caller wins; the recorder is deterministic regardless of which test it is.
    fn install_recording_runtime() {
        let _ = RUNTIME.set(rpi_plugin_sdk::PluginApi {
            abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
            struct_size: std::mem::size_of::<rpi_plugin_sdk::PluginApi>() as u32,
            free_string: rpi_plugin_sdk::callbacks::free_string,
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
            runtime_action: recording_runtime_action,
            dispatch_event: None,
            user_data: std::ptr::null_mut(),
            declare: None,
            register_before_agent_start: None,
            register_oauth_provider: None,
        });
    }

    /// Regression for the HTTP 400: a `goal` tool call runs *inside* a tool
    /// batch, so appending its notice immediately placed a custom message
    /// between `assistant(tool_calls)` and its `toolResult`. The notice must be
    /// queued and flushed only at the turn boundary.
    #[test]
    fn goal_notice_is_appended_at_the_turn_boundary_not_inside_the_batch() {
        let _serial = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _cwd = isolated_cwd("inject-boundary");
        let _role = ProcessRoleGuard::parent();
        install_recording_runtime();
        remember_injected(None);
        APPENDED.lock().unwrap_or_else(|p| p.into_inner()).clear();

        // Inside the batch: none of these may append anything.
        op_start("Ship it", "", None, None).expect("start");
        op_update(None, Some("active")).expect("update");
        op_complete().expect("complete");
        assert!(
            APPENDED
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_empty(),
            "a goal tool call inside a batch must not append a message"
        );

        // The turn boundary is where the queued notice lands.
        turn_end();
        let appended = APPENDED.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            appended.len(),
            1,
            "exactly one notice at the boundary, got {appended:?}"
        );
        assert!(appended[0].contains("Ship it"), "got {}", appended[0]);
    }

    #[test]
    fn queued_goal_notices_retry_after_host_append_failure() {
        let _serial = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _cwd = isolated_cwd("notice-retry");
        let _role = ProcessRoleGuard::parent();
        install_recording_runtime();
        for status in ["active", "paused", "complete"] {
            remember_injected(None);
            op_start("Retry notice", "", None, None).unwrap();
            match status {
                "paused" => {
                    op_set_status("paused").unwrap();
                }
                "complete" => {
                    op_complete().unwrap();
                }
                _ => {}
            }
            APPENDED.lock().unwrap().clear();
            FAIL_APPENDS.store(1, Ordering::SeqCst);
            turn_end();
            let delivered = APPENDED.lock().unwrap().clone();
            assert!(
                delivered.is_empty(),
                "retry ran twice at one boundary: {delivered:?}"
            );
            assert!(
                pending_inject().lock().unwrap().is_some(),
                "{status} notice was lost after AppendEntry failed"
            );
            assert!(
                last_injected().lock().unwrap().is_none(),
                "{status} notice was marked delivered before AppendEntry succeeded"
            );
            turn_end();
            let delivered = APPENDED.lock().unwrap().clone();
            assert_eq!(delivered.len(), 1, "{status}: {delivered:?}");
            let message: Value = serde_json::from_str(&delivered[0]).unwrap();
            assert_eq!(message["message"]["data"]["details"]["status"], status);
            assert!(pending_inject().lock().unwrap().is_none());
            turn_end();
            assert_eq!(
                APPENDED.lock().unwrap().len(),
                1,
                "duplicate {status} notice"
            );
        }
    }

    #[test]
    fn exhausted_budget_notice_retries_without_replaying_stale_active_goal() {
        let _serial = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _cwd = isolated_cwd("budget-notice-retry");
        let _role = ProcessRoleGuard::parent();
        install_recording_runtime();
        remember_injected(None);
        op_start("Budget notice", "", Some(1), None).unwrap();
        APPENDED.lock().unwrap().clear();
        // Fail both the queued active notice and the subsequent budget notice.
        FAIL_APPENDS.store(2, Ordering::SeqCst);
        turn_end();
        assert_eq!(read_goals().active.unwrap().status, "paused");
        assert!(APPENDED.lock().unwrap().is_empty());
        assert!(
            pending_inject().lock().unwrap().is_some(),
            "budget notice was lost"
        );
        turn_end();
        let delivered = APPENDED.lock().unwrap().clone();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert!(delivered[0].contains("turn budget reached (1)"));
        assert!(!delivered[0].contains("Active goal"));
        turn_end();
        assert_eq!(APPENDED.lock().unwrap().len(), 1);
    }

    #[test]
    fn newer_goal_change_supersedes_a_notice_waiting_for_retry() {
        let _serial = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _cwd = isolated_cwd("notice-superseded");
        let _role = ProcessRoleGuard::parent();
        install_recording_runtime();
        remember_injected(None);
        op_start("Latest notice", "", None, None).unwrap();
        op_set_status("paused").unwrap();
        APPENDED.lock().unwrap().clear();
        FAIL_APPENDS.store(1, Ordering::SeqCst);
        turn_end();
        assert!(APPENDED.lock().unwrap().is_empty());

        // The user resumes and updates the goal before its failed pause notice
        // can be retried. Only the current active state should reach context.
        op_update(Some("new progress"), Some("active")).unwrap();
        turn_end();
        let delivered = APPENDED.lock().unwrap().clone();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        let message: Value = serde_json::from_str(&delivered[0]).unwrap();
        let details = &message["message"]["data"]["details"];
        assert_eq!(details["status"], "active");
        assert_eq!(details["notes"], "new progress");
        assert!(pending_inject().lock().unwrap().is_none());
        assert_eq!(read_goals().active.unwrap().turns_used, 1);
    }

    #[test]
    fn older_failed_flush_cannot_requeue_after_a_newer_notice_was_delivered() {
        let _serial = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _cwd = isolated_cwd("notice-in-flight");
        let _role = ProcessRoleGuard::parent();
        install_recording_runtime();
        remember_injected(None);
        op_start("Concurrent notice", "old progress", None, None).unwrap();
        APPENDED.lock().unwrap().clear();
        flush_pending_inject_with(|_| {
            // Interleave another safe-boundary flush while the older callback
            // is still in flight, then let the older callback report failure.
            op_update(Some("new progress"), None).unwrap();
            flush_pending_inject();
            Err("older host callback failed".into())
        });
        assert!(
            pending_inject().lock().unwrap().is_none(),
            "stale notice was requeued"
        );
        let delivered = APPENDED.lock().unwrap().clone();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert!(delivered[0].contains("new progress"));
        assert_eq!(
            *last_injected().lock().unwrap(),
            Some(fingerprint(&read_goals().active.unwrap()))
        );
        turn_end();
        assert_eq!(
            APPENDED.lock().unwrap().len(),
            1,
            "stale notice reached context"
        );
    }
}
