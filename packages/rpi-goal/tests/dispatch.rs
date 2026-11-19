//! Continuation dispatch: one automatic run per settled boundary, stopped by
//! pause, replacement, budgets, and the safety circuit breakers.
mod support;

use rpi_plugin_sdk::EventTag;
use serde_json::json;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

#[test]
fn starting_a_goal_dispatches_one_kickoff_prompt() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    support::command("Ship the release");
    let sends = support::wait_for("SendMessage", 1, TIMEOUT);
    let prompt = sends[0]["text"].as_str().unwrap();
    assert!(prompt.contains("Ship the release"), "{prompt}");
    assert!(prompt.contains("<goal_objective>"), "{prompt}");
    assert!(prompt.contains("<goal_id>"), "{prompt}");
    assert_eq!(sends[0]["triggerTurn"], true);

    let goal = support::active_goal().unwrap();
    assert_eq!(goal["iteration"], json!(1));
    assert!(prompt.contains(goal["id"].as_str().unwrap()));
}

#[test]
fn each_settled_boundary_dispatches_exactly_one_more_run() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();
    support::command("Settle boundary objective");
    let first = support::wait_sends_mentioning("Settle boundary objective", 1, TIMEOUT).len();
    assert_eq!(first, 1, "the kickoff dispatch must happen exactly once");

    support::fire_settled();
    let second =
        support::wait_sends_mentioning("Settle boundary objective", first + 1, TIMEOUT).len();
    assert_eq!(
        second,
        first + 1,
        "a settled boundary must dispatch exactly one more run"
    );

    support::fire_settled();
    let third =
        support::wait_sends_mentioning("Settle boundary objective", second + 1, TIMEOUT).len();
    assert_eq!(third, second + 1);
    support::sleep(120);
    assert_eq!(
        support::sends_mentioning("Settle boundary objective").len(),
        third,
        "no extra dispatch may follow a consumed boundary"
    );
}

#[test]
fn a_paused_goal_is_never_continued() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();
    support::command("Paused objective");
    let before = support::wait_sends_mentioning("Paused objective", 1, TIMEOUT).len();
    assert_eq!(before, 1);

    support::command("pause");
    support::fire_settled();
    support::fire_settled();
    support::sleep(150);
    assert_eq!(
        support::sends_mentioning("Paused objective").len(),
        before,
        "a paused goal must not start automatic runs"
    );
    assert_eq!(support::status(), "paused");
}

#[test]
fn a_goal_made_while_a_run_is_in_flight_waits_for_the_settled_boundary() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    support::block_sends(true);
    support::fire(EventTag::AgentStart);
    let started = support::tool(
        "goal",
        json!({"action": "start", "title": "Started mid-run"}),
    )
    .expect("starting a goal must not fail");
    assert!(started["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Started mid-run"));

    support::sleep(120);
    assert!(
        support::calls_named("SendMessage").is_empty(),
        "no continuation may be injected into a live run"
    );

    support::fire(EventTag::AgentEnd);
    support::fire_settled();
    let sends = support::wait_for("SendMessage", 1, TIMEOUT);
    assert_eq!(
        sends.len(),
        1,
        "the settled boundary must continue the goal"
    );
    support::release_sends();
}

#[test]
fn replacing_the_goal_makes_later_prompts_carry_the_new_objective_only() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    support::command("First objective");
    support::sleep(80);
    let first = support::active_goal().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    support::command("Second objective");
    support::sleep(120);
    let sends = support::sends_mentioning("Second objective");
    assert!(!sends.is_empty(), "the replacement must be continued");
    let latest = sends.last().unwrap()["text"].as_str().unwrap().to_string();
    let goal = support::active_goal().unwrap();
    assert_ne!(goal["id"].as_str().unwrap(), first);
    assert!(!latest.contains(&first), "the stale goal id must not leak");
}

#[test]
fn the_automatic_turn_limit_pauses_instead_of_continuing() {
    let _guard = support::serial();
    let _settings = support::SettingsDir::with_limits(Some(1), Some(3));
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    // Keep the first automatic run in flight so its turn counts as automatic.
    support::block_sends(true);
    support::command("Limited objective");
    support::wait_for("SendMessage", 1, TIMEOUT);
    support::fire_turn_end();
    support::release_sends();

    support::sleep(80);
    support::fire_settled();
    support::sleep(200);

    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("paused"), "{goal}");
    assert_eq!(goal["automaticTurns"], json!(1));
    assert_eq!(goal["safetyPauseCause"], json!("continuation_limit"));
    assert_eq!(
        support::sends_mentioning("Limited objective").len(),
        1,
        "the goal must pause instead of starting another run"
    );
}

#[test]
fn a_goal_at_the_no_progress_limit_is_paused_before_continuing() {
    let _guard = support::serial();
    let _settings = support::SettingsDir::with_limits(Some(25), Some(3));
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    support::set_goal(json!({
        "version": 2,
        "active": {
            "id": "aaaaaaaabbbbbbbb",
            "title": "Already stuck",
            "status": "active",
            "notes": "",
            "startedAt": rpi_goal::state::now_ms(),
            "updatedAt": rpi_goal::state::now_ms(),
            "noProgressCount": 3,
            "activeSince": rpi_goal::state::now_ms()
        },
        "archived": []
    }));
    support::fire_settled();
    support::sleep(150);

    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("paused"), "{goal}");
    assert_eq!(goal["safetyPauseCause"], json!("no_progress"));
    assert!(support::calls_named("SendMessage").is_empty());
}

#[test]
fn a_missing_goal_complete_tool_pauses_instead_of_running_unbounded() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();
    support::set_active_tools(&["goal", "goal_wait"]);

    support::command("No terminal tool objective");
    support::sleep(150);

    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("paused"), "{goal}");
    assert!(support::sends_mentioning("No terminal tool objective").is_empty());
}

#[test]
fn an_aborted_run_pauses_and_an_errored_run_waits() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    // Let the session-start wake drain before a goal exists, so it cannot
    // coalesce with the goal-start dispatch.
    support::settle_pause();

    support::block_sends(true);
    support::set_send_status("aborted");
    support::command("Aborted objective");
    support::wait_for("SendMessage", 1, TIMEOUT);
    support::release_sends();
    support::sleep(150);
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("paused"), "{goal}");
    support::set_send_status("completed");

    // A provider error instead quiets the goal without stopping it.
    support::block_sends(true);
    support::set_send_status("error");
    support::command("Error objective");
    support::wait_sends_mentioning("Error objective", 1, TIMEOUT);
    support::release_sends();
    support::sleep(150);
    let goal = support::active_goal().unwrap();
    assert_eq!(goal["status"], json!("active"), "{goal}");
    assert_eq!(goal["waiting"]["kind"], json!("provider"), "{goal}");

    // External input wakes it again and the run resumes normally.
    support::set_send_status("completed");
    support::fire(EventTag::BeforeAgentStart);
    support::sleep(150);
    let goal = support::active_goal().unwrap();
    assert!(goal["waiting"].is_null(), "{goal}");
    assert_eq!(goal["status"], json!("active"), "{goal}");
    assert_eq!(support::sends_mentioning("Error objective").len(), 2);
}

#[test]
fn pausing_a_live_run_aborts_it_instead_of_waiting() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();

    support::block_sends(true);
    support::fire(EventTag::AgentStart);
    support::command("Abort objective");
    support::wait_sends_mentioning("Abort objective", 1, TIMEOUT);

    support::command("pause");
    let aborts = support::wait_for("Abort", 1, TIMEOUT);
    assert_eq!(aborts.len(), 1, "pause must abort the in-flight run");
    assert_eq!(support::status(), "paused");
    support::fire(EventTag::AgentEnd);
    support::release_sends();
    support::sleep(80);
}

#[test]
fn pausing_without_a_live_run_does_not_abort_anything() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();

    support::command("Idle pause objective");
    support::wait_sends_mentioning("Idle pause objective", 1, TIMEOUT);
    support::command("pause");
    support::sleep(120);
    assert!(
        support::calls_named("Abort").is_empty(),
        "no run was in flight, so nothing may be aborted"
    );
    assert_eq!(support::status(), "paused");
}

#[test]
fn a_host_without_abort_still_pauses_cleanly() {
    let _guard = support::serial();
    support::install();
    support::fire(EventTag::SessionStart);
    support::settle_pause();
    support::set_abort_supported(false);

    support::block_sends(true);
    support::fire(EventTag::AgentStart);
    support::command("Legacy host objective");
    support::wait_sends_mentioning("Legacy host objective", 1, TIMEOUT);

    support::command("pause");
    let attempts = support::wait_for("Abort", 1, TIMEOUT);
    assert_eq!(attempts.len(), 1, "the action is attempted once");
    assert_eq!(
        support::status(),
        "paused",
        "an unsupported abort must not block the stop"
    );
    support::fire(EventTag::AgentEnd);
    support::release_sends();
    support::sleep(80);
}
