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
        "baselineTokens": 0,
        "usageSeen": 0,
        "usageUnavailable": false
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
    assert_eq!(first["noProgressCount"], json!(1));
    assert_eq!(first["automaticTurns"], json!(1));
    assert!(first["lastOutputFingerprint"].is_string());
    support::release_sends();
    support::sleep(100);
}

#[test]
fn a_token_budget_says_so_when_the_host_reports_no_usage() {
    let _guard = support::serial();
    seed_active("Unreported usage", json!({"tokenBudget": 100_000}));

    // One completed turn with no provider usage report.
    rpi_goal::events::account_turn_end("", true);
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["usageUnavailable"], json!(true), "{goal}");
    assert_eq!(support::status(), "active", "the goal must keep working");

    let warning = support::calls_named("AppendEntry").into_iter().any(|call| {
        call["message"]["content"][0]["text"]
            .as_str()
            .map(|text| text.contains("cannot be enforced"))
            .unwrap_or(false)
    });
    assert!(warning, "the limitation must be reported once");
    assert!(!rpi_goal::status::label(&{
        let mut goal = rpi_goal::state::read().unwrap().active.unwrap();
        goal.usage_unavailable = true;
        goal
    })
    .contains("0/100k"));
}

#[test]
fn message_end_without_usage_marks_a_token_budget_unavailable() {
    let _guard = support::serial();
    seed_active(
        "Unreported usage via MessageEnd",
        json!({"tokenBudget": 100_000}),
    );

    // A serialized AssistantMessage may carry a zeroed usage object even when
    // the provider reported nothing. This must not look like an observed
    // report just because the response reached MessageEnd.
    support::fire_message(
        rpi_plugin_sdk::EventTag::MessageEnd,
        &json!({
            "kind": "assistant",
            "role": "assistant",
            "content": [{"type": "text", "text": "done"}],
            "stopReason": "stop"
        })
        .to_string(),
    );
    support::fire_turn_end();

    let goal = support::active_goal().unwrap();
    assert_eq!(goal["tokensUsed"], json!(0), "{goal}");
    assert_eq!(goal["usageSeen"], json!(0), "{goal}");
    assert_eq!(goal["usageUnavailable"], json!(true), "{goal}");
}

#[test]
fn an_observed_usage_report_clears_the_unavailable_flag() {
    let _guard = support::serial();
    seed_active(
        "Reported usage",
        json!({"tokenBudget": 100_000, "usageUnavailable": true}),
    );
    rpi_goal::events::account_response(
        &json!({"stopReason": "stop", "usage": {"totalTokens": 250}}),
    );
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["usageSeen"], json!(1));
    assert_eq!(goal["usageUnavailable"], json!(false));
    assert_eq!(goal["tokensUsed"], json!(250));
}

fn assistant_message(text: &str, tokens: i64, stop: &str, timestamp: i64) -> String {
    json!({
        "kind": "assistant",
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "usage": {"input": tokens, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": tokens},
        "stopReason": stop,
        "timestamp": timestamp
    })
    .to_string()
}

#[test]
fn message_end_carries_usage_and_classification() {
    let _guard = support::serial();
    seed_active("Message end accounting", json!({}));

    support::fire_message(
        rpi_plugin_sdk::EventTag::MessageEnd,
        &assistant_message("all good", 120, "stop", 1),
    );
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["tokensUsed"], json!(120), "{goal}");
    assert_eq!(goal["usageSeen"], json!(1));

    // The same response must not be counted again when the optional
    // AfterProviderResponse observer also reports it.
    rpi_goal::events::account_response(&json!({
        "kind": "assistant",
        "usage": {"totalTokens": 120},
        "stopReason": "stop",
        "timestamp": 1
    }));
    assert_eq!(support::active_goal().unwrap()["tokensUsed"], json!(120));

    // A quota error in the same carrier stops the goal.
    support::fire_message(
        rpi_plugin_sdk::EventTag::MessageEnd,
        &json!({
            "kind": "assistant",
            "role": "assistant",
            "content": [],
            "usage": {"totalTokens": 5},
            "stopReason": "error",
            "errorMessage": "429 exceeded your current quota",
            "timestamp": 2
        })
        .to_string(),
    );
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("usage_limited"), "{goal}");
}

#[test]
fn user_messages_are_not_accounted() {
    let _guard = support::serial();
    seed_active("Ignore user messages", json!({}));
    support::fire_message(
        rpi_plugin_sdk::EventTag::MessageEnd,
        &json!({"kind": "user", "content": [{"type": "text", "text": "hi"}], "timestamp": 9})
            .to_string(),
    );
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["tokensUsed"], json!(0));
    assert_eq!(goal["usageSeen"], json!(0));
}

#[test]
fn identical_tool_free_output_pauses_an_automatic_loop() {
    let _guard = support::serial();
    let _settings = support::SettingsDir::with_limits(Some(25), Some(2));
    seed_active("Stuck loop", json!({}));

    support::block_sends(true);
    support::fire_settled();
    support::wait_for("SendMessage", 1, std::time::Duration::from_secs(3));

    // Two tool-free automatic turns whose visible text is identical, while the
    // timestamps and token counts differ.
    for (tokens, timestamp) in [(10, 1), (11, 2)] {
        support::fire_message(
            rpi_plugin_sdk::EventTag::MessageEnd,
            &assistant_message("same answer", tokens, "stop", timestamp),
        );
        support::fire_turn_end();
    }

    let goal = support::active_goal().unwrap();
    assert_eq!(goal["noProgressCount"], json!(2), "{goal}");
    assert_eq!(goal["status"], json!("paused"), "{goal}");
    assert_eq!(goal["safetyPauseCause"], json!("no_progress"));
    support::release_sends();
}
