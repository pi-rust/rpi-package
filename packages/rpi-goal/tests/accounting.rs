//! Turn accounting, token budgets, and provider-response classification through
//! the real state machine (no live provider).
mod support;

use serde_json::json;

fn seed_active(title: &str, extra: serde_json::Value) -> String {
    support::install();
    let mut active = json!({
        "id": "1111222233334444",
        "title": title,
        "status": "active",
        "notes": "",
        "startedAt": rpi_goal::state::now_ms(),
        "updatedAt": rpi_goal::state::now_ms(),
        "activeSince": rpi_goal::state::now_ms(),
        "iteration": 0,
        "turnsUsed": 0,
        "automaticTurns": 0,
        "noProgressCount": 0,
        "tokensUsed": 0,
        "baselineTokens": 0
    });
    for (key, value) in extra.as_object().unwrap() {
        active[key] = value.clone();
    }
    support::set_goal(json!({"version": 2, "active": active, "archived": []}));
    "1111222233334444".to_string()
}

#[test]
fn every_response_contributes_token_usage() {
    let _guard = support::serial();
    seed_active("Token accounting", json!({}));

    rpi_goal::events::account_response(
        &json!({"stop_reason": "stop", "usage": {"total_tokens": 120}}),
    );
    rpi_goal::events::account_response(
        &json!({"stop_reason": "toolUse", "usage": {"input": 80, "output": 20}}),
    );
    assert_eq!(support::active_goal().unwrap()["tokensUsed"], json!(220));
}

#[test]
fn a_token_budget_stops_continuation_at_the_next_boundary() {
    let _guard = support::serial();
    seed_active("Budgeted work", json!({"tokenBudget": 100}));

    rpi_goal::events::account_response(
        &json!({"stop_reason": "stop", "usage": {"total_tokens": 150}}),
    );
    support::fire_settled();
    support::sleep(150);

    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("budget_limited"), "{goal}");
    assert!(support::sends_mentioning("Budgeted work").is_empty());
}

#[test]
fn a_turn_budget_pauses_at_the_turn_end_boundary() {
    let _guard = support::serial();
    seed_active("Turn budget", json!({"maxTurns": 1}));

    rpi_goal::events::account_turn_end("", true);
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["turnsUsed"], json!(1));
    assert_eq!(goal["status"], json!("budget_limited"), "{goal}");
}

#[test]
fn stopped_goals_ignore_further_accounting() {
    let _guard = support::serial();
    seed_active("Paused work", json!({"status": "paused", "turnsUsed": 4}));

    rpi_goal::events::account_turn_end("output", false);
    rpi_goal::events::account_response(&json!({"usage": {"total_tokens": 999}}));

    let goal = support::active_goal().unwrap();
    assert_eq!(goal["turnsUsed"], json!(4));
    assert_eq!(goal["tokensUsed"], json!(0));
}

#[test]
fn a_usage_limit_stops_the_goal_and_a_provider_error_quietly_waits() {
    let _guard = support::serial();
    seed_active("Usage limited", json!({}));
    rpi_goal::events::account_response(&json!({
        "stop_reason": "error",
        "error_message": "429 Too Many Requests: exceeded your current quota",
        "usage": {"total_tokens": 10}
    }));
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("usage_limited"), "{goal}");
    assert!(goal["waiting"].is_null());

    support::reset();
    seed_active("Provider error", json!({}));
    rpi_goal::events::account_response(&json!({
        "stop_reason": "error",
        "error_message": "connection reset by peer"
    }));
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("active"), "{goal}");
    assert_eq!(goal["waiting"]["kind"], json!("provider"), "{goal}");
}

#[test]
fn a_tool_free_automatic_turn_feeds_the_no_progress_counter() {
    let _guard = support::serial();
    seed_active("No progress", json!({}));

    // Automatic run: keep the continuation in flight while the turn ends.
    support::block_sends(true);
    support::fire_settled();
    support::wait_for("SendMessage", 1, std::time::Duration::from_secs(3));
    rpi_goal::events::account_turn_end("same answer", false);
    let first = support::active_goal().unwrap();
    assert_eq!(first["noProgressCount"], json!(0));
    assert_eq!(first["automaticTurns"], json!(1));
    assert!(first["lastOutputFingerprint"].is_string());
    support::release_sends();
    support::sleep(100);
}
