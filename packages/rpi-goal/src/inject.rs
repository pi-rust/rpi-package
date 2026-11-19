//! Goal context notices.
//!
//! A notice is a generic custom message appended at the next *safe* boundary.
//! Appending from inside a tool batch would place it between an
//! `assistant(tool_calls)` message and its `toolResult`, which strict
//! OpenAI-compatible providers reject.
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use rpi_plugin_sdk::RuntimeActionId;

use crate::prompts;
use crate::state::{self, Goal, GOAL_CUSTOM_TYPE};

struct Pending {
    revision: u64,
    goal: Goal,
    text: String,
}

fn pending() -> &'static Mutex<Option<Pending>> {
    static PENDING: OnceLock<Mutex<Option<Pending>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(None))
}

fn last_injected() -> &'static Mutex<Option<String>> {
    static LAST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

static REVISION: AtomicU64 = AtomicU64::new(0);

/// State changes that the model must see. `text` overrides the default notice
/// for stop/wait transitions that explain *why* work stopped.
pub fn queue(goal: &Goal, text: Option<String>) {
    let mut slot = pending().lock().unwrap_or_else(|p| p.into_inner());
    let revision = REVISION.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    *slot = Some(Pending {
        revision,
        goal: goal.clone(),
        text: text.unwrap_or_else(|| prompts::notice(goal)),
    });
}

/// Append the queued notice. Returns false when an append failed; the notice is
/// kept so a later boundary retries it.
pub fn flush() -> bool {
    let claimed = pending().lock().unwrap_or_else(|p| p.into_inner()).take();
    let Some(notice) = claimed else {
        return true;
    };
    let result = append(&notice.goal, &notice.text);
    let mut slot = pending().lock().unwrap_or_else(|p| p.into_inner());
    // A newer notice may have been queued *and* delivered while this host call
    // was in flight, so an empty slot alone cannot prove this one is current.
    if REVISION.load(Ordering::Relaxed) != notice.revision {
        return result.is_ok();
    }
    match result {
        Ok(()) => {
            remember(Some(fingerprint(&notice.goal)));
            true
        }
        Err(_) => {
            slot.get_or_insert(notice);
            false
        }
    }
}

/// Drop any queued notice (the goal was cleared or replaced).
pub fn discard() {
    *pending().lock().unwrap_or_else(|p| p.into_inner()) = None;
    REVISION.fetch_add(1, Ordering::Relaxed);
    remember(None);
}

fn append(goal: &Goal, text: &str) -> Result<(), String> {
    let message = json!({
        "kind": "custom",
        "role": "custom",
        "content": [{"type": "text", "text": text}],
        "data": {
            "customType": GOAL_CUSTOM_TYPE,
            "display": true,
            "details": {
                "goalId": goal.id,
                "title": goal.title,
                "status": goal.status.as_str(),
                "notes": goal.notes,
                "iteration": goal.iteration,
                "turnsUsed": goal.turns_used,
                "maxTurns": goal.max_turns,
                "deadlineMs": goal.deadline_ms,
                "tokenBudget": goal.token_budget,
                "tokensUsed": goal.tokens_used,
                "updatedAt": goal.updated_at,
            }
        },
        "timestamp": goal.updated_at,
    });
    crate::call_runtime(RuntimeActionId::AppendEntry, json!({"message": message}))?;
    Ok(())
}

/// Dedupe key. Live counters are excluded so a running budget does not re-inject
/// the same notice on every turn.
fn fingerprint(goal: &Goal) -> String {
    format!(
        "{}|{}|{}|{}",
        goal.id,
        goal.title,
        goal.status.as_str(),
        goal.notes
    )
}

fn remember(fingerprint: Option<String>) {
    *last_injected().lock().unwrap_or_else(|p| p.into_inner()) = fingerprint;
}

/// True when this exact goal state has not been injected yet.
pub fn should_inject(goal: &Goal) -> bool {
    last_injected()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_deref()
        != Some(fingerprint(goal).as_str())
}

pub fn mark_injected(goal: &Goal) {
    remember(Some(fingerprint(goal)));
}

/// Test/diagnostic view of the queued notice.
pub fn pending_text() -> Option<Value> {
    pending()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|pending| json!({"text": pending.text, "goalId": pending.goal.id}))
}

pub fn has_pending() -> bool {
    pending()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some()
}

/// Append the current goal state right now (used by `/goal` commands, which run
/// outside a tool batch).
pub fn flush_now(_: &Goal) -> bool {
    state::flush() && flush()
}
