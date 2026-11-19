//! Tool surface.
//!
//! `goal` remains the command-style tool. `goal_complete`, `goal_blocked`, and
//! `goal_wait` are the explicit stopping reasons: plain assistant text never
//! completes a goal.
//!
//! The host calls `execute(params)` with no tool name, so each tool needs its own
//! function pointers. The `drive!` macro keeps the four ABI drives identical.
use serde_json::{json, Value};
use std::ffi::c_void;

use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    FreeStringFn, PluginApi, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult,
    ToolPartialCb,
};

use crate::ops;
use crate::render;
use crate::runtime;
use crate::state::{self, Status};

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

fn param_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn param_u32(params: &Value, key: &str) -> Option<u32> {
    params.get(key).and_then(Value::as_u64).map(|v| v as u32)
}

fn param_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}

/// The command-style `goal` tool.
pub fn run_goal(params: &Value) -> Result<String, String> {
    let action = param_str(params, "action")
        .unwrap_or("show")
        .trim()
        .to_lowercase();
    match action.as_str() {
        "start" => {
            let title = param_str(params, "title").ok_or("goal objective is required")?;
            let notes = param_str(params, "notes").unwrap_or("");
            let deadline = param_u32(params, "maxMinutes")
                .filter(|n| *n > 0)
                .map(|minutes| (minutes as i64) * 60_000);
            let goal = ops::start(ops::StartOptions {
                title: title.to_string(),
                notes: notes.to_string(),
                max_turns: param_u32(params, "maxTurns"),
                deadline_ms: deadline,
                token_budget: param_u64(params, "tokenBudget"),
            })?;
            Ok(render::envelope(
                "start",
                render::started_markdown(&goal),
                true,
                Some(&goal),
            ))
        }
        "update" => {
            let status = match param_str(params, "status") {
                Some("active") => Some(Status::Active),
                Some("paused") => Some(Status::Paused),
                Some(other) => {
                    return Err(format!(
                        "status must be \"active\" or \"paused\", got `{other}`"
                    ))
                }
                None => None,
            };
            let goal = ops::update(param_str(params, "notes"), status)?;
            Ok(render::envelope(
                "update",
                render::updated_markdown(&goal),
                true,
                Some(&goal),
            ))
        }
        "pause" => {
            let goal = ops::pause()?;
            Ok(render::envelope(
                "pause",
                format!("## ⏸️ Goal paused\n\n**{}**", goal.title),
                true,
                Some(&goal),
            ))
        }
        "resume" => {
            let goal = ops::resume()?;
            Ok(render::envelope(
                "resume",
                format!("## ▶️ Goal resumed\n\n**{}**", goal.title),
                true,
                Some(&goal),
            ))
        }
        "complete" => {
            let goal = ops::complete()?;
            Ok(render::envelope(
                "complete",
                render::completed_markdown(&goal),
                false,
                Some(&goal),
            ))
        }
        "limit" => {
            let max_turns = param_u32(params, "maxTurns").map(|n| n.max(1));
            let minutes = param_u64(params, "maxMinutes").map(|n| (n as i64) * 60_000);
            let tokens = param_u64(params, "tokenBudget").filter(|n| *n > 0);
            if max_turns.is_none() && minutes.is_none() && tokens.is_none() {
                return Err("limit requires `maxTurns`, `maxMinutes`, and/or `tokenBudget`".into());
            }
            let goal = ops::set_limits(max_turns.map(Some), minutes.map(Some), tokens.map(Some))?;
            Ok(render::envelope(
                "limit",
                render::limits_markdown(&goal),
                true,
                Some(&goal),
            ))
        }
        "show" | "status" => {
            let goals = state::read()?;
            Ok(render::envelope(
                "show",
                render::show_markdown(goals.active.as_ref()),
                goals.active.is_some(),
                goals.active.as_ref(),
            ))
        }
        "list" => {
            let goals = state::read()?;
            Ok(render::envelope(
                "list",
                render::list_markdown(&goals),
                goals.active.is_some(),
                goals.active.as_ref(),
            ))
        }
        other => Err(format!("unknown goal action: {other}")),
    }
}

pub fn run_complete(params: &Value) -> Result<String, String> {
    let goal_id = param_str(params, "goal_id").ok_or("goal_id is required")?;
    let summary = param_str(params, "summary").ok_or("completion summary is required")?;
    let goal = ops::complete_with(goal_id, summary)?;
    Ok(render::envelope(
        "goal_complete",
        crate::prompts::completion_summary(&goal, summary),
        false,
        Some(&goal),
    ))
}

pub fn run_blocked(params: &Value) -> Result<String, String> {
    let goal_id = param_str(params, "goal_id").ok_or("goal_id is required")?;
    let reason = param_str(params, "reason").ok_or("blocker reason is required")?;
    let evidence = param_str(params, "evidence").ok_or("blocker evidence is required")?;
    let repeated = param_u32(params, "repeated_turns").unwrap_or(0);
    let goal = ops::block(goal_id, reason, evidence, repeated)?;
    Ok(render::envelope(
        "goal_blocked",
        crate::prompts::blocked_summary(&goal, reason, evidence, repeated),
        false,
        Some(&goal),
    ))
}

pub fn run_wait(params: &Value) -> Result<String, String> {
    let goal_id = param_str(params, "goal_id").ok_or("goal_id is required")?;
    let reason = param_str(params, "reason").ok_or("wait reason is required")?;
    let resume_after_ms = params.get("resume_after_ms").and_then(Value::as_i64);
    let goal = ops::wait(goal_id, reason, resume_after_ms)?;
    Ok(render::envelope(
        "goal_wait",
        format!(
            "## ⏳ Goal waiting\n\n**{}**\n\n{}\n",
            goal.title,
            reason.trim()
        ),
        true,
        Some(&goal),
    ))
}

// ---------------------------------------------------------------------------
// ABI drives
// ---------------------------------------------------------------------------

/// Generate one tool drive. `$run` receives the parsed parameters and returns
/// the JSON result envelope.
macro_rules! drive {
    ($handles:ident, $execute:ident, $poll:ident, $cancel:ident, $destroy:ident, $run:path) => {
        static $handles: ToolHandles<Drive> = ToolHandles::new();

        extern "C" fn $execute(
            _: StbStringRef,
            params: StbString,
            free: Option<FreeStringFn>,
        ) -> StepHandle {
            let text = params.to_string_lossy();
            params.free_with(free);
            $handles.insert(Drive {
                runtime: runtime::snapshot(),
                params: serde_json::from_str(&text).unwrap_or(Value::Null),
                done: false,
            })
        }

        extern "C" fn $poll(
            handle: StepHandle,
            _: Option<ToolPartialCb>,
            _: *mut c_void,
        ) -> StepResult {
            if handle.is_null() {
                return StepResult::err(StbString::from_string("null goal handle".into()));
            }
            let Some(entry) = $handles.get(handle) else {
                return StepResult::err(StbString::from_string(
                    "invalid or destroyed tool handle".into(),
                ));
            };
            let mut drive = entry.lock();
            let _scope = runtime::enter(drive.runtime);
            if entry.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                drive.done = true;
                return StepResult::err(StbString::from_string("goal tool cancelled".into()));
            }
            if drive.done {
                return StepResult::err(StbString::from_string(
                    "goal tool polled after completion".into(),
                ));
            }
            drive.done = true;
            match $run(&drive.params) {
                Ok(value) => StepResult::done(StbString::from_string(value)),
                Err(error) => StepResult::err(StbString::from_string(error)),
            }
        }

        extern "C" fn $cancel(handle: StepHandle) {
            $handles.cancel(handle);
        }

        extern "C" fn $destroy(handle: StepHandle) {
            drop($handles.remove(handle));
        }
    };
}

struct Drive {
    runtime: Option<PluginApi>,
    params: Value,
    done: bool,
}

drive!(
    GOAL,
    goal_execute,
    goal_poll,
    goal_cancel,
    goal_destroy,
    run_goal
);
drive!(
    COMPLETE,
    complete_execute,
    complete_poll,
    complete_cancel,
    complete_destroy,
    run_complete
);
drive!(
    BLOCKED,
    blocked_execute,
    blocked_poll,
    blocked_cancel,
    blocked_destroy,
    run_blocked
);
drive!(
    WAIT,
    wait_execute,
    wait_poll,
    wait_cancel,
    wait_destroy,
    run_wait
);

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

fn schema(name: &str, description: &str, parameters: Value) -> StableToolSchema {
    StableToolSchema {
        name: StbString::from_string(name.into()),
        description: StbString::from_string(description.into()),
        parameters: StbString::from_string(parameters.to_string()),
    }
}

pub fn goal_schema() -> StableToolSchema {
    let parameters = json!({
        "type": "object", "additionalProperties": true,
        "description": "Arguments apply to the indicated actions.",
        "properties": {
            "action": {"type": "string", "enum": ["start", "update", "pause", "resume", "complete", "limit", "show", "list"], "default": "show", "description": "Operation; omitted action shows the current goal."},
            "title": {"type": "string", "pattern": "\\S", "description": "Required for start: the full objective, nonblank, at most 4000 Unicode scalar values. The previous goal is archived and replaced. Automatic continuation starts after the current turn settles."},
            "notes": {"type": "string", "description": "start/update: trimmed notes, at most 20000 Unicode scalar values. Start defaults to empty; update replaces notes and an empty string clears them."},
            "status": {"type": "string", "enum": ["active", "paused"], "description": "update only: explicit status change. At least notes or status is required."},
            "maxTurns": {"type": "integer", "minimum": 1, "maximum": 4294967295u64, "description": "start/limit: total turn budget, not additional turns. Omitted on start means unlimited; omitted on limit preserves it. Clear with /goal turns 0."},
            "maxMinutes": {"type": "integer", "minimum": 1, "maximum": 4294967295u64, "description": "start/limit: wall-clock minutes from now. Omitted on start means unlimited; omitted on limit preserves the current deadline. Clear with /goal time 0."},
            "tokenBudget": {"type": "integer", "minimum": 1, "description": "start/limit: cumulative token budget for Goal-owned work. Omitted means no token budget. Clear with /goal tokens 0."}
        },
        "allOf": [
            {"if": {"properties": {"action": {"const": "start"}}, "required": ["action"]}, "then": {"required": ["title"]}},
            {"if": {"properties": {"action": {"const": "update"}}, "required": ["action"]}, "then": {"anyOf": [{"required": ["notes"]}, {"required": ["status"]}]}},
            {"if": {"properties": {"action": {"const": "limit"}}, "required": ["action"]}, "then": {"anyOf": [{"required": ["maxTurns"]}, {"required": ["maxMinutes"]}, {"required": ["tokenBudget"]}]}}
        ]
    });
    schema(
        "goal",
        "Record one persistent objective in the current native session branch and enable goal mode: work continues automatically after the agent settles until the goal completes, blocks, waits, or reaches a limit. start archives/replaces the current goal; update replaces notes and/or status; pause/resume/limit require an existing goal; complete is the manual archive path (prefer goal_complete); show/list read state. This tool does not spawn a nested agent run. Subagents must not manage the parent's goal.",
        parameters,
    )
}

pub fn complete_schema() -> StableToolSchema {
    let parameters = json!({
        "type": "object", "additionalProperties": true,
        "required": ["goal_id", "summary"],
        "properties": {
            "goal_id": {"type": "string", "pattern": "\\S", "description": "The exact goal_id shown in the current goal prompt. Rejects stale calls from older, replaced, stopped, or cleared goals."},
            "summary": {"type": "string", "pattern": "\\S", "maxLength": 4000, "description": "Completion evidence: what proves every requirement is satisfied, matched to the requirement it supports. Not a plan or a promise."}
        }
    });
    schema(
        "goal_complete",
        "Mark the active goal complete. Only call this when the current goal prompt says goal mode is active, you pass its exact goal_id, and authoritative evidence proves every requirement is satisfied. Never call it for partial progress, blockers, failures, or unverified work; plain assistant text never completes a goal.",
        parameters,
    )
}

pub fn blocked_schema() -> StableToolSchema {
    let parameters = json!({
        "type": "object", "additionalProperties": true,
        "required": ["goal_id", "reason", "evidence", "repeated_turns"],
        "properties": {
            "goal_id": {"type": "string", "pattern": "\\S", "description": "The exact goal_id shown in the current goal prompt."},
            "reason": {"type": "string", "pattern": "\\S", "maxLength": 1000, "description": "The specific user or external action required to unblock the goal."},
            "evidence": {"type": "string", "pattern": "\\S", "maxLength": 4000, "description": "Concrete evidence from the repeated attempts that proves the impasse."},
            "repeated_turns": {"type": "integer", "minimum": 3, "description": "Number of separate turns spent hitting this same blocker. Values below 3 are rejected."}
        }
    });
    schema(
        "goal_blocked",
        "Stop the active goal only at a true impasse: the same evidenced external blocker recurred for at least three consecutive goal turns. Never use it for ordinary clarification, uncertainty, incomplete work, or a recoverable failure. Prefer goal_wait when progress depends on a later external event.",
        parameters,
    )
}

pub fn wait_schema() -> StableToolSchema {
    let parameters = json!({
        "type": "object", "additionalProperties": true,
        "required": ["goal_id", "reason"],
        "properties": {
            "goal_id": {"type": "string", "pattern": "\\S", "description": "The exact goal_id shown in the current goal prompt."},
            "reason": {"type": "string", "pattern": "\\S", "maxLength": 1000, "description": "Why the goal is waiting and which external event should wake it."},
            "resume_after_ms": {"type": "integer", "minimum": 1, "maximum": 2147483647i64, "description": "Optional safety deadline in milliseconds. Values below 10000 are clamped to 10000. A deadline is a safety wake-up, not a polling interval."}
        }
    });
    schema(
        "goal_wait",
        "Keep the active goal quiet without automatic continuation. Only call it after arranging a wake source (a monitor or other non-goal message), and call it alone in its turn. Omitting resume_after_ms keeps the goal quiet until external input; a deadline only bounds the wait.",
        parameters,
    )
}

/// Register all four tools. Returns the host return code of the last call.
pub fn register(api: &PluginApi) -> i32 {
    let Some(register) = api.register_tool else {
        return 1;
    };
    let plan: [(
        StableToolSchema,
        rpi_plugin_sdk::ToolExecuteFn,
        rpi_plugin_sdk::ToolPollFn,
        rpi_plugin_sdk::ToolCancelFn,
        rpi_plugin_sdk::ToolDestroyFn,
    ); 4] = [
        (
            goal_schema(),
            goal_execute,
            goal_poll,
            goal_cancel,
            goal_destroy,
        ),
        (
            complete_schema(),
            complete_execute,
            complete_poll,
            complete_cancel,
            complete_destroy,
        ),
        (
            blocked_schema(),
            blocked_execute,
            blocked_poll,
            blocked_cancel,
            blocked_destroy,
        ),
        (
            wait_schema(),
            wait_execute,
            wait_poll,
            wait_cancel,
            wait_destroy,
        ),
    ];
    let mut rc = 0;
    for (schema, execute, poll, cancel, destroy) in plan {
        let boxed = Box::new(schema);
        rc = register(
            &*boxed,
            execute,
            poll,
            cancel,
            destroy,
            rpi_plugin_sdk::callbacks::free_string,
        );
        drop(boxed);
        if rc != 0 {
            return rc;
        }
    }
    rc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameters(schema: StableToolSchema) -> Value {
        serde_json::from_str(&schema.parameters.to_string_lossy()).unwrap()
    }

    #[test]
    fn every_tool_schema_is_valid_and_conditional() {
        let goal = goal_schema();
        assert_eq!(goal.name.to_string_lossy(), "goal");
        let params = parameters(goal);
        assert_eq!(params["properties"]["action"]["default"], "show");
        assert_eq!(params["additionalProperties"], true);
        assert_eq!(params["allOf"].as_array().unwrap().len(), 3);
        assert_eq!(params["properties"]["title"]["pattern"], "\\S");

        let complete = complete_schema();
        assert_eq!(complete.name.to_string_lossy(), "goal_complete");
        let params = parameters(complete);
        assert_eq!(params["required"], json!(["goal_id", "summary"]));
        assert_eq!(params["properties"]["summary"]["maxLength"], 4000);

        let blocked = blocked_schema();
        assert_eq!(blocked.name.to_string_lossy(), "goal_blocked");
        let params = parameters(blocked);
        assert_eq!(
            params["required"],
            json!(["goal_id", "reason", "evidence", "repeated_turns"])
        );
        assert_eq!(params["properties"]["repeated_turns"]["minimum"], 3);

        let wait = wait_schema();
        assert_eq!(wait.name.to_string_lossy(), "goal_wait");
        let params = parameters(wait);
        assert_eq!(params["required"], json!(["goal_id", "reason"]));
        assert_eq!(
            params["properties"]["resume_after_ms"]["maximum"],
            2147483647i64
        );
    }

    #[test]
    fn unknown_actions_and_missing_fields_are_errors() {
        assert!(run_goal(&json!({"action": "nope"})).is_err());
        assert_eq!(
            run_goal(&json!({"action": "start"})).unwrap_err(),
            "goal objective is required"
        );
        assert!(run_goal(&json!({"action": "update"})).is_err());
        assert!(run_goal(&json!({"action": "limit"})).is_err());
        assert!(run_goal(&json!({"action": "update", "status": "blocked"})).is_err());
        assert!(run_complete(&json!({})).is_err());
        assert!(run_complete(&json!({"goal_id": " "})).is_err());
        assert!(run_blocked(&json!({"goal_id": "x"})).is_err());
        assert!(run_wait(&json!({"goal_id": "x"})).is_err());
    }
}
