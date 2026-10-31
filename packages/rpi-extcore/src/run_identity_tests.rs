fn identity_run() -> RunTracker {
    let mut run = RunTracker::new(json!("request"));
    run.require_message_ids(true);
    run.observe(
        &json!({"type":"response","id":"request","success":true,"data":{"accepted":true}}),
        false,
    );
    run
}

#[test]
fn identity_errors_emit_nothing_and_drain_until_accepted_native_settlement() {
    for invalid in [None, Some("changed")] {
        let mut run = identity_run();
        let initial = run.observe(
            &json!({"type":"message_start","messageId":"entry",
            "message":{"role":"assistant","content":[{"type":"text","text":"before"}]}}),
            false,
        );
        assert_eq!(
            initial.deltas,
            vec![json!({"kind":"text","text":"before","messageId":"entry"})]
        );
        let mut bad = json!({"type":"message_update","eventType":"text_delta","delta":"bad"});
        if let Some(id) = invalid {
            bad["messageId"] = json!(id);
        }
        let progress = run.observe(&bad, false);
        assert!(progress.outcome.is_none());
        assert!(progress.deltas.is_empty());
        for event in [
            json!({"type":"message_update","messageId":"entry","eventType":"text_delta","delta":"hidden"}),
            json!({"type":"message_start","messageId":"new","message":{"role":"assistant","content":[{"type":"text","text":"also hidden"}]}}),
            json!({"type":"message_end","messageId":"entry","message":{"role":"assistant","content":[]}}),
            json!({"type":"agent_end"}),
            json!({"type":"response","id":"other","success":true,"data":{"accepted":true}}),
        ] {
            let progress = run.observe(&event, false);
            assert!(progress.outcome.is_none());
            assert!(progress.deltas.is_empty());
        }
        let progress = run.observe(
            &json!({"type":"agent_settled","outcome":"completed","finalText":"before repaired"}),
            true,
        );
        assert!(progress.deltas.is_empty());
        assert!(progress.outcome.unwrap().is_err());
    }
}

#[test]
fn native_identity_requires_correlated_acceptance_before_reporting_stream_error() {
    let mut run = RunTracker::new(json!("request"));
    run.require_message_ids(true);
    run.observe(
        &json!({"type":"message_update","eventType":"text_delta","delta":"missing identity"}),
        false,
    );
    run.observe(
        &json!({"type":"response","id":"unrelated","success":true,"data":{"accepted":true}}),
        false,
    );
    assert!(run
        .observe(
            &json!({"type":"agent_settled","outcome":"completed"}),
            false
        )
        .outcome
        .is_none());
    run.observe(
        &json!({"type":"response","id":"request","success":true,"data":{"accepted":true}}),
        false,
    );
    let progress = run.observe(
        &json!({"type":"agent_settled","outcome":"completed"}),
        false,
    );
    assert!(progress.outcome.unwrap().is_err());
    assert!(progress.deltas.is_empty());
}

#[test]
fn native_identity_final_settlement_repair_keeps_current_entry() {
    let mut run = identity_run();
    run.observe(&json!({"type":"message_start","messageId":"entry","message":{"role":"assistant","content":[]}}), false);
    assert_eq!(run.observe(&json!({"type":"message_update","messageId":"entry","eventType":"thinking_delta","delta":"thought"}), false).deltas,
        vec![json!({"kind":"thinking","text":"thought","messageId":"entry"})]);
    let progress = run.observe(
        &json!({"type":"agent_settled","outcome":"completed","finalText":"answer"}),
        false,
    );
    assert_eq!(
        progress.deltas,
        vec![json!({"kind":"text","text":"answer","messageId":"entry"})]
    );
    assert_eq!(progress.outcome, Some(Ok(RunOutcome::Completed)));
}

#[test]
fn native_identity_empty_pre_token_abort_or_failure_does_not_fabricate_an_id() {
    for (outcome, expected) in [
        ("aborted", Ok(RunOutcome::Cancelled)),
        ("failed", Err("provider failed".into())),
    ] {
        let mut run = identity_run();
        let progress = run.observe(&json!({"type":"agent_settled","outcome":outcome,"error":"provider failed","finalText":""}), true);
        assert_eq!(progress.outcome, Some(expected));
        assert!(progress.deltas.is_empty());
    }
    let mut run = identity_run();
    let progress = run.observe(
        &json!({"type":"agent_settled","outcome":"completed","finalText":"unattributed"}),
        false,
    );
    assert!(progress.outcome.unwrap().is_err());
    assert!(progress.deltas.is_empty());
}

#[test]
fn native_identity_requirement_does_not_apply_to_compact_summary() {
    let mut run = RunTracker::for_kind(json!("compact"), RunKind::Compact);
    run.require_message_ids(true);
    run.observe(
        &json!({"type":"response","id":"compact","success":true,"data":{"accepted":true}}),
        false,
    );
    let progress = run.observe(
        &json!({"type":"agent_settled","outcome":"compacted","summary":"summary"}),
        false,
    );
    assert_eq!(
        progress.deltas,
        vec![json!({"kind":"text","text":"summary"})]
    );
    assert_eq!(progress.outcome, Some(Ok(RunOutcome::Completed)));
}

#[tokio::test]
async fn native_identity_error_does_not_release_prompt_lease_before_settlement() {
    let gate = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let lease = gate.clone().try_lock_owned().unwrap();
    let (_events, receiver) = broadcast::channel(8);
    let (_cancellation, cancellation) = watch::channel(0);
    let mut run = PromptRun::new(
        receiver,
        cancellation,
        json!("request"),
        RunKind::Prompt,
        lease,
    );
    run.tracker.require_message_ids(true);
    run.tracker.observe(
        &json!({"type":"response","id":"request","success":true,"data":{"accepted":true}}),
        false,
    );
    let progress = run.tracker.observe(
        &json!({"type":"message_update","eventType":"text_delta","delta":"missing ID"}),
        false,
    );
    assert!(progress.outcome.is_none());
    assert!(gate.clone().try_lock_owned().is_err());
    assert!(run
        .tracker
        .observe(&json!({"type":"agent_end"}), false)
        .outcome
        .is_none());
    assert!(gate.clone().try_lock_owned().is_err());
    let progress = run.tracker.observe(
        &json!({"type":"agent_settled","outcome":"completed"}),
        false,
    );
    assert!(progress.outcome.unwrap().is_err());
    drop(run);
    assert!(gate.try_lock_owned().is_ok());
}
