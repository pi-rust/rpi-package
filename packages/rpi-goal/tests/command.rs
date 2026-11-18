//! `/goal` commands persist state and return immediately; work starts from the
//! continuation worker, never from the TUI thread.
mod support;

use rpi_plugin_sdk::EventTag;
use serde_json::json;
use std::time::Duration;

#[test]
fn start_command_returns_before_work_starts_and_status_reports_it() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    let response = support::command("start Ship the release --notes \"be careful\"");
    assert_eq!(response["kind"], "message");
    let text = response["text"].as_str().unwrap();
    assert!(text.contains("Goal started"), "{text}");
    assert!(text.contains("Ship the release"), "{text}");

    let goal = support::active_goal().expect("the command must persist the goal");
    assert_eq!(goal["title"], "Ship the release");
    assert_eq!(goal["notes"], "be careful");
    assert_eq!(goal["status"], "active");

    // The worker sends the first work message without further user input.
    let sends = support::wait_for("SendMessage", 1, Duration::from_secs(3));
    assert_eq!(sends.len(), 1, "the first work message was never sent");
    let prompt = sends[0]["text"].as_str().unwrap();
    assert!(prompt.contains("Ship the release"), "{prompt}");
    assert_eq!(sends[0]["triggerTurn"], true);

    let status = support::command("status");
    let text = status["text"].as_str().unwrap();
    assert!(text.contains("Ship the release"), "{text}");
    assert!(text.contains("active"), "{text}");

    let help = support::command("help");
    assert!(help["text"].as_str().unwrap().contains("/goal start"));
    assert!(support::command("settings")["text"]
        .as_str()
        .unwrap()
        .contains("automatic turns"));
}

#[test]
fn a_token_budget_on_the_start_route_is_parsed_as_tokens() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();

    support::command("start Ship it --tokens 100");
    assert_eq!(support::active_goal().unwrap()["tokenBudget"], json!(100));

    support::command("start Ship it again --tokens 1.5m");
    assert_eq!(
        support::active_goal().unwrap()["tokenBudget"],
        json!(1_500_000)
    );
}

#[test]
fn shorthand_start_and_bare_status_do_not_break() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    let response = support::command("Ship it");
    assert!(response["text"].as_str().unwrap().contains("Goal started"));
    assert_eq!(support::active_goal().unwrap()["title"], json!("Ship it"));

    let empty = support::command("");
    assert!(empty["text"].as_str().unwrap().contains("Ship it"));
}

#[test]
fn pause_resume_stop_and_clear_route_through_the_goal() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    support::command("Ship it");
    let paused = support::command("pause");
    assert!(paused["text"].as_str().unwrap().contains("Paused"));
    assert_eq!(support::status(), "paused");

    let resumed = support::command("resume");
    assert!(resumed["text"].as_str().unwrap().contains("Resumed"));
    assert_eq!(support::status(), "active");

    let stopped = support::command("stop");
    assert!(stopped["text"].as_str().unwrap().contains("Completed"));
    assert!(support::active_goal().is_none());

    support::command("Ship it again");
    let cleared = support::command("clear");
    assert!(cleared["text"].as_str().unwrap().contains("Cleared"));
    assert!(support::active_goal().is_none());
}

#[test]
fn budgets_are_set_and_cleared_from_the_command() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();
    support::command("Ship it");

    support::command("turns 5");
    assert_eq!(support::active_goal().unwrap()["maxTurns"], json!(5));
    support::command("time 30m");
    assert!(
        support::active_goal().unwrap()["deadlineMs"]
            .as_i64()
            .unwrap()
            > 0
    );
    support::command("tokens 100k");
    assert_eq!(
        support::active_goal().unwrap()["tokenBudget"],
        json!(100_000)
    );

    support::command("turns 0");
    assert!(support::active_goal().unwrap()["maxTurns"].is_null());
    support::command("time 0");
    assert!(support::active_goal().unwrap()["deadlineMs"].is_null());
    support::command("tokens 0");
    assert!(support::active_goal().unwrap()["tokenBudget"].is_null());

    support::command("limit 7 15m");
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["maxTurns"], json!(7));
    assert!(goal["deadlineMs"].as_i64().unwrap() > 0);
}

#[test]
fn invalid_arguments_report_an_error_without_touching_state() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();
    support::command("Ship it");

    let budgets = |goal: &serde_json::Value| {
        json!([goal["maxTurns"], goal["deadlineMs"], goal["tokenBudget"]])
    };
    let before = budgets(&support::active_goal().unwrap());
    assert!(support::command("turns nope")["text"]
        .as_str()
        .unwrap()
        .starts_with("⚠️"));
    assert!(support::command("time 1x")["text"]
        .as_str()
        .unwrap()
        .starts_with("⚠️"));
    assert!(support::command("tokens nope")["text"]
        .as_str()
        .unwrap()
        .starts_with("⚠️"));
    assert_eq!(budgets(&support::active_goal().unwrap()), before);
}

#[test]
fn list_shows_active_and_archived_goals() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    support::command("First goal");
    support::command("Second goal");
    let text = support::command("list")["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("Second goal"), "{text}");
    assert!(text.contains("First goal"), "{text}");
    assert!(text.contains("archived: 1"), "{text}");
}
