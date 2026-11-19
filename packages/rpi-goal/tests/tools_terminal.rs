//! Explicit stopping reasons: `goal_complete`, `goal_blocked`, `goal_wait`.
mod support;

use rpi_plugin_sdk::EventTag;
use serde_json::json;

fn start_goal(title: &str) -> String {
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();
    support::tool("goal", json!({"action": "start", "title": title})).expect("start must succeed");
    support::active_goal().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn all_four_tools_are_registered() {
    let _guard = support::serial();
    support::install();
    let names = support::tool_names();
    for expected in ["goal", "goal_complete", "goal_blocked", "goal_wait"] {
        assert!(
            names.contains(&expected.to_string()),
            "missing {expected}: {names:?}"
        );
    }
}

#[test]
fn goal_complete_requires_the_current_goal_id_and_a_summary() {
    let _guard = support::serial();
    let id = start_goal("Complete me");

    let stale = support::tool(
        "goal_complete",
        json!({"goal_id": "0000000000000000", "summary": "done"}),
    )
    .expect_err("a stale goal id must be rejected");
    assert!(stale.contains("stale goal_id"), "{stale}");
    assert_eq!(
        support::status(),
        "active",
        "a rejected call must not mutate"
    );

    let empty = support::tool("goal_complete", json!({"goal_id": id, "summary": "   "}))
        .expect_err("an empty summary must be rejected");
    assert!(empty.contains("summary"), "{empty}");

    let done = support::tool(
        "goal_complete",
        json!({"goal_id": id, "summary": "Tests pass; PR opened."}),
    )
    .expect("a matching id and summary must complete the goal");
    assert!(done["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Goal complete"));
    assert_eq!(
        support::status(),
        "complete",
        "a completed goal is terminal but stays visible until replaced"
    );
    assert!(support::active_goal().is_some());

    // The completion summary is persisted for the transcript.
    let notice = support::calls_named("AppendEntry")
        .into_iter()
        .filter_map(|call| call["message"].clone().as_object().cloned())
        .any(|message| {
            message
                .get("content")
                .and_then(|content| content[0].get("text"))
                .and_then(|text| text.as_str())
                .map(|text| text.contains("Tests pass; PR opened."))
                .unwrap_or(false)
        });
    assert!(notice, "the accepted summary must be shown");
}

#[test]
fn terminal_tools_reject_the_replaced_goals_id() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();

    support::tool("goal", json!({"action": "start", "title": "First goal"})).unwrap();
    let first = support::active_goal().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    support::tool("goal", json!({"action": "start", "title": "Second goal"})).unwrap();
    let second = support::active_goal().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first, second);

    let stale = support::tool(
        "goal_complete",
        json!({"goal_id": first, "summary": "old work"}),
    )
    .expect_err("the replaced goal id must be rejected in the same transition");
    assert!(stale.contains("stale goal_id"), "{stale}");
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["id"], json!(second));
    assert_eq!(goal["status"], json!("active"));
}

#[test]
fn goal_blocked_needs_a_repeated_evidenced_impasse() {
    let _guard = support::serial();
    let id = start_goal("Block me");

    let premature = support::tool(
        "goal_blocked",
        json!({
            "goal_id": id,
            "reason": "needs credentials",
            "evidence": "401 twice",
            "repeated_turns": 2
        }),
    )
    .expect_err("fewer than three turns must be rejected");
    assert!(premature.contains("at least 3 turns"), "{premature}");
    assert_eq!(support::status(), "active");

    let blocked = support::tool(
        "goal_blocked",
        json!({
            "goal_id": id,
            "reason": "needs credentials",
            "evidence": "the same 401 on every attempt",
            "repeated_turns": 3
        }),
    )
    .expect("a repeated evidenced blocker must be accepted");
    assert!(blocked["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Goal blocked"));
    assert!(support::active_goal().is_none());
}

#[test]
fn goal_wait_clamps_short_deadlines_and_keeps_the_goal() {
    let _guard = support::serial();
    let id = start_goal("Wait for the review");

    let before = rpi_goal::state::now_ms();
    support::tool(
        "goal_wait",
        json!({"goal_id": id, "reason": "waiting for the review monitor", "resume_after_ms": 1500}),
    )
    .expect("a wait must be accepted");
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], "active");
    assert_eq!(goal["waiting"]["kind"], "external");
    let deadline = goal["waiting"]["deadlineMs"].as_i64().unwrap();
    assert!(
        deadline - before >= rpi_goal::state::MIN_WAIT_MS,
        "short waits must be clamped: {}",
        deadline - before
    );
    assert!(
        goal["activeSince"].is_null(),
        "waiting time is not active time"
    );
}

#[test]
fn stopped_goals_cannot_be_completed_blocked_or_waited() {
    let _guard = support::serial();
    let id = start_goal("Paused work");
    support::tool("goal", json!({"action": "pause"})).unwrap();
    assert_eq!(support::status(), "paused");

    for (tool, params) in [
        ("goal_complete", json!({"goal_id": id, "summary": "done"})),
        (
            "goal_blocked",
            json!({"goal_id": id, "reason": "r", "evidence": "e", "repeated_turns": 3}),
        ),
        ("goal_wait", json!({"goal_id": id, "reason": "r"})),
    ] {
        let error =
            support::tool(tool, params).expect_err("a paused goal must reject terminal tools");
        assert!(
            error.contains("resume it") || error.contains("already paused"),
            "{tool}: {error}"
        );
    }
    assert_eq!(support::status(), "paused");
}

#[test]
fn terminal_tools_without_a_goal_report_it_clearly() {
    let _guard = support::serial();
    support::install();
    support::reset();
    let error = support::tool("goal_complete", json!({"goal_id": "x", "summary": "done"}))
        .expect_err("no goal means no completion");
    assert!(error.contains("no active goal"), "{error}");
}
