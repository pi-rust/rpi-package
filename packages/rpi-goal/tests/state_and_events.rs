//! Session-state migration, compaction re-injection, and wait wake-up.
mod support;

use rpi_plugin_sdk::EventTag;
use serde_json::json;

#[test]
fn a_v1_goal_state_entry_still_loads() {
    let _guard = support::serial();
    support::install();
    support::settle_pause();
    support::set_goal(json!({
        "active": {
            "title": "Legacy objective",
            "status": "active",
            "notes": "old notes",
            "startedAt": 10,
            "updatedAt": 20,
            "turnsUsed": 3,
            "maxTurns": 10,
            "deadlineMs": null
        },
        "archived": []
    }));

    let goals = rpi_goal::state::read().unwrap();
    let goal = goals.active.unwrap();
    assert_eq!(goal.title, "Legacy objective");
    assert_eq!(goal.turns_used, 3);
    assert_eq!(goal.max_turns, Some(10));
    assert_eq!(goal.iteration, 0);
    assert!(goal.waiting.is_none());
    assert_eq!(goal.status, rpi_goal::state::Status::Active);

    // The status view tolerates an empty id from the legacy entry.
    let status = support::command("status");
    assert!(status["text"]
        .as_str()
        .unwrap()
        .contains("Legacy objective"));
}

#[test]
fn compaction_reinjects_the_goal_notice() {
    let _guard = support::serial();
    support::install();
    support::settle_pause();
    support::tool(
        "goal",
        json!({"action": "start", "title": "Survive compaction"}),
    )
    .expect("start must succeed");
    support::settle_pause();

    let before = support::calls_named("AppendEntry").len();
    support::fire(EventTag::SessionCompact);
    let after = support::calls_named("AppendEntry").len();
    assert!(
        after > before,
        "compaction must re-append the goal notice for the model"
    );

    let messages: Vec<String> = support::calls_named("AppendEntry")
        .into_iter()
        .filter_map(|call| {
            call["message"]["content"][0]["text"]
                .as_str()
                .map(|text| text.to_string())
        })
        .collect();
    assert!(messages
        .iter()
        .any(|text| text.contains("Survive compaction")));
    assert_eq!(support::status(), "active");
}

#[test]
fn a_new_run_wakes_a_waiting_goal() {
    let _guard = support::serial();
    support::install();
    support::settle_pause();
    support::set_goal(json!({
        "version": 2,
        "active": {
            "id": "aaaabbbbccccdddd",
            "title": "Waiting objective",
            "status": "active",
            "notes": "",
            "startedAt": rpi_goal::state::now_ms(),
            "updatedAt": rpi_goal::state::now_ms(),
            "tokensUsed": 0,
            "waiting": {
                "reason": "review monitor",
                "kind": "provider",
                "deadlineMs": null
            }
        },
        "archived": []
    }));

    support::fire(EventTag::BeforeAgentStart);
    support::sleep(80);
    let goal = support::active_goal().unwrap();
    assert!(goal["waiting"].is_null(), "{goal}");
    assert!(goal["activeSince"].as_i64().is_some());
}

#[test]
fn an_elapsed_wait_deadline_wakes_the_goal() {
    let _guard = support::serial();
    support::install();
    support::settle_pause();
    let past = rpi_goal::state::now_ms() - 5_000;
    support::set_goal(json!({
        "version": 2,
        "active": {
            "id": "ddddeeeeffff0000",
            "title": "Deadline objective",
            "status": "active",
            "notes": "",
            "startedAt": rpi_goal::state::now_ms(),
            "updatedAt": rpi_goal::state::now_ms(),
            "tokensUsed": 0,
            "waiting": {"reason": "safety wake", "kind": "external", "deadlineMs": past}
        },
        "archived": []
    }));

    support::fire_settled();
    let sends =
        support::wait_sends_mentioning("Deadline objective", 1, std::time::Duration::from_secs(3));
    assert_eq!(
        sends.len(),
        1,
        "an elapsed wait deadline must resume the goal"
    );
    assert!(support::active_goal().unwrap()["waiting"].is_null());
}

#[test]
fn a_wait_deadline_in_the_future_keeps_the_goal_quiet() {
    let _guard = support::serial();
    support::install();
    support::settle_pause();
    let future = rpi_goal::state::now_ms() + 600_000;
    support::set_goal(json!({
        "version": 2,
        "active": {
            "id": "9999888877776666",
            "title": "Quiet objective",
            "status": "active",
            "notes": "",
            "startedAt": rpi_goal::state::now_ms(),
            "updatedAt": rpi_goal::state::now_ms(),
            "tokensUsed": 0,
            "waiting": {"reason": "waiting", "kind": "external", "deadlineMs": future}
        },
        "archived": []
    }));

    support::fire_settled();
    support::sleep(150);
    assert!(support::sends_mentioning("Quiet objective").is_empty());
    assert_eq!(support::active_goal().unwrap()["status"], json!("active"));
}

#[test]
fn resume_rotates_the_goal_id_only_from_a_stopped_state() {
    let _guard = support::serial();
    support::install();
    support::settle_pause();
    support::tool(
        "goal",
        json!({"action": "start", "title": "Resume semantics"}),
    )
    .expect("start must succeed");
    let started = support::active_goal().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Waiting resume keeps the identity so the stale-turn guard stays valid.
    support::tool(
        "goal_wait",
        json!({"goal_id": started, "reason": "waiting for review"}),
    )
    .expect("wait must be accepted");
    support::tool("goal", json!({"action": "resume"})).expect("resume must succeed");
    let resumed = support::active_goal().unwrap();
    assert_eq!(resumed["id"], json!(started));
    assert!(resumed["waiting"].is_null());

    // Stopped resume rotates the id and resets the safety epoch.
    support::tool("goal", json!({"action": "pause"})).unwrap();
    support::tool("goal", json!({"action": "resume"})).unwrap();
    let after = support::active_goal().unwrap();
    assert_ne!(
        after["id"],
        json!(started),
        "a stopped resume rotates the id"
    );
    assert_eq!(after["automaticTurns"], json!(0));
    assert_eq!(after["noProgressCount"], json!(0));
    assert!(after["safetyPauseCause"].is_null());
}
