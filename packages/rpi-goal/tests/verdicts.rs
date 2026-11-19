//! Completion verification, the narrow form: the plugin records verifier
//! verdicts (`_verify-result`), the seven statuses stay untouched, and the
//! existing continuation worker dispatches a rejected goal's next action.
//!
//! Acceptance: a goal that claims completion with work missing keeps going.
mod support;

use serde_json::json;
use std::time::Duration;

use rpi_plugin_sdk::EventTag;

const TIMEOUT: Duration = Duration::from_secs(3);

fn start_goal(title: &str) {
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();
    support::command(title);
    support::wait_sends_mentioning(title, 1, TIMEOUT);
}

#[test]
fn a_rejected_completion_claim_keeps_the_goal_going() {
    let _guard = support::serial();
    start_goal("Objective with hidden requirements");

    // The model claims completion: the goal archives and the loop stops.
    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();
    support::tool(
        "goal_complete",
        json!({"goal_id": id, "summary": "everything is done"}),
    )
    .expect("the completion claim must be accepted");
    assert!(support::active_goal().is_none(), "the goal archived");
    support::sleep(120);
    let sends_after_complete = support::calls_named("SendMessage").len();

    // The host's verifier rejects the claim. The narrow contract: record the
    // verdict, reopen the goal, queue the next action.
    let verdict = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","reason":"tests were never run","nextAction":"Run the test suite and fix the failures"}}"#
    ));
    let text = verdict["text"].as_str().unwrap();
    assert!(text.contains("not satisfied"), "{text}");

    let goal = support::active_goal().expect("the goal must be reopened");
    assert_eq!(goal["status"], json!("active"), "{goal}");
    assert_ne!(goal["id"].as_str().unwrap(), id, "the id rotates on reopen");
    assert_eq!(goal["verifications"].as_array().unwrap().len(), 1);
    assert_eq!(
        goal["pendingNextAction"].as_str().unwrap(),
        "Run the test suite and fix the failures"
    );

    // The existing worker dispatches the verifier's next action — exactly once.
    let sends = support::wait_sends_mentioning("Run the test suite", 1, TIMEOUT);
    assert_eq!(sends.len(), 1, "the next action must dispatch exactly once");
    let prompt = sends[0]["text"].as_str().unwrap();
    assert!(prompt.contains("rejected"), "{prompt}");
    assert!(prompt.contains("tests were never run"), "{prompt}");
    assert!(prompt.contains("<next_action>"), "{prompt}");
    assert!(
        prompt.contains("Run the test suite and fix the failures"),
        "{prompt}"
    );
    assert!(prompt.contains("<goal_objective>"), "{prompt}");
    assert!(
        prompt.contains(goal["id"].as_str().unwrap()),
        "the dispatched prompt carries the fresh goal id"
    );

    // The verdict continuation replaced the generic continuation prompt: no
    // second send raced out with the plain "continue" text.
    support::sleep(150);
    assert_eq!(
        support::sends_mentioning("Run the test suite").len(),
        1,
        "the queued next action must not dispatch twice"
    );
    assert_eq!(
        support::calls_named("SendMessage").len(),
        sends_after_complete + 1,
        "no unrelated dispatch may follow"
    );
}

#[test]
fn a_pass_verdict_records_and_leaves_the_goal_archived() {
    let _guard = support::serial();
    start_goal("Pass verdict objective");

    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();
    support::tool(
        "goal_complete",
        json!({"goal_id": id, "summary": "done for real"}),
    )
    .expect("the completion claim must be accepted");
    support::sleep(120);
    let sends_after_complete = support::calls_named("SendMessage").len();

    let verdict = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"pass","reason":"all acceptance checks green"}}"#
    ));
    assert!(
        verdict["text"].as_str().unwrap().contains("pass"),
        "{verdict}"
    );
    assert!(
        support::active_goal().is_none(),
        "a pass verdict never resurrects the goal"
    );
    let archived = support::goal_state().unwrap()["archived"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(archived["verifications"].as_array().unwrap().len(), 1);
    assert_eq!(
        archived["verifications"][0]["outcome"],
        json!("pass"),
        "{archived}"
    );
    support::sleep(150);
    assert_eq!(
        support::calls_named("SendMessage").len(),
        sends_after_complete,
        "a pass verdict must not start a run"
    );
}

#[test]
fn a_verdict_on_a_live_run_waits_for_the_settled_boundary() {
    let _guard = support::serial();
    start_goal("Mid-run verdict objective");

    // Simulate a live user run on top of the goal.
    support::block_sends(true);
    support::fire(EventTag::AgentStart);
    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();

    let verdict = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","nextAction":"Recheck the requirement list"}}"#
    ));
    let text = verdict["text"].as_str().unwrap();
    assert!(!text.contains("⚠️"), "{text}");

    support::sleep(120);
    assert!(
        support::sends_mentioning("Recheck the requirement list").is_empty(),
        "no continuation may be injected into a live run"
    );

    // The settled boundary dispatches the queued next action.
    support::fire(EventTag::AgentEnd);
    support::fire_settled();
    let sends = support::wait_sends_mentioning("Recheck the requirement list", 1, TIMEOUT);
    assert_eq!(sends.len(), 1);
    support::release_sends();
}

#[test]
fn a_rejection_on_a_stopped_goal_stays_queued_until_resume() {
    let _guard = support::serial();
    start_goal("Stopped verdict objective");
    support::command("pause");
    support::sleep(120);
    assert_eq!(support::status(), "paused");

    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();
    support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied","nextAction":"Continue after the pause"}}"#
    ));
    support::sleep(120);
    let goal = support::active_goal().unwrap();
    assert_eq!(
        goal["status"],
        json!("paused"),
        "a paused goal stays paused"
    );
    assert_eq!(
        goal["pendingNextAction"].as_str().unwrap(),
        "Continue after the pause"
    );
    assert!(
        support::sends_mentioning("Continue after the pause").is_empty(),
        "nothing may run while the goal is stopped"
    );

    // Resume dispatches the queued next action (and rotates the id).
    support::command("resume");
    let sends = support::wait_sends_mentioning("Continue after the pause", 1, TIMEOUT);
    assert_eq!(sends.len(), 1);
}

#[test]
fn malformed_verdicts_fail_instead_of_silently_doing_nothing() {
    let _guard = support::serial();
    start_goal("Malformed verdict objective");
    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();

    // A rejection without a next action would strand the reopened goal.
    let text = support::command(&format!(
        r#"_verify-result {{"goalId":"{id}","outcome":"notSatisfied"}}"#
    ))["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("⚠️"), "{text}");
    assert!(text.contains("next_action"), "{text}");

    // Unknown outcome / bad JSON / stale goal_id are all explicit errors.
    for args in [
        r#"{"outcome":"maybe"}"#.to_string(),
        "not json".to_string(),
        r#"{"goalId":"deadbeefdeadbeef","outcome":"pass"}"#.to_string(),
    ] {
        let text = support::command(&format!("_verify-result {args}"))["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("⚠️"), "{args} -> {text}");
    }

    // Nothing was recorded and nothing was dispatched.
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["verifications"].as_array().unwrap().len(), 0);
    support::sleep(120);
    assert_eq!(
        support::sends_mentioning("Malformed verdict objective").len(),
        1
    );
}

#[test]
fn verdict_rows_are_bounded_in_persisted_state() {
    let _guard = support::serial();
    start_goal("Bounded verdicts objective");
    let goal = support::active_goal().unwrap();
    let id = goal["id"].as_str().unwrap().to_string();

    // Keep the worker parked so the active goal's ordinary loop cannot race the
    // verdict recording; verdicts are recorded through the command thread.
    support::block_sends(true);
    // Drive 51 pass verdicts through the host face; the 51st evicts the oldest.
    for _ in 0..51 {
        support::command(&format!(
            r#"_verify-result {{"goalId":"{id}","outcome":"pass"}}"#
        ));
    }
    let goal = support::active_goal().unwrap();
    let rows = goal["verifications"].as_array().unwrap();
    assert_eq!(rows.len(), rpi_goal::state::MAX_VERIFICATIONS);
    support::release_sends();
}
