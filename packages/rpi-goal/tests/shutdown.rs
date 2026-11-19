//! Session shutdown stops automatic work and checkpoints the active clock.
//!
//! This test file is deliberately the only test in its binary: the worker's
//! shutdown is process-wide and cannot be undone.
mod support;

use serde_json::json;

#[test]
fn shutdown_checkpoints_the_clock_and_stops_the_worker() {
    let _guard = support::serial();
    support::install();
    support::settle_pause();
    support::tool(
        "goal",
        json!({"action": "start", "title": "Shutdown objective"}),
    )
    .expect("start must succeed");
    support::settle_pause();

    let goal = support::active_goal().unwrap();
    assert!(goal["activeSince"].as_i64().is_some());

    support::fire(rpi_plugin_sdk::EventTag::SessionShutdown);

    let goal = support::active_goal().unwrap();
    assert!(
        goal["activeSince"].is_null(),
        "the active clock must stop at shutdown: {goal}"
    );

    // No further automatic work after shutdown, even on a settled boundary.
    let before = support::calls_named("SendMessage").len();
    support::fire_settled();
    support::sleep(150);
    assert_eq!(
        support::calls_named("SendMessage").len(),
        before,
        "the worker must not dispatch after shutdown"
    );
}
