//! Pure goal policy: budgets, safety circuit breakers, output fingerprints, and
//! provider-response classification.
//!
//! Everything here is a function of explicit inputs so the transition table can
//! be tested without a host. No state is read or written.
use crate::settings::Settings;
use crate::state::{format_token_count, Goal, Status, WaitKind, Waiting};

/// What one completed assistant response means for the goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseClass {
    /// Ordinary response; the turn may continue or settle normally.
    Ok,
    /// The provider cut the response off at the length limit.
    Length,
    /// A recognized account/usage limit stopped the request.
    UsageLimited,
    /// A provider failure that native retry did not resolve.
    ProviderError,
}

/// Classify an `AfterProviderResponse` payload (a serialized `AssistantMessage`).
pub fn classify_response(value: &serde_json::Value) -> ResponseClass {
    let stop = value
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let error = value
        .get("error_message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if stop == "length" {
        return ResponseClass::Length;
    }
    if stop == "error" || !error.is_empty() {
        if is_usage_limit(error) {
            return ResponseClass::UsageLimited;
        }
        return ResponseClass::ProviderError;
    }
    ResponseClass::Ok
}

/// Recognized quota / credit / billing exhaustion. Mirrors the wording classes
/// pi-goal treats as a usage limit; this is a classifier, not a retry policy.
pub fn is_usage_limit(text: &str) -> bool {
    let text = text.to_lowercase();
    const NEEDLES: &[&str] = &[
        "insufficient_quota",
        "insufficient quota",
        "quota exceeded",
        "exceeded your current quota",
        "usage limit",
        "rate limit exceeded",
        "credit balance",
        "credits exhausted",
        "out of credits",
        "billing",
        "payment required",
        "402",
        "429",
    ];
    NEEDLES.iter().any(|needle| text.contains(needle))
}

/// Usage tokens for one assistant message: prefer `total_tokens`, otherwise sum
/// the components pi-goal sums. `reasoning` is a subset of output and
/// `cacheWrite1h` a subset of cache writes, so neither is added.
pub fn response_tokens(value: &serde_json::Value) -> u64 {
    let usage = value.get("usage");
    let number = |key: &str| -> Option<i64> { usage?.get(key)?.as_i64().filter(|n| *n >= 0) };
    if let Some(total) = number("total_tokens") {
        if total > 0 {
            return total as u64;
        }
    }
    let sum = ["input", "output", "cache_read", "cache_write"]
        .iter()
        .filter_map(|key| number(key))
        .fold(0i64, |acc, n| acc.saturating_add(n));
    sum.max(0) as u64
}

/// Which budget ran out, if any.
pub fn exhausted_budget(goal: &Goal, now: i64) -> Option<String> {
    if let Some(max) = goal.max_turns {
        if goal.turns_used >= max {
            return Some(format!("turn budget reached ({max})"));
        }
    }
    if let Some(deadline) = goal.deadline_ms {
        if now >= deadline {
            return Some("time budget reached".to_string());
        }
    }
    if let Some(budget) = goal.token_budget {
        if goal.tokens_used >= budget {
            return Some(format!(
                "token budget reached ({}/{})",
                format_token_count(goal.tokens_used),
                format_token_count(budget)
            ));
        }
    }
    None
}

/// Safety circuit breakers. Independent of the user-facing budgets: these bound
/// automatic work even when no budget was configured.
pub fn safety_pause(goal: &Goal, settings: &Settings) -> Option<String> {
    if let Some(max) = settings.automatic_turns {
        if goal.automatic_turns >= max {
            return Some(format!("automatic limit {}/{}", goal.automatic_turns, max));
        }
    }
    if let Some(max) = settings.no_progress_turns {
        if goal.no_progress_count >= max {
            return Some(format!(
                "no progress: {} repeated tool-free turns",
                goal.no_progress_count
            ));
        }
    }
    None
}

/// Normalize assistant output for the no-progress fingerprint: case, whitespace,
/// control characters, and punctuation-only output all collapse to one value.
pub fn normalize_output(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut last_space = false;
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        if ch.is_whitespace() {
            if !last_space {
                normalized.push(' ');
                last_space = true;
            }
            continue;
        }
        last_space = false;
        for lower in ch.to_lowercase() {
            normalized.push(lower);
        }
    }
    let trimmed = normalized.trim();
    if trimmed.is_empty() || trimmed.chars().all(|c| c.is_ascii_punctuation()) {
        return String::new();
    }
    trimmed.to_string()
}

/// Fixed-size digest of normalized output. The raw assistant text is never
/// retained in goal state.
pub fn fingerprint(text: &str) -> String {
    let normalized = normalize_output(text);
    // FNV-1a 64: stable across processes, no dependency, fixed size.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalized.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    format!("{hash:016x}")
}

/// Fold one finished automatic, tool-free response into the no-progress counter.
pub fn record_tool_free_output(goal: &mut Goal, output: &str) -> bool {
    let fingerprint = fingerprint(output);
    let repeated = goal.last_output_fingerprint.as_deref() == Some(fingerprint.as_str());
    goal.last_output_fingerprint = Some(fingerprint);
    goal.no_progress_count = if repeated {
        goal.no_progress_count.saturating_add(1)
    } else {
        0
    };
    repeated
}

/// A tool attempt proves the run made real progress; reset the heuristic.
pub fn record_tool_attempt(goal: &mut Goal) {
    goal.no_progress_count = 0;
    goal.last_output_fingerprint = None;
}

/// Waiting record for a provider failure: quiet, no deadline, keeps the goal
/// active but stops automatic continuation.
pub fn provider_waiting(reason: &str) -> Waiting {
    Waiting {
        reason: bounded(reason, 1_000),
        kind: WaitKind::Provider,
        deadline_ms: None,
    }
}

pub fn usage_waiting(reason: &str) -> Waiting {
    Waiting {
        reason: bounded(reason, 1_000),
        kind: WaitKind::Usage,
        deadline_ms: None,
    }
}

fn bounded(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect()
}

/// Statuses that still permit a continuation dispatch.
pub fn may_continue(goal: &Goal) -> bool {
    goal.status == Status::Active && goal.waiting.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Goal;
    use serde_json::json;

    fn goal() -> Goal {
        Goal::new("t".into(), String::new(), 0)
    }

    #[test]
    fn responses_classify_by_stop_reason_and_error_text() {
        assert_eq!(
            classify_response(&json!({"stop_reason": "stop"})),
            ResponseClass::Ok
        );
        assert_eq!(
            classify_response(&json!({"stop_reason": "length"})),
            ResponseClass::Length
        );
        assert_eq!(
            classify_response(&json!({"stop_reason": "aborted"})),
            ResponseClass::Ok
        );
        assert_eq!(
            classify_response(
                &json!({"stop_reason": "error", "error_message": "429 Too Many Requests"})
            ),
            ResponseClass::UsageLimited
        );
        assert_eq!(
            classify_response(
                &json!({"stop_reason": "error", "error_message": "connection reset"})
            ),
            ResponseClass::ProviderError
        );
        assert_eq!(
            classify_response(&json!({"error_message": "Insufficient quota"})),
            ResponseClass::UsageLimited
        );
        assert_eq!(classify_response(&json!({})), ResponseClass::Ok);
    }

    #[test]
    fn usage_tokens_prefer_total_and_never_add_reasoning() {
        let with_total =
            json!({"usage": {"total_tokens": 120, "input": 100, "output": 20, "reasoning": 10}});
        assert_eq!(response_tokens(&with_total), 120);
        let without_total = json!({"usage": {"input": 100, "output": 20, "cache_read": 5, "cache_write": 5, "reasoning": 10}});
        assert_eq!(response_tokens(&without_total), 130);
        let negative = json!({"usage": {"total_tokens": -1, "input": 10, "output": -4}});
        assert_eq!(response_tokens(&negative), 10);
        assert_eq!(response_tokens(&json!({})), 0);
    }

    #[test]
    fn budgets_report_the_first_exhausted_dimension() {
        let mut goal = goal();
        assert!(exhausted_budget(&goal, 0).is_none());
        goal.max_turns = Some(2);
        goal.turns_used = 2;
        assert!(exhausted_budget(&goal, 0).unwrap().contains("turn budget"));
        goal.max_turns = None;
        goal.deadline_ms = Some(1_000);
        assert!(exhausted_budget(&goal, 1_000)
            .unwrap()
            .contains("time budget"));
        goal.deadline_ms = None;
        goal.token_budget = Some(100);
        goal.tokens_used = 100;
        assert!(exhausted_budget(&goal, 0).unwrap().contains("token budget"));
    }

    #[test]
    fn safety_breakers_pause_at_the_configured_limits() {
        let settings = Settings::default();
        let mut goal = goal();
        assert!(safety_pause(&goal, &settings).is_none());
        goal.automatic_turns = 25;
        assert!(safety_pause(&goal, &settings)
            .unwrap()
            .contains("automatic limit"));
        goal.automatic_turns = 0;
        goal.no_progress_count = 3;
        assert!(safety_pause(&goal, &settings)
            .unwrap()
            .contains("no progress"));
        let unlimited = Settings {
            automatic_turns: None,
            no_progress_turns: None,
            problem: None,
        };
        assert!(safety_pause(&goal, &unlimited).is_none());
    }

    #[test]
    fn fingerprints_ignore_case_whitespace_and_punctuation_only_output() {
        assert_eq!(fingerprint("Same  Text"), fingerprint("same text"));
        assert_eq!(fingerprint("  "), fingerprint("!!!"));
        assert_ne!(fingerprint("a"), fingerprint("b"));
        assert_eq!(fingerprint("a").len(), 16);
    }

    #[test]
    fn no_progress_counts_only_repeated_tool_free_output() {
        let mut goal = goal();
        assert!(!record_tool_free_output(&mut goal, "working"));
        assert_eq!(goal.no_progress_count, 0);
        assert!(record_tool_free_output(&mut goal, "working"));
        assert_eq!(goal.no_progress_count, 1);
        assert!(record_tool_free_output(&mut goal, "Working"));
        assert_eq!(goal.no_progress_count, 2);
        assert!(!record_tool_free_output(&mut goal, "different"));
        assert_eq!(goal.no_progress_count, 0);
        assert!(record_tool_free_output(&mut goal, "different"));
        record_tool_attempt(&mut goal);
        assert_eq!(goal.no_progress_count, 0);
        assert!(goal.last_output_fingerprint.is_none());
    }

    #[test]
    fn continued_dispatch_requires_an_active_unblocked_goal() {
        let mut goal = goal();
        assert!(may_continue(&goal));
        goal.status = Status::Paused;
        assert!(!may_continue(&goal));
        goal.status = Status::Active;
        goal.waiting = Some(provider_waiting("boom"));
        assert!(!may_continue(&goal));
    }

    #[test]
    fn waiting_reasons_are_bounded() {
        let reason = "x".repeat(2_000);
        assert_eq!(provider_waiting(&reason).reason.chars().count(), 1_000);
    }
}
