//! Footer status line.
//!
//! `SetStatus` publishes a short string under the `goal` key. Headless hosts
//! accept the write and render nothing, so this stays fire-and-forget.
use serde_json::json;

use crate::settings;
use crate::state::{self, format_token_count, Goal, Status, WaitKind};
use rpi_plugin_sdk::RuntimeActionId;

pub fn publish(goal: &Goal) {
    let _ = crate::call_runtime(
        RuntimeActionId::SetStatus,
        json!({"key": "goal", "value": label(goal)}),
    );
}

pub fn clear() {
    let _ = crate::call_runtime(
        RuntimeActionId::SetStatus,
        json!({"key": "goal", "value": ""}),
    );
}

/// Compact, plain-text status. Kept stable so a statusline extension can rely on
/// the shape: `<state>[ · <detail>][ · automatic <n>/<limit>]`.
pub fn label(goal: &Goal) -> String {
    let now = state::now_ms();
    let settings = settings::load();
    let automatic = format!(
        "automatic {}/{}",
        goal.automatic_turns,
        settings.automatic_turns_label()
    );
    match goal.status {
        Status::Active if goal.waiting.is_some() => "waiting".to_string(),
        Status::Active => {
            let elapsed = state::format_duration_ms(goal.elapsed_ms(now));
            match goal.token_budget {
                Some(_) if goal.usage_unavailable => {
                    format!("active {elapsed} · tokens unreported · {automatic}")
                }
                Some(budget) => format!(
                    "active {elapsed} · {}/{} · {automatic}",
                    format_token_count(goal.tokens_used),
                    format_token_count(budget)
                ),
                None => format!("active {elapsed} · {automatic}"),
            }
        }
        Status::Waiting => match goal.waiting.as_ref().map(|w| w.kind) {
            Some(WaitKind::Usage) => format!("usage · {automatic}"),
            Some(WaitKind::Provider) => format!("provider wait · {automatic}"),
            _ => "waiting".to_string(),
        },
        Status::Paused => match goal.safety_pause_cause.as_deref() {
            Some("continuation_limit") => format!("paused · limit {automatic}"),
            Some("no_progress") => format!("paused · no progress · {automatic}"),
            _ => format!("paused · {automatic}"),
        },
        Status::Blocked => format!("blocked · {automatic}"),
        Status::UsageLimited => format!("usage · {automatic}"),
        Status::BudgetLimited => message_for_budget(goal, &automatic),
        Status::Complete => "complete".to_string(),
    }
}

fn message_for_budget(goal: &Goal, automatic: &str) -> String {
    match goal.token_budget {
        Some(budget) => format!(
            "budget {}/{} · {automatic}",
            format_token_count(goal.tokens_used),
            format_token_count(budget)
        ),
        None => format!("budget · {automatic}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Goal, Status, WaitKind, Waiting};

    fn goal() -> Goal {
        Goal::new("t".into(), String::new(), 0)
    }

    #[test]
    fn active_label_matches_the_documented_shape() {
        let goal = Goal::new("t".into(), String::new(), state::now_ms());
        let label = label(&goal);
        assert!(label.starts_with("active 0s"), "{label}");
        assert!(label.contains("automatic 0/25"), "{label}");
    }

    #[test]
    fn stopped_labels_name_their_reason() {
        let mut goal = goal();
        goal.status = Status::BudgetLimited;
        goal.token_budget = Some(100_000);
        goal.tokens_used = 100_000;
        assert!(
            label(&goal).starts_with("budget 100k/100k"),
            "{}",
            label(&goal)
        );
        goal.status = Status::Paused;
        goal.safety_pause_cause = Some("continuation_limit".into());
        assert!(
            label(&goal).starts_with("paused · limit"),
            "{}",
            label(&goal)
        );
        goal.safety_pause_cause = Some("no_progress".into());
        assert!(label(&goal).contains("no progress"), "{}", label(&goal));
        goal.status = Status::Complete;
        assert_eq!(label(&goal), "complete");
    }

    #[test]
    fn waiting_label_hides_the_reason() {
        let mut goal = goal();
        goal.waiting = Some(Waiting {
            reason: "review monitor".into(),
            kind: WaitKind::External,
            deadline_ms: None,
        });
        assert_eq!(label(&goal), "waiting");
        goal.status = Status::Waiting;
        goal.waiting.as_mut().unwrap().kind = WaitKind::Usage;
        assert!(label(&goal).starts_with("usage"), "{}", label(&goal));
    }
}
