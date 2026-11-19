//! Completion verification: the host bridge (`_verify-enable`,
//! `_verify-start`, `_verify-result`) holds continuations until a verdict,
//! recovers from missing verdicts/boundaries, and keeps its persisted state
//! bounded.
mod support;

use rpi_plugin_sdk::EventTag;
use serde_json::{json, Value};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

fn arm() {
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();
    let armed = support::command("_verify-enable on");
    assert!(
        armed["text"].as_str().unwrap().contains("armed: true"),
        "arming must be acknowledged: {armed}"
    );
}

fn start(title: &str) {
    support::command(title);
    let sends = support::wait_sends_mentioning(title, 1, TIMEOUT);
    assert_eq!(sends.len(), 1, "the kickoff dispatch must happen");
}

fn verify_start(items: Value) -> Value {
    support::command(&format!("_verify-start {items}"))
}

fn verify_result(payload: Value) -> Value {
    support::command(&format!("_verify-result {payload}"))
}

/// A hand-built armed state for the timeout / recovery paths.
fn armed_state(status: &str, pending: bool, updated_at: i64) -> Value {
    let mut state = support::active_goal_state("manual-goal", "Manual objective");
    state["verification"] = json!(true);
    state["active"]["status"] = json!(status);
    state["active"]["pendingDispatch"] = json!(pending);
    state["active"]["updatedAt"] = json!(updated_at);
    state["active"]["verifyStartedAt"] = json!(updated_at);
    state
}

#[test]
fn pass_verdict_completes_the_goal_in_place() {
    let _guard = support::serial();
    arm();
    start("Verified objective");
    assert_eq!(support::status(), "active");

    // The run settles; an armed host must be asked before anything continues.
    support::fire_turn_end();
    support::fire_settled();
    support::sleep(150);
    assert_eq!(
        support::sends_mentioning("Verified objective").len(),
        1,
        "no continuation may start before the verdict"
    );

    let verifying = verify_start(json!([
        {"content": "write the fix", "status": "completed"},
        {"content": "run the suite", "status": "in_progress"}
    ]));
    assert!(verifying["text"].as_str().unwrap().contains("Verifying"));
    assert_eq!(support::status(), "verifying");

    let verdict = verify_result(json!({"outcome": "pass", "reason": "suite is green"}));
    assert!(verdict["text"].as_str().unwrap().contains("complete"));
    assert_eq!(support::status(), "complete");
    let goal = support::active_goal().expect("a completed goal stays visible");
    assert_eq!(goal["iterations"][0]["iteration"], json!(1));
    assert_eq!(
        goal["iterations"][0]["items"][1]["status"],
        json!("in_progress")
    );
    assert_eq!(goal["verifications"][0]["outcome"], json!("pass"));
    assert_eq!(goal["verifyIteration"], Value::Null);

    // Completion is terminal: a settle after it starts nothing.
    support::fire_settled();
    support::sleep(150);
    assert_eq!(
        support::sends_mentioning("Verified objective").len(),
        1,
        "a completed goal must not dispatch"
    );
}

#[test]
fn not_satisfied_dispatches_one_continuation_with_the_verdict() {
    let _guard = support::serial();
    arm();
    start("Iterating objective");
    support::fire_turn_end();
    support::fire_settled();
    support::sleep(100);

    verify_start(json!([]));
    assert_eq!(support::status(), "verifying");
    let verdict = verify_result(json!({
        "outcome": "notSatisfied",
        "reason": "tests were never run",
        "nextAction": "run the suite"
    }));
    assert!(verdict["text"].as_str().unwrap().contains("active"));

    let sends = support::wait_sends_mentioning("Iterating objective", 2, TIMEOUT);
    assert_eq!(sends.len(), 2, "exactly one continuation is owed");
    let continuation = sends[1]["text"].as_str().unwrap();
    assert!(
        continuation.contains("automatic continuation #2"),
        "{continuation}"
    );
    assert!(
        continuation.contains("tests were never run"),
        "{continuation}"
    );
    assert!(continuation.contains("run the suite"), "{continuation}");

    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("active"));
    assert_eq!(goal["verifications"][0]["outcome"], json!("not_satisfied"));
    // The next settled run waits for its own verdict again.
    support::fire_settled();
    support::sleep(150);
    assert_eq!(
        support::sends_mentioning("Iterating objective").len(),
        2,
        "the next run needs a fresh verdict"
    );
}

#[test]
fn verifier_failure_and_cancellation_pause_with_named_causes() {
    let _guard = support::serial();
    arm();
    start("Fail objective");
    support::fire_turn_end();
    support::fire_settled();
    support::sleep(100);

    verify_start(json!([]));
    verify_result(json!({"outcome": "failed", "reason": "verifier request failed"}));
    assert_eq!(support::status(), "paused");
    assert_eq!(
        support::active_goal().unwrap()["safetyPauseCause"],
        json!("verification_failed")
    );

    support::command("resume");
    assert_eq!(support::status(), "active");
    support::fire_turn_end();
    support::fire_settled();
    support::sleep(100);
    verify_start(json!([]));
    verify_result(json!({"outcome": "cancelled"}));
    assert_eq!(support::status(), "paused");
    assert_eq!(
        support::active_goal().unwrap()["safetyPauseCause"],
        json!("verification_cancelled")
    );
}

#[test]
fn verdicts_are_rejected_without_an_open_boundary() {
    let _guard = support::serial();
    arm();
    start("Strict objective");

    let out_of_order = verify_result(json!({"outcome": "pass"}));
    assert!(
        out_of_order["text"]
            .as_str()
            .unwrap()
            .contains("no verification is in flight"),
        "{out_of_order}"
    );
    assert_eq!(support::status(), "active");

    verify_start(json!([]));
    let double_start = verify_start(json!([]));
    assert!(
        double_start["text"]
            .as_str()
            .unwrap()
            .contains("verification needs an active goal"),
        "{double_start}"
    );
    // The open boundary still accepts exactly one verdict.
    verify_result(json!({"outcome": "pass"}));
    let late = verify_result(json!({"outcome": "pass"}));
    assert!(
        late["text"]
            .as_str()
            .unwrap()
            .contains("no verification is in flight"),
        "{late}"
    );
}

#[test]
fn abandoned_verification_times_out_to_a_named_pause() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();

    let stale = rpi_goal::state::now_ms() - rpi_goal::state::VERIFICATION_TIMEOUT_MS - 1_000;
    support::set_goal(armed_state("verifying", false, stale));
    support::fire_settled();

    let deadline = std::time::Instant::now() + TIMEOUT;
    while std::time::Instant::now() < deadline {
        if support::status() == "paused" {
            break;
        }
        support::sleep(20);
    }
    assert_eq!(support::status(), "paused");
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["safetyPauseCause"], json!("verification_timeout"));
    assert_eq!(goal["verifyIteration"], Value::Null);
    // A late verdict cannot revive a timed-out boundary.
    let late = verify_result(json!({"outcome": "pass"}));
    assert!(late["text"].as_str().unwrap().contains("no verification"));
}

#[test]
fn a_missed_boundary_recovers_instead_of_stalling() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();

    let stale = rpi_goal::state::now_ms() - rpi_goal::state::VERIFICATION_ARM_GRACE_MS - 1_000;
    support::set_goal(armed_state("active", false, stale));
    support::fire_settled();

    let sends = support::wait_sends_mentioning("Manual objective", 1, TIMEOUT);
    assert_eq!(
        sends.len(),
        1,
        "the worker must continue when the host misses the boundary"
    );
    let notice = support::calls_named("AppendEntry")
        .into_iter()
        .filter_map(|call| call["message"].clone().as_object().cloned())
        .any(|message| {
            message
                .get("content")
                .and_then(|content| content[0].get("text"))
                .and_then(|text| text.as_str())
                .map(|text| text.contains("did not open a boundary"))
                .unwrap_or(false)
        });
    assert!(notice, "the recovery must be visible in the transcript");
}

#[test]
fn deferred_host_commits_still_show_the_verdict() {
    let _guard = support::serial();
    arm();
    support::defer_appends(true);
    start("Deferred objective");
    support::fire_turn_end();
    support::fire_settled();
    support::sleep(100);

    verify_start(json!([]));
    verify_result(json!({"outcome": "pass", "reason": "ok"}));
    // `active_goal()` reads the host-committed entry; before the commit the
    // local intent must already serve `/goal status`.
    let local = support::command("status");
    assert!(
        local["text"].as_str().unwrap().contains("status: complete"),
        "the queued intent must stay visible before the host commits: {local}"
    );
    support::commit_staged();
    let committed = support::committed_goal().expect("the host committed the entry");
    assert_eq!(committed["active"]["status"], json!("complete"));
    assert_eq!(
        committed["active"]["verifications"][0]["outcome"],
        json!("pass")
    );
}

#[test]
fn disarming_restores_the_plain_continuation_loop() {
    let _guard = support::serial();
    arm();
    start("Disarm objective");
    support::fire(EventTag::AgentStart);
    let off = support::command("_verify-enable off");
    assert!(off["text"].as_str().unwrap().contains("armed: false"));
    support::sleep(150);
    assert_eq!(
        support::sends_mentioning("Disarm objective").len(),
        1,
        "a live run must not gain a concurrent continuation"
    );
    support::fire(EventTag::AgentEnd);
    support::fire(EventTag::AgentSettled);
    let sends = support::wait_sends_mentioning("Disarm objective", 2, TIMEOUT);
    assert_eq!(sends.len(), 2, "the v2 settled-boundary dispatch is back");
    support::sleep(120);
    assert_eq!(support::sends_mentioning("Disarm objective").len(), 2);
}

#[test]
fn a_failed_continuation_waits_instead_of_spinning() {
    let _guard = support::serial();
    arm();
    support::set_send_status("error");
    support::command("Error objective");
    let sends = support::wait_for("SendMessage", 1, TIMEOUT);
    assert_eq!(sends.len(), 1);
    support::sleep(200);
    assert_eq!(
        support::sends_mentioning("Error objective").len(),
        1,
        "a bridge failure must not retry in a tight loop"
    );
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("active"));
    assert_eq!(goal["waiting"]["kind"], json!("provider"));
}

#[test]
fn legacy_entries_still_reach_the_verification_boundary() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();

    // A v1/v2 entry has none of the verification fields; it must stay readable
    // and the host must still be able to open a boundary.
    support::set_goal(json!({
        "version": 2,
        "active": {
            "title": "Legacy objective",
            "status": "active",
            "notes": "",
            "turnsUsed": 1,
            "maxTurns": null,
            "deadlineMs": null
        },
        "archived": []
    }));
    assert_eq!(support::status(), "active");
    support::command("_verify-enable on");
    let started = verify_start(json!([{"content": "old todo", "status": "pending"}]));
    assert!(started["text"].as_str().unwrap().contains("Verifying"));
    assert_eq!(support::status(), "verifying");
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["iterations"][0]["iteration"], json!(1));
}
