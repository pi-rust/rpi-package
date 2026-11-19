//! Goal state transitions shared by the `goal` tool and the `/goal` command.
//!
//! Every operation persists the new state, queues the model-visible notice, and
//! wakes the continuation worker. None of them sends a message directly.
use crate::dispatch;
use crate::inject;
use crate::state::{self, Goal, GoalVerification, Goals, Status, VerdictOutcome};
use crate::status;

pub fn require_active(goals: &Goals) -> Result<Goal, String> {
    goals
        .active
        .clone()
        .ok_or_else(|| "no active goal — start one first".to_string())
}

/// Validate the caller's goal id against the active goal under the same state
/// lock that performs the terminal transition. Keeping this check in `ops`
/// closes the gap between a tool-side read and a later replacement.
pub fn require_current(goal_id: &str, goals: &Goals) -> Result<Goal, String> {
    let goal_id = goal_id.trim();
    if goal_id.is_empty() {
        return Err("goal_id is required; use the id from the current goal prompt".into());
    }
    let goal = require_active(goals)?;
    if goal.id != goal_id {
        return Err(
            "stale goal_id: it does not match the current active goal; read the latest goal prompt"
                .into(),
        );
    }
    Ok(goal)
}

pub fn validate_title(title: &str) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("goal objective is required".into());
    }
    if title.chars().count() > state::MAX_TITLE_CHARS {
        return Err(format!(
            "objective must not exceed {} characters",
            state::MAX_TITLE_CHARS
        ));
    }
    Ok(title.to_string())
}

pub fn validate_notes(notes: &str) -> Result<String, String> {
    let notes = notes.trim();
    if notes.chars().count() > state::MAX_NOTES_CHARS {
        return Err(format!(
            "notes must not exceed {} characters",
            state::MAX_NOTES_CHARS
        ));
    }
    Ok(notes.to_string())
}

pub fn bounded(text: &str, max: usize, field: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} is required"));
    }
    if trimmed.chars().count() > max {
        return Err(format!("{field} must not exceed {max} characters"));
    }
    Ok(trimmed.to_string())
}

/// Queue `goal` as the active goal. Callers hold [`state::lock`] so the queued
/// write cannot be overwritten by a stale snapshot from another thread.
fn queue_active(goals: &Goals, goal: &Goal) {
    let mut goals = goals.clone();
    goals.active = Some(goal.clone());
    state::queue(&goals);
}

/// Flush the queued state and notice, then wake the continuation worker. Runs
/// outside the state lock: `flush` performs host calls.
fn finish(goal: &Goal, notice: Option<String>) -> Result<Goal, String> {
    inject::queue(goal, notice);
    state::flush();
    inject::flush();
    status::publish(goal);
    dispatch::ensure_started();
    dispatch::wake();
    Ok(goal.clone())
}

pub struct StartOptions {
    pub title: String,
    pub notes: String,
    pub max_turns: Option<u32>,
    pub deadline_ms: Option<i64>,
    pub token_budget: Option<u64>,
}

pub fn start(options: StartOptions) -> Result<Goal, String> {
    let now = state::now_ms();
    let mut goal = Goal::new(
        validate_title(&options.title)?,
        validate_notes(&options.notes)?,
        now,
    );
    goal.max_turns = options.max_turns.filter(|n| *n > 0);
    goal.deadline_ms = options.deadline_ms.filter(|ms| *ms > 0).map(|ms| now + ms);
    goal.token_budget = options.token_budget.filter(|n| *n > 0);
    {
        let _guard = state::lock();
        let mut goals = state::read()?;
        if let Some(mut old) = goals.active.take() {
            if old.status.is_active() {
                old.stop_clock(now);
            }
            old.updated_at = now;
            goals.archived.push(old);
        }
        queue_active(&goals, &goal);
    }
    finish(&goal, None)
}

/// Notes and/or status for the active goal. Both are optional; at least one must
/// be present. An empty note string clears the notes.
pub fn update(notes: Option<&str>, new_status: Option<Status>) -> Result<Goal, String> {
    if notes.is_none() && new_status.is_none() {
        return Err("update requires `notes` and/or `status`".into());
    }
    let (goal, notice) = {
        let _guard = state::lock();
        let goals = state::read()?;
        let mut goal = require_active(&goals)?;
        let now = state::now_ms();
        let mut notice = None;
        if let Some(notes) = notes {
            goal.notes = validate_notes(notes)?;
            notice = Some(crate::prompts::objective_updated(&goal));
        }
        if let Some(new_status) = new_status {
            if new_status != Status::Active && new_status != Status::Paused {
                return Err("status must be \"active\" or \"paused\"".into());
            }
            apply_status(&mut goal, new_status, now);
        }
        goal.updated_at = now;
        queue_active(&goals, &goal);
        (goal, notice)
    };
    let result = finish(&goal, notice);
    if goal.status.is_stopped() {
        dispatch::request_abort();
    }
    result
}

fn apply_status(goal: &mut Goal, new_status: Status, now: i64) {
    match new_status {
        Status::Active => {
            if goal.status.is_stopped() {
                goal.id = state::new_goal_id();
                goal.automatic_turns = 0;
                goal.no_progress_count = 0;
                goal.last_output_fingerprint = None;
                goal.safety_pause_cause = None;
            }
            goal.status = Status::Active;
            goal.waiting = None;
            goal.start_clock(now);
        }
        _ => {
            goal.status = new_status;
            goal.stop_clock(now);
        }
    }
}

pub fn pause() -> Result<Goal, String> {
    set_status(Status::Paused)
}

pub fn resume() -> Result<Goal, String> {
    set_status(Status::Active)
}

pub fn set_status(new_status: Status) -> Result<Goal, String> {
    let (goal, notice) = {
        let _guard = state::lock();
        let goals = state::read()?;
        let mut goal = require_active(&goals)?;
        let now = state::now_ms();
        let was_waiting = goal.waiting.clone();
        apply_status(&mut goal, new_status, now);
        goal.updated_at = now;
        let notice = match (new_status, was_waiting) {
            (Status::Active, Some(waiting)) => {
                Some(crate::prompts::wait_resume(&goal, &waiting.reason))
            }
            _ => None,
        };
        queue_active(&goals, &goal);
        (goal, notice)
    };
    let result = finish(&goal, notice);
    if goal.status.is_stopped() {
        // `/goal pause` (or an update to `paused`) ends the live run now.
        dispatch::request_abort();
    }
    result
}

/// Archive the active goal. Used by `/goal stop|done|complete` and by the
/// legacy `goal` tool action; the model path is `goal_complete`.
pub fn complete() -> Result<Goal, String> {
    let goal = {
        let _guard = state::lock();
        let mut goals = state::read()?;
        let mut goal = require_active(&goals)?;
        let now = state::now_ms();
        goal.status = Status::Complete;
        goal.updated_at = now;
        goal.stop_clock(now);
        goal.waiting = None;
        goals.archived.push(goal.clone());
        goals.active = None;
        state::queue(&goals);
        goal
    };
    finish(&goal, None)
}

/// Drop the goal entirely without archiving it.
pub fn clear() -> Result<Option<Goal>, String> {
    inject::discard();
    state::remove_legacy_file();
    let removed = {
        let _guard = state::lock();
        let mut goals = state::read()?;
        let removed = goals.active.take();
        let now = state::now_ms();
        if let Some(mut goal) = removed.clone() {
            goal.stop_clock(now);
            goal.updated_at = now;
        }
        state::queue(&goals);
        removed
    };
    state::flush();
    status::clear();
    dispatch::wake();
    Ok(removed)
}

/// Merge budget changes. `None` leaves a field untouched, `Some(None)` clears it,
/// and `Some(Some(v))` sets it (a relative ms value for the deadline).
pub fn set_limits(
    max_turns: Option<Option<u32>>,
    deadline_ms: Option<Option<i64>>,
    token_budget: Option<Option<u64>>,
) -> Result<Goal, String> {
    let goal = {
        let _guard = state::lock();
        let goals = state::read()?;
        let mut goal = require_active(&goals)?;
        let now = state::now_ms();
        if let Some(max_turns) = max_turns {
            goal.max_turns = max_turns.filter(|n| *n > 0);
        }
        if let Some(deadline_ms) = deadline_ms {
            goal.deadline_ms = match deadline_ms {
                Some(ms) if ms > 0 => Some(now.saturating_add(ms)),
                _ => None,
            };
        }
        if let Some(token_budget) = token_budget {
            goal.token_budget = token_budget.filter(|n| *n > 0);
        }
        goal.updated_at = now;
        queue_active(&goals, &goal);
        goal
    };
    finish(&goal, None)
}

/// `goal_complete`: the model's evidence-based termination. Requires the exact
/// active goal id and a non-empty summary; stopped goals cannot complete.
pub fn complete_with(goal_id: &str, summary: &str) -> Result<Goal, String> {
    let summary = bounded(summary, state::MAX_SUMMARY_CHARS, "completion summary")?;
    let goal = {
        let _guard = state::lock();
        let mut goals = state::read()?;
        let mut goal = require_current(goal_id, &goals)?;
        if goal.status.is_stopped() {
            return Err(format!(
                "the goal is {}; resume it before completing",
                goal.status.as_str()
            ));
        }
        let now = state::now_ms();
        goal.status = Status::Complete;
        goal.updated_at = now;
        goal.stop_clock(now);
        goal.waiting = None;
        goal.last_outcome = Some("completed".into());
        goals.archived.push(goal.clone());
        goals.active = None;
        state::queue(&goals);
        goal
    };
    finish(
        &goal,
        Some(crate::prompts::completion_summary(&goal, &summary)),
    )
}

/// `goal_blocked`: a repeated, evidenced impasse.
pub fn block(
    goal_id: &str,
    reason: &str,
    evidence: &str,
    repeated_turns: u32,
) -> Result<Goal, String> {
    let reason = bounded(reason, state::MAX_REASON_CHARS, "blocker reason")?;
    let evidence = bounded(evidence, state::MAX_EVIDENCE_CHARS, "blocker evidence")?;
    if repeated_turns < 3 {
        return Err("blocked requires the same blocker across at least 3 turns".into());
    }
    let goal = {
        let _guard = state::lock();
        let mut goals = state::read()?;
        let mut goal = require_current(goal_id, &goals)?;
        if goal.status.is_stopped() {
            return Err(format!("the goal is already {}", goal.status.as_str()));
        }
        let now = state::now_ms();
        goal.status = Status::Blocked;
        goal.updated_at = now;
        goal.stop_clock(now);
        goal.waiting = None;
        goals.archived.push(goal.clone());
        goals.active = None;
        state::queue(&goals);
        goal
    };
    finish(
        &goal,
        Some(crate::prompts::blocked_summary(
            &goal,
            &reason,
            &evidence,
            repeated_turns,
        )),
    )
}

/// `goal_wait`: quiet the goal until an external wake or a safety deadline.
pub fn wait(goal_id: &str, reason: &str, resume_after_ms: Option<i64>) -> Result<Goal, String> {
    let reason = bounded(reason, state::MAX_REASON_CHARS, "wait reason")?;
    let (goal, notice) = {
        let _guard = state::lock();
        let goals = state::read()?;
        let mut goal = require_current(goal_id, &goals)?;
        if goal.status.is_stopped() {
            return Err(format!(
                "the goal is {}; resume it before waiting",
                goal.status.as_str()
            ));
        }
        let now = state::now_ms();
        let deadline_ms =
            resume_after_ms.map(|ms| now + ms.clamp(state::MIN_WAIT_MS, state::MAX_WAIT_MS));
        let waiting = state::Waiting {
            reason,
            kind: state::WaitKind::External,
            deadline_ms,
        };
        goal.waiting = Some(waiting.clone());
        goal.updated_at = now;
        goal.stop_clock(now);
        queue_active(&goals, &goal);
        let notice = crate::prompts::waiting_notice(&goal, &waiting);
        (goal, notice)
    };
    finish(&goal, Some(notice))
}

/// Host-facing completion verification. The plugin only *records* the verdict
/// and queues the verifier's next action; the seven statuses stay untouched.
/// A rejected goal that was archived (complete/blocked) is reopened: it moves
/// back to the active slot with a fresh id, and the existing continuation
/// worker dispatches the next action at the next wake — never into a live run.
pub struct VerdictInput {
    pub goal_id: Option<String>,
    pub outcome: VerdictOutcome,
    pub reason: Option<String>,
    pub next_action: Option<String>,
}

fn bounded_optional(text: &Option<String>, field: &str) -> Result<Option<String>, String> {
    match text.as_deref() {
        None => Ok(None),
        Some(text) if text.trim().is_empty() => Ok(None),
        Some(text) => Ok(Some(bounded(text, state::MAX_VERIFICATION_CHARS, field)?)),
    }
}

pub fn record_verification(input: VerdictInput) -> Result<Goal, String> {
    let reason = bounded_optional(&input.reason, "verification reason")?;
    let next_action = match input.outcome {
        VerdictOutcome::Pass => {
            if input
                .next_action
                .as_deref()
                .is_some_and(|a| !a.trim().is_empty())
            {
                return Err(
                    "next_action belongs to a notSatisfied verdict; a pass records nothing to run"
                        .into(),
                );
            }
            None
        }
        VerdictOutcome::NotSatisfied => Some(bounded(
            input.next_action.as_deref().unwrap_or(""),
            state::MAX_VERIFICATION_CHARS,
            "next_action",
        )?),
    };
    let goal_id = input
        .goal_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);

    let (goal, notice) = {
        let _guard = state::lock();
        let mut goals = state::read()?;
        let now = state::now_ms();
        let verdict = GoalVerification {
            outcome: input.outcome,
            reason,
            next_action: next_action.clone(),
            at_ms: now,
        };

        // Resolve the goal under review. An active goal is validated against
        // goal_id when the host passes one; an archived goal (the model
        // completed or blocked it) is reopened by removing it from the archive.
        if let Some(active) = goals.active.clone() {
            if let Some(id) = &goal_id {
                if active.id != *id {
                    return Err(format!(
                        "stale goal_id `{id}`: it does not match the current active goal; read `/goal status`"
                    ));
                }
            }
            let mut goal = active;
            goal.push_verification(verdict.clone());
            match input.outcome {
                VerdictOutcome::Pass => {
                    // A pass supersedes any queued rejection direction.
                    goal.pending_next_action = None;
                }
                VerdictOutcome::NotSatisfied => {
                    goal.pending_next_action = next_action;
                    if goal.status == Status::Waiting {
                        // The verdict *is* the external input the goal waited for.
                        goal.status = Status::Active;
                        goal.waiting = None;
                        goal.start_clock(now);
                    }
                }
            }
            goal.updated_at = now;
            let notice = crate::prompts::verdict_notice(&goal, &verdict);
            queue_active(&goals, &goal);
            (goal, notice)
        } else {
            let Some(id) = &goal_id else {
                return Err("no active goal — pass the goal_id of the goal under review".into());
            };
            let Some(position) = goals.archived.iter().rposition(|goal| &goal.id == id) else {
                return Err(format!("no goal matches goal_id `{id}`"));
            };
            let mut goal = goals.archived.remove(position);
            goal.push_verification(verdict.clone());
            match input.outcome {
                VerdictOutcome::Pass => {
                    // Stay archived; the completion claim is accepted.
                    goal.updated_at = now;
                    goals.archived.push(goal.clone());
                    let notice = crate::prompts::verdict_notice(&goal, &verdict);
                    state::queue(&goals);
                    (goal, notice)
                }
                VerdictOutcome::NotSatisfied => {
                    // Reopen: fresh id (stale terminal tools and stale verdicts
                    // for the old id are rejected), counters reset like a resume.
                    goal.status = Status::Active;
                    goal.id = state::new_goal_id();
                    goal.automatic_turns = 0;
                    goal.no_progress_count = 0;
                    goal.last_output_fingerprint = None;
                    goal.safety_pause_cause = None;
                    goal.waiting = None;
                    goal.pending_next_action = next_action;
                    goal.updated_at = now;
                    goal.start_clock(now);
                    let notice = crate::prompts::verdict_notice(&goal, &verdict);
                    queue_active(&goals, &goal);
                    (goal, notice)
                }
            }
        }
    };
    finish(&goal, Some(notice))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objective_validation_bounds_and_trims() {
        assert_eq!(validate_title("  ship it  ").unwrap(), "ship it");
        assert!(validate_title("   ").is_err());
        assert!(validate_title(&"x".repeat(state::MAX_TITLE_CHARS + 1)).is_err());
        assert_eq!(validate_notes("").unwrap(), "");
        assert!(validate_notes(&"x".repeat(state::MAX_NOTES_CHARS + 1)).is_err());
    }

    #[test]
    fn bounded_fields_reject_empty_and_long_values() {
        assert!(bounded("  ", 10, "reason").is_err());
        assert!(bounded("ok", 10, "reason").is_ok());
        assert!(bounded("too long", 3, "reason").is_err());
    }
}
