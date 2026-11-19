//! Goal state transitions shared by the `goal` tool and the `/goal` command.
//!
//! Every operation persists the new state, queues the model-visible notice, and
//! wakes the continuation worker. None of them sends a message directly.
use crate::dispatch;
use crate::inject;
use crate::prompts;
use crate::state::{self, Goal, GoalItem, GoalIteration, GoalVerification, Goals, Status};
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
            goal.verify_iteration = None;
            goal.verify_started_at = None;
            // A resumed goal owes the next run (kickoff after a stop, or the
            // continuation after a verdict); the worker consumes this flag.
            goal.pending_dispatch = true;
            goal.start_clock(now);
        }
        _ => {
            goal.status = new_status;
            goal.verify_iteration = None;
            goal.verify_started_at = None;
            goal.pending_dispatch = false;
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

/// Archive the active goal (`/goal stop`). The goal keeps its status in the
/// archived list and the active slot is freed for a new objective.
pub fn archive() -> Result<Goal, String> {
    let goal = {
        let _guard = state::lock();
        let mut goals = state::read()?;
        let mut goal = require_active(&goals)?;
        let now = state::now_ms();
        if !goal.status.is_stopped() {
            goal.status = Status::Complete;
        }
        goal.pending_dispatch = false;
        goal.verify_iteration = None;
        goal.verify_started_at = None;
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

/// Mark the active goal `complete` **in place** (ZCode keeps a finished target
/// visible): the goal stays the active slot's entry with the terminal status so
/// a host panel can render it until the next `start` or `/goal clear`. Automatic
/// continuation stops; archiving is `/goal stop`.
pub fn complete() -> Result<Goal, String> {
    let goal = complete_in_place(None)?;
    finish(
        &goal,
        Some(prompts::completion_summary(
            &goal,
            "The goal was marked complete.",
        )),
    )
}

/// `goal_complete` / `/goal done|complete` with the model's evidence summary.
pub fn complete_with(goal_id: &str, summary: &str) -> Result<Goal, String> {
    let summary = bounded(summary, state::MAX_SUMMARY_CHARS, "completion summary")?;
    let goal = complete_in_place(Some(goal_id))?;
    finish(&goal, Some(prompts::completion_summary(&goal, &summary)))
}

fn complete_in_place(goal_id: Option<&str>) -> Result<Goal, String> {
    let _guard = state::lock();
    let goals = state::read()?;
    let mut goal = match goal_id {
        Some(goal_id) => require_current(goal_id, &goals)?,
        None => require_active(&goals)?,
    };
    if goal.status.is_stopped() {
        return Err(format!(
            "the goal is {}; resume it before completing",
            goal.status.as_str()
        ));
    }
    let now = state::now_ms();
    goal.status = Status::Complete;
    goal.pending_dispatch = false;
    goal.verify_iteration = None;
    goal.verify_started_at = None;
    goal.updated_at = now;
    goal.last_outcome = Some("completed".into());
    goal.stop_clock(now);
    goal.waiting = None;
    queue_active(&goals, &goal);
    Ok(goal)
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
        goal.pending_dispatch = false;
        goal.verify_iteration = None;
        goal.verify_started_at = None;
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

// ---------------------------------------------------------------------------
// Completion verification (host bridge)
// ---------------------------------------------------------------------------

/// Arm or disarm completion verification (`/goal _verify-enable on|off`).
///
/// Armed: the continuation worker refuses to start a goal-owned run until the
/// host reports a verdict for the run that just settled (`_verify-start` /
/// `_verify-result`). Disarmed: the plain v2 continuation loop. The flag lives
/// in `Goals` so it applies to subsequent goals in the same branch.
pub fn set_verification_armed(armed: bool) -> Result<bool, String> {
    let changed = {
        let _guard = state::lock();
        let mut goals = state::read()?;
        let changed = goals.verification != armed;
        if changed {
            goals.verification = armed;
            state::queue(&goals);
        }
        changed
    };
    if changed {
        state::flush();
        // An armed goal may now be waiting for a verdict, or a disarmed one may
        // be free to continue; let the worker re-evaluate either way.
        dispatch::wake();
    }
    Ok(armed)
}

/// Open the verification boundary for the run that just settled. The host hands
/// over the run's todo snapshot; the goal flips to `verifying` and no automatic
/// continuation may start until `verify_result`.
pub fn verify_start(items: Vec<GoalItem>) -> Result<Goal, String> {
    let goal = {
        let _guard = state::lock();
        let goals = state::read()?;
        let mut goal = require_active(&goals)?;
        if !goal.status.is_active() {
            return Err(format!(
                "goal is {} — verification needs an active goal",
                goal.status.as_str()
            ));
        }
        let now = state::now_ms();
        // The row number is the dispatch iteration that just settled; a goal
        // that never dispatched (host without a worker) still gets row 1.
        let iteration = goal.iteration.max(1);
        goal.push_iteration(GoalIteration {
            iteration,
            items: state::bound_items(items),
            updated_at: now,
        });
        goal.status = Status::Verifying;
        goal.verify_iteration = Some(iteration);
        goal.verify_started_at = Some(now);
        goal.pending_dispatch = false;
        goal.updated_at = now;
        queue_active(&goals, &goal);
        goal
    };
    state::flush();
    status::publish(&goal);
    Ok(goal)
}

/// Apply one completion-verifier verdict (`/goal _verify-result`).
///
/// * `pass` → `complete` in place (terminal, visible);
/// * `not_satisfied` → `active` with the continuation owed again; the worker
///   sends the next run with the verdict reason / next action in its prompt;
/// * `failed` / `cancelled` → `paused` with a named `safety_pause_cause`.
pub fn verify_result(
    outcome: &str,
    reason: Option<&str>,
    next_action: Option<&str>,
) -> Result<Goal, String> {
    let outcome = match outcome.trim() {
        "pass" => "pass",
        "notSatisfied" | "not_satisfied" => "not_satisfied",
        "failed" => "failed",
        "cancelled" | "canceled" => "cancelled",
        other => {
            return Err(format!(
                "outcome must be pass | notSatisfied | failed | cancelled (got `{other}`)"
            ))
        }
    };
    let reason = reason.map(|text| {
        text.trim()
            .chars()
            .take(state::MAX_VERIFICATION_REASON_CHARS)
            .collect::<String>()
    });
    let next_action = next_action.map(|text| {
        text.trim()
            .chars()
            .take(state::MAX_VERIFICATION_NEXT_ACTION_CHARS)
            .collect::<String>()
    });
    let (goal, notice) = {
        let _guard = state::lock();
        let goals = state::read()?;
        let mut goal = require_active(&goals)?;
        if !goal.status.is_verifying() {
            return Err(format!(
                "goal is {} — no verification is in flight",
                goal.status.as_str()
            ));
        }
        let now = state::now_ms();
        let iteration = goal
            .verify_iteration
            .unwrap_or_else(|| goal.iteration.max(1));
        let verdict = GoalVerification {
            iteration,
            outcome: outcome.to_string(),
            at: now,
            reason: reason.filter(|text| !text.is_empty()),
            next_action: next_action.filter(|text| !text.is_empty()),
        };
        goal.push_verification(verdict.clone());
        goal.verify_iteration = None;
        goal.verify_started_at = None;
        let notice = match outcome {
            "pass" => {
                goal.status = Status::Complete;
                goal.pending_dispatch = false;
                goal.last_outcome = Some("completed".into());
                goal.stop_clock(now);
                goal.waiting = None;
                Some(prompts::verified_notice(&goal, &verdict))
            }
            "not_satisfied" => {
                goal.status = Status::Active;
                goal.pending_dispatch = true;
                goal.last_outcome = Some("not_satisfied".into());
                goal.start_clock(now);
                None
            }
            "failed" => {
                goal.status = Status::Paused;
                goal.pending_dispatch = false;
                goal.safety_pause_cause = Some("verification_failed".into());
                goal.last_outcome = Some("verification_failed".into());
                goal.stop_clock(now);
                Some(prompts::stop_notice(
                    &goal,
                    &format!(
                        "the completion verifier failed{}",
                        verdict
                            .reason
                            .as_deref()
                            .map(|reason| format!(": {reason}"))
                            .unwrap_or_default()
                    ),
                ))
            }
            _ => {
                goal.status = Status::Paused;
                goal.pending_dispatch = false;
                goal.safety_pause_cause = Some("verification_cancelled".into());
                goal.last_outcome = Some("verification_cancelled".into());
                goal.stop_clock(now);
                Some(prompts::stop_notice(
                    &goal,
                    "no completion verdict was delivered",
                ))
            }
        };
        goal.updated_at = now;
        queue_active(&goals, &goal);
        (goal, notice)
    };
    finish(&goal, notice).map(|_| goal)
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
