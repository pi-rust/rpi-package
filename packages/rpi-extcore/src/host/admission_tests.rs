use super::*;
#[test]
fn correlated_rejection_settlement_and_unsent_rollback_never_clear_other_execution() {
    let state = Arc::new(StdMutex::new(ExecutionAdmission::default()));
    let request = json!({"type":"prompt","id":"request-a"});
    let ticket = ExecutionTicket::register(&state, &request)
        .unwrap()
        .unwrap();
    let queued = ticket.clone();
    drop(ticket);
    assert!(!state.lock().unwrap().unsettled());
    assert!(!queued.published());
    drop(queued);
    let ticket = ExecutionTicket::register(&state, &request)
        .unwrap()
        .unwrap();
    assert!(ticket.published());
    state
        .lock()
        .unwrap()
        .observe(&json!({"type":"response","id":"other","success":false}));
    assert!(state.lock().unwrap().unsettled());
    state
        .lock()
        .unwrap()
        .observe(&json!({"type":"agent_settled","runId":"old"}));
    assert!(state.lock().unwrap().unsettled());
    drop(ticket);
    assert!(
        state.lock().unwrap().unsettled(),
        "published caller cancellation is not unsent"
    );
    state
        .lock()
        .unwrap()
        .observe(&json!({"type":"response","id":"request-a","success":false}));
    assert!(!state.lock().unwrap().unsettled());
    let ticket = ExecutionTicket::register(&state, &request)
        .unwrap()
        .unwrap();
    assert!(ticket.published());
    state.lock().unwrap().observe(
        &json!({"type":"response","id":"request-a","success":true,"data":{"accepted":true}}),
    );
    state
        .lock()
        .unwrap()
        .observe(&json!({"type":"agent_start","runId":"current"}));
    state
        .lock()
        .unwrap()
        .observe(&json!({"type":"response","id":"request-a","success":false}));
    assert!(
        state.lock().unwrap().unsettled(),
        "post-acceptance rejection is not settlement"
    );
    state
        .lock()
        .unwrap()
        .observe(&json!({"type":"agent_settled","runId":"old"}));
    assert!(state.lock().unwrap().unsettled());
    state
        .lock()
        .unwrap()
        .observe(&json!({"type":"agent_settled","runId":"current"}));
    assert!(!state.lock().unwrap().unsettled());
    drop(ticket);
    let mut notified = ExecutionTicket::register(&state, &request)
        .unwrap()
        .unwrap();
    let queued = notified.clone();
    notified.retain_queued();
    drop(notified);
    assert!(state.lock().unwrap().unsettled());
    drop(queued);
    assert!(
        !state.lock().unwrap().unsettled(),
        "dropped unsent outbound rolls back exactly its queued intent"
    );
}
