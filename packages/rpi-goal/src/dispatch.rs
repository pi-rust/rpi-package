//! Continuation dispatcher.
//!
//! One process-wide worker serializes every Goal-owned automatic run:
//!
//! * `AgentSettled` only *wakes* the worker. Sending from the dispatch thread
//!   would block the event fan-out, and `SendMessage` blocks for the whole run.
//! * The worker re-reads state after each dispatch, so a pause, replacement, or
//!   completion that happened during the run wins over the stale intent.
//! * `SendMessage {triggerTurn: true}` is the reservation primitive: an idle lane
//!   starts a run and blocks, a busy lane queues the message into the live run.
//!   rpi has no `isIdle()` equivalent, and it does not need one.
//! * State is only read and queued under [`state::lock`]. Host calls happen after
//!   the lock is released, so a re-entrant callback cannot deadlock.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rpi_plugin_sdk::RuntimeActionId;
use serde_json::json;

use crate::inject;
use crate::policy;
use crate::prompts;
use crate::runtime;
use crate::settings;
use crate::state::{self, Goal, Goals, Status};

/// Set while a Goal-owned automatic run is in flight. Turn accounting reads it
/// to distinguish automatic work from user work.
static AUTOMATIC: AtomicBool = AtomicBool::new(false);

/// The rejection is reported once. It is not latched into "never try again":
/// a stop is rare, and a host that gains the action (or a reload) must not stay
/// permanently degraded because of one early failure.
static ABORT_WARNED: AtomicBool = AtomicBool::new(false);

/// Cancel the in-flight run so a stop takes effect now instead of after the
/// current turn. Hosts without the action reject it; the goal then stops at the
/// next boundary exactly as before.
pub fn request_abort() {
    if crate::is_subagent() || !crate::events::is_run_active() {
        return;
    }
    let Some(runtime) = runtime::snapshot() else {
        return;
    };
    // The action runs asynchronously in the host and reclaims the queue; sending
    // it from this thread would block the caller that is trying to stop the goal.
    let _ = std::thread::Builder::new()
        .name("rpi-goal-abort".into())
        .spawn(move || {
            let _scope = runtime::enter(Some(runtime));
            if let Err(error) =
                crate::call_runtime_raw(crate::raw_actions::ABORT, json!({}))
            {
                if !ABORT_WARNED.swap(true, Ordering::SeqCst) {
                    eprintln!(
                        "rpi-goal: this host cannot abort the current run ({error}); a stop takes effect at the next turn boundary"
                    );
                }
            }
        });
}

pub fn is_automatic() -> bool {
    AUTOMATIC.load(Ordering::SeqCst)
}

struct Worker {
    inner: Mutex<Inner>,
    ready: Condvar,
}

struct Inner {
    /// Level-triggered wake counter. The worker re-evaluates state on every
    /// wake, so a coalesced or repeated signal cannot dispatch twice.
    seq: u64,
    shutdown: bool,
}

fn worker() -> &'static Worker {
    static WORKER: OnceLock<Worker> = OnceLock::new();
    WORKER.get_or_init(|| Worker {
        inner: Mutex::new(Inner {
            seq: 0,
            shutdown: false,
        }),
        ready: Condvar::new(),
    })
}

/// Start the worker if it is not running yet. Safe to call from any thread.
pub fn ensure_started() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("rpi-goal-worker".into())
            .spawn(run);
        if let Err(error) = spawned {
            eprintln!("rpi-goal: unable to start the goal worker: {error}");
        }
    });
}

/// Ask the worker to re-evaluate: a settle boundary, a goal mutation, a wait
/// deadline change, or new user input.
pub fn wake() {
    let worker = worker();
    let mut inner = worker.inner.lock().unwrap_or_else(|p| p.into_inner());
    inner.seq = inner.seq.wrapping_add(1);
    worker.ready.notify_all();
}

pub fn shutdown() {
    let worker = worker();
    let mut inner = worker.inner.lock().unwrap_or_else(|p| p.into_inner());
    inner.shutdown = true;
    worker.ready.notify_all();
}

fn run() {
    let worker = worker();
    let mut seen = 0u64;
    let mut deadline: Option<Instant> = None;
    loop {
        let mut inner = worker.inner.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if inner.shutdown {
                return;
            }
            if inner.seq != seen {
                seen = inner.seq;
                break;
            }
            match deadline {
                None => {
                    inner = worker.ready.wait(inner).unwrap_or_else(|p| p.into_inner());
                }
                Some(at) => {
                    let now = Instant::now();
                    if now >= at {
                        break;
                    }
                    let (guard, timeout) = worker
                        .ready
                        .wait_timeout(inner, at.saturating_duration_since(now))
                        .unwrap_or_else(|p| p.into_inner());
                    inner = guard;
                    if timeout.timed_out() {
                        break;
                    }
                }
            }
        }
        drop(inner);
        deadline = step();
    }
}

enum Plan {
    Idle,
    Wait(Instant),
    /// The prompt to send. `Some` overrides the standard continuation with the
    /// verifier-directed next action queued by `_verify-result`.
    Send(Box<Goal>, u64, Option<String>),
    Stop(Status, String, Option<&'static str>),
}

/// One evaluation pass. Returns the next absolute deadline the worker should
/// wake itself for (a `goal_wait` safety wake), if any.
fn step() -> Option<Instant> {
    if runtime::snapshot().is_none() || crate::is_subagent() {
        return None;
    }
    // Read host-backed state and settings before taking the mutation lock. A
    // mutation that lands while we are reading (or before the lock below) makes
    // this snapshot stale; the next wake will retry from fresh state.
    let before = state::revision();
    let goals = state::read_lenient();
    let after = state::revision();
    if before != after {
        return None;
    }
    let settings = settings::load();
    let tools_available = required_tools_available();
    let plan = {
        let _guard = state::lock();
        if state::revision() != after {
            return None;
        }
        evaluate(goals, settings, tools_available)
    };
    match plan {
        Plan::Idle => None,
        Plan::Wait(at) => Some(at),
        Plan::Stop(status, reason, cause) => {
            stop(status, &reason, cause);
            None
        }
        Plan::Send(goal, revision, verdict_prompt) => {
            // A pause or replacement may have committed after `evaluate`
            // released the state lock. Check again before touching the host so
            // a stale plan cannot start a run for a stopped or replaced goal.
            if !dispatch_is_current(&goal, revision) {
                return None;
            }
            state::flush();
            inject::flush();
            if !dispatch_is_current(&goal, revision) {
                return None;
            }
            dispatch(&goal, revision, verdict_prompt);
            None
        }
    }
}

/// Decide what to do next from a snapshot read before the state lock. It only
/// mutates the in-memory snapshot and queues the write; host calls stay outside.
fn evaluate(goals: Goals, settings: settings::Settings, tools_available: bool) -> Plan {
    let Some(mut goal) = goals.active.clone() else {
        return Plan::Idle;
    };
    let now = state::now_ms();

    if !goal.status.is_active() {
        return Plan::Idle;
    }

    // Waiting: only an elapsed safety deadline wakes the goal. External input is
    // handled by `BeforeAgentStart`, which clears the wait.
    if let Some(waiting) = goal.waiting.clone() {
        match waiting.deadline_ms {
            Some(deadline) if now >= deadline => {}
            Some(deadline) => {
                let remaining = Duration::from_millis((deadline - now).max(0) as u64);
                return Plan::Wait(Instant::now() + remaining);
            }
            None => return Plan::Idle,
        }
    }

    if let Some(reason) = policy::exhausted_budget(&goal, now) {
        return Plan::Stop(Status::BudgetLimited, reason, None);
    }

    if let Some(reason) = policy::safety_pause(&goal, &settings) {
        let cause = if reason.starts_with("no progress") {
            "no_progress"
        } else {
            "continuation_limit"
        };
        return Plan::Stop(Status::Paused, reason, Some(cause));
    }

    if !tools_available {
        return Plan::Stop(
            Status::Paused,
            "goal_complete is not in the active tool set".into(),
            None,
        );
    }

    // Never inject into a live run: wait for its settled boundary.
    if crate::events::is_run_active() {
        return Plan::Idle;
    }

    goal.waiting = None;
    goal.iteration = goal.iteration.saturating_add(1);
    goal.updated_at = now;
    goal.last_outcome = None;
    if goal.active_since.is_none() {
        goal.start_clock(now);
    }
    // A queued verifier next action replaces the generic continuation prompt.
    // It stays in state until the run's fold-back clears it, so a stale plan
    // (a pause that lands first) cannot drop it.
    let verdict_prompt = goal.pending_next_action.clone();
    let mut next = goals.clone();
    next.active = Some(goal.clone());
    let revision = state::queue(&next);
    Plan::Send(Box::new(goal), revision, verdict_prompt)
}

/// Whether a plan created by `evaluate` is still the current intent. The
/// revision check distinguishes a pause/replacement from the state snapshot
/// that produced the plan; the id/status check keeps a stale plan harmless when
/// a host read only exposes committed state.
fn dispatch_is_current(goal: &Goal, revision: u64) -> bool {
    let goals = state::read_lenient();
    goals
        .active
        .as_ref()
        .is_some_and(|active| active.id == goal.id && active.status == Status::Active)
        && goals.revision == revision
}

/// Send one continuation and fold the outcome back into state. The call blocks
/// until the run ends, so the settlement that follows is already queued.
fn dispatch(goal: &Goal, revision: u64, verdict_prompt: Option<String>) {
    if !dispatch_is_current(goal, revision) {
        return;
    }
    let first = goal.iteration <= 1;
    let served_verdict_prompt = verdict_prompt.is_some();
    let prompt = match verdict_prompt {
        Some(action) => {
            let verdict =
                goal.verifications
                    .last()
                    .cloned()
                    .unwrap_or(crate::state::GoalVerification {
                        outcome: crate::state::VerdictOutcome::NotSatisfied,
                        reason: None,
                        next_action: Some(action),
                        at_ms: state::now_ms(),
                    });
            prompts::verdict_continuation(goal, &verdict)
        }
        None if first => prompts::kickoff(goal),
        None => prompts::continuation(goal, goal.iteration),
    };
    AUTOMATIC.store(true, Ordering::SeqCst);
    let result = crate::call_runtime(
        RuntimeActionId::SendMessage,
        json!({"text": prompt, "triggerTurn": true}),
    );
    AUTOMATIC.store(false, Ordering::SeqCst);

    // A stop (pause, budget, safety, usage limit) may have aborted this run while
    // it was in flight; that state is already committed and needs no fold-back.
    if !state::read_lenient()
        .active
        .as_ref()
        .map(|active| active.id == goal.id && active.status == Status::Active)
        .unwrap_or(false)
    {
        return;
    }

    match result {
        Ok(value) => {
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match status.as_str() {
                "aborted" => stop_current(&goal.id, Status::Paused, "the run was aborted", None),
                "error" | "suspended" => {
                    let reason = value
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("the provider request failed");
                    wait_current(&goal.id, policy::provider_waiting(reason));
                }
                other => {
                    // Read-modify-write in one critical section: a stale clone
                    // carried across two sections could overwrite a newer state.
                    let queued = {
                        let _guard = state::lock();
                        let mut goals = state::read_lenient();
                        match goals.active.clone() {
                            Some(active) if active.id == goal.id => {
                                let mut active = active;
                                active.last_outcome = Some(other.to_string());
                                active.updated_at = state::now_ms();
                                // The verifier-directed run has been served; a
                                // completed/aborted/errored run keeps it queued
                                // so `/goal resume` re-runs it.
                                if served_verdict_prompt {
                                    active.pending_next_action = None;
                                }
                                goals.active = Some(active);
                                state::queue(&goals);
                                true
                            }
                            _ => false,
                        }
                    };
                    if queued {
                        state::flush();
                    }
                }
            }
        }
        Err(error) => {
            // The bridge itself failed. Do not spin: wait for the next signal.
            eprintln!("rpi-goal: continuation failed: {error}");
        }
    }
}

/// Whether `goal_complete` is reachable. A host without `GetActiveTools` (or a
/// plugin-visible failure) must not stall the goal.
fn required_tools_available() -> bool {
    let Ok(value) = crate::call_runtime(RuntimeActionId::GetActiveTools, json!({})) else {
        return true;
    };
    let Some(tools) = value.get("tools").and_then(|v| v.as_array()) else {
        return true;
    };
    tools
        .iter()
        .filter_map(|tool| tool.as_str())
        .any(|tool| tool == "goal_complete")
}

/// Stop the goal at the boundary the worker just observed. Re-reads state so a
/// concurrent replacement wins.
pub fn stop(status: Status, reason: &str, cause: Option<&str>) {
    let goals = state::read_lenient();
    let Some(goal) = goals.active.clone() else {
        return;
    };
    stop_current(&goal.id, status, reason, cause);
}

/// Stop `expected_id` only. A stale id (the goal was replaced or cleared) is a
/// no-op, so a delayed continuation cannot pause a newer goal.
pub fn stop_current(expected_id: &str, status: Status, reason: &str, cause: Option<&str>) {
    let stopped = {
        let _guard = state::lock();
        let mut goals = state::read_lenient();
        let Some(mut goal) = goals.active.clone() else {
            return;
        };
        if goal.id != expected_id {
            return;
        }
        let now = state::now_ms();
        goal.status = status;
        goal.safety_pause_cause = cause.map(|c| c.to_string());
        goal.waiting = None;
        goal.updated_at = now;
        goal.stop_clock(now);
        goals.active = Some(goal.clone());
        state::queue(&goals);
        goal
    };
    let notice = prompts::stop_notice(&stopped, reason);
    inject::queue(&stopped, Some(notice));
    state::flush();
    inject::flush();
    crate::status::publish(&stopped);
    // The stop is now committed: end the run that was still working instead of
    // letting it finish work nobody asked for.
    request_abort();
}

pub fn wait_current(expected_id: &str, waiting: crate::state::Waiting) {
    let goal = {
        let _guard = state::lock();
        let mut goals = state::read_lenient();
        let Some(mut goal) = goals.active.clone() else {
            return;
        };
        if goal.id != expected_id {
            return;
        }
        let now = state::now_ms();
        goal.updated_at = now;
        goal.waiting = Some(waiting.clone());
        goal.stop_clock(now);
        goals.active = Some(goal.clone());
        state::queue(&goals);
        goal
    };
    let notice = prompts::waiting_notice(&goal, &waiting);
    inject::queue(&goal, Some(notice));
    state::flush();
    inject::flush();
    crate::status::publish(&goal);
}

/// Compact `active` view used by the tests and the status line.
pub fn is_running() -> bool {
    let _guard = state::lock();
    let state: Goals = state::read_lenient();
    state.active.is_some()
}
