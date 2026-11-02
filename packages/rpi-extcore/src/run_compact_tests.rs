#[test]
fn compact_outcomes_are_kind_specific_and_summary_is_terminal_once() {
    for (outcome, expected) in [
        ("compacted", Ok(RunOutcome::Completed)),
        ("declined", Ok(RunOutcome::Completed)),
        ("aborted", Ok(RunOutcome::Cancelled)),
        ("failed", Err("compactor offline".into())),
        (
            "completed",
            Err("rpi compaction did not complete: completed".into()),
        ),
        (
            "unknown",
            Err("rpi compaction did not complete: unknown".into()),
        ),
    ] {
        let mut run = RunTracker::for_kind(json!(7), RunKind::Compact);
        for event in [
            json!({"type":"agent_settled","outcome":outcome}),
            json!({"type":"response","id":8,"success":true,"data":{"accepted":true}}),
            json!({"type":"agent_settled","outcome":outcome}),
        ] {
            assert!(run.observe(&event, false).outcome.is_none());
        }
        assert!(run
            .observe(
                &json!({"type":"response","id":7,"success":true,"data":{"accepted":true}}),
                false
            )
            .outcome
            .is_none());
        for event in [
            json!({"type":"agent_end"}),
            json!({"type":"compaction_finished","summary":"not terminal"}),
            json!({"type":"message_update","eventType":"text_delta","delta":"not summary"}),
        ] {
            let progress = run.observe(&event, false);
            assert!(progress.outcome.is_none());
            assert!(progress.deltas.is_empty());
        }
        let settled = json!({"type":"agent_settled","outcome":outcome,"summary":"verdict 🌊","error":"compactor offline"});
        let progress = run.observe(&settled, false);
        assert_eq!(progress.outcome, Some(expected));
        assert_eq!(
            progress.deltas,
            vec![json!({"kind":"text","text":"verdict 🌊"})]
        );
        let duplicate = run.observe(&settled, false);
        assert!(duplicate.outcome.is_none());
        assert!(duplicate.deltas.is_empty());
    }
    let mut prompt = RunTracker::new(json!(7));
    prompt.observe(
        &json!({"type":"response","id":7,"success":true,"data":{"accepted":true}}),
        false,
    );
    assert!(prompt
        .observe(
            &json!({"type":"agent_settled","outcome":"compacted"}),
            false
        )
        .outcome
        .unwrap()
        .is_err());
}

#[test]
fn compact_cancelled_provider_failure_waits_for_settlement_and_stays_kind_specific() {
    let acknowledgement = json!({"type":"response","id":1,"success":true,"data":{"accepted":true}});
    let failure = json!({"type":"agent_settled","outcome":"failed","error":"abort error: Request aborted","summary":"Compaction failed: Request aborted"});
    for cancelled in [false, true] {
        let mut run = RunTracker::for_kind(json!(1), RunKind::Compact);
        assert!(run.observe(&failure, cancelled).outcome.is_none());
        assert!(run.observe(&acknowledgement, cancelled).outcome.is_none());
        assert!(run
            .observe(&json!({"type":"agent_end"}), cancelled)
            .outcome
            .is_none());
        let progress = run.observe(&failure, cancelled);
        if cancelled {
            assert_eq!(progress.outcome, Some(Ok(RunOutcome::Cancelled)));
            assert!(
                progress.deltas.is_empty(),
                "cancel must not display a failure summary"
            );
        } else {
            assert_eq!(
                progress.outcome,
                Some(Err("abort error: Request aborted".into()))
            );
            assert_eq!(progress.deltas.len(), 1);
        }
    }
    // This change is compact-only; normal prompt failure mapping is unchanged.
    let mut prompt = RunTracker::new(json!(1));
    prompt.observe(&acknowledgement, true);
    assert_eq!(
        prompt.observe(&failure, true).outcome,
        Some(Err("abort error: Request aborted".into()))
    );
}

#[test]
fn compact_optional_summary_rejection_cancellation_and_close() {
    for (outcome, text) in [
        ("declined", Some("Nothing to compact yet.")),
        ("compacted", None),
    ] {
        let mut run = RunTracker::for_kind(json!(1), RunKind::Compact);
        run.observe(
            &json!({"type":"response","id":1,"success":true,"data":{"accepted":true}}),
            false,
        );
        let progress = run.observe(&json!({"type":"agent_settled","outcome":outcome}), false);
        assert_eq!(progress.outcome, Some(Ok(RunOutcome::Completed)));
        assert_eq!(
            progress.deltas.first().and_then(|v| v["text"].as_str()),
            text
        );
    }
    for cancelled in [false, true] {
        let mut run = RunTracker::for_kind(json!(1), RunKind::Compact);
        assert_eq!(
            run.observe(
                &json!({"type":"response","id":1,"success":false,"error":"busy"}),
                cancelled
            )
            .outcome,
            Some(if cancelled {
                Ok(RunOutcome::Cancelled)
            } else {
                Err("busy".into())
            })
        );
    }
    let mut run = RunTracker::for_kind(json!(1), RunKind::Compact);
    assert!(run
        .observe(
            &json!({"type":"response","id":1,"success":true,"data":{}}),
            false
        )
        .outcome
        .unwrap()
        .is_err());
    for event in ["session_end", "host_closed"] {
        let mut run = RunTracker::for_kind(json!(1), RunKind::Compact);
        assert!(run
            .observe(&json!({"type":event}), false)
            .outcome
            .unwrap()
            .is_err());
    }
}
