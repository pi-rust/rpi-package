//! Event handlers.
//!
//! Every handler only *accounts* and *signals*: none of them performs a blocking
//! host call. Continuation dispatch belongs to the worker thread, because
//! `SendMessage` blocks until the run finishes.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use rpi_plugin_sdk::EventTag;

use crate::dispatch;
use crate::inject;
use crate::policy;
use crate::runtime;
use crate::state::{self, Status};
use crate::status;

/// Did this run attempt a tool? Attempted tools reset the no-progress heuristic.
static TOOL_ATTEMPTED: AtomicBool = AtomicBool::new(false);

/// Is a main-lane run in flight? The worker waits for the settled boundary
/// instead of injecting a continuation into the live run. Hosts that do not
/// deliver `AgentStart`/`AgentEnd` leave this false, and the continuation then
/// rides the host's own follow-up queue instead of being lost.
static RUN_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn is_run_active() -> bool {
    RUN_ACTIVE.load(Ordering::SeqCst)
}

/// Last assistant message text of the current turn, used only to compute the
/// no-progress fingerprint. Cleared on every `turn_end`.
static LAST_TEXT: Mutex<Option<String>> = Mutex::new(None);

/// Assistant-message timestamp of the last accounted response. `MessageEnd` and
/// `AfterProviderResponse` can describe the same response on hosts that deliver
/// both; accounting it twice would double a token budget.
static LAST_ACCOUNTED: Mutex<Option<i64>> = Mutex::new(None);

fn last_text() -> &'static Mutex<Option<String>> {
    &LAST_TEXT
}

/// Record one finished turn against the goal, then let the next settled boundary
/// decide whether work continues.
pub fn account_turn_end(output: &str, used_tool: bool) {
    inject::flush();
    state::flush();
    if crate::is_subagent() {
        return;
    }
    let automatic = dispatch::is_automatic();
    let goal = {
        let _guard = state::lock();
        let mut goals = state::read_lenient();
        let Some(mut goal) = goals.active.clone() else {
            return;
        };
        if !goal.status.is_active() {
            return;
        }
        let now = state::now_ms();
        goal.turns_used = goal.turns_used.saturating_add(1);
        goal.updated_at = now;
        if automatic {
            goal.automatic_turns = goal.automatic_turns.saturating_add(1);
            if used_tool {
                policy::record_tool_attempt(&mut goal);
            } else {
                policy::record_tool_free_output(&mut goal, output);
            }
        }
        if goal.active_since.is_none() {
            goal.start_clock(now);
        }
        goals.active = Some(goal.clone());
        state::queue(&goals);
        goal
    };

    let now = state::now_ms();
    state::flush();
    if let Some(reason) = policy::exhausted_budget(&goal, now) {
        dispatch::stop_current(&goal.id, Status::BudgetLimited, &reason, None);
        return;
    }
    let settings = crate::settings::load();
    if let Some(reason) = policy::safety_pause(&goal, &settings) {
        let cause = if reason.starts_with("no progress") {
            "no_progress"
        } else {
            "continuation_limit"
        };
        dispatch::stop_current(&goal.id, Status::Paused, &reason, Some(cause));
        return;
    }

    // A token budget that no host report can ever fill is a lie in the status
    // line. Say so once instead of silently never enforcing it.
    if goal.token_budget.is_some() && goal.usage_seen == 0 && !goal.usage_unavailable {
        let mut goals = state::read_lenient();
        if let Some(mut active) = goals.active.clone() {
            if active.id == goal.id {
                active.usage_unavailable = true;
                active.updated_at = state::now_ms();
                goals.active = Some(active.clone());
                if state::persist(&goals).is_ok() {
                    inject::queue(
                        &active,
                        Some(format!(
                            "⚠️ Token budget cannot be enforced: this host is not reporting provider usage to extensions. The turn, time, and continuation limits still apply ({}).",
                            active.budget_summary(state::now_ms())
                        )),
                    );
                    inject::flush();
                }
                status::publish(&active);
            }
        }
        return;
    }

    status::publish(&goal);
    if inject::should_inject(&goal) && !inject::has_pending() {
        inject::queue(&goal, None);
        inject::flush();
    }
}

/// Fold one assistant response into token usage and interruption classification.
pub fn account_response(value: &serde_json::Value) {
    if crate::is_subagent() {
        return;
    }
    if !policy::is_assistant(value) {
        return;
    }
    // One response, one accounting pass: `MessageEnd` is the reliable carrier on
    // every host, `AfterProviderResponse` is an optional duplicate.
    if let Some(timestamp) = value.get("timestamp").and_then(|v| v.as_i64()) {
        let mut last = LAST_ACCOUNTED.lock().unwrap_or_else(|p| p.into_inner());
        if *last == Some(timestamp) {
            return;
        }
        *last = Some(timestamp);
    }
    let tokens = policy::response_tokens(value);
    let class = policy::classify_response(value);
    let error = policy::error_message(value).to_string();

    let goal = {
        let _guard = state::lock();
        let mut goals = state::read_lenient();
        let Some(mut goal) = goals.active.clone() else {
            return;
        };
        if !goal.status.is_active() && goal.status != Status::Waiting {
            return;
        }
        goal.tokens_used = goal.tokens_used.saturating_add(tokens);
        goal.usage_seen = goal.usage_seen.saturating_add(1);
        goal.usage_unavailable = false;
        goal.updated_at = state::now_ms();
        goals.active = Some(goal.clone());
        state::queue(&goals);
        goal
    };
    state::flush();

    match class {
        policy::ResponseClass::UsageLimited => {
            let reason = if error.is_empty() {
                "the provider reported a usage limit".to_string()
            } else {
                format!("usage limit: {error}")
            };
            dispatch::stop_current(&goal.id, Status::UsageLimited, &reason, None);
        }
        policy::ResponseClass::ProviderError => {
            let reason = if error.is_empty() {
                "the provider request failed".to_string()
            } else {
                error
            };
            dispatch::wait_current(&goal.id, policy::provider_waiting(&reason));
        }
        policy::ResponseClass::Ok | policy::ResponseClass::Length => {}
    }
}

/// A new run started. Any wait is over: whatever woke the goal is now working.
pub fn wake_goal() {
    let woke = {
        let _guard = state::lock();
        let mut goals = state::read_lenient();
        let Some(mut goal) = goals.active.clone() else {
            return;
        };
        if goal.waiting.is_none() {
            return;
        }
        let now = state::now_ms();
        goal.waiting = None;
        goal.updated_at = now;
        goal.start_clock(now);
        goals.active = Some(goal.clone());
        state::queue(&goals);
        goal
    };
    state::flush();
    status::publish(&woke);
    dispatch::wake();
}

/// Compaction keeps the transcript entries but replaces the model-visible
/// context with a summary, so the goal notice must be re-appended.
pub fn after_compaction() {
    if crate::is_subagent() {
        return;
    }
    let goal = {
        let _guard = state::lock();
        let goals = state::read_lenient();
        let Some(goal) = goals.active.clone() else {
            return;
        };
        state::queue(&goals);
        goal
    };
    state::flush();
    inject::queue(&goal, None);
    inject::flush();
    dispatch::wake();
}

/// Checkpoint the active clock and stop automatic work for a shutting-down
/// session. The worker exits on its own once the run is cancelled.
pub fn before_shutdown() {
    state::flush();
    {
        let _guard = state::lock();
        let mut goals = state::read_lenient();
        if let Some(mut goal) = goals.active.clone() {
            goal.stop_clock(state::now_ms());
            goal.updated_at = state::now_ms();
            goals.active = Some(goal);
            state::queue(&goals);
        }
    }
    state::flush();
    dispatch::shutdown();
}

/// Registration list: `(tag, handler)`, shared by the plugin entrypoint and the
/// tests so they cannot drift.
pub fn registrations() -> Vec<(EventTag, rpi_plugin_sdk::EventHandlerFn)> {
    vec![
        (EventTag::SessionStart, on_session_start),
        (EventTag::BeforeAgentStart, on_before_agent_start),
        (EventTag::AgentStart, on_agent_start),
        (EventTag::AgentEnd, on_agent_end),
        (EventTag::AgentSettled, on_agent_settled),
        (EventTag::TurnEnd, on_turn_end),
        (EventTag::AfterProviderResponse, on_after_provider_response),
        (EventTag::ToolCall, on_tool_call),
        (EventTag::MessageEnd, on_message_end),
        (EventTag::SessionBeforeCompact, on_session_compact),
        (EventTag::SessionCompact, on_session_compact),
        (EventTag::SessionBeforeSwitch, on_session_replaced),
        (EventTag::SessionBeforeFork, on_session_replaced),
        (EventTag::SessionTree, on_session_replaced),
        (EventTag::SessionShutdown, on_session_shutdown),
    ]
}

rpi_plugin_sdk::event_handler! {
fn on_session_start(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    dispatch::ensure_started();
    dispatch::wake();
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_before_agent_start(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    TOOL_ATTEMPTED.store(false, Ordering::SeqCst);
    wake_goal();
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_agent_start(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    RUN_ACTIVE.store(true, Ordering::SeqCst);
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_agent_end(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    RUN_ACTIVE.store(false, Ordering::SeqCst);
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_agent_settled(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    RUN_ACTIVE.store(false, Ordering::SeqCst);
    TOOL_ATTEMPTED.store(false, Ordering::SeqCst);
    dispatch::ensure_started();
    dispatch::wake();
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_turn_end(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    let output = last_text()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .unwrap_or_default();
    let used_tool = TOOL_ATTEMPTED.swap(false, Ordering::SeqCst);
    account_turn_end(&output, used_tool);
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_after_provider_response(event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    if let Some(text) = event.data_text() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            account_response(&value);
        }
    }
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_tool_call(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    TOOL_ATTEMPTED.store(true, Ordering::SeqCst);
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_message_end(event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    let Some(raw) = event.message_text() else {
        return 0;
    };
    // The payload is the serialized `AgentMessage` (tagged by `kind`), not plain
    // text: assistant messages carry usage and the stop reason here.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        *last_text().lock().unwrap_or_else(|p| p.into_inner()) = Some(raw);
        return 0;
    };
    if policy::is_assistant(&value) {
        account_response(&value);
        *last_text().lock().unwrap_or_else(|p| p.into_inner()) = Some(policy::assistant_text(&value));
    }
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_session_compact(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    after_compaction();
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_session_replaced(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    // A queued write belongs to the branch this process no longer serves.
    state::discard_desired();
    inject::discard();
    0
}
}

rpi_plugin_sdk::event_handler! {
fn on_session_shutdown(_event, data) {
    let _scope = runtime::enter(runtime::callback(data));
    before_shutdown();
    0
}
}

/// Register every handler. Returns the first non-zero host return code.
pub fn register(api: &rpi_plugin_sdk::PluginApi) {
    let Some(register_event) = api.register_event_handler else {
        return;
    };
    for (tag, handler) in registrations() {
        let _ = register_event(tag, handler, api.user_data);
    }
    dispatch::ensure_started();
}
